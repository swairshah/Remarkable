//! The page model lives in libreink-page; this module re-exports it and
//! keeps what is genuinely this app's: the `Sketchbook` page container,
//! the per-page AI RENDER LAYER (a grayscale raster the agent generates
//! from the user's sketch, blitted under the strokes on the right panel),
//! and the fill-then-stamp re-render helpers.
//!
//! A sketchbook page is a SPREAD: the left panel (x < PANEL_W) is the
//! user's sketch — vector pen strokes, erasable — and the right panel is
//! the agent's rendered version of it, a 16-gray raster. Re-rendering a
//! region paints white, blits the raster where it applies, then stamps
//! strokes on top, so erasing ink never damages the render and vice versa.

use libreink_core::fb::{Framebuffer, SCREEN_H, SCREEN_W};

pub use libreink_page::*;

/// The divider: left of it the user sketches, right of it the agent renders.
pub const PANEL_W: i32 = SCREEN_W / 2;

/* ---- the AI render layer -------------------------------------------------- */

/// One grayscale raster placed on the page (the agent's render of the
/// sketch). Stored at final on-page size; (x0,y0) is its top-left in page
/// coordinates.
pub struct RenderLayer {
    pub x0: i32,
    pub y0: i32,
    pub w: i32,
    pub h: i32,
    pub gray: Vec<u8>, /* w*h, row-major, 0=black 255=white */
}

const RENDER_MAGIC: &[u8; 4] = b"SKR1";

impl RenderLayer {
    pub fn rect(&self) -> Rect {
        Rect { x0: self.x0, y0: self.y0, x1: self.x0 + self.w - 1, y1: self.y0 + self.h - 1 }
    }

    /// Paint the raster's intersection with `clip` into the framebuffer.
    pub fn blit(&self, fb: &mut Framebuffer, clip: Rect) {
        let r = self.rect();
        let x0 = clip.x0.max(r.x0).max(0);
        let y0 = clip.y0.max(r.y0).max(0);
        let x1 = clip.x1.min(r.x1).min(SCREEN_W - 1);
        let y1 = clip.y1.min(r.y1).min(SCREEN_H - 1);
        for y in y0..=y1 {
            let row = ((y - self.y0) * self.w) as usize;
            for x in x0..=x1 {
                let g = grain_px(self.gray[row + (x - self.x0) as usize], x, y);
                fb.px(x, y, gray_to_565(gray16(g)));
            }
        }
    }

    /// Darkest-wins plot into a 1/`div` snapshot buffer (before strokes).
    pub fn blit_snapshot(&self, buf: &mut [u8], div: i32) {
        let (bw, bh) = (SCREEN_W / div, SCREEN_H / div);
        for by in 0..bh {
            let y = by * div;
            if y < self.y0 || y >= self.y0 + self.h {
                continue;
            }
            let row = ((y - self.y0) * self.w) as usize;
            for bx in 0..bw {
                let x = bx * div;
                if x < self.x0 || x >= self.x0 + self.w {
                    continue;
                }
                let g = grain_px(self.gray[row + (x - self.x0) as usize], x, y);
                let idx = (by * bw + bx) as usize;
                if g < buf[idx] {
                    buf[idx] = g;
                }
            }
        }
    }

    pub fn save(&self, path: &str) -> std::io::Result<()> {
        let mut out = Vec::with_capacity(20 + self.gray.len());
        out.extend_from_slice(RENDER_MAGIC);
        for v in [self.x0, self.y0, self.w, self.h] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&self.gray);
        std::fs::write(path, out)
    }

    pub fn load(path: &str) -> Option<RenderLayer> {
        let b = std::fs::read(path).ok()?;
        if b.len() < 20 || &b[0..4] != RENDER_MAGIC {
            return None;
        }
        let rd = |i: usize| i32::from_le_bytes(b[i..i + 4].try_into().unwrap());
        let (x0, y0, w, h) = (rd(4), rd(8), rd(12), rd(16));
        if w <= 0 || h <= 0 || b.len() < 20 + (w * h) as usize {
            return None;
        }
        Some(RenderLayer { x0, y0, w, h, gray: b[20..20 + (w * h) as usize].to_vec() })
    }
}

/// Snap an 8-bit gray to the 16 levels GC16 can actually show.
fn gray16(g: u8) -> u8 {
    ((g as u32 / 17) * 17) as u8
}

fn gray_to_565(g: u8) -> u16 {
    let g = g as u16;
    ((g >> 3) << 11) | ((g >> 2) << 5) | (g >> 3)
}

/// Bilinear-resize a grayscale image to (dw, dh).
pub fn resize_gray(src: &[u8], sw: i32, sh: i32, dw: i32, dh: i32) -> Vec<u8> {
    let mut out = vec![255u8; (dw * dh) as usize];
    if sw <= 0 || sh <= 0 || dw <= 0 || dh <= 0 {
        return out;
    }
    for y in 0..dh {
        let fy = (y as f32 + 0.5) * sh as f32 / dh as f32 - 0.5;
        let y0 = (fy.floor() as i32).clamp(0, sh - 1);
        let y1 = (y0 + 1).min(sh - 1);
        let ty = (fy - y0 as f32).clamp(0.0, 1.0);
        for x in 0..dw {
            let fx = (x as f32 + 0.5) * sw as f32 / dw as f32 - 0.5;
            let x0 = (fx.floor() as i32).clamp(0, sw - 1);
            let x1 = (x0 + 1).min(sw - 1);
            let tx = (fx - x0 as f32).clamp(0.0, 1.0);
            let p = |xx: i32, yy: i32| src[(yy * sw + xx) as usize] as f32;
            let top = p(x0, y0) * (1.0 - tx) + p(x1, y0) * tx;
            let bot = p(x0, y1) * (1.0 - tx) + p(x1, y1) * tx;
            out[(y * dw + x) as usize] = (top * (1.0 - ty) + bot * ty).round() as u8;
        }
    }
    out
}

/* ---- pencil grain ---------------------------------------------------------- */

fn hash2(x: i32, y: i32, seed: u32) -> f32 {
    let mut h = (x as u32)
        .wrapping_mul(374_761_393)
        ^ (y as u32).wrapping_mul(668_265_263)
        ^ seed.wrapping_mul(2_246_822_519);
    h = (h ^ (h >> 13)).wrapping_mul(1_274_126_177);
    ((h ^ (h >> 16)) & 0xFFFF) as f32 / 65535.0
}

/// Bilinear value noise at (x, y), one octave, in [0, 1].
fn value_noise(x: f32, y: f32, seed: u32) -> f32 {
    let (x0, y0) = (x.floor() as i32, y.floor() as i32);
    let (tx, ty) = (x - x0 as f32, y - y0 as f32);
    let a = hash2(x0, y0, seed);
    let b = hash2(x0 + 1, y0, seed);
    let c = hash2(x0, y0 + 1, seed);
    let d = hash2(x0 + 1, y0 + 1, seed);
    a * (1.0 - tx) * (1.0 - ty) + b * tx * (1.0 - ty) + c * (1.0 - tx) * ty + d * tx * ty
}

/// Graphite tooth, one pixel at a time: perturb midtones with paper-grain
/// value noise (a fine and a medium octave) so the 16-level quantize
/// renders granular pencil texture instead of flat posterized bands — the
/// reMarkable pencil-brush look. Strength peaks in the midtones (^0.7
/// widens the toothy band into lights and darks) and vanishes at paper
/// white (stays clean) and solid black (lines stay crisp).
///
/// (x, y) are PAGE coordinates: the noise field is deterministic, so
/// partial repaints tile seamlessly with earlier blits. The STORED raster
/// stays clean — grain exists only on the panel and in snapshots — so
/// edit-mode round trips (render_get → model → render) never compound it.
pub fn grain_px(g: u8, x: i32, y: i32) -> u8 {
    let strength = grain_strength();
    if strength <= 0.0 {
        return g;
    }
    let g = g as f32;
    if g >= 250.0 {
        return g as u8; /* paper */
    }
    let (fx, fy) = (x as f32, y as f32);
    /* two octaves, centered on 0, typical spread ≈ ±0.2 */
    let n = 0.6 * value_noise(fx / 2.0, fy / 2.0, 7)
        + 0.4 * value_noise(fx / 5.0, fy / 5.0, 13)
        - 0.5;
    let k = ((g * (255.0 - g)) / (127.5 * 127.5)).powf(0.7);
    (g + n * strength * k).clamp(0.0, 255.0) as u8
}

/// SKETCHBOOK_GRAIN: 0 disables, 1.0 is the default 175-amplitude tooth,
/// other values scale it (e.g. 1.4 for even grittier).
fn grain_strength() -> f32 {
    static S: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *S.get_or_init(|| {
        std::env::var("SKETCHBOOK_GRAIN")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
            .map(|v| v * 175.0)
            .unwrap_or(175.0)
    })
}

/// Minimal base64 decode (standard alphabet, padding optional).
pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut acc: u32 = 0;
    let mut bits = 0;
    for &c in s.as_bytes() {
        if c == b'=' || c == b'\n' || c == b'\r' {
            continue;
        }
        acc = (acc << 6) | val(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/* ---- re-render helpers ----------------------------------------------------- */

/// Fill-white, blit the render layer, stamp strokes, redraw the divider.
/// The bool is "prefer the 16-level waveform" — true when the region
/// touches the gray raster (DU would posterize it).
pub trait RenderExt {
    fn render_region(&self, fb: &mut Framebuffer, r: Rect, render: Option<&RenderLayer>) -> bool;
    fn render_full(&self, fb: &mut Framebuffer, render: Option<&RenderLayer>);
}

fn draw_divider(fb: &mut Framebuffer, r: Rect) {
    if r.x0 <= PANEL_W && r.x1 >= PANEL_W - 1 {
        let y0 = r.y0.max(0);
        let y1 = r.y1.min(SCREEN_H - 1);
        fb.fill_rect(PANEL_W - 1, y0, 2, y1 - y0 + 1, crate::draw::BLACK);
    }
}

impl RenderExt for Page {
    fn render_region(&self, fb: &mut Framebuffer, r: Rect, render: Option<&RenderLayer>) -> bool {
        let r = r.clamp_screen();
        fb.fill_rect(r.x0, r.y0, r.w(), r.h(), crate::draw::WHITE);
        let mut had_gray = false;
        if let Some(rl) = render {
            let rr = rl.rect();
            if rr.x1 >= r.x0 && rr.x0 <= r.x1 && rr.y1 >= r.y0 && rr.y0 <= r.y1 {
                rl.blit(fb, r);
                had_gray = true;
            }
        }
        self.stamp_region(fb, r);
        draw_divider(fb, r);
        had_gray
    }
    fn render_full(&self, fb: &mut Framebuffer, render: Option<&RenderLayer>) {
        self.render_region(
            fb,
            Rect { x0: 0, y0: 0, x1: SCREEN_W - 1, y1: SCREEN_H - 1 },
            render,
        );
    }
}

/// Snapshot with the render layer underneath the strokes (what pi sees).
pub fn snapshot_with_render(page: &Page, render: Option<&RenderLayer>, div: i32) -> (i32, i32, Vec<u8>) {
    let (w, h) = (SCREEN_W / div, SCREEN_H / div);
    let mut buf = vec![255u8; (w * h) as usize];
    if let Some(rl) = render {
        rl.blit_snapshot(&mut buf, div);
    }
    /* the divider, so pi sees the spread structure */
    let dx = (PANEL_W / div).min(w - 1);
    for y in 0..h {
        buf[(y * w + dx) as usize] = 0;
    }
    page.snapshot_into(&mut buf, div);
    (w, h, buf)
}

/* ---- the sketchbook: an ordered set of page files -------------------------- */

pub struct Sketchbook {
    dir: String,
    pub current: usize,
    pub count: usize,
    pub page: Page,
    pub render: Option<RenderLayer>,
    pub render_dirty: bool,
}

fn data_dir() -> String {
    if let Ok(d) = std::env::var("SKETCHBOOK_DATA_DIR") {
        return d;
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/root".into());
    format!("{home}/.local/share/sketchbook/pages")
}

impl Sketchbook {
    pub fn open() -> Sketchbook {
        let dir = data_dir();
        let _ = std::fs::create_dir_all(&dir);
        let mut count = 0usize;
        while std::path::Path::new(&Self::path_of(&dir, count)).exists() {
            count += 1;
        }
        let current = count.saturating_sub(1);
        let page = if count > 0 {
            Page::load(&Self::path_of(&dir, current)).unwrap_or_default()
        } else {
            count = 1;
            Page::default()
        };
        let render = RenderLayer::load(&Self::render_path_of(&dir, current));
        println!("sketchbook: data dir {dir} ({count} pages, opening page {})", current + 1);
        Sketchbook { dir, current, count, page, render, render_dirty: false }
    }

    fn path_of(dir: &str, i: usize) -> String {
        format!("{dir}/page-{:04}.json", i + 1)
    }

    fn render_path_of(dir: &str, i: usize) -> String {
        format!("{dir}/render-{:04}.skr", i + 1)
    }

    pub fn page_path(&self, i: usize) -> String {
        Self::path_of(&self.dir, i)
    }

    pub fn render_path(&self, i: usize) -> String {
        Self::render_path_of(&self.dir, i)
    }

    pub fn save_current(&mut self) {
        if self.page.dirty || !std::path::Path::new(&Self::path_of(&self.dir, self.current)).exists() {
            let path = Self::path_of(&self.dir, self.current);
            if let Err(e) = self.page.save(&path) {
                eprintln!("sketchbook: save {path}: {e}");
            }
        }
        if self.render_dirty {
            let path = Self::render_path_of(&self.dir, self.current);
            match &self.render {
                Some(rl) => {
                    if let Err(e) = rl.save(&path) {
                        eprintln!("sketchbook: save render {path}: {e}");
                    }
                }
                None => {
                    let _ = std::fs::remove_file(&path);
                }
            }
            self.render_dirty = false;
        }
    }

    /// Flip by `delta` pages. Forward past the last page creates a fresh one
    /// (quick-sheets style) unless the current last page is still empty.
    /// Returns false if nothing changed (already at an edge).
    pub fn flip(&mut self, delta: i32) -> bool {
        let target = self.current as i32 + delta;
        if target < 0 {
            return false;
        }
        let target = target as usize;
        if target >= self.count {
            if self.page.is_empty() && self.render.is_none() {
                return false; /* don't stack empty pages */
            }
            self.save_current();
            self.count += 1;
            self.current = self.count - 1;
            self.page = Page::default();
            self.render = None;
            self.render_dirty = false;
            return true;
        }
        self.save_current();
        self.current = target;
        self.page = Page::load(&Self::path_of(&self.dir, target)).unwrap_or_default();
        self.render = RenderLayer::load(&Self::render_path_of(&self.dir, target));
        self.render_dirty = false;
        true
    }
}
