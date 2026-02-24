// render.rs — window rule application, surface helpers

use smithay::{
    desktop::{PopupKind, PopupManager, Space, Window, WindowSurfaceType},
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point, SERIAL_COUNTER as SCOUNTER},
    wayland::{compositor::with_states, seat::WaylandFocus, shell::xdg::XdgToplevelSurfaceData},
};

use crate::config::FloatingMarker;
use crate::state::{KittyCompositor, RulesApplied};

// ── window rules ──────────────────────────────────────────────────────────────

pub fn apply_window_rules(state: &mut KittyCompositor, window: &Window, app_id: &str, title: &str) {
    let rule = state
        .config
        .window_rules
        .iter()
        .find(|r| r.matches(app_id, title))
        .cloned();

    let Some(rule) = rule else { return };
    if !rule.floating {
        return;
    }

    let output_geo = state
        .space
        .outputs()
        .next()
        .and_then(|o| state.space.output_geometry(o))
        .unwrap_or_default();

    let sz: smithay::utils::Size<i32, Logical> = rule
        .size
        .map(|s| (s[0], s[1]).into())
        .unwrap_or_else(|| (640, 480).into());

    let pos: Point<i32, Logical> =
        rule.position
            .map(|p| (p[0], p[1]).into())
            .unwrap_or_else(|| {
                let cx = output_geo.loc.x + (output_geo.size.w - sz.w) / 2;
                let cy = output_geo.loc.y + (output_geo.size.h - sz.h) / 2;
                (cx, cy).into()
            });

    window.user_data().insert_if_missing(|| FloatingMarker {
        size: Some((sz.w, sz.h)),
        position: Some((pos.x, pos.y)),
    });

    if let Some(toplevel) = window.toplevel() {
        toplevel.with_pending_state(|s| s.size = Some(sz));
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }

    state.space.map_element(window.clone(), pos, true);
    state.space.raise_element(window, true);

    if let Some(surface) = window.wl_surface().map(|s| s.into_owned()) {
        let serial = SCOUNTER.next_serial();
        if let Some(kbd) = state.seat.get_keyboard() {
            kbd.set_focus(state, Some(surface), serial);
        }
    }

    tracing::info!(
        "Applied floating rule to app_id={:?} title={:?} size={:?} pos={:?}",
        app_id,
        title,
        sz,
        pos
    );
}

// ── surface helpers ───────────────────────────────────────────────────────────

pub fn surface_under(
    space: &Space<Window>,
    pos: Point<f64, Logical>,
) -> Option<(WlSurface, Point<f64, Logical>)> {
    space.element_under(pos).and_then(|(w, loc)| {
        w.surface_under(pos - loc.to_f64(), WindowSurfaceType::ALL)
            .map(|(s, sloc)| (s, (sloc + loc).to_f64()))
    })
}

pub fn ensure_initial_configure(
    surface: &WlSurface,
    space: &Space<Window>,
    popups: &mut PopupManager,
) {
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

// ── commit helper (called from handlers.rs) ───────────────────────────────────

pub fn try_apply_pending_rule(state: &mut KittyCompositor, surface: &WlSurface) {
    let pending: Option<(Window, String, String)> = {
        let window = state
            .space
            .elements()
            .find(|w| w.wl_surface().as_deref() == Some(surface))
            .cloned();

        window.and_then(|w| {
            if w.user_data().get::<RulesApplied>().is_some() {
                return None;
            }
            let (app_id, title) = with_states(surface, |states| {
                states
                    .data_map
                    .get::<XdgToplevelSurfaceData>()
                    .and_then(|d| d.lock().ok())
                    .map(|lock| {
                        (
                            lock.app_id.clone().unwrap_or_default(),
                            lock.title.clone().unwrap_or_default(),
                        )
                    })
            })
            .unwrap_or_default();

            if app_id.is_empty() {
                return None;
            }
            Some((w, app_id, title))
        })
    };

    if let Some((window, app_id, title)) = pending {
        apply_window_rules(state, &window, &app_id, &title);
        window.user_data().insert_if_missing(|| RulesApplied);
    }
}
