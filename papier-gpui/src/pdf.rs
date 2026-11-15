//! Book pages straight from the source PDF, at exactly the size they're
//! shown — the way the web viewer uses PDF.js — so text stays crisp at any
//! zoom, in both views and through the page curl. Rendering is poppler's
//! `pdftoppm` (vector text, anti-aliased) on background threads; the page
//! is laid onto the 1404×1872 papier page exactly as mkbook lays it (crop
//! rect inside a 30px border, or the page inside its margins), so ink made
//! on the tablet lands where it was written.

use std::{path::Path, process::Command};

use tiny_skia::{IntSize, Pixmap, PixmapPaint, Transform};

pub const PAGE_W: f32 = 1404.0;
pub const PAGE_H: f32 = 1872.0;
const BORDER: f32 = 30.0;

/// Every page's size in points, from `pdfinfo`.
pub fn page_sizes(pdf: &Path) -> Option<Vec<(f32, f32)>> {
    let out = Command::new("pdfinfo").args(["-f", "1", "-l", "100000"]).arg(pdf).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let sizes: Vec<(f32, f32)> = text
        .lines()
        .filter_map(|l| {
            // "Page    3 size: 444.96 x 594.96 pts"
            let rest = l.strip_prefix("Page")?.split_once("size:")?.1;
            let mut it = rest.split_whitespace();
            let w = it.next()?.parse().ok()?;
            it.next()?;
            let h = it.next()?.parse().ok()?;
            Some((w, h))
        })
        .collect();
    (!sizes.is_empty()).then_some(sizes)
}

/// Where a PDF page goes on the papier page (device units): its scale k
/// (device units per point), the crop rect (fractions), and the top-left
/// of the placed region.
pub fn placement(size: (f32, f32), crop: Option<&[f64]>, margins: Option<&[f64]>) -> (f32, [f32; 4], f32, f32) {
    let (pw, ph) = size;
    match crop.filter(|c| c.len() == 4) {
        Some(c) => {
            let c = [c[0] as f32, c[1] as f32, c[2] as f32, c[3] as f32];
            let (cw, ch) = ((c[2] - c[0]) * pw, (c[3] - c[1]) * ph);
            let k = ((PAGE_W - 2.0 * BORDER) / cw).min((PAGE_H - 2.0 * BORDER) / ch);
            (k, c, (PAGE_W - cw * k) / 2.0, (PAGE_H - ch * k) / 2.0)
        }
        None => {
            let m: Vec<f32> = margins.filter(|m| m.len() == 4).map_or(vec![40.0; 4], |m| m.iter().map(|v| *v as f32).collect());
            let (bw, bh) = (PAGE_W - m[0] - m[2], PAGE_H - m[1] - m[3]);
            let k = (bw / pw).min(bh / ph);
            (k, [0.0, 0.0, 1.0, 1.0], m[0] + (bw - pw * k) / 2.0, m[1] + (bh - ph * k) / 2.0)
        }
    }
}

/// Render page `page0` (0-based) onto a white `out_w`×`out_h` pixmap of
/// the whole papier page.
pub fn render(pdf: &Path, page0: u32, size: (f32, f32), crop: Option<&[f64]>, margins: Option<&[f64]>, out_w: u32, out_h: u32) -> Option<Pixmap> {
    let z = out_w as f32 / PAGE_W;
    let (k, c, ox, oy) = placement(size, crop, margins);
    // the whole PDF page at its placed scale, in output pixels
    let (rw, rh) = ((size.0 * k * z).round().max(1.0), (size.1 * k * z).round().max(1.0));
    let (x, y) = ((c[0] * rw).round(), (c[1] * rh).round());
    let (w, h) = (((c[2] - c[0]) * rw).round().max(1.0), ((c[3] - c[1]) * rh).round().max(1.0));
    let n = (page0 + 1).to_string();
    let out = Command::new("pdftoppm")
        .args(["-f", &n, "-l", &n, "-aa", "yes", "-aaVector", "yes"])
        .args(["-scale-to-x", &(rw as u32).to_string(), "-scale-to-y", &(rh as u32).to_string()])
        .args(["-x", &(x as u32).to_string(), "-y", &(y as u32).to_string(), "-W", &(w as u32).to_string(), "-H", &(h as u32).to_string()])
        .arg(pdf)
        .output()
        .ok()?;
    let region = parse_ppm(&out.stdout)?;
    let mut page = Pixmap::new(out_w, out_h)?;
    page.fill(tiny_skia::Color::WHITE);
    page.draw_pixmap((ox * z).round() as i32, (oy * z).round() as i32, region.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
    Some(page)
}

/// Binary PPM (P6, maxval 255) -> opaque pixmap.
fn parse_ppm(data: &[u8]) -> Option<Pixmap> {
    let mut fields = Vec::new();
    let mut i = 0;
    while fields.len() < 4 && i < data.len() {
        while i < data.len() && data[i].is_ascii_whitespace() {
            i += 1;
        }
        if data.get(i) == Some(&b'#') {
            while i < data.len() && data[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        let start = i;
        while i < data.len() && !data[i].is_ascii_whitespace() {
            i += 1;
        }
        fields.push(std::str::from_utf8(&data[start..i]).ok()?.to_owned());
    }
    if fields.first().map(String::as_str) != Some("P6") || fields.get(3).map(String::as_str) != Some("255") {
        return None;
    }
    let (w, h): (u32, u32) = (fields[1].parse().ok()?, fields[2].parse().ok()?);
    let pixels = data.get(i + 1..i + 1 + (w * h * 3) as usize)?;
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for p in pixels.chunks_exact(3) {
        rgba.extend_from_slice(&[p[0], p[1], p[2], 255]);
    }
    Pixmap::from_vec(rgba, IntSize::from_wh(w, h)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ppm_parses() {
        let mut d = b"P6\n# c\n2 1\n255\n".to_vec();
        d.extend_from_slice(&[255, 0, 0, 0, 0, 255]);
        let p = parse_ppm(&d).unwrap();
        assert_eq!((p.width(), p.height()), (2, 1));
        assert_eq!(&p.data()[..4], &[255, 0, 0, 255]);
    }

    #[test]
    fn full_crop_fits_inside_the_border() {
        let (k, _, ox, oy) = placement((445.0, 595.0), Some(&[0.0, 0.0, 1.0, 1.0]), None);
        assert!(ox >= BORDER - 0.5 && oy >= BORDER - 0.5);
        assert!((445.0 * k + 2.0 * ox - PAGE_W).abs() < 1.0);
    }
}

#[cfg(test)]
mod check {
    /// PDF=f.pdf PAGE=2 W=1404 OUT=o.png cargo test pdf::check -- --ignored
    #[test]
    #[ignore]
    fn render_page() {
        let pdf = std::path::PathBuf::from(std::env::var("PDF").unwrap());
        let page: u32 = std::env::var("PAGE").unwrap().parse().unwrap();
        let w: u32 = std::env::var("W").unwrap().parse().unwrap();
        let sizes = super::page_sizes(&pdf).unwrap();
        let h = (w as f32 * 1872.0 / 1404.0).round() as u32;
        let t = std::time::Instant::now();
        let px = super::render(&pdf, page, sizes[page as usize], Some(&[0.0, 0.0, 1.0, 1.0]), None, w, h).unwrap();
        eprintln!("rendered {w}x{h} in {:?}", t.elapsed());
        px.save_png(std::env::var("OUT").unwrap()).unwrap();
    }
}
