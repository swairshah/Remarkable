//! Ink: variable-width stroke outlines, live stroke capture, the page
//! raster (paper × book print × committed ink in one opaque image, updated
//! by dirty rectangle), the splitting eraser and snapshot undo.
//!
//! Why a raster: a heavily annotated reMarkable page can carry thousands of
//! strokes. Painting each as a GPU path every frame would make the pen lag;
//! instead committed ink lives in one texture and only the stroke under the
//! nib is a live path. Frames stay at one image + one path while drawing.

use std::sync::Arc;

use gpui::RenderImage;
use image::{Frame, RgbaImage};
use tiny_skia::{BlendMode, Color, FillRule, FilterQuality, Paint, PathBuilder, Pixmap, PixmapPaint, Transform};

use crate::model::{InkPage, InkPoint, InkStroke};

pub const PI_BLUE: u32 = 0x1d48a8;

pub fn ink_rgb(gray: i64) -> u32 {
    if gray <= 0 {
        0x1a1a1a
    } else {
        let g = gray.clamp(0, 255) as u32;
        (g << 16) | (g << 8) | g
    }
}

// ---------------------------------------------------------------- outline

fn catmull(p0: (f32, f32), p1: (f32, f32), p2: (f32, f32), p3: (f32, f32), t: f32) -> (f32, f32) {
    let t2 = t * t;
    let t3 = t2 * t;
    let f = |a: f32, b: f32, c: f32, d: f32| {
        0.5 * ((2.0 * b) + (-a + c) * t + (2.0 * a - 5.0 * b + 4.0 * c - d) * t2 + (-a + 3.0 * b - 3.0 * c + d) * t3)
    };
    (f(p0.0, p1.0, p2.0, p3.0), f(p0.1, p1.1, p2.1, p3.1))
}

fn cap(poly: &mut Vec<(f32, f32)>, cx: f32, cy: f32, r: f32, from: f32) {
    let steps = 8;
    for i in 1..steps {
        let a = from - std::f32::consts::PI * (i as f32 / steps as f32);
        poly.push((cx + r * a.cos(), cy + r * a.sin()));
    }
}

pub fn circle(cx: f32, cy: f32, r: f32, steps: usize) -> Vec<(f32, f32)> {
    (0..steps)
        .map(|i| {
            let a = std::f32::consts::TAU * i as f32 / steps as f32;
            (cx + r * a.cos(), cy + r * a.sin())
        })
        .collect()
}

/// Closed outline polygon (page units) of a variable-width stroke, ready
/// to fill with the non-zero rule. Sparse input is Catmull-Rom resampled;
/// dense tablet ink passes through untouched.
pub fn outline(points: &[InkPoint], min_r: f32) -> Vec<(f32, f32)> {
    let n = points.len();
    if n == 0 {
        return Vec::new();
    }
    if n == 1 {
        let p = points[0];
        return circle(p.x, p.y, p.r.max(min_r), 14);
    }
    let get = |i: isize| {
        let p = points[i.clamp(0, n as isize - 1) as usize];
        (p.x, p.y)
    };
    let mut pts: Vec<(f32, f32, f32)> = Vec::with_capacity(n * 2);
    for i in 0..n - 1 {
        let (a, b) = (points[i], points[i + 1]);
        let steps = ((a.x - b.x).hypot(a.y - b.y) / 3.0).ceil().clamp(1.0, 6.0) as usize;
        for s in 0..steps {
            let t = s as f32 / steps as f32;
            let (x, y) = if steps == 1 { (a.x, a.y) } else { catmull(get(i as isize - 1), get(i as isize), get(i as isize + 1), get(i as isize + 2), t) };
            pts.push((x, y, a.r + (b.r - a.r) * t));
        }
    }
    let last = points[n - 1];
    pts.push((last.x, last.y, last.r));

    let m = pts.len();
    let mut left = Vec::with_capacity(m);
    let mut right = Vec::with_capacity(m);
    let mut normals = Vec::with_capacity(m);
    let mut normal = (0.0f32, -1.0f32);
    for j in 0..m {
        let a = pts[j.saturating_sub(1)];
        let b = pts[(j + 1).min(m - 1)];
        let (tx, ty) = (b.0 - a.0, b.1 - a.1);
        let len = tx.hypot(ty);
        if len > 1e-4 {
            normal = (-ty / len, tx / len);
        }
        normals.push(normal);
        let r = pts[j].2.max(min_r);
        left.push((pts[j].0 + normal.0 * r, pts[j].1 + normal.1 * r));
        right.push((pts[j].0 - normal.0 * r, pts[j].1 - normal.1 * r));
    }
    let mut poly = Vec::with_capacity(m * 2 + 16);
    poly.extend_from_slice(&left);
    let (ex, ey, er) = pts[m - 1];
    cap(&mut poly, ex, ey, er.max(min_r), normals[m - 1].1.atan2(normals[m - 1].0));
    poly.extend(right.iter().rev());
    let (sx, sy, sr) = pts[0];
    cap(&mut poly, sx, sy, sr.max(min_r), normals[0].1.atan2(normals[0].0) + std::f32::consts::PI);
    poly
}

// ------------------------------------------------------------- live stroke

/// Pen widths, as multipliers of the reMarkable pen's 1.7–3.1 page-px range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Width {
    Fine,
    Medium,
    Bold,
}

impl Width {
    pub const ALL: [Width; 3] = [Width::Fine, Width::Medium, Width::Bold];
    pub fn factor(self) -> f32 {
        match self {
            Width::Fine => 0.75,
            Width::Medium => 1.0,
            Width::Bold => 1.7,
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            Width::Fine => "fine",
            Width::Medium => "medium",
            Width::Bold => "bold",
        }
    }
    pub fn from_key(k: &str) -> Width {
        Self::ALL.into_iter().find(|w| w.key() == k).unwrap_or(Width::Medium)
    }
}

/// Half-width in page units: the reMarkable pen's 1.7–3.1 range, scaled.
pub fn radius_for(pressure: f32, width: Width) -> f32 {
    let p = pressure.clamp(0.05, 1.0);
    2.4 * (0.55 + 0.75 * p) * width.factor()
}

/// The stroke under the nib, in page units. Samples are lightly low-pass
/// filtered (digitizer jitter) with a distance gate scaled to the display.
pub struct Live {
    pub points: Vec<InkPoint>,
    pressure: f32,
    width: Width,
}

impl Live {
    pub fn new(x: f32, y: f32, pressure: f32, width: Width) -> Live {
        Live { points: vec![InkPoint { x, y, r: radius_for(pressure, width) }], pressure, width }
    }

    pub fn push(&mut self, x: f32, y: f32, pressure: f32, scale: f32) {
        let last = *self.points.last().unwrap();
        if (x - last.x).hypot(y - last.y) * scale < 0.7 {
            return;
        }
        let k = 0.55;
        self.pressure += (pressure - self.pressure) * 0.35;
        self.points.push(InkPoint { x: last.x + (x - last.x) * k, y: last.y + (y - last.y) * k, r: radius_for(self.pressure, self.width) });
    }

    /// Finalize: a gentle taper at both ends so strokes read as ink.
    pub fn finish(mut self, id: u64) -> InkStroke {
        let n = self.points.len();
        if n > 6 {
            let taper = 4.min(n / 3);
            for i in 0..taper {
                let f = 0.55 + 0.45 * (i as f32 / taper as f32);
                self.points[i].r *= f;
                self.points[n - 1 - i].r *= f;
            }
        }
        InkStroke { id, gray: 0, points: self.points }
    }
}

// ------------------------------------------------------------ page raster

#[derive(Clone, Copy, Debug)]
pub struct IRect {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

impl IRect {
    fn union(self, o: IRect) -> IRect {
        IRect { x0: self.x0.min(o.x0), y0: self.y0.min(o.y0), x1: self.x1.max(o.x1), y1: self.y1.max(o.y1) }
    }
    fn clamp(self, w: i32, h: i32) -> Option<IRect> {
        let r = IRect { x0: self.x0.max(0), y0: self.y0.max(0), x1: self.x1.min(w), y1: self.y1.min(h) };
        (r.x1 > r.x0 && r.y1 > r.y0).then_some(r)
    }
}

/// One page, composited: paper tone × book print, then every committed
/// stroke (user ink black, pi ink blue). Owns the GPU image it last made.
pub struct PageRaster {
    pub w: u32,
    pub h: u32,
    /// Raster pixels per page unit.
    pub scale: f32,
    pixmap: Pixmap,
    bgra: Vec<u8>,
    paper: [u8; 3],
    print: Option<Arc<Print>>,
    dark: Option<[u8; 3]>,
    pub image: Option<Arc<RenderImage>>,
    dirty: Option<IRect>,
}

/// One committed mark to raster: ink, or a highlighter wash (multiplied
/// onto the print so the text underneath stays legible).
#[derive(Clone, Copy)]
pub struct Mark<'a> {
    pub stroke: &'a InkStroke,
    pub rgb: u32,
    pub wash: bool,
}

/// A mark's color on this raster: dark mode turns black ink cream and
/// lifts pi's blue; highlighter washes lighten the charcoal instead of
/// darkening white.
fn shade(rgb: u32, wash: bool, dark: bool) -> (u32, BlendMode) {
    match (dark, wash) {
        (false, false) => (rgb, BlendMode::SourceOver),
        (false, true) => (rgb, BlendMode::Darken),
        (true, true) => (0x4d473f, BlendMode::Lighten),
        (true, false) if rgb == PI_BLUE => (crate::theme::PI_BLUE_DARK, BlendMode::SourceOver),
        (true, false) => {
            // gray g on white -> the same contrast, cream on charcoal
            let g = (rgb & 0xff) as f32 / 255.0;
            let k = 1.0 - (g - 0x1a as f32 / 255.0).max(0.0) / (1.0 - 0x1a as f32 / 255.0);
            let lerp = |a: u32, b: u32| ((a as f32) + (b as f32 - a as f32) * k) as u32;
            let (bg, fg) = (0x1e1c19u32, crate::theme::CREAM);
            let ch = |sh: u32| lerp((bg >> sh) & 0xff, (fg >> sh) & 0xff) << sh;
            (ch(16) | ch(8) | ch(0), BlendMode::SourceOver)
        }
    }
}

/// Paper and print, mapped to lamplight: lightness becomes charcoal →
/// cream (a reader's night mode), neutral so prints stay legible.
fn to_dark(tile: &mut Pixmap, light_paper: [u8; 3], dark_paper: [u8; 3], picture: &dyn Fn(u32, u32) -> bool) {
    let luma = |r: f32, g: f32, b: f32| 0.299 * r + 0.587 * g + 0.114 * b;
    let pl = luma(light_paper[0] as f32, light_paper[1] as f32, light_paper[2] as f32).max(1.0);
    let cream = [(crate::theme::CREAM >> 16) as u8 as f32, (crate::theme::CREAM >> 8) as u8 as f32, crate::theme::CREAM as u8 as f32];
    let tw = tile.width();
    for (i, px) in tile.data_mut().chunks_exact_mut(4).enumerate() {
        let (tx, ty) = (i as u32 % tw, i as u32 / tw);
        if picture(tx, ty) {
            // pictures keep their look, just dimmed so they don't glare
            for c in &mut px[..3] {
                *c = (*c as f32 * 0.82) as u8;
            }
            continue;
        }
        let d = (1.0 - luma(px[0] as f32, px[1] as f32, px[2] as f32) / pl).clamp(0.0, 1.0);
        // ease the midtones so text stays crisp but not glaring
        let d = d.powf(0.9) * 0.92;
        for c in 0..3 {
            px[c] = (dark_paper[c] as f32 + (cream[c] - dark_paper[c] as f32) * d) as u8;
        }
        px[3] = 255;
    }
}

fn fill_poly(pixmap: &mut Pixmap, poly: &[(f32, f32)], ts: Transform, rgb: u32, wash: bool, dark: bool) {
    if poly.len() < 3 {
        return;
    }
    let mut pb = PathBuilder::new();
    pb.move_to(poly[0].0, poly[0].1);
    for &(x, y) in &poly[1..] {
        pb.line_to(x, y);
    }
    pb.close();
    let Some(path) = pb.finish() else { return };
    let (rgb, blend) = shade(rgb, wash, dark);
    let mut paint = Paint::default();
    paint.set_color(Color::from_rgba8((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8, 255));
    paint.anti_alias = true;
    paint.blend_mode = blend;
    pixmap.fill_path(&path, &paint, FillRule::Winding, ts, None);
}

impl PageRaster {
    /// `dark`: the charcoal paper to map onto, in dark mode.
    pub fn new(w: u32, h: u32, scale: f32, paper: [u8; 3], print: Option<Arc<Print>>, dark: Option<[u8; 3]>) -> Option<PageRaster> {
        let pixmap = Pixmap::new(w.max(1), h.max(1))?;
        Some(PageRaster { w, h, scale, bgra: vec![0; (w * h * 4) as usize], pixmap, paper, print, dark, image: None, dirty: None })
    }

    pub fn same_setup(&self, w: u32, h: u32, paper: [u8; 3], print: &Option<Arc<Print>>, dark: Option<[u8; 3]>) -> bool {
        self.w == w
            && self.h == h
            && self.paper == paper
            && self.dark == dark
            && match (&self.print, print) {
                (None, None) => true,
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                _ => false,
            }
    }

    fn page_rect(&self, b: [f32; 4]) -> IRect {
        let s = self.scale;
        IRect { x0: (b[0] * s).floor() as i32 - 2, y0: (b[1] * s).floor() as i32 - 2, x1: (b[2] * s).ceil() as i32 + 2, y1: (b[3] * s).ceil() as i32 + 2 }
    }

    fn mark(&mut self, r: IRect) {
        self.dirty = Some(self.dirty.map_or(r, |d| d.union(r)));
    }

    /// Redraw everything: background then all strokes.
    pub fn render_all<'a>(&mut self, marks: impl Iterator<Item = Mark<'a>>) {
        let all = IRect { x0: 0, y0: 0, x1: self.w as i32, y1: self.h as i32 };
        self.render_rect(all, marks);
    }

    /// Redraw a page-unit rectangle (eraser, undo, pi patch changes).
    pub fn render_page_rect<'a>(&mut self, b: [f32; 4], marks: impl Iterator<Item = Mark<'a>>) {
        let r = self.page_rect(b);
        self.render_rect(r, marks);
    }

    fn render_rect<'a>(&mut self, r: IRect, marks: impl Iterator<Item = Mark<'a>>) {
        let Some(r) = r.clamp(self.w as i32, self.h as i32) else { return };
        let (rw, rh) = ((r.x1 - r.x0) as u32, (r.y1 - r.y0) as u32);
        let Some(mut tile) = Pixmap::new(rw, rh) else { return };
        let [pr, pg, pb] = self.paper;
        tile.fill(Color::from_rgba8(pr, pg, pb, 255));
        let shift = Transform::from_translate(-r.x0 as f32, -r.y0 as f32);
        if let Some(print) = &self.print {
            let sx = self.w as f32 / print.pixmap.width() as f32;
            let sy = self.h as f32 / print.pixmap.height() as f32;
            let quality = if sx < 0.5 { FilterQuality::Bilinear } else { FilterQuality::Bicubic };
            tile.draw_pixmap(
                0,
                0,
                print.pixmap.as_ref(),
                &PixmapPaint { opacity: 1.0, blend_mode: BlendMode::Multiply, quality },
                shift.pre_scale(sx, sy),
                None,
            );
        }
        if let Some(dp) = self.dark {
            // which tile pixels fall on a picture in the print (kept as is)
            let print = self.print.clone();
            let (w, h) = (self.w as f32, self.h as f32);
            let (x0, y0) = (r.x0 as f32, r.y0 as f32);
            let picture = move |tx: u32, ty: u32| -> bool {
                print.as_ref().is_some_and(|p| p.is_picture((x0 + tx as f32) / w, (y0 + ty as f32) / h))
            };
            to_dark(&mut tile, self.paper, dp, &picture);
        }
        let ts = shift.pre_scale(self.scale, self.scale);
        let min_r = 0.45 / self.scale;
        let (px0, py0) = (r.x0 as f32 / self.scale, r.y0 as f32 / self.scale);
        let (px1, py1) = (r.x1 as f32 / self.scale, r.y1 as f32 / self.scale);
        for m in marks {
            let b = m.stroke.bounds();
            if b[2] < px0 || b[0] > px1 || b[3] < py0 || b[1] > py1 {
                continue;
            }
            fill_poly(&mut tile, &outline(&m.stroke.points, min_r), ts, m.rgb, m.wash, self.dark.is_some());
        }
        self.pixmap.draw_pixmap(r.x0, r.y0, tile.as_ref(), &PixmapPaint { blend_mode: BlendMode::Source, ..Default::default() }, Transform::identity(), None);
        self.mark(r);
    }

    /// Draw one new stroke on top (pen-up): no background redraw needed.
    pub fn add_mark(&mut self, m: Mark) {
        let s = m.stroke;
        let ts = Transform::from_scale(self.scale, self.scale);
        fill_poly(&mut self.pixmap, &outline(&s.points, 0.45 / self.scale), ts, m.rgb, m.wash, self.dark.is_some());
        let r = self.page_rect(s.bounds());
        if let Some(r) = r.clamp(self.w as i32, self.h as i32) {
            self.mark(r);
        }
    }

    /// Fold dirty pixels into the BGRA buffer and mint a new GPU image.
    /// Returns the image it replaced (to drop from the atlas).
    pub fn flush(&mut self) -> Option<Arc<RenderImage>> {
        let d = self.dirty.take()?;
        let src = self.pixmap.data();
        let stride = self.w as usize * 4;
        for y in d.y0 as usize..d.y1 as usize {
            let row = y * stride;
            let (a, b) = (row + d.x0 as usize * 4, row + d.x1 as usize * 4);
            let (s, t) = (&src[a..b], &mut self.bgra[a..b]);
            for (sp, tp) in s.chunks_exact(4).zip(t.chunks_exact_mut(4)) {
                // Opaque page: premultiplied == straight; swap to BGRA.
                tp[0] = sp[2];
                tp[1] = sp[1];
                tp[2] = sp[0];
                tp[3] = 255;
            }
        }
        let img = RgbaImage::from_raw(self.w, self.h, self.bgra.clone())?;
        let new = Arc::new(RenderImage::new(vec![Frame::new(img)]));
        self.image.replace(new)
    }
}

/// A book page's pre-rendered raster, plus where its pictures are.
pub struct Print {
    pub pixmap: Pixmap,
    /// Picture boxes as page fractions [x0, y0, x1, y1]: one clean
    /// rectangle per figure, so dark mode leaves each picture whole.
    pictures: Vec<[f32; 4]>,
}

const CELL: u32 = 24;

impl Print {
    /// Is this point (fractions of the page) on a picture?
    pub fn is_picture(&self, fx: f32, fy: f32) -> bool {
        self.pictures.iter().any(|r| fx >= r[0] && fx < r[2] && fy >= r[1] && fy < r[3])
    }

    /// Find the pictures, so dark mode can leave them alone. Text is mostly
    /// paper with thin dark glyphs; photos are dense in mid-tones;
    /// engravings and halftones are dense in ink. Cells are classified and
    /// smoothed, connected cells become boxes, boxes that touch or nearly
    /// touch merge (a sparse engraving is one figure), and each box edge is
    /// tightened to the exact pixel where the picture's content starts.
    fn segment(pixmap: Pixmap) -> Print {
        let (w, h) = (pixmap.width(), pixmap.height());
        let (cols, rows) = (w.div_ceil(CELL) as usize, h.div_ceil(CELL) as usize);
        let data = pixmap.data();
        let luma_at = |x: u32, y: u32| {
            let i = ((y * w + x) * 4) as usize;
            (data[i] as u32 * 299 + data[i + 1] as u32 * 587 + data[i + 2] as u32 * 114) / 1000
        };
        let mut raw = vec![false; cols * rows];
        for r in 0..rows {
            for c in 0..cols {
                let (mut mid, mut ink, mut n) = (0u32, 0u32, 0u32);
                for y in (r as u32 * CELL)..((r as u32 + 1) * CELL).min(h) {
                    for x in (c as u32 * CELL)..((c as u32 + 1) * CELL).min(w) {
                        let i = ((y * w + x) * 4) as usize;
                        let (pr, pg, pb) = (data[i] as i32, data[i + 1] as i32, data[i + 2] as i32);
                        let l = (pr * 299 + pg * 587 + pb * 114) / 1000;
                        let chroma = pr.max(pg).max(pb) - pr.min(pg).min(pb);
                        if chroma > 40 || (50..=205).contains(&l) {
                            mid += 1;
                        } else if l < 50 {
                            ink += 1;
                        }
                        n += 1;
                    }
                }
                let n = n.max(1) as f32;
                raw[r * cols + c] = mid as f32 / n > 0.30 || ink as f32 / n > 0.45;
            }
        }
        let at = |m: &Vec<bool>, r: isize, c: isize| r >= 0 && c >= 0 && (r as usize) < rows && (c as usize) < cols && m[r as usize * cols + c as usize];
        let mut cells = vec![false; cols * rows];
        for r in 0..rows as isize {
            for c in 0..cols as isize {
                let votes = (-1..=1).flat_map(|dr| (-1..=1).map(move |dc| (dr, dc))).filter(|(dr, dc)| at(&raw, r + dr, c + dc)).count();
                cells[r as usize * cols + c as usize] = at(&raw, r, c) && votes >= 4 || votes >= 6;
            }
        }
        // connected components -> cell boxes [c0, r0, c1, r1] (inclusive)
        let mut seen = vec![false; cols * rows];
        let mut boxes: Vec<[usize; 4]> = Vec::new();
        for start in 0..cols * rows {
            if !cells[start] || seen[start] {
                continue;
            }
            let mut stack = vec![start];
            seen[start] = true;
            let mut bx = [usize::MAX, usize::MAX, 0, 0];
            while let Some(i) = stack.pop() {
                let (r, c) = (i / cols, i % cols);
                bx = [bx[0].min(c), bx[1].min(r), bx[2].max(c), bx[3].max(r)];
                for dr in -1isize..=1 {
                    for dc in -1isize..=1 {
                        let (nr, nc) = (r as isize + dr, c as isize + dc);
                        if at(&cells, nr, nc) {
                            let j = nr as usize * cols + nc as usize;
                            if !seen[j] {
                                seen[j] = true;
                                stack.push(j);
                            }
                        }
                    }
                }
            }
            boxes.push(bx);
        }
        // merge boxes within two cells of each other, until stable
        let near = |a: &[usize; 4], b: &[usize; 4]| a[0] <= b[2] + 2 && b[0] <= a[2] + 2 && a[1] <= b[3] + 2 && b[1] <= a[3] + 2;
        loop {
            let mut merged = false;
            'outer: for i in 0..boxes.len() {
                for j in i + 1..boxes.len() {
                    if near(&boxes[i], &boxes[j]) {
                        let b = boxes.remove(j);
                        let a = &mut boxes[i];
                        *a = [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])];
                        merged = true;
                        break 'outer;
                    }
                }
            }
            if !merged {
                break;
            }
        }
        let mut pictures = Vec::new();
        for b in boxes {
            let (bw, bh) = (b[2] - b[0] + 1, b[3] - b[1] + 1);
            if bw < 3 || bh < 3 || bw * bh < 12 {
                continue; // a stray dense glyph cluster, not a figure
            }
            // tighten to content pixels, searching one cell beyond the box
            let x0 = (b[0].saturating_sub(1) as u32) * CELL;
            let y0 = (b[1].saturating_sub(1) as u32) * CELL;
            let x1 = ((b[2] as u32 + 2) * CELL).min(w);
            let y1 = ((b[3] as u32 + 2) * CELL).min(h);
            let mut t = [u32::MAX, u32::MAX, 0, 0];
            for y in (y0..y1).step_by(2) {
                for x in (x0..x1).step_by(2) {
                    if luma_at(x, y) < 232 {
                        t = [t[0].min(x), t[1].min(y), t[2].max(x + 2), t[3].max(y + 2)];
                    }
                }
            }
            if t[0] >= t[2] || t[1] >= t[3] {
                continue;
            }
            // a scanned plate's own paper (pale gray) belongs to the
            // picture: grow each edge while it still runs over non-paper
            let off_paper = |x0: u32, y0: u32, x1: u32, y1: u32| {
                let (mut n, mut hit) = (0u32, 0u32);
                for y in (y0..y1.max(y0 + 1)).step_by(2) {
                    for x in (x0..x1.max(x0 + 1)).step_by(2) {
                        n += 1;
                        if luma_at(x.min(w - 1), y.min(h - 1)) < 243 {
                            hit += 1;
                        }
                    }
                }
                n > 0 && hit * 10 >= n * 7
            };
            let reach = 8 * CELL;
            let (ox0, oy0, ox1, oy1) = (t[0], t[1], t[2], t[3]);
            while t[0] >= 2 && ox0 - t[0] < reach && off_paper(t[0] - 2, t[1], t[0], t[3]) {
                t[0] -= 2;
            }
            while t[2] + 2 <= w && t[2] - ox1 < reach && off_paper(t[2], t[1], t[2] + 2, t[3]) {
                t[2] += 2;
            }
            while t[1] >= 2 && oy0 - t[1] < reach && off_paper(t[0], t[1] - 2, t[2], t[1]) {
                t[1] -= 2;
            }
            while t[3] + 2 <= h && t[3] - oy1 < reach && off_paper(t[0], t[3], t[2], t[3] + 2) {
                t[3] += 2;
            }
            pictures.push([t[0] as f32 / w as f32, t[1] as f32 / h as f32, t[2].min(w) as f32 / w as f32, t[3].min(h) as f32 / h as f32]);
        }
        Print { pixmap, pictures }
    }
}

/// Lay a raw source-PDF page render onto the 1404×1872 page exactly as
/// mkbook (and the web viewer's `pagePlacement`) does: the crop rect fitted
/// inside a 30px border, or the page fitted inside its margins, centred.
pub fn place_preview(bytes: &[u8], crop: Option<&[f64]>, margins: Option<&[f64]>) -> Option<Print> {
    const PW: f32 = 1404.0;
    const PH: f32 = 1872.0;
    const BORDER: f32 = 30.0;
    let raw = decode_pixmap(bytes)?;
    let (iw, ih) = (raw.width() as f32, raw.height() as f32);
    let (k, ox, oy, sx, sy) = match crop.filter(|c| c.len() == 4) {
        Some(c) => {
            let (cw, ch) = ((c[2] - c[0]) as f32 * iw, (c[3] - c[1]) as f32 * ih);
            let k = ((PW - 2.0 * BORDER) / cw).min((PH - 2.0 * BORDER) / ch);
            (k, (PW - cw * k) / 2.0, (PH - ch * k) / 2.0, c[0] as f32 * iw, c[1] as f32 * ih)
        }
        None => {
            let m: Vec<f32> = margins.filter(|m| m.len() == 4).map_or(vec![40.0; 4], |m| m.iter().map(|v| *v as f32).collect());
            let (bw, bh) = (PW - m[0] - m[2], PH - m[1] - m[3]);
            let k = (bw / iw).min(bh / ih);
            (k, m[0] + (bw - iw * k) / 2.0, m[1] + (bh - ih * k) / 2.0, 0.0, 0.0)
        }
    };
    let mut page = Pixmap::new(PW as u32, PH as u32)?;
    page.fill(Color::WHITE);
    let ts = Transform::from_translate(ox - sx * k, oy - sy * k).pre_scale(k, k);
    page.draw_pixmap(0, 0, raw.as_ref(), &PixmapPaint { quality: FilterQuality::Bicubic, ..Default::default() }, ts, None);
    Some(Print::segment(page))
}

/// Decode a book page PNG, ready to be multiplied onto paper.
pub fn decode_print(bytes: &[u8]) -> Option<Print> {
    decode_pixmap(bytes).map(Print::segment)
}

fn decode_pixmap(bytes: &[u8]) -> Option<Pixmap> {
    let img = image::load_from_memory(bytes).ok()?.into_rgba8();
    let (w, h) = img.dimensions();
    let mut data = img.into_raw();
    // Premultiply (rasters are normally opaque; keep it correct anyway).
    for px in data.chunks_exact_mut(4) {
        let a = px[3] as u16;
        if a < 255 {
            for c in &mut px[..3] {
                *c = ((*c as u16 * a + 127) / 255) as u8;
            }
        }
    }
    Pixmap::from_vec(data, tiny_skia::IntSize::from_wh(w, h)?)
}

/// Decode an image (cover) for GPUI: BGRA frame.
pub fn decode_render_image(bytes: &[u8], max_w: u32) -> Option<Arc<RenderImage>> {
    let mut img = image::load_from_memory(bytes).ok()?;
    if img.width() > max_w {
        let h = (img.height() as f32 * max_w as f32 / img.width() as f32) as u32;
        img = img.resize_exact(max_w, h.max(1), image::imageops::FilterType::Triangle);
    }
    let mut rgba = img.into_rgba8();
    for px in rgba.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    Some(Arc::new(RenderImage::new(vec![Frame::new(rgba)])))
}

// ------------------------------------------------------------------ eraser

fn hit_point(p: &InkPoint, x: f32, y: f32, r: f32) -> bool {
    (p.x - x).hypot(p.y - y) <= r + p.r
}

/// Split strokes around the nib (pixel eraser). Returns the page-unit
/// bounds of everything that changed.
pub fn erase_split(strokes: &mut Vec<InkStroke>, x: f32, y: f32, r: f32, next_id: &mut u64) -> Option<[f32; 4]> {
    let mut changed: Option<[f32; 4]> = None;
    let mut out = Vec::with_capacity(strokes.len());
    for stroke in strokes.drain(..) {
        let b = stroke.bounds();
        if x + r < b[0] || x - r > b[2] || y + r < b[1] || y - r > b[3] || !stroke.points.iter().any(|p| hit_point(p, x, y, r)) {
            out.push(stroke);
            continue;
        }
        changed = Some(changed.map_or(b, |c| union(c, b)));
        let mut run: Vec<InkPoint> = Vec::new();
        let mut first = true;
        let mut emit = |run: Vec<InkPoint>, out: &mut Vec<InkStroke>| {
            if run.len() >= 2 {
                let id = if first { stroke.id } else { *next_id += 1; *next_id - 1 };
                first = false;
                out.push(InkStroke { id, gray: stroke.gray, points: run });
            }
        };
        for p in &stroke.points {
            if hit_point(p, x, y, r) {
                emit(std::mem::take(&mut run), &mut out);
            } else {
                run.push(*p);
            }
        }
        emit(run, &mut out);
    }
    *strokes = out;
    changed
}

pub fn union(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]
}

/// Erase pi ink: split its strokes, and rub typeset text glyph by glyph.
/// `offsets(text, size)` gives the x offset of each char boundary.
/// Returns the ids of patches that changed and the dirty bounds.
pub fn erase_patches(
    page: &mut InkPage,
    x: f32,
    y: f32,
    r: f32,
    offsets: &dyn Fn(&str, f32) -> Vec<f32>,
) -> (Vec<u64>, Option<[f32; 4]>) {
    let mut ids = Vec::new();
    let mut dirty = None;
    let mut next = page.next_stroke;
    for patch in &mut page.patches {
        let mut changed = false;
        if let Some(b) = erase_split(&mut patch.strokes, x, y, r, &mut next) {
            dirty = Some(dirty.map_or(b, |d| union(d, b)));
            changed = true;
        }
        let mut texts = Vec::with_capacity(patch.texts.len());
        for t in patch.texts.drain(..) {
            match erase_glyphs(&t, x, y, r, offsets) {
                Some(pieces) => {
                    changed = true;
                    texts.extend(pieces);
                }
                None => texts.push(t),
            }
        }
        patch.texts = texts;
        if changed {
            ids.push(patch.id);
        }
    }
    page.next_stroke = next;
    page.patches.retain(|p| !(p.strokes.is_empty() && p.texts.is_empty()));
    (ids, dirty)
}

fn erase_glyphs(t: &crate::model::InkText, x: f32, y: f32, r: f32, offsets: &dyn Fn(&str, f32) -> Vec<f32>) -> Option<Vec<crate::model::InkText>> {
    if y < t.y - t.size - r || y > t.y + t.size * 0.35 + r {
        return None;
    }
    let chars: Vec<char> = t.text.chars().collect();
    if chars.is_empty() {
        return None;
    }
    let offs = offsets(&t.text, t.size);
    if offs.len() != chars.len() + 1 || x < t.x - r || x > t.x + offs[chars.len()] + r {
        return None;
    }
    let keep: Vec<bool> = (0..chars.len()).map(|i| !(x + r >= t.x + offs[i] && x - r <= t.x + offs[i + 1])).collect();
    if keep.iter().all(|k| *k) {
        return None;
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if !keep[i] {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && keep[i] {
            i += 1;
        }
        let text: String = chars[start..i].iter().collect();
        if !text.trim().is_empty() {
            out.push(crate::model::InkText { x: t.x + offs[start], y: t.y, size: t.size, gray: t.gray, text });
        }
    }
    Some(out)
}

// ----------------------------------------------------------------- history

/// Snapshot undo over the user's own marks (strokes + highlights).
pub type Snap = (Vec<InkStroke>, Vec<InkStroke>);

#[derive(Default)]
pub struct History {
    undo: Vec<Snap>,
    redo: Vec<Snap>,
}

impl History {
    pub fn checkpoint(&mut self, snap: Snap) {
        self.undo.push(snap);
        self.redo.clear();
        if self.undo.len() > 200 {
            self.undo.remove(0);
        }
    }
    pub fn undo(&mut self, current: Snap) -> Option<Snap> {
        let prev = self.undo.pop()?;
        self.redo.push(current);
        Some(prev)
    }
    pub fn redo(&mut self, current: Snap) -> Option<Snap> {
        let next = self.redo.pop()?;
        self.undo.push(current);
        Some(next)
    }
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(id: u64) -> InkStroke {
        InkStroke { id, gray: 0, points: (0..20).map(|i| InkPoint { x: i as f32 * 5.0, y: 10.0, r: 2.0 }).collect() }
    }

    #[test]
    fn eraser_splits_in_two() {
        let mut v = vec![line(1)];
        let mut next = 10;
        let dirty = erase_split(&mut v, 50.0, 10.0, 4.0, &mut next);
        assert!(dirty.is_some());
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].id, 1);
        assert_eq!(v[1].id, 10);
    }

    #[test]
    fn segments_text_from_pictures() {
        // left half: a mid-gray "photo"; right half: white with thin black lines ("text")
        let (w, h) = (192u32, 192u32);
        let mut px = Pixmap::new(w, h).unwrap();
        for (i, p) in px.data_mut().chunks_exact_mut(4).enumerate() {
            let (x, y) = (i as u32 % w, i as u32 / w);
            let v = if x < 96 { 128 } else if y % 12 == 0 { 0 } else { 255 };
            p.copy_from_slice(&[v, v, v, 255]);
        }
        let print = Print::segment(px);
        assert!(print.is_picture(0.2, 0.5));
        assert!(!print.is_picture(0.85, 0.5));
    }

    #[test]
    fn raster_flushes() {
        let mut r = PageRaster::new(100, 120, 0.1, [250, 248, 242], None, None).unwrap();
        r.render_all(std::iter::empty());
        assert!(r.flush().is_none());
        assert!(r.image.is_some());
        r.add_mark(Mark { stroke: &line(1), rgb: 0, wash: false });
        assert!(r.flush().is_some());
    }
}

#[cfg(test)]
mod dark_preview {
    use super::*;

    /// PAPIER_PRINT=page.png PAPIER_OUT=out.png cargo test dark_preview -- --ignored
    #[test]
    #[ignore]
    fn render_dark_page() {
        let bytes = std::fs::read(std::env::var("PAPIER_PRINT").unwrap()).unwrap();
        let print = Arc::new(decode_print(&bytes).unwrap());
        eprintln!("pictures: {:?}", print.pictures);
        let (w, h) = (702, 936);
        let mut r = PageRaster::new(w, h, w as f32 / 1404.0, [250, 248, 242], Some(print), Some([0x1e, 0x1c, 0x19])).unwrap();
        r.render_all(std::iter::empty());
        r.pixmap.save_png(std::env::var("PAPIER_OUT").unwrap()).unwrap();
    }
}

#[cfg(test)]
mod place_preview_check {
    /// PREVIEW=raw.png OUT=placed.png cargo test place_preview_check -- --ignored
    #[test]
    #[ignore]
    fn place() {
        let bytes = std::fs::read(std::env::var("PREVIEW").unwrap()).unwrap();
        let p = super::place_preview(&bytes, Some(&[0.0, 0.0, 1.0, 1.0]), None).unwrap();
        p.pixmap.save_png(std::env::var("OUT").unwrap()).unwrap();
    }
}
