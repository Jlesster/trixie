// main.rs — entry point, event loop, exec, config reload

mod backend;
mod config;
mod embedded_ipc;
mod embedded_window;
mod handlers;
mod input;
mod render;
mod shader_config;
mod shader_ipc;
mod shader_pass;
mod state;

// expose reload_config to input.rs via crate::main_loop
pub mod main_loop {
    pub use super::reload_config;
}

use config::{Config, ExecEntry, VsyncMode};
use shader_pass::ShaderPass;
use state::{ClientState, KittyCompositor, MouseMode};

use notify::{EventKind, RecursiveMode, Watcher};
use std::{
    process::Command,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::Duration,
};

use smithay::{
    backend::{
        allocator::dmabuf::Dmabuf,
        drm::DrmNode,
        input::InputEvent,
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::ImportDma,
        session::{libseat::LibSeatSession, Event as SessionEvent, Session},
        udev::{all_gpus, primary_gpu, UdevBackend, UdevEvent},
    },
    input::{keyboard::XkbConfig, SeatState},
    reexports::{
        calloop::{generic::Generic, EventLoop, Interest, Mode as CalloopMode, PostAction},
        input::Libinput,
        wayland_server::Display as WlDisplay,
    },
    utils::Clock,
    wayland::{
        compositor::CompositorState,
        dmabuf::DmabufState,
        output::OutputManagerState,
        selection::{data_device::DataDeviceState, primary_selection::PrimarySelectionState},
        shell::{
            wlr_layer::WlrLayerShellState,
            xdg::{decoration::XdgDecorationState, XdgShellState},
        },
        shm::ShmState,
        socket::ListeningSocketSource,
    },
};

use smithay::backend::drm::NodeType;
use smithay::desktop::{PopupManager, Space};
use smithay::input::pointer::CursorImageStatus;

// ── config reload ─────────────────────────────────────────────────────────────

pub fn reload_config(state: &mut KittyCompositor) {
    tracing::info!("Reloading config…");
    let new = Config::load();

    state.config.terminal = new.terminal;
    state.config.background_color = new.background_color;
    state.config.target_hz = new.target_hz;
    state.config.vsync = new.vsync;
    state.config.keybinds = new.keybinds;
    state.config.window_rules = new.window_rules;
    state.config.exec = new.exec.clone();
    run_exec(state);

    // Reload shader registry and recompile any changed sources.
    state.config.shaders = new.shaders;
    let changed = state.config.shaders.hot_reload();
    state.shader_pass.sync_programs(&state.config.shaders);
    for name in changed {
        state
            .shader_pass
            .recompile_shader(&state.config.shaders, &name);
    }

    if new.keyboard.repeat_delay != state.config.keyboard.repeat_delay
        || new.keyboard.repeat_rate != state.config.keyboard.repeat_rate
    {
        if let Some(kbd) = state.seat.get_keyboard() {
            kbd.change_repeat_info(
                new.keyboard.repeat_rate as i32,
                new.keyboard.repeat_delay as i32,
            );
        }
    }
    state.config.keyboard = new.keyboard;

    if new.seat_name != state.config.seat_name {
        tracing::warn!(
            "seat_name changed ({:?} → {:?}) but cannot be applied without a restart — ignoring",
            state.config.seat_name,
            new.seat_name
        );
    }

    tracing::info!("Config reloaded OK");
}

// ── exec ──────────────────────────────────────────────────────────────────────

fn run_exec(state: &KittyCompositor) {
    for entry in &state.config.exec {
        spawn_exec_entry(entry, &state.wayland_socket);
    }
}

fn run_exec_once(state: &mut KittyCompositor) {
    if state.exec_once_done {
        return;
    }
    state.exec_once_done = true;
    let entries: Vec<_> = state.config.exec_once.clone();
    for entry in &entries {
        spawn_exec_entry(entry, &state.wayland_socket);
    }
}

fn spawn_exec_entry(entry: &ExecEntry, wayland_socket: &str) {
    let bin = config::expand_tilde(&entry.command);
    tracing::info!("exec: {bin} {:?}", entry.args);
    if let Err(e) = Command::new(&bin)
        .args(&entry.args)
        .env("WAYLAND_DISPLAY", wayland_socket)
        .spawn()
    {
        tracing::warn!("exec failed ({bin}): {e}");
    }
}

// ── main ──────────────────────────────────────────────────────────────────────

fn main() {
    // ── Nvidia environment (must precede EGL/GBM init) ────────────────────────
    // SAFETY: called before any threads are spawned.
    unsafe {
        std::env::set_var("GBM_BACKEND", "nvidia-drm");
        std::env::set_var("__GLX_VENDOR_LIBRARY_NAME", "nvidia");
        std::env::set_var("LIBVA_DRIVER_NAME", "nvidia");
        std::env::set_var("__GL_SYNC_TO_VBLANK", "0");
    }

    tracing_subscriber::fmt().compact().init();

    let config = Config::load();

    // ── vsync env overrides ───────────────────────────────────────────────────
    // SAFETY: called before any threads are spawned.
    unsafe {
        match config.vsync {
            VsyncMode::On => {
                std::env::set_var("__GL_SYNC_TO_VBLANK", "1");
                std::env::set_var("vblank_mode", "2");
            }
            VsyncMode::Off => {
                std::env::set_var("__GL_SYNC_TO_VBLANK", "0");
                std::env::set_var("vblank_mode", "0");
            }
            VsyncMode::Adaptive => {
                std::env::set_var("__GL_SYNC_TO_VBLANK", "0");
                std::env::set_var("vblank_mode", "1");
            }
        }
    }

    let mut event_loop: EventLoop<'static, KittyCompositor> = EventLoop::try_new().unwrap();
    let display: WlDisplay<KittyCompositor> = WlDisplay::new().unwrap();
    let dh = display.handle();

    event_loop
        .handle()
        .insert_source(
            Generic::new(display, Interest::READ, CalloopMode::Level),
            |_, display, state| {
                unsafe { display.get_mut().dispatch_clients(state).unwrap() };
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
                state.libinput.suspend();
                for b in state.backends.values_mut() {
                    b.drm.pause();
                }
            }
            SessionEvent::ActivateSession => {
                tracing::info!("Session resumed");
                if let Err(e) = state
                    .libinput
                    .udev_assign_seat(state.session.seat().as_str())
                {
                    tracing::error!("Failed to reassign libinput seat on resume: {e:?}");
                }
                for b in state.backends.values_mut() {
                    if let Err(e) = b.drm.activate(false) {
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

    // ── libinput ──────────────────────────────────────────────────────────────
    let mut libinput_ctx =
        Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(session.clone().into());
    libinput_ctx
        .udev_assign_seat(session.seat().as_str())
        .unwrap();

    // ── seat + keyboard ───────────────────────────────────────────────────────
    let mut seat_state = SeatState::new();
    let mut seat = seat_state.new_wl_seat(&dh, &config.seat_name);
    let pointer = seat.add_pointer();
    seat.add_keyboard(
        XkbConfig {
            layout: config.keyboard.layout.as_deref().unwrap_or(""),
            variant: config.keyboard.variant.as_deref().unwrap_or(""),
            options: config.keyboard.options.clone(),
            ..XkbConfig::default()
        },
        config.keyboard.repeat_delay as i32,
        config.keyboard.repeat_rate as i32,
    )
    .unwrap();

    let start_time = std::time::Instant::now();

    let mut state = KittyCompositor {
        display_handle: dh.clone(),
        running: Arc::new(AtomicBool::new(true)),
        handle: event_loop.handle(),
        clock: Clock::new(),
        config,
        compositor_state: CompositorState::new::<KittyCompositor>(&dh),
        shm_state: ShmState::new::<KittyCompositor>(&dh, vec![]),
        dmabuf_state: DmabufState::new(),
        dmabuf_global: None,
        output_manager_state: OutputManagerState::new_with_xdg_output::<KittyCompositor>(&dh),
        seat_state,
        data_device_state: DataDeviceState::new::<KittyCompositor>(&dh),
        primary_selection_state: PrimarySelectionState::new::<KittyCompositor>(&dh),
        xdg_shell_state: XdgShellState::new::<KittyCompositor>(&dh),
        layer_shell_state: WlrLayerShellState::new::<KittyCompositor>(&dh),
        xdg_decoration_state: XdgDecorationState::new::<KittyCompositor>(&dh),
        popups: PopupManager::default(),
        space: Space::default(),
        seat,
        pointer,
        cursor_status: CursorImageStatus::default_named(),
        mouse_mode: MouseMode::Normal,
        session,
        backends: Default::default(),
        primary_gpu,
        wayland_socket: socket_name.clone(),
        libinput: libinput_ctx,
        exec_once_done: false,
        shader_pass: ShaderPass::new(start_time),
        start_time,
    };

    // ── udev ──────────────────────────────────────────────────────────────────
    let udev_backend = UdevBackend::new(state.session.seat()).unwrap();
    for (dev_id, path) in udev_backend.device_list() {
        let Ok(node) = DrmNode::from_dev_id(dev_id) else {
            continue;
        };
        if let Err(e) = backend::add_gpu(&mut state, &dh, node, path) {
            tracing::warn!("Skipping GPU {node}: {e}");
        }
    }

    if let Some(b) = state.backends.get(&state.primary_gpu) {
        let formats: Vec<_> = b.renderer.dmabuf_formats().iter().copied().collect();
        let global = state
            .dmabuf_state
            .create_global::<KittyCompositor>(&dh, formats);
        state.dmabuf_global = Some(global);
    }

    // Compile shaders now that the GL context exists.
    state.shader_pass.sync_programs(&state.config.shaders);

    // IPC socket for the ratatui shader manager.
    {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;
        let ipc_path = shader_ipc::socket_path();
        let _ = std::fs::remove_file(&ipc_path);
        match UnixListener::bind(&ipc_path) {
            Ok(listener) => {
                listener.set_nonblocking(true).ok();
                tracing::info!("Shader IPC socket: {}", ipc_path.display());
                event_loop
                    .handle()
                    .insert_source(
                        Generic::new(listener, Interest::READ, CalloopMode::Level),
                        |_, listener, state| {
                            loop {
                                match listener.accept() {
                                    Ok((mut stream, _)) => {
                                        stream.set_nonblocking(false).ok();
                                        let mut line = String::new();
                                        if BufReader::new(stream.try_clone().unwrap())
                                            .read_line(&mut line)
                                            .is_err()
                                        {
                                            continue;
                                        }
                                        let trimmed = line.trim();
                                        if trimmed.is_empty() {
                                            continue;
                                        }
                                        if let Ok(cmd) =
                                            serde_json::from_str::<shader_ipc::IpcCommand>(trimmed)
                                        {
                                            let mut recompile = Vec::new();
                                            let resp = shader_ipc::dispatch_command_with_registry(
                                                cmd,
                                                &mut state.config.shaders,
                                                &mut recompile,
                                            );
                                            for name in recompile {
                                                state
                                                    .shader_pass
                                                    .recompile_shader(&state.config.shaders, &name);
                                            }
                                            if let Ok(mut json) = serde_json::to_string(&resp) {
                                                json.push('\n');
                                                stream.write_all(json.as_bytes()).ok();
                                            }
                                        }
                                    }
                                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                                    Err(_) => break,
                                }
                            }
                            Ok(PostAction::Continue)
                        },
                    )
                    .ok();
            }
            Err(e) => tracing::warn!("Could not bind shader IPC socket: {e}"),
        }
    }

    event_loop
        .handle()
        .insert_source(udev_backend, |event, _, state| match event {
            UdevEvent::Added { device_id, path } => {
                if let Ok(node) = DrmNode::from_dev_id(device_id) {
                    let _ = backend::add_gpu(state, &state.display_handle.clone(), node, &path);
                }
            }
            UdevEvent::Changed { .. } => {}
            UdevEvent::Removed { device_id } => {
                if let Ok(node) = DrmNode::from_dev_id(device_id) {
                    state.backends.remove(&node);
                }
            }
        })
        .unwrap();

    event_loop
        .handle()
        .insert_source(
            LibinputInputBackend::new(state.libinput.clone()),
            |event, _, state| input::handle_input(state, event),
        )
        .unwrap();

    // ── launch terminal ───────────────────────────────────────────────────────
    let running = state.running.clone();
    let (bin, args) = state.config.terminal_cmd();
    println!("Launching {bin} on WAYLAND_DISPLAY={socket_name}");
    Command::new(&bin)
        .args(&args)
        .env("WAYLAND_DISPLAY", &socket_name)
        .spawn()
        .unwrap_or_else(|e| panic!("Failed to spawn {bin}: {e}"));

    run_exec_once(&mut state);
    run_exec(&state);

    // ── config file watcher ───────────────────────────────────────────────────
    let config_dir = Config::config_dir();
    let (reload_tx, reload_rx) = mpsc::channel::<()>();

    let mut watcher = {
        let tx = reload_tx.clone();
        notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(ev) = res else { return };
            let is_write = matches!(
                ev.kind,
                EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
            );
            let affects_config = ev.paths.iter().any(|p| {
                matches!(
                    p.extension().and_then(|e| e.to_str()),
                    Some("json") | Some("glsl")
                )
            });
            if is_write && affects_config {
                let _ = tx.send(());
            }
        })
    }
    .expect("Failed to create config file watcher");

    if config_dir.exists() {
        // Recursive so changes to shaders/*.glsl are also caught.
        if let Err(e) = watcher.watch(&config_dir, RecursiveMode::Recursive) {
            tracing::warn!("Could not watch config dir {}: {e}", config_dir.display());
        } else {
            tracing::info!("Watching config dir for changes: {}", config_dir.display());
        }
    }

    // ── event loop ────────────────────────────────────────────────────────────
    let mut dh = state.display_handle.clone();
    while running.load(Ordering::SeqCst) {
        let _ = event_loop.dispatch(Some(Duration::from_millis(1)), &mut state);

        if reload_rx.try_recv().is_ok() {
            while reload_rx.try_recv().is_ok() {}
            reload_config(&mut state);
        }

        state.space.refresh();
        state.popups.cleanup();
        if let Err(e) = dh.flush_clients() {
            tracing::warn!("flush_clients: {e}");
        }
    }
}
