// util.rs — shared parsing / path helpers used by both config.rs and twm_config.rs
//
// Add to both crates' lib.rs / main.rs:
//   mod util;
// Then import with:
//   use crate::util::{expand_tilde, resolve_path, shell_words, strip_comment};

use std::path::{Path, PathBuf};

// ── Path helpers ──────────────────────────────────────────────────────────────

pub fn expand_tilde(s: &str) -> String {
    if s.starts_with('~') {
        let home = std::env::var("HOME").unwrap_or_default();
        format!("{home}{}", &s[1..])
    } else {
        s.to_owned()
    }
}

pub fn resolve_path(value: &str, relative_to: &Path) -> PathBuf {
    let expanded = expand_tilde(value);
    let p = PathBuf::from(&expanded);
    if p.is_absolute() {
        p
    } else {
        relative_to.parent().unwrap_or(Path::new(".")).join(p)
    }
}

// ── Shell-style word splitter ─────────────────────────────────────────────────
//
// Handles single-quoted, double-quoted, and backslash-escaped tokens.
// Does NOT perform glob expansion or variable substitution.

pub fn shell_words(s: &str) -> Vec<String> {
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

// ── Comment stripper ──────────────────────────────────────────────────────────
//
// A `#` is treated as a comment only when it is preceded by whitespace (or is
// at column 0) AND it is not the first non-whitespace character of the *value*
// part of a `key = value` line (to avoid stripping hex colour literals like
// `background = #1E1E2E`).
//
// For lines without `=` (section headers, braces) the simpler rule applies:
// strip from the first `#` that is preceded by whitespace or is at col 0.

pub fn strip_comment(line: &str) -> &str {
    if let Some(eq_pos) = line.find('=') {
        // ── key side: strip from first whitespace-preceded `#` ────────────────
        let key_part = &line[..eq_pos];
        let key_end = key_part
            .as_bytes()
            .iter()
            .enumerate()
            .find(|&(i, &b)| {
                b == b'#' && (i == 0 || key_part.as_bytes()[i - 1].is_ascii_whitespace())
            })
            .map(|(i, _)| i)
            .unwrap_or(eq_pos);

        if key_end < eq_pos {
            return &line[..key_end];
        }

        // ── value side ────────────────────────────────────────────────────────
        // The first non-whitespace character after `=` may be `#` (hex colour).
        // Only strip a `#` that appears *after* the colour token has ended
        // (i.e. after whitespace following the hex digits).
        let val = &line[eq_pos + 1..];
        let val_bytes = val.as_bytes();
        let mut seen_nonws = false;
        let mut in_hex_token = false;

        for (i, &b) in val_bytes.iter().enumerate() {
            match b {
                b'#' if !seen_nonws => {
                    // Leading `#` — start of a hex colour literal.
                    in_hex_token = true;
                    seen_nonws = true;
                }
                b'#' => {
                    // A second `#`, or `#` after non-hex content — comment.
                    return &line[..eq_pos + 1 + i];
                }
                b if b.is_ascii_whitespace() => {
                    in_hex_token = false;
                }
                _ => {
                    seen_nonws = true;
                    in_hex_token = false;
                }
            }
        }

        line
    } else {
        // Section header / brace / bare keyword — strip from first `#`
        // that is at col 0 or preceded by whitespace.
        let bytes = line.as_bytes();
        for (i, &b) in bytes.iter().enumerate() {
            if b == b'#' && (i == 0 || bytes[i - 1].is_ascii_whitespace()) {
                return &line[..i];
            }
        }
        line
    }
}

// ── Hex colour helpers ────────────────────────────────────────────────────────

/// Parse `#RRGGBB` or `#RRGGBBAA` → `[f32; 4]`.
/// Falls back to magenta on malformed input (with a tracing warning).
pub fn hex4(s: &str) -> [f32; 4] {
    let s = s.trim().trim_start_matches('#');
    let p = |i: usize| u8::from_str_radix(&s[i..i + 2], 16).unwrap_or(0xFF) as f32 / 255.0;
    match s.len() {
        6 => [p(0), p(2), p(4), 1.0],
        8 => [p(0), p(2), p(4), p(6)],
        _ => {
            tracing::warn!("Bad hex colour '#{s}' — using magenta");
            [1.0, 0.0, 1.0, 1.0]
        }
    }
}

/// Parse `#RRGGBB` → `[u8; 3]`.
/// Falls back to magenta on malformed input (with a tracing warning).
pub fn hex3(s: &str) -> [u8; 3] {
    let s = s.trim().trim_start_matches('#');
    let p = |i: usize| u8::from_str_radix(&s[i..i + 2], 16).unwrap_or(0xFF);
    if s.len() >= 6 {
        [p(0), p(2), p(4)]
    } else {
        tracing::warn!("Bad hex colour '#{s}' — using magenta");
        [0xFF, 0x00, 0xFF]
    }
}

/// `[f32; 4]` RGBA → `[u8; 3]` RGB.
pub fn f32x4_to_u8x3(c: [f32; 4]) -> [u8; 3] {
    [
        (c[0].clamp(0.0, 1.0) * 255.0).round() as u8,
        (c[1].clamp(0.0, 1.0) * 255.0).round() as u8,
        (c[2].clamp(0.0, 1.0) * 255.0).round() as u8,
    ]
}

// ── Font search ───────────────────────────────────────────────────────────────

pub fn find_font(needle: &str) -> Option<std::path::PathBuf> {
    let home = std::env::var("HOME").unwrap_or_default();
    let roots: &[String] = &[
        "/usr/share/fonts".into(),
        "/usr/local/share/fonts".into(),
        format!("{home}/.local/share/fonts"),
        format!("{home}/.fonts"),
    ];
    let needle_lower = needle.to_lowercase();
    for root in roots {
        if let Some(p) = walk_fonts(Path::new(root), &needle_lower) {
            return Some(p);
        }
    }
    None
}

fn walk_fonts(dir: &Path, needle: &str) -> Option<PathBuf> {
    let rd = std::fs::read_dir(dir).ok()?;
    let mut subdirs = Vec::new();
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() {
            subdirs.push(path);
        } else if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.to_lowercase().contains(needle) {
                tracing::info!("Font search: found '{needle}' at {}", path.display());
                return Some(path);
            }
        }
    }
    for sub in subdirs {
        if let Some(p) = walk_fonts(&sub, needle) {
            return Some(p);
        }
    }
    None
}

pub fn derive_variant(regular_name: &str, variant: &str) -> String {
    for sep in &["-Regular", "_Regular", "Regular"] {
        if let Some(idx) = regular_name.to_lowercase().find(&sep.to_lowercase()) {
            let base = &regular_name[..idx];
            let ext = regular_name
                .rfind('.')
                .map(|i| &regular_name[i..])
                .unwrap_or(".ttf");
            return format!("{base}-{variant}{ext}");
        }
    }
    let ext = regular_name
        .rfind('.')
        .map(|i| &regular_name[i..])
        .unwrap_or(".ttf");
    let stem = regular_name
        .rfind('.')
        .map(|i| &regular_name[..i])
        .unwrap_or(regular_name);
    format!("{stem}-{variant}{ext}")
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_hex_colour_untouched() {
        assert_eq!(
            strip_comment("background_color = #1E1E2E"),
            "background_color = #1E1E2E"
        );
    }

    #[test]
    fn strip_trailing_comment() {
        assert_eq!(
            strip_comment("vsync = off  # disable vsync"),
            "vsync = off  "
        );
    }

    #[test]
    fn strip_hex_with_trailing_comment() {
        // second `#` after the colour token is a comment
        assert_eq!(
            strip_comment("background_color = #1E1E2E # dark bg"),
            "background_color = #1E1E2E "
        );
    }

    #[test]
    fn shell_words_quoted() {
        assert_eq!(
            shell_words(r#"kitty --title "my term" -e fish"#),
            vec!["kitty", "--title", "my term", "-e", "fish"]
        );
    }

    #[test]
    fn shell_words_single_quoted() {
        assert_eq!(
            shell_words("exec '/usr/bin/my app'"),
            vec!["exec", "/usr/bin/my app"]
        );
    }

    #[test]
    fn expand_tilde_home() {
        std::env::set_var("HOME", "/home/user");
        assert_eq!(expand_tilde("~/.config"), "/home/user/.config");
        assert_eq!(expand_tilde("/absolute"), "/absolute");
    }

    #[test]
    fn hex4_rrggbb() {
        let c = hex4("#1E1E2E");
        assert!((c[0] - 0x1E as f32 / 255.0).abs() < 1e-4);
        assert_eq!(c[3], 1.0);
    }

    #[test]
    fn hex3_parse() {
        assert_eq!(hex3("#B4BEFE"), [0xB4, 0xBE, 0xFE]);
        assert_eq!(hex3("B4BEFE"), [0xB4, 0xBE, 0xFE]);
    }
}
