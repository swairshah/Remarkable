//! The home screen, papier's xochitl look: the status bar, "My files" in
//! Garamond with the black "+ NOTEBOOK" and outlined sort buttons, a 2px
//! rule, then the thumbnail grid — folders first, titles centered under
//! 1px-outlined covers, a gray date/pages line. Plus the modal dialogs.

use gpui::{AnyElement, ClickEvent, Context, MouseButton, ObjectFit, SharedString, Window, div, img, prelude::*, px, svg};

use crate::{
    app::{Dialog, Papier},
    assets::icon,
    doc::Sync,
    model::{Doc, Sort},
    status::{ago, rel_date},
    theme::*,
};

/// The tablet's header buttons: filled black, or a 2px black outline.
fn hdr_button(id: &'static str, label: impl Into<String>, filled: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .h(px(BTN_H))
        .px(px(16.0))
        .flex()
        .items_center()
        .justify_center()
        .font_family(MONO)
        .text_size(px(13.0))
        .border_2()
        .border_color(c(BLACK))
        .when(filled, |d| d.bg(c(BLACK)).text_color(c(WHITE)).hover(|s| s.bg(c(0x333333))))
        .when(!filled, |d| d.bg(c(WHITE)).text_color(c(BLACK)).hover(|s| s.bg(c(WASH))))
        .child(label.into())
}

fn pages_label(d: &Doc) -> String {
    let n = d.page_count();
    if n == 1 { "1 page".into() } else { format!("{n} pages") }
}

impl Papier {
    pub fn status_bar(&self, doc: Option<(String, String, Sync)>, cx: &mut Context<Self>) -> AnyElement {
        let st = &self.status;
        let mid: AnyElement = match &doc {
            Some((title, counter, _)) => div()
                .flex()
                .gap(px(10.0))
                .items_baseline()
                .child(div().text_size(px(16.0)).max_w(px(520.0)).line_clamp(1).text_ellipsis().child(title.clone()))
                .child(div().text_size(px(13.0)).text_color(c(GRAY)).font_family(MONO).child(counter.clone()))
                .into_any_element(),
            None => div().into_any_element(),
        };
        let (sync_icon, sync_text) = match &doc {
            Some((_, _, s)) => match s {
                Sync::Dirty | Sync::Saving => ("sync", "Saving".to_owned()),
                Sync::Saved => ("check", "Synced".to_owned()),
                Sync::Error(_) => ("offline", "Kept locally".to_owned()),
                _ => ("check", String::new()),
            },
            None if self.mirror.is_some() && self.lib_error.is_none() => {
                use std::sync::atomic::Ordering::Relaxed;
                let m = self.mirror.as_ref().unwrap();
                let (done, total) = (m.0.load(Relaxed), m.1.load(Relaxed));
                if total == 0 { ("sync", "Synced".to_owned()) } else { ("sync", format!("Saving for offline {done}/{total}")) }
            }
            None => match (&self.lib_error, self.synced_at) {
                (Some(_), Some(at)) => ("offline", format!("Offline · synced {}", ago(at))),
                (Some(_), None) => ("offline", "Offline".to_owned()),
                (None, Some(at)) => ("check", format!("Synced {}", ago(at))),
                (None, None) => ("sync", "Connecting…".to_owned()),
            },
        };
        let strip = match &self.screen {
            crate::app::Screen::Doc(dv) if dv.toolbar_open => STRIP_W,
            crate::app::Screen::Doc(_) => 60.0,
            _ => 0.0,
        };
        let mut bar = div()
            .id("status")
            .absolute()
            .top_0()
            .left_0()
            .right(px(strip))
            .h(px(STATUS_H))
            .bg(c(WHITE))
            .border_b_2()
            .border_color(c(BLACK))
            .flex()
            .items_center()
            .px(px(14.0))
            .gap(px(14.0))
            .child(div().text_size(px(16.0)).child(st.clock()))
            .child(div().flex_1().flex().justify_center().child(mid))
            .child(
                div()
                    .id("sync")
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .text_size(px(13.0))
                    .text_color(c(0x555555))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.refresh(cx);
                        this.toast("Syncing…");
                        cx.notify();
                    }))
                    .child(svg().path(icon(sync_icon)).size(px(15.0)).text_color(c(0x555555)))
                    .child(sync_text),
            )
            .child(wifi(st.wifi))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .text_size(px(14.0))
                    .child(if st.battery >= 0 { format!("{}%", st.battery) } else { "--".into() })
                    .child(battery(st.battery, st.charging)),
            );
        if doc.is_some() {
            bar = bar.child(hdr_button("close-doc", "CLOSE", false).h(px(22.0)).px(px(10.0)).text_size(px(11.0)).on_click(cx.listener(|this, _, _, cx| this.go_home(cx))));
        }
        bar.into_any_element()
    }

    pub fn render_home(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let (vw, _) = self.viewport;
        if self.home_momentum() {
            window.request_animation_frame();
        }
        let docs = self.visible_docs();
        let folders = self.folders();
        let inner = (vw - 2.0 * SIDE_PAD).max(CELL_W);
        let cols = ((inner / CELL_W).floor() as usize).max(1);
        // Leave slack: cells that exactly fill the row overflow by a hair
        // once layout rounds them to device pixels, and the last one wraps
        // (fullscreen showed 4 per row where a smaller window showed 5).
        let cell_w = (inner / cols as f32).floor() - 1.0;

        let title: AnyElement = match &self.folder {
            None => div().child("My files").into_any_element(),
            Some(f) => div()
                .id("crumb")
                .flex()
                .items_center()
                .gap(px(4.0))
                .cursor_pointer()
                .on_click(cx.listener(|this, _, _, cx| {
                    this.folder = None;
                    cx.notify();
                }))
                .child(svg().path(icon("left")).size(px(24.0)).text_color(c(BLACK)))
                .child(f.clone())
                .into_any_element(),
        };
        let sort_label = match self.sort {
            Sort::Title => "A - Z",
            Sort::Added => "ADDED",
            Sort::Recent => "RECENT",
        };
        let header = div()
            .h(px(HEADER_H))
            .px(px(SIDE_PAD))
            .flex()
            .items_center()
            .gap(px(12.0))
            .child(div().flex_1().text_size(px(30.0)).font_weight(gpui::FontWeight::SEMIBOLD).child(title))
            .child(
                div()
                    .id("find")
                    .size(px(BTN_H))
                    .flex()
                    .items_center()
                    .justify_center()
                    .hover(|s| s.bg(c(WASH)))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.dialog = Some(Dialog::Find { query: String::new(), sel: 0 });
                        cx.notify();
                    }))
                    .child(svg().path(icon("search")).size(px(20.0)).text_color(c(BLACK))),
            )
            .child(
                div()
                    .id("settings")
                    .size(px(BTN_H))
                    .flex()
                    .items_center()
                    .justify_center()
                    .hover(|s| s.bg(c(WASH)))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.dialog = Some(Dialog::Settings { server: this.settings.server.clone(), test: None });
                        cx.notify();
                    }))
                    .child(svg().path(icon("sliders")).size(px(20.0)).text_color(c(BLACK))),
            )
            .child(hdr_button("sort", sort_label, false).w(px(112.0)).on_click(cx.listener(|this, _, _, cx| {
                let i = Sort::ALL.iter().position(|s| *s == this.sort).unwrap_or(0);
                this.set_sort(Sort::ALL[(i + 1) % Sort::ALL.len()]);
                cx.notify();
            })))
            .child(hdr_button("new", "+ NOTEBOOK", true).w(px(160.0)).on_click(cx.listener(|this, _, _, cx| {
                this.dialog = Some(Dialog::NewNotebook { title: String::new() });
                cx.notify();
            })));

        let mut cells: Vec<AnyElement> = Vec::new();
        for (i, (name, count)) in folders.into_iter().enumerate() {
            let n = name.clone();
            cells.push(
                div()
                    .id(("folder", i))
                    .w(px(cell_w))
                    .flex()
                    .flex_col()
                    .items_center()
                    .pt(px(12.0))
                    .pb(px(26.0))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.folder = Some(n.clone());
                        this.home_scroll.set_offset(gpui::point(px(0.0), px(0.0)));
                        cx.notify();
                    }))
                    .child(folder_glyph())
                    .child(div().mt(px(10.0)).text_size(px(17.0)).max_w(px(cell_w - 24.0)).line_clamp(1).text_ellipsis().child(name))
                    .child(div().mt(px(2.0)).font_family(MONO).text_size(px(11.0)).text_color(c(GRAY)).child(format!("{count} items")))
                    .into_any_element(),
            );
        }
        for (i, doc) in docs.iter().enumerate() {
            let cover = self.cover(doc, cx);
            let id = doc.id.clone();
            let id2 = doc.id.clone();
            let title = doc.meta.title.clone();
            let date = match self.sort {
                Sort::Added => rel_date(doc.added_at.or(doc.mtime)),
                _ => rel_date(doc.mtime),
            };
            let thumb = div()
                .w(px(THUMB_W))
                .h(px(THUMB_H))
                .bg(c(WHITE))
                .border_1()
                .border_color(c(BLACK))
                .overflow_hidden()
                .flex()
                .items_center()
                .justify_center()
                .child(match cover {
                    Some(image) => img(image).size_full().object_fit(ObjectFit::Cover).into_any_element(),
                    None => svg()
                        .path(icon(if doc.is_notebook() { "notebook" } else { "book" }))
                        .size(px(30.0))
                        .text_color(c(LIGHT))
                        .into_any_element(),
                });
            cells.push(
                div()
                    .id(("doc", i))
                    .group("cell")
                    .w(px(cell_w))
                    .flex()
                    .flex_col()
                    .items_center()
                    .pt(px(12.0))
                    .pb(px(26.0))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.open_doc(&id, window, cx)))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, _, _, cx| {
                            this.dialog = Some(Dialog::DocMenu { id: id2.clone(), title: title.clone() });
                            cx.notify();
                        }),
                    )
                    .child(div().p(px(1.0)).group_hover("cell", |s| s.bg(c(BLACK))).child(thumb))
                    .child(
                        div()
                            .mt(px(9.0))
                            .h(px(40.0))
                            .w(px(cell_w - 24.0))
                            .text_size(px(16.5))
                            .line_height(px(20.0))
                            .text_center()
                            .line_clamp(2)
                            .text_ellipsis()
                            .child(doc.meta.title.clone()),
                    )
                    .child(div().mt(px(2.0)).font_family(MONO).text_size(px(11.0)).text_color(c(GRAY)).child(format!("{date}  -  {}", pages_label(doc))))
                    .into_any_element(),
            );
        }

        let empty = cells.is_empty();
        let body: AnyElement = if empty {
            let (a, b) = if self.loading {
                ("Loading library…", String::new())
            } else if self.lib_error.is_some() {
                ("Can't reach the papier cloud.", format!("{} — check Tailscale, or the server in settings.", self.client.root))
            } else {
                ("Nothing here yet.", "Documents from the tablet and the web viewer appear here.".to_owned())
            };
            div()
                .px(px(SIDE_PAD))
                .pt(px(34.0))
                .flex()
                .flex_col()
                .gap(px(10.0))
                .child(div().text_size(px(20.0)).child(a))
                .child(div().text_size(px(15.0)).text_color(c(GRAY)).child(b))
                .into_any_element()
        } else {
            div()
                .id("grid")
                .flex_1()
                .overflow_y_scroll()
                .track_scroll(&self.home_scroll)
                .child(div().px(px(SIDE_PAD)).pt(px(8.0)).flex().flex_wrap().children(cells))
                .into_any_element()
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(c(WHITE))
            .child(div().h(px(STATUS_H)))
            .child(header)
            .child(div().mx(px(0.0)).h(px(2.0)).bg(c(BLACK)))
            .child(body)
            .child(self.status_bar(None, cx))
            .into_any_element()
    }

    // ------------------------------------------------------------ dialogs

    pub fn render_dialog(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let dialog = self.dialog.as_ref()?;
        let row = |id: &'static str, label: String| {
            div()
                .id(id)
                .h(px(DLG_ROW_H))
                .px(px(22.0))
                .flex()
                .items_center()
                .text_size(px(18.0))
                .border_t_1()
                .border_color(c(LIGHT))
                .hover(|s| s.bg(c(WASH)))
                .child(label)
        };
        let field = |text: &str, placeholder: &str| {
            div()
                .mx(px(22.0))
                .mb(px(16.0))
                .h(px(40.0))
                .px(px(10.0))
                .border_2()
                .border_color(c(BLACK))
                .flex()
                .items_center()
                .font_family(MONO)
                .text_size(px(14.0))
                .child(if text.is_empty() {
                    div().text_color(c(GRAY)).child(placeholder.to_owned()).into_any_element()
                } else {
                    div().child(text.to_owned()).into_any_element()
                })
                .child(div().w(px(1.5)).h(px(18.0)).bg(c(BLACK)))
        };
        let heading = |t: &str| div().px(px(22.0)).pt(px(20.0)).pb(px(14.0)).text_size(px(24.0)).font_weight(gpui::FontWeight::SEMIBOLD).child(t.to_owned());
        let actions = |ok: &'static str, ok_label: &'static str| {
            div()
                .flex()
                .justify_end()
                .gap(px(12.0))
                .px(px(22.0))
                .pb(px(20.0))
                .child(hdr_button("dlg-cancel", "CANCEL", false).on_click(cx.listener(|this, _, _, cx| {
                    this.dialog = None;
                    cx.notify();
                })))
                .child(hdr_button(ok, ok_label, true).on_click(cx.listener(|this, _, window, cx| {
                    this.dialog_confirm(window, cx);
                    cx.notify();
                })))
        };

        let (width, content): (f32, AnyElement) = match dialog {
            Dialog::NewNotebook { title } => (
                DLG_W,
                div()
                    .child(heading("New notebook"))
                    .child(field(title, "Title"))
                    .child(div().px(px(22.0)).pb(px(16.0)).text_size(px(14.0)).text_color(c(GRAY)).child("Appears here now, and on the reMarkable after its next sync."))
                    .child(actions("dlg-create", "CREATE"))
                    .into_any_element(),
            ),
            Dialog::Settings { server, test } => {
                let paper = self.settings.paper.clone();
                let s2 = server.clone();
                let tones = div().flex().gap(px(8.0)).children(["white", "paper", "soft"].into_iter().map(|t| {
                    let active = paper == t;
                    div()
                        .id(t)
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .px(px(10.0))
                        .h(px(30.0))
                        .border_2()
                        .border_color(c(if active { BLACK } else { LIGHT }))
                        .font_family(MONO)
                        .text_size(px(12.0))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.settings.paper = t.to_owned();
                            this.settings.save();
                            cx.notify();
                        }))
                        .child(div().size(px(12.0)).border_1().border_color(c(GRAY)).bg(gpui::rgb(paper_u32(t))))
                        .child(t.to_uppercase())
                }));
                let theme = self.settings.theme.clone();
                let themes = div().flex().gap(px(8.0)).children(["system", "light", "dark"].into_iter().map(|t| {
                    let active = theme == t;
                    div()
                        .id(SharedString::from(format!("theme-{t}")))
                        .flex()
                        .items_center()
                        .px(px(12.0))
                        .h(px(30.0))
                        .border_2()
                        .border_color(c(if active { BLACK } else { LIGHT }))
                        .font_family(MONO)
                        .text_size(px(12.0))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.settings.theme = t.to_owned();
                            this.settings.save();
                            cx.notify();
                        }))
                        .child(t.to_uppercase())
                }));
                (
                    DLG_W + 60.0,
                    div()
                        .child(heading("Settings"))
                        .child(div().px(px(22.0)).pb(px(6.0)).font_family(MONO).text_size(px(11.0)).text_color(c(GRAY)).child("PAPIER SERVER (TAILNET)"))
                        .child(field(server, "http://100.x.y.z:8000"))
                        .child(
                            div()
                                .px(px(22.0))
                                .pb(px(16.0))
                                .flex()
                                .items_center()
                                .gap(px(12.0))
                                .child(hdr_button("dlg-test", "TEST", false).h(px(28.0)).on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(Dialog::Settings { test, .. }) = this.dialog.as_mut() {
                                        *test = Some("Testing…".into());
                                    }
                                    this.test_server(s2.clone(), cx);
                                    cx.notify();
                                })))
                                .child(div().text_size(px(14.0)).text_color(c(0x555555)).child(test.clone().unwrap_or_default())),
                        )
                        .child(div().px(px(22.0)).pb(px(6.0)).font_family(MONO).text_size(px(11.0)).text_color(c(GRAY)).child("THEME"))
                        .child(div().px(px(22.0)).pb(px(16.0)).child(themes))
                        .child(div().px(px(22.0)).pb(px(6.0)).font_family(MONO).text_size(px(11.0)).text_color(c(GRAY)).child("PAPER"))
                        .child(div().px(px(22.0)).pb(px(18.0)).child(tones))
                        .child(
                            div()
                                .px(px(22.0))
                                .pb(px(16.0))
                                .text_size(px(13.0))
                                .text_color(c(GRAY))
                                .child("Pen: draw · barrel button or the rubber end: erase. Fingers: swipe to turn, pinch to zoom, two fingers down for the library, up for the previous document."),
                        )
                        .child(actions("dlg-save", "SAVE"))
                        .into_any_element(),
                )
            }
            Dialog::Delete { title, .. } => (
                DLG_W,
                div()
                    .child(heading("Delete document?"))
                    .child(div().px(px(22.0)).pb(px(8.0)).text_size(px(17.0)).child(format!("“{title}”")))
                    .child(div().px(px(22.0)).pb(px(18.0)).text_size(px(14.0)).text_color(c(GRAY)).child("It leaves Papier and the web now, the reMarkable after its next sync. Recoverable from Papier trash."))
                    .child(actions("dlg-delete", "DELETE"))
                    .into_any_element(),
            ),
            Dialog::DocMenu { id, title } => {
                let (id1, id2, t2) = (id.clone(), id.clone(), title.clone());
                (
                    DLG_W - 80.0,
                    div()
                        .child(div().px(px(22.0)).py(px(16.0)).text_size(px(19.0)).line_clamp(1).text_ellipsis().child(title.clone()))
                        .child(row("menu-open", "Open".into()).on_click(cx.listener(move |this, _, window, cx| this.open_doc(&id1, window, cx))))
                        .child(row("menu-delete", "Delete".into()).on_click(cx.listener(move |this, _, _, cx| {
                            this.dialog = Some(Dialog::Delete { id: id2.clone(), title: t2.clone() });
                            cx.notify();
                        })))
                        .child(row("menu-cancel", "Cancel".into()).text_color(c(GRAY)).on_click(cx.listener(|this, _, _, cx| {
                            this.dialog = None;
                            cx.notify();
                        })))
                        .into_any_element(),
                )
            }
            Dialog::GoTo { digits } => {
                // papier's numpad: big touch targets, no keyboard needed
                let key = |label: &'static str| {
                    div()
                        .id(label)
                        .h(px(56.0))
                        .flex_1()
                        .flex()
                        .items_center()
                        .justify_center()
                        .border_2()
                        .border_color(c(BLACK))
                        .text_size(px(24.0))
                        .hover(|s| s.bg(c(WASH)))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if let Some(Dialog::GoTo { digits }) = this.dialog.as_mut() {
                                match label {
                                    "⌫" => {
                                        digits.pop();
                                    }
                                    "GO" => {
                                        this.dialog_confirm(window, cx);
                                    }
                                    d if digits.len() < 4 => digits.push_str(d),
                                    _ => {}
                                }
                            }
                            cx.notify();
                        }))
                        .child(label)
                };
                let rows = [["1", "2", "3"], ["4", "5", "6"], ["7", "8", "9"], ["⌫", "0", "GO"]];
                let total = match &self.screen {
                    crate::app::Screen::Doc(dv) => dv.seq.len(),
                    _ => 0,
                };
                (
                    320.0,
                    div()
                        .child(heading("Go to page"))
                        .child(
                            div()
                                .px(px(22.0))
                                .pb(px(14.0))
                                .flex()
                                .items_baseline()
                                .gap(px(8.0))
                                .child(div().text_size(px(34.0)).child(if digits.is_empty() { "–".to_owned() } else { digits.clone() }))
                                .child(div().text_size(px(15.0)).text_color(c(GRAY)).child(format!("of {total}"))),
                        )
                        .child(div().px(px(22.0)).pb(px(22.0)).flex().flex_col().gap(px(8.0)).children(rows.into_iter().map(|r| div().flex().gap(px(8.0)).children(r.into_iter().map(key)))))
                        .into_any_element(),
                )
            }
            Dialog::Find { query, sel } => {
                let hits = self.find(query);
                let sel = (*sel).min(hits.len().saturating_sub(1));
                let list = div().pb(px(10.0)).children(hits.into_iter().enumerate().map(|(i, d)| {
                    let id = d.id.clone();
                    div()
                        .id(("hit", i))
                        .h(px(40.0))
                        .px(px(22.0))
                        .flex()
                        .items_center()
                        .gap(px(10.0))
                        .when(i == sel, |r| r.bg(c(BLACK)).text_color(c(WHITE)))
                        .when(i != sel, |r| r.hover(|s| s.bg(c(WASH))))
                        .on_click(cx.listener(move |this, _, window, cx| this.open_doc(&id, window, cx)))
                        .child(
                            svg()
                                .path(icon(if d.is_notebook() { "notebook" } else { "book" }))
                                .size(px(16.0))
                                .text_color(c(if i == sel { WHITE } else { GRAY })),
                        )
                        .child(div().flex_1().text_size(px(17.0)).line_clamp(1).text_ellipsis().child(d.meta.title.clone()))
                        .child(div().font_family(MONO).text_size(px(11.0)).text_color(c(GRAY)).child(rel_date(d.mtime)))
                }));
                (
                    DLG_W + 120.0,
                    div().pt(px(18.0)).child(field(query, "Find a document…")).child(list).into_any_element(),
                )
            }
        };

        let top = if matches!(dialog, Dialog::Find { .. }) { px(70.0) } else { px(0.0) };
        Some(
            div()
                .id("scrim")
                .absolute()
                .inset_0()
                .bg(ca(WHITE, 0.55))
                .flex()
                .justify_center()
                .when(top == px(0.0), |d| d.items_center())
                .when(top != px(0.0), |d| d.pt(top))
                .occlude()
                .on_click(cx.listener(|this, _, _, cx| {
                    this.dialog = None;
                    cx.notify();
                }))
                .child(
                    div()
                        .id("dialog")
                        .w(px(width))
                        .bg(c(WHITE))
                        .border_2()
                        .border_color(c(BLACK))
                        .on_click(|_, _, cx| cx.stop_propagation())
                        .child(content),
                )
                .into_any_element(),
        )
    }
}

fn folder_glyph() -> impl IntoElement {
    div()
        .w(px(THUMB_W))
        .h(px(THUMB_H))
        .flex()
        .flex_col()
        .justify_center()
        .child(div().w(px(64.0)).h(px(15.0)).bg(c(LIGHT)).border_2().border_color(c(BLACK)))
        .child(div().w(px(THUMB_W)).h(px(THUMB_H - 100.0)).mt(px(-2.0)).bg(c(LIGHT)).border_2().border_color(c(BLACK)))
}

/// The wifi fan: three bars rising, struck through when there's no link.
fn wifi(bars: i8) -> impl IntoElement {
    div().flex().items_end().gap(px(2.0)).h(px(14.0)).children((1..=3).map(move |i| {
        div().w(px(3.0)).h(px(4.0 + 4.5 * (i - 1) as f32)).bg(c(if bars >= i { BLACK } else { LIGHT }))
    }))
}

/// Outline + fill + nub, like the tablet's.
fn battery(pct: i32, charging: bool) -> impl IntoElement {
    let inner = 22.0 * (pct.clamp(0, 100) as f32 / 100.0);
    div()
        .flex()
        .items_center()
        .child(
            div()
                .w(px(28.0))
                .h(px(14.0))
                .border_2()
                .border_color(c(BLACK))
                .p(px(2.0))
                .child(div().h_full().w(px(inner)).bg(c(if charging { 0x555555 } else { BLACK }))),
        )
        .child(div().w(px(2.5)).h(px(6.0)).bg(c(BLACK)))
}
