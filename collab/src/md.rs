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

use crate::draw::{CODE_BG, GRAY, LIGHT};
use crate::font::CHAR_ROWS;
use crate::fb::Framebuffer;
use crate::text::{self, Face};
use crate::svg;

const BG: i32 = 14; /* gap between segments */
const CODE_PAD: i32 = 12;
const LABEL_H: i32 = CHAR_ROWS * 2 + 8; /* bitmap-font language label */

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

/* ---- per-segment metrics (height + draw share these) ---------------------
 * `base` is a pixel size now (not a bitmap-font integer scale): the body
 * text height. Headings scale up from it, code scales slightly down. */

fn heading_px(level: u8, base: i32) -> f32 {
    base as f32
        * match level {
            1 => 1.5,
            2 => 1.28,
            _ => 1.12,
        }
}

fn code_px(base: i32) -> f32 {
    (base as f32 * 0.82).max(15.0)
}

fn bullet_indent(base: i32) -> i32 {
    (base as f32 * 1.4) as i32
}

/// A code block's pixel height for `n` lines. Also used for the SVG
/// fallback, so both paths agree.
fn code_height(n: i32, base: i32, has_lang: bool) -> i32 {
    let label = if has_lang { LABEL_H } else { 0 };
    CODE_PAD + label + n.max(1) * text::line_h(Face::Mono, code_px(base)) + CODE_PAD
}

/// If an SVG rasterizes, its (w, h, pixels); otherwise None (draw as code).
fn svg_image(src: &str, width: i32) -> Option<(i32, i32, Vec<u8>)> {
    svg::rasterize(src, width, 760)
}

fn seg_height(seg: &Seg, base: i32, width: i32) -> i32 {
    let bpx = base as f32;
    match seg {
        Seg::Heading(l, txt) => {
            let hpx = heading_px(*l, base);
            text::wrap(Face::Heading, hpx, width, txt).len() as i32 * text::line_h(Face::Heading, hpx)
                + 8 /* underline + spacing */
        }
        Seg::Bullet(txt) => {
            let w = width - bullet_indent(base);
            text::wrap(Face::Body, bpx, w, txt).len() as i32 * text::line_h(Face::Body, bpx)
        }
        Seg::Para(txt) => {
            text::wrap(Face::Body, bpx, width, txt).len() as i32 * text::line_h(Face::Body, bpx)
        }
        Seg::Code(lang, lines) => code_height(lines.len() as i32, base, !lang.is_empty()),
        Seg::Svg(src) => match svg_image(src, width) {
            Some((_, h, _)) => h + 8,
            None => code_height(src.split('\n').count() as i32, base, true),
        },
    }
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
    let bpx = base as f32;
    match seg {
        Seg::Heading(l, txt) => {
            let hpx = heading_px(*l, base);
            let mut ly = y;
            for line in text::wrap(Face::Heading, hpx, width, txt) {
                text::draw_line(fb, x, ly, Face::Heading, hpx, &line);
                ly += text::line_h(Face::Heading, hpx);
            }
            fb.fill_rect(x, ly + 2, width, 2, LIGHT); /* underline */
        }
        Seg::Bullet(txt) => {
            let indent = bullet_indent(base);
            let lh = text::line_h(Face::Body, bpx);
            fb.disc(x + indent / 2, y + lh / 2, (base / 7).max(3), crate::draw::BLACK);
            let mut ly = y;
            for line in text::wrap(Face::Body, bpx, width - indent, txt) {
                text::draw_line(fb, x + indent, ly, Face::Body, bpx, &line);
                ly += lh;
            }
        }
        Seg::Para(txt) => {
            let mut ly = y;
            for line in text::wrap(Face::Body, bpx, width, txt) {
                text::draw_line(fb, x, ly, Face::Body, bpx, &line);
                ly += text::line_h(Face::Body, bpx);
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
    /* very pale box + a darker left accent bar so it still reads as code
     * without a heavy gray fill that flickers on e-ink */
    fb.fill_rect(x - 8, y, width + 16, h, CODE_BG);
    fb.fill_rect(x - 8, y, 4, h, GRAY);
    let cpx = code_px(base);
    let clh = text::line_h(Face::Mono, cpx);
    let mut ly = y + CODE_PAD;
    if !lang.is_empty() {
        /* the small language tag stays on the dim bitmap font */
        fb.text(x, ly, lang, 2, GRAY);
        ly += LABEL_H;
    }
    let maxc = (width / text::advance(Face::Mono, cpx)).max(1) as usize;
    for line in lines {
        /* preserve line breaks; clip overlong lines rather than wrapping */
        let shown: String = line.chars().take(maxc).collect();
        text::draw_line(fb, x, ly, Face::Mono, cpx, &shown);
        ly += clh;
    }
}
