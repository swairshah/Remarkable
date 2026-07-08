//! On-device book import: render a PDF from xochitl's store into a reader
//! bundle using a bundled `mutool` (MuPDF — the same engine the desk-side
//! mkbook uses, so output is identical).
//!
//! The work runs as a chain of short-lived mutool child processes, one
//! step at a time, polled from the app's main loop — the pen never waits:
//!
//!   info                 -> page count
//!   per page: stext      -> page size (pt) + char quads -> words + text
//!             draw -r    -> gray PNG scaled into the margin box
//!
//! Rasters are stored at their rendered size with a per-page offset in
//! meta.json ("offsets"); Book pads them onto the white canvas at decode
//! time. meta.json is written LAST, so a half-imported bundle never shows
//! up as a broken book on the shelf.

use crate::fb::{SCREEN_H, SCREEN_W};
use crate::xochitl;
use serde_json::{json, Value};
use std::io::Read;
use std::process::{Child, Command, Stdio};

/// Default guaranteed white border (device px) for on-device imports.
const MARGIN: i32 = 40;

fn mutool_dir() -> String {
    std::env::var("READER_MUTOOL").unwrap_or_else(|_| "/home/root/opt/mutool".into())
}

/// Is the bundled mutool present (deploy-mutool pushed it)?
pub fn available() -> bool {
    let d = mutool_dir();
    std::path::Path::new(&format!("{d}/mutool")).exists()
        && std::path::Path::new(&format!("{d}/ld-musl-armhf.so.1")).exists()
}

fn mutool_cmd(args: &[String]) -> Command {
    let d = mutool_dir();
    let loader = format!("{d}/ld-musl-armhf.so.1");
    let mut c = Command::new(loader);
    c.arg("--library-path").arg(&d).arg(format!("{d}/mutool"));
    c.args(args);
    c.stdout(Stdio::piped()).stderr(Stdio::null()).stdin(Stdio::null());
    c
}

#[derive(Clone)]
pub struct Job {
    pub uuid: String,
    pub title: String,
    pub slug: String,
}

enum Step {
    Info,
    Stext(usize), /* 0-based page being text-extracted */
    Draw(usize),  /* 0-based page being rendered */
}

pub enum Tick {
    Working(usize, usize), /* (done_pages, total_pages) */
    Finished(String),      /* slug */
    Failed(String),
}

pub struct Importer {
    pub job: Job,
    step: Step,
    child: Child,
    pages: usize,
    out: String,           /* bundle dir being built */
    stext_path: String,
    k: f32,                /* current page: pt -> px scale */
    offsets: Vec<(i32, i32)>,
}

impl Importer {
    pub fn start(job: Job, books_dir: &str) -> Result<Importer, String> {
        if !available() {
            return Err("mutool is not installed (make deploy-mutool)".into());
        }
        let out = format!("{books_dir}/{}", job.slug);
        let _ = std::fs::remove_dir_all(&out); /* stale partial import */
        std::fs::create_dir_all(format!("{out}/pages")).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(format!("{out}/text")).map_err(|e| e.to_string())?;
        let pdf = xochitl::pdf_path(&job.uuid);
        let child = mutool_cmd(&["info".into(), pdf])
            .spawn()
            .map_err(|e| format!("spawn mutool: {e}"))?;
        Ok(Importer {
            stext_path: format!("{out}/.stext.tmp"),
            job,
            step: Step::Info,
            child,
            pages: 0,
            out,
            k: 1.0,
            offsets: Vec::new(),
        })
    }

    /// Non-blocking: advance the state machine if the current child is
    /// done. Call every main-loop iteration.
    pub fn poll(&mut self) -> Tick {
        match self.child.try_wait() {
            Ok(None) => {
                let done = match self.step {
                    Step::Info => 0,
                    Step::Stext(p) | Step::Draw(p) => p,
                };
                return Tick::Working(done, self.pages.max(1));
            }
            Err(e) => return self.fail(format!("wait: {e}")),
            Ok(Some(st)) if !st.success() => {
                return self.fail(format!("mutool exited {:?} during {}", st.code(), self.step_name()));
            }
            Ok(Some(_)) => {}
        }
        /* the child finished; consume its output and start the next one */
        match self.step {
            Step::Info => {
                let mut s = String::new();
                if let Some(mut o) = self.child.stdout.take() {
                    let _ = o.read_to_string(&mut s);
                }
                let pages = s
                    .lines()
                    .find_map(|l| l.trim().strip_prefix("Pages: "))
                    .and_then(|n| n.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if pages == 0 {
                    return self.fail("could not read page count".into());
                }
                self.pages = pages;
                self.spawn_stext(0)
            }
            Step::Stext(p) => {
                let stext = std::fs::read_to_string(&self.stext_path).unwrap_or_default();
                let Some(page) = parse_stext(&stext) else {
                    return self.fail(format!("bad stext for page {}", p + 1));
                };
                /* fit into the margin box; words move by the same k */
                let (bw, bh) = (
                    (SCREEN_W - 2 * MARGIN) as f32,
                    (SCREEN_H - 2 * MARGIN) as f32,
                );
                self.k = (bw / page.w).min(bh / page.h);
                let dpi = 72.0 * self.k;
                let pdf = xochitl::pdf_path(&self.job.uuid);
                let png = format!("{}/pages/{:04}.png", self.out, p + 1);
                /* words/text are finished with a provisional centered
                 * offset; corrected after the draw reports actual dims */
                let ow = (page.w * self.k).round() as i32;
                let oh = (page.h * self.k).round() as i32;
                let ox = MARGIN + ((bw as i32 - ow) / 2).max(0);
                let oy = MARGIN + ((bh as i32 - oh) / 2).max(0);
                let words: Vec<Value> = page
                    .words
                    .iter()
                    .map(|w| {
                        json!([
                            (w.x0 * self.k) as i32 + ox,
                            (w.y0 * self.k) as i32 + oy,
                            (w.x1 * self.k + 0.5) as i32 + ox,
                            (w.y1 * self.k + 0.5) as i32 + oy,
                            w.text,
                        ])
                    })
                    .collect();
                let doc = json!({ "text": page.text, "words": words });
                let tpath = format!("{}/text/{:04}.json", self.out, p + 1);
                if std::fs::write(&tpath, serde_json::to_vec(&doc).unwrap_or_default()).is_err() {
                    return self.fail(format!("write {tpath}"));
                }
                self.offsets.push((ox, oy));
                let args: Vec<String> = vec![
                    "draw".into(),
                    "-F".into(),
                    "png".into(),
                    "-c".into(),
                    "gray".into(),
                    "-r".into(),
                    format!("{dpi:.4}"),
                    "-o".into(),
                    png,
                    pdf,
                    format!("{}", p + 1),
                ];
                match mutool_cmd(&args).spawn() {
                    Ok(c) => {
                        self.child = c;
                        self.step = Step::Draw(p);
                        Tick::Working(p, self.pages)
                    }
                    Err(e) => self.fail(format!("spawn draw: {e}")),
                }
            }
            Step::Draw(p) => {
                if p + 1 < self.pages {
                    self.spawn_stext(p + 1)
                } else {
                    self.finish()
                }
            }
        }
    }

    fn spawn_stext(&mut self, p: usize) -> Tick {
        let pdf = xochitl::pdf_path(&self.job.uuid);
        let args: Vec<String> = vec![
            "draw".into(),
            "-F".into(),
            "stext".into(),
            "-o".into(),
            self.stext_path.clone(),
            pdf,
            format!("{}", p + 1),
        ];
        match mutool_cmd(&args).spawn() {
            Ok(c) => {
                self.child = c;
                self.step = Step::Stext(p);
                Tick::Working(p, self.pages)
            }
            Err(e) => self.fail(format!("spawn stext: {e}")),
        }
    }

    fn finish(&mut self) -> Tick {
        let _ = std::fs::remove_file(&self.stext_path);
        let offsets: Vec<Value> = self.offsets.iter().map(|(x, y)| json!([x, y])).collect();
        let meta = json!({
            "title": self.job.title,
            "pages": self.pages,
            "w": SCREEN_W,
            "h": SCREEN_H,
            "offsets": offsets,
            "src": "device",
        });
        let path = format!("{}/meta.json", self.out);
        if std::fs::write(&path, serde_json::to_vec(&meta).unwrap_or_default()).is_err() {
            return self.fail(format!("write {path}"));
        }
        Tick::Finished(self.job.slug.clone())
    }

    fn fail(&mut self, msg: String) -> Tick {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.out); /* no broken bundles */
        Tick::Failed(format!("import '{}': {msg}", self.job.title))
    }

    fn step_name(&self) -> String {
        match self.step {
            Step::Info => "info".into(),
            Step::Stext(p) => format!("stext p{}", p + 1),
            Step::Draw(p) => format!("draw p{}", p + 1),
        }
    }
}

impl Drop for Importer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/* ---- stext parsing --------------------------------------------------------
 * mutool's structured text is XML with one <char .../> per glyph:
 *   <page id="page1" width="444.96" height="594.96">
 *     <line bbox="..."><font ...><char quad="x0 y0 x1 y1 x2 y2 x3 y3" c="H"/>
 * We only need page dims and per-word boxes; words are chars within a line
 * split on whitespace or a gap wider than ~40% of the char height.       */

pub struct StWord {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    pub text: String,
}

pub struct StPage {
    pub w: f32,
    pub h: f32,
    pub words: Vec<StWord>,
    pub text: String,
}

fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let pat = format!("{name}=\"");
    let s = tag.find(&pat)? + pat.len();
    let e = tag[s..].find('"')? + s;
    Some(&tag[s..e])
}

fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    s.replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

pub fn parse_stext(xml: &str) -> Option<StPage> {
    let page_tag_s = xml.find("<page ")?;
    let page_tag_e = xml[page_tag_s..].find('>')? + page_tag_s;
    let ptag = &xml[page_tag_s..page_tag_e];
    let w: f32 = attr(ptag, "width")?.parse().ok()?;
    let h: f32 = attr(ptag, "height")?.parse().ok()?;

    let mut words: Vec<StWord> = Vec::new();
    let mut text = String::new();
    let mut cur: Option<StWord> = None;
    let mut last_x1 = f32::MIN;

    let flush = |cur: &mut Option<StWord>, words: &mut Vec<StWord>, text: &mut String| {
        if let Some(wd) = cur.take() {
            if !wd.text.trim().is_empty() {
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push(' ');
                }
                text.push_str(&wd.text);
                words.push(wd);
            }
        }
    };

    let mut i = page_tag_e;
    while let Some(off) = xml[i..].find('<') {
        let s = i + off;
        let e = match xml[s..].find('>') {
            Some(e) => s + e + 1,
            None => break,
        };
        let tag = &xml[s..e];
        i = e;
        if tag.starts_with("</line") {
            flush(&mut cur, &mut words, &mut text);
            if !text.ends_with('\n') {
                text.push('\n');
            }
            last_x1 = f32::MIN;
        } else if tag.starts_with("<char ") {
            let Some(q) = attr(tag, "quad") else { continue };
            let c = unescape(attr(tag, "c").unwrap_or(""));
            let nums: Vec<f32> = q.split_whitespace().filter_map(|v| v.parse().ok()).collect();
            if nums.len() < 8 {
                continue;
            }
            let (x0, y0) = (
                nums[0].min(nums[2]).min(nums[4]).min(nums[6]),
                nums[1].min(nums[3]).min(nums[5]).min(nums[7]),
            );
            let (x1, y1) = (
                nums[0].max(nums[2]).max(nums[4]).max(nums[6]),
                nums[1].max(nums[3]).max(nums[5]).max(nums[7]),
            );
            let ch_h = y1 - y0;
            if c.trim().is_empty() {
                flush(&mut cur, &mut words, &mut text);
                last_x1 = x1;
                continue;
            }
            /* a wide horizontal jump inside a line also ends the word */
            if cur.is_some() && last_x1 > f32::MIN && x0 - last_x1 > (ch_h * 0.4).max(2.0) {
                flush(&mut cur, &mut words, &mut text);
            }
            last_x1 = x1;
            match cur.as_mut() {
                Some(wd) => {
                    wd.x0 = wd.x0.min(x0);
                    wd.y0 = wd.y0.min(y0);
                    wd.x1 = wd.x1.max(x1);
                    wd.y1 = wd.y1.max(y1);
                    wd.text.push_str(&c);
                }
                None => cur = Some(StWord { x0, y0, x1, y1, text: c }),
            }
        }
    }
    flush(&mut cur, &mut words, &mut text);
    Some(StPage { w, h, words, text: text.trim().to_string() })
}
