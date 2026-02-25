// shader_pass.rs — post-processing shader pass for trixie compositor
//
// Architecture
// ────────────
// The fundamental problem with post-processing in Smithay is that
// DrmCompositor owns its scanout buffers entirely. We cannot safely read from
// or write to its FBO except through the render_frame API.
//
// The correct approach:
//
//   WHEN SHADERS ARE DISABLED (common case):
//     Call render_frame normally. Zero overhead.
//
//   WHEN SHADERS ARE ENABLED:
//     1. Before render_frame, bind GlesRenderer to our own intermediate FBO
//        using bind_to_fbo() — this redirects all rendering into our texture.
//     2. Call render_frame as normal — Smithay renders the full scene into our
//        FBO instead of the DRM buffer.
//     3. Unbind our FBO, restore the DRM binding.
//     4. Run ping-pong shader passes over the intermediate texture.
//     5. Blit the final result into the DRM FBO (write-only, no read-back).
//
// Call site in render_surface (state.rs):
//
//   // Bind renderer to intermediate FBO if shaders are active.
//   let shader_active = state.shader_pass.begin(
//       &mut backend.renderer, w, h, &state.config.shaders
//   );
//
//   // Normal render_frame call — unmodified.
//   let frame = sd.compositor.render_frame(&mut backend.renderer, &elements, bg, flags)?;
//
//   if !frame.is_empty {
//       if shader_active {
//           // Get the DRM FBO id while the renderer is still bound to it.
//           let drm_fbo = unsafe { current_draw_fbo() };
//           state.shader_pass.end(drm_fbo, w, h, mouse, &state.config.shaders);
//       }
//       sd.compositor.queue_frame(())?;
//   }

use std::{collections::HashMap, ffi::CString, time::Instant};

use smithay::backend::renderer::gles::ffi;

use crate::shader_config::ShaderRegistry;

// ── GLSL ──────────────────────────────────────────────────────────────────────

const VERT_SRC: &str = r#"
#version 300 es
out vec2 v_uv;
void main() {
    // Fullscreen triangle from gl_VertexID, no VBO needed.
    vec2 pos = vec2(
        float((gl_VertexID & 1) << 2) - 1.0,
        float((gl_VertexID & 2) << 1) - 1.0
    );
    // v_uv (0,0) = top-left, matching user expectations.
    v_uv = pos * 0.5 + 0.5;
    v_uv.y = 1.0 - v_uv.y;
    gl_Position = vec4(pos, 0.0, 1.0);
}
"#;

const FRAG_PREAMBLE: &str = r#"
#version 300 es
precision mediump float;
uniform sampler2D u_tex;
uniform float     u_time;
uniform vec2      u_resolution;
uniform vec2      u_mouse;
in  vec2 v_uv;
out vec4 fragColor;
"#;

// ── compiled GL program ───────────────────────────────────────────────────────

struct GlProgram {
    id: ffi::types::GLuint,
    loc_tex: ffi::types::GLint,
    loc_time: ffi::types::GLint,
    loc_resolution: ffi::types::GLint,
    loc_mouse: ffi::types::GLint,
    user_locs: HashMap<String, ffi::types::GLint>,
}

impl GlProgram {
    unsafe fn compile(frag_source: &str, user_uniform_names: &[&str]) -> Result<Self, String> {
        let gl = gl_fns();

        let vert = compile_shader(&gl, ffi::VERTEX_SHADER, VERT_SRC)?;
        let full_frag = format!("{FRAG_PREAMBLE}\n{frag_source}");
        let frag = compile_shader(&gl, ffi::FRAGMENT_SHADER, &full_frag)?;

        let prog = gl.CreateProgram();
        gl.AttachShader(prog, vert);
        gl.AttachShader(prog, frag);
        gl.LinkProgram(prog);
        gl.DeleteShader(vert);
        gl.DeleteShader(frag);

        let mut ok = 0;
        gl.GetProgramiv(prog, ffi::LINK_STATUS, &mut ok);
        if ok == 0 {
            let mut len = 0;
            gl.GetProgramiv(prog, ffi::INFO_LOG_LENGTH, &mut len);
            let mut buf = vec![0u8; len as usize];
            gl.GetProgramInfoLog(prog, len, std::ptr::null_mut(), buf.as_mut_ptr() as *mut _);
            gl.DeleteProgram(prog);
            return Err(String::from_utf8_lossy(&buf).into_owned());
        }

        let loc = |name: &str| {
            let c = CString::new(name).unwrap();
            gl.GetUniformLocation(prog, c.as_ptr())
        };
        let user_locs = user_uniform_names
            .iter()
            .map(|&n| (n.to_owned(), loc(n)))
            .collect();

        Ok(Self {
            id: prog,
            loc_tex: loc("u_tex"),
            loc_time: loc("u_time"),
            loc_resolution: loc("u_resolution"),
            loc_mouse: loc("u_mouse"),
            user_locs,
        })
    }

    unsafe fn delete(&self) {
        gl_fns().DeleteProgram(self.id);
    }
}

// ── ping-pong FBO pair ────────────────────────────────────────────────────────

struct FboPair {
    fbos: [ffi::types::GLuint; 2],
    textures: [ffi::types::GLuint; 2],
    width: u32,
    height: u32,
}

impl FboPair {
    unsafe fn new(width: u32, height: u32) -> Result<Self, String> {
        let gl = gl_fns();
        let mut fbos = [0u32; 2];
        let mut textures = [0u32; 2];

        gl.GenFramebuffers(2, fbos.as_mut_ptr());
        gl.GenTextures(2, textures.as_mut_ptr());

        for i in 0..2 {
            gl.BindTexture(ffi::TEXTURE_2D, textures[i]);
            gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MIN_FILTER, ffi::LINEAR as i32);
            gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MAG_FILTER, ffi::LINEAR as i32);
            gl.TexParameteri(
                ffi::TEXTURE_2D,
                ffi::TEXTURE_WRAP_S,
                ffi::CLAMP_TO_EDGE as i32,
            );
            gl.TexParameteri(
                ffi::TEXTURE_2D,
                ffi::TEXTURE_WRAP_T,
                ffi::CLAMP_TO_EDGE as i32,
            );
            gl.TexImage2D(
                ffi::TEXTURE_2D,
                0,
                ffi::RGBA8 as i32,
                width as i32,
                height as i32,
                0,
                ffi::RGBA,
                ffi::UNSIGNED_BYTE,
                std::ptr::null(),
            );
            gl.BindFramebuffer(ffi::FRAMEBUFFER, fbos[i]);
            gl.FramebufferTexture2D(
                ffi::FRAMEBUFFER,
                ffi::COLOR_ATTACHMENT0,
                ffi::TEXTURE_2D,
                textures[i],
                0,
            );
            let status = gl.CheckFramebufferStatus(ffi::FRAMEBUFFER);
            if status != ffi::FRAMEBUFFER_COMPLETE {
                return Err(format!("FBO {i} incomplete: 0x{status:X}"));
            }
        }

        gl.BindFramebuffer(ffi::FRAMEBUFFER, 0);
        gl.BindTexture(ffi::TEXTURE_2D, 0);
        Ok(Self {
            fbos,
            textures,
            width,
            height,
        })
    }

    unsafe fn delete(&self) {
        let gl = gl_fns();
        gl.DeleteFramebuffers(2, self.fbos.as_ptr());
        gl.DeleteTextures(2, self.textures.as_ptr());
    }
}

// ── intermediate scene FBO ────────────────────────────────────────────────────
// This is the FBO we redirect render_frame into. It is a plain RGBA8 FBO that
// GlesRenderer can bind to via raw GL — we store the id and bind it manually
// before calling render_frame, then unbind after.

struct SceneFbo {
    fbo: ffi::types::GLuint,
    tex: ffi::types::GLuint,
    width: u32,
    height: u32,
}

impl SceneFbo {
    unsafe fn new(width: u32, height: u32) -> Result<Self, String> {
        let gl = gl_fns();
        let mut tex = 0u32;
        let mut fbo = 0u32;

        gl.GenTextures(1, &mut tex);
        gl.BindTexture(ffi::TEXTURE_2D, tex);
        gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MIN_FILTER, ffi::LINEAR as i32);
        gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MAG_FILTER, ffi::LINEAR as i32);
        gl.TexParameteri(
            ffi::TEXTURE_2D,
            ffi::TEXTURE_WRAP_S,
            ffi::CLAMP_TO_EDGE as i32,
        );
        gl.TexParameteri(
            ffi::TEXTURE_2D,
            ffi::TEXTURE_WRAP_T,
            ffi::CLAMP_TO_EDGE as i32,
        );
        gl.TexImage2D(
            ffi::TEXTURE_2D,
            0,
            ffi::RGBA8 as i32,
            width as i32,
            height as i32,
            0,
            ffi::RGBA,
            ffi::UNSIGNED_BYTE,
            std::ptr::null(),
        );

        gl.GenFramebuffers(1, &mut fbo);
        gl.BindFramebuffer(ffi::FRAMEBUFFER, fbo);
        gl.FramebufferTexture2D(
            ffi::FRAMEBUFFER,
            ffi::COLOR_ATTACHMENT0,
            ffi::TEXTURE_2D,
            tex,
            0,
        );

        let status = gl.CheckFramebufferStatus(ffi::FRAMEBUFFER);
        gl.BindFramebuffer(ffi::FRAMEBUFFER, 0);
        gl.BindTexture(ffi::TEXTURE_2D, 0);

        if status != ffi::FRAMEBUFFER_COMPLETE {
            return Err(format!("Scene FBO incomplete: 0x{status:X}"));
        }

        Ok(Self {
            fbo,
            tex,
            width,
            height,
        })
    }

    unsafe fn delete(&self) {
        let gl = gl_fns();
        gl.DeleteFramebuffers(1, &self.fbo);
        gl.DeleteTextures(1, &self.tex);
    }
}

// ── ShaderPass ────────────────────────────────────────────────────────────────

pub struct ShaderPass {
    programs: HashMap<String, GlProgram>,
    ping_pong: Option<FboPair>,
    scene: Option<SceneFbo>,
    /// FBO id that was bound before we replaced it, restored after render_frame.
    saved_drm_fbo: ffi::types::GLuint,
    size: (u32, u32),
    start: Instant,
}

impl ShaderPass {
    pub fn new(start: Instant) -> Self {
        Self {
            programs: HashMap::new(),
            ping_pong: None,
            scene: None,
            saved_drm_fbo: 0,
            size: (0, 0),
            start,
        }
    }

    pub fn sync_programs(&mut self, registry: &ShaderRegistry) {
        self.programs
            .retain(|name, _| registry.entries.iter().any(|e| &e.name == name));
        for entry in &registry.entries {
            if !self.programs.contains_key(&entry.name) {
                self.compile_shader_entry(entry);
            }
        }
    }

    pub fn recompile_shader(&mut self, registry: &ShaderRegistry, name: &str) {
        if let Some(entry) = registry.entries.iter().find(|e| e.name == name) {
            if let Some(old) = self.programs.remove(name) {
                unsafe { old.delete() };
            }
            self.compile_shader_entry(entry);
        }
    }

    fn compile_shader_entry(&mut self, entry: &crate::shader_config::ShaderEntry) {
        let user_names: Vec<&str> = entry.uniforms.keys().map(|s| s.as_str()).collect();
        match unsafe { GlProgram::compile(&entry.source, &user_names) } {
            Ok(prog) => {
                tracing::info!("Compiled shader '{}'", entry.name);
                self.programs.insert(entry.name.clone(), prog);
            }
            Err(e) => tracing::error!("Shader '{}' compile error:\n{e}", entry.name),
        }
    }

    // ── ensure resources ──────────────────────────────────────────────────────

    unsafe fn ensure_resources(&mut self, width: u32, height: u32) -> bool {
        if self.size == (width, height) && self.scene.is_some() && self.ping_pong.is_some() {
            return true;
        }

        // Drop old resources on resize.
        if let Some(s) = self.scene.take() {
            s.delete();
        }
        if let Some(p) = self.ping_pong.take() {
            p.delete();
        }

        let scene = match SceneFbo::new(width, height) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("{e}");
                return false;
            }
        };
        let pp = match FboPair::new(width, height) {
            Ok(p) => p,
            Err(e) => {
                tracing::error!("{e}");
                scene.delete();
                return false;
            }
        };

        self.scene = Some(scene);
        self.ping_pong = Some(pp);
        self.size = (width, height);
        true
    }

    // ── begin ─────────────────────────────────────────────────────────────────
    //
    // Call BEFORE render_frame. If shaders are active, redirects the renderer
    // to our intermediate FBO and returns true. render_frame will then paint
    // into our texture instead of the DRM buffer.
    //
    // If no shaders are active, does nothing and returns false — caller skips
    // the end() call entirely.

    pub fn begin(&mut self, width: u32, height: u32, registry: &ShaderRegistry) -> bool {
        if !registry.any_active() {
            return false;
        }

        unsafe {
            if !self.ensure_resources(width, height) {
                return false;
            }

            let scene = self.scene.as_ref().unwrap();

            // Save the current draw FBO (the DRM scanout buffer's FBO that
            // DrmCompositor has set up). We need it in end() to write the
            // final result back.
            let mut current: ffi::types::GLint = 0;
            gl_fns().GetIntegerv(ffi::DRAW_FRAMEBUFFER_BINDING, &mut current);
            self.saved_drm_fbo = current as ffi::types::GLuint;

            // Redirect rendering into our scene FBO.
            gl_fns().BindFramebuffer(ffi::FRAMEBUFFER, scene.fbo);
        }

        true
    }

    // ── end ───────────────────────────────────────────────────────────────────
    //
    // Call AFTER render_frame succeeds, BEFORE queue_frame.
    // Restores the DRM FBO binding, runs shaders over the scene texture,
    // then blits the result into the DRM FBO.

    pub fn end(&mut self, width: u32, height: u32, mouse: (f32, f32), registry: &ShaderRegistry) {
        let enabled: Vec<_> = registry.enabled().collect();
        if enabled.is_empty() {
            return;
        }

        unsafe {
            let gl = gl_fns();
            let drm_fbo = self.saved_drm_fbo;

            // Restore DRM FBO so DrmCompositor's state is intact for queue_frame.
            gl.BindFramebuffer(ffi::FRAMEBUFFER, drm_fbo);

            let scene = match self.scene.as_ref() {
                Some(s) => s,
                None => return,
            };
            let pp = match self.ping_pong.as_ref() {
                Some(p) => p,
                None => return,
            };

            let time = self.start.elapsed().as_secs_f32();

            // ── ping-pong shader passes ───────────────────────────────────────
            // First pass reads from the scene texture (fbo[0] is pre-loaded
            // via blit from scene). Actually we use the scene texture directly
            // as the first source — no extra blit needed.

            // Blit scene into ping-pong[0] as first pass input.
            gl.BindFramebuffer(ffi::READ_FRAMEBUFFER, scene.fbo);
            gl.BindFramebuffer(ffi::DRAW_FRAMEBUFFER, pp.fbos[0]);
            gl.BlitFramebuffer(
                0,
                0,
                width as i32,
                height as i32,
                0,
                0,
                width as i32,
                height as i32,
                ffi::COLOR_BUFFER_BIT,
                ffi::NEAREST,
            );

            let mut src_idx = 0usize;

            for entry in &enabled {
                let Some(prog) = self.programs.get(&entry.name) else {
                    tracing::warn!(
                        "Shader '{}' enabled but not compiled — skipping",
                        entry.name
                    );
                    continue;
                };

                let dst_idx = 1 - src_idx;

                gl.BindFramebuffer(ffi::FRAMEBUFFER, pp.fbos[dst_idx]);
                gl.Viewport(0, 0, width as i32, height as i32);
                gl.UseProgram(prog.id);

                gl.ActiveTexture(ffi::TEXTURE0);
                gl.BindTexture(ffi::TEXTURE_2D, pp.textures[src_idx]);
                if prog.loc_tex >= 0 {
                    gl.Uniform1i(prog.loc_tex, 0);
                }
                if prog.loc_time >= 0 {
                    gl.Uniform1f(prog.loc_time, time);
                }
                if prog.loc_resolution >= 0 {
                    gl.Uniform2f(prog.loc_resolution, width as f32, height as f32);
                }
                if prog.loc_mouse >= 0 {
                    gl.Uniform2f(prog.loc_mouse, mouse.0, mouse.1);
                }

                for (name, &value) in &entry.uniforms {
                    if let Some(&loc) = prog.user_locs.get(name) {
                        if loc >= 0 {
                            gl.Uniform1f(loc, value);
                        }
                    }
                }

                gl.DrawArrays(ffi::TRIANGLES, 0, 3);
                src_idx = dst_idx;
            }

            // ── blit final result into DRM FBO ────────────────────────────────
            // Write-only into the DRM buffer — we never read back from it.
            // Blit with Y-flip: the DRM buffer expects bottom-up, our FBOs
            // are also bottom-up, so coords are straight (no flip needed).
            gl.BindFramebuffer(ffi::READ_FRAMEBUFFER, pp.fbos[src_idx]);
            gl.BindFramebuffer(ffi::DRAW_FRAMEBUFFER, drm_fbo);
            gl.BlitFramebuffer(
                0,
                0,
                width as i32,
                height as i32,
                0,
                0,
                width as i32,
                height as i32,
                ffi::COLOR_BUFFER_BIT,
                ffi::NEAREST,
            );

            // Restore clean state.
            gl.BindFramebuffer(ffi::FRAMEBUFFER, drm_fbo);
            gl.UseProgram(0);
            gl.BindTexture(ffi::TEXTURE_2D, 0);
        }
    }
}

impl Drop for ShaderPass {
    fn drop(&mut self) {
        for (_, prog) in self.programs.drain() {
            unsafe { prog.delete() };
        }
        unsafe {
            if let Some(s) = self.scene.take() {
                s.delete();
            }
            if let Some(p) = self.ping_pong.take() {
                p.delete();
            }
        }
    }
}

// ── GL helpers ────────────────────────────────────────────────────────────────

unsafe fn gl_fns() -> ffi::Gles2 {
    ffi::Gles2::load_with(|s| {
        let c = CString::new(s).unwrap();
        smithay::backend::egl::ffi::egl::GetProcAddress(c.as_ptr()) as *const _
    })
}

unsafe fn compile_shader(
    gl: &ffi::Gles2,
    kind: ffi::types::GLenum,
    src: &str,
) -> Result<ffi::types::GLuint, String> {
    let shader = gl.CreateShader(kind);
    let c_src = CString::new(src).unwrap();
    let src_ptr = c_src.as_ptr();
    gl.ShaderSource(shader, 1, &src_ptr, std::ptr::null());
    gl.CompileShader(shader);

    let mut ok = 0;
    gl.GetShaderiv(shader, ffi::COMPILE_STATUS, &mut ok);
    if ok == 0 {
        let mut len = 0;
        gl.GetShaderiv(shader, ffi::INFO_LOG_LENGTH, &mut len);
        let mut buf = vec![0u8; len as usize];
        gl.GetShaderInfoLog(
            shader,
            len,
            std::ptr::null_mut(),
            buf.as_mut_ptr() as *mut _,
        );
        gl.DeleteShader(shader);
        return Err(String::from_utf8_lossy(&buf).into_owned());
    }
    Ok(shader)
}
