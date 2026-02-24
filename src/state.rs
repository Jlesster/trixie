// state.rs — KittyCompositor struct, per-GPU/output data, and render methods

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use smithay::{
    backend::{
        allocator::gbm::GbmDevice,
        drm::{
            compositor::DrmCompositor, exporter::gbm::GbmFramebufferExporter, DrmDevice,
            DrmDeviceFd, DrmNode,
        },
        renderer::{damage::OutputDamageTracker, gles::GlesRenderer},
        session::libseat::LibSeatSession,
    },
    desktop::{
        space::{space_render_elements, SpaceRenderElements},
        PopupManager, Space, Window,
    },
    input::{
        pointer::{CursorImageStatus, PointerHandle},
        Seat, SeatState,
    },
    output::Output,
    reexports::{
        calloop::LoopHandle,
        drm::control::crtc,
        input::Libinput,
        wayland_server::{backend::ClientId, protocol::wl_surface::WlSurface, DisplayHandle},
    },
    utils::{Clock, Monotonic},
    wayland::{
        compositor::CompositorState,
        dmabuf::{DmabufGlobal, DmabufState},
        selection::{data_device::DataDeviceState, primary_selection::PrimarySelectionState},
        shell::{
            wlr_layer::WlrLayerShellState,
            xdg::{decoration::XdgDecorationState, XdgShellState},
        },
        shm::ShmState,
    },
};

use crate::config::{Config, FloatingMarker};
use crate::render::surface_under;

// ── type alias ────────────────────────────────────────────────────────────────

pub type GbmDrmCompositor =
    DrmCompositor<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>;

use smithay::backend::allocator::gbm::GbmAllocator;
use smithay::backend::drm::compositor::FrameFlags;
use smithay::backend::renderer::{element::surface::WaylandSurfaceRenderElement, ImportDma};

// ── markers ───────────────────────────────────────────────────────────────────

pub struct RulesApplied;

// ── per-output data ───────────────────────────────────────────────────────────

pub struct SurfaceData {
    pub output: Output,
    pub compositor: GbmDrmCompositor,
    pub damage_tracker: OutputDamageTracker,
    pub next_frame_time: Instant,
    pub pending_frame: bool,
    pub frame_duration: Duration,
}

// ── per-GPU data ──────────────────────────────────────────────────────────────

pub struct BackendData {
    pub surfaces: HashMap<crtc::Handle, SurfaceData>,
    pub renderer: GlesRenderer,
    pub gbm: GbmDevice<DrmDeviceFd>,
    pub drm: DrmDevice,
    pub drm_node: DrmNode,
}

// ── client state ──────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct ClientState {
    pub compositor: smithay::wayland::compositor::CompositorClientState,
}

impl smithay::reexports::wayland_server::backend::ClientData for ClientState {
    fn initialized(&self, _: ClientId) {}
    fn disconnected(
        &self,
        _: ClientId,
        _: smithay::reexports::wayland_server::backend::DisconnectReason,
    ) {
    }
}

// ── compositor state ──────────────────────────────────────────────────────────

pub struct KittyCompositor {
    pub display_handle: DisplayHandle,
    pub running: Arc<AtomicBool>,
    pub handle: LoopHandle<'static, Self>,
    pub clock: Clock<Monotonic>,
    pub config: Config,
    pub libinput: Libinput,

    pub compositor_state: CompositorState,
    pub shm_state: ShmState,
    pub dmabuf_state: DmabufState,
    pub dmabuf_global: Option<DmabufGlobal>,
    pub output_manager_state: smithay::wayland::output::OutputManagerState,
    pub seat_state: SeatState<Self>,
    pub data_device_state: DataDeviceState,
    pub primary_selection_state: PrimarySelectionState,
    pub xdg_shell_state: XdgShellState,
    pub layer_shell_state: WlrLayerShellState,
    pub xdg_decoration_state: XdgDecorationState,
    pub popups: PopupManager,

    pub space: Space<Window>,
    pub seat: Seat<Self>,
    pub pointer: PointerHandle<Self>,
    pub cursor_status: CursorImageStatus,

    pub session: LibSeatSession,
    pub backends: HashMap<DrmNode, BackendData>,
    pub primary_gpu: DrmNode,
    pub wayland_socket: String,
    pub exec_once_done: bool,
}

// ── render ────────────────────────────────────────────────────────────────────

impl KittyCompositor {
    pub fn render_all(&mut self) {
        let nodes: Vec<_> = self.backends.keys().copied().collect();
        for node in nodes {
            let crtcs: Vec<_> = self.backends[&node].surfaces.keys().copied().collect();
            for crtc in crtcs {
                self.render_surface(node, crtc);
            }
        }
    }

    pub fn render_surface(&mut self, node: DrmNode, crtc: crtc::Handle) {
        let now = Instant::now();
        {
            let sd = self.backends.get(&node).and_then(|b| b.surfaces.get(&crtc));
            if let Some(s) = sd {
                if s.pending_frame || now < s.next_frame_time {
                    return;
                }
            }
        }

        let (output, bg, frame_duration) = {
            let b = match self.backends.get(&node) {
                Some(b) => b,
                None => return,
            };
            let s = match b.surfaces.get(&crtc) {
                Some(s) => s,
                None => return,
            };
            (
                s.output.clone(),
                self.config.background_color,
                s.frame_duration,
            )
        };

        let output_size = output
            .current_mode()
            .map(|m| m.size.to_logical(1))
            .unwrap_or_default();

        // ── resize tiled windows ──────────────────────────────────────────────
        let needs_resize: Vec<Window> = self
            .space
            .elements()
            .filter(|w| w.user_data().get::<FloatingMarker>().is_none())
            .filter(|w| {
                w.toplevel()
                    .map(|tl| tl.with_pending_state(|s| s.size != Some(output_size)))
                    .unwrap_or(false)
            })
            .cloned()
            .collect();

        for window in needs_resize {
            if let Some(tl) = window.toplevel() {
                tl.with_pending_state(|s| s.size = Some(output_size));
                tl.send_pending_configure();
            }
            if self.space.element_location(&window).is_none() {
                self.space.map_element(window, (0, 0), false);
            }
        }

        // ── build render elements and submit frame ────────────────────────────
        let backend = match self.backends.get_mut(&node) {
            Some(b) => b,
            None => return,
        };
        let sd = match backend.surfaces.get_mut(&crtc) {
            Some(s) => s,
            None => return,
        };

        let elements: Vec<
            SpaceRenderElements<GlesRenderer, WaylandSurfaceRenderElement<GlesRenderer>>,
        > = match space_render_elements(&mut backend.renderer, [&self.space], &output, 1.0) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("render_elements: {e}");
                return;
            }
        };

        let frame_flags = FrameFlags::empty();

        match sd
            .compositor
            .render_frame(&mut backend.renderer, &elements, bg, frame_flags)
        {
            Ok(frame) => {
                if !frame.is_empty {
                    match sd.compositor.queue_frame(()) {
                        Ok(()) => sd.pending_frame = true,
                        Err(e) => tracing::warn!("queue_frame: {e}"),
                    }
                }

                let clock_now = self.clock.now();
                for w in self.space.elements() {
                    w.send_frame(&output, clock_now, Some(frame_duration), |_, _| {
                        Some(output.clone())
                    });
                }

                sd.next_frame_time += frame_duration;
                if sd.next_frame_time < now {
                    sd.next_frame_time = now + frame_duration;
                }
            }
            Err(e) => tracing::warn!("render_frame: {e}"),
        }
    }

    pub fn frame_finish(&mut self, node: DrmNode, crtc: crtc::Handle) {
        if let Some(b) = self.backends.get_mut(&node) {
            if let Some(s) = b.surfaces.get_mut(&crtc) {
                if let Err(e) = s.compositor.frame_submitted() {
                    tracing::warn!("frame_submitted: {e}");
                }
                s.pending_frame = false;
            }
        }
    }
}
