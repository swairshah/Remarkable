//! Drawing primitives on the Framebuffer (evolved from ../sample-app-rust).
//!
//! Two additions for a scrolling chat UI:
//!  - a vertical CLIP range, so re-rendering the conversation viewport can
//!    never bleed into the header or the input strip;
//!  - fill_rect writes rows via slices instead of per-pixel calls, because
//!    the viewport (1404x1300) is redrawn on every scroll/typing tick.

use crate::font::{glyph, ADVANCE, CHAR_ROWS};
use crate::qtfb::{Framebuffer, RM2_HEIGHT, RM2_WIDTH};

pub const WHITE: u16 = 0xFFFF;
pub const BLACK: u16 = 0x0000;
pub const GRAY: u16 = 0x8410;
pub const LIGHT: u16 = 0xC618; /* light gray, for subtle borders */

impl Framebuffer {
    /// Restrict all drawing to rows y0..y1 (screen stays the x bound).
    pub fn set_clip(&mut self, y0: i32, y1: i32) {
        self.clip_y0 = y0.max(0);
        self.clip_y1 = y1.min(RM2_HEIGHT);
    }

    pub fn clear_clip(&mut self) {
        self.clip_y0 = 0;
        self.clip_y1 = RM2_HEIGHT;
    }

    pub fn px(&mut self, x: i32, y: i32, c: u16) {
        if (0..RM2_WIDTH).contains(&x) && y >= self.clip_y0 && y < self.clip_y1 {
            self.pixels()[(y * RM2_WIDTH + x) as usize] = c;
        }
    }

    pub fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, c: u16) {
        let x0 = x.max(0);
        let x1 = (x + w).min(RM2_WIDTH);
        let y0 = y.max(self.clip_y0);
        let y1 = (y + h).min(self.clip_y1);
        if x0 >= x1 {
            return;
        }
        let px = self.pixels();
        for row in y0..y1 {
            let base = (row * RM2_WIDTH) as usize;
            px[base + x0 as usize..base + x1 as usize].fill(c);
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

    /// Draw `s` (single line, no wrapping) scaled `scale`x; returns width.
    pub fn text(&mut self, x: i32, y: i32, s: &str, scale: i32, c: u16) -> i32 {
        let mut cx = x;
        for ch in s.chars() {
            /* skip glyphs entirely outside the clip band — cheap and makes
             * drawing a partially visible line of the log painless */
            if y < self.clip_y1 && y + CHAR_ROWS * scale > self.clip_y0 {
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
            }
            cx += ADVANCE * scale;
        }
        cx - x
    }

    /// Blend an 8-bit coverage bitmap as BLACK over the current contents
    /// (antialiased glyphs from text.rs). Coverage 255 = solid black, 0 =
    /// untouched; partial coverage darkens whatever background is there, so
    /// text over the white page and over gray code boxes both look right.
    pub fn blend_black(&mut self, x: i32, y: i32, w: i32, h: i32, cov: &[u8]) {
        let (cy0, cy1) = (self.clip_y0, self.clip_y1);
        let px = self.pixels();
        for row in 0..h {
            let py = y + row;
            if py < cy0 || py >= cy1 {
                continue;
            }
            for col in 0..w {
                let a = cov[(row * w + col) as usize] as u32;
                if a == 0 {
                    continue;
                }
                let sx = x + col;
                if sx < 0 || sx >= RM2_WIDTH {
                    continue;
                }
                let idx = (py * RM2_WIDTH + sx) as usize;
                /* background luminance from the green channel, then darken */
                let bg = (((px[idx] >> 5) & 0x3F) as u32) * 255 / 63;
                let g = (bg * (255 - a) / 255) as u16;
                px[idx] = ((g >> 3) << 11) | ((g >> 2) << 5) | (g >> 3);
            }
        }
    }

    /// Blit an 8-bit grayscale image (the stored ink of a sent message).
    pub fn blit_gray(&mut self, x: i32, y: i32, w: i32, h: i32, gray: &[u8]) {
        for j in 0..h {
            let sy = y + j;
            if sy < self.clip_y0 || sy >= self.clip_y1 {
                continue;
            }
            for i in 0..w {
                let g = gray[(j * w + i) as usize] as u16;
                /* gray8 -> RGB565 */
                let c = ((g >> 3) << 11) | ((g >> 2) << 5) | (g >> 3);
                self.px(x + i, sy, c);
            }
        }
    }
}

pub fn text_width(s: &str, scale: i32) -> i32 {
    s.chars().count() as i32 * ADVANCE * scale
}
