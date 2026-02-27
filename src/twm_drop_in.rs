// twm_drop_in.rs — portable TWM layer extracted from the new trixie codebase.
//
// DROP-IN INSTRUCTIONS
// ────────────────────
// 1. Add this file to your working build as `src/twm_drop_in.rs`
// 2. In main.rs add:  mod twm_drop_in;
// 3. In your render path, after your existing GL context is bound, call:
//
//      let cmds = twm_drop_in::build_frame_cmds(&mut twm_state, cell_w, cell_h, vp_w, vp_h);
//      // then flush cmds through your existing UiRenderer::flush(&cmds)
//
// 4. Route keybinds through:  twm_state.dispatch(&action)
// 5. Route new_toplevel through: twm_state.open_shell_pane(app_id)
//
// DEPENDENCIES (already in your working Cargo.toml)
// ──────────────────────────────────────────────────
//   ratatui = { version = "...", default-features = false }
//
// This file has NO dependency on smithay, gl, EGL, DRM, or any compositor
// internals. It is pure CPU logic.

use std::collections::HashMap;
use std::time::Instant;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget},
};

// ── Re-export the DrawCmd type your UiRenderer already uses ───────────────────
// Your working build already has pixelui::DrawCmd. This module produces
// Vec<pixelui::DrawCmd> so nothing in the renderer changes.
pub use crate::pixelui::{
    layout::Rect as PixRect,
    style::{Color as PixColor, Style as PixStyle},
    DrawCmd,
};

// ─────────────────────────────────────────────────────────────────────────────
// SECTION 1 — Cell buffer (ratatui → pixel DrawCmds)
// ─────────────────────────────────────────────────────────────────────────────

/// One rendered terminal cell.
#[derive(Clone, Debug)]
pub struct TwmCell {
    pub ch: char,
    pub fg: [u8; 3],
    pub bg: [u8; 3],
    pub bold: bool,
    pub italic: bool,
}

impl Default for TwmCell {
    fn default() -> Self {
        Self {
            ch: ' ',
            fg: [205, 214, 244], // Catppuccin text
            bg: [30, 30, 46],    // Catppuccin base
            bold: false,
            italic: false,
        }
    }
}

/// Flat cell grid written to by ratatui, read by the DrawCmd builder.
pub struct CellBuffer {
    pub cols: u16,
    pub rows: u16,
    cells: Vec<TwmCell>,
}

impl CellBuffer {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            cols,
            rows,
            cells: vec![TwmCell::default(); cols as usize * rows as usize],
        }
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.cols = cols;
        self.rows = rows;
        self.cells
            .resize(cols as usize * rows as usize, TwmCell::default());
    }

    fn blit(&mut self, buf: &Buffer) {
        let area = buf.area;
        let cols = area.width.min(self.cols);
        let rows = area.height.min(self.rows);
        for row in 0..rows {
            for col in 0..cols {
                let c = buf.get(area.x + col, area.y + row);
                let idx = row as usize * self.cols as usize + col as usize;
                self.cells[idx] = cell_convert(c);
            }
        }
    }

    /// Build pixel-space DrawCmds from the current cell contents.
    pub fn to_draw_cmds(&self, cell_w: u32, cell_h: u32, vp_w: u32, vp_h: u32) -> Vec<DrawCmd> {
        let mut cmds = Vec::with_capacity(self.cells.len() * 2);
        for row in 0..self.rows {
            for col in 0..self.cols {
                let px = col as u32 * cell_w;
                let py = row as u32 * cell_h;
                if px >= vp_w || py >= vp_h {
                    continue;
                }
                let w = cell_w.min(vp_w - px);
                let h = cell_h.min(vp_h - py);
                let cell = &self.cells[row as usize * self.cols as usize + col as usize];

                // Background — skip pure-black transparent cells (embedded pane holes)
                let [br, bg, bb] = cell.bg;
                if !(br == 0 && bg == 0 && bb == 0 && cell.ch == ' ') {
                    cmds.push(DrawCmd::FillRect {
                        rect: PixRect::new(px, py, w, h),
                        color: PixColor(br, bg, bb, 255),
                    });
                }

                // Glyph
                if cell.ch != ' ' && cell.ch != '\0' {
                    let [fr, fg, fb] = cell.fg;
                    cmds.push(DrawCmd::Text {
                        x: px,
                        y: py,
                        text: cell.ch.to_string(),
                        style: PixStyle {
                            fg: PixColor(fr, fg, fb, 255),
                            bg: PixColor::RESET,
                            bold: cell.bold,
                            italic: cell.italic,
                        },
                        max_width: Some(cell_w),
                    });
                }
            }
        }
        cmds
    }
}

fn cell_convert(c: &ratatui::buffer::Cell) -> TwmCell {
    let mods = c.modifier;
    let mut fg = ratatui_color(c.fg, [205, 214, 244]);
    let mut bg = ratatui_color(c.bg, [30, 30, 46]);
    if mods.contains(Modifier::REVERSED) {
        std::mem::swap(&mut fg, &mut bg);
    }
    if mods.contains(Modifier::DIM) {
        fg = [fg[0] / 2, fg[1] / 2, fg[2] / 2];
    }
    TwmCell {
        ch: c.symbol().chars().next().unwrap_or(' '),
        fg,
        bg,
        bold: mods.contains(Modifier::BOLD),
        italic: mods.contains(Modifier::ITALIC),
    }
}

fn ratatui_color(c: Color, default: [u8; 3]) -> [u8; 3] {
    match c {
        Color::Rgb(r, g, b) => [r, g, b],
        Color::Reset => default,
        Color::Black => [69, 71, 90],
        Color::Red => [243, 139, 168],
        Color::Green => [166, 227, 161],
        Color::Yellow => [249, 226, 175],
        Color::Blue => [137, 180, 250],
        Color::Magenta => [203, 166, 247],
        Color::Cyan => [148, 226, 213],
        Color::White => [186, 194, 222],
        Color::DarkGray => [88, 91, 112],
        Color::Gray => [166, 173, 200],
        Color::LightRed => [243, 139, 168],
        Color::LightGreen => [166, 227, 161],
        Color::LightBlue => [137, 180, 250],
        Color::LightYellow => [249, 226, 175],
        Color::LightMagenta => [203, 166, 247],
        Color::LightCyan => [148, 226, 213],
        Color::Indexed(i) => ansi256(i),
    }
}

fn ansi256(i: u8) -> [u8; 3] {
    match i {
        0..=15 => [
            [0, 0, 0],
            [128, 0, 0],
            [0, 128, 0],
            [128, 128, 0],
            [0, 0, 128],
            [128, 0, 128],
            [0, 128, 128],
            [192, 192, 192],
            [128, 128, 128],
            [255, 0, 0],
            [0, 255, 0],
            [255, 255, 0],
            [0, 0, 255],
            [255, 0, 255],
            [0, 255, 255],
            [255, 255, 255],
        ][i as usize],
        16..=231 => {
            let v = i - 16;
            let f = |x: u8| if x == 0 { 0 } else { 55 + x * 40 };
            [f(v / 36), f((v / 6) % 6), f(v % 6)]
        }
        232..=255 => {
            let l = 8 + (i - 232) * 10;
            [l, l, l]
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// SECTION 2 — Animation helpers
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Easing {
    EaseOutCubic,
    EaseInOut,
    Linear,
}

impl Easing {
    pub fn apply(self, t: f64) -> f64 {
        match self {
            Self::EaseOutCubic => 1.0 - (1.0 - t).powi(3),
            Self::EaseInOut => t * t * (3.0 - 2.0 * t),
            Self::Linear => t,
        }
    }
}

#[derive(Clone, Copy)]
struct AnimRect {
    src: RF,
    dst: RF,
    start: Instant,
    dur_ms: f64,
    ease: Easing,
}

#[derive(Clone, Copy)]
struct RF {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

impl From<Rect> for RF {
    fn from(r: Rect) -> Self {
        Self {
            x: r.x as f64,
            y: r.y as f64,
            w: r.width as f64,
            h: r.height as f64,
        }
    }
}
impl From<RF> for Rect {
    fn from(r: RF) -> Self {
        Rect::new(
            r.x as u16,
            r.y as u16,
            r.w.max(1.0) as u16,
            r.h.max(1.0) as u16,
        )
    }
}
impl RF {
    fn lerp(self, o: Self, t: f64) -> Self {
        Self {
            x: self.x + (o.x - self.x) * t,
            y: self.y + (o.y - self.y) * t,
            w: self.w + (o.w - self.w) * t,
            h: self.h + (o.h - self.h) * t,
        }
    }
}

impl AnimRect {
    fn still(r: Rect) -> Self {
        let rf = RF::from(r);
        Self {
            src: rf,
            dst: rf,
            start: Instant::now(),
            dur_ms: 0.0,
            ease: Easing::EaseOutCubic,
        }
    }
    fn current(&self) -> Rect {
        if self.dur_ms <= 0.0 {
            return self.dst.into();
        }
        let t = (self.start.elapsed().as_secs_f64() * 1000.0 / self.dur_ms).min(1.0);
        self.src.lerp(self.dst, self.ease.apply(t)).into()
    }
    fn is_done(&self) -> bool {
        self.dur_ms <= 0.0 || self.start.elapsed().as_secs_f64() * 1000.0 >= self.dur_ms
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// SECTION 3 — Pane / Workspace / Layout
// ─────────────────────────────────────────────────────────────────────────────

pub type PaneId = u32;

static NEXT_ID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
fn new_id() -> PaneId {
    NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

#[derive(Clone, Debug)]
pub enum PaneContent {
    /// A Wayland toplevel tracked by app_id.
    Shell { title: String },
    /// An embedded surface tracked by app_id.
    Embedded { app_id: String },
    /// Nothing assigned yet.
    Empty,
}

impl PaneContent {
    fn label(&self) -> &str {
        match self {
            Self::Shell { title } => title,
            Self::Embedded { app_id } => app_id,
            Self::Empty => "empty",
        }
    }
    pub fn is_embedded(&self) -> bool {
        matches!(self, Self::Embedded { .. })
    }
}

pub struct Pane {
    pub id: PaneId,
    pub content: PaneContent,
    anim: AnimRect,
    pub fullscreen: bool,
}

impl Pane {
    fn new(content: PaneContent) -> Self {
        Self {
            id: new_id(),
            content,
            anim: AnimRect::still(Rect::default()),
            fullscreen: false,
        }
    }
    fn title_label(&self) -> String {
        match &self.content {
            PaneContent::Embedded { app_id } => format!(" {app_id} [{}] 󰖟 ", self.id),
            PaneContent::Shell { title } => format!(" {title} [{}] ◆ ", self.id),
            PaneContent::Empty => format!(" empty [{}] ", self.id),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Layout {
    Bsp,
    Columns,
    Rows,
    Monocle,
}

impl Layout {
    pub fn next(&self) -> Self {
        match self {
            Self::Bsp => Self::Columns,
            Self::Columns => Self::Rows,
            Self::Rows => Self::Monocle,
            Self::Monocle => Self::Bsp,
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            Self::Bsp => "BSP",
            Self::Columns => "Columns",
            Self::Rows => "Rows",
            Self::Monocle => "Monocle",
        }
    }
}

pub struct Workspace {
    pub panes: Vec<PaneId>,
    pub focused: Option<PaneId>,
    pub layout: Layout,
    pub main_ratio: f32,
    pub gap: u16,
}

impl Workspace {
    fn new(gap: u16) -> Self {
        Self {
            panes: vec![],
            focused: None,
            layout: Layout::Bsp,
            main_ratio: 0.5,
            gap,
        }
    }
    fn focus_idx(&self) -> Option<usize> {
        let fid = self.focused?;
        self.panes.iter().position(|&p| p == fid)
    }
    fn cycle_focus(&mut self, delta: i32) {
        if self.panes.is_empty() {
            return;
        }
        let n = self.panes.len() as i32;
        let cur = self.focus_idx().map(|i| i as i32).unwrap_or(0);
        self.focused = Some(self.panes[((cur + delta).rem_euclid(n)) as usize]);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// SECTION 4 — TwmState (the main object you keep in KittyCompositor)
// ─────────────────────────────────────────────────────────────────────────────

/// Keybind actions — map your existing input.rs actions to these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    FocusLeft,
    FocusRight,
    FocusUp,
    FocusDown,
    MoveLeft,
    MoveRight,
    Close,
    Workspace(u8),
    MoveToWorkspace(u8),
    NextLayout,
    GrowMain,
    ShrinkMain,
    NextWorkspace,
    PrevWorkspace,
    ToggleBar,
    Fullscreen,
    OpenShell(String), // spawn a new placeholder pane with this title
}

pub struct TwmState {
    pub panes: HashMap<PaneId, Pane>,
    pub workspaces: Vec<Workspace>,
    pub active_ws: usize,
    pub cols: u16,
    pub rows: u16,
    // animation settings — set from your config
    pub anim_duration_ms: f64,
    pub anim_ease: Easing,
    pub anim_enabled: bool,
    // bar
    pub bar_visible: bool,
    pub bar_height: u16, // in cells, typically 1
    bar_at_bottom: bool,
    bar_clock: String,
    // colours (set from your existing theme)
    pub active_border: [u8; 3],
    pub inactive_border: [u8; 3],
    pub active_title: [u8; 3],
    pub inactive_title: [u8; 3],
    pub pane_bg: [u8; 3],
    pub bar_bg: [u8; 3],
    pub bar_fg: [u8; 3],
    // internal
    buf: Buffer,
    cells: CellBuffer,
    dirty: bool,
}

impl TwmState {
    /// Create with sensible defaults matching Catppuccin Mocha.
    /// Call resize() when you know the real cell dimensions.
    pub fn new(cols: u16, rows: u16) -> Self {
        let mut workspaces: Vec<Workspace> = (0..9).map(|_| Workspace::new(1)).collect();

        // Seed workspace 0 with one empty shell pane so there's always
        // something to draw before any client connects.
        let root = Pane::new(PaneContent::Shell {
            title: "trixterm".into(),
        });
        let root_id = root.id;
        let mut panes = HashMap::new();
        panes.insert(root_id, root);
        workspaces[0].panes.push(root_id);
        workspaces[0].focused = Some(root_id);

        let buf = Buffer::empty(Rect::new(0, 0, cols, rows));
        let cells = CellBuffer::new(cols, rows);

        let mut s = Self {
            panes,
            workspaces,
            active_ws: 0,
            cols,
            rows,
            anim_duration_ms: 120.0,
            anim_ease: Easing::EaseOutCubic,
            anim_enabled: true,
            bar_visible: true,
            bar_height: 1,
            bar_at_bottom: true,
            bar_clock: String::new(),
            active_border: [180, 190, 254],
            inactive_border: [69, 71, 90],
            active_title: [180, 190, 254],
            inactive_title: [88, 91, 112],
            pane_bg: [17, 17, 27],
            bar_bg: [24, 24, 37],
            bar_fg: [166, 173, 200],
            buf,
            cells,
            dirty: true,
        };
        s.reflow();
        s
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.cols = cols;
        self.rows = rows;
        self.buf = Buffer::empty(Rect::new(0, 0, cols, rows));
        self.cells.resize(cols, rows);
        self.dirty = true;
        self.reflow();
    }

    pub fn animating(&self) -> bool {
        self.panes.values().any(|p| !p.anim.is_done())
    }

    // ── Pane management ───────────────────────────────────────────────────────

    /// Call from new_toplevel / open terminal keybind.
    pub fn open_shell_pane(&mut self, title: &str) {
        let p = Pane::new(PaneContent::Shell {
            title: title.to_owned(),
        });
        let id = p.id;
        self.panes.insert(id, p);
        let ws = &mut self.workspaces[self.active_ws];
        ws.panes.push(id);
        ws.focused = Some(id);
        self.dirty = true;
    }

    /// Call when an embedded app_id arrives (from new_toplevel for embedded clients).
    /// Replaces the focused empty/shell placeholder or opens a new pane.
    pub fn assign_embedded(&mut self, app_id: &str) -> PaneId {
        let focused = self.workspaces[self.active_ws].focused;
        if let Some(fid) = focused {
            if let Some(p) = self.panes.get_mut(&fid) {
                if matches!(p.content, PaneContent::Empty | PaneContent::Shell { .. }) {
                    p.content = PaneContent::Embedded {
                        app_id: app_id.to_owned(),
                    };
                    self.dirty = true;
                    return fid;
                }
            }
        }
        let p = Pane::new(PaneContent::Embedded {
            app_id: app_id.to_owned(),
        });
        let id = p.id;
        self.panes.insert(id, p);
        let ws = &mut self.workspaces[self.active_ws];
        ws.panes.push(id);
        ws.focused = Some(id);
        self.dirty = true;
        id
    }

    /// Call from toplevel_destroyed.
    pub fn close_pane_by_app_id(&mut self, app_id: &str) {
        let id = self
            .panes
            .iter()
            .find(|(_, p)| p.content.label() == app_id)
            .map(|(&id, _)| id);
        if let Some(id) = id {
            self.close_pane(id);
        }
    }

    pub fn close_pane(&mut self, id: PaneId) {
        self.panes.remove(&id);
        for ws in &mut self.workspaces {
            ws.panes.retain(|&p| p != id);
            if ws.focused == Some(id) {
                ws.focused = ws.panes.last().copied();
            }
        }
        self.dirty = true;
    }

    pub fn close_focused(&mut self) {
        if let Some(id) = self.focused_id() {
            self.close_pane(id);
        }
    }

    // ── Accessors ─────────────────────────────────────────────────────────────

    pub fn focused_id(&self) -> Option<PaneId> {
        self.workspaces[self.active_ws].focused
    }

    pub fn focused_content(&self) -> Option<&PaneContent> {
        self.focused_id()
            .and_then(|id| self.panes.get(&id))
            .map(|p| &p.content)
    }

    /// Cell rect for a given embedded app_id on the active workspace.
    pub fn embedded_cell_rect(&self, app_id: &str) -> Option<Rect> {
        let ws = &self.workspaces[self.active_ws];
        ws.panes.iter().find_map(|&id| {
            let p = self.panes.get(&id)?;
            if let PaneContent::Embedded { app_id: aid } = &p.content {
                if aid == app_id {
                    return Some(p.anim.current());
                }
            }
            None
        })
    }

    /// All embedded panes on the active workspace: (app_id, cell_rect).
    pub fn all_embedded_cell_rects(&self) -> Vec<(String, Rect)> {
        let ws = &self.workspaces[self.active_ws];
        ws.panes
            .iter()
            .filter_map(|&id| {
                let p = self.panes.get(&id)?;
                if let PaneContent::Embedded { app_id } = &p.content {
                    Some((app_id.clone(), p.anim.current()))
                } else {
                    None
                }
            })
            .collect()
    }

    // ── Action dispatch ───────────────────────────────────────────────────────

    pub fn dispatch(&mut self, action: &Action) {
        match action {
            Action::FocusLeft => self.focus_dir(-1, 0),
            Action::FocusRight => self.focus_dir(1, 0),
            Action::FocusUp => self.focus_dir(0, -1),
            Action::FocusDown => self.focus_dir(0, 1),
            Action::MoveLeft => {
                let ws = &mut self.workspaces[self.active_ws];
                ws.panes.len() >= 2 && {
                    ws_swap(ws, false);
                    true
                };
                self.dirty = true;
            }
            Action::MoveRight => {
                let ws = &mut self.workspaces[self.active_ws];
                ws.panes.len() >= 2 && {
                    ws_swap(ws, true);
                    true
                };
                self.dirty = true;
            }
            Action::Close => self.close_focused(),
            Action::Workspace(n) => {
                let idx = n.saturating_sub(1) as usize;
                if idx < self.workspaces.len() {
                    self.active_ws = idx;
                    self.dirty = true;
                }
            }
            Action::MoveToWorkspace(n) => {
                let idx = n.saturating_sub(1) as usize;
                if idx != self.active_ws && idx < self.workspaces.len() {
                    if let Some(id) = self.focused_id() {
                        self.workspaces[self.active_ws].panes.retain(|&p| p != id);
                        self.workspaces[self.active_ws].focused =
                            self.workspaces[self.active_ws].panes.last().copied();
                        self.workspaces[idx].panes.push(id);
                        self.workspaces[idx].focused = Some(id);
                        self.dirty = true;
                    }
                }
            }
            Action::NextLayout => {
                let ws = &mut self.workspaces[self.active_ws];
                ws.layout = ws.layout.next();
                self.dirty = true;
            }
            Action::GrowMain => {
                self.workspaces[self.active_ws].main_ratio =
                    (self.workspaces[self.active_ws].main_ratio + 0.05).min(0.9);
                self.dirty = true;
            }
            Action::ShrinkMain => {
                self.workspaces[self.active_ws].main_ratio =
                    (self.workspaces[self.active_ws].main_ratio - 0.05).max(0.1);
                self.dirty = true;
            }
            Action::NextWorkspace => {
                self.active_ws = (self.active_ws + 1) % self.workspaces.len();
                self.dirty = true;
            }
            Action::PrevWorkspace => {
                let n = self.workspaces.len();
                self.active_ws = (self.active_ws + n - 1) % n;
                self.dirty = true;
            }
            Action::ToggleBar => {
                self.bar_visible = !self.bar_visible;
                self.dirty = true;
            }
            Action::Fullscreen => {
                if let Some(id) = self.focused_id() {
                    if let Some(p) = self.panes.get_mut(&id) {
                        p.fullscreen = !p.fullscreen;
                    }
                }
                self.dirty = true;
            }
            Action::OpenShell(title) => self.open_shell_pane(title),
        }
    }

    // ── Main entry point called from your render path ─────────────────────────

    /// Render the TWM chrome into a ratatui Buffer, convert to DrawCmds,
    /// and return them. Pass the result directly to UiRenderer::flush(&cmds).
    ///
    /// `cell_w` / `cell_h` — pixel dimensions of one monospace cell.
    /// `vp_w`  / `vp_h`   — output pixel dimensions.
    pub fn build_frame_cmds(
        &mut self,
        cell_w: u32,
        cell_h: u32,
        vp_w: u32,
        vp_h: u32,
    ) -> Vec<DrawCmd> {
        // Tick clock
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.bar_clock = format!(
            "{:02}:{:02}:{:02}",
            (secs / 3600) % 24,
            (secs / 60) % 60,
            secs % 60
        );

        // Recompute cols/rows from the actual pixel viewport and cell size every
        // frame. Cell dimensions can change after font initialisation (e.g. after
        // the em_scale fix grows cell_h by ~32%), and the ratatui cell grid must
        // always exactly tile the viewport or content gets cut off / mispositioned.
        if cell_w > 0 && cell_h > 0 {
            let cols = (vp_w / cell_w).max(1) as u16;
            let rows = (vp_h / cell_h).max(1) as u16;
            if cols != self.cols || rows != self.rows {
                self.resize(cols, rows);
            }
        }

        if self.dirty {
            self.reflow();
        }

        let area = Rect::new(0, 0, self.cols, self.rows);

        // Snapshot everything the renderer needs — no live borrows on self.
        let snap = TwmSnapshot::from_state(self);

        // Render into a *local* buffer; self.buf is not borrowed during render.
        let mut buf = Buffer::empty(area);
        TwmRenderer { snap: &snap }.render_all(&mut buf, area);

        // Copy result into self.buf (needed by cells.blit below).
        self.buf = buf;

        self.cells.blit(&self.buf);
        self.cells.to_draw_cmds(cell_w, cell_h, vp_w, vp_h)
    }

    // ── Internal ──────────────────────────────────────────────────────────────

    fn focus_dir(&mut self, dx: i32, dy: i32) {
        let fid = match self.focused_id() {
            Some(id) => id,
            None => return,
        };
        let cur = match self.panes.get(&fid) {
            Some(p) => p.anim.current(),
            None => return,
        };
        let cx = cur.x as i32 + cur.width as i32 / 2;
        let cy = cur.y as i32 + cur.height as i32 / 2;
        let ws = &self.workspaces[self.active_ws];
        let best = ws
            .panes
            .iter()
            .filter(|&&id| id != fid)
            .filter_map(|&id| self.panes.get(&id).map(|p| (id, p.anim.current())))
            .filter(|(_, r)| {
                let rx = r.x as i32 + r.width as i32 / 2;
                let ry = r.y as i32 + r.height as i32 / 2;
                (dx > 0 && rx > cx)
                    || (dx < 0 && rx < cx)
                    || (dy > 0 && ry > cy)
                    || (dy < 0 && ry < cy)
            })
            .min_by_key(|(_, r)| {
                let rx = r.x as i32 + r.width as i32 / 2;
                let ry = r.y as i32 + r.height as i32 / 2;
                (rx - cx).abs() + (ry - cy).abs()
            })
            .map(|(id, _)| id);
        if let Some(id) = best {
            self.workspaces[self.active_ws].focused = Some(id);
            self.dirty = true;
        }
    }

    fn anim_dur(&self) -> f64 {
        if self.anim_enabled {
            self.anim_duration_ms
        } else {
            0.0
        }
    }

    fn reflow(&mut self) {
        let ws = &self.workspaces[self.active_ws];
        let ids: Vec<PaneId> = ws.panes.clone();
        if ids.is_empty() {
            self.dirty = false;
            return;
        }

        let content_area = self.content_rect();
        let gap = ws.gap;
        let rects = match ws.layout {
            Layout::Bsp => bsp_split(content_area, ids.len(), gap),
            Layout::Columns => col_split(content_area, ids.len(), ws.main_ratio, gap),
            Layout::Rows => row_split(content_area, ids.len(), ws.main_ratio, gap),
            Layout::Monocle => vec![content_area; ids.len()],
        };

        let dur = self.anim_dur();
        let ease = self.anim_ease;
        for (i, &id) in ids.iter().enumerate() {
            let dst = rects[i];
            if let Some(pane) = self.panes.get_mut(&id) {
                let cur = pane.anim.current();
                if cur != dst {
                    pane.anim = AnimRect {
                        src: RF::from(cur),
                        dst: RF::from(dst),
                        start: Instant::now(),
                        dur_ms: dur,
                        ease,
                    };
                }
            }
        }
        self.dirty = false;
    }

    fn content_rect(&self) -> Rect {
        let full = Rect::new(0, 0, self.cols, self.rows);
        if !self.bar_visible {
            return full;
        }
        if self.bar_at_bottom {
            Rect::new(0, 0, self.cols, self.rows.saturating_sub(self.bar_height))
        } else {
            Rect::new(
                0,
                self.bar_height,
                self.cols,
                self.rows.saturating_sub(self.bar_height),
            )
        }
    }

    fn bar_rect(&self) -> Rect {
        if self.bar_at_bottom {
            Rect::new(
                0,
                self.rows.saturating_sub(self.bar_height),
                self.cols,
                self.bar_height,
            )
        } else {
            Rect::new(0, 0, self.cols, self.bar_height)
        }
    }

    fn draw_into(&self, _buf: &mut Buffer, _area: Rect) {} // stub; real impl below via TwmRenderer
}

// ── Renderer (kept separate so it can borrow state immutably) ─────────────────

struct PaneSnap {
    id: PaneId,
    rect: Rect, // animated current rect
    content: PaneContent,
    fullscreen: bool,
    focused: bool,
}

struct TwmSnapshot {
    panes: Vec<PaneSnap>,
    focused_id: Option<PaneId>,
    active_ws: usize,
    layout_label: &'static str,
    bar_visible: bool,
    bar_rect: Rect,
    bar_at_bottom: bool,
    bar_clock: String,
    // colours
    active_border: [u8; 3],
    inactive_border: [u8; 3],
    active_title: [u8; 3],
    inactive_title: [u8; 3],
    pane_bg: [u8; 3],
    bar_bg: [u8; 3],
    bar_fg: [u8; 3],
    // workspace tab info: (index, occupied, active)
    ws_tabs: Vec<(usize, bool, bool)>,
    content_area: Rect,
}

impl TwmSnapshot {
    fn from_state(s: &TwmState) -> Self {
        let focused_id = s.focused_id();
        let ws = &s.workspaces[s.active_ws];

        let panes = ws
            .panes
            .iter()
            .filter_map(|&id| {
                let p = s.panes.get(&id)?;
                Some(PaneSnap {
                    id,
                    rect: p.anim.current(),
                    content: p.content.clone(),
                    fullscreen: p.fullscreen,
                    focused: Some(id) == focused_id,
                })
            })
            .collect();

        let ws_tabs = s
            .workspaces
            .iter()
            .enumerate()
            .map(|(i, w)| (i, !w.panes.is_empty(), i == s.active_ws))
            .collect();

        Self {
            panes,
            focused_id,
            active_ws: s.active_ws,
            layout_label: ws.layout.label(),
            bar_visible: s.bar_visible,
            bar_rect: s.bar_rect(),
            bar_at_bottom: s.bar_at_bottom,
            bar_clock: s.bar_clock.clone(),
            active_border: s.active_border,
            inactive_border: s.inactive_border,
            active_title: s.active_title,
            inactive_title: s.inactive_title,
            pane_bg: s.pane_bg,
            bar_bg: s.bar_bg,
            bar_fg: s.bar_fg,
            ws_tabs,
            content_area: s.content_rect(),
        }
    }
}

struct TwmRenderer<'a> {
    snap: &'a TwmSnapshot,
}

impl<'a> TwmRenderer<'a> {
    fn render_all(&self, buf: &mut Buffer, area: Rect) {
        let s = self.snap;
        let ab = ratatui_rgb(s.active_border);
        let ib = ratatui_rgb(s.inactive_border);
        let at = ratatui_rgb(s.active_title);
        let it = ratatui_rgb(s.inactive_title);
        let bg = ratatui_rgb(s.pane_bg);

        // Is any pane fullscreen?
        let fs_id = s.focused_id.filter(|&id| {
            s.panes
                .iter()
                .find(|p| p.id == id)
                .map(|p| p.fullscreen)
                .unwrap_or(false)
        });

        for pane in &s.panes {
            if let Some(fsid) = fs_id {
                if pane.id != fsid {
                    continue;
                }
            }

            let r = if fs_id.is_some() {
                s.content_area
            } else {
                let raw = pane.rect;
                let x0 = raw.x.max(area.x);
                let y0 = raw.y.max(area.y);
                let x1 = (raw.x + raw.width).min(area.x + area.width);
                let y1 = (raw.y + raw.height).min(area.y + area.height);
                if x1 <= x0 || y1 <= y0 {
                    continue;
                }
                Rect::new(x0, y0, x1 - x0, y1 - y0)
            };

            let border_style = if pane.focused {
                Style::default().fg(ab).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(ib)
            };
            let title_style = if pane.focused {
                Style::default().fg(at).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(it)
            };

            let title_text = {
                let raw = match &pane.content {
                    PaneContent::Embedded { app_id } => format!(" {app_id} [{}] 󰖟 ", pane.id),
                    PaneContent::Shell { title } => format!(" {title} [{}] ◆ ", pane.id),
                    PaneContent::Empty => format!(" empty [{}] ", pane.id),
                };
                truncate(&raw, r.width.saturating_sub(4) as usize)
            };

            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(border_style)
                .title(Span::styled(title_text, title_style))
                .style(Style::default().bg(bg));
            let inner = block.inner(r);
            block.render(r, buf);

            if inner.width > 0 && inner.height > 0 {
                match &pane.content {
                    PaneContent::Embedded { .. } => {
                        for y in inner.y..inner.y + inner.height {
                            for x in inner.x..inner.x + inner.width {
                                buf.get_mut(x, y)
                                    .set_char(' ')
                                    .set_style(Style::default().bg(Color::Reset).fg(Color::Reset));
                            }
                        }
                    }
                    PaneContent::Shell { title } => {
                        let hint = format!("  {title}  (no window)");
                        Paragraph::new(Line::from(Span::styled(hint, Style::default().fg(it))))
                            .render(inner, buf);
                    }
                    PaneContent::Empty => {}
                }
            }
        }

        if s.bar_visible {
            self.render_bar(buf);
        }
    }

    fn render_bar(&self, buf: &mut Buffer) {
        let s = self.snap;
        let area = s.bar_rect;
        if area.width == 0 || area.height == 0 {
            return;
        }

        let bar_bg = ratatui_rgb(s.bar_bg);
        let bar_fg = ratatui_rgb(s.bar_fg);
        let accent = ratatui_rgb(s.active_border);
        let dim = ratatui_rgb(s.inactive_title);

        for x in area.x..area.x + area.width {
            buf.get_mut(x, area.y)
                .set_char(' ')
                .set_style(Style::default().bg(bar_bg).fg(bar_fg));
        }

        // Left: workspace tabs
        let mut x = area.x + 1;
        for &(i, occupied, active) in &s.ws_tabs {
            if x >= area.x + area.width {
                break;
            }
            let label = format!(" {} ", i + 1);
            let style = if active {
                Style::default()
                    .fg(Color::Black)
                    .bg(accent)
                    .add_modifier(Modifier::BOLD)
            } else if occupied {
                Style::default().fg(accent).bg(bar_bg)
            } else {
                Style::default().fg(dim).bg(bar_bg)
            };
            for ch in label.chars() {
                if x >= area.x + area.width {
                    break;
                }
                buf.get_mut(x, area.y).set_char(ch).set_style(style);
                x += 1;
            }
            if x < area.x + area.width {
                buf.get_mut(x, area.y)
                    .set_char('│')
                    .set_style(Style::default().fg(dim).bg(bar_bg));
                x += 1;
            }
        }

        // Centre: layout label
        let layout_label = format!(" [{}] ", s.layout_label);
        let ll = layout_label.len() as u16;
        if ll < area.width {
            let cx = area.x + (area.width / 2).saturating_sub(ll / 2);
            for (i, ch) in layout_label.chars().enumerate() {
                let bx = cx + i as u16;
                if bx >= area.x + area.width {
                    break;
                }
                buf.get_mut(bx, area.y)
                    .set_char(ch)
                    .set_style(Style::default().fg(accent).bg(bar_bg));
            }
        }

        // Right: clock
        let right = format!(" {} ", s.bar_clock);
        let rl = right.len() as u16;
        if rl <= area.width {
            let rx = area.x + area.width.saturating_sub(rl);
            for (i, ch) in right.chars().enumerate() {
                let bx = rx + i as u16;
                if bx >= area.x + area.width {
                    break;
                }
                buf.get_mut(bx, area.y)
                    .set_char(ch)
                    .set_style(Style::default().fg(bar_fg).bg(bar_bg));
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// SECTION 5 — Public entry point
// ─────────────────────────────────────────────────────────────────────────────

/// Call this once per frame from your existing render path, after the
/// DRM FBO is bound but before render_frame(). Returns DrawCmds to pass
/// to your existing UiRenderer::flush().
///
/// Example in render_surface():
///
///   if let Some(twm) = &mut self.twm {
///       let (cw, ch) = overlay_element::cell_size();
///       let cmds = twm.build_frame_cmds(cw, ch, output_w, output_h);
///       // TwmChromeElement::new(cmds, output_w, output_h) as before
///   }
///
pub fn build_frame_cmds(
    state: &mut TwmState,
    cell_w: u32,
    cell_h: u32,
    vp_w: u32,
    vp_h: u32,
) -> Vec<DrawCmd> {
    state.build_frame_cmds(cell_w, cell_h, vp_w, vp_h)
}

// ─────────────────────────────────────────────────────────────────────────────
// SECTION 6 — Layout algorithms
// ─────────────────────────────────────────────────────────────────────────────

fn bsp_split(area: Rect, n: usize, gap: u16) -> Vec<Rect> {
    bsp_inner(area, n, gap, area.width >= area.height)
}
fn bsp_inner(area: Rect, n: usize, gap: u16, vert: bool) -> Vec<Rect> {
    if n == 0 {
        return vec![];
    }
    if n == 1 {
        return vec![area];
    }
    if vert {
        let lw = area.width.saturating_sub(gap) / 2;
        let rw = area.width.saturating_sub(lw + gap);
        let left = Rect::new(area.x, area.y, lw, area.height);
        let right = Rect::new(area.x + lw + gap, area.y, rw, area.height);
        let mut out = vec![left];
        out.extend(bsp_inner(right, n - 1, gap, false));
        out
    } else {
        let th = area.height.saturating_sub(gap) / 2;
        let bh = area.height.saturating_sub(th + gap);
        let top = Rect::new(area.x, area.y, area.width, th);
        let bot = Rect::new(area.x, area.y + th + gap, area.width, bh);
        let mut out = vec![top];
        out.extend(bsp_inner(bot, n - 1, gap, true));
        out
    }
}

fn col_split(area: Rect, n: usize, ratio: f32, gap: u16) -> Vec<Rect> {
    if n == 1 {
        return vec![area];
    }
    let mw = ((area.width as f32 * ratio) as u16).max(4);
    let sw = area.width.saturating_sub(mw + gap);
    let rest = n - 1;
    let each_h = area
        .height
        .saturating_sub(gap * (rest as u16).saturating_sub(1))
        / rest as u16;
    let mut out = vec![Rect::new(area.x, area.y, mw, area.height)];
    for i in 0..rest {
        out.push(Rect::new(
            area.x + mw + gap,
            area.y + i as u16 * (each_h + gap),
            sw,
            each_h,
        ));
    }
    out
}

fn row_split(area: Rect, n: usize, ratio: f32, gap: u16) -> Vec<Rect> {
    if n == 1 {
        return vec![area];
    }
    let mh = ((area.height as f32 * ratio) as u16).max(3);
    let sh = area.height.saturating_sub(mh + gap);
    let rest = n - 1;
    let each_w = area
        .width
        .saturating_sub(gap * (rest as u16).saturating_sub(1))
        / rest as u16;
    let mut out = vec![Rect::new(area.x, area.y, area.width, mh)];
    for i in 0..rest {
        out.push(Rect::new(
            area.x + i as u16 * (each_w + gap),
            area.y + mh + gap,
            each_w,
            sh,
        ));
    }
    out
}

// ─────────────────────────────────────────────────────────────────────────────
// SECTION 7 — Tiny helpers
// ─────────────────────────────────────────────────────────────────────────────

fn truncate(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    if max <= 3 {
        return s.chars().take(max).collect();
    }
    let mut t: String = s.chars().take(max - 3).collect();
    t.push_str("...");
    t
}

fn ratatui_rgb(c: [u8; 3]) -> Color {
    Color::Rgb(c[0], c[1], c[2])
}

fn ws_swap(ws: &mut Workspace, forward: bool) {
    let n = ws.panes.len();
    if let Some(cur) = ws.focus_idx() {
        let tgt = if forward {
            (cur + 1) % n
        } else {
            (cur + n - 1) % n
        };
        ws.panes.swap(cur, tgt);
    }
}
