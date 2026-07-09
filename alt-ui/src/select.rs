//! Lasso selection: capture a pen-drawn loop, select the strokes fully
//! inside it (xochitl semantics), show a dashed box with a chip bar, drag
//! to move. E-ink discipline: the trail and the box are cheap DU dashes;
//! nothing drags pixels — a move commits as one vector translate and a
//! model re-render.

use crate::draw::{text_width, BLACK, WHITE};
use crate::fb::{Framebuffer, SCREEN_H, SCREEN_W};
use crate::ink::{stroke_bbox, Owner, Page, Rect};

/* capture */
pub const LASSO_MIN_SEG: f32 = 4.0; /* decimation: min px between points */
pub const LASSO_MIN_PTS: usize = 8;
pub const LASSO_MIN_BOX: i32 = 20;

/// A stroke is selected only when ALL its points fall inside the loop
/// (flip to a fraction threshold here if device testing wants it looser).
pub const LASSO_ALL_POINTS: bool = true;

/* the selection box */
pub const SEL_PAD: i32 = 12;
const DASH_ON: i32 = 8;
const DASH_OFF: i32 = 6;
const DASH_T: i32 = 2;

/* the chip bar */
pub const CHIP_H: i32 = 64;
pub const CHIP_W: i32 = 170;
const CHIP_GAP: i32 = 10;
const CHIPS: [&str; 2] = ["DELETE", "CUT"];

pub enum Chip {
    Delete,
    Cut,
}

/// The pen-drawn loop, while capturing.
pub struct Lasso {
    pub pts: Vec<(f32, f32)>,
}

impl Lasso {
    pub fn new(x: f32, y: f32) -> Self {
        Lasso { pts: vec![(x, y)] }
    }

    /// Append a point if it moved far enough; returns the previous point
    /// when a trail segment should be drawn.
    pub fn extend(&mut self, x: f32, y: f32) -> Option<(f32, f32)> {
        let &(px, py) = self.pts.last().unwrap();
        if (x - px).abs().max((y - py).abs()) < LASSO_MIN_SEG {
            return None;
        }
        self.pts.push((x, y));
        Some((px, py))
    }

    pub fn bbox(&self) -> Rect {
        let mut r = Rect {
            x0: self.pts[0].0 as i32,
            y0: self.pts[0].1 as i32,
            x1: self.pts[0].0 as i32,
            y1: self.pts[0].1 as i32,
        };
        for &(x, y) in &self.pts {
            r.x0 = r.x0.min(x as i32);
            r.y0 = r.y0.min(y as i32);
            r.x1 = r.x1.max(x.ceil() as i32);
            r.y1 = r.y1.max(y.ceil() as i32);
        }
        r
    }

    /// Big enough to mean anything?
    pub fn viable(&self) -> bool {
        let b = self.bbox();
        self.pts.len() >= LASSO_MIN_PTS && (b.w() >= LASSO_MIN_BOX || b.h() >= LASSO_MIN_BOX)
    }
}

/// Even-odd point-in-polygon (the polygon auto-closes).
fn point_in_poly(poly: &[(f32, f32)], x: f32, y: f32) -> bool {
    let mut inside = false;
    let n = poly.len();
    let mut j = n - 1;
    for i in 0..n {
        let (xi, yi) = poly[i];
        let (xj, yj) = poly[j];
        if ((yi > y) != (yj > y)) && (x < (xj - xi) * (y - yi) / (yj - yi) + xi) {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Strokes fully inside the loop, as stable refs + their union bbox.
pub fn select_strokes(page: &Page, poly: &[(f32, f32)]) -> (Vec<(Owner, u64)>, Option<Rect>) {
    let lb = {
        let mut r = Rect { x0: i32::MAX, y0: i32::MAX, x1: i32::MIN, y1: i32::MIN };
        for &(x, y) in poly {
            r.x0 = r.x0.min(x as i32);
            r.y0 = r.y0.min(y as i32);
            r.x1 = r.x1.max(x.ceil() as i32);
            r.y1 = r.y1.max(y.ceil() as i32);
        }
        r
    };
    let mut refs = Vec::new();
    let mut bbox: Option<Rect> = None;
    let mut test = |owner: Owner, s: &crate::ink::Stroke| {
        let Some(b) = stroke_bbox(s) else { return };
        /* fast reject: the stroke's bbox must sit inside the loop's bbox */
        if b.x0 < lb.x0 || b.x1 > lb.x1 || b.y0 < lb.y0 || b.y1 > lb.y1 {
            return;
        }
        let inside = if LASSO_ALL_POINTS {
            s.pts.iter().all(|p| point_in_poly(poly, p.x, p.y))
        } else {
            let hits = s.pts.iter().filter(|p| point_in_poly(poly, p.x, p.y)).count();
            hits * 2 > s.pts.len()
        };
        if inside {
            refs.push((owner, s.id));
            bbox = Some(bbox.map_or(b, |a| a.union(b)));
        }
    };
    for s in &page.strokes {
        test(Owner::User, s);
    }
    for p in &page.patches {
        for s in &p.strokes {
            test(Owner::Patch(p.id), s);
        }
    }
    (refs, bbox)
}

/// An active selection: refs + the box, with a live drag offset.
pub struct Selection {
    pub refs: Vec<(Owner, u64)>,
    pub bbox: Rect, /* committed position (no live offset) */
    pub dx: i32,
    pub dy: i32,
    pub drag_from: Option<(i32, i32)>,
}

impl Selection {
    pub fn new(refs: Vec<(Owner, u64)>, bbox: Rect) -> Self {
        Selection { refs, bbox, dx: 0, dy: 0, drag_from: None }
    }

    /// The dashed ring's rect at the current drag offset.
    pub fn ring(&self) -> Rect {
        Rect {
            x0: self.bbox.x0 + self.dx,
            y0: self.bbox.y0 + self.dy,
            x1: self.bbox.x1 + self.dx,
            y1: self.bbox.y1 + self.dy,
        }
        .pad(SEL_PAD)
    }

    /// Clamp a proposed drag offset so the ring stays on screen.
    pub fn clamp_offset(&self, dx: i32, dy: i32) -> (i32, i32) {
        let r = self.bbox.pad(SEL_PAD);
        (
            dx.clamp(-r.x0, SCREEN_W - 1 - r.x1),
            dy.clamp(-r.y0, SCREEN_H - 1 - r.y1),
        )
    }

    /// The chip bar rect: above the ring, below it when clamped at top.
    pub fn chips_rect(&self) -> Rect {
        let r = self.ring();
        let w = CHIPS.len() as i32 * CHIP_W + (CHIPS.len() as i32 - 1) * CHIP_GAP;
        let x0 = ((r.x0 + r.x1) / 2 - w / 2).clamp(4, SCREEN_W - w - 4);
        let y0 = if r.y0 - CHIP_H - 14 >= 0 { r.y0 - CHIP_H - 14 } else { r.y1 + 14 };
        Rect { x0, y0, x1: x0 + w - 1, y1: y0 + CHIP_H - 1 }
    }

    /// Everything the selection paints (ring strips + chips), for repaint
    /// and dismissal.
    pub fn chrome_rect(&self) -> Rect {
        self.ring().pad(DASH_T + 2).union(self.chips_rect().pad(4))
    }

    pub fn chip_at(&self, x: i32, y: i32) -> Option<Chip> {
        let c = self.chips_rect();
        if y < c.y0 || y > c.y1 {
            return None;
        }
        let mut bx = c.x0;
        for (i, _) in CHIPS.iter().enumerate() {
            if x >= bx && x < bx + CHIP_W {
                return match i {
                    0 => Some(Chip::Delete),
                    _ => Some(Chip::Cut),
                };
            }
            bx += CHIP_W + CHIP_GAP;
        }
        None
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        let r = self.ring();
        x >= r.x0 && x <= r.x1 && y >= r.y0 && y <= r.y1
    }
}

/* ---- drawing ---------------------------------------------------------------- */

fn dash_h(fb: &mut Framebuffer, x0: i32, x1: i32, y: i32) {
    let mut x = x0;
    while x < x1 {
        let seg = (x + DASH_ON).min(x1);
        fb.fill_rect(x, y, seg - x, DASH_T, BLACK);
        x += DASH_ON + DASH_OFF;
    }
}

fn dash_v(fb: &mut Framebuffer, y0: i32, y1: i32, x: i32) {
    let mut y = y0;
    while y < y1 {
        let seg = (y + DASH_ON).min(y1);
        fb.fill_rect(x, y, DASH_T, seg - y, BLACK);
        y += DASH_ON + DASH_OFF;
    }
}

/// The dashed selection box + chip bar.
pub fn draw_selection(fb: &mut Framebuffer, sel: &Selection) {
    let r = sel.ring().clamp_screen();
    dash_h(fb, r.x0, r.x1, r.y0);
    dash_h(fb, r.x0, r.x1, r.y1 - DASH_T + 1);
    dash_v(fb, r.y0, r.y1, r.x0);
    dash_v(fb, r.y0, r.y1, r.x1 - DASH_T + 1);

    let c = sel.chips_rect();
    let mut bx = c.x0;
    for label in CHIPS {
        fb.fill_rect(bx, c.y0, CHIP_W, CHIP_H, WHITE);
        fb.rect_outline(bx, c.y0, CHIP_W, CHIP_H, 2, BLACK);
        fb.text(
            bx + (CHIP_W - text_width(label, 2)) / 2,
            c.y0 + (CHIP_H - 14) / 2,
            label,
            2,
            BLACK,
        );
        bx += CHIP_W + CHIP_GAP;
    }
}

/// One dashed trail segment while the lasso is being drawn (cheap DU dots).
pub fn draw_trail_segment(fb: &mut Framebuffer, from: (f32, f32), to: (f32, f32)) -> Rect {
    let steps = ((to.0 - from.0).abs().max((to.1 - from.1).abs()) / 3.0).ceil().max(1.0) as i32;
    for i in 0..=steps {
        if (i / 2) % 2 == 1 {
            continue; /* the gaps in the dash */
        }
        let t = i as f32 / steps as f32;
        let (x, y) = (from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t);
        fb.fill_rect(x as i32, y as i32, 2, 2, BLACK);
    }
    Rect {
        x0: from.0.min(to.0) as i32 - 2,
        y0: from.1.min(to.1) as i32 - 2,
        x1: from.0.max(to.0).ceil() as i32 + 2,
        y1: from.1.max(to.1).ceil() as i32 + 2,
    }
}
