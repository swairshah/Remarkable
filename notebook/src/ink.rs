//! The page model now lives in libreink-page; this module re-exports it
//! and keeps what is genuinely this app's: the `Notebook` page container
//! and the white-fill render helpers the notebook uses (a note page has no
//! book raster underneath, so re-rendering always starts from white).

use libreink_core::fb::Framebuffer;

pub use libreink_page::*;

/// Fill-white-then-stamp, the notebook's re-render. The bool mirrors the
/// old API ("prefer the 16-level waveform") and is always false: screen
/// ink is uniformly black, which DU renders crisply.
pub trait RenderExt {
    fn render_region(&self, fb: &mut Framebuffer, r: Rect) -> bool;
    fn render_full(&self, fb: &mut Framebuffer);
}

impl RenderExt for Page {
    fn render_region(&self, fb: &mut Framebuffer, r: Rect) -> bool {
        let r = r.clamp_screen();
        fb.fill_rect(r.x0, r.y0, r.w(), r.h(), crate::draw::WHITE);
        self.stamp_region(fb, r);
        false
    }
    fn render_full(&self, fb: &mut Framebuffer) {
        self.render_region(
            fb,
            Rect { x0: 0, y0: 0, x1: libreink_core::fb::SCREEN_W - 1, y1: libreink_core::fb::SCREEN_H - 1 },
        );
    }
}

/* ---- the notebook: an ordered set of page files -------------------------- */

pub struct Notebook {
    dir: String,
    pub current: usize,
    pub count: usize,
    pub page: Page,
}

fn data_dir() -> String {
    if let Ok(d) = std::env::var("NOTEBOOK_DATA_DIR") {
        return d;
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/root".into());
    format!("{home}/.local/share/notebook/pages")
}

impl Notebook {
    pub fn open() -> Notebook {
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
        println!("notebook: data dir {dir} ({count} pages, opening page {})", current + 1);
        Notebook { dir, current, count, page }
    }

    fn path_of(dir: &str, i: usize) -> String {
        format!("{dir}/page-{:04}.json", i + 1)
    }

    pub fn page_path(&self, i: usize) -> String {
        Self::path_of(&self.dir, i)
    }

    pub fn save_current(&mut self) {
        if self.page.dirty || !std::path::Path::new(&Self::path_of(&self.dir, self.current)).exists() {
            let path = Self::path_of(&self.dir, self.current);
            if let Err(e) = self.page.save(&path) {
                eprintln!("notebook: save {path}: {e}");
            }
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
            if self.page.is_empty() {
                return false; /* don't stack empty pages */
            }
            self.save_current();
            self.count += 1;
            self.current = self.count - 1;
            self.page = Page::default();
            return true;
        }
        self.save_current();
        self.current = target;
        self.page = Page::load(&Self::path_of(&self.dir, target)).unwrap_or_default();
        true
    }
}
