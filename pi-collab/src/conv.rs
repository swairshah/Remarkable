//! The conversation model + its layout and rendering.
//!
//! A conversation is a flat list of entries (your handwriting, pi's text,
//! and dimmed notices). Rendering is immediate-mode: given a scroll offset
//! and a viewport band, `draw` paints the slice that falls inside it. The
//! draw.rs clip band means an entry straddling the top/bottom edge is
//! simply cut off, no per-entry scissoring needed here.

use crate::draw::{GRAY, LIGHT, WHITE};
use crate::font::{ADVANCE, CHAR_ROWS};
use crate::qtfb::{Framebuffer, RM2_WIDTH as FB_W};

pub const MARGIN: i32 = 28;
const LABEL_SCALE: i32 = 2;
const LABEL_H: i32 = CHAR_ROWS * LABEL_SCALE + 8; /* 22 */
const ENTRY_GAP: i32 = 24;
const PAD_TOP: i32 = 16;

/// How many characters of pi text fit on one line at the given scale.
pub fn wrap_cols(scale: i32) -> usize {
    ((FB_W - 2 * MARGIN) / (ADVANCE * scale)).max(1) as usize
}

/// A grayscale bitmap (0 = black), already scaled to display size.
pub struct GrayImg {
    pub w: i32,
    pub h: i32,
    pub px: Vec<u8>,
}

pub enum Entry {
    /// The user's handwritten ink for one message.
    You(GrayImg),
    /// pi's reply text (may still be growing while it streams).
    Pi(String),
    /// A dimmed status line (tool runs, errors, lifecycle).
    Note(String),
}

/// Wrap `text` to `cols` columns, honoring existing newlines and breaking
/// words longer than a line. Always returns at least one line.
pub fn wrap(text: &str, cols: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for para in text.split('\n') {
        let mut line = String::new();
        for word in para.split(' ') {
            let mut word = word;
            /* a single word longer than the line: hard-break it */
            while word.chars().count() > cols {
                let take: String = word.chars().take(cols).collect();
                if !line.is_empty() {
                    lines.push(std::mem::take(&mut line));
                }
                lines.push(take);
                word = &word[word.char_indices().nth(cols).unwrap().0..];
            }
            let sep = if line.is_empty() { 0 } else { 1 };
            if line.chars().count() + sep + word.chars().count() > cols {
                lines.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
        lines.push(line);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn entry_height(e: &Entry, scale: i32) -> i32 {
    match e {
        Entry::You(img) => LABEL_H + img.h,
        Entry::Pi(text) => LABEL_H + crate::md::height(&crate::md::parse(text), scale, content_w()),
        Entry::Note(text) => {
            wrap(text, wrap_cols(LABEL_SCALE)).len() as i32 * (CHAR_ROWS * LABEL_SCALE + 6)
        }
    }
}

/// Usable content width for pi's rendered blocks.
fn content_w() -> i32 {
    FB_W - 2 * MARGIN
}

pub fn total_height(entries: &[Entry], scale: i32) -> i32 {
    PAD_TOP
        + entries
            .iter()
            .map(|e| entry_height(e, scale) + ENTRY_GAP)
            .sum::<i32>()
}

fn draw_entry(fb: &mut Framebuffer, e: &Entry, y: i32, scale: i32) {
    match e {
        Entry::You(img) => {
            fb.text(MARGIN, y, "you", LABEL_SCALE, GRAY);
            /* right-align the ink and underline it, so it reads as "sent" */
            let x = (FB_W - MARGIN - img.w).max(MARGIN);
            fb.blit_gray(x, y + LABEL_H, img.w, img.h, &img.px);
            fb.fill_rect(MARGIN, y + LABEL_H + img.h + 2, FB_W - 2 * MARGIN, 1, LIGHT);
        }
        Entry::Pi(text) => {
            fb.text(MARGIN, y, "pi", LABEL_SCALE, GRAY);
            let segs = crate::md::parse(text);
            crate::md::draw(fb, MARGIN, y + LABEL_H, content_w(), &segs, scale);
        }
        Entry::Note(text) => {
            let mut ly = y;
            for line in wrap(text, wrap_cols(LABEL_SCALE)) {
                fb.text(MARGIN, ly, &line, LABEL_SCALE, GRAY);
                ly += CHAR_ROWS * LABEL_SCALE + 6;
            }
        }
    }
}

/// Repaint the viewport band [y0, y1) for the given scroll offset (pixels
/// of content hidden above y0). Fills white first, then the visible slice.
pub fn draw(fb: &mut Framebuffer, entries: &[Entry], y0: i32, y1: i32, scroll: i32, scale: i32) {
    fb.set_clip(y0, y1);
    fb.fill_rect(0, y0, FB_W, y1 - y0, WHITE);
    let mut y = y0 + PAD_TOP - scroll;
    for e in entries {
        let h = entry_height(e, scale);
        if y + h >= y0 && y < y1 {
            draw_entry(fb, e, y, scale);
        }
        y += h + ENTRY_GAP;
        if y >= y1 {
            break; /* everything below is off-screen */
        }
    }
    fb.clear_clip();
}
