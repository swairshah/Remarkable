//! Papier for the Framework laptop: the reMarkable papier UX — home grid,
//! right-edge toolbar, pen-first pages, pi in the margin — on a pen
//! touchscreen, talking to the same papier cloud as the tablet, the web
//! viewer and the iPad.
//!
//! Module map:
//!   model     library manifest + ink JSON (byte-compatible with the tablet)
//!   api       the cloud client (blocking; run on background threads)
//!   store     settings, offline caches, pending/unsent ink
//!   ink       outlines, page raster, eraser, undo
//!   curl      the two-page view's page curl (CPU-rendered cylinder)
//!   input     raw evdev pen + touch (GPUI has neither on Wayland)
//!   geometry  where our window sits, so ink lands under the nib
//!   app       the root view: input routing, gestures, sync, dialogs
//!   home      the library grid
//!   doc       an open document: pages, tools, toolbar, pi

mod api;
mod app;
mod assets;
mod curl;
mod doc;
mod geometry;
mod home;
mod ink;
mod input;
mod model;
mod status;
mod store;
mod theme;

use gpui::{App, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions, prelude::*, px, size};

fn main() {
    let (input_tx, input_rx) = futures::channel::mpsc::unbounded();
    input::spawn(input_tx);

    Application::new().with_assets(assets::Assets).run(move |cx: &mut App| {
        if let Err(e) = cx.text_system().add_fonts(assets::fonts()) {
            eprintln!("papier: could not load bundled fonts: {e}");
        }
        let bounds = Bounds::centered(None, size(px(1180.0), px(760.0)), cx);
        let options = WindowOptions {
            app_id: Some("papier".into()),
            titlebar: Some(TitlebarOptions { title: Some("Papier".into()), ..Default::default() }),
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..Default::default()
        };
        cx.open_window(options, move |window, cx| {
            if std::env::var_os("PAPIER_FULLSCREEN").is_some() {
                window.toggle_fullscreen();
            }
            cx.new(|cx| app::Papier::new(input_rx, window, cx))
        })
        .expect("failed to open window");
        cx.on_window_closed(|cx| cx.quit()).detach();
        cx.activate(true);
    });
}
