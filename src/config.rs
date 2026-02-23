// config.rs — trixie compositor configuration
//
// Config is loaded from ~/.config/trixie/*.json (or $TRIXIE_CONFIG_DIR).
// Files are merged in alphabetical order. Recognised filenames and the keys
// they are expected to carry (all optional, unknown keys are silently ignored):
//
//   general.json  →  terminal, seat_name, background_color
//   keyboard.json →  keyboard  { layout, variant, options, repeat_delay, repeat_rate }
//   keymaps.json  →  keybinds  [ { mods, key, action } … ]
//   rules.json    →  window_rules [ { app_id, title, floating, size, position } … ]
//
// Any file may contain any subset of the above keys; they are all merged into
// a single Config.  Unknown top-level keys are ignored (no deny_unknown_fields
// at the root level) so users can freely split things however they like.

use serde::Deserialize;
use std::path::PathBuf;

// ── top-level ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Config {
    pub terminal: String,
    pub seat_name: String,
    pub background_color: [f32; 4],
    pub keyboard: KeyboardConfig,
    pub keybinds: Vec<Keybind>,
    pub window_rules: Vec<WindowRule>,
}

/// Serde target for a single JSON file — every field is optional so partial
/// files work without errors.
#[derive(Debug, Default, Deserialize)]
struct RawConfig {
    terminal: Option<String>,
    seat_name: Option<String>,
    background_color: Option<[f32; 4]>,
    #[serde(default)]
    keyboard: RawKeyboardConfig,
    #[serde(default)]
    keybinds: Vec<Keybind>,
    #[serde(default)]
    window_rules: Vec<WindowRule>,
}

// ── keyboard sub-table ────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct KeyboardConfig {
    pub layout: Option<String>,
    pub variant: Option<String>,
    pub options: Option<String>,
    pub repeat_delay: u32,
    pub repeat_rate: u32,
}

impl Default for KeyboardConfig {
    fn default() -> Self {
        Self {
            layout: None,
            variant: None,
            options: None,
            repeat_delay: 200,
            repeat_rate: 25,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct RawKeyboardConfig {
    layout: Option<String>,
    variant: Option<String>,
    options: Option<String>,
    repeat_delay: Option<u32>,
    repeat_rate: Option<u32>,
}

// ── keybind ───────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Clone)]
pub struct Keybind {
    #[serde(default)]
    pub mods: Vec<String>,
    pub key: String,
    pub action: KeyAction,
}

impl Keybind {
    /// Return a normalised copy: mods lowercased, key name lowercased.
    /// Normalisation happens once at load time so hot-path matching is cheap.
    fn normalise(mut self) -> Self {
        self.mods = self.mods.into_iter().map(|m| m.to_lowercase()).collect();
        self.key = self.key.to_lowercase();
        self
    }
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

// ── window rule ───────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Clone)]
pub struct WindowRule {
    /// Substring match against the XDG app-id.
    pub app_id: Option<String>,
    /// Substring match against the XDG title.
    pub title: Option<String>,
    #[serde(default)]
    pub floating: bool,
    /// Fixed logical size [width, height].  `null` → use rule default / centre.
    pub size: Option<[i32; 2]>,
    /// Fixed logical position [x, y].  `null` → centre on output.
    pub position: Option<[i32; 2]>,
}

impl WindowRule {
    /// Returns true when both constraints (if present) match.
    pub fn matches(&self, app_id: &str, title: &str) -> bool {
        self.app_id.as_deref().map_or(true, |p| app_id.contains(p))
            && self.title.as_deref().map_or(true, |p| title.contains(p))
    }
}

/// Inserted into a Window's `user_data` map for every window that matched a
/// floating rule.  `render_surface` checks for this marker to skip the
/// full-output resize step.
#[derive(Debug)]
pub struct FloatingMarker {
    pub size: Option<(i32, i32)>,
    pub position: Option<(i32, i32)>,
}

// ── defaults ──────────────────────────────────────────────────────────────────

impl Default for Config {
    fn default() -> Self {
        Self {
            terminal: "kitty".into(),
            seat_name: "seat0".into(),
            background_color: [0.05, 0.05, 0.05, 1.0],
            keyboard: KeyboardConfig::default(),
            keybinds: vec![Keybind {
                mods: vec!["super".into(), "shift".into()],
                key: "print".into(),
                action: KeyAction::Quit,
            }],
            window_rules: vec![],
        }
    }
}

// ── loading ───────────────────────────────────────────────────────────────────

impl Config {
    /// Load and merge all `*.json` files found in the config directory.
    /// Silently falls back to `Config::default()` when the directory does not
    /// exist or contains no JSON files.
    pub fn load() -> Self {
        let dir = Self::config_dir();
        tracing::info!("Config dir: {}", dir.display());

        // Collect *.json paths, sorted so load order is deterministic.
        let mut paths: Vec<PathBuf> = match std::fs::read_dir(&dir) {
            Ok(rd) => rd
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
                .collect(),
            Err(e) => {
                tracing::info!(
                    "Could not read config dir {}: {e} — using defaults",
                    dir.display()
                );
                return Config::default();
            }
        };
        paths.sort();

        if paths.is_empty() {
            tracing::info!("No JSON files in {} — using defaults", dir.display());
            return Config::default();
        }

        // Start from built-in defaults, then overlay each file.
        let mut cfg = Config::default();
        // Reset keybinds/rules so they don't duplicate the baked-in defaults
        // when the user has provided their own files.
        cfg.keybinds.clear();
        cfg.window_rules.clear();
        let mut has_keybinds = false;

        for path in &paths {
            tracing::info!("Loading config fragment: {}", path.display());
            let text = match std::fs::read_to_string(path) {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!("Could not read {}: {e}", path.display());
                    continue;
                }
            };
            let raw: RawConfig = match serde_json::from_str(&text) {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!("JSON parse error in {}: {e}", path.display());
                    continue;
                }
            };

            // Scalar overrides.
            if let Some(t) = raw.terminal {
                cfg.terminal = t;
            }
            if let Some(s) = raw.seat_name {
                cfg.seat_name = s;
            }
            if let Some(b) = raw.background_color {
                cfg.background_color = b;
            }

            // Keyboard: merge individual fields so keyboard.json only needs the
            // keys the user actually wants to change.
            let rk = raw.keyboard;
            if rk.layout.is_some() {
                cfg.keyboard.layout = rk.layout;
            }
            if rk.variant.is_some() {
                cfg.keyboard.variant = rk.variant;
            }
            if rk.options.is_some() {
                cfg.keyboard.options = rk.options;
            }
            if let Some(d) = rk.repeat_delay {
                cfg.keyboard.repeat_delay = d;
            }
            if let Some(r) = rk.repeat_rate {
                cfg.keyboard.repeat_rate = r;
            }

            // Lists are accumulated across files.
            // Normalise at load time so hot-path matching never has to case-fold.
            if !raw.keybinds.is_empty() {
                has_keybinds = true;
                cfg.keybinds
                    .extend(raw.keybinds.into_iter().map(Keybind::normalise));
            }
            cfg.window_rules.extend(raw.window_rules);
        }

        // If no file provided keybinds, restore the built-in quit shortcut so
        // the compositor is not completely impossible to exit.
        if !has_keybinds {
            cfg.keybinds = Config::default().keybinds;
        }

        cfg
    }

    /// Returns the config directory, honouring `$TRIXIE_CONFIG_DIR` and
    /// `$XDG_CONFIG_HOME` in that order.
    pub fn config_dir() -> PathBuf {
        if let Ok(p) = std::env::var("TRIXIE_CONFIG_DIR") {
            return PathBuf::from(p);
        }
        let base = std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config")
            });
        base.join("trixie")
    }

    /// Split `self.terminal` into (binary, args), respecting shell-style quoting.
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

// ── modifier matching helper ──────────────────────────────────────────────────

use smithay::input::keyboard::ModifiersState;

/// Check that the live modifier state matches the keybind's required modifiers.
/// Both sides are already lowercase (mods normalised at load, strings are literals).
pub fn mods_match(mods: &ModifiersState, required: &[String]) -> bool {
    mods.logo == required.iter().any(|m| m == "super")
        && mods.shift == required.iter().any(|m| m == "shift")
        && mods.ctrl == required.iter().any(|m| m == "ctrl")
        && mods.alt == required.iter().any(|m| m == "alt")
}

/// Normalise an xkb key name to lowercase for case-insensitive matching
/// against the (already-lowercased) `key` field in Keybind.
pub fn normalise_key_name(name: &str) -> String {
    name.to_lowercase()
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Expand a leading `~` to `$HOME`.
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
            ' ' | '\t' => words.push(std::mem::take(&mut cur)),
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
