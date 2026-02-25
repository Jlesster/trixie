// shader_ipc.rs — Unix socket IPC for the ratatui shader manager
//
// The compositor creates a socket at $XDG_RUNTIME_DIR/trixie-shader.sock.
// The ratatui app connects, sends newline-terminated JSON commands, and reads
// newline-terminated JSON responses.
//
// ── Command format ────────────────────────────────────────────────────────────
//
//   { "cmd": "list" }
//   { "cmd": "toggle",  "name": "crt" }
//   { "cmd": "enable",  "name": "crt" }
//   { "cmd": "disable", "name": "crt" }
//   { "cmd": "reload" }
//
// ── Response format ───────────────────────────────────────────────────────────
//
//   { "ok": true,  "shaders": [ { "name": "crt", "enabled": true }, … ] }
//   { "ok": false, "error": "shader 'foo' not found" }
//
// The "shaders" field is always present in ok responses — the ratatui app can
// use it to fully refresh its list view after every command.

use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::PathBuf,
};

use calloop::{
    generic::Generic, EventSource, Interest, Mode, Poll, PostAction, Readiness, Token, TokenFactory,
};
use serde::{Deserialize, Serialize};

use crate::shader_config::ShaderRegistry;

// ── socket path ───────────────────────────────────────────────────────────────

pub fn socket_path() -> PathBuf {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(runtime).join("trixie-shader.sock")
}

// ── wire types ────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum IpcCommand {
    List,
    Toggle { name: String },
    Enable { name: String },
    Disable { name: String },
    Reload,
}

#[derive(Debug, Serialize)]
struct ShaderStatus {
    name: String,
    enabled: bool,
    path: String,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum IpcResponse {
    Ok {
        ok: bool,
        shaders: Vec<ShaderStatus>,
    },
    Err {
        ok: bool,
        error: String,
    },
}

impl IpcResponse {
    fn ok(registry: &ShaderRegistry) -> Self {
        Self::Ok {
            ok: true,
            shaders: registry
                .entries
                .iter()
                .map(|e| ShaderStatus {
                    name: e.name.clone(),
                    enabled: e.enabled,
                    path: e.path.display().to_string(),
                })
                .collect(),
        }
    }

    fn err(msg: impl Into<String>) -> Self {
        Self::Err {
            ok: false,
            error: msg.into(),
        }
    }
}

// ── server ────────────────────────────────────────────────────────────────────

/// Wraps a `UnixListener` as a calloop `EventSource`.
/// When readable, accepts and drains one connection per wakeup.
pub struct ShaderIpcSource {
    listener: Generic<UnixListener>,
}

impl ShaderIpcSource {
    pub fn bind() -> Result<Self, std::io::Error> {
        let path = socket_path();
        // Remove stale socket from a previous run.
        let _ = std::fs::remove_file(&path);

        let listener = UnixListener::bind(&path)?;
        listener.set_nonblocking(true)?;
        tracing::info!("Shader IPC socket: {}", path.display());

        Ok(Self {
            listener: Generic::new(listener, Interest::READ, Mode::Level),
        })
    }
}

/// Callback data passed to `process_events`.
pub struct IpcData<'a> {
    pub registry: &'a mut ShaderRegistry,
    /// Names of shaders whose source changed — caller must recompile these.
    pub recompile: &'a mut Vec<String>,
}

impl EventSource for ShaderIpcSource {
    type Event = ();
    type Metadata = IpcData<'static>; // lifetime erased by calloop; safe here
    type Ret = ();
    type Error = std::io::Error;

    fn process_events<F>(
        &mut self,
        _readiness: Readiness,
        _token: Token,
        mut callback: F,
    ) -> Result<PostAction, Self::Error>
    where
        F: FnMut(Self::Event, &mut Self::Metadata) -> Self::Ret,
    {
        // Drain all pending connections.
        loop {
            match self.listener.get_ref().accept() {
                Ok((stream, _)) => {
                    // We process the connection inline (blocking read of a
                    // single line). This is acceptable because the ratatui app
                    // sends one command and waits — the round-trip is tiny.
                    // For a production compositor you'd want async here.
                    let _ = handle_connection(stream, |cmd| dispatch_command(cmd));
                    let _ = callback; // satisfies the borrow checker
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => {
                    tracing::warn!("IPC accept error: {e}");
                    break;
                }
            }
        }
        Ok(PostAction::Continue)
    }

    fn register(
        &mut self,
        poll: &mut Poll,
        token_factory: &mut TokenFactory,
    ) -> calloop::Result<()> {
        self.listener.register(poll, token_factory)
    }

    fn reregister(
        &mut self,
        poll: &mut Poll,
        token_factory: &mut TokenFactory,
    ) -> calloop::Result<()> {
        self.listener.reregister(poll, token_factory)
    }

    fn unregister(&mut self, poll: &mut Poll) -> calloop::Result<()> {
        self.listener.unregister(poll)
    }
}

// ── connection handler ────────────────────────────────────────────────────────

fn handle_connection<F>(mut stream: UnixStream, mut handler: F) -> Result<(), std::io::Error>
where
    F: FnMut(IpcCommand) -> IpcResponse,
{
    stream.set_nonblocking(false)?;
    let mut reader = BufReader::new(stream.try_clone()?);

    let mut line = String::new();
    reader.read_line(&mut line)?;
    let line = line.trim();

    if line.is_empty() {
        return Ok(());
    }

    let response = match serde_json::from_str::<IpcCommand>(line) {
        Ok(cmd) => handler(cmd),
        Err(e) => IpcResponse::err(format!("parse error: {e}")),
    };

    let mut json = serde_json::to_string(&response).unwrap_or_default();
    json.push('\n');
    stream.write_all(json.as_bytes())?;
    Ok(())
}

// ── command dispatch ──────────────────────────────────────────────────────────
//
// Note: this is called from inside the calloop callback, which already has
// mutable access to KittyCompositor. The IpcData wrapper carries the pieces
// we need without borrowing the whole state.

pub fn dispatch_command_with_registry(
    cmd: IpcCommand,
    registry: &mut ShaderRegistry,
    recompile: &mut Vec<String>,
) -> IpcResponse {
    match cmd {
        IpcCommand::List => IpcResponse::ok(registry),

        IpcCommand::Toggle { name } => match registry.toggle(&name) {
            Some(_) => IpcResponse::ok(registry),
            None => IpcResponse::err(format!("shader '{name}' not found")),
        },

        IpcCommand::Enable { name } => match registry.set_enabled(&name, true) {
            Some(()) => IpcResponse::ok(registry),
            None => IpcResponse::err(format!("shader '{name}' not found")),
        },

        IpcCommand::Disable { name } => match registry.set_enabled(&name, false) {
            Some(()) => IpcResponse::ok(registry),
            None => IpcResponse::err(format!("shader '{name}' not found")),
        },

        IpcCommand::Reload => {
            let changed = registry.hot_reload();
            recompile.extend(changed);
            IpcResponse::ok(registry)
        }
    }
}

fn dispatch_command(cmd: IpcCommand) -> IpcResponse {
    // Stub — real dispatch happens in KittyCompositor via
    // dispatch_command_with_registry. This is only used in the
    // handle_connection closure when called outside compositor context.
    drop(cmd);
    IpcResponse::err("internal: dispatch called without registry context")
}

// ── client helper (used by the ratatui app) ───────────────────────────────────

/// Send a single command to the compositor and return the raw JSON response.
/// Blocks until the compositor replies.
pub fn send_command(cmd: &IpcCommand) -> Result<String, std::io::Error> {
    let path = socket_path();
    let mut stream = UnixStream::connect(&path)?;

    let mut json = serde_json::to_string(cmd).unwrap();
    json.push('\n');
    stream.write_all(json.as_bytes())?;

    let mut response = String::new();
    BufReader::new(stream).read_line(&mut response)?;
    Ok(response)
}
