//! Hershey single-stroke fonts, as polylines — three faces:
//!
//!   Sans    Simplex        the neutral plotter look
//!   Script  Script Simplex natural, cursive-handwriting-like
//!   Serif   Times Roman    formal; the stroke-font cousin of collab's Garamond
//!
//! This is what makes the AI's text look *drawn* rather than typeset: every
//! glyph is a set of pen paths, so `<text>` in a patch renders through the
//! same stroke pipeline as everything else and animates in like handwriting.
//!
//! The default face comes from $READER_FONT (sans|script|serif, default
//! serif); pi can override per element with font-family in its SVG.
//!
//! Geometry (font units): y grows DOWN, baseline at y=9, cap top at y=-12
//! (cap height 21). We map a CSS-ish `font-size` so that size ≈ the em:
//! scale = size / 30 puts the cap height at 0.7*size, close to real fonts.
//!
//! Data: futural/scripts/timesr .jhf (public domain), converted offline
//! into hershey_data.rs.

use crate::hershey_data::{math_glyph, GREEK, SANS, SCRIPT, SERIF};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Face {
    Sans,
    Script,
    Serif,
}

pub(crate) struct Glyph {
    pub adv: i8,               /* advance width, font units */
    pub left: i8,              /* left margin (subtract to left-align) */
    pub pts: &'static [(i8, i8)], /* vertices; (-64,-64) = pen up */
}

const PEN_UP: (i8, i8) = (-64, -64);
const CAP_TOP: f32 = -12.0;
const BASELINE: f32 = 9.0;

/// Resolve a font name (from $READER_FONT or an SVG font-family) to a
/// face. Generic CSS families map to the closest look.
pub fn face_from_name(name: &str) -> Option<Face> {
    let n = name.to_ascii_lowercase();
    if n.contains("script") || n.contains("cursive") || n.contains("hand") {
        Some(Face::Script)
    } else if n.contains("serif") && !n.contains("sans") || n.contains("times")
        || n.contains("formal") || n.contains("roman") || n.contains("garamond")
    {
        Some(Face::Serif)
    } else if n.contains("sans") || n.contains("simplex") || n.contains("plotter")
        || n.contains("mono")
    {
        Some(Face::Sans)
    } else {
        None
    }
}

/// The app-wide default face ($READER_FONT, default serif).
pub fn default_face() -> Face {
    std::env::var("READER_FONT")
        .ok()
        .and_then(|v| face_from_name(&v))
        .unwrap_or(Face::Serif)
}

fn table(f: Face) -> &'static [Glyph; 96] {
    match f {
        Face::Sans => &SANS,
        Face::Script => &SCRIPT,
        Face::Serif => &SERIF,
    }
}

/// Greek letters live in their own Hershey face, transliteration-ordered
/// into the Latin slots (A..X, a..x). Any face gets Greek from it.
fn greek_slot(c: char) -> Option<usize> {
    let u = c as u32;
    let (base, i) = match u {
        0x0391..=0x03A1 => ('A', u - 0x0391),         /* Α..Ρ */
        0x03A3..=0x03A9 => ('A', u - 0x0391 - 1),     /* Σ..Ω (03A2 unused) */
        0x03B1..=0x03C1 => ('a', u - 0x03B1),         /* α..ρ */
        0x03C2 => ('a', 17),                          /* ς -> σ */
        0x03C3..=0x03C9 => ('a', u - 0x03B1 - 1),     /* σ..ω */
        _ => return None,
    };
    Some(base as usize - 32 + i as usize)
}

fn glyph(f: Face, c: char) -> &'static Glyph {
    if let Some(slot) = greek_slot(c) {
        return &GREEK[slot];
    }
    if let Some(g) = math_glyph(c) {
        return g;
    }
    let i = c as usize;
    let t = table(f);
    if (32..128).contains(&i) {
        &t[i - 32]
    } else {
        &t[('?' as usize) - 32]
    }
}

/// Fold Unicode with no glyph of its own (curly quotes, dashes, ellipses)
/// to drawable equivalents instead of letting it collapse into '?'.
/// Math symbols (±, ×, ≤, ∫, ∑, √, ∞, arrows, ...) render natively via
/// the MATH table; Greek via the GREEK face — don't fold those.
pub fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{2032}' => out.push('\''),
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{2033}' => out.push('"'),
            '\u{2010}' | '\u{2011}' | '\u{2013}' | '\u{2014}' | '\u{2015}' | '\u{2212}' => {
                out.push('-')
            }
            '\u{2026}' => out.push_str("..."),
            '\u{2022}' | '\u{25CF}' | '\u{25AA}' => out.push('-'),
            '\u{2194}' => out.push_str("<->"),
            '\u{21D2}' | '\u{27F9}' | '\u{21A6}' => out.push('\u{2192}'), /* => real arrow */
            '\u{21D0}' | '\u{27F8}' => out.push('\u{2190}'),
            '\u{00A0}' | '\u{2009}' | '\u{200A}' | '\u{2002}' | '\u{2003}' => out.push(' '),
            '\u{00B5}' => out.push('\u{03BC}'),   /* micro -> Greek mu */
            '\u{03F5}' => out.push('\u{03B5}'),   /* lunate epsilon */
            '\u{03D5}' => out.push('\u{03C6}'),   /* phi variants */
            '\u{03D1}' => out.push('\u{03B8}'),   /* theta variant */
            '\u{211D}' => out.push('R'),          /* double-struck sets */
            '\u{2115}' => out.push('N'),
            '\u{2124}' => out.push('Z'),
            '\u{211A}' => out.push('Q'),
            '\u{2102}' => out.push('C'),
            '\u{210F}' => out.push('h'),          /* hbar */
            '\u{2113}' => out.push('l'),          /* script ell */
            '\u{2217}' => out.push('*'),
            '\u{2223}' => out.push('|'),
            '\u{2225}' => out.push_str("||"),
            '\u{27E8}' | '\u{2329}' => out.push('<'),
            '\u{27E9}' | '\u{232A}' => out.push('>'),
            '\u{00B9}' => out.push('1'),          /* super/subscript digits: */
            '\u{00B2}' => out.push('2'),          /* better flat than '?'    */
            '\u{00B3}' => out.push('3'),
            '\u{2070}' => out.push('0'),
            c @ '\u{2074}'..='\u{2079}' => out.push((b'0' + (c as u32 - 0x2070) as u8) as char),
            c @ '\u{2080}'..='\u{2089}' => out.push((b'0' + (c as u32 - 0x2080) as u8) as char),
            _ => out.push(c), /* other non-ASCII still ends up as '?' */
        }
    }
    out
}

/// Advance width of `s` in font units (scale by `size/30` for pixels).
pub fn width_units(f: Face, s: &str) -> f32 {
    fold(s).chars().map(|c| glyph(f, c).adv as f32).sum()
}

pub fn scale_for(size: f32) -> f32 {
    size / 30.0
}

/// Baseline-to-baseline line height in pixels for `size`.
pub fn line_height(size: f32) -> f32 {
    (BASELINE - CAP_TOP + 11.0) * scale_for(size) /* 21 + leading */
}

pub fn text_width(f: Face, s: &str, size: f32) -> f32 {
    width_units(f, s) * scale_for(size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn math_symbols_have_real_glyphs() {
        let question = strokes(Face::Serif, "?", 0.0, 0.0, 30.0);
        for c in [
            '\u{222B}', '\u{2211}', '\u{220F}', '\u{221A}', '\u{221E}', '\u{00B1}',
            '\u{00D7}', '\u{00F7}', '\u{2264}', '\u{2265}', '\u{2260}', '\u{2248}',
            '\u{2202}', '\u{2207}', '\u{2208}', '\u{2192}', '\u{00B7}', '\u{00B0}',
            '\u{2200}', '\u{2203}', '\u{2205}', '\u{03C0}',
        ] {
            let s = c.to_string();
            assert!(width_units(Face::Serif, &s) > 0.0, "{c} has no advance");
            let st = strokes(Face::Serif, &s, 0.0, 0.0, 30.0);
            assert!(!st.is_empty(), "{c} drew nothing");
            assert_ne!(st, question, "{c} fell back to '?'");
        }
    }
}

/// Lay `s` out with its baseline at (x, y), returning one polyline per pen
/// path, in pixels. Multi-line input is NOT handled here (split upstream).
pub fn strokes(f: Face, s: &str, x: f32, y: f32, size: f32) -> Vec<Vec<(f32, f32)>> {
    let k = scale_for(size);
    let mut out = Vec::new();
    let mut pen_x = x;
    for c in fold(s).chars() {
        let g = glyph(f, c);
        let mut cur: Vec<(f32, f32)> = Vec::new();
        for &(gx, gy) in g.pts {
            if (gx, gy) == PEN_UP {
                if cur.len() >= 2 {
                    out.push(std::mem::take(&mut cur));
                } else {
                    cur.clear();
                }
            } else {
                cur.push((
                    pen_x + (gx as f32 - g.left as f32) * k,
                    y + (gy as f32 - BASELINE) * k,
                ));
            }
        }
        if cur.len() >= 2 {
            out.push(cur);
        }
        pen_x += g.adv as f32 * k;
    }
    out
}
