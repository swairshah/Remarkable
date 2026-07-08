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

thread_local! {
    /* (face, char, size-bits) -> rasterized coverage. Keyed by the f32 bit
     * pattern so identical sizes hit; the app uses a handful of sizes. */
    static CACHE: RefCell<HashMap<(u8, char, u32), (Metrics, Vec<u8>)>> =
        RefCell::new(HashMap::new());
}

fn with_glyph<R>(f: Face, c: char, px: f32, use_it: impl FnOnce(&Metrics, &[u8]) -> R) -> R {
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
    s.chars().map(|c| font(f).metrics(c, px).advance_width).sum()
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
    for c in s.chars() {
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
    for c in s.chars() {
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
                    let a = font(f).metrics(c, px).advance_width;
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
