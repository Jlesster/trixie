// handlers.rs — Smithay protocol delegate implementations

use std::{process::Command, sync::atomic::Ordering};

use smithay::{
    delegate_compositor, delegate_data_device, delegate_dmabuf, delegate_layer_shell,
    delegate_output, delegate_primary_selection, delegate_seat, delegate_shm,
    delegate_xdg_decoration, delegate_xdg_shell,
    desktop::{layer_map_for_output, LayerSurface, PopupKind, Window},
    input::{Seat, SeatHandler, SeatState},
    reexports::{
        wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1,
        wayland_server::{
            protocol::{wl_buffer::WlBuffer, wl_output, wl_seat, wl_surface::WlSurface},
            Client, Resource,
        },
    },
    utils::SERIAL_COUNTER as SCOUNTER,
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
                decoration::XdgDecorationHandler, PopupSurface, PositionerState, ToplevelSurface,
                XdgShellHandler, XdgShellState,
            },
        },
        shm::{ShmHandler, ShmState},
    },
};

use smithay::backend::renderer::{utils::on_commit_buffer_handler, ImportDma};
use smithay::input::pointer::CursorImageStatus;

use crate::{
    render::{ensure_initial_configure, try_apply_pending_rule},
    state::{ClientState, KittyCompositor},
};

// ── dmabuf ────────────────────────────────────────────────────────────────────

impl DmabufHandler for KittyCompositor {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }
    fn dmabuf_imported(
        &mut self,
        _: &DmabufGlobal,
        dmabuf: smithay::backend::allocator::dmabuf::Dmabuf,
        notifier: ImportNotifier,
    ) {
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

// ── shm / buffer ──────────────────────────────────────────────────────────────

impl BufferHandler for KittyCompositor {
    fn buffer_destroyed(&mut self, _: &WlBuffer) {}
}

impl ShmHandler for KittyCompositor {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}
delegate_shm!(KittyCompositor);

// ── compositor ────────────────────────────────────────────────────────────────

impl CompositorHandler for KittyCompositor {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }
    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor
    }
    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);

        // If this surface belongs to an embedded entry, import its buffer now.
        // We need the renderer from the primary GPU's backend.
        if self.embedded.is_embedded_surface(surface) {
            if let Some(b) = self.backends.get_mut(&self.primary_gpu) {
                self.embedded.on_commit(&mut b.renderer, surface);
            }
            // Embedded surfaces don't participate in Space configure logic.
            return;
        }

        // Normal path.
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
        try_apply_pending_rule(self, surface);
    }
}
delegate_compositor!(KittyCompositor);

// ── selection / data device ───────────────────────────────────────────────────

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

// ── output ────────────────────────────────────────────────────────────────────

impl OutputHandler for KittyCompositor {}
delegate_output!(KittyCompositor);

// ── seat ──────────────────────────────────────────────────────────────────────

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

// ── layer shell ───────────────────────────────────────────────────────────────

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

// ── xdg shell ─────────────────────────────────────────────────────────────────

impl XdgShellHandler for KittyCompositor {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        // Read the app_id from pending state before deciding what to do.
        let app_id = surface.current_state().app_id.clone().unwrap_or_default();

        // If EmbeddedManager has a pending reservation for this app_id,
        // claim the surface — it won't be mapped into Space at all.
        let wl = surface.wl_surface().clone();
        if self.embedded.try_claim(&app_id, wl, surface.clone()) {
            // Surface is now owned by EmbeddedManager.
            // Update the IPC window list so List commands return current state.
            let statuses = self.embedded.window_statuses();
            self.embed_ipc.update_windows(statuses);
            return;
        }

        // Normal path: map as a tiled window.
        let window = Window::new_wayland_window(surface);
        self.space.map_element(window, (0, 0), false);
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        // Check if this was an embedded surface being destroyed.
        let wl = surface.wl_surface();
        if self.embedded.is_embedded_surface(wl) {
            // Find its app_id and remove it.
            let app_id = self
                .embedded
                .entries
                .iter()
                .find(|(_, e)| &e.surface == wl)
                .map(|(id, _)| id.clone());
            if let Some(id) = app_id {
                tracing::info!("Embedded surface '{}' destroyed", id);
                self.embedded.remove(&id);
                let statuses = self.embedded.window_statuses();
                self.embed_ipc.update_windows(statuses);
            }
            return;
        }

        // Normal tiled window destroyed.
        if self.space.elements().count() == 0 {
            let (bin, args) = self.config.terminal_cmd();
            tracing::info!("Terminal closed — relaunching {bin}");
            match Command::new(&bin)
                .args(&args)
                .env("WAYLAND_DISPLAY", &self.wayland_socket)
                .spawn()
            {
                Ok(_) => {}
                Err(e) => {
                    tracing::error!("Failed to relaunch {bin}: {e} — quitting");
                    self.running.store(false, Ordering::SeqCst);
                }
            }
        } else {
            let next = self
                .space
                .elements()
                .next()
                .and_then(|w| w.wl_surface().map(|s| s.into_owned()));
            if let Some(s) = next {
                let serial = SCOUNTER.next_serial();
                if let Some(kbd) = self.seat.get_keyboard() {
                    kbd.set_focus(self, Some(s), serial);
                }
            }
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

    fn grab(&mut self, _: PopupSurface, _: wl_seat::WlSeat, _: smithay::utils::Serial) {}
}
delegate_xdg_shell!(KittyCompositor);

// ── xdg decoration ────────────────────────────────────────────────────────────

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
