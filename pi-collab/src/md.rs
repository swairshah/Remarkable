//! Light content detection + rendering for pi's replies. pi writes prose,
//! but also code, markdown, and the occasional SVG diagram — this splits a
//! reply into typed segments and renders each in its own style:
//!
//!   - headings  -> larger text, underlined
//!   - bullets   -> "• " with a hanging indent
//!   - code      -> a gray box, monospace, line breaks preserved
//!   - svg       -> rasterized to a bitmap (see svg.rs); falls back to a
//!                  code box if it uses anything the mini-renderer can't do
//!   - prose     -> word-wrapped, with **bold**/`code` markers stripped
//!
//! Detection is by the usual markers: fenced ``` blocks (with an optional
//! language), a bare <svg>…</svg>, leading #/-/* on a line. `height` and
//! `draw` walk the same segment list so scroll math and painting agree.

use crate::conv::wrap;
use crate::draw::{BLACK, GRAY, LIGHT};
use crate::font::{ADVANCE, CHAR_ROWS};
use crate::qtfb::Framebuffer;
use crate::svg;

const BG: i32 = 12; /* gap between segments */
const CODE_PAD: i32 = 12;

pub enum Seg {
    Heading(u8, String),
    Bullet(String),
    Para(String),
    Code(String, Vec<String>), /* language, lines */
    Svg(String),               /* source */
}

/* ---- parsing ------------------------------------------------------------- */

pub fn parse(text: &str) -> Vec<Seg> {
    let mut segs = Vec::new();
    let mut para: Vec<String> = Vec::new();
    let flush_para = |para: &mut Vec<String>, segs: &mut Vec<Seg>| {
        if !para.is_empty() {
            segs.push(Seg::Para(clean_inline(&para.join(" "))));
            para.clear();
        }
    };

    let lines: Vec<&str> = text.split('\n').collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let t = line.trim_start();

        /* fenced code block */
        if let Some(rest) = t.strip_prefix("```") {
            flush_para(&mut para, &mut segs);
            let lang = rest.trim().to_string();
            let mut body = Vec::new();
            i += 1;
            while i < lines.len() && !lines[i].trim_start().starts_with("```") {
                body.push(lines[i].to_string());
                i += 1;
            }
            i += 1; /* skip the closing fence (or run off the end) */
            let joined = body.join("\n");
            if lang.eq_ignore_ascii_case("svg") || joined.contains("<svg") {
                segs.push(Seg::Svg(joined));
            } else {
                segs.push(Seg::Code(lang, body));
            }
            continue;
        }

        /* a bare <svg>…</svg> block, not fenced */
        if t.contains("<svg") {
            flush_para(&mut para, &mut segs);
            let mut body = vec![line.to_string()];
            while i < lines.len() && !lines[i].contains("</svg>") {
                i += 1;
                if i < lines.len() {
                    body.push(lines[i].to_string());
                }
            }
            i += 1;
            segs.push(Seg::Svg(body.join("\n")));
            continue;
        }

        /* heading */
        if let Some(h) = heading(t) {
            flush_para(&mut para, &mut segs);
            segs.push(h);
            i += 1;
            continue;
        }

        /* bullet / numbered list item */
        if let Some(b) = bullet(t) {
            flush_para(&mut para, &mut segs);
            segs.push(Seg::Bullet(clean_inline(&b)));
            i += 1;
            continue;
        }

        if t.is_empty() {
            flush_para(&mut para, &mut segs);
        } else {
            para.push(t.to_string());
        }
        i += 1;
    }
    flush_para(&mut para, &mut segs);
    segs
}

fn heading(t: &str) -> Option<Seg> {
    let hashes = t.bytes().take_while(|&b| b == b'#').count();
    if (1..=6).contains(&hashes) && t.as_bytes().get(hashes) == Some(&b' ') {
        return Some(Seg::Heading(
            hashes as u8,
            clean_inline(t[hashes..].trim()),
        ));
    }
    None
}

fn bullet(t: &str) -> Option<String> {
    for p in ["- ", "* ", "+ "] {
        if let Some(r) = t.strip_prefix(p) {
            return Some(r.to_string());
        }
    }
    /* "1. ", "2. ", … */
    let digits = t.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digits > 0 && t[digits..].starts_with(". ") {
        return Some(t[digits + 2..].to_string());
    }
    None
}

/// Strip inline markdown emphasis so markers don't render literally.
fn clean_inline(s: &str) -> String {
    s.replace("**", "").replace("__", "").replace('`', "")
}

/* ---- per-segment metrics (height + draw share these) --------------------- */

fn line_h(scale: i32) -> i32 {
    CHAR_ROWS * scale + 8
}

fn heading_scale(level: u8, base: i32) -> i32 {
    match level {
        1 => base + 2,
        2 => base + 1,
        _ => base,
    }
}

fn code_scale(base: i32) -> i32 {
    (base - 1).max(2)
}

fn code_line_h(base: i32) -> i32 {
    CHAR_ROWS * code_scale(base) + 6
}

/// A code block's pixel height for `n` lines. Also used for the SVG
/// fallback, so both paths agree.
fn code_height(n: i32, base: i32, has_lang: bool) -> i32 {
    let label = if has_lang { CHAR_ROWS * 2 + 6 } else { 0 };
    CODE_PAD + label + n.max(1) * code_line_h(base) + CODE_PAD
}

/// If an SVG rasterizes, its (w, h, pixels); otherwise None (draw as code).
fn svg_image(src: &str, width: i32) -> Option<(i32, i32, Vec<u8>)> {
    svg::rasterize(src, width, 760)
}

fn seg_height(seg: &Seg, base: i32, width: i32) -> i32 {
    match seg {
        Seg::Heading(l, txt) => {
            let s = heading_scale(*l, base);
            wrap(txt, cols(width, s)).len() as i32 * line_h(s) + 6 /* underline */
        }
        Seg::Bullet(txt) => wrap(txt, cols(width, base) - 2).len() as i32 * line_h(base),
        Seg::Para(txt) => wrap(txt, cols(width, base)).len() as i32 * line_h(base),
        Seg::Code(lang, lines) => code_height(lines.len() as i32, base, !lang.is_empty()),
        Seg::Svg(src) => match svg_image(src, width) {
            Some((_, h, _)) => h + 8,
            None => code_height(src.split('\n').count() as i32, base, true),
        },
    }
}

fn cols(width: i32, scale: i32) -> usize {
    (width / (ADVANCE * scale)).max(1) as usize
}

pub fn height(segs: &[Seg], base: i32, width: i32) -> i32 {
    segs.iter().map(|s| seg_height(s, base, width) + BG).sum()
}

/* ---- drawing ------------------------------------------------------------- */

/// Draw all segments starting at (x, y); returns the total height drawn.
pub fn draw(fb: &mut Framebuffer, x: i32, y0: i32, width: i32, segs: &[Seg], base: i32) -> i32 {
    let mut y = y0;
    for seg in segs {
        draw_seg(fb, x, y, width, seg, base);
        y += seg_height(seg, base, width) + BG;
    }
    y - y0
}

fn draw_seg(fb: &mut Framebuffer, x: i32, y: i32, width: i32, seg: &Seg, base: i32) {
    match seg {
        Seg::Heading(l, txt) => {
            let s = heading_scale(*l, base);
            let mut ly = y;
            for line in wrap(txt, cols(width, s)) {
                fb.text(x, ly, &line, s, BLACK);
                ly += line_h(s);
            }
            fb.fill_rect(x, ly, width, 2, LIGHT); /* underline */
        }
        Seg::Bullet(txt) => {
            /* the ASCII font has no bullet glyph, so draw a small filled dot
             * centered on the first line, then hang the text to its right */
            fb.disc(x + 2 * base, y + CHAR_ROWS * base / 2, base, BLACK);
            let ix = x + 2 * ADVANCE * base;
            let mut ly = y;
            for line in wrap(txt, cols(width, base) - 2) {
                fb.text(ix, ly, &line, base, BLACK);
                ly += line_h(base);
            }
        }
        Seg::Para(txt) => {
            let mut ly = y;
            for line in wrap(txt, cols(width, base)) {
                fb.text(x, ly, &line, base, BLACK);
                ly += line_h(base);
            }
        }
        Seg::Code(lang, lines) => draw_code(fb, x, y, width, lang, lines, base),
        Seg::Svg(src) => match svg_image(src, width) {
            Some((iw, ih, buf)) => {
                let ix = x + (width - iw).max(0) / 2;
                fb.blit_gray(ix, y, iw, ih, &buf);
            }
            None => {
                let lines: Vec<String> = src.split('\n').map(|s| s.to_string()).collect();
                draw_code(fb, x, y, width, "svg", &lines, base);
            }
        },
    }
}

fn draw_code(fb: &mut Framebuffer, x: i32, y: i32, width: i32, lang: &str, lines: &[String], base: i32) {
    let h = code_height(lines.len() as i32, base, !lang.is_empty());
    /* box + left accent bar */
    fb.fill_rect(x - 8, y, width + 16, h, LIGHT);
    fb.fill_rect(x - 8, y, 4, h, GRAY);
    let cs = code_scale(base);
    let mut ly = y + CODE_PAD;
    if !lang.is_empty() {
        fb.text(x, ly, lang, 2, GRAY);
        ly += CHAR_ROWS * 2 + 6;
    }
    let maxc = (width / (ADVANCE * cs)).max(1) as usize;
    for line in lines {
        /* preserve line breaks; clip overlong lines rather than wrapping */
        let shown: String = line.chars().take(maxc).collect();
        fb.text(x, ly, &shown, cs, BLACK);
        ly += code_line_h(base);
    }
}
