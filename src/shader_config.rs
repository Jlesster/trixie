// shader_config.rs — shaders.json parser and shader registry
//
// Expected file: ~/.config/trixie/shaders.json
//
// Example shape:
// {
//   "shaders": [
//     {
//       "name": "crt",
//       "enabled": true,
//       "path": "~/.config/trixie/shaders/crt.glsl",
//       "uniforms": {
//         "curvature": 3.0,
//         "scanline_intensity": 0.4
//       }
//     },
//     {
//       "name": "chromatic",
//       "enabled": false,
//       "path": "~/.config/trixie/shaders/chromatic.glsl",
//       "uniforms": {}
//     }
//   ]
// }
//
// Auto-injected uniforms (do NOT define these in "uniforms" — the renderer
// sets them every frame from compositor state):
//
//   uniform float     u_time;        // seconds since compositor start
//   uniform vec2      u_resolution;  // output size in physical pixels
//   uniform vec2      u_mouse;       // pointer position in physical pixels
//   uniform sampler2D u_tex;         // composited framebuffer texture
//
// Vertex stage provides:
//
//   in vec2 v_uv;   // [0,1] UV over the full output
//
// Minimal pass-through fragment shader:
//
//   void main() {
//       fragColor = texture(u_tex, v_uv);
//   }

use serde::Deserialize;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::SystemTime,
};

use crate::config::expand_tilde;

// ── raw deserialization ───────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct RawShaderFile {
    #[serde(default)]
    shaders: Vec<RawShaderEntry>,
}

#[derive(Debug, Deserialize)]
struct RawShaderEntry {
    name: String,
    #[serde(default = "bool_true")]
    enabled: bool,
    path: String,
    #[serde(default)]
    uniforms: HashMap<String, f32>,
}

fn bool_true() -> bool {
    true
}

// ── public types ──────────────────────────────────────────────────────────────

/// A fully resolved, source-loaded shader entry.
#[derive(Debug, Clone)]
pub struct ShaderEntry {
    /// Identifier used by the ratatui UI and IPC socket.
    pub name: String,
    /// Whether this shader is active in the post-process chain.
    pub enabled: bool,
    /// Absolute path to the .glsl file on disk.
    pub path: PathBuf,
    /// Raw GLSL fragment source. Loaded at startup and on hot-reload.
    pub source: String,
    /// User-defined uniform overrides. Auto-injected names are rejected at
    /// load time to avoid silent conflicts.
    pub uniforms: HashMap<String, f32>,
    /// mtime at last successful load, used for stale-check without inotify.
    pub last_modified: Option<SystemTime>,
}

impl ShaderEntry {
    /// True if the file on disk is newer than our cached mtime.
    pub fn is_stale(&self) -> bool {
        let Ok(meta) = std::fs::metadata(&self.path) else {
            return false;
        };
        let Ok(mtime) = meta.modified() else {
            return false;
        };
        self.last_modified.map_or(true, |lm| mtime > lm)
    }

    /// Re-read source from disk in place.
    /// Returns `Ok(true)` if the content changed, `Ok(false)` if unchanged.
    pub fn reload(&mut self) -> Result<bool, std::io::Error> {
        let meta = std::fs::metadata(&self.path)?;
        let mtime = meta.modified().ok();

        // If mtime is identical there is nothing to do.
        if self.last_modified == mtime && mtime.is_some() {
            return Ok(false);
        }

        let new_source = std::fs::read_to_string(&self.path)?;
        let changed = new_source != self.source;
        self.source = new_source;
        self.last_modified = mtime;
        Ok(changed)
    }
}

// ── registry ──────────────────────────────────────────────────────────────────

/// Ordered collection of all shaders defined in shaders.json.
/// Shaders are applied in declaration order when chaining multiple passes.
#[derive(Debug, Default, Clone)]
pub struct ShaderRegistry {
    pub entries: Vec<ShaderEntry>,
}

// Names the renderer injects automatically — block users from shadowing them.
const RESERVED_UNIFORMS: &[&str] = &["u_time", "u_resolution", "u_mouse", "u_tex"];

impl ShaderRegistry {
    /// Load from `<config_dir>/shaders.json`.
    /// A missing file is treated as an empty registry, not an error.
    pub fn load(config_dir: &Path) -> Self {
        let path = config_dir.join("shaders.json");
        if !path.exists() {
            tracing::info!(
                "No shaders.json at {} — shader post-processing disabled",
                path.display()
            );
            return Self::default();
        }

        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("Could not read shaders.json: {e}");
                return Self::default();
            }
        };

        let raw: RawShaderFile = match serde_json::from_str(&text) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("JSON parse error in shaders.json: {e}");
                return Self::default();
            }
        };

        let mut entries = Vec::with_capacity(raw.shaders.len());

        for mut raw_entry in raw.shaders {
            let resolved = PathBuf::from(expand_tilde(&raw_entry.path));

            // Strip reserved uniforms from the user map before storing.
            for &reserved in RESERVED_UNIFORMS {
                if raw_entry.uniforms.remove(reserved).is_some() {
                    tracing::warn!(
                        "Shader '{}': uniform '{reserved}' is auto-injected — \
                         removed from user uniforms map",
                        raw_entry.name
                    );
                }
            }

            let (source, last_modified) = match load_source(&resolved) {
                Ok(pair) => pair,
                Err(e) => {
                    tracing::warn!(
                        "Shader '{}': failed to load {}: {e} — skipping",
                        raw_entry.name,
                        resolved.display()
                    );
                    continue;
                }
            };

            if let Err(e) = validate_glsl_source(&source) {
                tracing::warn!("Shader '{}': GLSL validation warning: {e}", raw_entry.name);
                // We don't skip — let the GPU driver be the final arbiter.
            }

            tracing::info!(
                "Shader '{}' registered from {} (enabled={})",
                raw_entry.name,
                resolved.display(),
                raw_entry.enabled,
            );

            entries.push(ShaderEntry {
                name: raw_entry.name,
                enabled: raw_entry.enabled,
                path: resolved,
                source,
                uniforms: raw_entry.uniforms,
                last_modified,
            });
        }

        Self { entries }
    }

    // ── queries ───────────────────────────────────────────────────────────────

    /// Iterator over entries that are currently enabled, in order.
    pub fn enabled(&self) -> impl Iterator<Item = &ShaderEntry> {
        self.entries.iter().filter(|e| e.enabled)
    }

    /// True if at least one shader is enabled.
    pub fn any_active(&self) -> bool {
        self.entries.iter().any(|e| e.enabled)
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut ShaderEntry> {
        self.entries.iter_mut().find(|e| e.name == name)
    }

    // ── mutations (used by IPC / ratatui app) ─────────────────────────────────

    /// Toggle a shader by name. Returns the new enabled state or None if the
    /// name was not found.
    pub fn toggle(&mut self, name: &str) -> Option<bool> {
        let entry = self.get_mut(name)?;
        entry.enabled = !entry.enabled;
        tracing::info!("Shader '{}' toggled → enabled={}", name, entry.enabled);
        Some(entry.enabled)
    }

    pub fn set_enabled(&mut self, name: &str, enabled: bool) -> Option<()> {
        let entry = self.get_mut(name)?;
        entry.enabled = enabled;
        tracing::info!("Shader '{}' set enabled={}", name, enabled);
        Some(())
    }

    // ── hot reload ────────────────────────────────────────────────────────────

    /// Poll every entry for file changes and reload stale sources in place.
    /// Returns the names of shaders whose source text actually changed —
    /// the caller must recompile those GPU programs.
    pub fn hot_reload(&mut self) -> Vec<String> {
        let mut changed = Vec::new();
        for entry in &mut self.entries {
            if !entry.is_stale() {
                continue;
            }
            match entry.reload() {
                Ok(true) => {
                    tracing::info!("Shader '{}' hot-reloaded", entry.name);
                    changed.push(entry.name.clone());
                }
                Ok(false) => {}
                Err(e) => tracing::warn!("Shader '{}' reload error: {e}", entry.name),
            }
        }
        changed
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn load_source(path: &Path) -> Result<(String, Option<SystemTime>), std::io::Error> {
    let meta = std::fs::metadata(path)?;
    let mtime = meta.modified().ok();
    let source = std::fs::read_to_string(path)?;
    Ok((source, mtime))
}

/// Lightweight pre-flight check — not a real GLSL compiler.
/// Catches the most common mistakes before sending source to the GPU driver.
pub fn validate_glsl_source(source: &str) -> Result<(), String> {
    if !source.contains("void main()") && !source.contains("void main ()") {
        return Err("missing 'void main()' entry point".into());
    }
    if !source.contains("fragColor") {
        return Err("missing write to 'fragColor'".into());
    }
    Ok(())
}
