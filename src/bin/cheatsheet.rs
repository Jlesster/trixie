// src/bin/cheatsheet.rs — trixie keybind cheatsheet
//
// Spawned by the compositor via a keybind, e.g. in keymaps.json:
//   {
//     "mods": ["super"],
//     "key": "c",
//     "action": {
//       "type": "spawn",
//       "command": "kitty",
//       "args": ["--class", "cheatsheet", "--title", "cheatsheet", "-e", "cheatsheet"]
//     }
//   }
//
// Dismiss with q, Escape, or Enter.

use trixie::config::{Config, KeyAction};

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Cell, Paragraph, Row, Table},
    Frame, Terminal,
};
use std::{io, path::PathBuf};

// ── palette ───────────────────────────────────────────────────────────────────

const FG: Color = Color::Rgb(205, 214, 244);
const BG: Color = Color::Rgb(24, 24, 37);
const SURFACE: Color = Color::Rgb(30, 30, 46);
const OVERLAY: Color = Color::Rgb(49, 50, 68);
const SUBTEXT: Color = Color::Rgb(108, 112, 134);
const YELLOW: Color = Color::Rgb(249, 226, 175);

const BADGE_QUIT: Color = Color::Rgb(88, 45, 55);
const BADGE_SPAWN: Color = Color::Rgb(35, 48, 73);
const BADGE_KITTY: Color = Color::Rgb(35, 65, 50);

const LABEL_QUIT: Color = Color::Rgb(243, 139, 168);
const LABEL_SPAWN: Color = Color::Rgb(137, 180, 250);
const LABEL_KITTY: Color = Color::Rgb(166, 227, 161);

const ACTION_QUIT: Color = Color::Rgb(210, 110, 140);
const ACTION_SPAWN: Color = Color::Rgb(116, 160, 230);
const ACTION_KITTY: Color = Color::Rgb(140, 200, 140);

// ── compositor entries ────────────────────────────────────────────────────────

#[derive(Debug)]
struct Entry {
    chord: String,
    action: String,
    kind: EntryKind,
}

#[derive(Debug, PartialEq)]
enum EntryKind {
    Quit,
    Close,
    Spawn,
}

fn mod_label(m: &str) -> &str {
    match m {
        "super" => " ",
        "shift" => "󰘶 ",
        "ctrl" => "󰘴",
        "alt" => "󰘵 ",
        other => other,
    }
}

fn key_label(k: &str) -> String {
    match k {
        "return" => "󰌑 ".into(),
        "space" => "󱁐 ".into(),
        "slash" => "/".into(),
        "backspace" => "󰌥 ".into(),
        "tab" => "󰌒 ".into(),
        "print" => "󰹑 ".into(),
        "escape" => "󱊷 ".into(),
        "delete" => "󰹾 ".into(),
        "insert" => "󰏒 ".into(),
        "home" => "󰸟 ".into(),
        "end" => "󰸡 ".into(),
        "page_up" => "󰞕 ".into(),
        "page_down" => "󰞒 ".into(),
        "up" => "󰁝".into(),
        "down" => "󰁅".into(),
        "left" => "󰁍".into(),
        "right" => "󰁔".into(),
        other => {
            if other.len() == 1 {
                other.to_uppercase()
            } else {
                other.into()
            }
        }
    }
}

fn action_icon(bin: &str, args: &[String]) -> &'static str {
    let subcmd = args
        .iter()
        .skip_while(|a| a.starts_with('-'))
        .find(|a| !a.starts_with('-'))
        .map(|s| s.as_str())
        .unwrap_or("");
    match subcmd {
        "nvim" | "vim" | "vi" => "󰕷 ",
        "yazi" | "ranger" | "lf" => "󰉋 ",
        "lazygit" | "gitui" => "󰊢 ",
        "btop" | "htop" | "top" => "󰈏 ",
        "sysmenu" => "󰒓 ",
        _ => match bin {
            "kitty" | "foot" | "alacritty" | "wezterm" => "󰆍 ",
            "fuzzel" | "wofi" | "rofi" | "dmenu" => "󰍉 ",
            "dolphin" | "thunar" | "nautilus" => "󰉋 ",
            "grimblast" | "grim" | "scrot" => "󰹑 ",
            "tlock" | "swaylock" | "hyprlock" => "󰌾 ",
            "playerctl" => "󰎆 ",
            "wpctl" | "pamixer" | "pactl" => "󰕾 ",
            "brightnessctl" => "󰃟 ",
            "sh" | "bash" | "zsh" | "fish" => "󰆍 ",
            _ => "󰑮 ",
        },
    }
}

fn build_compositor_entries(config: &Config) -> Vec<Entry> {
    let mut entries: Vec<Entry> = config
        .keybinds
        .iter()
        .map(|bind| {
            let mut parts: Vec<String> =
                bind.mods.iter().map(|m| mod_label(m).to_string()).collect();
            parts.push(key_label(&bind.key));
            let chord = parts.join(" + ");

            let (action, kind) = match &bind.action {
                KeyAction::Quit => ("󰩈  Quit compositor".into(), EntryKind::Quit),
                KeyAction::CloseWindow => ("󰅗  Close window".into(), EntryKind::Close),
                KeyAction::ReloadConfig => ("󰑓  Reload config".into(), EntryKind::Close),
                KeyAction::Spawn { command, args } => {
                    let bin = command.rsplit('/').next().unwrap_or(command).to_string();
                    let icon = action_icon(&bin, args);
                    let first_arg = args
                        .iter()
                        .find(|a| !a.starts_with('-'))
                        .map(|s| s.as_str())
                        .unwrap_or("");
                    let label = if first_arg.is_empty() {
                        format!("{icon}{bin}")
                    } else {
                        format!("{icon}{bin}  {first_arg}")
                    };
                    (label, EntryKind::Spawn)
                }
            };
            Entry {
                chord,
                action,
                kind,
            }
        })
        .collect();

    entries.sort_by_key(|e| {
        let p = match e.kind {
            EntryKind::Quit => 0,
            EntryKind::Close => 1,
            EntryKind::Spawn => 2,
        };
        (p, e.action.clone())
    });
    entries
}

// ── kitty entries ─────────────────────────────────────────────────────────────

#[derive(Debug)]
struct KittyEntry {
    chord: String,
    action: String,
}

/// Known kitty action names → (icon, readable label).
fn kitty_action_label(action: &str) -> String {
    // Strip leading `kitten ` for display
    let action = action.strip_prefix("kitten ").unwrap_or(action);
    match action {
        "copy_to_clipboard" => "󰆏  Copy".into(),
        "paste_from_clipboard" => "󰆒  Paste".into(),
        "paste_from_selection" => "󰆒  Paste selection".into(),
        "new_window" => "󰓏  New window".into(),
        "new_window_with_cwd" => "󰓏  New window (cwd)".into(),
        "new_tab" => "󰐱  New tab".into(),
        "new_tab_with_cwd" => "󰐱  New tab (cwd)".into(),
        "close_window" => "󰅗  Close window".into(),
        "close_tab" => "󰅙  Close tab".into(),
        "next_window" => "󰒭  Next window".into(),
        "previous_window" => "󰒮  Prev window".into(),
        "next_tab" => "󰒭  Next tab".into(),
        "previous_tab" => "󰒮  Prev tab".into(),
        "move_tab_forward" => "󰒻  Move tab →".into(),
        "move_tab_backward" => "󰒺  Move tab ←".into(),
        "set_tab_title" => "󰑇  Set tab title".into(),
        "scroll_up" => "󰁝  Scroll up".into(),
        "scroll_down" => "󰁅  Scroll down".into(),
        "scroll_page_up" => "󰞕  Page up".into(),
        "scroll_page_down" => "󰞒  Page down".into(),
        "scroll_home" => "󰸟  Scroll home".into(),
        "scroll_end" => "󰸡  Scroll end".into(),
        "scroll_to_prompt -1" => "󰫍  Prev prompt".into(),
        "scroll_to_prompt 1" => "󰫎  Next prompt".into(),
        "show_scrollback" => "󰋚  Scrollback".into(),
        "show_last_command_output" => "󰋚  Last output".into(),
        "increase_font_size" => "󰐾  Font +".into(),
        "decrease_font_size" => "󰐿  Font −".into(),
        "restore_font_size" => "󰐽  Font reset".into(),
        "toggle_fullscreen" => "󰊓  Fullscreen".into(),
        "toggle_maximized" => "󱟿  Maximise".into(),
        "input_unicode_character" => "󰊿  Unicode input".into(),
        "edit_config_file" => "󰏗  Edit config".into(),
        "load_config_file" => "󰑓  Reload config".into(),
        "open_url_with_hints" => "󰌸  Open URL".into(),
        "hints" => "󰌸  Hints".into(),
        "unicode_input" => "󰊿  Unicode".into(),
        "remote_control" => "󰑓  Remote control".into(),
        "launch" => "󰑮  Launch".into(),
        "send_text" => "󰌌  Send text".into(),
        "combine" => "󰘔  Combine".into(),
        "clear_terminal" => "󰃿  Clear".into(),
        "clear_terminal reset active" => "󰃿  Hard clear".into(),
        "select_all" => "󰒉  Select all".into(),
        other => format!("󰆍  {other}"),
    }
}

/// Parse `~/.config/trixie/kitty/kitty.conf` (falls back to `~/.config/kitty/kitty.conf`).
/// Returns an empty Vec if neither file exists or can be read.
fn build_kitty_entries() -> Vec<KittyEntry> {
    let home = std::env::var("HOME").unwrap_or_default();

    let candidates = [
        PathBuf::from(&home).join(".config/trixie/kitty/kitty.conf"),
        PathBuf::from(&home).join(".config/kitty/kitty.conf"),
    ];

    let text = candidates
        .iter()
        .find_map(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();

    let mut entries = Vec::new();

    for line in text.lines() {
        let line = line.trim();
        // Skip comments and blank lines.
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // `map <chord> <action…>`
        let mut tokens = line.splitn(3, char::is_whitespace);
        if tokens.next() != Some("map") {
            continue;
        }
        let Some(chord_raw) = tokens.next() else {
            continue;
        };
        let Some(action_raw) = tokens.next() else {
            continue;
        };
        let action_raw = action_raw.trim();
        if action_raw.is_empty() {
            continue;
        }

        // Parse chord — kitty uses `+` as separator: `ctrl+shift+c`
        let parts: Vec<&str> = chord_raw.split('+').collect();
        let (mods, key) = parts.split_last().unwrap_or((&"", &[]));
        let chord = {
            let mut spans: Vec<String> = key
                .iter()
                .map(|m| match m.to_lowercase().as_str() {
                    "ctrl" => "󰘴".into(),
                    "shift" => "󰘶".into(),
                    "alt" => "󰘵".into(),
                    "super" => "󰖳".into(),
                    other => other.to_string(),
                })
                .collect();
            spans.push(key_label(&mods.to_lowercase()));
            spans.join(" + ")
        };

        entries.push(KittyEntry {
            chord,
            action: kitty_action_label(action_raw),
        });
    }

    entries
}

// ── rendering ─────────────────────────────────────────────────────────────────

fn render(f: &mut Frame, comp: &[Entry], kitty: &[KittyEntry], config: &Config) {
    f.render_widget(Block::default().style(Style::default().bg(BG)), f.size());

    let vchunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(3),
        ])
        .split(f.size());

    render_header(f, vchunks[0], config);

    // Three columns: compositor left, compositor right, kitty.
    let hchunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(33),
            Constraint::Percentage(33),
            Constraint::Percentage(34),
        ])
        .split(vchunks[1]);

    let mid = (comp.len() + 1) / 2;
    render_compositor_column(f, hchunks[0], &comp[..mid], true, "compositor");
    render_compositor_column(f, hchunks[1], &comp[mid..], false, "");
    render_kitty_column(f, hchunks[2], kitty);

    render_footer(f, vchunks[2]);
}

fn render_header(f: &mut Frame, area: Rect, config: &Config) {
    let text = Paragraph::new(Line::from(vec![
        Span::styled(
            " 󰖳  trixie",
            Style::default()
                .fg(LABEL_SPAWN)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ·  ", Style::default().fg(SUBTEXT)),
        Span::styled(
            "󰌌  Keybindings",
            Style::default().fg(FG).add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ·  ", Style::default().fg(SUBTEXT)),
        Span::styled(
            format!(
                "󰄛  {}",
                config.terminal.split_whitespace().next().unwrap_or("kitty")
            ),
            Style::default().fg(LABEL_KITTY),
        ),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(OVERLAY))
            .style(Style::default().bg(SURFACE)),
    );
    f.render_widget(text, area);
}

fn chord_spans<'a>(chord: &'a str, badge_bg: Color, badge_fg: Color) -> Line<'a> {
    let parts: Vec<&str> = chord.split(" + ").collect();
    let mut spans = Vec::new();
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" + ", Style::default().fg(OVERLAY)));
        }
        spans.push(Span::styled(
            format!(" {part} "),
            Style::default()
                .fg(badge_fg)
                .bg(badge_bg)
                .add_modifier(Modifier::BOLD),
        ));
    }
    Line::from(spans)
}

fn header_row(label: &'static str) -> Row<'static> {
    let cells = [format!("  {label}"), "Action".into()].map(|h| {
        Cell::from(h).style(
            Style::default()
                .fg(YELLOW)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        )
    });
    Row::new(cells)
        .style(Style::default().bg(SURFACE))
        .height(1)
}

fn render_compositor_column(
    f: &mut Frame,
    area: Rect,
    entries: &[Entry],
    left: bool,
    title: &'static str,
) {
    let borders = if left {
        Borders::ALL
    } else {
        Borders::TOP | Borders::RIGHT | Borders::BOTTOM
    };

    let rows: Vec<Row> = entries
        .iter()
        .map(|e| {
            let (badge_bg, badge_fg, action_color) = match e.kind {
                EntryKind::Quit | EntryKind::Close => (BADGE_QUIT, LABEL_QUIT, ACTION_QUIT),
                EntryKind::Spawn => (BADGE_SPAWN, LABEL_SPAWN, ACTION_SPAWN),
            };
            Row::new(vec![
                Cell::from(chord_spans(&e.chord, badge_bg, badge_fg)),
                Cell::from(Line::from(Span::styled(
                    &e.action,
                    Style::default().fg(action_color),
                ))),
            ])
            .height(1)
            .style(Style::default().bg(BG))
        })
        .collect();

    let table = Table::new(
        rows,
        [Constraint::Percentage(45), Constraint::Percentage(55)],
    )
    .header(header_row(title))
    .block(
        Block::default()
            .borders(borders)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(OVERLAY))
            .style(Style::default().bg(BG)),
    )
    .column_spacing(1);
    f.render_widget(table, area);
}

fn render_kitty_column(f: &mut Frame, area: Rect, entries: &[KittyEntry]) {
    let rows: Vec<Row> = entries
        .iter()
        .map(|e| {
            Row::new(vec![
                Cell::from(chord_spans(&e.chord, BADGE_KITTY, LABEL_KITTY)),
                Cell::from(Line::from(Span::styled(
                    &e.action,
                    Style::default().fg(ACTION_KITTY),
                ))),
            ])
            .height(1)
            .style(Style::default().bg(BG))
        })
        .collect();

    let table = Table::new(
        rows,
        [Constraint::Percentage(45), Constraint::Percentage(55)],
    )
    .header(header_row("kitty"))
    .block(
        Block::default()
            .borders(Borders::TOP | Borders::RIGHT | Borders::BOTTOM)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(OVERLAY))
            .style(Style::default().bg(BG)),
    )
    .column_spacing(1);
    f.render_widget(table, area);
}

fn render_footer(f: &mut Frame, area: Rect) {
    let text = Paragraph::new(Line::from(vec![
        Span::styled("  ", Style::default()),
        Span::styled(
            " q ",
            Style::default()
                .fg(BG)
                .bg(SUBTEXT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ", Style::default().fg(SUBTEXT)),
        Span::styled(
            " 󱊷 ",
            Style::default()
                .fg(BG)
                .bg(SUBTEXT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  dismiss    ", Style::default().fg(SUBTEXT)),
        Span::styled(
            " 󰌑 ",
            Style::default()
                .fg(BG)
                .bg(SUBTEXT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  confirm", Style::default().fg(SUBTEXT)),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(OVERLAY))
            .style(Style::default().bg(SURFACE)),
    );
    f.render_widget(text, area);
}

// ── main ──────────────────────────────────────────────────────────────────────

fn main() -> io::Result<()> {
    let config = Config::load();
    let comp_entries = build_compositor_entries(&config);
    let kitty_entries = build_kitty_entries();

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    loop {
        terminal.draw(|f| render(f, &comp_entries, &kitty_entries, &config))?;

        if event::poll(std::time::Duration::from_millis(200))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc | KeyCode::Enter => break,
                        _ => {}
                    }
                }
            }
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    Ok(())
}
