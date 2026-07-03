//! Drawing primitives on the Framebuffer. Everything clips against the
//! screen bounds, so callers never have to be careful near edges.
//!
//! Coordinates are framebuffer pixels: (0,0) top-left, 1404x1872. Colors
//! are RGB565; on the monochrome e-ink panel only luminance shows, so the
//! useful "palette" is black / white / grays.

use crate::font::glyph;
use crate::qtfb::{Framebuffer, RM2_HEIGHT, RM2_WIDTH};

pub const WHITE: u16 = 0xFFFF;
pub const BLACK: u16 = 0x0000;
pub const GRAY: u16 = 0x8410;

impl Framebuffer {
    pub fn px(&mut self, x: i32, y: i32, c: u16) {
        if (0..RM2_WIDTH).contains(&x) && (0..RM2_HEIGHT).contains(&y) {
            self.pixels()[(y * RM2_WIDTH + x) as usize] = c;
        }
    }

    pub fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, c: u16) {
        for j in y..y + h {
            for i in x..x + w {
                self.px(i, j, c);
            }
        }
    }

    pub fn rect_outline(&mut self, x: i32, y: i32, w: i32, h: i32, t: i32, c: u16) {
        self.fill_rect(x, y, w, t, c); /* top */
        self.fill_rect(x, y + h - t, w, t, c); /* bottom */
        self.fill_rect(x, y, t, h, c); /* left */
        self.fill_rect(x + w - t, y, t, h, c); /* right */
    }

    pub fn disc(&mut self, cx: i32, cy: i32, r: i32, c: u16) {
        for j in -r..=r {
            for i in -r..=r {
                if i * i + j * j <= r * r {
                    self.px(cx + i, cy + j, c);
                }
            }
        }
    }

    /// Draw `s` with the 5x7 font scaled `scale` times; returns the width.
    pub fn text(&mut self, x: i32, y: i32, s: &str, scale: i32, c: u16) -> i32 {
        let mut cx = x;
        for ch in s.chars() {
            let cols = glyph(ch);
            for (col, bits) in cols.iter().enumerate() {
                for row in 0..7 {
                    if (bits >> row) & 1 == 1 {
                        self.fill_rect(
                            cx + col as i32 * scale,
                            y + row * scale,
                            scale,
                            scale,
                            c,
                        );
                    }
                }
            }
            cx += 6 * scale; /* 5 columns + 1 of spacing */
        }
        cx - x
    }
}

pub fn text_width(s: &str, scale: i32) -> i32 {
    s.chars().count() as i32 * 6 * scale
}
