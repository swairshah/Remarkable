//! Toolbar icon glyphs: polyline tables on a -14..14 design grid, stroked
//! into the framebuffer at any size/color. Placeholders until the M6
//! fidelity pass swaps in 1-bit bitmaps cropped from real xochitl
//! screenshots — `draw_icon` isolates that change from every caller.

use crate::fb::Framebuffer;

/// (-128, -128) lifts the pen between segments.
pub const UP: (i8, i8) = (-128, -128);

pub type Icon = &'static [(i8, i8)];

pub const PEN: Icon = &[(-10, 10), (6, -6), (10, -10), (11, -11), UP, (-10, 10), (-12, 12), (-8, 11), UP, (6, -6), (10, -2)];
pub const ERASER: Icon = &[(-11, 6), (-2, -3), (6, 5), (-3, 14), (-11, 6), UP, (-2, -3), (4, -9), (12, -1), (6, 5), UP, (-3, 14), (12, 14)];
pub const LASSO: Icon = &[
    (0, -10), (6, -9), (10, -5), (10, 0), (6, 4), (0, 5), (-6, 4), (-10, 0), (-10, -5), (-6, -9), (0, -10),
    UP, (-4, 5), (-6, 9), (-4, 13), (0, 14),
];
pub const UNDO: Icon = &[(-2, -8), (4, -8), (9, -4), (10, 2), (6, 7), (0, 8), (-6, 7), UP, (-2, -12), (-2, -8), (2, -4), UP, (-2, -12), (-6, -8), (-2, -4)];
pub const REDO: Icon = &[(2, -8), (-4, -8), (-9, -4), (-10, 2), (-6, 7), (0, 8), (6, 7), UP, (2, -12), (2, -8), (-2, -4), UP, (2, -12), (6, -8), (2, -4)];
pub const PREV: Icon = &[(5, -10), (-5, 0), (5, 10)];
pub const NEXT: Icon = &[(-5, -10), (5, 0), (-5, 10)];
pub const HOME: Icon = &[
    (0, -11), (-11, 0), (-8, 0), (-8, 10), (-2, 10), (-2, 3), (2, 3), (2, 10), (8, 10), (8, 0), (11, 0), (0, -11),
];

/// A thick-ish line via stamped discs (works in any color).
fn seg(fb: &mut Framebuffer, x0: f32, y0: f32, x1: f32, y1: f32, t: i32, color: u16) {
    let steps = ((x1 - x0).abs().max((y1 - y0).abs()).ceil() as i32).max(1);
    for i in 0..=steps {
        let f = i as f32 / steps as f32;
        let (cx, cy) = ((x0 + (x1 - x0) * f) as i32, (y0 + (y1 - y0) * f) as i32);
        for j in -t..=t {
            for k in -t..=t {
                if j * j + k * k <= t * t {
                    fb.px(cx + k, cy + j, color);
                }
            }
        }
    }
}

/// Stroke `icon` centered at (cx, cy), scaled so the 28-unit grid spans
/// `size` px.
pub fn draw_icon(fb: &mut Framebuffer, cx: i32, cy: i32, size: i32, icon: Icon, color: u16) {
    let k = size as f32 / 28.0;
    let t = (size / 24).max(1);
    let mut last: Option<(f32, f32)> = None;
    for &(x, y) in icon {
        if (x, y) == UP {
            last = None;
            continue;
        }
        let p = (cx as f32 + x as f32 * k, cy as f32 + y as f32 * k);
        if let Some(a) = last {
            seg(fb, a.0, a.1, p.0, p.1, t, color);
        }
        last = Some(p);
    }
}
