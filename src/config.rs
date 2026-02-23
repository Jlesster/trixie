// config.rs — kitty-compositor configuration

use serde::Deserialize;
use std::path::PathBuf;

// ── top-level ─────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_terminal")]
    pub terminal: String,
    #[serde(default = "default_seat_name")]
    pub seat_name: String,
    #[serde(default = "default_background_color")]
    pub background_color: [f32; 4],
    #[serde(default)]
    pub keyboard: KeyboardConfig,
    #[serde(default)]
    pub keybinds: Vec<Keybind>,
}

// ── keyboard sub-table ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct KeyboardConfig {
    pub layout: Option<String>,
    pub variant: Option<String>,
    pub options: Option<String>,
    #[serde(default = "default_repeat_delay")]
    pub repeat_delay: u32,
    #[serde(default = "default_repeat_rate")]
    pub repeat_rate: u32,
}

// ── keybind sub-table ─────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Keybind {
    #[serde(default)]
    pub mods: Vec<String>,
    pub key: String,
    pub action: KeyAction,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum KeyAction {
    Quit,
    CloseWindow,
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
            keybinds: vec![Keybind {
                mods: vec!["super".into(), "shift".into()],
                key: "Print".into(),
                action: KeyAction::Quit,
            }],
        }
    }
}

// ── modifier matching helper ──────────────────────────────────────────────────

use smithay::input::keyboard::ModifiersState;

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
    pub fn load() -> Self {
        let Some(path) = Self::config_path() else {
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
        if let Ok(p) = std::env::var("KITTY_COMPOSITOR_CONFIG") {
            let path = PathBuf::from(p);
            if path.exists() {
                return Some(path);
            }
        }
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

    /// Split `self.terminal` into (binary, args), respecting shell-style quoting.
    /// e.g. `kitty -c ~/.config/trixie/kitty/kitty.conf`
    ///   → ("kitty", ["-c", "~/.config/trixie/kitty/kitty.conf"])
    pub fn terminal_cmd(&self) -> (String, Vec<String>) {
        let mut parts = shell_words(&self.terminal);
        if parts.is_empty() {
            return ("kitty".into(), vec![]);
        }
        let bin = expand_tilde(&parts.remove(0));
        let args = parts.into_iter().map(|a| expand_tilde(&a)).collect();
        (bin, args)
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Expand a leading `~` to the user's home directory.
pub fn expand_tilde(s: &str) -> String {
    if s.starts_with('~') {
        let home = std::env::var("HOME").unwrap_or_default();
        format!("{}{}", home, &s[1..])
    } else {
        s.to_owned()
    }
}

/// Minimal shell-word splitter: handles single/double quotes and backslash escapes.
fn shell_words(s: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' if cur.is_empty() => {}
            ' ' | '\t' => {
                words.push(std::mem::take(&mut cur));
            }
            '\'' => {
                for c in chars.by_ref() {
                    if c == '\'' {
                        break;
                    }
                    cur.push(c);
                }
            }
            '"' => {
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' => {
                            if let Some(e) = chars.next() {
                                cur.push(e);
                            }
                        }
                        _ => cur.push(c),
                    }
                }
            }
            '\\' => {
                if let Some(e) = chars.next() {
                    cur.push(e);
                }
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    words
}
