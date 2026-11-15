//! The page curl: a leaf of paper wrapped round a small cylinder, rendered
//! on the CPU (GPUI can't warp or rotate an image), straight from the page
//! rasters' pixels. Work happens in the *leaf frame*: u runs from the spine
//! (0) to the leaf's outer edge (w), v from the top (0) to the bottom (h);
//! negative u is the facing page the leaf turns onto.
//!
//! Geometry (the classic cylinder curl): the edge point you grabbed, O,
//! follows your finger to F. The fold lies along the perpendicular
//! bisector of O→F; the paper wraps a cylinder of radius r there, and its
//! far part lies flat again, face down, so O lands exactly under F. For a
//! destination point at distance d past the fold, the paper over it came
//! from unrolled distance s; layers top to bottom are: the flat back
//! (s = πr − d), the cylinder's back half (s = r(π − asin d/r)), its front
//! half (s = r·asin d/r), and the flat front (s = d).

use std::sync::Arc;

use gpui::RenderImage;

/// A page image to sample (BGRA, straight alpha).
pub struct Src {
    img: Arc<RenderImage>,
}

impl Src {
    pub fn new(img: Arc<RenderImage>) -> Src {
        Src { img }
    }

    fn dims(&self) -> (i32, i32) {
        let sz = self.img.size(0);
        (i32::from(sz.width).max(1), i32::from(sz.height).max(1))
    }

}

/// A source resolved once per frame: bytes and size, no per-pixel lookups.
#[derive(Clone, Copy)]
struct View<'a> {
    b: &'a [u8],
    w: i32,
    h: i32,
}

impl View<'_> {
    #[inline(always)]
    fn rgb(&self, x: i32, y: i32) -> [f32; 3] {
        let i = ((y * self.w + x) * 4) as usize;
        [self.b[i] as f32, self.b[i + 1] as f32, self.b[i + 2] as f32]
    }

    #[inline(always)]
    fn exact(&self, x: f32, y: f32) -> [u8; 4] {
        let (x, y) = ((x as i32).clamp(0, self.w - 1), (y as i32).clamp(0, self.h - 1));
        let i = ((y * self.w + x) * 4) as usize;
        [self.b[i], self.b[i + 1], self.b[i + 2], 255]
    }

    #[inline(always)]
    fn smooth(&self, x: f32, y: f32) -> [u8; 4] {
        let (x, y) = ((x - 0.5).clamp(0.0, (self.w - 1) as f32), (y - 0.5).clamp(0.0, (self.h - 1) as f32));
        let (x0, y0) = (x as i32, y as i32);
        let (x1, y1) = ((x0 + 1).min(self.w - 1), (y0 + 1).min(self.h - 1));
        let (fx, fy) = (x - x0 as f32, y - y0 as f32);
        let (a, b, c, d) = (self.rgb(x0, y0), self.rgb(x1, y0), self.rgb(x0, y1), self.rgb(x1, y1));
        let mut out = [0u8, 0, 0, 255];
        for k in 0..3 {
            let top = a[k] + (b[k] - a[k]) * fx;
            let bot = c[k] + (d[k] - c[k]) * fx;
            out[k] = (top + (bot - top) * fy) as u8;
        }
        out
    }
}

impl Src {
    fn view(&self) -> Option<View<'_>> {
        let (w, h) = self.dims();
        let b = self.img.as_bytes(0)?;
        (b.len() >= (w * h * 4) as usize).then_some(View { b, w, h })
    }
}

pub struct Curl<'a> {
    /// The leaf's size in output pixels.
    pub w: f32,
    pub h: f32,
    /// true: the right leaf turning forward; false: the left leaf, backward.
    pub forward: bool,
    /// Grabbed point and where it is now, in leaf frame.
    pub origin: (f32, f32),
    pub finger: (f32, f32),
    pub front: Option<&'a Src>,
    pub back: Option<&'a Src>,
    /// Paper color (BGRA) for a page whose raster isn't ready.
    pub paper: [u8; 4],
    pub dark: bool,
    /// > 0: the GPU draws the leaf's flat front itself (crisp, clipped to
    /// strips along the fold); the buffer only covers this band (in leaf
    /// px) of flat paper before the fold, plus everything that warps.
    pub gpu_band: f32,
    /// 0 → 1 as the leaf settles on the far page: the back's shading fades
    /// out, so the last frame matches the page at rest (no tone jump).
    pub land: f32,
    /// The GPU draws the flat back (only with a vertical fold, where it is a
    /// pure shift of the next page): leave those pixels transparent.
    pub gpu_back: bool,
    /// The GPU also draws the fold's darkening and the shadow (vertical
    /// folds): the buffer holds just the roll and a thin seam band.
    pub gpu_shading: bool,
}

/// Give fully transparent pixels the colour of a drawn neighbour (alpha
/// stays 0). The GPU samples this image with linear filtering and straight
/// alpha: a transparent pixel stored as black would bleed a dark fringe
/// into every edge of the curl.
fn bleed(buf: &mut [u8], w: usize, h: usize) {
    let src = buf.to_vec();
    let opaque = |x: usize, y: usize| src[(y * w + x) * 4 + 3] > 200;
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 4;
            if src[i + 3] != 0 {
                continue;
            }
            let near = [(x.wrapping_sub(1), y), (x + 1, y), (x, y.wrapping_sub(1)), (x, y + 1)];
            if let Some(&(nx, ny)) = near.iter().find(|&&(nx, ny)| nx < w && ny < h && opaque(nx, ny)) {
                let j = (ny * w + nx) * 4;
                buf[i..i + 3].copy_from_slice(&src[j..j + 3]);
            }
        }
    }
}

/// Keep the finger where paper can actually reach: the leaf is bound at
/// the spine, so no point may get farther from either spine corner than
/// it was to begin with.
pub fn constrain(origin: (f32, f32), finger: (f32, f32), h: f32) -> (f32, f32) {
    let mut f = finger;
    for _ in 0..3 {
        for corner in [(0.0f32, 0.0f32), (0.0, h)] {
            let reach = (origin.0 - corner.0).hypot(origin.1 - corner.1);
            let (dx, dy) = (f.0 - corner.0, f.1 - corner.1);
            let d = dx.hypot(dy);
            if d > reach && d > 0.0 {
                f = (corner.0 + dx * reach / d, corner.1 + dy * reach / d);
            }
        }
    }
    f
}

impl Curl<'_> {
    /// Render the whole spread area (2w × h, the facing page on the left
    /// for a forward turn) as BGRA with alpha: transparent where the
    /// spread underneath shows, translucent black where the leaf's shadow
    /// falls on it.
    pub fn render(&self) -> Vec<u8> {
        self.render_full().0
    }

    /// Render, then crop to what's not fully transparent: (pixels, x, y,
    /// width, height) within the 2w × h spread. Keeps per-frame uploads
    /// small while the leaf has only just lifted.
    pub fn render_cropped(&self) -> Option<(Vec<u8>, u32, u32, u32, u32)> {
        // With the flat parts on the GPU, what can draw lies between two
        // lines parallel to the fold: allocate and render just that box.
        if let Some((fold, n, r)) = self.geometry().filter(|&(_, n, _)| self.gpu_back && self.gpu_band > 0.0 && n.0 > 0.05) {
            let (full_w, oh) = ((2.0 * self.w).round() as usize, self.h.round() as usize);
            let reach = if self.gpu_shading { r + 2.0 } else { r + self.w * 0.14 + 2.0 };
            let (dlo, dhi) = (-self.gpu_band - 2.0, reach);
            let at = |d: f32, y: f32| fold.0 + (d - (y - fold.1) * n.1) / n.0;
            let us = [at(dlo, 0.0), at(dlo, self.h), at(dhi, 0.0), at(dhi, self.h)];
            let xs = us.map(|u| if self.forward { u + self.w } else { self.w - u });
            let x0 = (xs.iter().cloned().fold(f32::MAX, f32::min).floor().max(0.0) as usize).min(full_w);
            let x1 = (xs.iter().cloned().fold(f32::MIN, f32::max).ceil().max(0.0) as usize).min(full_w);
            if x1 <= x0 {
                return None;
            }
            let ow = x1 - x0;
            let mut out = vec![0u8; ow * oh * 4];
            self.rows(&mut out, ow, x0, n, fold, r);
            bleed(&mut out, ow, oh);
            return Some((out, x0 as u32, 0, ow as u32, oh as u32));
        }
        let (buf, ow, oh) = self.render_full();
        let (mut x0, mut y0, mut x1, mut y1) = (ow, oh, 0usize, 0usize);
        for y in 0..oh {
            let row = &buf[y * ow * 4..(y + 1) * ow * 4];
            let first = (0..ow).find(|&x| row[x * 4 + 3] != 0);
            if let Some(f) = first {
                let last = (0..ow).rev().find(|&x| row[x * 4 + 3] != 0).unwrap_or(f);
                x0 = x0.min(f);
                x1 = x1.max(last + 1);
                y0 = y0.min(y);
                y1 = y1.max(y + 1);
            }
        }
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        let (cw, ch) = (x1 - x0, y1 - y0);
        let mut out = Vec::with_capacity(cw * ch * 4);
        for y in y0..y1 {
            out.extend_from_slice(&buf[(y * ow + x0) * 4..(y * ow + x1) * 4]);
        }
        bleed(&mut out, cw, ch);
        Some((out, x0 as u32, y0 as u32, cw as u32, ch as u32))
    }

    /// The fold: where the cylinder starts (leaf frame), the unit normal
    /// pointing toward the lifted edge, and the cylinder's radius. None
    /// while the leaf lies flat.
    pub fn geometry(&self) -> Option<((f32, f32), (f32, f32), f32)> {
        let o = self.origin;
        let f = constrain(o, self.finger, self.h);
        let (vx, vy) = (o.0 - f.0, o.1 - f.1);
        let len = vx.hypot(vy);
        if len < 0.5 {
            return None;
        }
        let n = (vx / len, vy / len);
        // the cylinder shrinks as the leaf nears flat on the far page
        // ...and flattens away as the leaf lands, so the last frame is the
        // page lying flat (no leftover roll at the spine)
        let land = self.land.clamp(0.0, 1.0);
        let r = ((self.w * 0.09).min(len / std::f32::consts::PI * 0.9) * (1.0 - land)).max(0.5);
        let pi_r = std::f32::consts::PI * r;
        let mid = ((o.0 + f.0) / 2.0, (o.1 + f.1) / 2.0);
        Some(((mid.0 - n.0 * pi_r / 2.0, mid.1 - n.1 * pi_r / 2.0), n, r))
    }

    fn render_full(&self) -> (Vec<u8>, usize, usize) {
        let (w, h) = (self.w, self.h);
        let (ow, oh) = ((2.0 * w).round() as usize, h.round() as usize);
        let mut out = vec![0u8; ow * oh * 4];
        match self.geometry() {
            Some((fold, n, r)) => self.rows(&mut out, ow, 0, n, fold, r),
            None => self.rows(&mut out, ow, 0, (1.0, 0.0), (1.0e6, 0.0), 2.0),
        }
        (out, ow, oh)
    }

    /// Fill `out` (`ow` columns of the spread, starting at column `x_off`).
    fn rows(&self, out: &mut [u8], ow: usize, x_off: usize, n: (f32, f32), fold: (f32, f32), r: f32) {
        let views = (self.front.and_then(Src::view), self.back.and_then(Src::view));
        // how far the leaf stands off the paper: 0 at rest, 1 mid-turn
        let o = self.origin;
        let f = constrain(o, self.finger, self.h);
        let travel = ((o.0 - f.0).hypot(o.1 - f.1) / (2.0 * self.w)).clamp(0.0, 1.0);
        let lift = (std::f32::consts::PI * travel).sin() * (1.0 - self.land.clamp(0.0, 1.0));
        let threads = std::env::var("CURL_THREADS").ok().and_then(|t| t.parse().ok()).unwrap_or(6usize);
        let oh = out.len() / (ow * 4);
        let chunk = oh.div_ceil(threads).max(1);
        std::thread::scope(|scope| {
            for (k, part) in out.chunks_mut(chunk * ow * 4).enumerate() {
                scope.spawn(move || {
                    for (j, row) in part.chunks_mut(ow * 4).enumerate() {
                        let y = (k * chunk + j) as f32 + 0.5;
                        // With the flat front and back on the GPU, only the band
                        // before the fold, the roll and the shadow past it can
                        // draw: d ∈ [-band, r + shadow]. d is linear along the
                        // row, so compute just those columns.
                        let (mut i0, mut i1) = (0usize, ow);
                        if self.gpu_back && self.gpu_band > 0.0 && n.0 > 0.05 {
                            let reach = if self.gpu_shading { r + 2.0 } else { r + self.w * 0.14 + 2.0 };
                            let (dlo, dhi) = (-self.gpu_band - 2.0, reach);
                            // d = (u - fold.u)·n.u + (y - fold.v)·n.v  =>  u = fold.u + (d - (y - fold.v)·n.v) / n.u
                            let at = |d: f32| fold.0 + (d - (y - fold.1) * n.1) / n.0;
                            let (ulo, uhi) = (at(dlo), at(dhi));
                            let (xa, xb) = if self.forward { (ulo + self.w, uhi + self.w) } else { (self.w - uhi, self.w - ulo) };
                            i0 = ((xa.floor() - x_off as f32).max(0.0) as usize).min(ow);
                            i1 = ((xb.ceil() - x_off as f32).max(0.0) as usize).min(ow).max(i0);
                        }
                        for i in i0..i1 {
                            let x = (i + x_off) as f32 + 0.5;
                            // spread x -> leaf frame u (left half of the output is u < 0 for forward)
                            let u = if self.forward { x - self.w } else { self.w - x };
                            let px = self.pixel(u, y, n, fold, r, views, lift);
                            row[i * 4..i * 4 + 4].copy_from_slice(&px);
                        }
                    }
                });
            }
        });
    }

    #[inline]
    fn inside(&self, s: (f32, f32)) -> bool {
        s.0 >= 0.0 && s.0 <= self.w && s.1 >= 0.0 && s.1 <= self.h
    }

    /// Sample a face at leaf point `s` (output pixels; the leaf is rendered
    /// at the page raster's own resolution, so flat paper maps 1:1).
    #[inline]
    fn sample(&self, views: (Option<View>, Option<View>), back: bool, s: (f32, f32), shade: f32, exact: bool) -> [u8; 4] {
        // the back face is the mirror image of the next page
        let x = if back == self.forward { self.w - s.0 } else { s.0 };
        let src = if back { views.1 } else { views.0 };
        let mut px = match src {
            Some(v) => {
                // map leaf pixels onto the source raster (same size, normally)
                let (sx, sy) = (x * v.w as f32 / self.w, s.1 * v.h as f32 / self.h);
                if exact { v.exact(sx, sy) } else { v.smooth(sx, sy) }
            }
            None => self.paper,
        };
        if shade < 0.999 {
            for c in &mut px[..3] {
                *c = (*c as f32 * shade).min(255.0) as u8;
            }
        }
        px[3] = 255;
        px
    }

    #[inline]
    fn pixel(&self, u: f32, v: f32, n: (f32, f32), fold: (f32, f32), r: f32, views: (Option<View>, Option<View>), lift: f32) -> [u8; 4] {
        let d = (u - fold.0) * n.0 + (v - fold.1) * n.1;
        let pi_r = std::f32::consts::PI * r;
        let src = |s: f32| (u + n.0 * (s - d), v + n.1 * (s - d));
        let land = self.land.clamp(0.0, 1.0);
        let back_tone = (if self.dark { 0.86 } else { 0.94 }) * (1.0 - land) + land;
        // a near-vertical fold makes the flat back a pure horizontal shift of
        // the next page: copy it exactly, as sharp as the page at rest
        let straight = n.1.abs() < 0.03;
        // 1. the far part, lying flat face down
        if d < 0.0 {
            let s = src(pi_r - d);
            if pi_r - d > pi_r && self.inside(s) {
                if self.gpu_back {
                    return [0, 0, 0, 0];
                }
                // a little darker right at the fold
                let near = (1.0 + d / (r * 1.5)).clamp(0.0, 1.0) * (1.0 - land);
                return self.sample(views, true, s, back_tone * (1.0 - 0.18 * near), straight);
            }
        }
        if (0.0..=r).contains(&d) {
            let a = (d / r).clamp(-1.0, 1.0).asin();
            // 2. over the top of the cylinder: the back, rolling into view
            let s = src(r * (std::f32::consts::PI - a));
            if self.inside(s) {
                return self.sample(views, true, s, back_tone * (0.90 + 0.10 * (d / r)), false);
            }
            // 3. the front, curving away from the light
            let s = src(r * a);
            if self.inside(s) {
                return self.sample(views, false, s, 0.80 + 0.20 * a.cos(), false);
            }
        }
        // 4. still flat on the paper (the GPU draws it, beyond the band)
        if self.gpu_band > 0.0 && d < -self.gpu_band {
            return [0, 0, 0, 0];
        }
        if d < 0.0 && u >= 0.0 && self.inside((u, v)) {
            let near = if self.gpu_shading { 0.0 } else { (1.0 + d / (r * 2.0)).clamp(0.0, 1.0) };
            return self.sample(views, false, (u, v), 1.0 - 0.12 * near * near, self.gpu_band <= 0.0);
        }
        // 5. the page beneath: the leaf's shadow
        if d >= 0.0 && u <= self.w && u >= -self.w {
            if self.gpu_shading {
                return [0, 0, 0, 0];
            }
            let k = 1.0 - ((d - r * 0.3) / (self.w * 0.14)).clamp(0.0, 1.0);
            let a = (if self.dark { 0.40 } else { 0.20 }) * k * k * lift;
            return [0, 0, 0, (a * 255.0) as u8];
        }
        [0, 0, 0, 0]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spine_holds_the_leaf() {
        // dragging far past the facing page can't tear the leaf off
        let f = constrain((100.0, 120.0), (-400.0, 120.0), 150.0);
        assert!(f.0 >= -101.0, "{f:?}");
    }

    #[test]
    fn flat_leaf_is_opaque_on_its_side_only() {
        let c = Curl { w: 20.0, h: 10.0, forward: true, origin: (20.0, 5.0), finger: (20.0, 5.0), front: None, back: None, paper: [9, 9, 9, 255], dark: false, gpu_band: 0.0, land: 0.0, gpu_back: false, gpu_shading: false };
        let out = c.render();
        let px = |x: usize, y: usize| out[(y * 40 + x) * 4 + 3];
        assert_eq!(px(5, 5), 0); // facing page shows through
        assert_eq!(px(30, 5), 255); // the leaf itself
    }

    #[test]
    fn half_turn_puts_back_face_over_facing_page() {
        let c = Curl { w: 40.0, h: 20.0, forward: true, origin: (40.0, 10.0), finger: (-30.0, 10.0), front: None, back: None, paper: [9, 9, 9, 255], dark: false, gpu_band: 0.0, land: 0.0, gpu_back: false, gpu_shading: false };
        let out = c.render();
        // well over on the left: covered by the back of the leaf
        assert_eq!(out[(10 * 80 + 20) * 4 + 3], 255);
    }
}

#[cfg(test)]
mod preview {
    use super::*;

    fn load(path: &str, w: u32, h: u32) -> Arc<RenderImage> {
        let img = image::open(path).unwrap().resize_exact(w, h, image::imageops::FilterType::Triangle);
        let mut rgba = img.into_rgba8();
        for p in rgba.chunks_exact_mut(4) {
            p.swap(0, 2);
        }
        Arc::new(RenderImage::new(vec![image::Frame::new(rgba)]))
    }

    /// CURL_FRONT=a.png CURL_BACK=b.png CURL_UNDER=c.png CURL_OUT=dir cargo test preview -- --ignored
    #[test]
    #[ignore]
    fn render_frames() {
        let (w, h) = (420.0f32, 560.0f32);
        let front = Src::new(load(&std::env::var("CURL_FRONT").unwrap(), w as u32, h as u32));
        let back = Src::new(load(&std::env::var("CURL_BACK").unwrap(), w as u32, h as u32));
        let under = image::open(std::env::var("CURL_UNDER").unwrap()).unwrap().resize_exact(w as u32, h as u32, image::imageops::FilterType::Triangle).into_rgba8();
        let out = std::env::var("CURL_OUT").unwrap();
        // straight pull from mid-edge, a lower-corner peel, a diagonal, nearly done
        let frames = [((w, h * 0.5), (w * 0.55, h * 0.5)), ((w, h * 0.9), (w * 0.5, h * 0.75)), ((w, h * 0.9), (w * 0.05, h * 0.55)), ((w, h * 0.85), (-w * 0.8, h * 0.8))];
        for (k, (o, f)) in frames.into_iter().enumerate() {
            let c = Curl { w, h, forward: true, origin: o, finger: f, front: Some(&front), back: Some(&back), paper: [242, 248, 250, 255], dark: false, gpu_band: 0.0, land: 0.0, gpu_back: false, gpu_shading: false };
            let t0 = std::time::Instant::now();
            let buf = c.render();
            eprintln!("frame {k}: {:?}", t0.elapsed());
            let s = 1290.0 / w;
            let big = Curl { w: 1290.0, h: 1720.0, origin: (o.0 * s, o.1 * s), finger: (f.0 * s, f.1 * s), ..c };
            let fast = Curl { origin: (big.origin.0, big.h * 0.62), finger: (big.finger.0, big.h * 0.62), gpu_back: true, gpu_shading: true, gpu_band: 4.0, ..big };
            let tf = std::time::Instant::now();
            let fc = fast.render_cropped();
            eprintln!("  fast path (vertical fold, GPU flats): {:?} box {:?}", tf.elapsed(), fc.map(|c| (c.3, c.4)));
            let t2 = std::time::Instant::now();
            let _ = big.render_full();
            eprintln!("  render_full alone: {:?}", t2.elapsed());
            let t1 = std::time::Instant::now();
            let cropped = big.render_cropped();
            eprintln!("  device-res spread (2580x1720) + crop: {:?}  crop {:?}", t1.elapsed(), cropped.map(|c| (c.3, c.4)));
            let mut canvas = image::RgbaImage::from_pixel(2 * w as u32, h as u32, image::Rgba([233, 232, 228, 255]));
            // the spread underneath: a blank facing page and the next right page
            for y in 0..h as u32 {
                for x in 0..w as u32 {
                    canvas.put_pixel(x, y, image::Rgba([250, 248, 242, 255]));
                    canvas.put_pixel(x + w as u32, y, *under.get_pixel(x, y));
                }
            }
            for (i, px) in canvas.pixels_mut().enumerate() {
                let s = &buf[i * 4..i * 4 + 4];
                let a = s[3] as f32 / 255.0;
                let src = [s[2], s[1], s[0]];
                for c in 0..3 {
                    px[c] = (px[c] as f32 * (1.0 - a) + src[c] as f32 * a) as u8;
                }
            }
            canvas.save(format!("{out}/curl{k}.png")).unwrap();
        }
    }
}
