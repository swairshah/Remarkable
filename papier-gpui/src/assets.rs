//! Bundled fonts and the line icons (24x24, 1.8px round strokes — drawn
//! to sit beside the reMarkable's own thin black toolbar glyphs).

use std::borrow::Cow;

use gpui::{AssetSource, SharedString};

macro_rules! fonts {
    ($($name:literal),* $(,)?) => {
        &[$(include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/fonts/", $name)) as &[u8]),*]
    };
}

pub const FONTS: &[&[u8]] = fonts!(
    "EBGaramond-Regular.ttf",
    "EBGaramond-SemiBold.ttf",
    "GoogleSansCode-Regular.ttf",
    "IBMPlexSans-400.ttf",
    "IBMPlexSans-500.ttf",
    "IBMPlexSans-600.ttf",
    "IBMPlexMono-400.ttf",
    "IBMPlexMono-500.ttf",
);

pub fn fonts() -> Vec<Cow<'static, [u8]>> {
    FONTS.iter().map(|b| Cow::Borrowed(*b)).collect()
}

const ICONS: &[(&str, &str)] = &[
    ("pen", r#"<path d="M4 20l1.2-4.6L15.8 4.8a2 2 0 0 1 2.8 0l.6.6a2 2 0 0 1 0 2.8L8.6 18.8z"/><path d="M14 6.6l3.4 3.4"/>"#),
    ("eraser", r#"<path d="M8.5 19.5L4 15a2 2 0 0 1 0-2.8l8.2-8.2a2 2 0 0 1 2.8 0l5 5a2 2 0 0 1 0 2.8l-7.7 7.7z"/><path d="M9.5 7l7.5 7.5"/><path d="M8.5 19.5H20"/>"#),
    ("highlighter", r#"<path d="M9 13.5l7.6-9a1.6 1.6 0 0 1 2.3-.2l1.9 1.6a1.6 1.6 0 0 1 .2 2.3L13.4 17z"/><path d="M9 13.5L7.6 17.5l2 1.7 3.8-2.2"/><path d="M7.6 17.5L5 20h4.3"/>"#),
    ("undo", r#"<path d="M9 14L4 9l5-5"/><path d="M4 9h10.5a5.5 5.5 0 0 1 0 11H11"/>"#),
    ("redo", r#"<path d="M15 14l5-5-5-5"/><path d="M20 9H9.5a5.5 5.5 0 0 0 0 11H13"/>"#),
    ("up", r#"<path d="M6 15l6-6 6 6"/>"#),
    ("down", r#"<path d="M6 9l6 6 6-6"/>"#),
    ("left", r#"<path d="M15 6l-6 6 6 6"/>"#),
    ("right", r#"<path d="M9 6l6 6-6 6"/>"#),
    ("hash", r#"<path d="M5 9h14M5 15h14M10.5 4L8.5 20M15.5 4l-2 16"/>"#),
    ("add-page", r#"<path d="M14 3H6.5A1.5 1.5 0 0 0 5 4.5v15A1.5 1.5 0 0 0 6.5 21h11a1.5 1.5 0 0 0 1.5-1.5V8z"/><path d="M14 3v5h5"/><path d="M12 11v6M9 14h6"/>"#),
    ("home", r#"<path d="M3.5 11L12 4l8.5 7"/><path d="M5.5 9.5V20h4.5v-6h4v6h4.5V9.5"/>"#),
    ("menu", r#"<path d="M5 7h14M5 12h14M5 17h14"/>"#),
    ("close", r#"<path d="M6 6l12 12M18 6L6 18"/>"#),
    ("finger", r#"<path d="M9.5 12V5.2a1.7 1.7 0 0 1 3.4 0V11"/><path d="M12.9 10.4a1.7 1.7 0 0 1 3.4 0v1.2"/><path d="M16.3 11.2a1.6 1.6 0 0 1 3.2 0V15a6 6 0 0 1-6 6h-1.3a6 6 0 0 1-4.9-2.6L4.6 15a1.6 1.6 0 0 1 2.6-1.9l2.3 2.6"/>"#),
    ("spread", r#"<path d="M12 6.5C10 5 7 4.5 3.5 5v13c3.5-.5 6.5 0 8.5 1.5 2-1.5 5-2 8.5-1.5V5C17 4.5 14 5 12 6.5z"/><path d="M12 6.5v13"/>"#),
    ("scroll", r#"<rect x="6" y="2.5" width="12" height="8" rx="1"/><rect x="6" y="13.5" width="12" height="8" rx="1"/>"#),
    ("fit", r#"<path d="M4 9V4h5M15 4h5v5M20 15v5h-5M9 20H4v-5"/>"#),
    ("sliders", r#"<path d="M4 7h9M17 7h3M4 12h3M11 12h9M4 17h11M19 17h1"/><circle cx="15" cy="7" r="2"/><circle cx="9" cy="12" r="2"/><circle cx="17" cy="17" r="2"/>"#),
    ("sync", r#"<path d="M19.5 10A8 8 0 0 0 5.6 7.2L4 9"/><path d="M4 4.5V9h4.5"/><path d="M4.5 14a8 8 0 0 0 13.9 2.8L20 15"/><path d="M20 19.5V15h-4.5"/>"#),
    ("offline", r#"<path d="M3 3l18 18"/><path d="M8.5 7.6A6 6 0 0 1 18 11a4 4 0 0 1 2.6 6.2"/><path d="M17 19H7a4.5 4.5 0 0 1-1.6-8.7"/>"#),
    ("check", r#"<path d="M5 12.5l4.5 4.5L19 7.5"/>"#),
    ("trash", r#"<path d="M4 7h16M10 11v6M14 11v6"/><path d="M6 7l1 12.5A1.5 1.5 0 0 0 8.5 21h7a1.5 1.5 0 0 0 1.5-1.5L18 7"/><path d="M9 7V4.5A1.5 1.5 0 0 1 10.5 3h3A1.5 1.5 0 0 1 15 4.5V7"/>"#),
    ("folder", r#"<path d="M3.5 7V18.5A1.5 1.5 0 0 0 5 20h14a1.5 1.5 0 0 0 1.5-1.5v-10A1.5 1.5 0 0 0 19 7h-7l-2-3H5A1.5 1.5 0 0 0 3.5 5.5z"/>"#),
    ("search", r#"<circle cx="11" cy="11" r="6.5"/><path d="M16 16l4.5 4.5"/>"#),
    ("notebook", r#"<path d="M6 3h12a1 1 0 0 1 1 1v16a1 1 0 0 1-1 1H6z"/><path d="M9 3v18M4 7h4M4 12h4M4 17h4"/>"#),
    ("book", r#"<path d="M4 5.5A1.5 1.5 0 0 1 5.5 4H11v16H5.5A1.5 1.5 0 0 1 4 18.5z"/><path d="M20 5.5A1.5 1.5 0 0 0 18.5 4H13v16h5.5a1.5 1.5 0 0 0 1.5-1.5z"/>"#),
    ("backspace", r#"<path d="M9 5h10.5A1.5 1.5 0 0 1 21 6.5v11a1.5 1.5 0 0 1-1.5 1.5H9l-6-7z"/><path d="M12 9l6 6M18 9l-6 6"/>"#),
];

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        let Some(name) = path.strip_prefix("icons/").and_then(|p| p.strip_suffix(".svg")) else {
            return Ok(None);
        };
        Ok(ICONS.iter().find(|(n, _)| *n == name).map(|(_, body)| {
            Cow::Owned(
                format!(
                    r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="black" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">{body}</svg>"#
                )
                .into_bytes(),
            )
        }))
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .map(|(n, _)| SharedString::from(format!("icons/{n}.svg")))
            .filter(|p| p.starts_with(path))
            .collect())
    }
}

pub fn icon(name: &str) -> SharedString {
    SharedString::from(format!("icons/{name}.svg"))
}
