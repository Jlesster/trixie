// pixelui.rs — native pixel-space UI framework.

use std::ffi::CString;

pub use self::layout::{Constraint, Direction, Layout, Rect};
pub use self::style::{Color, Style};
pub use self::widgets::{Block, Input, InputState, List, ListState, Paragraph};

use crate::font::GlyphAtlas;
use crate::shaper::Shaper;

// ── Color ─────────────────────────────────────────────────────────────────────

pub mod style {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Color(pub u8, pub u8, pub u8, pub u8);

    impl Color {
        pub const BLACK: Self = Self(0, 0, 0, 255);
        pub const WHITE: Self = Self(255, 255, 255, 255);
        pub const RESET: Self = Self(0, 0, 0, 0);
        pub const CYAN: Self = Self(0, 200, 255, 255);
        pub const GRAY: Self = Self(128, 128, 128, 255);
        pub const DARK_GRAY: Self = Self(64, 64, 64, 255);

        pub fn rgb(r: u8, g: u8, b: u8) -> Self {
            Self(r, g, b, 255)
        }
        pub fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
            Self(r, g, b, a)
        }
        pub fn to_f32(self) -> [f32; 4] {
            [
                self.0 as f32 / 255.,
                self.1 as f32 / 255.,
                self.2 as f32 / 255.,
                self.3 as f32 / 255.,
            ]
        }
    }

    impl Default for Color {
        fn default() -> Self {
            Self::RESET
        }
    }

    #[derive(Clone, Copy, Debug)]
    pub struct Style {
        pub fg: Color,
        pub bg: Color,
        pub bold: bool,
        pub italic: bool,
    }

    impl Default for Style {
        fn default() -> Self {
            Self {
                fg: Color::WHITE,
                bg: Color::RESET,
                bold: false,
                italic: false,
            }
        }
    }

    impl Style {
        pub fn fg(mut self, c: Color) -> Self {
            self.fg = c;
            self
        }
        pub fn bg(mut self, c: Color) -> Self {
            self.bg = c;
            self
        }
        pub fn bold(mut self) -> Self {
            self.bold = true;
            self
        }
        pub fn italic(mut self) -> Self {
            self.italic = true;
            self
        }
    }
}

// ── Layout ────────────────────────────────────────────────────────────────────

pub mod layout {
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct Rect {
        pub x: u32,
        pub y: u32,
        pub w: u32,
        pub h: u32,
    }

    impl Rect {
        pub fn new(x: u32, y: u32, w: u32, h: u32) -> Self {
            Self { x, y, w, h }
        }
        pub fn area(&self) -> u32 {
            self.w * self.h
        }
        pub fn is_empty(&self) -> bool {
            self.w == 0 || self.h == 0
        }
        pub fn inner(&self, px: u32) -> Self {
            let px2 = px * 2;
            Self {
                x: self.x + px,
                y: self.y + px,
                w: self.w.saturating_sub(px2),
                h: self.h.saturating_sub(px2),
            }
        }
    }

    #[derive(Clone, Copy, Debug)]
    pub enum Direction {
        Horizontal,
        Vertical,
    }

    #[derive(Clone, Copy, Debug)]
    pub enum Constraint {
        Fixed(u32),
        Min(u32),
        Max(u32),
        Percentage(u32),
    }

    pub struct Layout;

    impl Layout {
        pub fn split(area: Rect, dir: Direction, constraints: &[Constraint]) -> Vec<Rect> {
            if constraints.is_empty() {
                return vec![];
            }
            let total = match dir {
                Direction::Horizontal => area.w,
                Direction::Vertical => area.h,
            };
            let sizes = Self::resolve(total, constraints);
            let mut out = Vec::with_capacity(sizes.len());
            let mut offset = 0u32;
            for sz in sizes {
                let r = match dir {
                    Direction::Horizontal => Rect::new(area.x + offset, area.y, sz, area.h),
                    Direction::Vertical => Rect::new(area.x, area.y + offset, area.w, sz),
                };
                out.push(r);
                offset += sz;
            }
            out
        }

        pub fn split2(area: Rect, dir: Direction, c: &[Constraint; 2]) -> [Rect; 2] {
            let v = Self::split(area, dir, c);
            [
                v.get(0).copied().unwrap_or_default(),
                v.get(1).copied().unwrap_or_default(),
            ]
        }

        fn resolve(total: u32, constraints: &[Constraint]) -> Vec<u32> {
            let mut sizes = vec![0u32; constraints.len()];
            let mut remaining = total;
            let mut flex_count = 0u32;
            for (i, c) in constraints.iter().enumerate() {
                match c {
                    Constraint::Fixed(v) => {
                        let v = (*v).min(remaining);
                        sizes[i] = v;
                        remaining = remaining.saturating_sub(v);
                    }
                    Constraint::Percentage(p) => {
                        let v = (total * (*p).min(100) / 100).min(remaining);
                        sizes[i] = v;
                        remaining = remaining.saturating_sub(v);
                    }
                    Constraint::Min(_) | Constraint::Max(_) => {
                        flex_count += 1;
                    }
                }
            }
            if flex_count > 0 {
                let share = remaining / flex_count;
                for (i, c) in constraints.iter().enumerate() {
                    match c {
                        Constraint::Min(m) => sizes[i] = share.max(*m),
                        Constraint::Max(m) => sizes[i] = share.min(*m),
                        _ => {}
                    }
                }
            }
            sizes
        }
    }
}

// ── DrawCmd ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum DrawCmd {
    FillRect {
        rect: Rect,
        color: Color,
    },
    StrokeRect {
        rect: Rect,
        color: Color,
        thickness: u32,
    },
    Text {
        x: u32,
        y: u32,
        text: String,
        style: Style,
        max_width: Option<u32>,
    },
    HLine {
        x: u32,
        y: u32,
        w: u32,
        color: Color,
    },
    VLine {
        x: u32,
        y: u32,
        h: u32,
        color: Color,
    },
}

// ── DrawContext ───────────────────────────────────────────────────────────────

pub struct DrawContext {
    pub cmds: Vec<DrawCmd>,
    pub area: Rect,
    pub cell_w: u32,
    pub cell_h: u32,
}

impl DrawContext {
    pub fn area(&self) -> Rect {
        self.area
    }
    pub fn cell_size(&self) -> (u32, u32) {
        (self.cell_w, self.cell_h)
    }
    pub fn render_widget<W: Widget>(&mut self, widget: W, rect: Rect) {
        widget.render(rect, self);
    }

    pub fn fill_rect(&mut self, rect: Rect, color: Color) {
        if !rect.is_empty() {
            self.cmds.push(DrawCmd::FillRect { rect, color });
        }
    }
    pub fn stroke_rect(&mut self, rect: Rect, color: Color, thickness: u32) {
        if !rect.is_empty() {
            self.cmds.push(DrawCmd::StrokeRect {
                rect,
                color,
                thickness,
            });
        }
    }
    pub fn text(&mut self, x: u32, y: u32, s: &str, style: Style, max_w: Option<u32>) {
        if !s.is_empty() {
            self.cmds.push(DrawCmd::Text {
                x,
                y,
                text: s.to_owned(),
                style,
                max_width: max_w,
            });
        }
    }
    pub fn hline(&mut self, x: u32, y: u32, w: u32, color: Color) {
        self.cmds.push(DrawCmd::HLine { x, y, w, color });
    }
    pub fn vline(&mut self, x: u32, y: u32, h: u32, color: Color) {
        self.cmds.push(DrawCmd::VLine { x, y, h, color });
    }
}

// ── Widget ────────────────────────────────────────────────────────────────────

pub trait Widget {
    fn render(self, area: Rect, ctx: &mut DrawContext);
}

// ── Widgets ───────────────────────────────────────────────────────────────────

pub mod widgets {
    use super::*;

    #[derive(Clone, Debug)]
    pub struct Block {
        title: Option<String>,
        border_color: Color,
        bg: Color,
        title_style: Style,
        padding: u32,
    }

    impl Default for Block {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Block {
        pub fn new() -> Self {
            Self {
                title: None,
                border_color: Color::GRAY,
                bg: Color::RESET,
                title_style: Style::default().fg(Color::WHITE).bold(),
                padding: 1,
            }
        }
        pub fn title(mut self, t: impl Into<String>) -> Self {
            self.title = Some(t.into());
            self
        }
        pub fn border_color(mut self, c: Color) -> Self {
            self.border_color = c;
            self
        }
        pub fn bg(mut self, c: Color) -> Self {
            self.bg = c;
            self
        }
        pub fn title_style(mut self, s: Style) -> Self {
            self.title_style = s;
            self
        }
        pub fn inner(&self, area: Rect) -> Rect {
            area.inner(1 + self.padding)
        }
    }

    impl Widget for Block {
        fn render(self, area: Rect, ctx: &mut DrawContext) {
            if area.is_empty() {
                return;
            }
            if self.bg != Color::RESET {
                ctx.fill_rect(area, self.bg);
            }
            ctx.stroke_rect(area, self.border_color, 1);
            if let Some(ref title) = self.title {
                let tx = area.x + 2;
                let ty = area.y;
                let tw = (title.chars().count() as u32 + 2) * (ctx.cell_w / 2).max(6);
                ctx.fill_rect(Rect::new(tx.saturating_sub(1), ty, tw, 1), self.bg);
                ctx.text(
                    tx,
                    ty,
                    title,
                    self.title_style,
                    Some(area.w.saturating_sub(4)),
                );
            }
        }
    }

    #[derive(Clone, Debug)]
    pub struct Paragraph {
        lines: Vec<(String, Style)>,
        block: Option<Block>,
        wrap: bool,
    }

    impl Paragraph {
        pub fn new(text: impl Into<String>) -> Self {
            Self {
                lines: vec![(text.into(), Style::default())],
                block: None,
                wrap: true,
            }
        }
        pub fn styled(lines: Vec<(String, Style)>) -> Self {
            Self {
                lines,
                block: None,
                wrap: true,
            }
        }
        pub fn block(mut self, b: Block) -> Self {
            self.block = Some(b);
            self
        }
        pub fn no_wrap(mut self) -> Self {
            self.wrap = false;
            self
        }
    }

    impl Widget for Paragraph {
        fn render(self, area: Rect, ctx: &mut DrawContext) {
            let inner = if let Some(b) = self.block {
                let inner = b.inner(area);
                b.render(area, ctx);
                inner
            } else {
                area
            };
            if inner.is_empty() {
                return;
            }
            let line_h = ctx.cell_h;
            let mut y = inner.y;
            for (text, style) in &self.lines {
                if y + line_h > inner.y + inner.h {
                    break;
                }
                ctx.text(
                    inner.x,
                    y,
                    text,
                    *style,
                    if self.wrap { Some(inner.w) } else { None },
                );
                y += line_h;
            }
        }
    }

    #[derive(Debug, Default)]
    pub struct ListState {
        pub selected: Option<usize>,
        pub offset: usize,
    }

    impl ListState {
        pub fn select(&mut self, i: Option<usize>) {
            self.selected = i;
        }
        pub fn selected(&self) -> Option<usize> {
            self.selected
        }
        pub fn next(&mut self, len: usize) {
            self.selected = Some(match self.selected {
                None => 0,
                Some(i) => (i + 1).min(len.saturating_sub(1)),
            });
        }
        pub fn prev(&mut self) {
            self.selected = Some(match self.selected {
                None | Some(0) => 0,
                Some(i) => i - 1,
            });
        }
    }

    pub struct List<'a> {
        items: Vec<(String, Style)>,
        state: &'a mut ListState,
        block: Option<Block>,
        highlight_style: Style,
        highlight_sym: &'static str,
    }

    impl<'a> List<'a> {
        pub fn new(items: Vec<impl Into<String>>, state: &'a mut ListState) -> Self {
            Self {
                items: items
                    .into_iter()
                    .map(|s| (s.into(), Style::default()))
                    .collect(),
                state,
                block: None,
                highlight_style: Style::default().fg(Color::BLACK).bg(Color::CYAN).bold(),
                highlight_sym: "▶ ",
            }
        }
        pub fn styled(items: Vec<(String, Style)>, state: &'a mut ListState) -> Self {
            Self {
                items,
                state,
                block: None,
                highlight_style: Style::default().fg(Color::BLACK).bg(Color::CYAN).bold(),
                highlight_sym: "▶ ",
            }
        }
        pub fn block(mut self, b: Block) -> Self {
            self.block = Some(b);
            self
        }
        pub fn highlight_style(mut self, s: Style) -> Self {
            self.highlight_style = s;
            self
        }
    }

    impl<'a> Widget for List<'a> {
        fn render(self, area: Rect, ctx: &mut DrawContext) {
            let inner = if let Some(b) = self.block {
                let inner = b.inner(area);
                b.render(area, ctx);
                inner
            } else {
                area
            };
            if inner.is_empty() {
                return;
            }
            let line_h = ctx.cell_h;
            let visible = (inner.h / line_h) as usize;
            if let Some(sel) = self.state.selected {
                if sel < self.state.offset {
                    self.state.offset = sel;
                }
                if sel >= self.state.offset + visible {
                    self.state.offset = sel.saturating_sub(visible - 1);
                }
            }
            let sym_w = (self.highlight_sym.chars().count() as u32) * (ctx.cell_w / 2).max(6);
            for (vi, i) in (self.state.offset..).take(visible).enumerate() {
                let Some((text, style)) = self.items.get(i) else {
                    break;
                };
                let y = inner.y + vi as u32 * line_h;
                let sel = self.state.selected == Some(i);
                let (bg, fg, sym) = if sel {
                    (
                        self.highlight_style.bg,
                        self.highlight_style.fg,
                        self.highlight_sym,
                    )
                } else {
                    (Color::RESET, style.fg, "  ")
                };
                if bg != Color::RESET {
                    ctx.fill_rect(Rect::new(inner.x, y, inner.w, line_h), bg);
                }
                ctx.text(inner.x, y, sym, Style::default().fg(fg).bg(bg), Some(sym_w));
                ctx.text(
                    inner.x + sym_w,
                    y,
                    text,
                    Style::default().fg(fg).bg(bg).bold(),
                    Some(inner.w.saturating_sub(sym_w)),
                );
            }
            if self.items.len() > visible {
                let total = self.items.len() as u32;
                let bar_h = ((visible as u32 * inner.h) / total).max(2);
                let bar_y = inner.y + (self.state.offset as u32 * inner.h) / total;
                ctx.fill_rect(
                    Rect::new(inner.x + inner.w.saturating_sub(2), bar_y, 2, bar_h),
                    Color::GRAY,
                );
            }
        }
    }

    #[derive(Debug, Default)]
    pub struct InputState {
        pub value: String,
        pub cursor: usize,
    }

    impl InputState {
        pub fn insert(&mut self, ch: char) {
            let byte = self.char_to_byte(self.cursor);
            self.value.insert(byte, ch);
            self.cursor += 1;
        }
        pub fn backspace(&mut self) {
            if self.cursor == 0 {
                return;
            }
            self.cursor -= 1;
            let byte = self.char_to_byte(self.cursor);
            self.value.remove(byte);
        }
        pub fn delete(&mut self) {
            if self.cursor >= self.value.chars().count() {
                return;
            }
            let byte = self.char_to_byte(self.cursor);
            self.value.remove(byte);
        }
        pub fn move_left(&mut self) {
            self.cursor = self.cursor.saturating_sub(1);
        }
        pub fn move_right(&mut self) {
            self.cursor = (self.cursor + 1).min(self.value.chars().count());
        }
        pub fn home(&mut self) {
            self.cursor = 0;
        }
        pub fn end(&mut self) {
            self.cursor = self.value.chars().count();
        }
        pub fn clear(&mut self) {
            self.value.clear();
            self.cursor = 0;
        }
        fn char_to_byte(&self, char_idx: usize) -> usize {
            self.value
                .char_indices()
                .nth(char_idx)
                .map(|(b, _)| b)
                .unwrap_or(self.value.len())
        }
    }

    pub struct Input<'a> {
        state: &'a mut InputState,
        block: Option<Block>,
        style: Style,
        cursor_color: Color,
        placeholder: Option<String>,
        focused: bool,
    }

    impl<'a> Input<'a> {
        pub fn new(state: &'a mut InputState) -> Self {
            Self {
                state,
                block: None,
                style: Style::default(),
                cursor_color: Color::CYAN,
                placeholder: None,
                focused: true,
            }
        }
        pub fn block(mut self, b: Block) -> Self {
            self.block = Some(b);
            self
        }
        pub fn style(mut self, s: Style) -> Self {
            self.style = s;
            self
        }
        pub fn cursor_color(mut self, c: Color) -> Self {
            self.cursor_color = c;
            self
        }
        pub fn placeholder(mut self, s: impl Into<String>) -> Self {
            self.placeholder = Some(s.into());
            self
        }
        pub fn focused(mut self, f: bool) -> Self {
            self.focused = f;
            self
        }
    }

    impl<'a> Widget for Input<'a> {
        fn render(self, area: Rect, ctx: &mut DrawContext) {
            let inner = if let Some(b) = self.block {
                let inner = b.inner(area);
                b.render(area, ctx);
                inner
            } else {
                area
            };
            if inner.is_empty() {
                return;
            }
            let cw = (ctx.cell_w / 2).max(6);
            let ch = ctx.cell_h;
            let text = &self.state.value;
            let cur = self.state.cursor;
            let visible_chars = (inner.w / cw) as usize;
            let scroll = if cur >= visible_chars {
                cur - visible_chars + 1
            } else {
                0
            };
            let visible: String = text.chars().skip(scroll).take(visible_chars).collect();
            let style = if text.is_empty() && self.placeholder.is_some() {
                Style::default().fg(Color::GRAY)
            } else {
                self.style
            };
            let display = if text.is_empty() {
                self.placeholder.as_deref().unwrap_or("").to_owned()
            } else {
                visible.clone()
            };
            ctx.text(inner.x, inner.y, &display, style, Some(inner.w));
            if self.focused {
                let cur_rel = cur.saturating_sub(scroll);
                let cx = inner.x + cur_rel as u32 * cw;
                if cx + cw <= inner.x + inner.w {
                    let cur_char: String = text
                        .chars()
                        .nth(cur)
                        .map(|c| c.to_string())
                        .unwrap_or(" ".into());
                    ctx.fill_rect(Rect::new(cx, inner.y, cw, ch), self.cursor_color);
                    ctx.text(
                        cx,
                        inner.y,
                        &cur_char,
                        Style::default().fg(Color::BLACK).bg(self.cursor_color),
                        Some(cw),
                    );
                }
            }
        }
    }
}

// ── GL renderer ───────────────────────────────────────────────────────────────

const UI_BG_VERT: &str = r#"
#version 300 es
precision mediump float;
in vec2 a_pos;
in vec4 i_rect;
in vec4 i_color;
uniform vec2 u_vp;
out vec4 v_color;
void main() {
    vec2 px  = i_rect.xy + a_pos * i_rect.zw;
    vec2 ndc = (px / u_vp) * 2.0 - 1.0;
    gl_Position = vec4(ndc, 0.0, 1.0);
    v_color = i_color;
}
"#;

const UI_BG_FRAG: &str = r#"
#version 300 es
precision mediump float;
in vec4 v_color;
out vec4 fragColor;
void main() { fragColor = v_color; }
"#;

const UI_GLYPH_VERT: &str = r#"
#version 300 es
precision mediump float;
in vec2 a_pos;
in vec4 i_glyph;
in vec4 i_uv;
in vec4 i_fg;
uniform vec2 u_vp;
out vec2 v_uv;
out vec4 v_fg;
void main() {
    vec2 px  = i_glyph.xy + a_pos * i_glyph.zw;
    vec2 ndc = (px / u_vp) * 2.0 - 1.0;
    gl_Position = vec4(ndc, 0.0, 1.0);
    v_uv = mix(i_uv.xy, i_uv.zw, a_pos);
    v_fg = i_fg;
}
"#;

const UI_GLYPH_FRAG: &str = r#"
#version 300 es
precision mediump float;
uniform sampler2D u_atlas;
in vec2 v_uv;
in vec4 v_fg;
out vec4 fragColor;
void main() {
    float a = texture(u_atlas, v_uv).a;
    if (a < 0.004) discard;
    // ab_glyph produces linear (physical) coverage values in [0,1].
    // The framebuffer is sRGB (gamma ~2.2). Rendering linear coverage
    // directly into sRGB makes glyphs look thin and washed out because
    // the display darkens mid-grey values.
    //
    // To match FreeType/kitty's perceived stroke weight we convert from
    // linear coverage to an sRGB-encoded alpha:
    //   a_srgb = pow(a_linear, 1.0/2.2)  [exact sRGB EOTF approximation]
    //
    // This brightens the coverage curve so that a physically 50% covered
    // pixel appears as 50% grey on a calibrated display, matching what
    // FreeType's hinted rasteriser produces after gamma correction.
    a = pow(a, 0.454545);  // 1.0/2.2
    fragColor = vec4(v_fg.rgb, v_fg.a * a);
}
"#;

#[repr(C)]
#[derive(Clone, Copy)]
struct BgInst {
    rect: [f32; 4],
    color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct GlyphInst {
    glyph: [f32; 4],
    uv: [f32; 4],
    fg: [f32; 4],
}

#[rustfmt::skip]
const QUAD: [f32; 12] = [0.,0., 1.,0., 1.,1., 0.,0., 1.,1., 0.,1.];

pub struct UiRenderer {
    bg_prog: u32,
    bg_vao: u32,
    bg_ivbo: u32,
    bg_cap: usize,
    glyph_prog: u32,
    glyph_vao: u32,
    glyph_ivbo: u32,
    glyph_cap: usize,
    atlas_tex: u32,
    pub atlas: GlyphAtlas,
    shaper: Shaper,
    pub cell_w: u32,
    pub cell_h: u32,
    ascender: i32,
    vp_w: u32,
    vp_h: u32,
}

impl UiRenderer {
    pub fn new(atlas: GlyphAtlas, shaper: Shaper, vp_w: u32, vp_h: u32) -> Result<Self, String> {
        let cell_w = atlas.cell_w;
        let cell_h = atlas.cell_h;
        let ascender = atlas.ascender;
        let bg_prog = unsafe { compile_prog(UI_BG_VERT, UI_BG_FRAG)? };
        let glyph_prog = unsafe { compile_prog(UI_GLYPH_VERT, UI_GLYPH_FRAG)? };
        let (bg_vao, bg_ivbo) = unsafe { create_bg_vao(bg_prog, 1024) };
        let (glyph_vao, glyph_ivbo) = unsafe { create_glyph_vao(glyph_prog, 4096) };
        let atlas_tex = unsafe { upload_atlas(&atlas) };
        Ok(Self {
            bg_prog,
            bg_vao,
            bg_ivbo,
            bg_cap: 1024,
            glyph_prog,
            glyph_vao,
            glyph_ivbo,
            glyph_cap: 4096,
            atlas_tex,
            atlas,
            shaper,
            cell_w,
            cell_h,
            ascender,
            vp_w,
            vp_h,
        })
    }

    pub fn resize(&mut self, w: u32, h: u32) {
        self.vp_w = w;
        self.vp_h = h;
    }

    /// Flush DrawCmds into whatever FBO is currently bound.
    /// Never touches gl::BindFramebuffer — that is the caller's responsibility.
    pub fn flush(&mut self, cmds: &[DrawCmd]) {
        tracing::info!(
            "UiRenderer::flush: {} cmds, vp={}x{}",
            cmds.len(),
            self.vp_w,
            self.vp_h
        );
        if cmds.is_empty() {
            return;
        }

        let mut bg_cpu: Vec<BgInst> = Vec::new();
        let mut glyph_cpu: Vec<GlyphInst> = Vec::new();

        for cmd in cmds {
            match cmd {
                DrawCmd::FillRect { rect, color } => {
                    if *color == Color::RESET {
                        continue;
                    }
                    bg_cpu.push(BgInst {
                        rect: [rect.x as f32, rect.y as f32, rect.w as f32, rect.h as f32],
                        color: color.to_f32(),
                    });
                }
                DrawCmd::StrokeRect {
                    rect,
                    color,
                    thickness,
                } => {
                    let t = *thickness as f32;
                    let (x, y, w, h) = (rect.x as f32, rect.y as f32, rect.w as f32, rect.h as f32);
                    for r in &[
                        [x, y, w, t],
                        [x, y + h - t, w, t],
                        [x, y, t, h],
                        [x + w - t, y, t, h],
                    ] {
                        bg_cpu.push(BgInst {
                            rect: *r,
                            color: color.to_f32(),
                        });
                    }
                }
                DrawCmd::HLine { x, y, w, color } => {
                    bg_cpu.push(BgInst {
                        rect: [*x as f32, *y as f32, *w as f32, 1.0],
                        color: color.to_f32(),
                    });
                }
                DrawCmd::VLine { x, y, h, color } => {
                    bg_cpu.push(BgInst {
                        rect: [*x as f32, *y as f32, 1.0, *h as f32],
                        color: color.to_f32(),
                    });
                }
                DrawCmd::Text {
                    x,
                    y,
                    text,
                    style,
                    max_width,
                } => {
                    if style.bg != Color::RESET {
                        let est_w = (text.chars().count() as u32) * self.cell_w;
                        let w = max_width.map(|m| m.min(est_w)).unwrap_or(est_w);
                        bg_cpu.push(BgInst {
                            rect: [*x as f32, *y as f32, w as f32, self.cell_h as f32],
                            color: style.bg.to_f32(),
                        });
                    }
                    self.shape_text_into(*x, *y, text, style, *max_width, &mut glyph_cpu);
                }
            }
        }

        if self.atlas.dirty {
            unsafe {
                patch_atlas(self.atlas_tex, &self.atlas);
            }
            self.atlas.dirty = false;
        }

        let (vw, vh) = (self.vp_w as f32, self.vp_h as f32);

        unsafe {
            gl::Enable(gl::BLEND);
            gl::BlendFuncSeparate(
                gl::SRC_ALPHA,
                gl::ONE_MINUS_SRC_ALPHA,
                gl::ONE,
                gl::ONE_MINUS_SRC_ALPHA,
            );

            gl::UseProgram(self.bg_prog);
            gl::BindVertexArray(self.bg_vao);
            set_u2f(self.bg_prog, "u_vp", vw, vh);
            gl::BindBuffer(gl::ARRAY_BUFFER, self.bg_ivbo);
            upload_inst(&bg_cpu, &mut self.bg_cap, std::mem::size_of::<BgInst>());
            if !bg_cpu.is_empty() {
                gl::DrawArraysInstanced(gl::TRIANGLES, 0, 6, bg_cpu.len() as i32);
            }

            gl::UseProgram(self.glyph_prog);
            gl::BindVertexArray(self.glyph_vao);
            set_u2f(self.glyph_prog, "u_vp", vw, vh);
            set_u1i(self.glyph_prog, "u_atlas", 0);
            gl::BindBuffer(gl::ARRAY_BUFFER, self.glyph_ivbo);
            upload_inst(
                &glyph_cpu,
                &mut self.glyph_cap,
                std::mem::size_of::<GlyphInst>(),
            );
            gl::ActiveTexture(gl::TEXTURE0);
            gl::BindTexture(gl::TEXTURE_2D, self.atlas_tex);
            if !glyph_cpu.is_empty() {
                gl::DrawArraysInstanced(gl::TRIANGLES, 0, 6, glyph_cpu.len() as i32);
            }

            gl::BindVertexArray(0);
            gl::UseProgram(0);
        }
    }

    fn shape_text_into(
        &mut self,
        x: u32,
        y: u32,
        text: &str,
        style: &Style,
        max_w: Option<u32>,
        out: &mut Vec<GlyphInst>,
    ) {
        use crate::shaper::segment_str;

        let cell_w = self.cell_w as f32;
        let fg = style.fg.to_f32();
        let max_px = max_w.map(|m| m as f32);
        // Sub-pixel x accumulator — snap to integer only at draw time to
        // prevent rounding drift over long runs of glyphs.
        let mut px = x as f32;

        for run in segment_str(text, style.bold, style.italic) {
            if run.synthetic {
                // ── Synthetic path ─────────────────────────────────────────────
                // Each char is looked up by codepoint — render_box_char already
                // produced a pixel-perfect cell-sized bitmap in the atlas.
                for ch in run.text.chars() {
                    if let Some(max) = max_px {
                        if px - x as f32 >= max {
                            return;
                        }
                    }
                    if let Some(uv) = self.atlas.glyph(ch, false, false) {
                        if uv.width > 0 && uv.height > 0 {
                            // Synthetic glyphs have bearing_x=0, bearing_y=ascender,
                            // so they sit flush at (px, y) filling the full cell.
                            out.push(GlyphInst {
                                glyph: [px.round(), y as f32, uv.width as f32, uv.height as f32],
                                uv: [uv.uv_x, uv.uv_y, uv.uv_x + uv.uv_w, uv.uv_y + uv.uv_h],
                                fg,
                            });
                        }
                    }
                    px += cell_w;
                }
            } else {
                // ── Shaped path ────────────────────────────────────────────────
                // Run through HarfBuzz for ligatures / correct glyph selection.
                // Advance using the font's own per-glyph advance, NOT cell_w.
                // Using cell_w here causes wrong inter-glyph spacing for UI text
                // where characters have varying widths (symbols, CJK, etc.).
                let shaped = self.shaper.shape(&run.text);
                for sg in &shaped {
                    if let Some(max) = max_px {
                        if px - x as f32 >= max {
                            return;
                        }
                    }
                    if let Some(uv) = self.atlas.glyph_by_id(sg.glyph_id, run.bold, run.italic) {
                        if uv.width > 0 && uv.height > 0 {
                            out.push(GlyphInst {
                                glyph: [
                                    (px + uv.bearing_x as f32).round(),
                                    (y as f32 + (self.ascender - uv.bearing_y) as f32).round(),
                                    uv.width as f32,
                                    uv.height as f32,
                                ],
                                uv: [uv.uv_x, uv.uv_y, uv.uv_x + uv.uv_w, uv.uv_y + uv.uv_h],
                                fg,
                            });
                        }
                        // Advance by the font's actual advance × cluster width.
                        // For monospace fonts this equals cell_w. For UI symbols
                        // and variable-width characters this is the correct value.
                        px += uv.advance as f32 * sg.cluster_width as f32;
                    } else {
                        // Glyph missing from atlas — fall back to cell_w.
                        px += cell_w * sg.cluster_width as f32;
                    }
                }
            }
        }
    }
}

// ── PixelUi (thin wrapper, kept for API compat) ───────────────────────────────

pub struct PixelUi {
    pub renderer: UiRenderer,
    vp_w: u32,
    vp_h: u32,
}

impl PixelUi {
    pub fn new_overlay(
        atlas: GlyphAtlas,
        shaper: Shaper,
        vp_w: u32,
        vp_h: u32,
    ) -> Result<Self, String> {
        Ok(Self {
            renderer: UiRenderer::new(atlas, shaper, vp_w, vp_h)?,
            vp_w,
            vp_h,
        })
    }
    pub fn resize(&mut self, w: u32, h: u32) {
        self.vp_w = w;
        self.vp_h = h;
        self.renderer.resize(w, h);
    }
    pub fn cell_size(&self) -> (u32, u32) {
        (self.renderer.cell_w, self.renderer.cell_h)
    }
}

// ── Smithay render element ────────────────────────────────────────────────────

pub mod overlay_element {
    static CHROME_ID: std::sync::OnceLock<Id> = std::sync::OnceLock::new();
    use super::{DrawCmd, UiRenderer};
    use std::cell::RefCell;

    use smithay::backend::renderer::{
        element::{Element, Id, Kind, RenderElement, UnderlyingStorage},
        gles::{GlesError, GlesFrame, GlesRenderer},
        utils::{CommitCounter, DamageSet, OpaqueRegions},
    };
    use smithay::utils::{Buffer, Physical, Rectangle, Scale};

    // ── Thread-local UiRenderer ───────────────────────────────────────────────
    // GlesRenderer is strictly single-threaded (tied to one EGL context).
    // A thread_local is the correct way to make UiRenderer accessible from
    // inside RenderElement::draw() without lifetime gymnastics.

    thread_local! {
        static RENDERER: RefCell<Option<UiRenderer>> = const { RefCell::new(None) };
    }

    /// Call once after the GL context exists (inside init_pixel_ui).
    pub fn install_renderer(r: UiRenderer) {
        RENDERER.with(|cell| *cell.borrow_mut() = Some(r));
    }

    pub fn is_installed() -> bool {
        RENDERER.with(|cell| cell.borrow().is_some())
    }

    /// Read cell dimensions from the installed renderer.
    pub fn cell_size() -> (u32, u32) {
        RENDERER.with(|cell| {
            cell.borrow()
                .as_ref()
                .map(|r| (r.cell_w, r.cell_h))
                .unwrap_or((8, 16))
        })
    }

    /// Update viewport size (no GL calls, just stores two u32s).
    pub fn set_viewport(w: u32, h: u32) {
        RENDERER.with(|cell| {
            if let Some(r) = cell.borrow_mut().as_mut() {
                r.resize(w, h);
            }
        });
    }

    /// Read the physical pixel viewport dimensions previously set by set_viewport().
    /// Returns (1920, 1080) as a safe fallback if the renderer is not yet installed.
    /// Use this in render_surface() to get the same dimensions that u_vp is set to,
    /// so DrawCmd pixel coordinates and the NDC projection are always in sync.
    pub fn get_viewport() -> (u32, u32) {
        RENDERER.with(|cell| {
            cell.borrow()
                .as_ref()
                .map(|r| (r.vp_w, r.vp_h))
                .unwrap_or((1920, 1080))
        })
    }

    // ── TwmChromeElement ──────────────────────────────────────────────────────

    /// A Smithay render element that carries pre-built DrawCmds and flushes
    /// them to the GPU inside draw(), which Smithay calls with its own FBO
    /// already bound.  No FBO manipulation here — that is what was deadlocking.
    pub struct TwmChromeElement {
        id: Id,
        commit: CommitCounter,
        cmds: Vec<DrawCmd>,
        w: u32,
        h: u32,
    }

    impl TwmChromeElement {
        pub fn new(cmds: Vec<DrawCmd>, w: u32, h: u32) -> Self {
            Self {
                id: CHROME_ID.get_or_init(Id::new).clone(),
                commit: CommitCounter::default(),
                cmds,
                w,
                h,
            }
        }
    }

    impl Element for TwmChromeElement {
        fn id(&self) -> &Id {
            &self.id
        }
        fn current_commit(&self) -> CommitCounter {
            self.commit
        }
        fn src(&self) -> Rectangle<f64, Buffer> {
            Rectangle::from_loc_and_size((0., 0.), (self.w as f64, self.h as f64))
        }
        fn geometry(&self, _: Scale<f64>) -> Rectangle<i32, Physical> {
            Rectangle::from_loc_and_size((0, 0), (self.w as i32, self.h as i32))
        }
        fn damage_since(
            &self,
            scale: Scale<f64>,
            _: Option<CommitCounter>,
        ) -> DamageSet<i32, Physical> {
            DamageSet::from_slice(&[self.geometry(scale)])
        }
        fn opaque_regions(&self, _: Scale<f64>) -> OpaqueRegions<i32, Physical> {
            OpaqueRegions::default()
        }
        fn alpha(&self) -> f32 {
            1.0
        }
        fn kind(&self) -> Kind {
            Kind::Unspecified
        }
    }

    impl RenderElement<GlesRenderer> for TwmChromeElement {
        fn draw(
            &self,
            _frame: &mut GlesFrame<'_, '_>,
            _src: Rectangle<f64, Buffer>,
            _dst: Rectangle<i32, Physical>,
            _damage: &[Rectangle<i32, Physical>],
            _opaque: &[Rectangle<i32, Physical>],
        ) -> Result<(), GlesError> {
            RENDERER.with(|cell| {
                let mut borrow = cell.borrow_mut();
                if let Some(r) = borrow.as_mut() {
                    // Reset GL viewport to the full output before our instanced draw.
                    // Smithay may have scissored/viewported to a damage sub-rect.
                    r.flush(&self.cmds);
                }
            });
            Ok(())
        }

        fn underlying_storage(&self, _: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
            None
        }
    }
}

// ── GL helpers ────────────────────────────────────────────────────────────────

unsafe fn compile_prog(vert: &str, frag: &str) -> Result<u32, String> {
    let v = compile_shader(gl::VERTEX_SHADER, vert)?;
    let f = compile_shader(gl::FRAGMENT_SHADER, frag)?;
    let p = gl::CreateProgram();
    gl::AttachShader(p, v);
    gl::AttachShader(p, f);
    gl::LinkProgram(p);
    gl::DeleteShader(v);
    gl::DeleteShader(f);
    let mut ok = 0i32;
    gl::GetProgramiv(p, gl::LINK_STATUS, &mut ok);
    if ok == 0 {
        let mut len = 0i32;
        gl::GetProgramiv(p, gl::INFO_LOG_LENGTH, &mut len);
        let mut buf = vec![0u8; len as usize];
        gl::GetProgramInfoLog(p, len, std::ptr::null_mut(), buf.as_mut_ptr() as *mut _);
        gl::DeleteProgram(p);
        return Err(String::from_utf8_lossy(&buf).into_owned());
    }
    Ok(p)
}

unsafe fn compile_shader(kind: u32, src: &str) -> Result<u32, String> {
    let s = gl::CreateShader(kind);
    let c = CString::new(src).unwrap();
    gl::ShaderSource(s, 1, &c.as_ptr(), std::ptr::null());
    gl::CompileShader(s);
    let mut ok = 0i32;
    gl::GetShaderiv(s, gl::COMPILE_STATUS, &mut ok);
    if ok == 0 {
        let mut len = 0i32;
        gl::GetShaderiv(s, gl::INFO_LOG_LENGTH, &mut len);
        let mut buf = vec![0u8; len as usize];
        gl::GetShaderInfoLog(s, len, std::ptr::null_mut(), buf.as_mut_ptr() as *mut _);
        gl::DeleteShader(s);
        return Err(String::from_utf8_lossy(&buf).into_owned());
    }
    Ok(s)
}

unsafe fn create_bg_vao(prog: u32, cap: usize) -> (u32, u32) {
    let (mut vao, mut qvbo, mut ivbo) = (0u32, 0u32, 0u32);
    gl::GenVertexArrays(1, &mut vao);
    gl::GenBuffers(1, &mut qvbo);
    gl::GenBuffers(1, &mut ivbo);
    gl::BindVertexArray(vao);
    gl::BindBuffer(gl::ARRAY_BUFFER, qvbo);
    gl::BufferData(
        gl::ARRAY_BUFFER,
        (QUAD.len() * 4) as isize,
        QUAD.as_ptr() as *const _,
        gl::STATIC_DRAW,
    );
    let a = attr_loc(prog, "a_pos");
    gl::EnableVertexAttribArray(a);
    gl::VertexAttribPointer(a, 2, gl::FLOAT, gl::FALSE, 8, 0 as *const _);
    gl::BindBuffer(gl::ARRAY_BUFFER, ivbo);
    gl::BufferData(
        gl::ARRAY_BUFFER,
        (cap * std::mem::size_of::<BgInst>()) as isize,
        std::ptr::null(),
        gl::DYNAMIC_DRAW,
    );
    let s = std::mem::size_of::<BgInst>() as i32;
    inst_attr(prog, "i_rect", 4, 0, s);
    inst_attr(prog, "i_color", 4, 16, s);
    gl::BindVertexArray(0);
    (vao, ivbo)
}

unsafe fn create_glyph_vao(prog: u32, cap: usize) -> (u32, u32) {
    let (mut vao, mut qvbo, mut ivbo) = (0u32, 0u32, 0u32);
    gl::GenVertexArrays(1, &mut vao);
    gl::GenBuffers(1, &mut qvbo);
    gl::GenBuffers(1, &mut ivbo);
    gl::BindVertexArray(vao);
    gl::BindBuffer(gl::ARRAY_BUFFER, qvbo);
    gl::BufferData(
        gl::ARRAY_BUFFER,
        (QUAD.len() * 4) as isize,
        QUAD.as_ptr() as *const _,
        gl::STATIC_DRAW,
    );
    let a = attr_loc(prog, "a_pos");
    gl::EnableVertexAttribArray(a);
    gl::VertexAttribPointer(a, 2, gl::FLOAT, gl::FALSE, 8, 0 as *const _);
    gl::BindBuffer(gl::ARRAY_BUFFER, ivbo);
    gl::BufferData(
        gl::ARRAY_BUFFER,
        (cap * std::mem::size_of::<GlyphInst>()) as isize,
        std::ptr::null(),
        gl::DYNAMIC_DRAW,
    );
    let s = std::mem::size_of::<GlyphInst>() as i32;
    inst_attr(prog, "i_glyph", 4, 0, s);
    inst_attr(prog, "i_uv", 4, 16, s);
    inst_attr(prog, "i_fg", 4, 32, s);
    gl::BindVertexArray(0);
    (vao, ivbo)
}

unsafe fn upload_atlas(atlas: &GlyphAtlas) -> u32 {
    let atlas_dim = ((atlas.pixels.len() / 4) as f64).sqrt() as i32;
    let mut tex = 0u32;
    gl::GenTextures(1, &mut tex);
    gl::BindTexture(gl::TEXTURE_2D, tex);
    // NEAREST filtering: all glyph quads are positioned at integer pixel
    // coordinates (bearing offsets are .round()ed in shape_text_into), so
    // UV lookups land exactly on atlas texel centres. NEAREST gives a crisp
    // 1:1 texel fetch. LINEAR would average adjacent texels producing blur.
    gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::NEAREST as i32);
    gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::NEAREST as i32);
    gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE as i32);
    gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE as i32);
    gl::TexImage2D(
        gl::TEXTURE_2D,
        0,
        gl::RGBA8 as i32,
        atlas_dim,
        atlas_dim,
        0,
        gl::RGBA,
        gl::UNSIGNED_BYTE,
        atlas.pixels.as_ptr() as *const _,
    );
    tex
}

unsafe fn patch_atlas(tex: u32, atlas: &GlyphAtlas) {
    let atlas_dim = ((atlas.pixels.len() / 4) as f64).sqrt() as i32;
    let rows = (atlas.cursor_y as i32 + atlas.row_h as i32 + 1).min(atlas_dim);
    gl::BindTexture(gl::TEXTURE_2D, tex);
    gl::TexSubImage2D(
        gl::TEXTURE_2D,
        0,
        0,
        0,
        atlas_dim,
        rows,
        gl::RGBA,
        gl::UNSIGNED_BYTE,
        atlas.pixels.as_ptr() as *const _,
    );
}

unsafe fn upload_inst<T: Copy>(data: &[T], cap: &mut usize, item_sz: usize) {
    let byte_len = (data.len() * item_sz) as isize;
    if data.len() > *cap {
        let new_cap = (data.len() * 2).max(64);
        gl::BufferData(
            gl::ARRAY_BUFFER,
            (new_cap * item_sz) as isize,
            std::ptr::null(),
            gl::DYNAMIC_DRAW,
        );
        *cap = new_cap;
    }
    if !data.is_empty() {
        gl::BufferSubData(gl::ARRAY_BUFFER, 0, byte_len, data.as_ptr() as *const _);
    }
}

fn attr_loc(prog: u32, name: &str) -> u32 {
    let c = CString::new(name).unwrap();
    unsafe { gl::GetAttribLocation(prog, c.as_ptr()) as u32 }
}

unsafe fn inst_attr(prog: u32, name: &str, size: i32, offset: i32, stride: i32) {
    let loc = attr_loc(prog, name);
    gl::EnableVertexAttribArray(loc);
    gl::VertexAttribPointer(loc, size, gl::FLOAT, gl::FALSE, stride, offset as *const _);
    gl::VertexAttribDivisor(loc, 1);
}

unsafe fn set_u2f(prog: u32, name: &str, x: f32, y: f32) {
    let c = CString::new(name).unwrap();
    gl::Uniform2f(gl::GetUniformLocation(prog, c.as_ptr()), x, y);
}

unsafe fn set_u1i(prog: u32, name: &str, v: i32) {
    let c = CString::new(name).unwrap();
    gl::Uniform1i(gl::GetUniformLocation(prog, c.as_ptr()), v);
}
