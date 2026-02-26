use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use crate::config::Config;
use crate::embedded_ipc::{EmbedCommand, EmbedIpcServer};
use crate::embedded_window::{EmbeddedManager, EmbeddedPlacement, EmbeddedRenderElement};
use crate::shader_pass::ShaderPass;

use smithay::{
    backend::renderer::element::AsRenderElements,
    backend::{
        allocator::gbm::{GbmAllocator, GbmDevice},
        drm::{
            compositor::DrmCompositor, exporter::gbm::GbmFramebufferExporter, DrmDevice,
            DrmDeviceFd, DrmNode,
        },
        renderer::{
            damage::OutputDamageTracker,
            element::{solid::SolidColorRenderElement, surface::WaylandSurfaceRenderElement},
            gles::{GlesRenderer, GlesTexture},
            ImportDma,
        },
        session::libseat::LibSeatSession,
    },
    desktop::{PopupManager, Space, Window},
    input::{
        pointer::{CursorImageStatus, PointerHandle},
        Seat, SeatState,
    },
    output::Output,
    reexports::{
        calloop::LoopHandle,
        drm::control::crtc,
        input::Libinput,
        wayland_server::{
            backend::{ClientId, ObjectId},
            protocol::wl_surface::WlSurface,
            DisplayHandle,
        },
    },
    utils::{Clock, Monotonic, SERIAL_COUNTER as SCOUNTER},
    wayland::{
        compositor::CompositorState,
        dmabuf::{DmabufGlobal, DmabufState},
        selection::{data_device::DataDeviceState, primary_selection::PrimarySelectionState},
        shell::{
            wlr_layer::WlrLayerShellState,
            xdg::{decoration::XdgDecorationState, ToplevelSurface, XdgShellState},
        },
        shm::ShmState,
    },
};

// ── type alias ────────────────────────────────────────────────────────────────

pub type GbmDrmCompositor =
    DrmCompositor<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>;

// ── render elements ───────────────────────────────────────────────────────────

smithay::backend::renderer::element::render_elements! {
    pub TrixieRenderElement<=GlesRenderer>;
    Space    = WaylandSurfaceRenderElement<GlesRenderer>,
    Cursor   = SolidColorRenderElement,
    Embedded = EmbeddedRenderElement,
}

// ── mouse mode ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MouseMode {
    #[default]
    Normal,
    Insert,
}

pub const CURSOR_W: i32 = 2;
pub const CURSOR_H: i32 = 16;

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
    pub mouse_mode: MouseMode,

    // ── embedded windows ──────────────────────────────────────────────────────
    pub embedded: EmbeddedManager,
    pub embed_ipc: EmbedIpcServer,
    /// Toplevels whose app_id was not yet known at new_toplevel time.
    /// Keyed by WlSurface ObjectId; retried on each commit.
    pub unclaimed_toplevels: HashMap<ObjectId, ToplevelSurface>,

    pub session: LibSeatSession,
    pub backends: HashMap<DrmNode, BackendData>,
    pub primary_gpu: DrmNode,
    pub wayland_socket: String,
    pub exec_once_done: bool,
    pub shader_pass: ShaderPass,
    pub start_time: Instant,
}

// ── render ────────────────────────────────────────────────────────────────────

impl KittyCompositor {
    pub fn render_all(&mut self) {
        let nodes: Vec<DrmNode> = self.backends.keys().copied().collect();
        for node in nodes {
            let crtcs: Vec<crtc::Handle> = self.backends[&node].surfaces.keys().copied().collect();
            for crtc in crtcs {
                self.render_surface(node, crtc);
            }
        }
    }

    pub fn render_surface(&mut self, node: DrmNode, crtc: crtc::Handle) {
        let now = Instant::now();

        let backend = match self.backends.get_mut(&node) {
            Some(b) => b,
            None => return,
        };
        let surface = match backend.surfaces.get_mut(&crtc) {
            Some(s) => s,
            None => return,
        };

        if surface.pending_frame || now < surface.next_frame_time {
            return;
        }

        let output = surface.output.clone();
        let scale = smithay::utils::Scale::from(output.current_scale().fractional_scale());
        let clear: [f32; 4] = {
            let c = self.config.background_color;
            [c[0], c[1], c[2], 1.0]
        };

        // ── 1. Embedded quads ─────────────────────────────────────────────────
        let embedded_elements: Vec<TrixieRenderElement> = self
            .embedded
            .render_elements()
            .into_iter()
            .map(TrixieRenderElement::Embedded)
            .collect();

        // ── 2. Space elements ─────────────────────────────────────────────────
        // space_render_elements returns a single SpaceRenderElements value,
        // not a Vec — collect via AsRenderElements on each window instead.
        let space_elements: Vec<TrixieRenderElement> = self
            .space
            .elements()
            .flat_map(|w| {
                let loc = self.space.element_location(w).unwrap_or_default();
                w.render_elements::<WaylandSurfaceRenderElement<GlesRenderer>>(
                    &mut backend.renderer,
                    loc.to_physical_precise_round(scale),
                    scale,
                    1.0,
                )
            })
            .map(TrixieRenderElement::Space)
            .collect();

        // ── 3. Merge ──────────────────────────────────────────────────────────
        let mut all: Vec<TrixieRenderElement> = embedded_elements;
        all.extend(space_elements);

        // ── 4. Render frame ───────────────────────────────────────────────────
        let render_result = surface.compositor.render_frame::<_, TrixieRenderElement>(
            &mut backend.renderer,
            &all,
            clear,
            smithay::backend::drm::compositor::FrameFlags::empty(),
        );

        match render_result {
            Ok(frame) => {
                if !frame.is_empty {
                    match surface.compositor.queue_frame(()) {
                        Ok(()) => {
                            surface.pending_frame = true;
                        }
                        Err(e) => tracing::warn!("queue_frame({node},{crtc:?}): {e}"),
                    }
                }
            }
            Err(e) => tracing::warn!("render_frame({node},{crtc:?}): {e}"),
        }

        surface.next_frame_time = now + surface.frame_duration;
    }

    pub fn frame_finish(&mut self, node: DrmNode, crtc: crtc::Handle) {
        if let Some(b) = self.backends.get_mut(&node) {
            if let Some(s) = b.surfaces.get_mut(&crtc) {
                s.pending_frame = false;
                if let Err(e) = s.compositor.frame_submitted() {
                    tracing::warn!("frame_submitted({node},{crtc:?}): {e}");
                }
            }
        }
    }

    // ── IPC ───────────────────────────────────────────────────────────────────

    pub fn process_embed_ipc(&mut self) {
        let cmds = self.embed_ipc.drain();
        for cmd in cmds {
            self.handle_embed_command(cmd);
        }
        let statuses = self.embedded.window_statuses();
        self.embed_ipc.update_windows(statuses);
    }

    fn handle_embed_command(&mut self, cmd: EmbedCommand) {
        match cmd {
            EmbedCommand::Spawn {
                app_id,
                args,
                x,
                y,
                w,
                h,
            } => {
                self.embedded
                    .request_placement(&app_id, EmbeddedPlacement { x, y, w, h });

                let socket = self.wayland_socket.clone();
                tracing::info!("Spawning embedded '{}' on {socket}", app_id);
                if let Err(e) = std::process::Command::new(&app_id)
                    .args(&args)
                    .env("WAYLAND_DISPLAY", &socket)
                    .env("MOZ_ENABLE_WAYLAND", "1")
                    .env("GDK_BACKEND", "wayland")
                    .env("QT_QPA_PLATFORM", "wayland")
                    .spawn()
                {
                    tracing::warn!("Failed to spawn '{}': {e}", app_id);
                }
            }

            EmbedCommand::Move { app_id, x, y, w, h } => {
                self.embedded
                    .update_placement(&app_id, EmbeddedPlacement { x, y, w, h });
            }

            EmbedCommand::Focus { app_id } => {
                let surface = self
                    .embedded
                    .entries
                    .get(&app_id)
                    .map(|e| e.surface.clone());
                if let Some(s) = surface {
                    let serial = SCOUNTER.next_serial();
                    if let Some(kbd) = self.seat.get_keyboard() {
                        kbd.set_focus(self, Some(s), serial);
                    }
                } else {
                    tracing::warn!("Focus: '{}' not found", app_id);
                }
            }

            EmbedCommand::Close { app_id } => {
                if let Some(entry) = self.embedded.entries.get(&app_id) {
                    entry.toplevel.send_close();
                } else {
                    self.embedded.remove(&app_id);
                }
            }

            EmbedCommand::List => {}
        }
    }
}
