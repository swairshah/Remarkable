//! The reMarkable look: white paper, true black, one mid gray. Sizes are
//! papier's tablet constants scaled to the laptop (tablet px x 0.55 ≈ the
//! same physical size on the Framework's 1.5x panel).

use std::sync::atomic::{AtomicBool, Ordering};

use gpui::{Rgba, rgb, rgba};

pub const WHITE: u32 = 0xffffff;
pub const BLACK: u32 = 0x000000;
/// Readout / meta gray (the tablet's GRAY).
pub const GRAY: u32 = 0x8a8a8a;
/// Folder fills, disabled glyphs.
pub const LIGHT: u32 = 0xe4e4e4;
/// Press/hover wash on white chrome.
pub const WASH: u32 = 0xf0f0f0;
/// Around the page when the window is wider than the paper.
pub const DESK: u32 = 0xe9e8e4;
pub const PAGE_EDGE: u32 = 0xc9c7c1;

pub const SERIF: &str = "EB Garamond";
pub const MONO: &str = "Google Sans Code";

// ---- dark mode: "the same paper by lamplight" (after sketchpad): warm
// charcoal where the tablet is white, cream where it is black. Every color
// in the app goes through `c`/`ca`, so the light palette above is the one
// source of truth and dark is a mapping of it.

static DARK: AtomicBool = AtomicBool::new(false);

pub fn dark() -> bool {
    DARK.load(Ordering::Relaxed)
}

pub fn set_dark(on: bool) {
    DARK.store(on, Ordering::Relaxed);
}

/// Cream ink on dark paper.
pub const CREAM: u32 = 0xe6dfd2;
/// pi's blue, lifted to read on charcoal.
pub const PI_BLUE_DARK: u32 = 0x7aa8e6;

fn map(v: u32) -> u32 {
    if !dark() {
        return v;
    }
    match v {
        WHITE => 0x1e1c19,
        BLACK | 0x1a1a1a => CREAM,
        GRAY => 0x8f8678,
        LIGHT => 0x3d3831,
        WASH => 0x2c2925,
        DESK => 0x141210,
        PAGE_EDGE => 0x34302a,
        0x333333 => 0xc8c0b2,
        0x555555 | 0x696969 => 0xa89f91,
        0x777777 => 0x8f8678,
        0x9a9a9a => 0x6a6258,
        crate::ink::PI_BLUE => PI_BLUE_DARK,
        0xfaf8f2 => 0x1e1c19,
        0xefede6 => 0x1a1816,
        other => other,
    }
}

pub fn c(v: u32) -> Rgba {
    rgb(map(v))
}

pub fn ca(v: u32, a: f32) -> Rgba {
    rgba((map(v) << 8) | (a.clamp(0.0, 1.0) * 255.0) as u32)
}

// ---- metrics (logical px)
pub const STATUS_H: f32 = 30.0;
pub const HEADER_H: f32 = 78.0;
pub const SIDE_PAD: f32 = 26.0;
pub const BTN_H: f32 = 34.0;
pub const CELL_W: f32 = 212.0;
pub const THUMB_W: f32 = 150.0;
pub const THUMB_H: f32 = 200.0;
pub const STRIP_W: f32 = 56.0;
pub const TOOL_BTN: f32 = 44.0;
pub const DLG_W: f32 = 440.0;
pub const DLG_ROW_H: f32 = 52.0;

/// Paper tones: real paper isn't #FFFFFF. Ink stays true black; book
/// rasters are multiplied onto the tone so books and notebooks match.
pub fn paper(tone: &str) -> [u8; 3] {
    match tone {
        "white" => [255, 255, 255],
        "soft" => [239, 237, 230],
        _ => [250, 248, 242],
    }
}

/// The page's paper as it shows (dark mode: the charcoal tone).
pub fn paper_u32(tone: &str) -> u32 {
    let [r, g, b] = if dark() { dark_paper(tone) } else { paper(tone) };
    ((r as u32) << 16) | ((g as u32) << 8) | b as u32
}

/// Dark-mode paper for each tone.
pub fn dark_paper(tone: &str) -> [u8; 3] {
    match tone {
        "white" => [0x26, 0x24, 0x21],
        "soft" => [0x19, 0x17, 0x15],
        _ => [0x1e, 0x1c, 0x19],
    }
}
