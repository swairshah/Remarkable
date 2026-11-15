//! Highlighter snapping: a highlight drawn near printed text becomes a clean
//! band along the text line(s), snapped to word boundaries — like selecting
//! text. Everything is read off the page *image* (no PDF text layer), so
//! the same logic works for any book page here and on the reMarkable.
//!
//! Lines are found from a row profile of ink around the stroke: runs of
//! rows that contain ink are text lines (taller runs are pictures and are
//! ignored). Words are runs of ink columns within a line, split at gaps
//! wider than a fraction of the line height; columns of text are split at
//! much wider gaps, so a highlight never jumps into the next column.

use crate::model::InkPoint;

/// Read access to a page image, in page units (1404 × 1872).
pub trait Ink {
    /// Is there printed ink at this page point?
    fn dark(&self, x: f32, y: f32) -> bool;
    /// Page units per sampling step (one image pixel).
    fn step(&self) -> f32;
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Band {
    top: f32,
    bottom: f32,
}

impl Band {
    fn h(&self) -> f32 {
        self.bottom - self.top
    }
    fn mid(&self) -> f32 {
        (self.top + self.bottom) / 2.0
    }
}

const PAGE_W: f32 = 1404.0;
/// Plausible text line heights, page units (tablet px).
const MIN_LINE: f32 = 9.0;
const MAX_LINE: f32 = 80.0;

/// Text lines crossing the window [y0, y1] × [x0, x1].
fn bands(img: &dyn Ink, x0: f32, x1: f32, y0: f32, y1: f32) -> Vec<Band> {
    let s = img.step();
    let cols = ((x1 - x0) / s).max(1.0) as usize;
    let need = (cols / 120).max(2);
    let mut out = Vec::new();
    let mut run: Option<f32> = None;
    let mut gap = 0;
    let mut last_ink = y0;
    let mut y = y0;
    while y <= y1 + s {
        let mut n = 0;
        let mut x = x0;
        while x < x1 && n < need {
            if img.dark(x, y) {
                n += 1;
            }
            x += s;
        }
        let inked = n >= need && y <= y1;
        if inked {
            run.get_or_insert(y);
            last_ink = y;
            gap = 0;
        } else if let Some(top) = run {
            gap += 1;
            // a line's ink can have a pixel or two of white inside it
            if gap > 2 || y > y1 {
                out.push(Band { top, bottom: last_ink + s });
                run = None;
            }
        }
        y += s;
    }
    out.retain(|b| (MIN_LINE..=MAX_LINE).contains(&b.h()));
    out
}

/// Word extents along a line, grouped into text columns: Vec<column> of
/// Vec<(x0, x1)>.
fn columns(img: &dyn Ink, band: Band, x_from: f32, x_to: f32) -> Vec<Vec<(f32, f32)>> {
    let s = img.step();
    let word_gap = band.h() * 0.33;
    let col_gap = band.h() * 2.5;
    let mut words: Vec<(f32, f32)> = Vec::new();
    let mut start: Option<f32> = None;
    let mut last = x_from;
    let mut x = x_from;
    while x <= x_to {
        let mut y = band.top;
        let mut ink = false;
        while y <= band.bottom {
            if img.dark(x, y) {
                ink = true;
                break;
            }
            y += s;
        }
        if ink {
            match start {
                Some(_) if x - last > word_gap => {
                    words.push((start.unwrap(), last + s));
                    start = Some(x);
                }
                None => start = Some(x),
                _ => {}
            }
            last = x;
        }
        x += s;
    }
    if let Some(st) = start {
        words.push((st, last + s));
    }
    let mut cols: Vec<Vec<(f32, f32)>> = Vec::new();
    for w in words {
        match cols.last_mut() {
            Some(c) if w.0 - c.last().unwrap().1 <= col_gap => c.push(w),
            _ => cols.push(vec![w]),
        }
    }
    cols
}

/// The band a point belongs to, if it is close enough to one.
fn band_at(bands: &[Band], y: f32) -> Option<usize> {
    bands
        .iter()
        .enumerate()
        .filter(|(_, b)| y >= b.top - b.h() * 0.6 && y <= b.bottom + b.h() * 0.6)
        .min_by(|a, b| (a.1.mid() - y).abs().total_cmp(&(b.1.mid() - y).abs()))
        .map(|(i, _)| i)
}

/// The column (words) on a line that contains, or is nearest to, x.
fn column_at(cols: &[Vec<(f32, f32)>], x: f32) -> Option<&Vec<(f32, f32)>> {
    cols.iter().min_by(|a, b| {
        let dist = |c: &Vec<(f32, f32)>| {
            let (l, r) = (c[0].0, c.last().unwrap().1);
            if x < l { l - x } else if x > r { x - r } else { 0.0 }
        };
        dist(a).total_cmp(&dist(b))
    })
}

/// The word containing x, or the nearest one.
fn word_at(words: &[(f32, f32)], x: f32) -> (f32, f32) {
    *words
        .iter()
        .min_by(|a, b| {
            let d = |w: &(f32, f32)| if x < w.0 { w.0 - x } else if x > w.1 { x - w.1 } else { 0.0 };
            d(a).total_cmp(&d(b))
        })
        .unwrap()
}

/// Snap a highlighter stroke (page units) to the text under it. Returns one
/// straight highlight per covered line, or None to keep the freehand mark.
pub fn snap(img: &dyn Ink, pts: &[InkPoint]) -> Option<Vec<Vec<InkPoint>>> {
    if pts.len() < 2 {
        return None;
    }
    let (mut x0, mut x1, mut y0, mut y1) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
    for p in pts {
        x0 = x0.min(p.x);
        x1 = x1.max(p.x);
        y0 = y0.min(p.y);
        y1 = y1.max(p.y);
    }
    // where the stroke starts and ends decides the selection direction
    let (mut a, mut b) = (pts[0], pts[pts.len() - 1]);
    let lines = bands(img, (x0 - 300.0).max(0.0), (x1 + 300.0).min(PAGE_W), y0 - MAX_LINE, y1 + MAX_LINE);
    let (mut ia, mut ib) = (band_at(&lines, a.y)?, band_at(&lines, b.y)?);
    if ia > ib || (ia == ib && a.x > b.x) {
        std::mem::swap(&mut a, &mut b);
        std::mem::swap(&mut ia, &mut ib);
    }
    let row = |band: Band| columns(img, band, 0.0, PAGE_W);
    let mut out = Vec::new();
    for (i, band) in lines.iter().enumerate().take(ib + 1).skip(ia) {
        let cols = row(*band);
        // stay in the column the stroke started in
        let col = column_at(&cols, if i == ia { a.x } else { a.x.min(b.x) })?;
        let left = if i == ia { word_at(col, a.x).0 } else { col[0].0 };
        let right = if i == ib { word_at(col, if ia == ib { x1 } else { b.x }).1 } else { col.last().unwrap().1 };
        let left = if i == ia && ia == ib { word_at(col, x0).0 } else { left };
        if right <= left {
            continue;
        }
        let r = (band.h() * 0.62).max(9.0);
        let (l, rr) = (left - 3.0, right + 3.0);
        let n = (((rr - l) / 14.0).ceil() as usize).max(1);
        out.push((0..=n).map(|k| InkPoint { x: l + (rr - l) * k as f32 / n as f32, y: band.mid(), r }).collect());
    }
    (!out.is_empty()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake page: lines of "words" (ink blocks) 30 units tall every 50.
    struct Page;
    impl Ink for Page {
        fn dark(&self, x: f32, y: f32) -> bool {
            let line = (y - 100.0) / 50.0;
            if !(0.0..6.0).contains(&line) || (y - 100.0) % 50.0 > 30.0 {
                return false;
            }
            // words 80 wide with 20 gaps, between x 100 and 700
            (100.0..700.0).contains(&x) && (x - 100.0) % 100.0 < 80.0
        }
        fn step(&self) -> f32 {
            1.0
        }
    }

    fn stroke(pts: &[(f32, f32)]) -> Vec<InkPoint> {
        pts.iter().map(|&(x, y)| InkPoint { x, y, r: 17.0 }).collect()
    }

    #[test]
    fn single_line_snaps_to_words() {
        // a wobbly stroke over line 1 (y 150..180) from inside word 2 to word 4
        let out = snap(&Page, &stroke(&[(230.0, 168.0), (300.0, 160.0), (420.0, 172.0)])).unwrap();
        assert_eq!(out.len(), 1);
        let line = &out[0];
        assert!((line[0].x - 197.0).abs() < 3.0, "start {}", line[0].x); // word 2 starts at 200
        assert!((line.last().unwrap().x - 483.0).abs() < 3.0, "end {}", line.last().unwrap().x); // word 4 ends at 480
        assert!((line[0].y - 165.0).abs() < 2.0);
    }

    #[test]
    fn multi_line_selects_like_text() {
        let out = snap(&Page, &stroke(&[(330.0, 115.0), (500.0, 190.0), (250.0, 265.0)])).unwrap();
        assert_eq!(out.len(), 4); // lines 0..=3
        assert!((out[1][0].x - 97.0).abs() < 3.0); // middle lines run full width
        assert!((out[1].last().unwrap().x - 683.0).abs() < 3.0);
    }

    #[test]
    fn far_from_text_stays_freehand() {
        assert!(snap(&Page, &stroke(&[(300.0, 900.0), (500.0, 905.0)])).is_none());
    }
}

#[cfg(test)]
mod real_page {
    use super::*;

    /// SNAP_PAGE=page.png SNAP_OUT=out.png cargo test snap::real_page -- --ignored --nocapture
    #[test]
    #[ignore]
    fn snap_on_a_real_page() {
        let bytes = std::fs::read(std::env::var("SNAP_PAGE").unwrap()).unwrap();
        let print = crate::ink::decode_print(&bytes).unwrap();
        let mut px = print.pixmap.clone();
        let sc = px.width() as f32 / 1404.0;
        let wobbly = |x0: f32, x1: f32, y: f32| -> Vec<InkPoint> {
            (0..30).map(|i| InkPoint { x: x0 + (x1 - x0) * i as f32 / 29.0, y: y + 7.0 * (i as f32 * 0.7).sin(), r: 17.0 }).collect()
        };
        let strokes = [wobbly(300.0, 700.0, 717.0), {
            let mut v = wobbly(500.0, 900.0, 770.0);
            v.extend(wobbly(900.0, 400.0, 820.0));
            v
        }];
        for (k, s) in strokes.iter().enumerate() {
            let t = std::time::Instant::now();
            let out = snap(&print, s);
            eprintln!("stroke {k}: {:?} in {:?}", out.as_ref().map(|o| o.iter().map(|l| (l[0].x as i32, l.last().unwrap().x as i32, l[0].y as i32, l[0].r as i32)).collect::<Vec<_>>()), t.elapsed());
            for line in out.unwrap_or_default() {
                let (x0, x1, y, r) = (line[0].x, line.last().unwrap().x, line[0].y, line[0].r);
                let rect = tiny_skia::Rect::from_xywh(x0 * sc, (y - r) * sc, (x1 - x0) * sc, 2.0 * r * sc).unwrap();
                let mut paint = tiny_skia::Paint::default();
                paint.set_color_rgba8(if k == 0 { 255 } else { 80 }, 200, if k == 0 { 0 } else { 255 }, 90);
                px.fill_rect(rect, &paint, tiny_skia::Transform::identity(), None);
            }
            for p in s {
                let r = tiny_skia::Rect::from_xywh(p.x * sc - 2.0, p.y * sc - 2.0, 4.0, 4.0).unwrap();
                let mut paint = tiny_skia::Paint::default();
                paint.set_color_rgba8(220, 0, 0, 255);
                px.fill_rect(r, &paint, tiny_skia::Transform::identity(), None);
            }
        }
        px.save_png(std::env::var("SNAP_OUT").unwrap()).unwrap();
    }
}
