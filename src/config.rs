// config.rs — kitty-compositor configuration
//
// Loaded from (in priority order):
//   1. $KITTY_COMPOSITOR_CONFIG
//   2. $XDG_CONFIG_HOME/kitty-compositor/config.toml
//   3. ~/.config/trixie/config.toml
//
// All fields have sane defaults so the file is entirely optional.

use serde::Deserialize;
use std::path::PathBuf;

// ── top-level ─────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Command used to launch (and relaunch) the terminal.
    /// Defaults to `"kitty"`.
    #[serde(default = "default_terminal")]
    pub terminal: String,

    /// Wayland seat name advertised to clients.
    /// Defaults to `"seat0"`.
    #[serde(default = "default_seat_name")]
    pub seat_name: String,

    /// RGBA background/clear colour shown when no window covers the output.
    /// Values are linear floats in [0.0, 1.0].  Defaults to a near-black.
    #[serde(default = "default_background_color")]
    pub background_color: [f32; 4],

    /// Keyboard configuration.
    #[serde(default)]
    pub keyboard: KeyboardConfig,

    /// Keybinds.
    #[serde(default)]
    pub keybinds: Vec<Keybind>,
}

// ── keyboard sub-table ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct KeyboardConfig {
    /// XKB layout name, e.g. `"us"`, `"gb"`, `"de"`.
    /// Leave unset to use the system default.
    pub layout: Option<String>,

    /// XKB variant, e.g. `"dvorak"`, `"colemak"`.
    /// Leave unset to use the layout default.
    pub variant: Option<String>,

    /// XKB options string, e.g. `"ctrl:nocaps,compose:ralt"`.
    /// Leave unset for no extra options.
    pub options: Option<String>,

    /// Milliseconds before key repeat begins.  Defaults to 200.
    #[serde(default = "default_repeat_delay")]
    pub repeat_delay: u32,

    /// Key repeats per second.  Defaults to 25.
    #[serde(default = "default_repeat_rate")]
    pub repeat_rate: u32,
}

// ── keybind sub-table ─────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Keybind {
    /// Modifier keys — any subset of `"super"`, `"shift"`, `"ctrl"`, `"alt"`.
    #[serde(default)]
    pub mods: Vec<String>,

    /// XKB key name, e.g. `"Return"`, `"t"`, `"F1"`, `"Escape"`.
    pub key: String,

    /// Action to perform when the bind fires.
    pub action: KeyAction,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum KeyAction {
    /// Quit the compositor cleanly.
    Quit,
    /// Close the currently focused window.
    CloseWindow,
    /// Spawn an external program.
    Spawn {
        command: String,
        #[serde(default)]
        args: Vec<String>,
    },
}

// ── defaults ──────────────────────────────────────────────────────────────────

fn default_terminal() -> String {
    "kitty".into()
}
fn default_seat_name() -> String {
    "seat0".into()
}
fn default_background_color() -> [f32; 4] {
    [0.05, 0.05, 0.05, 1.0]
}
fn default_repeat_delay() -> u32 {
    200
}
fn default_repeat_rate() -> u32 {
    25
}

impl Default for Config {
    fn default() -> Self {
        Self {
            terminal: default_terminal(),
            seat_name: default_seat_name(),
            background_color: default_background_color(),
            keyboard: KeyboardConfig::default(),
            keybinds: vec![],
        }
    }
}

// ── modifier matching helper ───────────────────────────────────────────────────

use smithay::input::keyboard::ModifiersState;

/// Returns true if the active modifiers exactly match the list in the keybind.
pub fn mods_match(mods: &ModifiersState, required: &[String]) -> bool {
    let want_super = required.iter().any(|m| m == "super");
    let want_shift = required.iter().any(|m| m == "shift");
    let want_ctrl = required.iter().any(|m| m == "ctrl");
    let want_alt = required.iter().any(|m| m == "alt");

    mods.logo == want_super
        && mods.shift == want_shift
        && mods.ctrl == want_ctrl
        && mods.alt == want_alt
}

// ── loading ───────────────────────────────────────────────────────────────────

impl Config {
    /// Load config from the first path that exists, or return defaults.
    pub fn load() -> Self {
        let path = Self::config_path();

        let Some(path) = path else {
            tracing::info!("No config file found — using defaults");
            return Config::default();
        };

        tracing::info!("Loading config from {}", path.display());
        match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<Config>(&text) {
                Ok(cfg) => cfg,
                Err(e) => {
                    tracing::warn!(
                        "Config parse error in {}: {e} — using defaults",
                        path.display()
                    );
                    Config::default()
                }
            },
            Err(e) => {
                tracing::warn!("Could not read {}: {e} — using defaults", path.display());
                Config::default()
            }
        }
    }

    fn config_path() -> Option<PathBuf> {
        // 1. Explicit env override
        if let Ok(p) = std::env::var("KITTY_COMPOSITOR_CONFIG") {
            let path = PathBuf::from(p);
            if path.exists() {
                return Some(path);
            }
        }

        // 2. XDG_CONFIG_HOME / fallback ~/.config
        let xdg_base = std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_default();
                PathBuf::from(home).join(".config")
            });

        let path = xdg_base.join("trixie").join("config.toml");
        if path.exists() {
            return Some(path);
        }

        None
    }
}
