//! The root view. Owns the library, the open document, dialogs, and — the
//! interesting part — input routing.
//!
//! Input model. The pen and the touchscreen arrive as raw evdev (input.rs).
//! The pen on the paper inks directly. Everything else — a finger or the
//! nib on the toolbar, the home grid, a dialog — is turned into synthetic
//! GPUI mouse events (click on tap, right-click on long-press, wheel on
//! drag), so every ordinary GPUI button works by touch. Any pointer events
//! the compositor emulates from the pen are swallowed in the capture phase
//! so nothing happens twice.
//!
//! Gestures follow papier on the reMarkable:
//!   one finger ................... scroll the pages (with momentum)
//!   two fingers down (top half) .. back to the library
//!   two fingers up (bottom half) . reopen the previous document
//!   pinch ........................ zoom
//!   swipe down from the top edge . peek the status bar
//!   long-press ................... document menu / remove a highlight
//! A palm next to a hovering or recently-lifted pen is ignored.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use futures::{StreamExt, channel::mpsc::UnboundedReceiver};
use gpui::AnimationExt;
use gpui::{
    AnyElement, App, Context, DispatchPhase, FocusHandle, KeyDownEvent, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, PlatformInput, Point, RenderImage, ScrollDelta, ScrollHandle, ScrollWheelEvent, Task, Timer, TouchPhase, Window,
    canvas, div, prelude::*, px,
};

use crate::{
    api::{Client, Fetched},
    doc::DocView,
    geometry,
    input::InputEvent,
    ink,
    model::{Doc, Sort},
    status::SysStatus,
    store::{self, Settings},
    theme::*,
};

// papier's gesture constants, as fractions of the window (tablet px / 1872)
pub const TWO_SWIPE_DY: f32 = 0.139;
pub const HOLD: Duration = Duration::from_millis(650);
pub const PEN_TIMEOUT: Duration = Duration::from_millis(1500);
const TAP_SLOP: f32 = 10.0;
const EDGE_Y: f32 = 14.0;
const PEEK_SWIPE: f32 = 48.0;

thread_local! {
    /// True while we dispatch our own synthetic pointer events.
    static SYNTH: Cell<bool> = const { Cell::new(false) };
}

pub enum Screen {
    Home,
    Doc(Box<DocView>),
}

pub enum Load<T> {
    Loading,
    Ready(T),
    Failed,
}

pub enum Dialog {
    NewNotebook { title: String },
    Settings { server: String, test: Option<String> },
    Delete { id: String, title: String },
    DocMenu { id: String, title: String },
    GoTo { digits: String },
    Find { query: String, sel: usize },
}

/// Fling velocity from the last ~100ms of samples (least-squares slope),
/// not the last frame: a finger slows and settles as it leaves the glass,
/// and those final near-still samples must not cancel a strong swipe.
#[derive(Default)]
pub(crate) struct Tracker {
    samples: std::collections::VecDeque<(Instant, (f32, f32))>,
}

impl Tracker {
    fn push(&mut self, at: (f32, f32)) {
        let now = Instant::now();
        self.samples.push_back((now, at));
        while self.samples.front().is_some_and(|(t, _)| now.duration_since(*t) > Duration::from_millis(120)) {
            self.samples.pop_front();
        }
    }

    /// px/s; zero if the finger rested before lifting.
    fn velocity(&self) -> (f32, f32) {
        let Some(&(last_t, _)) = self.samples.back() else { return (0.0, 0.0) };
        if last_t.elapsed() > Duration::from_millis(80) || self.samples.len() < 3 {
            return (0.0, 0.0);
        }
        let t0 = self.samples[0].0;
        let n = self.samples.len() as f32;
        let ts: Vec<f32> = self.samples.iter().map(|(t, _)| t.duration_since(t0).as_secs_f32()).collect();
        let mt = ts.iter().sum::<f32>() / n;
        let var: f32 = ts.iter().map(|t| (t - mt).powi(2)).sum();
        if var < 1e-6 {
            return (0.0, 0.0);
        }
        let slope = |k: usize| {
            let vals: Vec<f32> = self.samples.iter().map(|(_, p)| if k == 0 { p.0 } else { p.1 }).collect();
            let mv = vals.iter().sum::<f32>() / n;
            ts.iter().zip(&vals).map(|(t, v)| (t - mt) * (v - mv)).sum::<f32>() / var
        };
        (slope(0), slope(1))
    }
}

/// One finger (or the nib on chrome) being turned into mouse events.
struct Emu {
    start: (f32, f32),
    last: (f32, f32),
    t0: Instant,
    moved: f32,
    scrolling: bool,
    held: bool,
    track: Tracker,
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// Pointer emulation (home, chrome, dialogs).
    Emu,
    /// Finger drawing (finger-draw mode).
    Ink,
    /// From the top edge: maybe a status peek.
    Edge,
    /// One finger on the paper: scroll.
    Page,
    /// Two or more fingers: pinch, or the home / previous-doc swipes.
    Multi,
    /// Already resolved (e.g. the status peek): ignore the rest.
    Done,
}

struct Gesture {
    kind: Kind,
    start: (f32, f32),
    last: (f32, f32),
    t0: Instant,
    max_n: usize,
    moved: f32,
    spread0: f32,
    zoom0: f32,
    pinching: bool,
    held: bool,
    /// Recent finger positions, for the fling.
    track: Tracker,
}

pub struct Pen {
    pub in_range: bool,
    pub down: bool,
    pub on_ink: bool,
    pub hover: Option<(f32, f32)>,
    pub eraser: bool,
    pub seen: Option<Instant>,
}

pub struct Papier {
    pub focus: FocusHandle,
    pub settings: Settings,
    pub client: Client,
    // library
    pub docs: Vec<Doc>,
    etag: Option<String>,
    pub synced_at: Option<u64>,
    pub lib_error: Option<String>,
    pub loading: bool,
    refresh_id: u64,
    pub covers: HashMap<String, Load<Arc<RenderImage>>>,
    pub prints: HashMap<String, Load<Arc<ink::Print>>>,
    /// Source PDFs on disk with their page sizes, by doc id.
    pdfs: HashMap<String, Load<Arc<(std::path::PathBuf, Vec<(f32, f32)>)>>>,
    /// Pages rendered from them: (doc, pdf page, pixel width).
    pdf_prints: HashMap<(String, u32, u32), Load<Arc<ink::Print>>>,
    pdf_order: Vec<(String, u32, u32)>,
    /// The local mirror: (documents done, documents to fetch) while a pass
    /// runs, None when idle.
    pub mirror: Option<Arc<(std::sync::atomic::AtomicUsize, std::sync::atomic::AtomicUsize)>>,
    print_order: Vec<String>,
    // views
    pub screen: Screen,
    pub folder: Option<String>,
    pub sort: Sort,
    pub home_scroll: ScrollHandle,
    pub dialog: Option<Dialog>,
    pub toast: Option<(String, Instant)>,
    pub status: SysStatus,
    pub peek_until: Option<Instant>,
    recent: Vec<String>,
    // input
    geometry: geometry::Shared,
    pub viewport: (f32, f32),
    pub window_active: bool,
    pub pen: Pen,
    gesture: Option<Gesture>,
    touch_mute: bool,
    emu: Option<Emu>,
    momentum: Option<((f32, f32), (f32, f32), Instant)>,
    block_real_until: Rc<Cell<Instant>>,
    pub mouse_ink: bool,
    /// Touchpad scrolling: running position + tracker, for momentum.
    pad: Tracker,
    pad_pos: (f32, f32),
    /// When the last touchpad scroll arrived (and where the pointer was).
    pad_last: Option<(Instant, (f32, f32))>,
    pub retired: Rc<RefCell<Vec<Arc<RenderImage>>>>,
    /// Retired images waiting out the frames still in flight.
    aging: Rc<RefCell<Vec<(Instant, Arc<RenderImage>)>>>,
    transition: u64,
    dialog_seq: u64,
    dialog_was_open: bool,
    _loops: Vec<Task<()>>,
}

/// CPU work (rasters, PDF renders) on GPUI's background pool.
pub fn bg<T: Send + 'static>(cx: &mut App, work: impl FnOnce() -> T + Send + 'static) -> Task<T> {
    cx.background_executor().spawn(async move { work() })
}

/// Network work on its own thread: a request stuck waiting on a dead
/// connection must never hold a pool thread that renders pages.
pub fn net<T: Send + 'static>(cx: &mut App, work: impl FnOnce() -> T + Send + 'static) -> Task<T> {
    net_on(cx.background_executor(), work)
}

pub fn net_on<T: Send + 'static>(ex: &gpui::BackgroundExecutor, work: impl FnOnce() -> T + Send + 'static) -> Task<T> {
    let (tx, rx) = futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(work());
    });
    ex.spawn(async move { rx.await.expect("network thread") })
}

impl Papier {
    pub fn new(mut input: UnboundedReceiver<InputEvent>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let settings = Settings::load();
        let client = Client::new(&settings.server);
        let focus = cx.focus_handle();
        focus.focus(window);

        // The input pump: wakes the instant a sample lands, routes the
        // batch, then dispatches any synthetic pointer events outside the
        // entity update (GPUI listeners need to borrow this view).
        let input_loop = cx.spawn_in(window, async move |this, cx| {
            while let Some(first) = input.next().await {
                let mut batch = vec![first];
                while let Ok(e) = input.try_recv() {
                    batch.push(e);
                }
                let Ok(synth) = this.update_in(cx, |this, window, cx| {
                    let mut out = Vec::new();
                    let mut dirty = false;
                    for e in batch {
                        dirty |= this.on_input(e, window, &mut out, cx);
                    }
                    if dirty {
                        cx.notify();
                    }
                    out
                }) else {
                    break;
                };
                if !synth.is_empty() {
                    dispatch(cx, synth);
                }
            }
        });

        // The frame tick: long-press timers, momentum, flip and pi-ink
        // animations, toasts. Cheap when idle (nothing notifies).
        let tick_loop = cx.spawn_in(window, async move |this, cx| {
            loop {
                Timer::after(Duration::from_millis(16)).await;
                let Ok(synth) = this.update_in(cx, |this, window, cx| {
                    let mut out = Vec::new();
                    if this.tick(window, &mut out, cx) {
                        cx.notify();
                    }
                    out
                }) else {
                    break;
                };
                if !synth.is_empty() {
                    dispatch(cx, synth);
                }
            }
        });

        // Library sync: the ETagged manifest, every 60s at home and every
        // 20s inside a document (so web/tablet edits show up promptly).
        let sync_loop = cx.spawn(async move |this, cx| {
            loop {
                let Ok(period) = this.update(cx, |this, cx| {
                    this.refresh(cx);
                    if matches!(this.screen, Screen::Doc(_)) { 20 } else { 60 }
                }) else {
                    break;
                };
                Timer::after(Duration::from_secs(period)).await;
            }
        });

        let mut app = Papier {
            focus,
            client,
            docs: Vec::new(),
            etag: None,
            synced_at: None,
            lib_error: None,
            loading: false,
            refresh_id: 0,
            covers: HashMap::new(),
            prints: HashMap::new(),
            pdfs: HashMap::new(),
            pdf_prints: HashMap::new(),
            pdf_order: Vec::new(),
            mirror: None,
            print_order: Vec::new(),
            screen: Screen::Home,
            folder: None,
            sort: Sort::from_key(&settings.sort),
            home_scroll: ScrollHandle::new(),
            dialog: None,
            toast: None,
            status: SysStatus::read(),
            peek_until: None,
            recent: settings
                .get("recent")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_owned)).collect())
                .unwrap_or_default(),
            geometry: geometry::spawn(),
            viewport: (1180.0, 760.0),
            window_active: true,
            pen: Pen { in_range: false, down: false, on_ink: false, hover: None, eraser: false, seen: None },
            gesture: None,
            touch_mute: false,
            emu: None,
            momentum: None,
            block_real_until: Rc::new(Cell::new(Instant::now())),
            mouse_ink: false,
            pad: Tracker::default(),
            pad_pos: (0.0, 0.0),
            pad_last: None,
            retired: Rc::new(RefCell::new(Vec::new())),
            aging: Rc::new(RefCell::new(Vec::new())),
            transition: 0,
            dialog_seq: 0,
            dialog_was_open: false,
            settings,
            _loops: vec![input_loop, tick_loop, sync_loop],
        };
        app.restore_library();
        // PAPIER_OPEN=<doc id>: land straight in a document (handy for testing).
        if let Some(id) = std::env::var("PAPIER_OPEN").ok().filter(|id| app.doc_by_id(id).is_some()) {
            app.open_doc(&id, window, cx);
        }
        app
    }

    // ---------------------------------------------------------- library

    fn restore_library(&mut self) {
        if let Some(snap) = store::load_library(&self.client.root) {
            self.docs = snap.library.docs;
            self.etag = snap.etag;
            self.synced_at = snap.synced_at;
        }
    }

    pub fn toast(&mut self, text: impl Into<String>) {
        self.toast = Some((text.into(), Instant::now()));
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if !self.client.root.contains("://") {
            return;
        }
        self.refresh_id += 1;
        let id = self.refresh_id;
        self.loading = self.docs.is_empty();
        let client = self.client.clone();
        let etag = self.etag.clone();
        let work = net(cx, move || {
            let r = client.library(etag.as_deref());
            // Offline writes waiting for the server go out on a good sync.
            if r.is_ok() {
                for (doc, key, page, base) in store::unsent() {
                    if client.post_ink(&doc, &key, &page).is_ok() {
                        store::remember_pending(&doc, &key, &page, &base, false);
                    }
                }
            }
            r
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                if id != this.refresh_id {
                    return;
                }
                this.loading = false;
                match result {
                    Ok(Fetched::Fresh(lib, tag)) => {
                        let now = store::now_ms();
                        store::save_library(&this.client.root, &lib, tag.as_deref(), now);
                        store::prune_pending(&lib);
                        this.etag = tag;
                        this.docs = lib.docs;
                        this.synced_at = Some(now);
                        this.lib_error = None;
                        this.library_changed(cx);
                        this.start_mirror(cx);
                    }
                    Ok(Fetched::NotModified) => {
                        this.synced_at = Some(store::now_ms());
                        this.lib_error = None;
                        this.start_mirror(cx);
                    }
                    Err(e) => this.lib_error = Some(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The manifest moved: an open document adopts its new version/seq and
    /// reloads pages that changed elsewhere (web viewer, tablet, iPad).
    fn library_changed(&mut self, cx: &mut Context<Self>) {
        let Screen::Doc(dv) = &mut self.screen else { return };
        let Some(fresh) = self.docs.iter().find(|d| d.id == dv.doc.id).cloned() else { return };
        if fresh.version != dv.doc.version {
            dv.adopt_version(fresh);
            self.reload_clean_pages(cx);
        }
    }

    /// Keep every document on this laptop: ink, the source PDF (or page
    /// images), the cover — fetching only documents whose version moved
    /// since the last pass. Runs after each good library sync, one doc at
    /// a time on its own thread; stops when the network goes and resumes
    /// at the next sync.
    fn start_mirror(&mut self, cx: &mut Context<Self>) {
        if self.mirror.is_some() || std::env::var_os("PAPIER_NO_MIRROR").is_some() {
            return;
        }
        let progress = Arc::new((std::sync::atomic::AtomicUsize::new(0), std::sync::atomic::AtomicUsize::new(0)));
        self.mirror = Some(progress.clone());
        let (client, docs) = (self.client.clone(), self.docs.clone());
        let work = net(cx, move || {
            use std::sync::atomic::Ordering::Relaxed;
            let mut synced = store::load_synced(&client.root);
            // documents the server no longer lists
            let gone: Vec<String> = synced.keys().filter(|id| !docs.iter().any(|d| &d.id == *id)).cloned().collect();
            for id in gone {
                store::forget_offline_doc(&client.root, &id);
                synced.remove(&id);
            }
            let want = |d: &Doc| (d.version.clone(), d.src_version.clone().unwrap_or_default());
            let todo: Vec<&Doc> = docs.iter().filter(|d| synced.get(&d.id) != Some(&want(d))).collect();
            progress.1.store(todo.len(), Relaxed);
            for doc in todo {
                if crate::api::offline() {
                    break;
                }
                let before = synced.get(&doc.id).cloned();
                if mirror_doc(&client, doc, before.as_ref().map(|b| b.1.as_str())) {
                    synced.insert(doc.id.clone(), want(doc));
                    store::save_synced(&client.root, &synced);
                }
                progress.0.fetch_add(1, Relaxed);
            }
            store::save_synced(&client.root, &synced);
        });
        cx.spawn_in_window_free(work, |this, (), _| this.mirror = None);
        // repaint the progress now and then while it runs
        cx.spawn(async move |this, cx| {
            loop {
                Timer::after(Duration::from_millis(700)).await;
                let Ok(running) = this.update(cx, |this, cx| {
                    cx.notify();
                    this.mirror.is_some()
                }) else {
                    break;
                };
                if !running {
                    break;
                }
            }
        })
        .detach();
    }

    pub fn doc_by_id(&self, id: &str) -> Option<&Doc> {
        self.docs.iter().find(|d| d.id == id)
    }

    pub fn cover(&mut self, doc: &Doc, cx: &mut Context<Self>) -> Option<Arc<RenderImage>> {
        let path = doc.cover.as_ref()?;
        let url = self.client.url(path);
        match self.covers.get(&url) {
            Some(Load::Ready(img)) => return Some(img.clone()),
            Some(_) => return None,
            None => {}
        }
        self.covers.insert(url.clone(), Load::Loading);
        let client = self.client.clone();
        let fetch_url = url.clone();
        let work = net(cx, move || {
            let bytes = store::cached_asset(&fetch_url).or_else(|| {
                let b = client.get_bytes(&fetch_url).ok()?;
                store::store_asset(&fetch_url, &b);
                Some(b)
            })?;
            ink::decode_render_image(&bytes, (THUMB_W * 2.0) as u32)
        });
        cx.spawn(async move |this, cx| {
            let img = work.await;
            let _ = this.update(cx, |this, cx| {
                this.covers.insert(url, img.map(Load::Ready).unwrap_or(Load::Failed));
                cx.notify();
            });
        })
        .detach();
        None
    }

    /// A book page's print (pre-rendered raster), fetched once per doc
    /// version and kept for the last few pages.
    /// `fallback`: when the pre-rendered page is missing, the server's raw
    /// render of that source-PDF page, with the doc's crop/margins to lay it
    /// out the way mkbook (and the web viewer) do.
    pub fn print(&mut self, url: &str, fallback: Option<(String, Option<Vec<f64>>, Option<Vec<f64>>)>, cx: &mut Context<Self>) -> Option<Arc<ink::Print>> {
        match self.prints.get(url) {
            Some(Load::Ready(p)) => {
                let p = p.clone();
                self.print_order.retain(|u| u != url);
                self.print_order.push(url.to_owned());
                return Some(p);
            }
            Some(_) => return None,
            None => {}
        }
        self.prints.insert(url.to_owned(), Load::Loading);
        let client = self.client.clone();
        let fetch_url = url.to_owned();
        let work = net(cx, move || {
            let fetch = |u: &str| {
                store::cached_asset(u).or_else(|| {
                    let b = client.get_bytes(u).ok()?;
                    store::store_asset(u, &b);
                    Some(b)
                })
            };
            if let Some(print) = fetch(&fetch_url).and_then(|b| ink::decode_print(&b)) {
                return Some(Arc::new(print));
            }
            let (preview, crop, margins) = fallback?;
            let bytes = fetch(&preview)?;
            ink::place_preview(&bytes, crop.as_deref(), margins.as_deref()).map(Arc::new)
        });
        let url = url.to_owned();
        cx.spawn(async move |this, cx| {
            let print = work.await;
            let failed = print.is_none();
            let _ = this.update(cx, |this, cx| {
                this.prints.insert(url.clone(), print.map(Load::Ready).unwrap_or(Load::Failed));
                if failed {
                    // try again later: a timeout shouldn't blank a page for the session
                    let retry = url.clone();
                    cx.spawn(async move |this, cx| {
                        Timer::after(Duration::from_secs(20)).await;
                        let _ = this.update(cx, |this, cx| {
                            if matches!(this.prints.get(&retry), Some(Load::Failed)) {
                                this.prints.remove(&retry);
                                cx.notify();
                            }
                        });
                    })
                    .detach();
                }
                this.print_order.push(url);
                while this.print_order.len() > 8 {
                    let old = this.print_order.remove(0);
                    this.prints.remove(&old);
                }
                cx.notify();
            });
        })
        .detach();
        None
    }

    /// A book page rendered from its source PDF at `w`×`h` pixels — the
    /// exact size it's shown, so text is crisp. Until that render lands,
    /// the nearest size already rendered stands in (None: use the PNG).
    /// `render: false` (mid-pinch) only looks, so a gesture doesn't spawn a
    /// render per frame.
    pub fn pdf_print(&mut self, doc: &Doc, page: u32, w: u32, h: u32, render: bool, cx: &mut Context<Self>) -> Option<Arc<ink::Print>> {
        let source = match self.pdfs.get(&doc.id) {
            Some(Load::Ready(src)) => src.clone(),
            Some(_) => return None,
            None => {
                self.pdfs.insert(doc.id.clone(), Load::Loading);
                let url = self.client.source_pdf_url(doc);
                let client = self.client.clone();
                let id = doc.id.clone();
                let work = net(cx, move || {
                    let path = store::asset_path(&url);
                    if !path.exists() {
                        let bytes = client.get_bytes(&url).ok().filter(|b| b.starts_with(b"%PDF"))?;
                        store::store_asset(&url, &bytes);
                    }
                    let sizes = crate::pdf::page_sizes(&path)?;
                    Some(Arc::new((path, sizes)))
                });
                cx.spawn_in_window_free(work, move |this, src, cx| {
                    let failed = src.is_none();
                    this.pdfs.insert(id.clone(), src.map(Load::Ready).unwrap_or(Load::Failed));
                    if failed {
                        // offline, probably: try again in a while
                        cx.spawn(async move |this, cx| {
                            Timer::after(Duration::from_secs(20)).await;
                            let _ = this.update(cx, |this, cx| {
                                if matches!(this.pdfs.get(&id), Some(Load::Failed)) {
                                    this.pdfs.remove(&id);
                                    cx.notify();
                                }
                            });
                        })
                        .detach();
                    }
                });
                return None;
            }
        };
        let key = (doc.id.clone(), page, w);
        match self.pdf_prints.get(&key) {
            Some(Load::Ready(p)) => {
                let p = p.clone();
                self.pdf_order.retain(|k| *k != key);
                self.pdf_order.push(key);
                return Some(p);
            }
            Some(_) => {}
            None if render => {
                self.pdf_prints.insert(key.clone(), Load::Loading);
                let size = source.1.get(page as usize).copied().unwrap_or((612.0, 792.0));
                let (crop, margins) = (doc.meta.crop.clone(), doc.meta.margins.clone());
                let work = bg(cx, move || {
                    crate::pdf::render(&source.0, page, size, crop.as_deref(), margins.as_deref(), w, h).map(|px| Arc::new(ink::Print::segment(px)))
                });
                let k = key.clone();
                cx.spawn_in_window_free(work, move |this, print, _| {
                    this.pdf_prints.insert(k.clone(), print.map(Load::Ready).unwrap_or(Load::Failed));
                    this.pdf_order.push(k);
                    // keep the pages around the reader, a few sizes each
                    while this.pdf_order.len() > 14 {
                        let old = this.pdf_order.remove(0);
                        if !this.pdf_order.contains(&old) {
                            this.pdf_prints.remove(&old);
                        }
                    }
                });
            }
            None => {}
        }
        // stand-in: the closest width already rendered for this page
        self.pdf_prints
            .iter()
            .filter_map(|((d, p, pw), l)| match l {
                Load::Ready(print) if *d == doc.id && *p == page => Some((pw.abs_diff(w), print.clone())),
                _ => None,
            })
            .min_by_key(|(d, _)| *d)
            .map(|(_, p)| p)
    }

    // ---------------------------------------------------------- navigation

    pub fn open_doc(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(doc) = self.doc_by_id(id).cloned() else { return };
        self.close_doc(cx);
        self.dialog = None;
        self.recent.retain(|r| r != id);
        self.recent.push(id.to_owned());
        if self.recent.len() > 12 {
            self.recent.remove(0);
        }
        self.settings.set("recent", serde_json::json!(self.recent));
        let dv = DocView::new(doc, &self.settings);
        self.screen = Screen::Doc(Box::new(dv));
        self.transition += 1;
        self.doc_opened(window, cx);
        cx.notify();
    }

    pub fn close_doc(&mut self, cx: &mut Context<Self>) {
        if let Screen::Doc(_) = &self.screen {
            self.flush_all(cx);
            let Screen::Doc(dv) = std::mem::replace(&mut self.screen, Screen::Home) else { unreachable!() };
            self.retired.borrow_mut().extend(dv.into_images());
        }
    }

    pub fn go_home(&mut self, cx: &mut Context<Self>) {
        if matches!(self.screen, Screen::Doc(_)) {
            self.transition += 1;
        }
        self.close_doc(cx);
        self.dialog = None;
        self.refresh(cx);
        cx.notify();
    }

    /// The two-finger swipe up: the previous document (repeat to bounce
    /// between the last two).
    pub fn previous_doc(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = match &self.screen {
            Screen::Doc(dv) => Some(dv.doc.id.clone()),
            Screen::Home => None,
        };
        let prev = self.recent.iter().rev().find(|id| Some(*id) != current.as_ref() && self.doc_by_id(id).is_some()).cloned();
        match prev {
            Some(id) => self.open_doc(&id, window, cx),
            None => self.toast("No previous document"),
        }
    }

    pub fn create_notebook(&mut self, title: String, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let work = net(cx, move || client.create_notebook(&title));
        cx.spawn_in_window_free(work, |this, result, cx| match result {
            Ok(_) => {
                this.toast("Notebook created");
                this.etag = None;
                this.refresh(cx);
            }
            Err(e) => this.toast(format!("Couldn't create notebook: {e}")),
        });
    }

    pub fn delete_doc(&mut self, id: String, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let doc_id = id.clone();
        let work = net(cx, move || client.delete_document(&doc_id));
        cx.spawn_in_window_free(work, move |this, result, cx| match result {
            Ok(()) => {
                this.docs.retain(|d| d.id != id);
                store::forget_pending_doc(&id);
                this.etag = None;
                this.toast("Deleted — recoverable from Papier trash");
                this.refresh(cx);
            }
            Err(e) => this.toast(format!("Couldn't delete: {e}")),
        });
    }

    pub fn set_server(&mut self, server: String, cx: &mut Context<Self>) {
        let server = crate::api::normalize(&server);
        if server == self.client.root {
            return;
        }
        self.settings.server = server.clone();
        self.settings.save();
        self.client = Client::new(&server);
        self.docs.clear();
        self.etag = None;
        self.synced_at = None;
        self.covers.clear();
        self.restore_library();
        self.refresh(cx);
    }

    // ---------------------------------------------------------------- input

    fn takes_input(&self) -> bool {
        let shown = self.geometry.lock().ok().and_then(|g| *g).is_none_or(|g| g.shown);
        self.window_active && shown
    }

    fn map(&self, nx: f32, ny: f32) -> (f32, f32) {
        match self.geometry.lock().ok().and_then(|g| *g) {
            Some(g) => g.map(nx, ny),
            None => {
                let (u, v) = geometry::rotate(0, nx, ny);
                (u * self.viewport.0, v * self.viewport.1)
            }
        }
    }

    fn inside(&self, (x, y): (f32, f32)) -> bool {
        x >= 0.0 && y >= 0.0 && x <= self.viewport.0 && y <= self.viewport.1
    }

    /// Is this point over chrome (toolbar, dialogs, peek bar) rather than
    /// paper? Chrome gets mouse emulation; paper gets ink and gestures.
    pub fn on_chrome(&self, x: f32, y: f32) -> bool {
        if self.dialog.is_some() {
            return true;
        }
        match &self.screen {
            Screen::Home => true,
            Screen::Doc(dv) => {
                if self.peek_until.is_some() && y < STATUS_H {
                    return true;
                }
                dv.chrome_hit(x, y, self.viewport)
            }
        }
    }

    fn block_real(&self, for_ms: u64) {
        let until = Instant::now() + Duration::from_millis(for_ms);
        if until > self.block_real_until.get() {
            self.block_real_until.set(until);
        }
    }

    fn palm(&self) -> bool {
        self.pen.in_range || self.pen.down || self.pen.seen.is_some_and(|t| t.elapsed() < PEN_TIMEOUT)
    }

    fn on_input(&mut self, event: InputEvent, window: &mut Window, out: &mut Vec<PlatformInput>, cx: &mut Context<Self>) -> bool {
        match event {
            InputEvent::Error(e) => {
                self.toast(e);
                true
            }
            InputEvent::Pen { x, y, pressure, touching, in_range, eraser } => {
                if !self.takes_input() {
                    if self.pen.down && self.pen.on_ink {
                        self.ink_end(window, cx);
                    }
                    self.pen.down = false;
                    self.pen.hover = None;
                    self.pen.in_range = false;
                    return true;
                }
                let at = self.map(x, y);
                if !self.inside(at) && !self.pen.down {
                    self.pen.hover = None;
                    self.pen.in_range = false;
                    return true;
                }
                self.block_real(400);
                self.pen.seen = Some(Instant::now());
                self.pen.in_range = in_range;
                self.pen.eraser = eraser;
                self.pen.hover = (in_range && !touching).then_some(at);
                // A palm already mid-gesture loses to the pen.
                if let Some(g) = self.gesture.take() {
                    self.cancel_gesture(g, window, cx);
                }
                let was = self.pen.down;
                self.pen.down = touching;
                match (was, touching) {
                    (false, true) => {
                        if !self.on_chrome(at.0, at.1) {
                            self.pen.on_ink = true;
                            self.ink_begin(at, pressure, eraser, window, cx);
                        } else {
                            self.pen.on_ink = false;
                            self.emu_begin(at, out);
                        }
                    }
                    (true, true) if self.pen.on_ink => self.ink_move(at, pressure, window, cx),
                    (true, true) => self.emu_move(at, out),
                    (true, false) => {
                        if self.pen.on_ink {
                            self.ink_end(window, cx);
                        } else {
                            self.emu_end(out);
                        }
                        self.pen.on_ink = false;
                    }
                    (false, false) => {
                        if in_range && self.on_chrome(at.0, at.1) {
                            out.push(mouse_move(at, None));
                        }
                    }
                }
                true
            }
            InputEvent::Touch(contacts) => self.on_touch(contacts, window, out, cx),
        }
    }

    fn cancel_gesture(&mut self, g: Gesture, window: &mut Window, cx: &mut Context<Self>) {
        match g.kind {
            Kind::Ink => self.ink_cancel(window, cx),
            _ => {}
        }
        self.emu = None;
        self.touch_mute = true;
    }

    fn on_touch(&mut self, contacts: Vec<(usize, f32, f32)>, window: &mut Window, out: &mut Vec<PlatformInput>, cx: &mut Context<Self>) -> bool {
        if !self.takes_input() {
            self.gesture = None;
            self.emu = None;
            return false;
        }
        if !contacts.is_empty() {
            self.block_real(400);
        }
        if self.touch_mute {
            if contacts.is_empty() {
                self.touch_mute = false;
            }
            return false;
        }
        if self.palm() {
            if let Some(g) = self.gesture.take() {
                self.cancel_gesture(g, window, cx);
            }
            return false;
        }
        let pts: Vec<(f32, f32)> = contacts.iter().map(|&(_, x, y)| self.map(x, y)).collect();
        let n = pts.len();
        let centroid = (n > 0).then(|| (pts.iter().map(|p| p.0).sum::<f32>() / n as f32, pts.iter().map(|p| p.1).sum::<f32>() / n as f32));
        let spread = if n >= 2 { (pts[0].0 - pts[1].0).hypot(pts[0].1 - pts[1].1) } else { 0.0 };
        let (_, vh) = self.viewport;

        // ---- a new contact
        let Some(g) = self.gesture.as_mut() else {
            let Some(c) = centroid.filter(|c| self.inside(*c)) else { return false };
            self.momentum = None;
            let kind = if n >= 2 {
                Kind::Multi
            } else if self.on_chrome(c.0, c.1) {
                Kind::Emu
            } else if c.1 <= EDGE_Y {
                Kind::Edge
            } else if matches!(&self.screen, Screen::Doc(dv) if dv.finger_draws) {
                Kind::Ink
            } else {
                Kind::Page
            };
            let zoom0 = match &self.screen {
                Screen::Doc(dv) => dv.zoom,
                Screen::Home => 1.0,
            };
            self.gesture = Some(Gesture { kind, start: c, last: c, t0: Instant::now(), max_n: n, moved: 0.0, spread0: spread, zoom0, pinching: false, held: false, track: Tracker::default() });
            if let Some(g) = self.gesture.as_mut() {
                g.track.push(c);
            }
            if let Screen::Doc(dv) = &mut self.screen {
                dv.stop(); // a touch catches a moving page
            }
            match kind {
                Kind::Emu => self.emu_begin(c, out),
                Kind::Ink => self.ink_begin(c, 0.55, false, window, cx),
                _ => {}
            }
            return true;
        };

        // ---- all fingers lifted: the gesture resolves
        let Some(c) = centroid else {
            let g = self.gesture.take().unwrap();
            match g.kind {
                Kind::Emu => self.emu_end(out),
                Kind::Ink => self.ink_end(window, cx),
                Kind::Page => {
                    if let Screen::Doc(dv) = &mut self.screen {
                        if g.moved > TAP_SLOP {
                            dv.fling(g.track.velocity());
                        }
                    }
                }
                Kind::Edge | Kind::Multi | Kind::Done => {}
            }
            return true;
        };

        // ---- a second finger: one-finger modes become a multi gesture
        if n >= 2 && g.kind != Kind::Multi {
            let old = g.kind;
            g.kind = Kind::Multi;
            g.start = c;
            g.last = c;
            g.spread0 = spread;
            g.max_n = n;
            g.t0 = Instant::now();
            match old {
                Kind::Ink => self.ink_cancel(window, cx),
                Kind::Emu => self.emu = None,
                _ => {}
            }
            return true;
        }

        let g = self.gesture.as_mut().unwrap();
        let prev = g.last;
        g.last = c;
        g.track.push(c);
        g.moved = g.moved.max((c.0 - g.start.0).hypot(c.1 - g.start.1));

        // ---- the multi gesture ends with its first lifted finger
        if g.kind == Kind::Multi && n < g.max_n {
            let g = self.gesture.take().unwrap();
            self.touch_mute = true;
            if !g.pinching && g.max_n == 2 {
                let (dx, dy) = (g.last.0 - g.start.0, g.last.1 - g.start.1);
                if dy.abs() >= TWO_SWIPE_DY * vh && dy.abs() > dx.abs() {
                    if dy > 0.0 && g.start.1 < vh / 2.0 {
                        match &self.screen {
                            Screen::Doc(_) => self.go_home(cx),
                            Screen::Home if self.folder.is_some() => {
                                self.folder = None;
                            }
                            Screen::Home => {}
                        }
                    } else if dy < 0.0 && g.start.1 >= vh / 2.0 {
                        self.previous_doc(window, cx);
                    }
                }
            }
            return true;
        }
        g.max_n = g.max_n.max(n);

        match g.kind {
            Kind::Emu => self.emu_move(c, out),
            Kind::Ink => {
                if let Some(&p) = pts.first() {
                    self.ink_move(p, 0.55, window, cx);
                }
            }
            Kind::Edge => {
                if c.1 - g.start.1 >= PEEK_SWIPE {
                    self.peek_until = Some(Instant::now() + Duration::from_secs(4));
                    g.kind = Kind::Done;
                }
            }
            Kind::Page => {
                if g.moved > TAP_SLOP {
                    if let Screen::Doc(dv) = &mut self.screen {
                        dv.drag((c.0 - prev.0, c.1 - prev.1), c, self.viewport);
                    }
                }
            }
            Kind::Done => {}
            Kind::Multi => {
                if let Screen::Doc(dv) = &mut self.screen {
                    let ratio = if g.spread0 > 1.0 { spread / g.spread0 } else { 1.0 };
                    if !g.pinching && (ratio - 1.0).abs() > 0.12 {
                        g.pinching = true;
                    }
                    if g.pinching {
                        dv.pinch(g.zoom0 * ratio, c, (c.0 - prev.0, c.1 - prev.1), self.viewport);
                    }
                }
            }
        }
        true
    }

    // ---- pointer emulation (fingers and the nib on chrome)

    fn emu_begin(&mut self, at: (f32, f32), out: &mut Vec<PlatformInput>) {
        self.momentum = None;
        let mut track = Tracker::default();
        track.push(at);
        self.emu = Some(Emu { start: at, last: at, t0: Instant::now(), moved: 0.0, scrolling: false, held: false, track });
        out.push(mouse_move(at, None));
    }

    fn emu_move(&mut self, at: (f32, f32), out: &mut Vec<PlatformInput>) {
        let Some(e) = self.emu.as_mut() else { return };
        e.moved = e.moved.max((at.0 - e.start.0).hypot(at.1 - e.start.1));
        let d = (at.0 - e.last.0, at.1 - e.last.1);
        if !e.scrolling && !e.held && e.moved > TAP_SLOP {
            e.scrolling = true;
        }
        e.track.push(at);
        if e.scrolling && (d.0 != 0.0 || d.1 != 0.0) {
            out.push(wheel(at, d, TouchPhase::Moved));
        }
        e.last = at;
    }

    fn emu_end(&mut self, out: &mut Vec<PlatformInput>) {
        let Some(e) = self.emu.take() else { return };
        if e.scrolling {
            let v = e.track.velocity();
            if v.0.hypot(v.1) > 150.0 {
                self.momentum = Some((v, e.last, Instant::now()));
            }
        } else if !e.held {
            out.push(mouse_down(e.start, MouseButton::Left));
            out.push(mouse_up(e.start, MouseButton::Left));
        }
    }

    // ---- ink (pen, finger-draw, mouse)

    pub fn ink_begin(&mut self, at: (f32, f32), pressure: f32, eraser: bool, window: &mut Window, _cx: &mut Context<Self>) {
        let vp = self.viewport;
        if let Screen::Doc(dv) = &mut self.screen {
            dv.ink_begin(at, pressure, eraser, vp, window);
        }
    }

    pub fn ink_move(&mut self, at: (f32, f32), pressure: f32, window: &mut Window, _cx: &mut Context<Self>) {
        let vp = self.viewport;
        if let Screen::Doc(dv) = &mut self.screen {
            dv.ink_move(at, pressure, vp, window);
        }
    }

    pub fn ink_end(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let key = if let Screen::Doc(dv) = &mut self.screen { dv.ink_end(window) } else { None };
        if let Some(key) = key {
            self.page_edited(&key, cx);
        }
    }

    fn ink_cancel(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        if let Screen::Doc(dv) = &mut self.screen {
            dv.ink_cancel();
        }
    }

    // ---------------------------------------------------------------- tick

    fn tick(&mut self, window: &mut Window, out: &mut Vec<PlatformInput>, cx: &mut Context<Self>) -> bool {
        let mut dirty = self.status.refresh();
        let active = window.is_window_active();
        if active != self.window_active {
            self.window_active = active;
            dirty = true;
        }
        // long-press: emulated right-click (doc menus) or the doc's hold
        if let Some(e) = self.emu.as_mut() {
            if !e.scrolling && !e.held && e.moved < TAP_SLOP && e.t0.elapsed() >= HOLD {
                e.held = true;
                out.push(mouse_down(e.start, MouseButton::Right));
                out.push(mouse_up(e.start, MouseButton::Right));
            }
        }
        if let Some(g) = self.gesture.as_mut() {
            if g.kind == Kind::Page && !g.held && g.moved < TAP_SLOP && g.t0.elapsed() >= HOLD {
                g.held = true;
                let at = g.start;
                if let Screen::Doc(dv) = &mut self.screen {
                    dirty |= dv.hold(at, self.viewport);
                }
            }
        }
        // touchpad scrolls that went quiet: the fingers lifted (in case the
        // compositor's axis_stop never arrives)
        if let Some((t, at)) = self.pad_last {
            if t.elapsed() > Duration::from_millis(40) {
                self.pad_lifted(at, cx);
            }
        }
        if self.toast.as_ref().is_some_and(|(_, t)| t.elapsed() > Duration::from_secs(4)) {
            self.toast = None;
            dirty = true;
        }
        if self.peek_until.is_some_and(|t| Instant::now() > t) {
            self.peek_until = None;
            dirty = true;
        }
        if let Screen::Doc(dv) = &mut self.screen {
            dirty |= dv.tick();
        }
        dirty
    }

    // ---------------------------------------------------------- keyboard

    fn on_key(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let k = &e.keystroke;
        let key = k.key.as_str();
        let ctrl = k.modifiers.control || k.modifiers.platform;
        if key == "f11" {
            window.toggle_fullscreen();
            return;
        }
        if self.dialog.is_some() {
            self.dialog_key(e, window, cx);
            cx.notify();
            return;
        }
        match (&self.screen, ctrl, key) {
            (_, true, "n") => self.dialog = Some(Dialog::NewNotebook { title: String::new() }),
            (_, true, ",") => self.dialog = Some(Dialog::Settings { server: self.settings.server.clone(), test: None }),
            (_, true, "p") | (Screen::Home, false, "/") => self.dialog = Some(Dialog::Find { query: String::new(), sel: 0 }),
            (_, true, "d") if k.modifiers.shift => {
                let next = if dark() { "light" } else { "dark" };
                self.settings.theme = next.into();
                self.settings.save();
            }
            (_, true, "r") => {
                self.etag = None;
                self.refresh(cx);
                self.toast("Syncing…");
            }
            (_, true, "q") => {
                self.flush_all(cx);
                cx.quit();
            }
            (Screen::Home, false, "backspace") | (Screen::Home, false, "escape") if self.folder.is_some() => self.folder = None,
            (Screen::Doc(_), _, _) => self.doc_key(key, ctrl, k.modifiers.shift, window, cx),
            _ => {}
        }
        cx.notify();
    }

    fn dialog_key(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let k = &e.keystroke;
        let key = k.key.as_str();
        if key == "escape" {
            self.dialog = None;
            return;
        }
        if key == "enter" {
            self.dialog_confirm(window, cx);
            return;
        }
        let typed = k.key_char.clone().filter(|_| !k.modifiers.control && !k.modifiers.platform);
        let visible = self.visible_docs();
        match self.dialog.as_mut() {
            Some(Dialog::NewNotebook { title: text }) | Some(Dialog::Settings { server: text, .. }) => edit_text(text, key, typed),
            Some(Dialog::Find { query, sel }) => match key {
                "down" => *sel += 1,
                "up" => *sel = sel.saturating_sub(1),
                _ => {
                    edit_text(query, key, typed);
                    *sel = 0;
                }
            },
            Some(Dialog::GoTo { digits }) => match key {
                "backspace" => {
                    digits.pop();
                }
                d if d.len() == 1 && d.chars().all(|c| c.is_ascii_digit()) && digits.len() < 4 => digits.push_str(d),
                _ => {}
            },
            _ => {}
        }
        let _ = visible;
    }

    pub fn dialog_confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.dialog.take() else { return };
        match d {
            Dialog::NewNotebook { title } => {
                let t = title.trim().to_owned();
                if !t.is_empty() {
                    self.create_notebook(t, cx);
                }
            }
            Dialog::Settings { server, .. } => self.set_server(server, cx),
            Dialog::Delete { id, .. } => self.delete_doc(id, cx),
            Dialog::DocMenu { id, .. } => self.open_doc(&id, window, cx),
            Dialog::GoTo { digits } => {
                if let Ok(n) = digits.parse::<usize>() {
                    self.goto_page(n.saturating_sub(1), window, cx);
                }
            }
            Dialog::Find { query, sel } => {
                let hits = self.find(&query);
                if let Some(doc) = hits.get(sel.min(hits.len().saturating_sub(1))) {
                    let id = doc.id.clone();
                    self.open_doc(&id, window, cx);
                }
            }
        }
    }

    /// Documents matching a quick-open query (subsequence match, Zed style).
    pub fn find(&self, query: &str) -> Vec<Doc> {
        let q: Vec<char> = query.to_lowercase().chars().filter(|c| !c.is_whitespace()).collect();
        let mut scored: Vec<(i32, Doc)> = self
            .docs
            .iter()
            .filter_map(|d| {
                let title = d.meta.title.to_lowercase();
                let mut it = title.chars().enumerate();
                let mut score = 0i32;
                let mut last = None;
                for qc in &q {
                    let (i, _) = it.find(|(_, c)| c == qc)?;
                    score += match last {
                        Some(l) if i == l + 1 => 3,
                        _ => 1,
                    };
                    if i == 0 {
                        score += 2;
                    }
                    last = Some(i);
                }
                Some((score, d.clone()))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.mtime.unwrap_or(0.0).total_cmp(&a.1.mtime.unwrap_or(0.0))));
        scored.into_iter().map(|(_, d)| d).take(12).collect()
    }

    pub fn visible_docs(&self) -> Vec<Doc> {
        let mut docs: Vec<Doc> = self.docs.iter().filter(|d| d.meta.folder.as_deref().filter(|f| !f.is_empty()) == self.folder.as_deref()).cloned().collect();
        self.sort.sort(&mut docs);
        docs
    }

    pub fn folders(&self) -> Vec<(String, usize)> {
        if self.folder.is_some() {
            return Vec::new();
        }
        let mut f: Vec<(String, usize)> = Vec::new();
        for d in &self.docs {
            if let Some(name) = d.meta.folder.as_deref().filter(|f| !f.is_empty()) {
                match f.iter_mut().find(|(n, _)| n == name) {
                    Some(e) => e.1 += 1,
                    None => f.push((name.to_owned(), 1)),
                }
            }
        }
        f.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
        f
    }

    /// Replay the screen fade (view mode switches).
    pub fn transition_view(&mut self) {
        self.transition += 1;
    }

    /// Touchpad scrolls (real ones, not our finger emulation): track the
    /// fingers, and when they lift carry on with momentum.
    fn on_pad_scroll(&mut self, e: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let ScrollDelta::Pixels(d) = e.delta else { return };
        let at = (f32::from(e.position.x), f32::from(e.position.y));
        if std::env::var_os("PAPIER_TRACE").is_some() {
            eprintln!("pad {:?} {:?} v={:?}", e.touch_phase, d, self.pad.velocity());
        }
        if matches!(e.touch_phase, TouchPhase::Ended) {
            self.pad_lifted(at, cx);
            return;
        }
        self.momentum = None;
        self.pad_pos = (self.pad_pos.0 + f32::from(d.x), self.pad_pos.1 + f32::from(d.y));
        self.pad.push(self.pad_pos);
        self.pad_last = Some((Instant::now(), at));
    }

    /// The fingers left the touchpad (the compositor said so, or the
    /// scroll went quiet): coast with the swipe's speed.
    fn pad_lifted(&mut self, at: (f32, f32), cx: &mut Context<Self>) {
        self.pad_last = None;
        let v = self.pad.velocity();
        self.pad = Tracker::default();
        if std::env::var_os("PAPIER_TRACE").is_some() {
            eprintln!("pad lifted v={v:?}");
        }
        if v.0.hypot(v.1) > 150.0 {
            match &mut self.screen {
                Screen::Home => self.momentum = Some((v, at, Instant::now())),
                Screen::Doc(dv) => dv.fling(v),
            }
            cx.notify();
        }
    }

    /// A flung grid keeps gliding, one vsync frame at a time.
    pub fn home_momentum(&mut self) -> bool {
        let Some((v, at, last)) = self.momentum else { return false };
        let dt = last.elapsed().as_secs_f32().min(0.05);
        let off = self.home_scroll.offset();
        let max = self.home_scroll.max_offset();
        let y = (f32::from(off.y) + v.1 * dt).clamp(-f32::from(max.height), 0.0);
        self.home_scroll.set_offset(gpui::point(off.x, px(y)));
        let decay = (-2.4 * dt).exp();
        let v = (v.0 * decay, v.1 * decay);
        if v.1.abs() < 12.0 || (y - f32::from(off.y)).abs() < 0.01 && dt > 0.0 {
            self.momentum = None;
            return false;
        }
        self.momentum = Some((v, at, Instant::now()));
        true
    }

    pub fn set_sort(&mut self, sort: Sort) {
        self.sort = sort;
        self.settings.sort = sort.key().into();
        self.settings.save();
    }

    pub fn test_server(&mut self, server: String, cx: &mut Context<Self>) {
        let work = net(cx, move || crate::api::check(&server));
        cx.spawn_in_window_free(work, |this, result, _cx| {
            let msg = match result {
                Ok(n) => format!("Connected — {n} documents"),
                Err(e) => format!("Failed — {e}"),
            };
            if let Some(Dialog::Settings { test, .. }) = this.dialog.as_mut() {
                *test = Some(msg);
            }
        });
    }
}

/// Fetch one document's files to disk. `old_src`: the source-PDF version
/// last mirrored (its file is deleted when the PDF changed). Returns false
/// if anything failed (it's retried next pass).
fn mirror_doc(client: &Client, doc: &Doc, old_src: Option<&str>) -> bool {
    let fetch = |url: &str, check: &dyn Fn(&[u8]) -> bool| -> bool {
        if store::cached_asset(url).is_some() {
            return true;
        }
        match client.get_bytes(url) {
            Ok(b) if check(&b) => {
                store::store_asset(url, &b);
                true
            }
            Ok(_) => false,
            Err(e) => e.to_string().contains("HTTP 404"), // nothing to fetch
        }
    };
    for key in doc.ink.clone().unwrap_or_default() {
        match client.fetch_ink(&doc.id, &key) {
            Ok(Some(page)) => store::save_offline_ink(&client.root, &doc.id, &key, &page),
            Ok(None) => {}
            Err(_) => return false,
        }
    }
    let mut ok = true;
    if doc.has_source {
        let url = client.source_pdf_url(doc);
        if let Some(old) = old_src.filter(|o| !o.is_empty() && Some(*o) != doc.src_version.as_deref()) {
            let mut stale = doc.clone();
            stale.src_version = Some(old.to_owned());
            let _ = std::fs::remove_file(store::asset_path(&client.source_pdf_url(&stale)));
        }
        ok &= fetch(&url, &|b| b.starts_with(b"%PDF"));
    } else {
        for e in &doc.seq {
            if let crate::model::SeqEntry::Pdf(p) = e {
                ok &= fetch(&client.page_url(doc, *p), &|b| !b.is_empty());
            }
        }
    }
    if let Some(cover) = &doc.cover {
        ok &= fetch(&client.url(cover), &|b| !b.is_empty());
    }
    ok
}

fn edit_text(text: &mut String, key: &str, typed: Option<String>) {
    match key {
        "backspace" => {
            text.pop();
        }
        _ => {
            if let Some(t) = typed.filter(|t| !t.chars().any(char::is_control)) {
                text.push_str(&t);
            }
        }
    }
}

/// Run background work, then update the view with its result.
pub trait SpawnExt {
    fn spawn_in_window_free<T: 'static>(&mut self, work: Task<T>, done: impl FnOnce(&mut Papier, T, &mut Context<Papier>) + 'static);
}

impl SpawnExt for Context<'_, Papier> {
    fn spawn_in_window_free<T: 'static>(&mut self, work: Task<T>, done: impl FnOnce(&mut Papier, T, &mut Context<Papier>) + 'static) {
        self.spawn(async move |this, cx| {
            let v = work.await;
            let _ = this.update(cx, |this, cx| {
                done(this, v, cx);
                cx.notify();
            });
        })
        .detach();
    }
}

// ------------------------------------------------------ synthetic events

fn point(at: (f32, f32)) -> Point<gpui::Pixels> {
    gpui::point(px(at.0), px(at.1))
}

fn mouse_move(at: (f32, f32), pressed: Option<MouseButton>) -> PlatformInput {
    PlatformInput::MouseMove(MouseMoveEvent { position: point(at), pressed_button: pressed, modifiers: Modifiers::default() })
}

fn mouse_down(at: (f32, f32), button: MouseButton) -> PlatformInput {
    PlatformInput::MouseDown(MouseDownEvent { button, position: point(at), modifiers: Modifiers::default(), click_count: 1, first_mouse: false })
}

fn mouse_up(at: (f32, f32), button: MouseButton) -> PlatformInput {
    PlatformInput::MouseUp(MouseUpEvent { button, position: point(at), modifiers: Modifiers::default(), click_count: 1 })
}

fn wheel(at: (f32, f32), d: (f32, f32), phase: TouchPhase) -> PlatformInput {
    PlatformInput::ScrollWheel(ScrollWheelEvent {
        position: point(at),
        delta: ScrollDelta::Pixels(gpui::point(px(d.0), px(d.1))),
        modifiers: Modifiers::default(),
        touch_phase: phase,
    })
}

fn dispatch(cx: &mut gpui::AsyncWindowContext, events: Vec<PlatformInput>) {
    let _ = cx.update(|window, cx| {
        SYNTH.with(|s| s.set(true));
        for e in events {
            window.dispatch_event(e, cx);
        }
        SYNTH.with(|s| s.set(false));
    });
}

// ---------------------------------------------------------------- render

impl Render for Papier {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let size = window.viewport_size();
        self.viewport = (f32::from(size.width), f32::from(size.height));
        let dark = match self.settings.theme.as_str() {
            "dark" => true,
            "light" => false,
            _ => matches!(window.appearance(), gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark),
        };
        set_dark(dark);
        if self.dialog.is_some() != self.dialog_was_open {
            self.dialog_was_open = self.dialog.is_some();
            self.dialog_seq += 1;
        }
        let body: AnyElement = match &self.screen {
            Screen::Home => self.render_home(window, cx),
            Screen::Doc(_) => self.render_doc(window, cx),
        };
        // A quick fade between the library and a document (re-keyed per
        // switch so it replays), and for dialogs.
        let body = div()
            .size_full()
            .child(body)
            .with_animation(
                gpui::ElementId::NamedInteger("screen".into(), self.transition),
                gpui::Animation::new(Duration::from_millis(170)).with_easing(gpui::ease_out_quint()),
                |el, t| el.opacity(t),
            );
        let dialog = self.render_dialog(window, cx).map(|d| {
            div().absolute().inset_0().child(d).with_animation(
                gpui::ElementId::NamedInteger("dialog".into(), self.dialog_seq),
                gpui::Animation::new(Duration::from_millis(120)).with_easing(gpui::ease_out_quint()),
                |el, t| el.opacity(t),
            )
        });
        let block = self.block_real_until.clone();
        let retired = self.retired.clone();
        let this = cx.entity().downgrade();
        let aging = self.aging.clone();
        div()
            .size_full()
            .relative()
            .bg(c(WHITE))
            .text_color(c(BLACK))
            .font_family(SERIF)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| this.on_key(e, window, cx)))
            .child(body)
            .children(dialog)
            .children(self.toast.as_ref().map(|(t, _)| toast(t)))
            // Swallow pointer events the compositor emulates from the pen or
            // touch (ours are let through), and drop retired GPU images.
            .child(
                canvas(
                    |_, _, _| {},
                    move |_, _, window, _| {
                        // Free retired images only once no frame in flight can
                        // still be sampling them: GPUI destroys the texture
                        // on the spot, and a frame that drew with it comes
                        // out garbled (a full-screen blink).
                        let now = Instant::now();
                        let mut a = aging.borrow_mut();
                        a.extend(retired.borrow_mut().drain(..).map(|img| (now, img)));
                        let old = a.partition_point(|(t, _)| now.duration_since(*t) >= Duration::from_millis(250));
                        let keep = a.split_off(old);
                        for (_, img) in std::mem::replace(&mut *a, keep) {
                            let _ = window.drop_image(img);
                        }
                        if !a.is_empty() {
                            window.request_animation_frame();
                        }
                        let b1 = block.clone();
                        window.on_mouse_event(move |_: &MouseDownEvent, phase, _, cx| {
                            if phase == DispatchPhase::Capture && !SYNTH.with(Cell::get) && Instant::now() < b1.get() {
                                cx.stop_propagation();
                            }
                        });
                        let b2 = block.clone();
                        window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                            if phase == DispatchPhase::Capture && !SYNTH.with(Cell::get) && Instant::now() < b2.get() {
                                cx.stop_propagation();
                            }
                        });
                        let this = this.clone();
                        window.on_mouse_event(move |e: &ScrollWheelEvent, phase, _, cx| {
                            if phase == DispatchPhase::Capture && !SYNTH.with(Cell::get) {
                                let _ = this.update(cx, |this, cx| this.on_pad_scroll(e, cx));
                            }
                        });
                        let b3 = block.clone();
                        window.on_mouse_event(move |_: &MouseMoveEvent, phase, _, cx| {
                            if phase == DispatchPhase::Capture && !SYNTH.with(Cell::get) && Instant::now() < b3.get() {
                                cx.stop_propagation();
                            }
                        });
                    },
                )
                .absolute()
                .size_0(),
            )
    }
}

fn toast(text: &str) -> impl IntoElement {
    div().absolute().bottom(px(22.0)).left_0().right_0().flex().justify_center().child(
        div()
            .px(px(18.0))
            .py(px(9.0))
            .bg(c(BLACK))
            .text_color(c(WHITE))
            .font_family(SERIF)
            .text_size(px(16.0))
            .child(text.to_owned()),
    )
}

#[cfg(test)]
mod tests {
    use super::Tracker;

    #[test]
    fn fling_survives_a_settling_lift() {
        let mut t = Tracker::default();
        let mut y = 0.0;
        for _ in 0..10 {
            y -= 24.0; // a strong upward swipe, ~3000 px/s
            t.push((0.0, y));
            std::thread::sleep(std::time::Duration::from_millis(8));
        }
        for _ in 0..2 {
            y -= 1.0; // the finger slows as it leaves the glass
            t.push((0.0, y));
            std::thread::sleep(std::time::Duration::from_millis(8));
        }
        let v = t.velocity();
        assert!(v.1 < -1200.0, "velocity {v:?}");
    }

    #[test]
    fn no_fling_after_a_rest() {
        let mut t = Tracker::default();
        t.push((0.0, 0.0));
        t.push((0.0, -50.0));
        t.push((0.0, -100.0));
        std::thread::sleep(std::time::Duration::from_millis(150));
        assert_eq!(t.velocity(), (0.0, 0.0));
    }
}
