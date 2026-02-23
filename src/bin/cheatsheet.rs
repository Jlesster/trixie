// src/bin/cheatsheet.rs — trixie keybind cheatsheet
//
// Spawned by the compositor via a keybind, e.g. in keymaps.json:
//   {
//     "mods": ["super"],
//     "key": "slash",
//     "action": {
//       "type": "spawn",
//       "command": "kitty",
//       "args": ["--class", "trixie-cheatsheet", "--title", "cheatsheet",
//                "-e", "trixie-cheatsheet"]
//     }
//   }
//
// The matching rules.json entry:
//   {
//     "app_id": "trixie-cheatsheet",
//     "floating": true,
//     "size": [900, 520]
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
use std::io;

// ── palette ───────────────────────────────────────────────────────────────────

const FG: Color = Color::Rgb(205, 214, 244); // mocha text
const BG: Color = Color::Rgb(24, 24, 37); // mocha base
const SURFACE: Color = Color::Rgb(30, 30, 46); // mocha surface0
const OVERLAY: Color = Color::Rgb(49, 50, 68); // mocha overlay0
const SUBTEXT: Color = Color::Rgb(108, 112, 134); // mocha subtext0
const YELLOW: Color = Color::Rgb(249, 226, 175); // mocha yellow — headers
const ACCENT: Color = Color::Rgb(137, 180, 250);

// Badge background: muted surface tones so badges sit in the palette
const BADGE_QUIT: Color = Color::Rgb(88, 45, 55); // dark rose tint
const BADGE_SPAWN: Color = Color::Rgb(35, 48, 73); // dark blue tint

// Badge foreground: softened versions of the accent colours
const LABEL_QUIT: Color = Color::Rgb(243, 139, 168); // mocha red/pink
const LABEL_SPAWN: Color = Color::Rgb(137, 180, 250); // mocha blue

// Action column text colours — same hues, slightly dimmer
const ACTION_QUIT: Color = Color::Rgb(210, 110, 140);
const ACTION_SPAWN: Color = Color::Rgb(116, 160, 230);

// ── data model ────────────────────────────────────────────────────────────────

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
        "super" => "󰖳", // nerd: windows/super key
        "shift" => "󰘶", // nerd: shift
        "ctrl" => "󰘴",  // nerd: ctrl
        "alt" => "󰘵",   // nerd: alt
        other => other,
    }
}

fn key_label(k: &str) -> String {
    match k {
        "Return" => "󰌑".into(), // nerd: return/enter
        "space" => "󱁐".into(),  // nerd: spacebar
        "slash" => "/".into(),
        "BackSpace" => "󰌥".into(), // nerd: backspace
        "Tab" => "󰌒".into(),       // nerd: tab
        "Print" => "󰹑".into(),     // nerd: print screen
        "Escape" => "󱊷".into(),    // nerd: escape
        "Delete" => "󰹾".into(),    // nerd: delete
        "Insert" => "󰏒".into(),    // nerd: insert
        "Home" => "󰸟".into(),      // nerd: home
        "End" => "󰸡".into(),       // nerd: end
        "Page_Up" => "󰞕".into(),   // nerd: page up
        "Page_Down" => "󰞒".into(), // nerd: page down
        "Up" => "󰁝".into(),
        "Down" => "󰁅".into(),
        "Left" => "󰁍".into(),
        "Right" => "󰁔".into(),
        other => {
            if other.len() == 1 {
                other.to_uppercase()
            } else {
                other.into()
            }
        }
    }
}

/// Returns a nerd font glyph hinting at what the spawned command does.
fn action_icon(bin: &str, args: &[String]) -> &'static str {
    // Check args for -e / subcommand first (kitty -e nvim etc.)
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

fn build_entries(config: &Config) -> Vec<Entry> {
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
                KeyAction::CloseWindow => ("󰅗  Close focused window".into(), EntryKind::Close),
                KeyAction::Spawn { command, args } => {
                    let bin = command.rsplit('/').next().unwrap_or(command).to_string();
                    let icon = action_icon(&bin, args);
                    let label = if args.is_empty() {
                        format!("{icon}{bin}")
                    } else {
                        let first_arg = args
                            .iter()
                            .find(|a| !a.starts_with('-'))
                            .map(|s| s.as_str())
                            .unwrap_or("");
                        if first_arg.is_empty() {
                            format!("{icon}{bin}")
                        } else {
                            format!("{icon}{bin}  {first_arg}")
                        }
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
        let priority = match e.kind {
            EntryKind::Quit => 0,
            EntryKind::Close => 1,
            EntryKind::Spawn => 2,
        };
        (priority, e.action.clone())
    });

    entries
}

// ── rendering ─────────────────────────────────────────────────────────────────

fn render(f: &mut Frame, entries: &[Entry], config: &Config) {
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
    render_table(f, vchunks[1], entries);
    render_footer(f, vchunks[2]);
}

fn render_header(f: &mut Frame, area: Rect, config: &Config) {
    let title = format!(
        " trixie  ·  {} ",
        config.terminal.split_whitespace().next().unwrap_or("kitty")
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(OVERLAY))
        .style(Style::default().bg(SURFACE));

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
                "󰆍  {}",
                config.terminal.split_whitespace().next().unwrap_or("kitty")
            ),
            Style::default().fg(SUBTEXT),
        ),
    ]))
    .block(block);

    f.render_widget(text, area);
}

fn render_table(f: &mut Frame, area: Rect, entries: &[Entry]) {
    let hchunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    let mid = (entries.len() + 1) / 2;
    render_column(f, hchunks[0], &entries[..mid], true);
    render_column(f, hchunks[1], &entries[mid..], false);
}

fn render_column(f: &mut Frame, area: Rect, entries: &[Entry], left: bool) {
    let borders = if left {
        Borders::ALL
    } else {
        Borders::TOP | Borders::RIGHT | Borders::BOTTOM
    };

    let header_cells = ["  Chord", "Action"].iter().map(|h| {
        Cell::from(*h).style(
            Style::default()
                .fg(YELLOW)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        )
    });
    let header = Row::new(header_cells)
        .style(Style::default().bg(SURFACE))
        .height(1);

    let rows: Vec<Row> = entries
        .iter()
        .map(|e| {
            let (badge_bg, badge_fg, action_color) = match e.kind {
                EntryKind::Quit | EntryKind::Close => (BADGE_QUIT, LABEL_QUIT, ACTION_QUIT),
                EntryKind::Spawn => (BADGE_SPAWN, LABEL_SPAWN, ACTION_SPAWN),
            };

            let chord_parts: Vec<&str> = e.chord.split(" + ").collect();
            let mut chord_spans: Vec<Span> = Vec::new();
            for (i, part) in chord_parts.iter().enumerate() {
                if i > 0 {
                    chord_spans.push(Span::styled(" + ", Style::default().fg(OVERLAY)));
                }
                chord_spans.push(Span::styled(
                    format!(" {part} "),
                    Style::default()
                        .fg(badge_fg)
                        .bg(badge_bg)
                        .add_modifier(Modifier::BOLD),
                ));
            }

            Row::new(vec![
                Cell::from(Line::from(chord_spans)),
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
    .header(header)
    .block(
        Block::default()
            .borders(borders)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(OVERLAY))
            .style(Style::default().bg(BG)),
    )
    .highlight_style(Style::default().bg(OVERLAY))
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
    let entries = build_entries(&config);

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    loop {
        terminal.draw(|f| render(f, &entries, &config))?;

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
