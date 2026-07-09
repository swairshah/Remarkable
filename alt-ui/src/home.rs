//! The home screen: a xochitl-like 3-column document grid under the status
//! bar. Folders first, then documents; tap opens, long-press offers the
//! document menu; vertical swipes page by whole rows (no smooth scrolling
//! on e-ink). Thumbnails render lazily — one per poll-loop pass — so a
//! cold start never blocks input.

use crate::draw::{text_width, BLACK, GRAY, LIGHT, WHITE};
use crate::fb::{Framebuffer, SCREEN_H, SCREEN_W};
use crate::statusbar::STATUS_H;
use crate::store::{self, DocInfo};
use crate::text;
use crate::thumbs::{self, THUMB_H, THUMB_W};

pub const HEADER_H: i32 = 150;
pub const GRID_Y0: i32 = STATUS_H + HEADER_H;
pub const COLS: i32 = 3;
pub const CELL_W: i32 = SCREEN_W / COLS; /* 468 */
pub const CELL_H: i32 = 550;
pub const ROWS: i32 = (SCREEN_H - GRID_Y0) / CELL_H; /* 3 */

/* header buttons (right-aligned) */
pub const NEW_BTN_W: i32 = 300;
pub const SORT_BTN_W: i32 = 220;
pub const HDR_BTN_H: i32 = 64;

#[derive(Clone, Copy, PartialEq)]
pub enum Sort {
    Opened,
    Title,
}

pub enum Cell {
    Folder(String),
    Doc(DocInfo),
}

pub struct HomeView {
    pub folder: Option<String>, /* None = root */
    pub cells: Vec<Cell>,
    pub top_row: usize,
    pub thumbs: Vec<Option<Vec<u8>>>, /* parallel to cells */
    pub pending: Vec<usize>,          /* cell indices needing a thumb */
    pub sig: String,                  /* content fingerprint, for rescan */
}

impl HomeView {
    pub fn build(folder: Option<String>, sort: Sort) -> HomeView {
        let mut docs = store::scan();
        if sort == Sort::Title {
            docs.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
        }
        let mut cells: Vec<Cell> = Vec::new();
        match &folder {
            None => {
                for f in store::folders() {
                    cells.push(Cell::Folder(f));
                }
                cells.extend(docs.into_iter().filter(|d| d.folder.is_empty()).map(Cell::Doc));
            }
            Some(f) => {
                cells.extend(docs.into_iter().filter(|d| &d.folder == f).map(Cell::Doc));
            }
        }
        let thumbs = cells
            .iter()
            .map(|c| match c {
                Cell::Doc(d) => thumbs::load_fresh(&d.id),
                Cell::Folder(_) => None,
            })
            .collect::<Vec<_>>();
        let pending = cells
            .iter()
            .enumerate()
            .filter(|(i, c)| matches!(c, Cell::Doc(_)) && thumbs[*i].is_none())
            .map(|(i, _)| i)
            .collect();
        /* fingerprint the content so the poll loop can cheaply detect a doc
         * that a background sync pulled in (the id set or an mtime changed) */
        let sig = cells
            .iter()
            .map(|c| match c {
                Cell::Folder(f) => format!("f:{f}"),
                Cell::Doc(d) => format!("d:{}:{}", d.id, d.opened),
            })
            .collect::<Vec<_>>()
            .join("|");
        HomeView { folder, cells, top_row: 0, thumbs, pending, sig }
    }

    /// Generate ONE missing thumbnail (called from the idle loop); true if
    /// a repaint of that cell is due.
    pub fn generate_one_thumb(&mut self) -> Option<usize> {
        let i = self.pending.pop()?;
        if let Cell::Doc(d) = &self.cells[i] {
            self.thumbs[i] = thumbs::generate(&d.id);
        }
        Some(i)
    }

    pub fn visible(&self) -> std::ops::Range<usize> {
        let a = self.top_row * COLS as usize;
        (a.min(self.cells.len()))..((a + (COLS * ROWS) as usize).min(self.cells.len()))
    }

    pub fn cell_rect(&self, i: usize) -> Option<(i32, i32)> {
        let vis = self.visible();
        if !vis.contains(&i) {
            return None;
        }
        let k = (i - vis.start) as i32;
        Some((k % COLS * CELL_W, GRID_Y0 + k / COLS * CELL_H))
    }

    pub fn cell_at(&self, x: i32, y: i32) -> Option<usize> {
        if y < GRID_Y0 {
            return None;
        }
        let (c, r) = ((x / CELL_W).min(COLS - 1), (y - GRID_Y0) / CELL_H);
        if r >= ROWS {
            return None;
        }
        let i = self.top_row * COLS as usize + (r * COLS + c) as usize;
        (i < self.cells.len()).then_some(i)
    }

    pub fn scroll(&mut self, down: bool) -> bool {
        let rows_total = self.cells.len().div_ceil(COLS as usize);
        if down {
            if (self.top_row + ROWS as usize) < rows_total {
                self.top_row += ROWS as usize;
                return true;
            }
        } else if self.top_row > 0 {
            self.top_row = self.top_row.saturating_sub(ROWS as usize);
            return true;
        }
        false
    }

    /* -- rendering ---------------------------------------------------- */

    /// Paint the header + grid (NOT the status bar — main.rs owns that).
    pub fn render(&self, fb: &mut Framebuffer, sort: Sort) {
        fb.fill_rect(0, STATUS_H, SCREEN_W, SCREEN_H - STATUS_H, WHITE);

        /* header: breadcrumb + buttons */
        let title = match &self.folder {
            None => "My files".to_string(),
            Some(f) => format!("< {f}"),
        };
        text::draw_line(fb, 48, STATUS_H + 36, text::Face::Heading, 48.0, &title);

        let by = STATUS_H + 40;
        let nx = SCREEN_W - 48 - NEW_BTN_W;
        fb.fill_rect(nx, by, NEW_BTN_W, HDR_BTN_H, BLACK);
        let nl = "+ NOTEBOOK";
        fb.text(nx + (NEW_BTN_W - text_width(nl, 3)) / 2, by + (HDR_BTN_H - 21) / 2, nl, 3, WHITE);
        let sx = nx - 24 - SORT_BTN_W;
        fb.rect_outline(sx, by, SORT_BTN_W, HDR_BTN_H, 2, BLACK);
        let sl = match sort {
            Sort::Opened => "RECENT",
            Sort::Title => "A - Z",
        };
        fb.text(sx + (SORT_BTN_W - text_width(sl, 3)) / 2, by + (HDR_BTN_H - 21) / 2, sl, 3, BLACK);

        fb.fill_rect(0, GRID_Y0 - 4, SCREEN_W, 2, BLACK);

        if self.cells.is_empty() {
            text::draw_line(
                fb,
                48,
                GRID_Y0 + 60,
                text::Face::Body,
                34.0,
                "Nothing here yet.",
            );
            text::draw_line(
                fb,
                48,
                GRID_Y0 + 120,
                text::Face::Body,
                30.0,
                "make book FILE=paper.pdf HOST=root@<tablet-ip>",
            );
            return;
        }

        for i in self.visible() {
            self.render_cell(fb, i);
        }

        /* page dots when there's more than one screenful */
        let rows_total = self.cells.len().div_ceil(COLS as usize);
        let pages = rows_total.div_ceil(ROWS as usize);
        if pages > 1 {
            let cur = self.top_row / ROWS as usize;
            let y = SCREEN_H - 26;
            let mut x = SCREEN_W / 2 - (pages as i32 * 28) / 2;
            for p in 0..pages {
                if p == cur {
                    fb.disc(x, y, 7, BLACK);
                } else {
                    fb.disc(x, y, 4, GRAY);
                }
                x += 28;
            }
        }
    }

    pub fn render_cell(&self, fb: &mut Framebuffer, i: usize) {
        let Some((x, y)) = self.cell_rect(i) else { return };
        fb.fill_rect(x, y, CELL_W, CELL_H, WHITE);
        let tx = x + (CELL_W - THUMB_W) / 2;
        let ty = y + 14;
        match &self.cells[i] {
            Cell::Folder(name) => {
                /* a folder glyph: tab + body */
                let (fx, fy, fw, fh) = (tx, ty + 90, THUMB_W, THUMB_H - 180);
                fb.fill_rect(fx, fy - 26, 120, 26, LIGHT);
                fb.rect_outline(fx, fy - 26, 120, 26, 2, BLACK);
                fb.fill_rect(fx, fy, fw, fh, LIGHT);
                fb.rect_outline(fx, fy, fw, fh, 2, BLACK);
                center_label(fb, x, y + 14 + THUMB_H + 14, name, 30.0);
            }
            Cell::Doc(d) => {
                match &self.thumbs[i] {
                    Some(buf) => fb.blit_gray(tx, ty, THUMB_W, THUMB_H, buf),
                    None => {
                        fb.fill_rect(tx, ty, THUMB_W, THUMB_H, WHITE);
                        let l = "...";
                        fb.text(
                            tx + (THUMB_W - text_width(l, 2)) / 2,
                            ty + THUMB_H / 2,
                            l,
                            2,
                            GRAY,
                        );
                    }
                }
                fb.rect_outline(tx, ty, THUMB_W, THUMB_H, 1, BLACK);
                center_label(fb, x, ty + THUMB_H + 12, &d.title, 30.0);
                let meta = format!("{}  -  {}", rel_date(d.opened), pages_label(d));
                let mw = text_width(&meta, 2);
                fb.text(x + (CELL_W - mw) / 2, ty + THUMB_H + 54, &meta, 2, GRAY);
            }
        }
    }
}

fn pages_label(d: &DocInfo) -> String {
    let n = match d.kind {
        crate::doc::DocKind::Book => d.pages,
        crate::doc::DocKind::Notebook => d.seq_len,
    };
    if d.pos > 0 {
        format!("at {} of {}", d.pos + 1, d.seq_len)
    } else if n == 1 {
        "1 page".into()
    } else {
        format!("{n} pages")
    }
}

fn center_label(fb: &mut Framebuffer, cell_x: i32, y: i32, s: &str, px: f32) {
    let mut t = s.to_string();
    while text::width(text::Face::Body, px, &t) > CELL_W - 40 && t.chars().count() > 4 {
        t = t.chars().take(t.chars().count() - 4).collect();
        t.push_str("..");
    }
    let w = text::width(text::Face::Body, px, &t);
    text::draw_line(fb, cell_x + (CELL_W - w) / 2, y, text::Face::Body, px, &t);
}

/// "Today", "Yesterday", "N days ago" from a unix-seconds stamp.
fn rel_date(opened: u64) -> String {
    if opened == 0 {
        return "new".into();
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let days = now.saturating_sub(opened) / 86400;
    match days {
        0 => "Today".into(),
        1 => "Yesterday".into(),
        n => format!("{n} days ago"),
    }
}
