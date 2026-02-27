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
    twm_drop_in::Action as TwmAction,
};

// ── vt keysym helper ──────────────────────────────────────────────────────────

pub fn vt_from_keysym(keysym: xkb::Keysym) -> Option<u32> {
    const VT_FIRST: u32 = 0x1008FE01;
    const VT_LAST: u32 = 0x1008FE0C;
    let raw = keysym.raw();
    (raw >= VT_FIRST && raw <= VT_LAST).then(|| raw - VT_FIRST + 1)
}

// ── TWM action dispatch ───────────────────────────────────────────────────────

/// Central point for all TWM keybind effects.
/// Dispatches to TwmState, then syncs focus + placement + triggers a redraw.
fn run_twm_action(state: &mut KittyCompositor, action: TwmAction) {
    if let Some(twm) = &mut state.twm {
        twm.dispatch(&action);
    }
    state.sync_twm_focus_to_wayland();
    state.render_all();
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

            // ── VT switching ──────────────────────────────────────────────────
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
            if mods.logo && !mods.shift && !mods.ctrl && !mods.alt && name == "i" {
                if state.mouse_mode != MouseMode::Insert {
                    tracing::info!("Mouse mode → Insert");
                    state.mouse_mode = MouseMode::Insert;
                    return FilterResult::Intercept(());
                }
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

            // ── Hard-coded TWM keybinds (Super + hjkl / arrows / etc.) ────────
            //
            // These are intercepted before the config keybind table so they
            // always work even if the user hasn't added them to trixie.conf.
            // You can comment out any you want to expose via config instead.
            if mods.logo && !mods.ctrl && !mods.alt {
                // Focus movement — Super+hjkl and Super+arrows
                let twm_action: Option<TwmAction> = if !mods.shift {
                    match name.as_str() {
                        "h" | "left" => Some(TwmAction::FocusLeft),
                        "l" | "right" => Some(TwmAction::FocusRight),
                        "k" | "up" => Some(TwmAction::FocusUp),
                        "j" | "down" => Some(TwmAction::FocusDown),
                        // Pane close
                        "q" => Some(TwmAction::Close),
                        // Cycle layout
                        "space" => Some(TwmAction::NextLayout),
                        // Bar toggle
                        "b" => Some(TwmAction::ToggleBar),
                        // Fullscreen
                        "f" => Some(TwmAction::Fullscreen),
                        // Main ratio
                        "equal" => Some(TwmAction::GrowMain),
                        "minus" => Some(TwmAction::ShrinkMain),
                        // Workspace by number (Super+1..9)
                        "1" => Some(TwmAction::Workspace(1)),
                        "2" => Some(TwmAction::Workspace(2)),
                        "3" => Some(TwmAction::Workspace(3)),
                        "4" => Some(TwmAction::Workspace(4)),
                        "5" => Some(TwmAction::Workspace(5)),
                        "6" => Some(TwmAction::Workspace(6)),
                        "7" => Some(TwmAction::Workspace(7)),
                        "8" => Some(TwmAction::Workspace(8)),
                        "9" => Some(TwmAction::Workspace(9)),
                        _ => None,
                    }
                } else {
                    // Super+Shift combos
                    match name.as_str() {
                        // Move pane within workspace
                        "h" | "left" => Some(TwmAction::MoveLeft),
                        "l" | "right" => Some(TwmAction::MoveRight),
                        // Move to workspace (Super+Shift+1..9)
                        "1" => Some(TwmAction::MoveToWorkspace(1)),
                        "2" => Some(TwmAction::MoveToWorkspace(2)),
                        "3" => Some(TwmAction::MoveToWorkspace(3)),
                        "4" => Some(TwmAction::MoveToWorkspace(4)),
                        "5" => Some(TwmAction::MoveToWorkspace(5)),
                        "6" => Some(TwmAction::MoveToWorkspace(6)),
                        "7" => Some(TwmAction::MoveToWorkspace(7)),
                        "8" => Some(TwmAction::MoveToWorkspace(8)),
                        "9" => Some(TwmAction::MoveToWorkspace(9)),
                        _ => None,
                    }
                };

                if let Some(action) = twm_action {
                    run_twm_action(state, action);
                    return FilterResult::Intercept(());
                }
            }

            // ── Config-file keybinds ──────────────────────────────────────────
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
                        // Close via TWM first; fall back to killing the focused
                        // space window if TWM has no pane for it.
                        let had_twm_pane =
                            state.twm.as_ref().and_then(|t| t.focused_id()).is_some();
                        if had_twm_pane {
                            run_twm_action(state, TwmAction::Close);
                        } else {
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
