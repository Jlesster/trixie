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
                XdgShellHandler, XdgShellState, XdgToplevelSurfaceData,
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

        // ── Retry deferred app_id claim ───────────────────────────────────────
        // Many apps (including Firefox) only set their app_id after the first
        // commit. new_toplevel parks them in unclaimed_toplevels and we retry
        // the claim here once the id is readable from XdgToplevelSurfaceData.
        let obj_id = surface.id();
        if self.unclaimed_toplevels.contains_key(&obj_id) {
            let app_id = with_states(surface, |states| {
                states
                    .data_map
                    .get::<XdgToplevelSurfaceData>()
                    .and_then(|d| d.lock().ok())
                    .and_then(|l| l.app_id.clone())
            })
            .unwrap_or_default();

            tracing::debug!("commit: unclaimed surface app_id={:?}", app_id);

            if !app_id.is_empty() {
                if self.embedded.has_pending(&app_id) {
                    // Unmap the temporary Space entry created in new_toplevel.
                    // We need to find the window first, then drop the borrow on
                    // self.space before we can call try_claim.
                    let window_to_unmap = self
                        .space
                        .elements()
                        .find(|w| w.wl_surface().as_deref() == Some(surface))
                        .cloned();

                    if let Some(window) = window_to_unmap {
                        self.space.unmap_elem(&window);
                    }

                    let toplevel = self.unclaimed_toplevels.remove(&obj_id).unwrap();
                    let wl = toplevel.wl_surface().clone();
                    if self.embedded.try_claim(&app_id, wl, toplevel) {
                        tracing::info!("commit: deferred claim succeeded for '{}'", app_id);
                        let statuses = self.embedded.window_statuses();
                        self.embed_ipc.update_windows(statuses);
                        // Fall through to the embedded commit path below.
                    }
                } else {
                    // Not an embedded app — promote to normal, stop retrying.
                    self.unclaimed_toplevels.remove(&obj_id);
                }
            }
        }

        // ── Embedded commit path ──────────────────────────────────────────────
        if self.embedded.is_embedded_surface(surface) {
            if let Some(b) = self.backends.get_mut(&self.primary_gpu) {
                self.embedded.on_commit(&mut b.renderer, surface);
            }
            return;
        }

        // ── Normal path ───────────────────────────────────────────────────────
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
        let app_id = with_states(surface.wl_surface(), |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .and_then(|d| d.lock().ok())
                .and_then(|l| l.app_id.clone())
        })
        .unwrap_or_default();

        tracing::info!("new_toplevel: app_id={:?}", app_id);

        let wl = surface.wl_surface().clone();

        // Fast path: app_id known and we have a pending reservation.
        if !app_id.is_empty() && self.embedded.has_pending(&app_id) {
            if self.embedded.try_claim(&app_id, wl, surface.clone()) {
                tracing::info!("new_toplevel: immediately claimed '{}'", app_id);
                let statuses = self.embedded.window_statuses();
                self.embed_ipc.update_windows(statuses);
                return;
            }
        }

        // Slow path: park for retry on first commit when app_id is available.
        let obj_id = surface.wl_surface().id();
        self.unclaimed_toplevels.insert(obj_id, surface.clone());

        let window = Window::new_wayland_window(surface);

        // FIX 3: send a configure with the real output size before mapping,
        // so the client (trixterm, etc.) knows its initial dimensions.
        // Without this the compositor sends Configure{0,0} and many terminals
        // render nothing or pick an arbitrary fallback size.
        let output_size = self
            .space
            .outputs()
            .next()
            .and_then(|o| self.space.output_geometry(o))
            .map(|g| g.size)
            .unwrap_or_else(|| smithay::utils::Size::from((1920, 1080)));

        if let Some(toplevel) = window.toplevel() {
            toplevel.with_pending_state(|s| {
                s.size = Some(output_size);
            });
            toplevel.send_configure();
        }

        self.space.map_element(window.clone(), (0, 0), false);

        // FIX 5: give the new window keyboard focus immediately on map.
        // Without this, trixterm receives no input until the user clicks.
        // The input.rs fix moved focus recovery outside the closure, but focus
        // is still never *set* proactively when a window first appears.
        let wl_surface = window.wl_surface().map(|s| s.into_owned());

        if let Some(surface) = wl_surface {
            let serial = SCOUNTER.next_serial();
            if let Some(kbd) = self.seat.get_keyboard() {
                kbd.set_focus(self, Some(surface), serial);
            }
        }
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        // Always clean up the staging map.
        let obj_id = surface.wl_surface().id();
        self.unclaimed_toplevels.remove(&obj_id);

        let wl = surface.wl_surface();
        if self.embedded.is_embedded_surface(wl) {
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
