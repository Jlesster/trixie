use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use crate::config::{Config, FloatingMarker};
use crate::embedded_ipc::{EmbedCommand, EmbedIpcServer, EmbedResponse, WindowStatus};
use crate::embedded_window::{EmbeddedManager, EmbeddedPlacement, EmbeddedRenderElement};
use crate::render::surface_under;
use crate::shader_pass::ShaderPass;

use smithay::{
    backend::{
        allocator::gbm::{GbmAllocator, GbmDevice},
        drm::{
            compositor::{DrmCompositor, FrameFlags},
            exporter::gbm::GbmFramebufferExporter,
            DrmDevice, DrmDeviceFd, DrmNode,
        },
        renderer::{
            damage::OutputDamageTracker,
            element::{solid::SolidColorRenderElement, surface::WaylandSurfaceRenderElement, Kind},
            gles::GlesRenderer,
            utils::on_commit_buffer_handler,
            ImportDma,
        },
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
    utils::{Clock, Logical, Monotonic, Physical, Rectangle, SERIAL_COUNTER as SCOUNTER},
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

// ── type alias ────────────────────────────────────────────────────────────────

pub type GbmDrmCompositor =
    DrmCompositor<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>;

// ── render elements ───────────────────────────────────────────────────────────

smithay::backend::renderer::element::render_elements! {
    pub TrixieRenderElement<=GlesRenderer>;
    Space    = SpaceRenderElements<GlesRenderer, WaylandSurfaceRenderElement<GlesRenderer>>,
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
    pub embedded: EmbeddedManager,
    pub embed_ipc: EmbedIpcServer,

    pub session: LibSeatSession,
    pub backends: HashMap<DrmNode, BackendData>,
    pub primary_gpu: DrmNode,
    pub wayland_socket: String,
    pub exec_once_done: bool,
    pub shader_pass: ShaderPass,
    pub start_time: Instant,

    // ── embedded windows ──────────────────────────────────────────────────────
    pub embedded: EmbeddedManager,
    pub embed_ipc: EmbedIpcServer,
}

// ── render ────────────────────────────────────────────────────────────────────

impl KittyCompositor {
    pub fn process_embed_ipc(&mut self) {
        let cmds = self.embed_ipc.drain();
        for cmd in cmds {
            self.handle_embed_command(cmd);
        }
        // Keep the IPC window list in sync after every batch.
        let statuses = self.embedded.window_statuses();
        self.embed_ipc.update_windows(statuses);
    }

    fn handle_embed_command(&mut self, cmd: crate::embedded_ipc::EmbedCommand) {
        use crate::embedded_ipc::EmbedCommand;
        use crate::embedded_window::EmbeddedPlacement;

        match cmd {
            // ── Spawn ─────────────────────────────────────────────────────────
            EmbedCommand::Spawn {
                app_id,
                args,
                x,
                y,
                w,
                h,
            } => {
                let p = EmbeddedPlacement { x, y, w, h };
                self.embedded.request_placement(&app_id, p);

                let socket = self.wayland_socket.clone();
                tracing::info!("Spawning embedded app '{}' on {socket}", app_id);
                let mut child = std::process::Command::new(&app_id);
                child
                    .args(&args)
                    .env("WAYLAND_DISPLAY", &socket)
                    .env("MOZ_ENABLE_WAYLAND", "1")
                    .env("GDK_BACKEND", "wayland")
                    .env("QT_QPA_PLATFORM", "wayland");
                if let Err(e) = child.spawn() {
                    tracing::warn!("Failed to spawn '{}': {e}", app_id);
                }
            }

            // ── Move / resize ─────────────────────────────────────────────────
            EmbedCommand::Move { app_id, x, y, w, h } => {
                let p = EmbeddedPlacement { x, y, w, h };
                self.embedded.update_placement(&app_id, p);
            }

            // ── Focus ─────────────────────────────────────────────────────────
            EmbedCommand::Focus { app_id } => {
                let surface = self
                    .embedded
                    .entries
                    .get(&app_id)
                    .map(|e| e.surface.clone());
                if let Some(s) = surface {
                    let serial = smithay::utils::SERIAL_COUNTER.next_serial();
                    if let Some(kbd) = self.seat.get_keyboard() {
                        kbd.set_focus(self, Some(s), serial);
                    }
                } else {
                    tracing::warn!("Focus: embedded app '{}' not found", app_id);
                }
            }

            // ── Close ─────────────────────────────────────────────────────────
            EmbedCommand::Close { app_id } => {
                // Send xdg_toplevel.close — the destroy event will clean up
                // the EmbeddedEntry via toplevel_destroyed in handlers.rs.
                if let Some(entry) = self.embedded.entries.get(&app_id) {
                    entry.toplevel.send_close();
                } else {
                    // App may not have connected yet — just remove the reservation.
                    self.embedded.remove(&app_id);
                    tracing::debug!("Close: removed pending reservation for '{}'", app_id);
                }
            }

            // ── List ──────────────────────────────────────────────────────────
            // Handled inline by EmbedIpcServer::drain() — never reaches here.
            EmbedCommand::List => {}
        }
    }
}
