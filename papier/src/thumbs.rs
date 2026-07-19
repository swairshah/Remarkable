//! Home-grid thumbnails: entry 0 of each document at 1/4 scale (351x468
//! gray8), cached as <doc>/thumb.png and regenerated when state or ink is
//! newer than the cache. Generation is side-effect free (no Doc::open —
//! that would stamp the "opened" ordering).

use crate::fb::{SCREEN_H, SCREEN_W};
use crate::ink::Page;
use crate::png;
use crate::png_dec;
use crate::store::{docs_dir, read_json};

pub const THUMB_DIV: i32 = 4;
pub const THUMB_W: i32 = SCREEN_W / THUMB_DIV; /* 351 */
pub const THUMB_H: i32 = SCREEN_H / THUMB_DIV; /* 468 */

fn thumb_path(id: &str) -> String {
    format!("{}/{id}/thumb.png", docs_dir())
}

fn mtime(p: &str) -> u64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

/// Newest mtime under <doc>/ink/ (0 if none).
fn newest_ink(id: &str) -> u64 {
    let dir = format!("{}/{id}/ink", docs_dir());
    let Ok(rd) = std::fs::read_dir(&dir) else { return 0 };
    rd.flatten()
        .map(|e| mtime(&e.path().to_string_lossy()))
        .max()
        .unwrap_or(0)
}

/// The cached thumb if it exists and is fresh.
pub fn load_fresh(id: &str) -> Option<Vec<u8>> {
    let tp = thumb_path(id);
    let t = mtime(&tp);
    if t == 0 {
        return None;
    }
    let base = format!("{}/{id}", docs_dir());
    if t < mtime(&format!("{base}/state.json")) || t < newest_ink(id) {
        return None; /* stale */
    }
    let data = std::fs::read(&tp).ok()?;
    match png_dec::decode_png_gray(&data) {
        Ok((w, h, buf)) if w == THUMB_W as u32 && h == THUMB_H as u32 => Some(buf),
        _ => None,
    }
}

/// Render entry 0 fresh (raster + ink at 1/4 scale) and cache it.
pub fn generate(id: &str) -> Option<Vec<u8>> {
    let base = format!("{}/{id}", docs_dir());
    /* entry 0: state.json seq[0] if present, else pdf p.1 / note 1 */
    let meta = read_json(&format!("{base}/meta.json"))?;
    let notebook = meta["kind"].as_str() == Some("notebook");
    let (is_pdf, num) = match read_json(&format!("{base}/state.json"))
        .and_then(|st| st["seq"].as_array().and_then(|a| a.first().cloned()))
    {
        Some(e) => match (e["p"].as_u64(), e["n"].as_u64()) {
            (Some(p), _) => (true, p),
            (_, Some(n)) => (false, n),
            _ => (!notebook, if notebook { 1 } else { 0 }),
        },
        None => (!notebook, if notebook { 1 } else { 0 }),
    };

    let mut buf = vec![255u8; (THUMB_W * THUMB_H) as usize];
    if is_pdf {
        if let Ok(data) = std::fs::read(format!("{base}/pages/{:04}.png", num + 1)) {
            if let Ok((w, h, raster)) = png_dec::decode_png_gray(&data) {
                if w == SCREEN_W as u32 && h == SCREEN_H as u32 {
                    box_filter(&raster, &mut buf);
                }
            }
        }
    }
    let ink_file = if is_pdf {
        format!("{base}/ink/pdf-{:04}.json", num + 1)
    } else {
        format!("{base}/ink/note-{:04}.json", num)
    };
    if let Some(page) = Page::load(&ink_file) {
        page.snapshot_into(&mut buf, THUMB_DIV);
    }

    let _ = std::fs::write(thumb_path(id), png::encode_gray(THUMB_W as u32, THUMB_H as u32, &buf));
    Some(buf)
}

fn box_filter(src: &[u8], out: &mut [u8]) {
    let n = (THUMB_DIV * THUMB_DIV) as u32;
    for y in 0..THUMB_H {
        for x in 0..THUMB_W {
            let mut acc = 0u32;
            for j in 0..THUMB_DIV {
                for i in 0..THUMB_DIV {
                    acc += src[((y * THUMB_DIV + j) * SCREEN_W + x * THUMB_DIV + i) as usize] as u32;
                }
            }
            out[(y * THUMB_W + x) as usize] = (acc / n) as u8;
        }
    }
}
