// font.rs — glyph rasterisation and atlas packing via ab_glyph
//
// Synthetic characters (box drawing, block elements, braille, Powerline) are
// rendered pixel-perfectly via box_drawing::render_box_char() rather than going
// through the font outline path. Everything else uses ab_glyph.

use ab_glyph::{Font, FontRef, GlyphId, PxScale, ScaleFont};
use std::collections::HashMap;

use crate::box_drawing::render_box_char;
use crate::shaper::is_synthetic;

pub const ATLAS_SIZE: u32 = 2048;
// 2px gap prevents LINEAR filter bleed between adjacent glyph bitmaps.
const GAP: u32 = 2;

#[derive(Debug, Clone, Copy)]
pub struct GlyphInfo {
    pub uv_x: f32,
    pub uv_y: f32,
    pub uv_w: f32,
    pub uv_h: f32,
    pub width: i32,
    pub height: i32,
    pub bearing_x: i32,
    pub bearing_y: i32,
    pub advance: i32,
}

// Cache key for char-based lookups (regular path + synthetic).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct GlyphKey {
    ch: char,
    bold: bool,
    italic: bool,
}

// Cache key for glyph-id-based lookups (shaped/ligature path).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct GlyphIdKey {
    id: u32,
    bold: bool,
    italic: bool,
}

// Owned font bytes + parsed ab_glyph handle together.
// We use a raw pointer trick so FontRef's lifetime is tied to the Vec
// inside the same struct, which is never moved after creation.
struct OwnedFont {
    _bytes: Vec<u8>,
    // FontRef<'static>: safe because _bytes is pinned inside this struct and
    // we never expose the 'static reference outside this module.
    font: FontRef<'static>,
    scale: PxScale,
}

impl OwnedFont {
    fn new(data: Vec<u8>, scale: PxScale) -> Result<Self, String> {
        let font_ref: FontRef<'static> = unsafe {
            let slice: &[u8] = &data;
            let extended: &'static [u8] = &*(slice as *const [u8]);
            FontRef::try_from_slice(extended).map_err(|e| format!("ab_glyph parse error: {e}"))?
        };
        Ok(Self {
            _bytes: data,
            font: font_ref,
            scale,
        })
    }

    fn scaled(&self) -> ab_glyph::PxScaleFont<&FontRef<'static>> {
        self.font.as_scaled(self.scale)
    }
}

pub struct GlyphAtlas {
    regular: OwnedFont,
    bold: Option<OwnedFont>,
    italic: Option<OwnedFont>,
    pub size_px: f32,
    cache: HashMap<GlyphKey, Option<GlyphInfo>>,
    id_cache: HashMap<GlyphIdKey, Option<GlyphInfo>>,
    pub pixels: Vec<u8>,
    pub cursor_x: u32,
    pub cursor_y: u32,
    pub row_h: u32,
    pub cell_w: u32,
    pub cell_h: u32,
    pub ascender: i32,
    pub dirty: bool,
}

impl GlyphAtlas {
    pub fn new(
        font_data: &[u8],
        bold_data: Option<&[u8]>,
        italic_data: Option<&[u8]>,
        size_px: f32,
        line_spacing: f32,
        dpi: u32,
    ) -> Result<Self, String> {
        // ── PxScale vs FreeType pixel size ────────────────────────────────────
        //
        // ab_glyph PxScale(n) sets:  ascent + |descent| = n  (in pixels)
        //   where height_unscaled = ascent_unscaled - descent_unscaled
        //
        // FreeType set_pixel_sizes(0, n) / kitty font_size (in px) sets:
        //   em_square = n  (i.e. UPM maps to n pixels)
        //
        // These are DIFFERENT. For JetBrainsMono (ascent=1020, descent=300,
        // UPM=1000): height_unscaled=1320, so PxScale(13) produces glyphs
        // that are 13px tall while FreeType at 13px produces 17.2px tall glyphs.
        // That 32% size difference makes the glyphs blurry when rendered into
        // a cell sized for em-based metrics.
        //
        // Fix: scale PxScale so that em_square = size_px:
        //   px_scale = size_px * height_unscaled / units_per_em
        //
        // This makes ab_glyph rasterise at the same visual size as FreeType.
        let em_scale = {
            // Parse a temporary FontRef just to read unscaled metrics.
            let tmp = FontRef::try_from_slice(font_data)
                .map_err(|e| format!("ab_glyph parse error: {e}"))?;
            let upm = tmp.units_per_em().unwrap_or(1000.0);
            let asc = tmp.ascent_unscaled();
            let dsc = tmp.descent_unscaled(); // negative
            let height_unscaled = asc - dsc; // positive total
            let ratio = height_unscaled / upm;
            tracing::info!(
                "font em_scale: UPM={upm} ascent_u={asc} descent_u={dsc} \
                 height_u={height_unscaled} ratio={ratio:.4} \
                 -> PxScale({:.3}) for {size_px}px",
                size_px * ratio
            );
            size_px * ratio
        };
        let scale = PxScale::from(em_scale);

        let regular = OwnedFont::new(font_data.to_vec(), scale)?;
        let bold = bold_data
            .map(|d| OwnedFont::new(d.to_vec(), scale))
            .transpose()
            .unwrap_or(None);
        let italic = italic_data
            .map(|d| OwnedFont::new(d.to_vec(), scale))
            .transpose()
            .unwrap_or(None);

        tracing::info!(
            "GlyphAtlas faces — regular: ok, bold: {}, italic: {}",
            bold.is_some(),
            italic.is_some()
        );

        let sf = regular.scaled();
        let ascender = sf.ascent().ceil() as i32;
        let descender = sf.descent().floor() as i32;
        let line_gap = sf.line_gap();
        let height_px = (ascender - descender) as f32 + line_gap;
        let cell_h = ((height_px * line_spacing).round() as u32).max(1);

        // ── cell_w: Kitty-compatible advance calculation ───────────────────────
        //
        // ab_glyph scales h_advance relative to (ascent + |descent|) = size_px.
        // Kitty (via FreeType) scales relative to the full line height including
        // line_gap, which is what cell_h represents after applying line_spacing.
        //
        // To match Kitty, we re-derive cell_w from the font's raw advance ratio
        // applied to cell_h rather than to size_px:
        //
        //   advance_ratio = h_advance_at_scale / size_px
        //   cell_w = round(advance_ratio * cell_h)
        //
        // This means at size 13 with JetBrainsMono:
        //   h_advance ≈ 6.24px at scale 13, ratio ≈ 0.48
        //   cell_h ≈ 17px (including line_gap * line_spacing)
        //   cell_w = round(0.48 * 17) = round(8.16) = 8  ✓ matches Kitty
        let cell_w = {
            let glyph_id = sf.font.glyph_id('0');
            let adv_at_scale = sf.h_advance(glyph_id);

            let cell_w_f = if adv_at_scale > 0.0 && size_px > 0.0 {
                // Scale advance from size_px basis up to cell_h basis
                let ratio = adv_at_scale / size_px;
                ratio * cell_h as f32
            } else {
                // Fallback: try space character
                let space_id = sf.font.glyph_id(' ');
                let space_adv = sf.h_advance(space_id);
                if space_adv > 0.0 {
                    (space_adv / size_px) * cell_h as f32
                } else {
                    // Last resort: 60% of cell_h (standard monospace ratio)
                    cell_h as f32 * 0.6
                }
            };

            tracing::info!(
                "font metrics: h_advance('0')={:.3} size_px={:.1} ratio={:.4} cell_h={} cell_w_f={:.3}",
                adv_at_scale,
                size_px,
                if size_px > 0.0 { adv_at_scale / size_px } else { 0.0 },
                cell_h,
                cell_w_f,
            );

            cell_w_f.round() as u32
        }
        .max(4);

        tracing::info!(
            "ab_glyph metrics — {size_px:.1}px: cell={cell_w}x{cell_h} baseline={ascender}"
        );

        let mut atlas = Self {
            regular,
            bold,
            italic,
            size_px,
            cache: HashMap::new(),
            id_cache: HashMap::new(),
            pixels: vec![0u8; (ATLAS_SIZE * ATLAS_SIZE * 4) as usize],
            cursor_x: 0,
            cursor_y: 0,
            row_h: 0,
            cell_w,
            cell_h,
            ascender,
            dirty: true,
        };

        // Pre-warm printable ASCII (all three variants).
        for ch in ' '..='~' {
            atlas.glyph(ch, false, false);
            atlas.glyph(ch, true, false);
            atlas.glyph(ch, false, true);
        }
        // Pre-warm synthetic ranges — these go through render_box_char, not the font.
        for cp in 0x2500u32..=0x259F {
            if let Some(ch) = char::from_u32(cp) {
                atlas.glyph(ch, false, false);
            }
        }
        for cp in 0x2800u32..=0x28FF {
            if let Some(ch) = char::from_u32(cp) {
                atlas.glyph(ch, false, false);
            }
        }
        for cp in [0xE0B0u32, 0xE0B1, 0xE0B2, 0xE0B3] {
            if let Some(ch) = char::from_u32(cp) {
                atlas.glyph(ch, false, false);
            }
        }

        Ok(atlas)
    }

    // ── char-based lookup ──────────────────────────────────────────────────────

    pub fn glyph(&mut self, ch: char, bold: bool, italic: bool) -> Option<GlyphInfo> {
        let key = GlyphKey { ch, bold, italic };
        if let Some(&cached) = self.cache.get(&key) {
            return cached;
        }
        let info = self.rasterise_char(ch, bold, italic);
        self.cache.insert(key, info);
        info
    }

    fn rasterise_char(&mut self, ch: char, bold: bool, italic: bool) -> Option<GlyphInfo> {
        // ── Synthetic fast-path ────────────────────────────────────────────────
        // Box drawing, block elements, braille, and Powerline glyphs are rendered
        // pixel-perfectly at exact cell size. Never go through the font for these.
        if is_synthetic(ch as u32) {
            if let Some(bitmap) = render_box_char(ch, self.cell_w, self.cell_h) {
                // Synthetic glyphs fill the whole cell: bearing_x=0, bearing_y=ascender,
                // advance=cell_w so they tile without gaps.
                return self.blit_bitmap(
                    bitmap,
                    self.cell_w,
                    self.cell_h,
                    0,                  // bearing_x: flush left
                    self.ascender,      // bearing_y: position at top of cell
                    self.cell_w as i32, // advance: exactly one cell
                    "synthetic",
                );
            }
            // render_box_char returned None for an unrecognised synthetic codepoint —
            // fall through to the font path as a last resort.
        }

        // ── Font outline path ──────────────────────────────────────────────────
        // Pick font pointer — SAFETY: always points into self which outlives this call.
        let font_ptr: *const OwnedFont = if bold && self.bold.is_some() {
            self.bold.as_ref().unwrap() as *const _
        } else if italic && self.italic.is_some() {
            self.italic.as_ref().unwrap() as *const _
        } else {
            &self.regular as *const _
        };

        let glyph_id = unsafe { (*font_ptr).scaled().font.glyph_id(ch) };

        // If variant font lacks the glyph, fall back to regular.
        let final_ptr: *const OwnedFont = if glyph_id == GlyphId(0) && (bold || italic) {
            &self.regular as *const _
        } else {
            font_ptr
        };

        let final_id = if final_ptr != font_ptr {
            unsafe { (*final_ptr).scaled().font.glyph_id(ch) }
        } else {
            glyph_id
        };

        self.rasterise_glyph_from_ptr(final_id, final_ptr)
    }

    // Rasterise glyph_id using the font at `font_ptr` (raw ptr to avoid
    // borrow-checker conflict with &mut self in blit_bitmap).
    fn rasterise_glyph_from_ptr(
        &mut self,
        glyph_id: GlyphId,
        font_ptr: *const OwnedFont,
    ) -> Option<GlyphInfo> {
        // SAFETY: font_ptr always points into self.{regular,bold,italic}.
        let sf = unsafe { (*font_ptr).scaled() };
        let advance = sf.h_advance(glyph_id).ceil() as i32;
        let ascent_px = sf.ascent().round();
        let glyph = glyph_id.with_scale_and_position(sf.scale, ab_glyph::point(0.0, ascent_px));
        let outlined = sf.font.outline_glyph(glyph)?;
        let bounds = outlined.px_bounds();
        let w = bounds.width().ceil() as u32;
        let h = bounds.height().ceil() as u32;
        let bearing_x = bounds.min.x.round() as i32;
        // bearing_y = how far above the baseline the top of the glyph sits.
        // With ascent_px rounded, this is now always an integer, matching ascender (also ceil).
        let bearing_y = (-bounds.min.y).round() as i32;

        let coverage: Option<Vec<u8>> = if w > 0 && h > 0 {
            let mut buf = vec![0u8; (w * h) as usize];
            outlined.draw(|px, py, cov| {
                let idx = (py * w + px) as usize;
                if idx < buf.len() {
                    buf[idx] = (cov * 255.0).round() as u8;
                }
            });
            Some(buf)
        } else {
            None
        };
        drop(sf);

        match coverage {
            None => Some(GlyphInfo {
                uv_x: 0.0,
                uv_y: 0.0,
                uv_w: 0.0,
                uv_h: 0.0,
                width: 0,
                height: 0,
                bearing_x,
                bearing_y,
                advance,
            }),
            Some(buf) => self.blit_bitmap(buf, w, h, bearing_x, bearing_y, advance, "glyph"),
        }
    }

    // ── glyph-id-based lookup (shaped/ligature path) ───────────────────────────

    pub fn glyph_by_id(&mut self, id: u32, bold: bool, italic: bool) -> Option<GlyphInfo> {
        let key = GlyphIdKey { id, bold, italic };
        if let Some(&cached) = self.id_cache.get(&key) {
            return cached;
        }
        let info = self.rasterise_by_id(id, bold, italic);
        self.id_cache.insert(key, info);
        info
    }

    fn rasterise_by_id(&mut self, id: u32, bold: bool, italic: bool) -> Option<GlyphInfo> {
        let font_ptr: *const OwnedFont = if bold && self.bold.is_some() {
            self.bold.as_ref().unwrap() as *const _
        } else if italic && self.italic.is_some() {
            self.italic.as_ref().unwrap() as *const _
        } else {
            &self.regular as *const _
        };
        let glyph_id = GlyphId(id as u16);
        self.rasterise_glyph_from_ptr(glyph_id, font_ptr)
    }

    // ── atlas blitter ─────────────────────────────────────────────────────────

    fn blit_bitmap(
        &mut self,
        bitmap_buf: Vec<u8>,
        w: u32,
        h: u32,
        bearing_x: i32,
        bearing_y: i32,
        advance: i32,
        label: &str,
    ) -> Option<GlyphInfo> {
        // For synthetic glyphs the bitmap is exactly cell_w × cell_h.
        // For font glyphs we cap height at cell_h*2 as before.
        let hu = if label == "synthetic" {
            h
        } else {
            h.min(self.cell_h * 2)
        };

        if self.cursor_x + w + GAP > ATLAS_SIZE {
            self.cursor_y += self.row_h + GAP;
            self.cursor_x = 0;
            self.row_h = 0;
        }
        if self.cursor_y + hu + GAP > ATLAS_SIZE {
            tracing::warn!("Atlas full — {label} dropped");
            return None;
        }

        let aw = ATLAS_SIZE as usize;
        for py in 0..hu {
            for px in 0..w {
                let src_idx = (py * w + px) as usize;
                // bitmap_buf may be alpha-only (font path) OR RGBA (box_drawing path).
                // We always produce RGBA in the atlas.
                let alpha = if bitmap_buf.len() == (w * h) as usize {
                    // alpha-only (font outline path)
                    bitmap_buf[src_idx]
                } else {
                    // RGBA (box_drawing path) — channel 3 is alpha
                    bitmap_buf[src_idx * 4 + 3]
                };
                let base = ((self.cursor_y + py) as usize * aw + (self.cursor_x + px) as usize) * 4;
                self.pixels[base] = 0xFF;
                self.pixels[base + 1] = 0xFF;
                self.pixels[base + 2] = 0xFF;
                self.pixels[base + 3] = alpha;
            }
        }

        // UV coordinates: inset by 0.5 texel so NEAREST sampling always fetches
        // the correct atlas pixel even when floating-point UV values land on a
        // texel boundary. Without this inset, borderline UVs can round the wrong
        // direction and fetch a neighbouring (possibly empty) texel.
        let half = 0.5 / ATLAS_SIZE as f32;
        let info = GlyphInfo {
            uv_x: self.cursor_x as f32 / ATLAS_SIZE as f32 + half,
            uv_y: self.cursor_y as f32 / ATLAS_SIZE as f32 + half,
            uv_w: w as f32 / ATLAS_SIZE as f32 - 2.0 * half,
            uv_h: hu as f32 / ATLAS_SIZE as f32 - 2.0 * half,
            width: w as i32,
            height: hu as i32,
            bearing_x,
            bearing_y,
            advance,
        };

        self.cursor_x += w + GAP;
        if hu > self.row_h {
            self.row_h = hu;
        }
        self.dirty = true;
        Some(info)
    }
}
