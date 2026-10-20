//! The highlighter nib: a wide, light-grey band laid under the words.
//!
//! It needs no new page format. A highlight is an ordinary user stroke
//! with a pale `gray` and a fat radius, so libreink-page's darkest-wins
//! stamping puts the band UNDER the print and under black ink by
//! construction — the text stays readable, and erase / lasso / undo /
//! sync / thumbnails all keep working with no special case.
//!
//! Two rules make it read like a real marker on e-ink:
//!
//!   - the band is always level. The y is fixed at press time, so a
//!     sweep across a line cannot wobble.
//!   - on a book page it SNAPS to the line under the nib, using the word
//!     boxes mkbook already extracts (doc.rs `words`), so the band covers
//!     the line's true ascender-to-descender extent.
//!
//! The toolbar cell is drawn here too: libreink's EdgeToolbar owns a
//! fixed `Feature` set, so papier paints the highlighter as a 14th cell
//! flush under the strip, matching its width and cell pitch.

use crate::doc::{Doc, Entry, Word};
use crate::draw::{text_width, BLACK, GRAY, WHITE};
use crate::fb::{Framebuffer, SCREEN_H};
use crate::ink::Rect;

/// The band's grey. Pale enough to leave print and ink legible through
/// it, dark enough to survive the 16-level waveform.
pub const HL_GRAY: u8 = 186;

/// Half-height of a free-standing band (notebook pages, or a sweep that
/// lands nowhere near a line of print).
pub const HL_R: f32 = 13.0;

/// Strokes at or above this grey are highlights, not ink. Well clear of
/// pi's own mid-grey (`ink::AI_GRAY`).
pub const HL_MIN_GRAY: u8 = 150;

/// Minimum pen travel before the band records another point.
pub const HL_STEP: f32 = 4.0;

pub fn is_highlight(gray: u8) -> bool {
    gray >= HL_MIN_GRAY
}

/// What the pen draws with while the Pen tool is armed.
#[derive(Clone, Copy, PartialEq)]
pub enum Nib {
    Pen,
    Highlighter,
}

impl Nib {
    pub fn key(self) -> &'static str {
        match self {
            Nib::Pen => "pen",
            Nib::Highlighter => "highlighter",
        }
    }

    pub fn from_key(k: &str) -> Nib {
        match k {
            "highlighter" => Nib::Highlighter,
            _ => Nib::Pen,
        }
    }

    pub fn toggled(self) -> Nib {
        match self {
            Nib::Pen => Nib::Highlighter,
            Nib::Highlighter => Nib::Pen,
        }
    }
}

/* ---- the band ------------------------------------------------------------- */

/// The word boxes of the entry on screen (empty on note pages).
pub fn page_words(doc: &Doc) -> Vec<Word> {
    match doc.entry(doc.current) {
        Some(Entry::Pdf(p)) => doc.words(p),
        _ => Vec::new(),
    }
}

/// The band (center y, half-height) for a press at `y`: the covered line
/// of print when there is one, else a fixed-height band at the nib.
pub fn band_for(words: &[Word], y: i32) -> (f32, f32) {
    snap(words, y).unwrap_or((y as f32, HL_R))
}

/// The line of print under `y`, as (center, half-height). None when the
/// page has no words or the nib is too far from any of them.
fn snap(words: &[Word], y: i32) -> Option<(f32, f32)> {
    let near = words
        .iter()
        .filter(|w| w.y1 > w.y0)
        .min_by_key(|w| (center(w) - y).abs())?;
    let h = near.y1 - near.y0;
    if (center(near) - y).abs() > h.max(20) {
        return None;
    }
    let tol = (h / 2).max(6);
    let line: Vec<&Word> = words
        .iter()
        .filter(|w| w.y1 > w.y0 && (center(w) - center(near)).abs() <= tol)
        .collect();
    let y0 = line.iter().map(|w| w.y0).min()? as f32;
    let y1 = line.iter().map(|w| w.y1).max()? as f32;
    Some(((y0 + y1) / 2.0, ((y1 - y0) / 2.0 + 3.0).clamp(8.0, 26.0)))
}

fn center(w: &Word) -> i32 {
    (w.y0 + w.y1) / 2
}

/* ---- the toolbar cell ----------------------------------------------------- */

/// The strip's cell pitch (libreink EdgeToolbar); the highlighter cell
/// abuts the last feature and shares its width.
pub const CELL_H: i32 = 104;

pub fn cell(strip: Rect) -> Rect {
    Rect {
        x0: strip.x0,
        y0: strip.y1 + 1,
        x1: strip.x1,
        y1: strip.y1 + CELL_H,
    }
}

/// False when the strip is long enough that a 14th cell would run off the
/// panel — the nib is then reachable only by re-tapping the armed Pen.
pub fn fits(strip: Rect) -> bool {
    cell(strip).y1 < SCREEN_H
}

pub fn hit(strip: Rect, x: i32, y: i32) -> bool {
    let c = cell(strip);
    x >= c.x0 && x <= c.x1 && y >= c.y0 && y <= c.y1
}

fn gray565(g: u8) -> u16 {
    let g = g as u16;
    ((g >> 3) << 11) | ((g >> 2) << 5) | (g >> 3)
}

/// "HL" over a swatch of the real band grey on its baseline rule; armed
/// takes an inset black frame.
pub fn draw(fb: &mut Framebuffer, strip: Rect, armed: bool) {
    let c = cell(strip);
    let (cx, cy) = ((c.x0 + c.x1) / 2, (c.y0 + c.y1) / 2);
    fb.fill_rect(c.x0, c.y0, c.w(), c.h(), WHITE);
    fb.fill_rect(c.x0 + 16, c.y0, c.w() - 32, 2, GRAY);
    fb.text(cx - text_width("HL", 2) / 2, cy - 28, "HL", 2, BLACK);
    fb.fill_rect(cx - 30, cy + 2, 60, 20, gray565(HL_GRAY));
    fb.fill_rect(cx - 30, cy + 24, 60, 3, BLACK);
    if armed {
        fb.rect_outline(c.x0 + 8, c.y0 + 8, c.w() - 16, c.h() - 16, 3, BLACK);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(y0: i32, y1: i32) -> Word {
        Word { x0: 100, y0, x1: 200, y1, text: "x".into() }
    }

    #[test]
    fn no_words_keeps_the_nib_height() {
        assert_eq!(band_for(&[], 500), (500.0, HL_R));
    }

    #[test]
    fn snaps_to_the_line_under_the_nib() {
        let words = vec![word(400, 440), word(600, 640)];
        let (cy, r) = band_for(&words, 615);
        assert_eq!(cy, 620.0);
        assert_eq!(r, 23.0);
    }

    #[test]
    fn a_line_is_the_union_of_its_words() {
        /* a descender on the same line widens the band */
        let words = vec![word(600, 640), word(604, 652)];
        let (cy, r) = band_for(&words, 620);
        assert_eq!(cy, 626.0);
        assert_eq!(r, 26.0);
    }

    #[test]
    fn far_from_any_line_does_not_snap() {
        let words = vec![word(400, 440)];
        assert_eq!(band_for(&words, 1200), (1200.0, HL_R));
    }

    #[test]
    fn highlight_grey_is_clear_of_ink_and_pi() {
        assert!(is_highlight(HL_GRAY));
        assert!(!is_highlight(crate::ink::AI_GRAY));
        assert!(!is_highlight(crate::ink::USER_GRAY));
    }
}
