//! Antialiased TrueType text for pi's replies, via fontdue.
//!
//! Three faces are embedded: EB Garamond (body + a semibold for headings)
//! and Google Sans Code (code). Glyphs are rasterized once per (face, char,
//! pixel-size) into an 8-bit coverage bitmap and cached; drawing blends that
//! coverage as black over whatever is already in the framebuffer, so text
//! antialiases correctly over both the white page and the gray code boxes.
//!
//! Only pi's message content uses these fonts — the small UI chrome (labels,
//! buttons, logo) stays on the crisp 5x7 bitmap font in font.rs.

use crate::fb::Framebuffer;
use fontdue::{Font, FontSettings, Metrics};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Clone, Copy, PartialEq)]
pub enum Face {
    Body,
    Heading,
    Mono,
}

struct Fonts {
    body: Font,
    heading: Font,
    mono: Font,
}

static FONTS: OnceLock<Fonts> = OnceLock::new();

fn fonts() -> &'static Fonts {
    FONTS.get_or_init(|| {
        let load = |b: &[u8]| Font::from_bytes(b, FontSettings::default()).expect("font parse");
        Fonts {
            body: load(include_bytes!("../assets/fonts/EBGaramond-Regular.ttf") as &[u8]),
            heading: load(include_bytes!("../assets/fonts/EBGaramond-SemiBold.ttf") as &[u8]),
            mono: load(include_bytes!("../assets/fonts/GoogleSansCode-Regular.ttf") as &[u8]),
        }
    })
}

fn font(f: Face) -> &'static Font {
    let fs = fonts();
    match f {
        Face::Body => &fs.body,
        Face::Heading => &fs.heading,
        Face::Mono => &fs.mono,
    }
}

/// The face that actually has a glyph for `c`: the requested one first,
/// then the others — EB Garamond carries the Greek and math (ζ ∑ ∫ ∞ √ π²)
/// that the code font lacks, so formulas in code blocks stop rendering as
/// tofu boxes. If nobody covers `c`, the requested face's notdef it is
/// (fold() below should have caught it first).
fn face_for(f: Face, c: char) -> Face {
    if c.is_ascii() || font(f).lookup_glyph_index(c) != 0 {
        return f;
    }
    let others = match f {
        Face::Mono => [Face::Body, Face::Heading],
        Face::Body => [Face::Heading, Face::Mono],
        Face::Heading => [Face::Body, Face::Mono],
    };
    for cand in others {
        if font(cand).lookup_glyph_index(c) != 0 {
            return cand;
        }
    }
    f
}

/// Per-char metrics through the face fallback.
fn cmetrics(f: Face, c: char, px: f32) -> Metrics {
    font(face_for(f, c)).metrics(c, px)
}

/// Fold characters NONE of the embedded faces cover to readable ASCII
/// (the TTF cousin of notebook's hershey::fold). Everything the fonts do
/// cover — Greek, ∑ ∏ ∫ ∞ √ ≤ ≈ arrows, super/subscripts — passes through
/// untouched and renders via face_for. Idempotent; borrow when clean.
fn fold(s: &str) -> std::borrow::Cow<'_, str> {
    if s.is_ascii() {
        return s.into();
    }
    let needs = |c: char| {
        matches!(c as u32,
            0x211D | 0x2115 | 0x2124 | 0x211A | 0x2102 | 0x2261 | 0x221D
            | 0x2217 | 0x2225 | 0x2208 | 0x2209 | 0x2200 | 0x2203
            | 0x2282 | 0x2283 | 0x2286 | 0x2287 | 0x222A | 0x2229
            | 0x22A5 | 0x2220 | 0x2234 | 0x2235 | 0x210F | 0x2113
            | 0x222E | 0x27E8 | 0x27E9 | 0x2329 | 0x232A)
    };
    if !s.chars().any(needs) {
        return s.into();
    }
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c as u32 {
            0x211D => out.push('R'), /* double-struck sets */
            0x2115 => out.push('N'),
            0x2124 => out.push('Z'),
            0x211A => out.push('Q'),
            0x2102 => out.push('C'),
            0x2261 => out.push_str("=="),
            0x221D => out.push('~'),          /* proportional-to */
            0x2217 => out.push('*'),
            0x2225 => out.push_str("||"),
            0x2208 => out.push_str(" in "),
            0x2209 => out.push_str(" not in "),
            0x2200 => out.push_str("for all "),
            0x2203 => out.push_str("exists "),
            0x2282 | 0x2286 => out.push_str(" subset of "),
            0x2283 | 0x2287 => out.push_str(" superset of "),
            0x222A => out.push_str(" union "),
            0x2229 => out.push_str(" intersect "),
            0x22A5 => out.push_str(" perp "),
            0x2220 => out.push_str("angle "),
            0x2234 => out.push_str(" therefore "),
            0x2235 => out.push_str(" because "),
            0x210F => out.push_str("hbar"),
            0x2113 => out.push('l'),          /* script ell */
            0x222E => out.push('\u{222B}'),   /* ∮ -> plain ∫ */
            0x27E8 | 0x2329 => out.push('<'),
            0x27E9 | 0x232A => out.push('>'),
            _ => out.push(c),
        }
    }
    out.into()
}

thread_local! {
    /* (face, char, size-bits) -> rasterized coverage. Keyed by the f32 bit
     * pattern so identical sizes hit; the app uses a handful of sizes. */
    static CACHE: RefCell<HashMap<(u8, char, u32), (Metrics, Vec<u8>)>> =
        RefCell::new(HashMap::new());
}

fn with_glyph<R>(f: Face, c: char, px: f32, use_it: impl FnOnce(&Metrics, &[u8]) -> R) -> R {
    let f = face_for(f, c);
    let key = (f as u8, c, px.to_bits());
    CACHE.with(|cache| {
        let mut m = cache.borrow_mut();
        let g = m.entry(key).or_insert_with(|| font(f).rasterize(c, px));
        use_it(&g.0, &g.1)
    })
}

/* ---- metrics ------------------------------------------------------------- */

/// Baseline-to-baseline line height at this size.
pub fn line_h(f: Face, px: f32) -> i32 {
    font(f)
        .horizontal_line_metrics(px)
        .map(|m| m.new_line_size.ceil() as i32)
        .unwrap_or((px * 1.3) as i32)
}

fn width_f(f: Face, px: f32, s: &str) -> f32 {
    fold(s).chars().map(|c| cmetrics(f, c, px).advance_width).sum()
}

pub fn width(f: Face, px: f32, s: &str) -> i32 {
    width_f(f, px, s).ceil() as i32
}

pub fn ascent(f: Face, px: f32) -> f32 {
    font(f).horizontal_line_metrics(px).map(|m| m.ascent).unwrap_or(px)
}

/// One character's advance — for the monospace code path.
pub fn advance(f: Face, px: f32) -> i32 {
    font(f).metrics('0', px).advance_width.ceil().max(1.0) as i32
}

/// Draw a line into a standalone 8-bit grayscale buffer (0=black, 255=white),
/// blending glyphs as black. Used to place `<text>` labels inside a
/// rasterized SVG, whose pixels live in their own buffer rather than the fb.
pub fn draw_gray(buf: &mut [u8], bw: i32, bh: i32, x: i32, y_top: i32, f: Face, px: f32, s: &str) {
    let baseline = y_top as f32 + ascent(f, px);
    let mut pen = x as f32;
    for c in fold(s).chars() {
        with_glyph(f, c, px, |m, cov| {
            if m.width > 0 && m.height > 0 {
                let gx = (pen + m.xmin as f32).round() as i32;
                let gy = (baseline - m.ymin as f32 - m.height as f32).round() as i32;
                for row in 0..m.height as i32 {
                    let py = gy + row;
                    if py < 0 || py >= bh {
                        continue;
                    }
                    for col in 0..m.width as i32 {
                        let a = cov[(row * m.width as i32 + col) as usize] as u32;
                        if a == 0 {
                            continue;
                        }
                        let sx = gx + col;
                        if sx < 0 || sx >= bw {
                            continue;
                        }
                        let idx = (py * bw + sx) as usize;
                        buf[idx] = (buf[idx] as u32 * (255 - a) / 255) as u8;
                    }
                }
            }
            pen += m.advance_width;
        });
    }
}

/* ---- drawing ------------------------------------------------------------- */

/// Draw a single (already-wrapped) line at top-left (x, y_top). Returns the
/// advance width in pixels.
pub fn draw_line(fb: &mut Framebuffer, x: i32, y_top: i32, f: Face, px: f32, s: &str) -> i32 {
    let ascent = font(f).horizontal_line_metrics(px).map(|m| m.ascent).unwrap_or(px);
    let baseline = y_top as f32 + ascent;
    let mut pen = x as f32;
    for c in fold(s).chars() {
        with_glyph(f, c, px, |m, cov| {
            if m.width > 0 && m.height > 0 {
                let gx = (pen + m.xmin as f32).round() as i32;
                let gy = (baseline - m.ymin as f32 - m.height as f32).round() as i32;
                fb.blend_black(gx, gy, m.width as i32, m.height as i32, cov);
            }
            pen += m.advance_width;
        });
    }
    (pen - x as f32).round() as i32
}

/* ---- wrapping (by pixel width) ------------------------------------------- */

/// Wrap `text` to `max_w` pixels for the given face/size, honoring existing
/// newlines and hard-breaking words too long to fit a line.
pub fn wrap(f: Face, px: f32, max_w: i32, text: &str) -> Vec<String> {
    let space = font(f).metrics(' ', px).advance_width;
    let maxw = max_w as f32;
    let mut lines = Vec::new();

    let text = fold(text); /* so wrap widths match what draw_line renders */
    for para in text.split('\n') {
        let mut line = String::new();
        let mut w = 0.0f32;
        for word in para.split(' ') {
            let mut word = word;
            /* a word wider than the whole line: break it by characters */
            while width_f(f, px, word) > maxw {
                let mut cut = String::new();
                let mut cw = 0.0;
                let mut chars = word.char_indices();
                let mut split_at = word.len();
                for (i, c) in chars.by_ref() {
                    let a = cmetrics(f, c, px).advance_width;
                    if cw + a > maxw && !cut.is_empty() {
                        split_at = i;
                        break;
                    }
                    cut.push(c);
                    cw += a;
                }
                if !line.is_empty() {
                    lines.push(std::mem::take(&mut line));
                    w = 0.0;
                }
                lines.push(cut);
                if split_at >= word.len() {
                    word = "";
                    break;
                }
                word = &word[split_at..];
            }
            if word.is_empty() {
                continue;
            }
            let ww = width_f(f, px, word);
            let add = if line.is_empty() { ww } else { space + ww };
            if !line.is_empty() && w + add > maxw {
                lines.push(std::mem::take(&mut line));
                w = 0.0;
            }
            if !line.is_empty() {
                line.push(' ');
                w += space;
            }
            line.push_str(word);
            w += ww;
        }
        lines.push(line);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

