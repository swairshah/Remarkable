//! A minimal on-screen keyboard for renaming documents and naming folders.
//! Taps only, lowercase + digits; OK/CANCEL end the session.

use crate::draw::{text_width, BLACK, LIGHT, WHITE};
use crate::fb::{Framebuffer, SCREEN_H, SCREEN_W};
use crate::text;

pub struct Kb {
    pub text: String,
    pub title: &'static str,
}

pub enum KbAction {
    Ok(String),
    Cancel,
    Edited,
}

const ROWS: [&str; 4] = ["1234567890", "qwertyuiop", "asdfghjkl", "zxcvbnm"];

pub const KB_H: i32 = 5 * KEY_H + 6 * GAP + TITLE_H;
const KEY_H: i32 = 96;
const GAP: i32 = 10;
const TITLE_H: i32 = 110;

fn kb_y0() -> i32 {
    SCREEN_H - KB_H - 40
}

fn key_w(row: &str) -> i32 {
    (SCREEN_W - 48 - GAP * (row.len() as i32 - 1)) / row.len() as i32
}

fn row_x0(row: &str) -> i32 {
    (SCREEN_W - (key_w(row) * row.len() as i32 + GAP * (row.len() as i32 - 1))) / 2
}

impl Kb {
    pub fn new(title: &'static str, text: String) -> Self {
        Kb { text, title }
    }

    pub fn render(&self, fb: &mut Framebuffer) {
        let y0 = kb_y0();
        fb.fill_rect(0, y0 - 8, SCREEN_W, KB_H + 48, WHITE);
        fb.fill_rect(0, y0 - 8, SCREEN_W, 2, BLACK);

        /* the field being edited */
        fb.text(24, y0 + 10, self.title, 2, crate::draw::GRAY);
        let field = format!("{}_", self.text);
        text::draw_line(fb, 24, y0 + 40, text::Face::Body, 40.0, &field);
        fb.fill_rect(24, y0 + TITLE_H - 14, SCREEN_W - 48, 2, LIGHT);

        for (ri, row) in ROWS.iter().enumerate() {
            let (kw, x0) = (key_w(row), row_x0(row));
            let y = y0 + TITLE_H + ri as i32 * (KEY_H + GAP);
            for (ci, c) in row.chars().enumerate() {
                let x = x0 + ci as i32 * (kw + GAP);
                fb.rect_outline(x, y, kw, KEY_H, 2, BLACK);
                let s = c.to_string();
                fb.text(x + (kw - text_width(&s, 3)) / 2, y + (KEY_H - 21) / 2, &s, 3, BLACK);
            }
        }
        /* bottom row: CANCEL | space | DEL | OK */
        let y = y0 + TITLE_H + 4 * (KEY_H + GAP);
        let labels = ["CANCEL", "SPACE", "DEL", "OK"];
        let widths = [280, 520, 240, 280];
        let total: i32 = widths.iter().sum::<i32>() + GAP * 3;
        let mut x = (SCREEN_W - total) / 2;
        for (label, w) in labels.iter().zip(widths) {
            fb.rect_outline(x, y, w, KEY_H, 2, BLACK);
            fb.text(x + (w - text_width(label, 3)) / 2, y + (KEY_H - 21) / 2, label, 3, BLACK);
            x += w + GAP;
        }
    }

    /// A press anywhere on screen while the keyboard is up.
    pub fn press(&mut self, x: i32, y: i32) -> KbAction {
        let y0 = kb_y0();
        for (ri, row) in ROWS.iter().enumerate() {
            let (kw, x0) = (key_w(row), row_x0(row));
            let ry = y0 + TITLE_H + ri as i32 * (KEY_H + GAP);
            if y >= ry && y < ry + KEY_H {
                let i = (x - x0) / (kw + GAP);
                if i >= 0 && (i as usize) < row.len() && x >= x0 {
                    if self.text.len() < 60 {
                        self.text.push(row.as_bytes()[i as usize] as char);
                    }
                    return KbAction::Edited;
                }
            }
        }
        let by = y0 + TITLE_H + 4 * (KEY_H + GAP);
        if y >= by && y < by + KEY_H {
            let widths = [280, 520, 240, 280];
            let total: i32 = widths.iter().sum::<i32>() + GAP * 3;
            let mut bx = (SCREEN_W - total) / 2;
            for (i, w) in widths.iter().enumerate() {
                if x >= bx && x < bx + w {
                    return match i {
                        0 => KbAction::Cancel,
                        1 => {
                            if self.text.len() < 60 && !self.text.is_empty() {
                                self.text.push(' ');
                            }
                            KbAction::Edited
                        }
                        2 => {
                            self.text.pop();
                            KbAction::Edited
                        }
                        _ => KbAction::Ok(self.text.trim().to_string()),
                    };
                }
                bx += w + GAP;
            }
        }
        KbAction::Edited /* stray taps just keep the keyboard up */
    }
}
