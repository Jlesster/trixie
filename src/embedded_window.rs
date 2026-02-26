use std::collections::HashMap;

use smithay::backend::renderer::utils::RendererSurfaceStateUserData;
use smithay::backend::renderer::Renderer;
use smithay::{
    backend::renderer::{
        element::{Element, Id, Kind, RenderElement, UnderlyingStorage},
        gles::{
            ffi, // GL constants: ffi::FRAMEBUFFER, ffi::RGBA, etc.
            GlesError,
            GlesFrame,
            GlesRenderer,
            GlesTexture,
        },
        utils::{CommitCounter, DamageSet, OpaqueRegions},
        Texture,
    },
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Buffer, Physical, Rectangle, Scale, Transform},
    wayland::{compositor::with_states, shell::xdg::ToplevelSurface},
};

use crate::shared_frame_shm::ShmWriter;

// ── Placement ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmbeddedPlacement {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl EmbeddedPlacement {
    pub fn logical_size(&self) -> smithay::utils::Size<i32, smithay::utils::Logical> {
        (self.w, self.h).into()
    }
}

struct PendingPlacement {
    placement: EmbeddedPlacement,
}

// ── Per-surface entry ─────────────────────────────────────────────────────────

pub struct EmbeddedEntry {
    pub surface: WlSurface,
    pub toplevel: ToplevelSurface,
    pub placement: EmbeddedPlacement,
    pub texture: Option<GlesTexture>,
    pub commit_counter: CommitCounter,
    pub mapped: bool,
    pub shm_writer: Option<ShmWriter>,
}

// ── Manager ───────────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct EmbeddedManager {
    pub entries: HashMap<String, EmbeddedEntry>,
    pending: HashMap<String, PendingPlacement>,
}

impl EmbeddedManager {
    pub fn has_pending(&self, app_id: &str) -> bool {
        self.pending.contains_key(app_id)
    }

    pub fn request_placement(&mut self, app_id: &str, placement: EmbeddedPlacement) {
        tracing::debug!(
            "EmbeddedManager: pending placement for '{}' @ {}x{}+{},{}",
            app_id,
            placement.w,
            placement.h,
            placement.x,
            placement.y,
        );
        self.pending
            .insert(app_id.to_owned(), PendingPlacement { placement });
    }

    pub fn try_claim(
        &mut self,
        app_id: &str,
        surface: WlSurface,
        toplevel: ToplevelSurface,
    ) -> bool {
        let Some(pending) = self.pending.remove(app_id) else {
            return false;
        };

        toplevel.with_pending_state(|s| s.size = Some(pending.placement.logical_size()));
        toplevel.send_configure();

        let shm_writer = ShmWriter::create(app_id)
            .map_err(|e| tracing::warn!("shm create '{}': {e}", app_id))
            .ok();

        self.entries.insert(
            app_id.to_owned(),
            EmbeddedEntry {
                surface,
                toplevel,
                placement: pending.placement,
                texture: None,
                commit_counter: CommitCounter::default(),
                mapped: false,
                shm_writer,
            },
        );

        tracing::info!("EmbeddedManager: claimed '{}'", app_id);
        true
    }

    // ── commit ────────────────────────────────────────────────────────────────

    pub fn on_commit(&mut self, renderer: &mut GlesRenderer, surface: &WlSurface) {
        let app_id = self
            .entries
            .iter()
            .find(|(_, e)| &e.surface == surface)
            .map(|(id, _)| id.clone());
        let Some(app_id) = app_id else { return };

        let texture: Option<GlesTexture> = with_states(surface, |states| {
            let data = states.data_map.get::<RendererSurfaceStateUserData>()?;
            let guard = data.lock().ok()?;
            let tex_ref = guard.texture::<GlesTexture>(renderer.context_id())?;
            Some(tex_ref.clone())
        });

        let Some(tex) = texture else {
            tracing::debug!("EmbeddedManager: no texture yet for '{}'", app_id);
            return;
        };

        let w = tex.width();
        let h = tex.height();

        if let Some(entry) = self.entries.get_mut(&app_id) {
            if let Some(ref writer) = entry.shm_writer {
                let mut pixels = vec![0u8; (w * h * 4) as usize];
                // Use GlesRenderer::with_context to safely access the GL
                // function table. This avoids any question of which crate
                // owns the GL function pointers.
                let _ = renderer.with_context(|gl| {
                    unsafe { readback_texture(gl, tex.tex_id(), w, h, &mut pixels) };
                });
                writer.write_frame(&pixels, w, h);
            }

            entry.texture = Some(tex);
            entry.commit_counter.increment();
            entry.mapped = true;
            tracing::trace!("EmbeddedManager: commit '{}' ({}x{})", app_id, w, h);
        }
    }

    pub fn update_placement(&mut self, app_id: &str, placement: EmbeddedPlacement) {
        if let Some(entry) = self.entries.get_mut(app_id) {
            entry.placement = placement;
            entry
                .toplevel
                .with_pending_state(|s| s.size = Some(placement.logical_size()));
            if entry.toplevel.is_initial_configure_sent() {
                entry.toplevel.send_pending_configure();
            }
        } else if let Some(p) = self.pending.get_mut(app_id) {
            p.placement = placement;
        }
    }

    pub fn remove(&mut self, app_id: &str) {
        self.entries.remove(app_id);
        self.pending.remove(app_id);
    }

    pub fn is_embedded_surface(&self, surface: &WlSurface) -> bool {
        self.entries.values().any(|e| &e.surface == surface)
    }

    pub fn window_statuses(&self) -> Vec<crate::embedded_ipc::WindowStatus> {
        self.entries
            .iter()
            .map(|(app_id, e)| crate::embedded_ipc::WindowStatus {
                app_id: app_id.clone(),
                x: e.placement.x,
                y: e.placement.y,
                w: e.placement.w,
                h: e.placement.h,
                mapped: e.mapped,
            })
            .collect()
    }

    pub fn render_elements(&self) -> Vec<EmbeddedRenderElement> {
        self.entries
            .values()
            .filter_map(|e| {
                let tex = e.texture.as_ref()?;
                if !e.mapped {
                    return None;
                }
                Some(EmbeddedRenderElement {
                    id: Id::new(),
                    texture: tex.clone(),
                    placement: e.placement,
                    commit_counter: e.commit_counter,
                })
            })
            .collect()
    }
}

// ── GL texture readback ───────────────────────────────────────────────────────
//
// Called inside renderer.with_context(), which provides the `gl` function
// table as a &ffi::Gles2. The ffi constants (FRAMEBUFFER, RGBA, etc.) come
// from the same module.
//
// Reads rows bottom-to-top and writes them top-to-bottom to flip the
// vertical axis from GL's bottom-left origin to KGP's top-left origin.

unsafe fn readback_texture(gl: &ffi::Gles2, tex_id: u32, w: u32, h: u32, out: &mut Vec<u8>) {
    out.resize((w * h * 4) as usize, 0);

    let mut fbo = 0u32;
    gl.GenFramebuffers(1, &mut fbo);
    gl.BindFramebuffer(ffi::FRAMEBUFFER, fbo);
    gl.FramebufferTexture2D(
        ffi::FRAMEBUFFER,
        ffi::COLOR_ATTACHMENT0,
        ffi::TEXTURE_2D,
        tex_id,
        0,
    );

    let status = gl.CheckFramebufferStatus(ffi::FRAMEBUFFER);
    if status == ffi::FRAMEBUFFER_COMPLETE {
        let row_bytes = (w * 4) as usize;
        let mut row_buf = vec![0u8; row_bytes];

        for y in 0..h {
            gl.ReadPixels(
                0,
                y as i32,
                w as i32,
                1,
                ffi::RGBA,
                ffi::UNSIGNED_BYTE,
                row_buf.as_mut_ptr() as *mut _,
            );
            // GL row 0 is the bottom row — place it at the end of `out`.
            let dst_row = (h - 1 - y) as usize;
            let dst = dst_row * row_bytes;
            out[dst..dst + row_bytes].copy_from_slice(&row_buf);
        }
    } else {
        tracing::warn!(
            "readback_texture: FBO incomplete for tex {} (status={:#x})",
            tex_id,
            status
        );
        out.fill(0);
    }

    gl.BindFramebuffer(ffi::FRAMEBUFFER, 0);
    gl.DeleteFramebuffers(1, &fbo);
}

// ── Render element ────────────────────────────────────────────────────────────

pub struct EmbeddedRenderElement {
    id: Id,
    texture: GlesTexture,
    placement: EmbeddedPlacement,
    commit_counter: CommitCounter,
}

impl Element for EmbeddedRenderElement {
    fn id(&self) -> &Id {
        &self.id
    }
    fn current_commit(&self) -> CommitCounter {
        self.commit_counter
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        Rectangle::from_loc_and_size(
            (0.0, 0.0),
            (self.texture.width() as f64, self.texture.height() as f64),
        )
    }

    fn geometry(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
        Rectangle::from_loc_and_size(
            (self.placement.x, self.placement.y),
            (self.placement.w, self.placement.h),
        )
    }

    fn damage_since(
        &self,
        scale: Scale<f64>,
        _: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        DamageSet::from_slice(&[self.geometry(scale)])
    }

    fn opaque_regions(&self, _: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        OpaqueRegions::default()
    }

    fn alpha(&self) -> f32 {
        1.0
    }
    fn kind(&self) -> Kind {
        Kind::Unspecified
    }
}

impl RenderElement<GlesRenderer> for EmbeddedRenderElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), GlesError> {
        frame.render_texture_from_to(
            &self.texture,
            src,
            dst,
            damage,
            &[],
            Transform::Normal,
            1.0,
            None,
            &[],
        )
    }

    fn underlying_storage(&self, _: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

// ── Config entry ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Deserialize)]
pub struct EmbeddedConfig {
    pub app_id: String,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl EmbeddedConfig {
    pub fn placement(&self) -> EmbeddedPlacement {
        EmbeddedPlacement {
            x: self.x,
            y: self.y,
            w: self.w,
            h: self.h,
        }
    }
}
