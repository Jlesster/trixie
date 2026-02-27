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
use crate::twm_drop_in::TwmState;

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

use crate::pixelui::overlay_element::TwmChromeElement;
use ratatui::layout::Margin;

// ── type alias ────────────────────────────────────────────────────────────────

pub type GbmDrmCompositor =
    DrmCompositor<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>;

// ── render elements ───────────────────────────────────────────────────────────

smithay::backend::renderer::element::render_elements! {
    pub TrixieRenderElement<=GlesRenderer>;
    Space    = WaylandSurfaceRenderElement<GlesRenderer>,
    Cursor   = SolidColorRenderElement,
    Embedded = EmbeddedRenderElement,
    Chrome   = TwmChromeElement,
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
    pub twm: Option<TwmState>,
    pub xdg_decoration_state: XdgDecorationState,
    pub popups: PopupManager,

    pub space: Space<Window>,
    pub seat: Seat<Self>,
    pub pointer: PointerHandle<Self>,
    pub cursor_status: CursorImageStatus,
    pub mouse_mode: MouseMode,

    pub embedded: EmbeddedManager,
    pub embed_ipc: EmbedIpcServer,
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

        // Use physical pixel dimensions — must match what set_viewport() received
        // (drm_mode.size() in backend.rs). output.current_mode().size is in logical
        // pixels and differs from physical when output scale != 1, which would desync
        // DrawCmd pixel coordinates from the u_vp NDC projection in the shader.
        let (output_w, output_h) = crate::pixelui::overlay_element::get_viewport();

        let scale = smithay::utils::Scale::from(output.current_scale().fractional_scale());
        let clear: [f32; 4] = {
            let c = self.config.background_color;
            [c[0], c[1], c[2], 1.0]
        };

        // ── 1. TWM chrome ─────────────────────────────────────────────────────
        let chrome_el: Option<TwmChromeElement> = if let Some(twm) = &mut self.twm {
            let (cw, ch) = crate::pixelui::overlay_element::cell_size();
            if crate::pixelui::overlay_element::is_installed() {
                let cols = (output_w / cw).max(1) as u16;
                let rows = (output_h / ch).max(1) as u16;
                tracing::info!(
                    "twm resize: output={}x{} cell={}x{} cols={} rows={} bar_bottom_px={}",
                    output_w,
                    output_h,
                    cw,
                    ch,
                    cols,
                    rows,
                    (rows as u32) * ch
                );
                if twm.cols != cols || twm.rows != rows {
                    twm.resize(cols, rows);
                }
                let cmds = twm.build_frame_cmds(cw, ch, output_w, output_h);
                tracing::info!(
                    "chrome: cell={}x{} output={}x{} cols={} rows={} cmds={}",
                    cw,
                    ch,
                    output_w,
                    output_h,
                    cols,
                    rows,
                    cmds.len()
                );
                Some(TwmChromeElement::new(cmds, output_w, output_h))
            } else {
                tracing::warn!("chrome: cell_size is (8,16) default — renderer not installed");
                None
            }
        } else {
            tracing::warn!("chrome: self.twm is None");
            None
        };

        // ── 2. Sync embedded placements from TWM layout ───────────────────────
        let embedded_rects: Vec<(String, i32, i32, i32, i32)> = if let Some(twm) = &self.twm {
            let (cw, ch) = crate::pixelui::overlay_element::cell_size();
            twm.all_embedded_cell_rects()
                .into_iter()
                .map(|(app_id, r)| {
                    let inner = r.inner(&Margin {
                        horizontal: 1,
                        vertical: 1,
                    });
                    let px = inner.x as i32 * cw as i32;
                    let py = inner.y as i32 * ch as i32;
                    let pw = inner.width as i32 * cw as i32;
                    let ph = inner.height as i32 * ch as i32;
                    (app_id, px, py, pw, ph)
                })
                .collect()
        } else {
            vec![]
        };
        for (app_id, px, py, pw, ph) in embedded_rects {
            self.embedded.update_placement(
                &app_id,
                EmbeddedPlacement {
                    x: px,
                    y: py,
                    w: pw,
                    h: ph,
                },
            );
        }

        // Re-borrow backend mutably after the self.twm / self.embedded work.
        let backend = match self.backends.get_mut(&node) {
            Some(b) => b,
            None => return,
        };
        let surface = match backend.surfaces.get_mut(&crtc) {
            Some(s) => s,
            None => return,
        };

        // ── 3. Embedded quads ─────────────────────────────────────────────────
        let embedded_elements: Vec<TrixieRenderElement> = self
            .embedded
            .render_elements()
            .into_iter()
            .map(TrixieRenderElement::Embedded)
            .collect();

        // ── 4. Space elements ─────────────────────────────────────────────────
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

        // ── 5. Assemble — chrome first (bottom), windows on top ───────────────
        let mut all: Vec<TrixieRenderElement> = Vec::new();
        if let Some(el) = chrome_el {
            all.push(TrixieRenderElement::Chrome(el));
        }
        all.extend(embedded_elements);
        all.extend(space_elements);

        tracing::info!("render_surface: {} total elements", all.len(),);

        // ── 6. Render frame ───────────────────────────────────────────────────
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
                        Ok(()) => surface.pending_frame = true,
                        Err(e) => tracing::warn!("queue_frame({node},{crtc:?}): {e}"),
                    }
                } else {
                    tracing::info!("render_surface: frame was empty (no damage)");
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

    // ── TWM helpers ───────────────────────────────────────────────────────────

    pub fn sync_twm_focus_to_wayland(&mut self) {
        use crate::twm_drop_in::PaneContent;
        let focused_app_id = self
            .twm
            .as_ref()
            .and_then(|t| t.focused_content())
            .and_then(|c| match c {
                PaneContent::Embedded { app_id } => Some(app_id.clone()),
                _ => None,
            });

        if let Some(app_id) = focused_app_id {
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

                if let Some(twm) = &mut self.twm {
                    twm.assign_embedded(&app_id);
                }

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
                if let Some(twm) = &mut self.twm {
                    twm.close_pane_by_app_id(&app_id);
                }
            }

            EmbedCommand::List => {}
        }
    }
}
