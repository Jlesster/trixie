// embedded_window.rs — render embedded Wayland clients (Firefox etc.) directly
// into the compositor framebuffer as textured quads, and expose a SharedFrame
// for the TWM-side KGP encoder.
//
// Flow:
//   1. TWM sends Spawn via embed IPC → compositor launches Firefox with
//      WAYLAND_DISPLAY pointing at our socket.
//   2. Firefox connects as a normal xdg_toplevel client.
//   3. handlers.rs::new_toplevel calls EmbeddedManager::try_claim() — if
//      the app_id matches a pending placement, the surface is "claimed" and
//      removed from Space (so tiling logic never sees it).
//   4. On every wl_surface.commit the buffer is imported into a GlesTexture,
//      then read back into the SharedFrame for TWM-side KGP encoding.
//   5. render_surface collects EmbeddedRenderElement instances and pushes them
//      below normal Space elements.
//   6. When the tile resizes, TWM sends Move → compositor calls
//      EmbeddedManager::update_placement() + sends xdg configure.

use std::collections::HashMap;

use smithay::{
    backend::renderer::{
        element::{Element, Id, Kind, RenderElement, UnderlyingStorage},
        gles::{ffi as gl, GlesError, GlesFrame, GlesRenderer, GlesTexture},
        utils::{CommitCounter, DamageSet, OpaqueRegions},
        ImportMemWl,
    },
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Buffer, Physical, Point, Rectangle, Scale, Size, Transform},
    wayland::shell::xdg::ToplevelSurface,
};

use crate::embedded_surface::{FrameBuffer, SharedFrame};

// ── Placement ─────────────────────────────────────────────────────────────────

/// Pixel-space bounding rect for an embedded surface on the output.
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

// ── Pending placement ─────────────────────────────────────────────────────────

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
}

// ── Manager ───────────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct EmbeddedManager {
    /// Live embedded surfaces, keyed by app_id.
    pub entries: HashMap<String, EmbeddedEntry>,
    /// Placements reserved before the app has connected.
    pending: HashMap<String, PendingPlacement>,
    /// CPU-side frame buffers shared with the TWM encoder, keyed by app_id.
    /// The TWM holds a clone of each Arc; the compositor writes on commit.
    pub shared_frames: HashMap<String, SharedFrame>,
}

impl EmbeddedManager {
    // ── pending placement API ─────────────────────────────────────────────────

    /// Reserve a placement for an app that will connect soon.
    /// Called by handle_embed_command(Spawn) before launching the process.
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

    // ── try_claim ─────────────────────────────────────────────────────────────

    /// Called from handlers.rs::new_toplevel.
    /// Returns true if this surface was claimed (caller must NOT map it into Space).
    /// Also registers a SharedFrame so the TWM encoder can read pixels.
    pub fn try_claim(
        &mut self,
        app_id: &str,
        surface: WlSurface,
        toplevel: ToplevelSurface,
    ) -> bool {
        let Some(pending) = self.pending.remove(app_id) else {
            return false;
        };

        tracing::info!(
            "EmbeddedManager: claiming '{}' surface {:?}",
            app_id,
            surface.id(),
        );

        toplevel.with_pending_state(|s| {
            s.size = Some(pending.placement.logical_size());
        });
        toplevel.send_configure();

        self.entries.insert(
            app_id.to_owned(),
            EmbeddedEntry {
                surface,
                toplevel,
                placement: pending.placement,
                texture: None,
                commit_counter: CommitCounter::default(),
                mapped: false,
            },
        );

        // Register a fresh SharedFrame for this surface.
        self.shared_frames
            .entry(app_id.to_owned())
            .or_insert_with(SharedFrame::default);

        true
    }

    // ── commit handler ────────────────────────────────────────────────────────

    /// Import the committed buffer into a GL texture and read pixels back into
    /// the SharedFrame so the TWM encoder can consume them.
    ///
    /// Call this for every wl_surface commit; non-embedded surfaces are ignored.
    pub fn on_commit(&mut self, renderer: &mut GlesRenderer, surface: &WlSurface) {
        // Find which app_id this surface belongs to.
        let app_id = self
            .entries
            .iter()
            .find(|(_, e)| &e.surface == surface)
            .map(|(id, _)| id.clone());

        let Some(app_id) = app_id else { return };

        // Import GL texture.
        match renderer.import_surface(surface, None) {
            Ok(tex) => {
                let w = tex.width();
                let h = tex.height();

                // Readback pixels into SharedFrame before we move the texture.
                if let Some(sf) = self.shared_frames.get(&app_id) {
                    let mut pixels = vec![0u8; (w * h * 4) as usize];
                    unsafe {
                        readback_texture(tex.tex_id(), w, h, &mut pixels);
                    }
                    let mut guard = sf.lock().unwrap();
                    let serial = guard.as_ref().map_or(1, |f| f.serial + 1);
                    *guard = Some(FrameBuffer {
                        pixels,
                        width: w,
                        height: h,
                        serial,
                    });
                }

                if let Some(entry) = self.entries.get_mut(&app_id) {
                    entry.texture = Some(tex);
                    entry.commit_counter.increment();
                    entry.mapped = true;
                    tracing::trace!(
                        "EmbeddedManager: imported texture for {:?} ({}x{})",
                        surface.id(),
                        w,
                        h,
                    );
                }
            }
            Err(e) => {
                tracing::warn!("EmbeddedManager: import_surface failed: {e}");
            }
        }
    }

    // ── resize / move ─────────────────────────────────────────────────────────

    /// Update the on-screen placement and send an xdg configure to the client.
    pub fn update_placement(&mut self, app_id: &str, placement: EmbeddedPlacement) {
        if let Some(entry) = self.entries.get_mut(app_id) {
            entry.placement = placement;
            entry.toplevel.with_pending_state(|s| {
                s.size = Some(placement.logical_size());
            });
            if entry.toplevel.is_initial_configure_sent() {
                entry.toplevel.send_pending_configure();
            }
            tracing::debug!(
                "EmbeddedManager: updated placement for '{}' → {}x{}+{},{}",
                app_id,
                placement.w,
                placement.h,
                placement.x,
                placement.y,
            );
        } else if let Some(p) = self.pending.get_mut(app_id) {
            // App not yet connected — update the pending reservation.
            p.placement = placement;
        }
    }

    // ── removal ───────────────────────────────────────────────────────────────

    pub fn remove(&mut self, app_id: &str) {
        self.entries.remove(app_id);
        self.pending.remove(app_id);
        self.shared_frames.remove(app_id);
    }

    // ── queries ───────────────────────────────────────────────────────────────

    /// True if this wl_surface belongs to an embedded entry.
    pub fn is_embedded_surface(&self, surface: &WlSurface) -> bool {
        self.entries.values().any(|e| &e.surface == surface)
    }

    /// Get the SharedFrame for a given app_id so the TWM can hold a clone.
    /// Returns None if the surface hasn't been claimed yet; the TWM should
    /// call this after receiving the Spawn ACK and retry on the next IPC poll
    /// if it gets None (the app may not have connected yet).
    pub fn shared_frame(&self, app_id: &str) -> Option<SharedFrame> {
        self.shared_frames.get(app_id).cloned()
    }

    // ── window status snapshot (for IPC List response) ────────────────────────

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

    // ── render elements ───────────────────────────────────────────────────────

    /// Collect one EmbeddedRenderElement per mapped embedded surface.
    /// Push these before Space elements so they appear behind normal windows.
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

/// Read pixels from a GL texture into `out` (RGBA, row-major) using a
/// temporary FBO. Must be called on the GL thread with the correct EGL
/// context current.
///
/// For high-frequency use (60 fps Firefox), prefer the zero-copy path:
/// share the `GlesTexture` handle directly with the TWM renderer via an
/// `Arc<Mutex<Option<GlesTexture>>>` and skip the CPU readback entirely.
unsafe fn readback_texture(tex_id: u32, w: u32, h: u32, out: &mut Vec<u8>) {
    out.resize((w * h * 4) as usize, 0);

    let mut fbo = 0u32;
    gl::GenFramebuffers(1, &mut fbo);
    gl::BindFramebuffer(gl::FRAMEBUFFER, fbo);
    gl::FramebufferTexture2D(
        gl::FRAMEBUFFER,
        gl::COLOR_ATTACHMENT0,
        gl::TEXTURE_2D,
        tex_id,
        0,
    );

    let status = gl::CheckFramebufferStatus(gl::FRAMEBUFFER);
    if status == gl::FRAMEBUFFER_COMPLETE {
        gl::ReadPixels(
            0,
            0,
            w as i32,
            h as i32,
            gl::RGBA,
            gl::UNSIGNED_BYTE,
            out.as_mut_ptr() as *mut _,
        );
    } else {
        tracing::warn!(
            "EmbeddedManager: readback FBO incomplete (status=0x{:X}) for tex {}",
            status,
            tex_id,
        );
        // Zero the buffer so the TWM doesn't render garbage.
        out.fill(0);
    }

    gl::BindFramebuffer(gl::FRAMEBUFFER, 0);
    gl::DeleteFramebuffers(1, &fbo);
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
        _commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        DamageSet::from_slice(&[self.geometry(scale)])
    }

    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
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
        frame: &mut GlesFrame<'_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), GlesError> {
        frame.render_texture_from_to(&self.texture, src, dst, damage, &[], Transform::Normal, 1.0)
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

// ── Config entry (for general.json) ──────────────────────────────────────────

/// Declares an app that should be embedded rather than tiled/floating.
///
/// Add to general.json:
/// ```json
/// "embedded": [
///   { "app_id": "firefox", "x": 0, "y": 0, "w": 1920, "h": 1080 }
/// ]
/// ```
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
