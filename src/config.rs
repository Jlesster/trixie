// config.rs — trixie compositor configuration
//
// Config is loaded from ~/.config/trixie/*.json (or $TRIXIE_CONFIG_DIR).
// Files are merged in alphabetical order. Recognised filenames and the keys
// they are expected to carry (all optional, unknown keys are silently ignored):
//
//   general.json  →  terminal, seat_name, background_color,
//                    target_hz, vsync,
//                    vibrance { enabled, strength, balance }
//   keyboard.json →  keyboard  { layout, variant, options, repeat_delay, repeat_rate }
//   keymaps.json  →  keybinds  [ { mods, key, action } … ]
//   rules.json    →  window_rules [ { app_id, title, floating, size, position } … ]
//   autostart.json → exec      [ { command, args? } … ]
//                    exec_once [ { command, args? } … ]

use serde::Deserialize;
use std::path::PathBuf;

// ── top-level ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Config {
    pub terminal: String,
    pub seat_name: String,
    pub background_color: [f32; 4],
    pub target_hz: Option<u64>,
    pub vsync: VsyncMode,
    pub vibrance: VibranceConfig,
    pub keyboard: KeyboardConfig,
    pub keybinds: Vec<Keybind>,
    pub window_rules: Vec<WindowRule>,
    pub exec: Vec<ExecEntry>,
    pub exec_once: Vec<ExecEntry>,
}

impl Config {
    pub fn frame_duration_for(&self, connector_hz: u64) -> std::time::Duration {
        let hz = match self.target_hz {
            Some(cap) => cap.min(connector_hz).max(1),
            None => connector_hz.max(1),
        };
        std::time::Duration::from_micros(1_000_000 / hz)
    }
}

#[derive(Debug, Default, Deserialize)]
struct RawConfig {
    terminal: Option<String>,
    seat_name: Option<String>,
    background_color: Option<[f32; 4]>,
    target_hz: Option<u64>,
    vsync: Option<VsyncMode>,
    vibrance: Option<RawVibranceConfig>,
    #[serde(default)]
    keyboard: RawKeyboardConfig,
    #[serde(default)]
    keybinds: Vec<Keybind>,
    #[serde(default)]
    window_rules: Vec<WindowRule>,
    #[serde(default)]
    exec: Vec<ExecEntry>,
    #[serde(default)]
    exec_once: Vec<ExecEntry>,
}

// ── vibrance ──────────────────────────────────────────────────────────────────

/// Vibrance post-processing settings.
///
/// JSON shape (all fields optional):
/// ```json
/// "vibrance": {
///     "enabled":  true,
///     "strength": 0.45,
///     "balance":  [1.0, 1.0, 1.0]
/// }
/// ```
/// `strength` ranges from -1.0 (desaturate) to 1.0 (saturate). Default 0.45.
/// `balance`  per-channel RGB weight applied to the vibrance coefficient.
#[derive(Debug, Clone)]
pub struct VibranceConfig {
    pub enabled: bool,
    pub strength: f32,
    pub balance: [f32; 3],
}

impl Default for VibranceConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            strength: 0.45,
            balance: [1.0, 1.0, 1.0],
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct RawVibranceConfig {
    enabled: Option<bool>,
    strength: Option<f32>,
    balance: Option<[f32; 3]>,
}

impl RawVibranceConfig {
    fn merge_into(self, dst: &mut VibranceConfig) {
        if let Some(e) = self.enabled {
            dst.enabled = e;
        }
        if let Some(s) = self.strength {
            dst.strength = s.clamp(-1.0, 1.0);
        }
        if let Some(b) = self.balance {
            dst.balance = b;
        }
    }
}

// ── vsync mode ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VsyncMode {
    #[default]
    On,
    Off,
    Adaptive,
}

// ── exec entry ────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Clone)]
pub struct ExecEntry {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

// ── keyboard ──────────────────────────────────────────────────────────────────

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
    ReloadConfig,
    Spawn {
        command: String,
        #[serde(default)]
        args: Vec<String>,
    },
}

// ── window rule ───────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Clone)]
pub struct WindowRule {
    pub app_id: Option<String>,
    pub title: Option<String>,
    #[serde(default)]
    pub floating: bool,
    pub size: Option<[i32; 2]>,
    pub position: Option<[i32; 2]>,
}

impl WindowRule {
    pub fn matches(&self, app_id: &str, title: &str) -> bool {
        self.app_id.as_deref().map_or(true, |p| app_id.contains(p))
            && self.title.as_deref().map_or(true, |p| title.contains(p))
    }
}

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
            target_hz: None,
            vsync: VsyncMode::On,
            vibrance: VibranceConfig::default(),
            keyboard: KeyboardConfig::default(),
            keybinds: vec![Keybind {
                mods: vec!["super".into(), "shift".into()],
                key: "print".into(),
                action: KeyAction::Quit,
            }],
            window_rules: vec![],
            exec: vec![],
            exec_once: vec![],
        }
    }
}

// ── loading ───────────────────────────────────────────────────────────────────

impl Config {
    pub fn load() -> Self {
        let dir = Self::config_dir();
        tracing::info!("Config dir: {}", dir.display());

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

        let mut cfg = Config::default();
        cfg.keybinds.clear();
        cfg.window_rules.clear();
        cfg.exec.clear();
        cfg.exec_once.clear();
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

            if let Some(t) = raw.terminal {
                cfg.terminal = t;
            }
            if let Some(s) = raw.seat_name {
                cfg.seat_name = s;
            }
            if let Some(b) = raw.background_color {
                cfg.background_color = b;
            }
            if let Some(h) = raw.target_hz {
                cfg.target_hz = Some(h);
            }
            if let Some(v) = raw.vsync {
                cfg.vsync = v;
            }
            if let Some(vib) = raw.vibrance {
                vib.merge_into(&mut cfg.vibrance);
            }

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

            if !raw.keybinds.is_empty() {
                has_keybinds = true;
                cfg.keybinds
                    .extend(raw.keybinds.into_iter().map(Keybind::normalise));
            }
            cfg.window_rules.extend(raw.window_rules);
            cfg.exec.extend(raw.exec);
            cfg.exec_once.extend(raw.exec_once);
        }

        if !has_keybinds {
            cfg.keybinds = Config::default().keybinds;
        }

        tracing::info!(
            "Config loaded — vsync={:?} target_hz={:?} vibrance={} strength={:.2}",
            cfg.vsync,
            cfg.target_hz,
            cfg.vibrance.enabled,
            cfg.vibrance.strength
        );
        cfg
    }

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

// ── modifier matching ─────────────────────────────────────────────────────────

use smithay::input::keyboard::ModifiersState;

pub fn mods_match(mods: &ModifiersState, required: &[String]) -> bool {
    mods.logo == required.iter().any(|m| m == "super")
        && mods.shift == required.iter().any(|m| m == "shift")
        && mods.ctrl == required.iter().any(|m| m == "ctrl")
        && mods.alt == required.iter().any(|m| m == "alt")
}

pub fn normalise_key_name(name: &str) -> String {
    name.to_lowercase()
}

// ── helpers ───────────────────────────────────────────────────────────────────

pub fn expand_tilde(s: &str) -> String {
    if s.starts_with('~') {
        let home = std::env::var("HOME").unwrap_or_default();
        format!("{}{}", home, &s[1..])
    } else {
        s.to_owned()
    }
}

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
