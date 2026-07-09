//! The right-edge toolbar, xochitl-style: a collapse circle at the top
//! right; expanded, a vertical strip of icon buttons — tools, undo/redo,
//! page navigation, home. The active tool renders inverted. Geometry is
//! centralized here for the M6 pixel-fidelity pass (the user prefers the
//! toolbar on the right; a `handed` setting can mirror `tb_x` later).

use crate::draw::{BLACK, LIGHT, WHITE};
use crate::fb::{Framebuffer, SCREEN_W};
use crate::icons;
use crate::ink::Rect;

#[derive(Clone, Copy, PartialEq)]
pub enum Tool {
    Pen,
    Eraser,
    Lasso,
}

pub enum TbAction {
    Toggle,
    Tool(Tool),
    Undo,
    Redo,
    PagePrev,
    PageNext,
    GoTo, /* tap the page indicator -> numpad */
    Font, /* pick pi's handwriting face */
    Home,
    Swallow, /* inside the strip, between buttons: eat the press */
}

pub const TB_W: i32 = 104;
pub const TB_X0: i32 = SCREEN_W - TB_W;
pub const TB_BTN_H: i32 = 96;
pub const TB_TOP: i32 = 132; /* first button (below the toggle circle) */
const DIV_H: i32 = 25; /* divider slot */

/* the collapse/expand circle */
pub const TOGGLE_CX: i32 = SCREEN_W - 60;
pub const TOGGLE_CY: i32 = 60;
pub const TOGGLE_R: i32 = 36;

/// Button slots top to bottom (None = divider).
const SLOTS: [Option<TbAction>; 14] = [
    Some(TbAction::Tool(Tool::Pen)),
    Some(TbAction::Tool(Tool::Eraser)),
    Some(TbAction::Tool(Tool::Lasso)),
    None,
    Some(TbAction::Undo),
    Some(TbAction::Redo),
    None,
    Some(TbAction::PagePrev),
    Some(TbAction::GoTo),
    Some(TbAction::PageNext),
    None,
    Some(TbAction::Font),
    Some(TbAction::Home),
    None,
];

fn slot_y(i: usize) -> i32 {
    let mut y = TB_TOP;
    for s in SLOTS.iter().take(i) {
        y += if s.is_some() { TB_BTN_H } else { DIV_H };
    }
    y
}

fn strip_h() -> i32 {
    slot_y(SLOTS.len())
}

/// The chrome rect (for input swallowing and restore_chrome_over).
pub fn tb_rect(open: bool) -> Rect {
    if open {
        Rect { x0: TB_X0, y0: 0, x1: SCREEN_W - 1, y1: strip_h() + 16 }
    } else {
        Rect {
            x0: TOGGLE_CX - TOGGLE_R - 4,
            y0: TOGGLE_CY - TOGGLE_R - 4,
            x1: SCREEN_W - 1,
            y1: TOGGLE_CY + TOGGLE_R + 4,
        }
    }
}

/// What a press at (x, y) does. None = not toolbar territory.
pub fn hit(x: i32, y: i32, open: bool) -> Option<TbAction> {
    let dx = x - TOGGLE_CX;
    let dy = y - TOGGLE_CY;
    if dx * dx + dy * dy <= (TOGGLE_R + 8) * (TOGGLE_R + 8) {
        return Some(TbAction::Toggle);
    }
    if !open || x < TB_X0 || y > strip_h() {
        return None;
    }
    for (i, s) in SLOTS.iter().enumerate() {
        if let Some(a) = s {
            let sy = slot_y(i);
            if y >= sy && y < sy + TB_BTN_H {
                return Some(match a {
                    TbAction::Tool(t) => TbAction::Tool(*t),
                    TbAction::Undo => TbAction::Undo,
                    TbAction::Redo => TbAction::Redo,
                    TbAction::PagePrev => TbAction::PagePrev,
                    TbAction::PageNext => TbAction::PageNext,
                    TbAction::GoTo => TbAction::GoTo,
                    TbAction::Font => TbAction::Font,
                    TbAction::Home => TbAction::Home,
                    TbAction::Toggle | TbAction::Swallow => TbAction::Swallow,
                });
            }
        }
    }
    /* inside the strip but between buttons: eat the press, do nothing */
    Some(TbAction::Swallow)
}

/// A white disc with a black ring (the toggle button).
fn ring(fb: &mut Framebuffer, cx: i32, cy: i32, r: i32) {
    fb.disc(cx, cy, r, BLACK);
    fb.disc(cx, cy, r - 3, WHITE);
}

/// Paint the toolbar chrome. `page` is (current 1-based, count).
pub fn paint(
    fb: &mut Framebuffer,
    open: bool,
    tool: Tool,
    can_undo: bool,
    can_redo: bool,
    page: (usize, usize),
) {
    /* the toggle circle is always visible */
    ring(fb, TOGGLE_CX, TOGGLE_CY, TOGGLE_R);
    if open {
        /* an X when open, three dashes when closed */
        icons::draw_icon(fb, TOGGLE_CX, TOGGLE_CY, 26, &[(-9, -9), (9, 9), icons::UP, (9, -9), (-9, 9)], BLACK);
    } else {
        for d in -1..=1 {
            fb.fill_rect(TOGGLE_CX - 14, TOGGLE_CY + d * 10 - 2, 28, 4, BLACK);
        }
        return;
    }

    /* the strip */
    let h = strip_h();
    fb.fill_rect(TB_X0, 0, TB_W, h + 16, WHITE);
    fb.fill_rect(TB_X0, 0, 2, h + 16, BLACK);
    fb.fill_rect(TB_X0, h + 14, TB_W, 2, BLACK);
    /* re-draw the toggle over the strip's top-left corner */
    ring(fb, TOGGLE_CX, TOGGLE_CY, TOGGLE_R);
    icons::draw_icon(fb, TOGGLE_CX, TOGGLE_CY, 26, &[(-9, -9), (9, 9), icons::UP, (9, -9), (-9, 9)], BLACK);

    let cx = TB_X0 + TB_W / 2;
    for (i, s) in SLOTS.iter().enumerate() {
        let y = slot_y(i);
        match s {
            None => {
                fb.fill_rect(TB_X0 + 20, y + DIV_H / 2, TB_W - 40, 2, LIGHT);
            }
            Some(a) => {
                let cy = y + TB_BTN_H / 2;
                let (icon, active, enabled): (icons::Icon, bool, bool) = match a {
                    TbAction::Tool(Tool::Pen) => (icons::PEN, tool == Tool::Pen, true),
                    TbAction::Tool(Tool::Eraser) => (icons::ERASER, tool == Tool::Eraser, true),
                    TbAction::Tool(Tool::Lasso) => (icons::LASSO, tool == Tool::Lasso, true),
                    TbAction::Undo => (icons::UNDO, false, can_undo),
                    TbAction::Redo => (icons::REDO, false, can_redo),
                    TbAction::PagePrev => (icons::PREV, false, page.0 > 1),
                    TbAction::PageNext => (icons::NEXT, false, true),
                    TbAction::Home => (icons::HOME, false, true),
                    TbAction::GoTo => {
                        /* the page indicator button: text, not an icon */
                        let label = format!("{}/{}", page.0, page.1);
                        let tw = crate::draw::text_width(&label, 2);
                        fb.text(cx - tw / 2, cy - 7, &label, 2, BLACK);
                        continue;
                    }
                    TbAction::Font => {
                        /* "Aa" — the pi-handwriting-face picker */
                        let tw = crate::draw::text_width("Aa", 3);
                        fb.text(cx - tw / 2, cy - 10, "Aa", 3, BLACK);
                        continue;
                    }
                    TbAction::Toggle | TbAction::Swallow => continue,
                };
                if active {
                    fb.fill_rect(TB_X0 + 12, y + 4, TB_W - 24, TB_BTN_H - 8, BLACK);
                    icons::draw_icon(fb, cx, cy, 44, icon, WHITE);
                } else {
                    icons::draw_icon(fb, cx, cy, 44, icon, if enabled { BLACK } else { LIGHT });
                }
            }
        }
    }
}
