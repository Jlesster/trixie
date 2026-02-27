// shaper.rs — HarfBuzz run shaping for ligature support via rustybuzz

use rustybuzz::{Face, GlyphBuffer, UnicodeBuffer};

// ── output types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ShapedGlyph {
    /// HarfBuzz / ab_glyph glyph index (NOT a Unicode codepoint).
    pub glyph_id: u32,
    /// Number of input *characters* this glyph consumes.
    /// 1 for normal glyphs, >1 for ligatures (e.g. "=>" → 2).
    pub cluster_width: usize,
}

// ── shaper ────────────────────────────────────────────────────────────────────

pub struct Shaper {
    face: Face<'static>,
}

impl Shaper {
    /// `font_data` must be `'static` — call `Box::leak(bytes.into_boxed_slice())`
    /// before passing it in. Font data lives for the whole process anyway.
    pub fn new(font_data: &'static [u8]) -> Self {
        let face = Face::from_slice(font_data, 0).expect("rustybuzz: failed to parse font face");
        Self { face }
    }

    /// Shape a single visual run (homogeneous style, no newlines).
    /// Returns one `ShapedGlyph` per output glyph — ligatures collapse
    /// multiple chars into one entry with `cluster_width > 1`.
    ///
    /// Only call this for non-synthetic runs. Synthetic characters (box drawing
    /// etc.) must be looked up by char via `GlyphAtlas::glyph()` directly.
    pub fn shape(&self, text: &str) -> Vec<ShapedGlyph> {
        if text.is_empty() {
            return vec![];
        }
        let mut buf = UnicodeBuffer::new();
        buf.push_str(text);
        // Let rustybuzz auto-detect script/language — do NOT pre-specify LATIN,
        // as that panics when the face reports a different script (e.g. Zzzz).
        let output: GlyphBuffer = rustybuzz::shape(&self.face, &[], buf);
        let positions = output.glyph_positions();
        let infos = output.glyph_infos();

        let _ = positions; // positions not used — we advance by cell_w

        let mut result = Vec::with_capacity(infos.len());
        for i in 0..infos.len() {
            let cluster_byte = infos[i].cluster as usize;
            let next_cluster_byte = infos
                .get(i + 1)
                .map(|g| g.cluster as usize)
                .unwrap_or(text.len());
            let cluster_chars = text[cluster_byte..next_cluster_byte].chars().count();
            result.push(ShapedGlyph {
                glyph_id: infos[i].glyph_id,
                cluster_width: cluster_chars.max(1),
            });
        }
        result
    }
}

// ── run segmentation ──────────────────────────────────────────────────────────

/// A contiguous slice of a terminal row with uniform style.
pub struct Run {
    pub start_col: usize,
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    /// If true, skip HarfBuzz and render each char independently
    /// (box drawing, block elements, braille, Powerline).
    pub synthetic: bool,
}

/// Split a string into shaped/synthetic runs.
///
/// `cells` is a slice of `(char, bold, italic)` triples as produced by the
/// terminal cell grid. Runs are split on changes of style OR synthetic status,
/// so box-drawing characters never get mixed into a shaped run.
pub fn segment_row(cells: &[(char, bool, bool)]) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    for (col, &(ch, bold, italic)) in cells.iter().enumerate() {
        let synthetic = is_synthetic(ch as u32);
        if let Some(last) = runs.last_mut() {
            if last.synthetic == synthetic && last.bold == bold && last.italic == italic {
                last.text.push(ch);
                continue;
            }
        }
        runs.push(Run {
            start_col: col,
            text: ch.to_string(),
            bold,
            italic,
            synthetic,
        });
    }
    runs
}

/// Segment a plain `&str` into runs, treating every character as having the
/// same bold/italic style. This is the path used by `pixelui`'s `DrawCmd::Text`
/// handler, which has no per-character style information.
pub fn segment_str(text: &str, bold: bool, italic: bool) -> Vec<Run> {
    let cells: Vec<(char, bool, bool)> = text.chars().map(|c| (c, bold, italic)).collect();
    segment_row(&cells)
}

/// Returns true for characters that must be rendered programmatically rather
/// than through the font shaping pipeline.
#[inline]
pub fn is_synthetic(cp: u32) -> bool {
    matches!(cp,
        0x2500..=0x257F |  // Box Drawing
        0x2580..=0x259F |  // Block Elements
        0x2800..=0x28FF |  // Braille Patterns
        0xE0B0 | 0xE0B1 | 0xE0B2 | 0xE0B3  // Powerline arrows
    )
}
