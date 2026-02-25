// embedded_ipc.rs — Unix socket IPC between the TWM tile manager and the
// compositor's EmbeddedManager.
//
// Socket path: $XDG_RUNTIME_DIR/trixie-embed.sock
//
// Protocol: newline-delimited JSON, one command → one response.
//
// ── Commands ──────────────────────────────────────────────────────────────────
//
//   Spawn a new embedded app (compositor launches it + reserves placement):
//   { "cmd": "spawn", "app_id": "firefox", "args": [],
//     "x": 0, "y": 0, "w": 960, "h": 1080 }
//
//   Resize / move an already-running embedded surface:
//   { "cmd": "move", "app_id": "firefox", "x": 0, "y": 0, "w": 1920, "h": 1080 }
//
//   Give keyboard focus to an embedded surface:
//   { "cmd": "focus", "app_id": "firefox" }
//
//   Send xdg_toplevel.close and remove from EmbeddedManager:
//   { "cmd": "close", "app_id": "firefox" }
//
//   List all currently embedded surfaces:
//   { "cmd": "list" }
//
// ── Responses ─────────────────────────────────────────────────────────────────
//
//   { "ok": true, "windows": [ { "app_id": "firefox", "x":0,"y":0,"w":960,"h":1080,
//                                 "mapped": true } ] }
//   { "ok": false, "error": "app_id 'foo' not found" }
//
// ── TWM side usage (twm.rs / trixterm) ───────────────────────────────────────
//
//   On tile create:   send Spawn with the tile's pixel rect
//   On tile resize:   send Move  with the new pixel rect
//   On tile focus:    send Focus
//   On tile close:    send Close
//   On pane list:     send List to sync state
//
// The TWM converts terminal cell coords to pixels using cell_w / cell_h.

use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::PathBuf,
};

use serde::{Deserialize, Serialize};

// ── socket path ───────────────────────────────────────────────────────────────

pub fn socket_path() -> PathBuf {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(runtime).join("trixie-embed.sock")
}

// ── wire types ────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Clone)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum EmbedCommand {
    Spawn {
        app_id: String,
        #[serde(default)]
        args: Vec<String>,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
    },
    Move {
        app_id: String,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
    },
    Focus {
        app_id: String,
    },
    Close {
        app_id: String,
    },
    List,
}

#[derive(Debug, Serialize, Clone)]
pub struct WindowStatus {
    pub app_id: String,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    /// True once the app has committed its first buffer.
    pub mapped: bool,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum EmbedResponse {
    Ok {
        ok: bool,
        windows: Vec<WindowStatus>,
    },
    Err {
        ok: bool,
        error: String,
    },
}

impl EmbedResponse {
    pub fn ok(windows: Vec<WindowStatus>) -> Self {
        Self::Ok { ok: true, windows }
    }
    pub fn err(msg: impl Into<String>) -> Self {
        Self::Err {
            ok: false,
            error: msg.into(),
        }
    }
}

// ── server (compositor side) ──────────────────────────────────────────────────

/// Non-blocking IPC server. Call `drain()` once per event-loop tick.
/// The socket is non-blocking; connections are processed inline (each
/// command is tiny so the round-trip is sub-millisecond).
pub struct EmbedIpcServer {
    listener: Option<UnixListener>,
    /// Pending commands that have been received but not yet acted on.
    /// `drain()` returns them and clears the queue.
    pending: Vec<EmbedCommand>,
    /// Snapshot of window state used to respond to List commands.
    /// Updated by the compositor after every structural change.
    pub windows: Vec<WindowStatus>,
}

impl EmbedIpcServer {
    pub fn bind() -> Self {
        let path = socket_path();
        let _ = std::fs::remove_file(&path);
        match UnixListener::bind(&path) {
            Ok(l) => {
                l.set_nonblocking(true).ok();
                tracing::info!("Embed IPC socket: {}", path.display());
                Self {
                    listener: Some(l),
                    pending: Vec::new(),
                    windows: Vec::new(),
                }
            }
            Err(e) => {
                tracing::warn!("Could not bind embed IPC socket: {e}");
                Self {
                    listener: None,
                    pending: Vec::new(),
                    windows: Vec::new(),
                }
            }
        }
    }

    /// Poll the socket for new connections and buffer any commands received.
    /// Returns the buffered commands and clears the internal queue.
    /// Call this once per event-loop tick before rendering.
    pub fn drain(&mut self) -> Vec<EmbedCommand> {
        if let Some(ref listener) = self.listener {
            loop {
                match listener.accept() {
                    Ok((stream, _)) => {
                        if let Some(cmd) = Self::handle_connection(stream, &self.windows) {
                            self.pending.push(cmd);
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) => {
                        tracing::warn!("Embed IPC accept error: {e}");
                        break;
                    }
                }
            }
        }
        std::mem::take(&mut self.pending)
    }

    /// Update the window list snapshot (call after any add/remove/move).
    pub fn update_windows(&mut self, windows: Vec<WindowStatus>) {
        self.windows = windows;
    }

    fn handle_connection(mut stream: UnixStream, windows: &[WindowStatus]) -> Option<EmbedCommand> {
        stream.set_nonblocking(false).ok();
        let mut reader = BufReader::new(stream.try_clone().ok()?);
        let mut line = String::new();
        reader.read_line(&mut line).ok()?;
        let line = line.trim();
        if line.is_empty() {
            return None;
        }

        let cmd: EmbedCommand = match serde_json::from_str(line) {
            Ok(c) => c,
            Err(e) => {
                let resp = EmbedResponse::err(format!("parse error: {e}"));
                if let Ok(mut j) = serde_json::to_string(&resp) {
                    j.push('\n');
                    stream.write_all(j.as_bytes()).ok();
                }
                return None;
            }
        };

        // List is answered immediately from the cached snapshot —
        // no need to wait for the compositor to process it.
        if matches!(cmd, EmbedCommand::List) {
            let resp = EmbedResponse::ok(windows.to_vec());
            if let Ok(mut j) = serde_json::to_string(&resp) {
                j.push('\n');
                stream.write_all(j.as_bytes()).ok();
            }
            return None; // don't push to pending queue
        }

        // For all other commands send an immediate ACK and queue for processing.
        let resp = EmbedResponse::ok(windows.to_vec());
        if let Ok(mut j) = serde_json::to_string(&resp) {
            j.push('\n');
            stream.write_all(j.as_bytes()).ok();
        }

        Some(cmd)
    }
}

impl Default for EmbedIpcServer {
    fn default() -> Self {
        Self {
            listener: None,
            pending: Vec::new(),
            windows: Vec::new(),
        }
    }
}

// ── client helper (TWM / trixterm side) ──────────────────────────────────────

/// Send a single command to the compositor and parse the response.
/// Blocks until the compositor replies (typically < 1ms).
pub fn send_command(cmd: &EmbedCommand) -> Result<EmbedResponse, String> {
    let path = socket_path();
    let mut stream =
        UnixStream::connect(&path).map_err(|e| format!("connect to {}: {e}", path.display()))?;

    let mut json = serde_json::to_string(cmd).map_err(|e| e.to_string())?;
    json.push('\n');
    stream
        .write_all(json.as_bytes())
        .map_err(|e| e.to_string())?;

    let mut resp_line = String::new();
    BufReader::new(stream)
        .read_line(&mut resp_line)
        .map_err(|e| e.to_string())?;

    serde_json::from_str(resp_line.trim()).map_err(|e| e.to_string())
}

/// Convenience: spawn an embedded app into a tile.
/// `cell_*` are the tile dimensions in terminal cells;
/// `pixel_*` are the actual pixel coords the compositor should use.
pub fn spawn_embedded(
    app_id: &str,
    args: &[String],
    x: i32,
    y: i32,
    w: i32,
    h: i32,
) -> Result<EmbedResponse, String> {
    send_command(&EmbedCommand::Spawn {
        app_id: app_id.to_owned(),
        args: args.to_vec(),
        x,
        y,
        w,
        h,
    })
}

/// Convenience: notify the compositor that a tile was resized.
pub fn move_embedded(
    app_id: &str,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
) -> Result<EmbedResponse, String> {
    send_command(&EmbedCommand::Move {
        app_id: app_id.to_owned(),
        x,
        y,
        w,
        h,
    })
}
