// config.rs — trixie compositor configuration
//
// Config is loaded from ~/.config/trixie/*.conf (or $TRIXIE_CONFIG_DIR).
// Uses Hyprland-style key = value / section { } syntax.
//
// Recognised files and the keys they carry (all optional):
//
//   general.conf   →  terminal, seat_name, background_color,
//                     target_hz, vsync
//                     vibrance { enabled, strength, balance }
//   keyboard.conf  →  keyboard { layout, variant, options,
//                                repeat_delay, repeat_rate,
//                                modifier }
//   keymaps.conf   →  bind = <mods>, <key>, <action>
//   rules.conf     →  windowrule = <action>, <matcher>[, size W H][, pos X Y]
//   autostart.conf →  exec      = <command>
//                     exec_once = <command>

use crate::shader_config::ShaderRegistry;
use crate::util::{expand_tilde, hex4, resolve_path, shell_words, strip_comment};
use std::path::{Path, PathBuf};

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
    pub shaders: ShaderRegistry,
}

impl Config {
    pub fn frame_duration_for(&self, connector_hz: u64) -> std::time::Duration {
        let hz = match self.target_hz {
            Some(cap) => cap.min(connector_hz).max(1),
            None => connector_hz.max(1),
        };
        let hz = if hz == 0 {
            self.target_hz.unwrap_or(60).max(1)
        } else {
            hz
        };
        std::time::Duration::from_micros(1_000_000 / hz)
    }
}

// ── vibrance ──────────────────────────────────────────────────────────────────

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

// ── vsync mode ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VsyncMode {
    #[default]
    On,
    Off,
    Adaptive,
}

// ── exec entry ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ExecEntry {
    pub command: String,
    pub args: Vec<String>,
}

// ── keyboard ──────────────────────────────────────────────────────────────────
//
// Unified keyboard config — absorbs both the old KeyboardConfig and the
// `modifier` field that previously lived only in twm_config::InputConfig.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modifier {
    Super,
    Alt,
    Ctrl,
}

impl Default for Modifier {
    fn default() -> Self {
        Self::Super
    }
}

#[derive(Debug, Clone)]
pub struct KeyboardConfig {
    pub layout: Option<String>,
    pub variant: Option<String>,
    pub options: Option<String>,
    pub repeat_delay: u32,
    pub repeat_rate: u32,
    /// Primary compositor modifier key (default: Super).
    pub modifier: Modifier,
}

impl Default for KeyboardConfig {
    fn default() -> Self {
        Self {
            layout: None,
            variant: None,
            options: None,
            repeat_delay: 200,
            repeat_rate: 25,
            modifier: Modifier::default(),
        }
    }
}

// ── keybind ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Keybind {
    pub mods: Vec<String>,
    pub key: String,
    pub action: KeyAction,
}

#[derive(Debug, Clone)]
pub enum KeyAction {
    Quit,
    CloseWindow,
    ReloadConfig,
    Spawn { command: String, args: Vec<String> },
}

// ── window rule ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct WindowRule {
    pub app_id: Option<String>,
    pub title: Option<String>,
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
            terminal: "trixterm".into(),
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
            shaders: ShaderRegistry::default(),
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
                .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("conf"))
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
            tracing::info!("No .conf files in {} — using defaults", dir.display());
            return Config::default();
        }

        let mut cfg = Config::default();
        cfg.keybinds.clear();
        cfg.window_rules.clear();
        cfg.exec.clear();
        cfg.exec_once.clear();
        let mut has_keybinds = false;

        for path in &paths {
            tracing::info!("Loading config: {}", path.display());
            let text = match std::fs::read_to_string(path) {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!("Could not read {}: {e}", path.display());
                    continue;
                }
            };
            let mut stack = vec![path.canonicalize().unwrap_or_else(|_| path.clone())];
            if let Err(e) = parse_into(&text, path, &mut stack, &mut cfg, &mut has_keybinds) {
                tracing::warn!("Config error in {}: {e}", path.display());
            }
        }

        if !has_keybinds {
            cfg.keybinds = Config::default().keybinds;
        }

        cfg.shaders = ShaderRegistry::load(&Self::config_dir());

        tracing::info!(
            "Config loaded — terminal={:?} vsync={:?} target_hz={:?} vibrance={} strength={:.2}",
            cfg.terminal,
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

// ── parser ────────────────────────────────────────────────────────────────────

fn parse_into(
    text: &str,
    file: &Path,
    stack: &mut Vec<PathBuf>,
    cfg: &mut Config,
    has_keybinds: &mut bool,
) -> Result<(), String> {
    let mut section_stack: Vec<String> = Vec::new();

    for (raw_no, raw_line) in text.lines().enumerate() {
        let lineno = raw_no + 1;
        let line = strip_comment(raw_line).trim();

        if line.is_empty() {
            continue;
        }

        // Open section
        if line.ends_with('{') {
            let name = line.trim_end_matches('{').trim().to_lowercase();
            section_stack.push(name);
            continue;
        }

        // Close section
        if line == "}" {
            section_stack
                .pop()
                .ok_or_else(|| format!("{}:{} — unexpected `}}`", file.display(), lineno))?;
            continue;
        }

        let (key, value) = split_kv(line).ok_or_else(|| {
            format!(
                "{}:{} — expected `key = value`, got `{line}`",
                file.display(),
                lineno
            )
        })?;

        let section = section_stack.last().map(String::as_str).unwrap_or("");

        // source directive
        if key == "source" && section.is_empty() {
            let path = resolve_path(value, file);
            if !path.exists() {
                tracing::warn!(
                    "{}:{} — source `{}` not found (skipping)",
                    file.display(),
                    lineno,
                    path.display()
                );
                continue;
            }
            let canon = path.canonicalize().unwrap_or_else(|_| path.clone());
            if stack.contains(&canon) {
                return Err(format!("circular source: {}", path.display()));
            }
            let text2 = std::fs::read_to_string(&path)
                .map_err(|e| format!("cannot read source `{}`: {e}", path.display()))?;
            stack.push(canon);
            parse_into(&text2, &path, stack, cfg, has_keybinds)?;
            stack.pop();
            continue;
        }

        match section {
            "" => apply_toplevel(key, value, file, lineno, cfg, has_keybinds),
            "general" => apply_general(key, value, file, lineno, cfg),
            "vibrance" => apply_vibrance(key, value, file, lineno, &mut cfg.vibrance),
            "keyboard" => apply_keyboard(key, value, file, lineno, &mut cfg.keyboard),
            other => tracing::warn!("{}:{} — unknown section `{other}`", file.display(), lineno),
        }
    }

    if !section_stack.is_empty() {
        return Err(format!(
            "{} — unclosed section(s): {}",
            file.display(),
            section_stack.join(" > ")
        ));
    }
    Ok(())
}

// ── top-level key dispatcher ──────────────────────────────────────────────────

fn apply_toplevel(
    key: &str,
    value: &str,
    file: &Path,
    lineno: usize,
    cfg: &mut Config,
    has_keybinds: &mut bool,
) {
    match key {
        "bind" => match parse_bind(value) {
            Some(kb) => {
                *has_keybinds = true;
                cfg.keybinds.push(kb);
            }
            None => tracing::warn!("{}:{} — invalid bind `{value}`", file.display(), lineno),
        },
        "windowrule" => match parse_windowrule(value) {
            Some(r) => cfg.window_rules.push(r),
            None => tracing::warn!(
                "{}:{} — invalid windowrule `{value}`",
                file.display(),
                lineno
            ),
        },
        "exec" => cfg.exec.push(parse_exec(value)),
        "exec_once" => cfg.exec_once.push(parse_exec(value)),
        // Allow general/vibrance/keyboard keys at top level too (flat files).
        "terminal" => cfg.terminal = value.to_string(),
        "seat_name" => cfg.seat_name = value.to_string(),
        "background_color" => {
            if let Some(c) = parse_color_f32(value) {
                cfg.background_color = c;
            }
        }
        "target_hz" => {
            if let Ok(n) = value.trim().parse::<u64>() {
                cfg.target_hz = Some(n);
            }
        }
        "vsync" => cfg.vsync = parse_vsync(value),
        _ => tracing::warn!(
            "{}:{} — unknown top-level key `{key}`",
            file.display(),
            lineno
        ),
    }
}

// ── section appliers ──────────────────────────────────────────────────────────

fn apply_general(key: &str, value: &str, file: &Path, lineno: usize, cfg: &mut Config) {
    match key {
        "terminal" => cfg.terminal = value.to_string(),
        "seat_name" => cfg.seat_name = value.to_string(),
        "background_color" => {
            if let Some(c) = parse_color_f32(value) {
                cfg.background_color = c;
            } else {
                tracing::warn!(
                    "{}:{} — bad background_color `{value}`",
                    file.display(),
                    lineno
                );
            }
        }
        "target_hz" | "target_hs" => match value.trim().parse::<u64>() {
            Ok(n) => cfg.target_hz = Some(n),
            Err(_) => tracing::warn!("{}:{} — bad target_hz `{value}`", file.display(), lineno),
        },
        "vsync" => cfg.vsync = parse_vsync(value),
        _ => tracing::warn!("{}:{} — unknown general.{key}", file.display(), lineno),
    }
}

fn apply_vibrance(key: &str, value: &str, file: &Path, lineno: usize, v: &mut VibranceConfig) {
    match key {
        "enabled" => match parse_bool(value) {
            Some(b) => v.enabled = b,
            None => tracing::warn!("{}:{} — bad bool `{value}`", file.display(), lineno),
        },
        "strength" => match value.trim().parse::<f32>() {
            Ok(s) => v.strength = s.clamp(-1.0, 1.0),
            Err(_) => tracing::warn!("{}:{} — bad strength `{value}`", file.display(), lineno),
        },
        "balance" => {
            let nums: Vec<f32> = value
                .split_whitespace()
                .filter_map(|s| s.parse().ok())
                .collect();
            if nums.len() == 3 {
                v.balance = [nums[0], nums[1], nums[2]];
            } else {
                tracing::warn!("{}:{} — balance expects 3 floats", file.display(), lineno);
            }
        }
        _ => tracing::warn!("{}:{} — unknown vibrance.{key}", file.display(), lineno),
    }
}

fn apply_keyboard(key: &str, value: &str, file: &Path, lineno: usize, k: &mut KeyboardConfig) {
    match key {
        "layout" => k.layout = Some(value.to_string()),
        "variant" => k.variant = Some(value.to_string()),
        "options" => k.options = Some(value.to_string()),
        "repeat_delay" => match value.trim().parse::<u32>() {
            Ok(n) => k.repeat_delay = n,
            Err(_) => tracing::warn!("{}:{} — bad repeat_delay `{value}`", file.display(), lineno),
        },
        "repeat_rate" => match value.trim().parse::<u32>() {
            Ok(n) => k.repeat_rate = n,
            Err(_) => tracing::warn!("{}:{} — bad repeat_rate `{value}`", file.display(), lineno),
        },
        "modifier" => {
            k.modifier = match value.trim().to_lowercase().as_str() {
                "alt" => Modifier::Alt,
                "ctrl" | "control" => Modifier::Ctrl,
                _ => Modifier::Super,
            };
        }
        _ => tracing::warn!("{}:{} — unknown keyboard.{key}", file.display(), lineno),
    }
}

// ── bind parsing ──────────────────────────────────────────────────────────────
//
// Format:  bind = <mods…>, <key>, <action> [args…]
//
// Examples:
//   bind = super shift, print,   quit
//   bind = super,       return,  spawn kitty
//   bind = super,       q,       close_window
//   bind = super,       r,       reload_config

fn parse_bind(value: &str) -> Option<Keybind> {
    let parts: Vec<&str> = value.splitn(3, ',').map(str::trim).collect();
    if parts.len() < 3 {
        return None;
    }

    let mods: Vec<String> = parts[0]
        .split_whitespace()
        .map(|m| m.to_lowercase())
        .filter(|m| !m.is_empty())
        .collect();

    let key = parts[1].trim().to_lowercase();
    if key.is_empty() {
        return None;
    }

    let action = parse_key_action(parts[2].trim())?;

    Some(Keybind { mods, key, action })
}

fn parse_key_action(s: &str) -> Option<KeyAction> {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix("spawn") {
        let cmd_str = rest.trim();
        if cmd_str.is_empty() {
            return Some(KeyAction::Spawn {
                command: std::env::var("SHELL").unwrap_or_else(|_| "kitty".into()),
                args: vec![],
            });
        }
        let mut words = shell_words(cmd_str);
        if words.is_empty() {
            return None;
        }
        let command = words.remove(0);
        return Some(KeyAction::Spawn {
            command,
            args: words,
        });
    }

    match s {
        "quit" => Some(KeyAction::Quit),
        "close_window" | "close" => Some(KeyAction::CloseWindow),
        "reload_config" | "reload" => Some(KeyAction::ReloadConfig),
        _ => None,
    }
}

// ── windowrule parsing ────────────────────────────────────────────────────────

fn parse_windowrule(value: &str) -> Option<WindowRule> {
    let parts: Vec<&str> = value.split(',').map(str::trim).collect();
    if parts.len() < 2 {
        return None;
    }

    let action = parts[0].to_lowercase();
    let floating = action == "float" || action == "floating";
    let matcher = parts[1].to_string();
    let app_id = if matcher.is_empty() {
        None
    } else {
        Some(matcher)
    };

    let mut size: Option<[i32; 2]> = None;
    let mut position: Option<[i32; 2]> = None;

    for extra in parts.iter().skip(2) {
        if let Some(s) = extra.strip_prefix("size ") {
            let ns: Vec<i32> = s
                .split_whitespace()
                .filter_map(|n| n.parse().ok())
                .collect();
            if ns.len() == 2 {
                size = Some([ns[0], ns[1]]);
            }
        } else if let Some(s) = extra.strip_prefix("pos ") {
            let ns: Vec<i32> = s
                .split_whitespace()
                .filter_map(|n| n.parse().ok())
                .collect();
            if ns.len() == 2 {
                position = Some([ns[0], ns[1]]);
            }
        }
    }

    Some(WindowRule {
        app_id,
        title: None,
        floating,
        size,
        position,
    })
}

// ── exec parsing ──────────────────────────────────────────────────────────────

fn parse_exec(value: &str) -> ExecEntry {
    let mut words = shell_words(value.trim());
    if words.is_empty() {
        return ExecEntry {
            command: value.to_string(),
            args: vec![],
        };
    }
    let command = words.remove(0);
    ExecEntry {
        command,
        args: words,
    }
}

// ── primitive parsers ─────────────────────────────────────────────────────────

fn split_kv(line: &str) -> Option<(&str, &str)> {
    line.find('=')
        .map(|i| (line[..i].trim(), line[i + 1..].trim()))
}

fn parse_bool(s: &str) -> Option<bool> {
    match s.trim().to_lowercase().as_str() {
        "true" | "yes" | "1" | "on" => Some(true),
        "false" | "no" | "0" | "off" => Some(false),
        _ => None,
    }
}

fn parse_vsync(s: &str) -> VsyncMode {
    match s.trim().to_lowercase().as_str() {
        "off" | "false" | "0" => VsyncMode::Off,
        "adaptive" => VsyncMode::Adaptive,
        _ => VsyncMode::On,
    }
}

fn parse_color_f32(s: &str) -> Option<[f32; 4]> {
    let stripped = s.trim().trim_start_matches('#');
    if stripped.len() >= 6 {
        return Some(hex4(s));
    }
    // Try space-separated floats: R G B [A]
    let nums: Vec<f32> = s
        .split_whitespace()
        .filter_map(|n| n.parse().ok())
        .collect();
    match nums.len() {
        3 => Some([nums[0], nums[1], nums[2], 1.0]),
        4 => Some([nums[0], nums[1], nums[2], nums[3]]),
        _ => None,
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

// ── spawn helper (pub for use from input.rs) ──────────────────────────────────

pub fn spawn_process(cmd: &str, args: &[String], wayland_socket: &str) {
    let bin = expand_tilde(cmd);
    if let Err(e) = std::process::Command::new(&bin)
        .args(args)
        .env("WAYLAND_DISPLAY", wayland_socket)
        .spawn()
    {
        tracing::warn!("Spawn failed ({bin}): {e}");
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Config {
        let mut cfg = Config::default();
        cfg.keybinds.clear();
        let mut has_keybinds = false;
        parse_into(
            text,
            Path::new("test.conf"),
            &mut vec![],
            &mut cfg,
            &mut has_keybinds,
        )
        .unwrap();
        cfg
    }

    #[test]
    fn general_section() {
        let cfg = parse("general {\n  terminal = foot\n  vsync = off\n  target_hz = 144\n}");
        assert_eq!(cfg.terminal, "foot");
        assert_eq!(cfg.vsync, VsyncMode::Off);
        assert_eq!(cfg.target_hz, Some(144));
    }

    #[test]
    fn bind_quit() {
        let cfg = parse("bind = super shift, print, quit");
        assert_eq!(cfg.keybinds.len(), 1);
        assert!(matches!(cfg.keybinds[0].action, KeyAction::Quit));
        assert!(cfg.keybinds[0].mods.contains(&"super".to_string()));
        assert!(cfg.keybinds[0].mods.contains(&"shift".to_string()));
    }

    #[test]
    fn bind_spawn() {
        let cfg = parse("bind = super, return, spawn kitty --title term");
        assert!(matches!(
            &cfg.keybinds[0].action,
            KeyAction::Spawn { command, .. } if command == "kitty"
        ));
    }

    #[test]
    fn bind_empty_key_rejected() {
        // A trailing comma or empty key field must not produce a keybind.
        let cfg = parse("bind = super, , quit");
        assert_eq!(cfg.keybinds.len(), 0);
    }

    #[test]
    fn keyboard_modifier() {
        let cfg = parse("keyboard {\n  modifier = alt\n}");
        assert_eq!(cfg.keyboard.modifier, Modifier::Alt);
    }

    #[test]
    fn windowrule_float_size_pos() {
        let cfg = parse("windowrule = float, sysmenu, size 450 286, pos 100 200");
        let r = &cfg.window_rules[0];
        assert!(r.floating);
        assert_eq!(r.app_id.as_deref(), Some("sysmenu"));
        assert_eq!(r.size, Some([450, 286]));
        assert_eq!(r.position, Some([100, 200]));
    }

    #[test]
    fn exec_entry() {
        let cfg = parse("exec_once = /usr/bin/waybar");
        assert_eq!(cfg.exec_once[0].command, "/usr/bin/waybar");
    }

    #[test]
    fn vibrance_section() {
        let cfg =
            parse("vibrance {\n  enabled = true\n  strength = 0.5\n  balance = 1.0 0.9 1.1\n}");
        assert!(cfg.vibrance.enabled);
        assert!((cfg.vibrance.strength - 0.5).abs() < 1e-6);
        assert!((cfg.vibrance.balance[1] - 0.9).abs() < 1e-6);
    }

    #[test]
    fn hex_color() {
        let cfg = parse("background_color = #0D0D14");
        assert!((cfg.background_color[0] - 0x0D as f32 / 255.0).abs() < 1e-4);
    }

    #[test]
    fn inline_comment_stripped() {
        let cfg = parse("general {\n  vsync = off  # disable vsync\n}");
        assert_eq!(cfg.vsync, VsyncMode::Off);
    }

    #[test]
    fn hex_color_not_stripped_as_comment() {
        let cfg = parse("background_color = #1E1E2E");
        assert!((cfg.background_color[0] - 0x1E as f32 / 255.0).abs() < 1e-4);
    }
}
