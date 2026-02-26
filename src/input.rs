// input.rs — keyboard, pointer, and axis event handling

use std::sync::atomic::Ordering;

use smithay::{
    backend::{
        input::{
            AbsolutePositionEvent, Axis, ButtonState, Event, InputEvent, KeyState,
            KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent, PointerMotionAbsoluteEvent,
            PointerMotionEvent,
        },
        libinput::LibinputInputBackend,
        session::Session,
    },
    input::{
        keyboard::FilterResult,
        pointer::{AxisFrame, ButtonEvent, MotionEvent},
    },
    reexports::wayland_server::protocol::wl_pointer,
    utils::SERIAL_COUNTER as SCOUNTER,
    wayland::seat::WaylandFocus,
};

use xkbcommon::xkb;

use crate::{
    config::{self, KeyAction},
    render::surface_under,
    state::{KittyCompositor, MouseMode},
};

// ── vt keysym helper ──────────────────────────────────────────────────────────

pub fn vt_from_keysym(keysym: xkb::Keysym) -> Option<u32> {
    const VT_FIRST: u32 = 0x1008FE01;
    const VT_LAST: u32 = 0x1008FE0C;
    let raw = keysym.raw();
    (raw >= VT_FIRST && raw <= VT_LAST).then(|| raw - VT_FIRST + 1)
}

// ── main input handler ────────────────────────────────────────────────────────

pub fn handle_input(state: &mut KittyCompositor, event: InputEvent<LibinputInputBackend>) {
    match event {
        InputEvent::Keyboard { event } => handle_keyboard(state, event),
        InputEvent::PointerMotionAbsolute { event } => handle_pointer_motion_abs(state, event),
        InputEvent::PointerMotion { event } => handle_pointer_motion(state, event),
        InputEvent::PointerButton { event } => handle_pointer_button(state, event),
        InputEvent::PointerAxis { event } => handle_pointer_axis(state, event),
        _ => {}
    }
}

// ── keyboard ──────────────────────────────────────────────────────────────────

fn handle_keyboard(
    state: &mut KittyCompositor,
    event: <LibinputInputBackend as smithay::backend::input::InputBackend>::KeyboardKeyEvent,
) {
    let serial = SCOUNTER.next_serial();
    let time = event.time_msec();
    let keycode = event.key_code();
    let key_state = event.state();

    // Auto-focus the first window if nothing is focused yet.
    {
        let kbd = state.seat.get_keyboard().unwrap();
        if kbd.current_focus().is_none() {
            let surface = state
                .space
                .elements()
                .next()
                .and_then(|w| w.wl_surface().map(|s| s.into_owned()));
            drop(kbd);
            if let Some(s) = surface {
                state
                    .seat
                    .get_keyboard()
                    .unwrap()
                    .set_focus(state, Some(s), serial);
            }
        }
    }

    let wayland_socket = state.wayland_socket.clone();

    state.seat.get_keyboard().unwrap().input(
        state,
        keycode,
        key_state,
        serial,
        time,
        |state, mods, keysym_handle| {
            if key_state != KeyState::Pressed {
                return FilterResult::Forward;
            }

            // ── VT switching (Ctrl+Alt+Fx / Ctrl+Alt+F1-F12) ─────────────────
            if mods.ctrl && mods.alt {
                let base_sym = keysym_handle
                    .raw_syms()
                    .first()
                    .copied()
                    .unwrap_or(xkb::Keysym::NoSymbol);

                let vt = vt_from_keysym(keysym_handle.modified_sym()).or_else(|| {
                    let raw = base_sym.raw();
                    (raw >= 0xFFBE && raw <= 0xFFC9).then(|| raw - 0xFFBE + 1)
                });

                if let Some(vt) = vt {
                    tracing::info!("Switching to VT {vt}");
                    if let Err(e) = state.session.change_vt(vt as i32) {
                        tracing::warn!("VT switch to {vt} failed: {e}");
                    }
                    return FilterResult::Intercept(());
                }
            }

            let pressed_sym = keysym_handle.modified_sym();
            let name = config::normalise_key_name(&xkb::keysym_get_name(pressed_sym));

            // ── mouse mode switching ──────────────────────────────────────────
            //
            // Super+i        → Insert (compositor owns pointer; glyph visible)
            // Super+Escape   → Normal (passthrough; glyph hidden)
            // Super+[        → Normal (alternative chord)
            //
            // Plain Escape is intentionally NOT intercepted here so that
            // applications (vim, fzf, …) always receive it.

            if mods.logo && !mods.shift && !mods.ctrl && !mods.alt && name == "i" {
                if state.mouse_mode != MouseMode::Insert {
                    tracing::info!("Mouse mode → Insert");
                    state.mouse_mode = MouseMode::Insert;
                    return FilterResult::Intercept(());
                }
                // Already in Insert — fall through so the key reaches the app.
            }

            if mods.logo
                && !mods.shift
                && !mods.ctrl
                && !mods.alt
                && (name == "escape" || name == "bracketleft")
            {
                if state.mouse_mode != MouseMode::Normal {
                    tracing::info!("Mouse mode → Normal");
                    state.mouse_mode = MouseMode::Normal;
                    return FilterResult::Intercept(());
                }
            }

            tracing::debug!(
                "key pressed: sym={} super:{} shift:{} ctrl:{} alt:{}",
                xkb::keysym_get_name(pressed_sym),
                mods.logo,
                mods.shift,
                mods.ctrl,
                mods.alt,
            );

            // ── user keybinds ─────────────────────────────────────────────────
            //
            // NOTE: Super+Return is deliberately NOT bound here.  trixterm
            // owns that key for TWM takeover.  The compositor forwards it.
            for i in 0..state.config.keybinds.len() {
                if !config::mods_match(mods, &state.config.keybinds[i].mods, &state.config.keyboard)
                {
                    continue;
                }
                if name != state.config.keybinds[i].key {
                    continue;
                }

                let action = state.config.keybinds[i].action.clone();
                match action {
                    KeyAction::Quit => {
                        state.running.store(false, Ordering::SeqCst);
                    }
                    KeyAction::CloseWindow => {
                        let focus = state.seat.get_keyboard().and_then(|k| k.current_focus());
                        let target = focus
                            .and_then(|fs| {
                                state
                                    .space
                                    .elements()
                                    .find(|w| w.wl_surface().as_deref() == Some(&fs))
                                    .cloned()
                            })
                            .or_else(|| state.space.elements().next().cloned());
                        if let Some(w) = target {
                            if let Some(t) = w.toplevel() {
                                t.send_close();
                            }
                        }
                    }
                    KeyAction::ReloadConfig => {
                        crate::main_loop::reload_config(state);
                    }
                    KeyAction::Spawn { command, args } => {
                        config::spawn_process(&command, &args, &wayland_socket);
                    }
                }
                return FilterResult::Intercept(());
            }

            FilterResult::Forward
        },
    );
}

// ── pointer motion (absolute) ─────────────────────────────────────────────────

fn handle_pointer_motion_abs(
    state: &mut KittyCompositor,
    event: <LibinputInputBackend as smithay::backend::input::InputBackend>::PointerMotionAbsoluteEvent,
) {
    let output_geo = state
        .space
        .outputs()
        .next()
        .and_then(|o| state.space.output_geometry(o))
        .unwrap_or_default();
    let pos = event.position_transformed(output_geo.size) + output_geo.loc.to_f64();
    let serial = SCOUNTER.next_serial();

    // Always update the internal pointer position so it is correct when
    // switching back to Insert, but only forward motion to clients in Normal.
    let under = match state.mouse_mode {
        MouseMode::Normal => surface_under(&state.space, pos),
        MouseMode::Insert => None,
    };

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

// ── pointer motion (relative) ─────────────────────────────────────────────────

fn handle_pointer_motion(
    state: &mut KittyCompositor,
    event: <LibinputInputBackend as smithay::backend::input::InputBackend>::PointerMotionEvent,
) {
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

    let under = match state.mouse_mode {
        MouseMode::Normal => surface_under(&state.space, pos),
        MouseMode::Insert => None,
    };

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

// ── pointer button ────────────────────────────────────────────────────────────

fn handle_pointer_button(
    state: &mut KittyCompositor,
    event: <LibinputInputBackend as smithay::backend::input::InputBackend>::PointerButtonEvent,
) {
    let serial = SCOUNTER.next_serial();
    let btn_state = wl_pointer::ButtonState::from(event.state());

    match state.mouse_mode {
        // Normal: pass click straight through to the focused surface.
        MouseMode::Normal => {
            let ptr = state.pointer.clone();
            ptr.button(
                state,
                &ButtonEvent {
                    button: event.button_code(),
                    state: btn_state.try_into().unwrap(),
                    serial,
                    time: event.time_msec(),
                },
            );
            ptr.frame(state);
        }

        // Insert: focus/raise the clicked window but do not forward the click.
        MouseMode::Insert => {
            if btn_state == wl_pointer::ButtonState::Pressed {
                let pos = state.pointer.current_location();
                let (window, surface) = {
                    let w = state
                        .space
                        .element_under(pos)
                        .map(|(w, _)| w.clone())
                        .or_else(|| state.space.elements().next().cloned());
                    let s = w
                        .as_ref()
                        .and_then(|w| w.wl_surface().map(|s| s.into_owned()));
                    (w, s)
                };
                if let Some(w) = &window {
                    state.space.raise_element(w, true);
                }
                if let Some(s) = surface {
                    state
                        .seat
                        .get_keyboard()
                        .unwrap()
                        .set_focus(state, Some(s), serial);
                }
            }
        }
    }
}

// ── pointer axis ──────────────────────────────────────────────────────────────

fn handle_pointer_axis(
    state: &mut KittyCompositor,
    event: <LibinputInputBackend as smithay::backend::input::InputBackend>::PointerAxisEvent,
) {
    // Scroll forwarded in Normal mode; suppressed in Insert (compositor owns
    // the mouse — there is no app scroll target).
    if state.mouse_mode == MouseMode::Insert {
        return;
    }

    let h = event
        .amount(Axis::Horizontal)
        .unwrap_or_else(|| event.amount_v120(Axis::Horizontal).unwrap_or(0.0) * 15.0 / 120.0);
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
