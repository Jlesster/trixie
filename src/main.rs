use std::{
    collections::HashMap,
    os::unix::io::{FromRawFd, IntoRawFd},
    process::Command,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use smithay::{backend::drm::compositor::FrameFlags, delegate_dmabuf};
use smithay::{backend::renderer::ImportDma, utils::Size};
use smithay::{
    backend::{
        allocator::{
            dmabuf::Dmabuf,
            gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
            Fourcc,
        },
        drm::{
            compositor::DrmCompositor, exporter::gbm::GbmFramebufferExporter, DrmDevice,
            DrmDeviceFd, DrmEvent, DrmNode, NodeType,
        },
        egl::{EGLContext, EGLDisplay},
        input::InputEvent,
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{
            damage::OutputDamageTracker, element::surface::WaylandSurfaceRenderElement,
            gles::GlesRenderer, utils::on_commit_buffer_handler, Bind, ImportMemWl,
        },
        session::{libseat::LibSeatSession, Event as SessionEvent, Session},
        udev::{all_gpus, primary_gpu, UdevBackend, UdevEvent},
    },
    delegate_compositor, delegate_data_device, delegate_layer_shell, delegate_output,
    delegate_primary_selection, delegate_seat, delegate_shm, delegate_xdg_decoration,
    delegate_xdg_shell,
    desktop::{
        layer_map_for_output,
        space::{space_render_elements, SpaceRenderElements},
        LayerSurface, PopupKind, PopupManager, Space, Window, WindowSurfaceType,
    },
    input::{
        keyboard::{FilterResult, XkbConfig},
        pointer::{AxisFrame, ButtonEvent, CursorImageStatus, MotionEvent, PointerHandle},
        Seat, SeatHandler, SeatState,
    },
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::{
        calloop::{
            generic::Generic,
            timer::{TimeoutAction, Timer},
            EventLoop, Interest, LoopHandle, Mode as CalloopMode, PostAction,
        },
        drm::control::{connector, crtc, Device as DrmControlDevice, ModeTypeFlags},
        input::Libinput,
        wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1,
        wayland_server::{
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::{wl_buffer::WlBuffer, wl_output, wl_seat, wl_surface::WlSurface},
            Client, Display, DisplayHandle, Resource,
        },
    },
    utils::{
        Clock, DeviceFd, IsAlive, Logical, Monotonic, Point, Rectangle, Scale, Serial, Transform,
        SERIAL_COUNTER as SCOUNTER,
    },
    wayland::{
        buffer::BufferHandler,
        compositor::{
            get_parent, is_sync_subsurface, with_states, CompositorClientState, CompositorHandler,
            CompositorState,
        },
        dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier},
        output::OutputHandler,
        seat::WaylandFocus,
        selection::{
            data_device::{
                set_data_device_focus, ClientDndGrabHandler, DataDeviceHandler, DataDeviceState,
                ServerDndGrabHandler,
            },
            primary_selection::{
                set_primary_focus, PrimarySelectionHandler, PrimarySelectionState,
            },
            SelectionHandler,
        },
        shell::{
            wlr_layer::{Layer, WlrLayerShellHandler, WlrLayerShellState},
            xdg::{
                decoration::XdgDecorationHandler, Configure, PopupSurface, PositionerState,
                ToplevelSurface, XdgShellHandler, XdgShellState, XdgToplevelSurfaceData,
            },
        },
        shm::{ShmHandler, ShmState},
        socket::ListeningSocketSource,
    },
};

// ── type alias ────────────────────────────────────────────────────────────────

/// Convenience alias matching the pattern used in smithay's own anvil example.
pub type GbmDrmCompositor =
    DrmCompositor<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>;

// ── per-output data ───────────────────────────────────────────────────────────

struct SurfaceData {
    output: Output,
    compositor: GbmDrmCompositor,
    damage_tracker: OutputDamageTracker,
}

// ── per-GPU data ──────────────────────────────────────────────────────────────

struct BackendData {
    surfaces: HashMap<crtc::Handle, SurfaceData>,
    renderer: GlesRenderer,
    gbm: GbmDevice<DrmDeviceFd>,
    drm: DrmDevice,
    drm_node: DrmNode,
}

// ── client state ──────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
struct ClientState {
    compositor: CompositorClientState,
}
impl ClientData for ClientState {
    fn initialized(&self, _: ClientId) {}
    fn disconnected(&self, _: ClientId, _: DisconnectReason) {}
}

// ── compositor state ──────────────────────────────────────────────────────────

struct KittyCompositor {
    display_handle: DisplayHandle,
    running: Arc<AtomicBool>,
    handle: LoopHandle<'static, Self>,
    clock: Clock<Monotonic>,

    compositor_state: CompositorState,
    shm_state: ShmState,
    dmabuf_state: DmabufState,
    dmabuf_global: Option<DmabufGlobal>,
    output_manager_state: smithay::wayland::output::OutputManagerState,
    seat_state: SeatState<Self>,
    data_device_state: DataDeviceState,
    primary_selection_state: PrimarySelectionState,
    xdg_shell_state: XdgShellState,
    layer_shell_state: WlrLayerShellState,
    xdg_decoration_state: smithay::wayland::shell::xdg::decoration::XdgDecorationState,
    popups: PopupManager,

    space: Space<Window>,
    seat: Seat<Self>,
    pointer: PointerHandle<Self>,
    cursor_status: CursorImageStatus,

    session: LibSeatSession,
    backends: HashMap<DrmNode, BackendData>,
    primary_gpu: DrmNode,
}

// ── DmabufHandler ─────────────────────────────────────────────────────────────

impl DmabufHandler for KittyCompositor {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }
    fn dmabuf_imported(&mut self, _: &DmabufGlobal, dmabuf: Dmabuf, notifier: ImportNotifier) {
        let ok = self
            .backends
            .get_mut(&self.primary_gpu)
            .map(|b| b.renderer.import_dmabuf(&dmabuf, None).is_ok())
            .unwrap_or(false);
        if ok {
            let _ = notifier.successful::<KittyCompositor>();
        } else {
            notifier.failed();
        }
    }
}
delegate_dmabuf!(KittyCompositor);

// ── protocol delegates ────────────────────────────────────────────────────────

impl BufferHandler for KittyCompositor {
    fn buffer_destroyed(&mut self, _: &WlBuffer) {}
}
impl ShmHandler for KittyCompositor {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}
delegate_shm!(KittyCompositor);

impl CompositorHandler for KittyCompositor {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }
    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor
    }
    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);
        if !is_sync_subsurface(surface) {
            let mut root = surface.clone();
            while let Some(p) = get_parent(&root) {
                root = p;
            }
            if let Some(w) = self
                .space
                .elements()
                .find(|w| w.wl_surface().as_deref() == Some(&root))
                .cloned()
            {
                w.on_commit();
            }
        }
        self.popups.commit(surface);
        ensure_initial_configure(surface, &self.space, &mut self.popups);
    }
}
delegate_compositor!(KittyCompositor);

impl SelectionHandler for KittyCompositor {
    type SelectionUserData = ();
}
impl ClientDndGrabHandler for KittyCompositor {}
impl ServerDndGrabHandler for KittyCompositor {}
impl DataDeviceHandler for KittyCompositor {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}
delegate_data_device!(KittyCompositor);

impl PrimarySelectionHandler for KittyCompositor {
    fn primary_selection_state(&self) -> &PrimarySelectionState {
        &self.primary_selection_state
    }
}
delegate_primary_selection!(KittyCompositor);

impl OutputHandler for KittyCompositor {}
delegate_output!(KittyCompositor);

impl SeatHandler for KittyCompositor {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;
    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }
    fn focus_changed(&mut self, seat: &Seat<Self>, target: Option<&WlSurface>) {
        let dh = &self.display_handle;
        let focus = target.and_then(|s| dh.get_client(s.id()).ok());
        set_data_device_focus(dh, seat, focus.clone());
        set_primary_focus(dh, seat, focus);
    }
    fn cursor_image(&mut self, _: &Seat<Self>, image: CursorImageStatus) {
        self.cursor_status = image;
    }
}
delegate_seat!(KittyCompositor);

impl WlrLayerShellHandler for KittyCompositor {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }
    fn new_layer_surface(
        &mut self,
        surface: smithay::wayland::shell::wlr_layer::LayerSurface,
        _output: Option<wl_output::WlOutput>,
        _layer: Layer,
        _namespace: String,
    ) {
        if let Some(output) = self.space.outputs().next().cloned() {
            let mut map = layer_map_for_output(&output);
            let _ = map.map_layer(&LayerSurface::new(surface, String::new()));
        }
    }
    fn layer_destroyed(&mut self, _: smithay::wayland::shell::wlr_layer::LayerSurface) {}
}
delegate_layer_shell!(KittyCompositor);

impl XdgShellHandler for KittyCompositor {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }
    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        let size = self
            .space
            .outputs()
            .next()
            .and_then(|o| self.space.output_geometry(o))
            .map(|g| g.size);
        if let Some(size) = size {
            surface.with_pending_state(|s| {
                s.size = Some(size);
            });
        }
        let window = Window::new_wayland_window(surface);
        self.space.map_element(window, (0, 0), true);
    }
    fn toplevel_destroyed(&mut self, _: ToplevelSurface) {
        if self.space.elements().count() == 0 {
            self.running.store(false, Ordering::SeqCst);
        }
    }
    fn new_popup(&mut self, surface: PopupSurface, _: PositionerState) {
        let _ = self.popups.track_popup(PopupKind::Xdg(surface));
    }
    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        surface.with_pending_state(|s| {
            s.geometry = positioner.get_geometry();
            s.positioner = positioner;
        });
        surface.send_repositioned(token);
    }
    fn grab(&mut self, _: PopupSurface, _: wl_seat::WlSeat, _: Serial) {}
}
delegate_xdg_shell!(KittyCompositor);

impl XdgDecorationHandler for KittyCompositor {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|s| {
            s.decoration_mode = Some(zxdg_toplevel_decoration_v1::Mode::ServerSide);
        });
    }
    fn request_mode(&mut self, toplevel: ToplevelSurface, _: zxdg_toplevel_decoration_v1::Mode) {
        toplevel.with_pending_state(|s| {
            s.decoration_mode = Some(zxdg_toplevel_decoration_v1::Mode::ServerSide);
        });
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }
    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|s| {
            s.decoration_mode = Some(zxdg_toplevel_decoration_v1::Mode::ServerSide);
        });
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }
}
delegate_xdg_decoration!(KittyCompositor);

// ── helpers ───────────────────────────────────────────────────────────────────

fn ensure_initial_configure(surface: &WlSurface, space: &Space<Window>, popups: &mut PopupManager) {
    if let Some(window) = space
        .elements()
        .find(|w| w.wl_surface().as_deref() == Some(surface))
        .cloned()
    {
        if let Some(toplevel) = window.toplevel() {
            let sent = with_states(surface, |s| {
                s.data_map
                    .get::<XdgToplevelSurfaceData>()
                    .map(|d| d.lock().unwrap().initial_configure_sent)
                    .unwrap_or(false)
            });
            if !sent {
                toplevel.send_configure();
            }
        }
        return;
    }
    if let Some(PopupKind::Xdg(ref p)) = popups.find_popup(surface) {
        if !p.is_initial_configure_sent() {
            let _ = p.send_configure();
        }
    }
}

fn surface_under(
    space: &Space<Window>,
    pos: Point<f64, Logical>,
) -> Option<(WlSurface, Point<f64, Logical>)> {
    space.element_under(pos).and_then(|(w, loc)| {
        w.surface_under(pos - loc.to_f64(), WindowSurfaceType::ALL)
            .map(|(s, sloc)| (s, (sloc + loc).to_f64()))
    })
}

// ── main ──────────────────────────────────────────────────────────────────────

fn main() {
    tracing_subscriber::fmt().compact().init();

    let mut event_loop: EventLoop<'static, KittyCompositor> = EventLoop::try_new().unwrap();
    let display: Display<KittyCompositor> = Display::new().unwrap();
    let dh = display.handle();

    event_loop
        .handle()
        .insert_source(
            Generic::new(display, Interest::READ, CalloopMode::Level),
            |_, display, state| {
                unsafe {
                    display.get_mut().dispatch_clients(state).unwrap();
                }
                Ok(PostAction::Continue)
            },
        )
        .unwrap();

    let source = ListeningSocketSource::new_auto().unwrap();
    let socket_name = source.socket_name().to_string_lossy().into_owned();
    event_loop
        .handle()
        .insert_source(source, |stream, _, state| {
            state
                .display_handle
                .insert_client(stream, Arc::new(ClientState::default()))
                .unwrap();
        })
        .unwrap();

    // ── libseat session ───────────────────────────────────────────────────────
    let (session, notifier) = LibSeatSession::new().expect("Failed to create libseat session");

    event_loop
        .handle()
        .insert_source(notifier, |event, _, state| match event {
            SessionEvent::PauseSession => {
                tracing::info!("Session paused");
                for backend in state.backends.values_mut() {
                    backend.drm.pause();
                }
            }
            SessionEvent::ActivateSession => {
                tracing::info!("Session resumed");
                for backend in state.backends.values_mut() {
                    if let Err(e) = backend.drm.activate(false) {
                        tracing::error!("Failed to activate DRM: {e}");
                    }
                }
                state.handle.insert_idle(|state| state.render_all());
            }
        })
        .unwrap();

    // ── primary GPU ───────────────────────────────────────────────────────────
    let primary_gpu = primary_gpu(session.seat())
        .unwrap()
        .and_then(|p| DrmNode::from_path(p).ok())
        .and_then(|n| n.node_with_type(NodeType::Render).and_then(|n| n.ok()))
        .unwrap_or_else(|| {
            all_gpus(session.seat())
                .unwrap()
                .into_iter()
                .find_map(|p| DrmNode::from_path(p).ok())
                .expect("No GPU found")
        });
    tracing::info!("Primary GPU: {primary_gpu}");

    // ── initial state ─────────────────────────────────────────────────────────
    let mut seat_state = SeatState::new();
    let mut seat = seat_state.new_wl_seat(&dh, "seat0");
    let pointer = seat.add_pointer();
    seat.add_keyboard(XkbConfig::default(), 200, 25).unwrap();

    let dmabuf_state = DmabufState::new();

    let mut state = KittyCompositor {
        display_handle: dh.clone(),
        running: Arc::new(AtomicBool::new(true)),
        handle: event_loop.handle(),
        clock: Clock::new(),
        compositor_state: CompositorState::new::<KittyCompositor>(&dh),
        shm_state: ShmState::new::<KittyCompositor>(&dh, vec![]),
        dmabuf_state,
        dmabuf_global: None,
        output_manager_state: smithay::wayland::output::OutputManagerState::new_with_xdg_output::<
            KittyCompositor,
        >(&dh),
        seat_state,
        data_device_state: DataDeviceState::new::<KittyCompositor>(&dh),
        primary_selection_state: PrimarySelectionState::new::<KittyCompositor>(&dh),
        xdg_shell_state: XdgShellState::new::<KittyCompositor>(&dh),
        layer_shell_state: WlrLayerShellState::new::<KittyCompositor>(&dh),
        xdg_decoration_state: smithay::wayland::shell::xdg::decoration::XdgDecorationState::new::<
            KittyCompositor,
        >(&dh),
        popups: PopupManager::default(),
        space: Space::default(),
        seat,
        pointer,
        cursor_status: CursorImageStatus::default_named(),
        session,
        backends: HashMap::new(),
        primary_gpu,
    };

    // ── udev ──────────────────────────────────────────────────────────────────
    let udev_backend = UdevBackend::new(state.session.seat()).unwrap();
    for (dev_id, path) in udev_backend.device_list() {
        let node = match DrmNode::from_dev_id(dev_id) {
            Ok(n) => n,
            Err(_) => continue,
        };
        if let Err(e) = add_gpu(&mut state, &dh, node, path) {
            tracing::warn!("Skipping GPU {node}: {e}");
        }
    }

    // dmabuf global from primary GPU formats
    if let Some(backend) = state.backends.get(&state.primary_gpu) {
        // FormatSet is not an iterator directly — use .iter()
        let formats: Vec<_> = backend.renderer.dmabuf_formats().iter().copied().collect();
        let global = state
            .dmabuf_state
            .create_global::<KittyCompositor>(&dh, formats);
        state.dmabuf_global = Some(global);
    }

    event_loop
        .handle()
        .insert_source(udev_backend, |event, _, state| match event {
            UdevEvent::Added { device_id, path } => {
                if let Ok(node) = DrmNode::from_dev_id(device_id) {
                    let _ = add_gpu(state, &state.display_handle.clone(), node, &path);
                }
            }
            UdevEvent::Changed { device_id } => {
                if let Ok(_node) = DrmNode::from_dev_id(device_id) {
                    // DrmDevice has no explicit changed() method in smithay 0.7;
                    // hotplug changes are handled automatically via DRM events.
                }
            }
            UdevEvent::Removed { device_id } => {
                if let Ok(node) = DrmNode::from_dev_id(device_id) {
                    state.backends.remove(&node);
                }
            }
        })
        .unwrap();

    // ── libinput ──────────────────────────────────────────────────────────────
    let mut libinput_ctx = Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(
        state.session.clone().into(),
    );
    libinput_ctx
        .udev_assign_seat(state.session.seat().as_str())
        .unwrap();
    let libinput_backend = LibinputInputBackend::new(libinput_ctx.clone());

    event_loop
        .handle()
        .insert_source(libinput_backend, |event, _, state| {
            handle_input(state, event);
        })
        .unwrap();

    state.handle.insert_idle(|state| state.render_all());

    let running = state.running.clone();
    println!("Launching kitty on WAYLAND_DISPLAY={socket_name}");
    Command::new("kitty")
        .env("WAYLAND_DISPLAY", &socket_name)
        .spawn()
        .expect("Failed to spawn kitty — is it installed?");

    while running.load(Ordering::SeqCst) {
        let _ = event_loop.dispatch(Some(Duration::from_millis(4)), &mut state);
        state.space.refresh();
        state.popups.cleanup();
        let mut dh = state.display_handle.clone();
        dh.flush_clients().unwrap();
    }
}

// ── GPU initialisation ────────────────────────────────────────────────────────

fn add_gpu(
    state: &mut KittyCompositor,
    dh: &DisplayHandle,
    node: DrmNode,
    path: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let owned_fd = state.session.open(
        path,
        rustix::fs::OFlags::RDWR | rustix::fs::OFlags::CLOEXEC | rustix::fs::OFlags::NOCTTY,
    )?;
    // Use raw fd to sidestep OwnedFd crate-version mismatch between rustix and smithay
    let raw_fd = owned_fd.into_raw_fd();
    let drm_fd = DrmDeviceFd::new(unsafe { DeviceFd::from_raw_fd(raw_fd) });

    let (drm, drm_notifier) = DrmDevice::new(drm_fd.clone(), true)?;
    let gbm = GbmDevice::new(drm_fd.clone())?;
    let egl = unsafe { EGLDisplay::new(gbm.clone())? };
    let ctx = EGLContext::new(&egl)?;
    let mut renderer = unsafe { GlesRenderer::new(ctx)? };

    // bind_wl_display is a method on EGLDisplay via the EGLGraphicsBackend trait
    if let Err(e) = egl.bind_wl_display(dh) {
        tracing::warn!("EGL bind_wl_display failed (hw-accel unavailable): {e}");
    }

    let drm_node = node;

    let node_copy = drm_node;
    state
        .handle
        .insert_source(drm_notifier, move |event, _, state| {
            if let DrmEvent::VBlank(crtc) = event {
                state.frame_finish(node_copy, crtc);
            }
        })
        .unwrap();

    let mut backend = BackendData {
        surfaces: HashMap::new(),
        renderer,
        gbm,
        drm,
        drm_node,
    };

    // resource_handles / get_connector require DrmControlDevice in scope
    let res_handles = backend.drm.resource_handles()?;
    let connectors: Vec<_> = res_handles
        .connectors()
        .iter()
        .filter_map(|&h| backend.drm.get_connector(h, false).ok())
        .filter(|c| c.state() == connector::State::Connected)
        .collect();

    for connector in connectors {
        let mode = connector
            .modes()
            .iter()
            .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
            .or_else(|| connector.modes().first())
            .copied();
        let Some(mode) = mode else { continue };

        let crtc = res_handles
            .crtcs()
            .iter()
            .copied()
            .find(|&crtc| !backend.surfaces.contains_key(&crtc));
        let Some(crtc) = crtc else { continue };

        if let Err(e) = add_output(
            state,
            dh,
            &mut backend,
            node,
            connector.handle(),
            crtc,
            mode,
        ) {
            tracing::warn!("Failed to set up connector: {e}");
        }
    }

    state.backends.insert(drm_node, backend);
    Ok(())
}

fn add_output(
    state: &mut KittyCompositor,
    dh: &DisplayHandle,
    backend: &mut BackendData,
    node: DrmNode,
    connector: connector::Handle,
    crtc: crtc::Handle,
    drm_mode: smithay::reexports::drm::control::Mode,
) -> Result<(), Box<dyn std::error::Error>> {
    let wl_mode = Mode {
        size: (drm_mode.size().0 as i32, drm_mode.size().1 as i32).into(),
        refresh: drm_mode.vrefresh() as i32 * 1000,
    };

    let output = Output::new(
        format!("{node}-{crtc:?}"),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "KittyWM".into(),
            model: "DRM".into(),
        },
    );
    let _global = output.create_global::<KittyCompositor>(dh);
    output.change_current_state(
        Some(wl_mode),
        Some(Transform::Normal),
        None,
        Some((0, 0).into()),
    );
    output.set_preferred(wl_mode);
    state.space.map_output(&output, (0, 0));

    let allocator = GbmAllocator::new(
        backend.gbm.clone(),
        GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
    );

    // GbmFramebufferExporter satisfies ExportFramebuffer<GbmBuffer> —
    // this is the fix for the main trait-bound error.
    let exporter = GbmFramebufferExporter::new(backend.gbm.clone(), Some(node));

    let color_formats = [Fourcc::Argb8888, Fourcc::Xrgb8888];

    let compositor = DrmCompositor::new(
        &output,
        backend.drm.create_surface(crtc, drm_mode, &[connector])?,
        None,
        allocator,
        exporter,
        color_formats.iter().copied(),
        backend
            .renderer
            .egl_context()
            .dmabuf_render_formats()
            .clone(),
        Size::<u32, smithay::utils::Buffer>::from((64, 64)), // ← missing arg
        None::<GbmDevice<DrmDeviceFd>>,
    )?;

    let damage_tracker = OutputDamageTracker::from_output(&output);
    backend.surfaces.insert(
        crtc,
        SurfaceData {
            output,
            compositor,
            damage_tracker,
        },
    );
    Ok(())
}

// ── render ────────────────────────────────────────────────────────────────────

impl KittyCompositor {
    fn render_all(&mut self) {
        let nodes: Vec<_> = self.backends.keys().copied().collect();
        for node in nodes {
            let crtcs: Vec<_> = self.backends[&node].surfaces.keys().copied().collect();
            for crtc in crtcs {
                self.render_surface(node, crtc);
            }
        }
    }

    fn render_surface(&mut self, node: DrmNode, crtc: crtc::Handle) {
        let Some(backend) = self.backends.get_mut(&node) else {
            return;
        };
        let Some(surface_data) = backend.surfaces.get_mut(&crtc) else {
            return;
        };

        let output = surface_data.output.clone();
        let output_size = output
            .current_mode()
            .map(|m| m.size.to_logical(1))
            .unwrap_or_default();

        // keep kitty filling the output
        let windows: Vec<_> = self.space.elements().cloned().collect();
        for window in &windows {
            if let Some(toplevel) = window.toplevel() {
                let needs = toplevel.with_pending_state(|s| {
                    if s.size != Some(output_size) {
                        s.size = Some(output_size);
                        true
                    } else {
                        false
                    }
                });
                if needs {
                    toplevel.send_pending_configure();
                }
                self.space.map_element(window.clone(), (0, 0), false);
            }
        }

        let backend = self.backends.get_mut(&node).unwrap();
        let surface_data = backend.surfaces.get_mut(&crtc).unwrap();

        let elements: Vec<
            SpaceRenderElements<GlesRenderer, WaylandSurfaceRenderElement<GlesRenderer>>,
        > = match space_render_elements(&mut backend.renderer, [&self.space], &output, 1.0) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("render elements: {e}");
                return;
            }
        };

        let render_res = surface_data.compositor.render_frame(
            &mut backend.renderer,
            &elements,
            [0.05f32, 0.05, 0.05, 1.0],
            FrameFlags::empty(), // ← missing arg
        );

        match render_res {
            Ok(frame) => {
                if !frame.is_empty {
                    if let Err(e) = surface_data.compositor.queue_frame(()) {
                        tracing::warn!("queue_frame: {e}");
                    }
                }
                let now = self.clock.now();
                for window in self.space.elements().cloned().collect::<Vec<_>>() {
                    window.send_frame(&output, now, Some(Duration::from_secs(1)), |_, _| {
                        Some(output.clone())
                    });
                }
            }
            Err(e) => tracing::warn!("render_frame: {e}"),
        }

        let handle = self.handle.clone();
        handle
            .insert_source(
                Timer::from_duration(Duration::from_millis(16)),
                move |_, _, state| {
                    state.render_surface(node, crtc);
                    TimeoutAction::Drop
                },
            )
            .ok();
    }

    fn frame_finish(&mut self, node: DrmNode, crtc: crtc::Handle) {
        if let Some(backend) = self.backends.get_mut(&node) {
            if let Some(surface) = backend.surfaces.get_mut(&crtc) {
                if let Err(e) = surface.compositor.frame_submitted() {
                    tracing::warn!("frame_submitted: {e}");
                }
            }
        }
        // Schedule next render after vblank, not on a blind timer
        self.handle.clone().insert_idle(move |state| {
            state.render_surface(node, crtc);
        });
    }
}

// ── input ─────────────────────────────────────────────────────────────────────

fn handle_input(state: &mut KittyCompositor, event: InputEvent<LibinputInputBackend>) {
    use smithay::backend::input::{
        AbsolutePositionEvent, Axis, Event, KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent,
        PointerMotionAbsoluteEvent,
    };
    use smithay::input::keyboard::FilterResult;
    use smithay::reexports::wayland_server::protocol::wl_pointer;

    match event {
        InputEvent::Keyboard { event } => {
            let serial = SCOUNTER.next_serial();
            let time = event.time_msec();
            let keycode = event.key_code();
            let key_state = event.state();
            let surface = state
                .space
                .elements()
                .next()
                .and_then(|w| w.wl_surface().map(|s| s.into_owned()));
            let kbd = state.seat.get_keyboard().unwrap();
            if kbd.current_focus().is_none() {
                if let Some(s) = surface {
                    kbd.set_focus(state, Some(s), serial);
                }
            }
            let kbd = state.seat.get_keyboard().unwrap();
            kbd.input::<(), _>(state, keycode, key_state, serial, time, |_, _, _| {
                FilterResult::Forward
            });
        }
        InputEvent::PointerMotionAbsolute { event } => {
            let output_geo = state
                .space
                .outputs()
                .next()
                .and_then(|o| state.space.output_geometry(o))
                .unwrap_or_default();
            let pos = event.position_transformed(output_geo.size) + output_geo.loc.to_f64();
            let serial = SCOUNTER.next_serial();
            let under = surface_under(&state.space, pos);
            let ptr = state.pointer.clone();
            ptr.motion(
                state,
                under,
                &MotionEvent {
                    location: pos,
                    serial,
                    time: event.time_msec(),
                },
            );
            ptr.frame(state);
        }
        InputEvent::PointerMotion { event } => {
            use smithay::backend::input::PointerMotionEvent;
            let mut pos = state.pointer.current_location() + event.delta();
            if let Some(geo) = state
                .space
                .outputs()
                .next()
                .and_then(|o| state.space.output_geometry(o))
            {
                pos.x = pos
                    .x
                    .clamp(geo.loc.x as f64, (geo.loc.x + geo.size.w) as f64);
                pos.y = pos
                    .y
                    .clamp(geo.loc.y as f64, (geo.loc.y + geo.size.h) as f64);
            }
            let serial = SCOUNTER.next_serial();
            let under = surface_under(&state.space, pos);
            let ptr = state.pointer.clone();
            ptr.motion(
                state,
                under,
                &MotionEvent {
                    location: pos,
                    serial,
                    time: event.time_msec(),
                },
            );
            ptr.frame(state);
        }
        InputEvent::PointerButton { event } => {
            let serial = SCOUNTER.next_serial();
            let button = event.button_code();
            let btn_state = wl_pointer::ButtonState::from(event.state());
            if btn_state == wl_pointer::ButtonState::Pressed {
                let (window, surface) = {
                    let w = state.space.elements().next().cloned();
                    let s = w
                        .as_ref()
                        .and_then(|w| w.wl_surface().map(|s| s.into_owned()));
                    (w, s)
                };
                if let Some(w) = window {
                    state.space.raise_element(&w, true);
                }
                if let Some(s) = surface {
                    let kbd = state.seat.get_keyboard().unwrap();
                    kbd.set_focus(state, Some(s), serial);
                }
            }
            let ptr = state.pointer.clone();
            ptr.button(
                state,
                &ButtonEvent {
                    button,
                    state: btn_state.try_into().unwrap(),
                    serial,
                    time: event.time_msec(),
                },
            );
            ptr.frame(state);
        }
        InputEvent::PointerAxis { event } => {
            let h = event.amount(Axis::Horizontal).unwrap_or_else(|| {
                event.amount_v120(Axis::Horizontal).unwrap_or(0.0) * 15.0 / 120.0
            });
            let v = event
                .amount(Axis::Vertical)
                .unwrap_or_else(|| event.amount_v120(Axis::Vertical).unwrap_or(0.0) * 15.0 / 120.0);
            let mut frame = AxisFrame::new(event.time_msec()).source(event.source());
            if h != 0.0 {
                frame = frame.value(Axis::Horizontal, h);
            }
            if v != 0.0 {
                frame = frame.value(Axis::Vertical, v);
            }
            let ptr = state.pointer.clone();
            ptr.axis(state, frame);
            ptr.frame(state);
        }
        _ => {}
    }
}
