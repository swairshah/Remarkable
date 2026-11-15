//! Raw evdev readers. GPUI's Wayland backend has no tablet or touch
//! support, so — like sketchpad — we read the stylus and touchscreen
//! directly (the user is in the `input` group). Events go out on an async
//! channel so the UI wakes the instant a sample lands; no polling delay.

use std::thread;

use evdev::{AbsoluteAxisCode, Device, EventSummary, KeyCode, SynchronizationCode};
use futures::channel::mpsc::UnboundedSender;

#[derive(Debug, Clone)]
pub enum InputEvent {
    Pen {
        /// Normalized over the built-in panel, native orientation.
        x: f32,
        y: f32,
        pressure: f32,
        touching: bool,
        in_range: bool,
        /// The rubber end, or the barrel button held.
        eraser: bool,
    },
    /// Normalized positions of all fingers on the glass, keyed by slot.
    Touch(Vec<(usize, f32, f32)>),
    Error(String),
}

struct Axis {
    min: i32,
    range: f32,
}

impl Axis {
    fn of(device: &Device, code: AbsoluteAxisCode) -> Option<Axis> {
        let info = device.get_absinfo().ok()?.find(|(axis, _)| *axis == code)?.1;
        Some(Axis { min: info.minimum(), range: (info.maximum() - info.minimum()).max(1) as f32 })
    }

    fn norm(&self, v: i32) -> f32 {
        ((v - self.min) as f32 / self.range).clamp(0.0, 1.0)
    }
}

pub fn spawn(sender: UnboundedSender<InputEvent>) {
    if let Some(path) = std::env::var_os("PAPIER_FAKE_INPUT") {
        let fake = sender.clone();
        thread::spawn(move || fake_loop(path.into(), fake));
    }
    let pen = sender.clone();
    thread::spawn(move || pen_loop(pen));
    thread::spawn(move || touch_loop(sender));
}

fn pen_loop(sender: UnboundedSender<InputEvent>) {
    let Some((_, mut device)) =
        evdev::enumerate().find(|(_, d)| d.supported_keys().is_some_and(|k| k.contains(KeyCode::BTN_TOOL_PEN)))
    else {
        let _ = sender.unbounded_send(InputEvent::Error("No stylus found — mouse drawing only".into()));
        return;
    };
    let (Some(ax), Some(ay)) = (Axis::of(&device, AbsoluteAxisCode::ABS_X), Axis::of(&device, AbsoluteAxisCode::ABS_Y)) else {
        return;
    };
    let ap = Axis::of(&device, AbsoluteAxisCode::ABS_PRESSURE);

    let (mut x, mut y, mut p) = (0, 0, 0);
    let (mut touching, mut pen, mut rubber, mut barrel) = (false, false, false, false);
    loop {
        let Ok(events) = device.fetch_events() else {
            let _ = sender.unbounded_send(InputEvent::Error("Stylus disconnected".into()));
            return;
        };
        for event in events {
            match event.destructure() {
                EventSummary::AbsoluteAxis(_, AbsoluteAxisCode::ABS_X, v) => x = v,
                EventSummary::AbsoluteAxis(_, AbsoluteAxisCode::ABS_Y, v) => y = v,
                EventSummary::AbsoluteAxis(_, AbsoluteAxisCode::ABS_PRESSURE, v) => p = v,
                EventSummary::Key(_, KeyCode::BTN_TOUCH, v) => touching = v != 0,
                EventSummary::Key(_, KeyCode::BTN_TOOL_PEN, v) => pen = v != 0,
                EventSummary::Key(_, KeyCode::BTN_TOOL_RUBBER, v) => rubber = v != 0,
                EventSummary::Key(_, KeyCode::BTN_STYLUS, v) => barrel = v != 0,
                EventSummary::Synchronization(_, SynchronizationCode::SYN_REPORT, _) => {
                    let pressure = match (&ap, touching) {
                        (Some(a), true) => a.norm(p).max(0.05),
                        (None, true) => 0.6,
                        _ => 0.0,
                    };
                    let frame = InputEvent::Pen {
                        x: ax.norm(x),
                        y: ay.norm(y),
                        pressure,
                        touching,
                        in_range: pen || rubber,
                        eraser: rubber || barrel,
                    };
                    if sender.unbounded_send(frame).is_err() {
                        return;
                    }
                }
                _ => {}
            }
        }
    }
}

fn touch_loop(sender: UnboundedSender<InputEvent>) {
    let Some((_, mut device)) = evdev::enumerate().find(|(_, d)| {
        let multitouch = d.supported_absolute_axes().is_some_and(|a| a.contains(AbsoluteAxisCode::ABS_MT_POSITION_X));
        let stylus = d.supported_keys().is_some_and(|k| k.contains(KeyCode::BTN_TOOL_PEN));
        let touchpad = d.supported_keys().is_some_and(|k| k.contains(KeyCode::BTN_TOOL_FINGER))
            && d.name().is_some_and(|n| n.to_ascii_lowercase().contains("touchpad"));
        multitouch && !stylus && !touchpad
    }) else {
        return; // no touchscreen: fingers simply do nothing
    };
    let (Some(ax), Some(ay)) = (
        Axis::of(&device, AbsoluteAxisCode::ABS_MT_POSITION_X),
        Axis::of(&device, AbsoluteAxisCode::ABS_MT_POSITION_Y),
    ) else {
        return;
    };

    const SLOTS: usize = 16;
    let mut active = [false; SLOTS];
    let mut pos = [(0i32, 0i32); SLOTS];
    let mut slot = 0usize;
    loop {
        let Ok(events) = device.fetch_events() else { return };
        for event in events {
            match event.destructure() {
                EventSummary::AbsoluteAxis(_, AbsoluteAxisCode::ABS_MT_SLOT, v) => slot = (v.max(0) as usize).min(SLOTS - 1),
                EventSummary::AbsoluteAxis(_, AbsoluteAxisCode::ABS_MT_TRACKING_ID, v) => active[slot] = v >= 0,
                EventSummary::AbsoluteAxis(_, AbsoluteAxisCode::ABS_MT_POSITION_X, v) => pos[slot].0 = v,
                EventSummary::AbsoluteAxis(_, AbsoluteAxisCode::ABS_MT_POSITION_Y, v) => pos[slot].1 = v,
                EventSummary::Synchronization(_, SynchronizationCode::SYN_REPORT, _) => {
                    let contacts = (0..SLOTS).filter(|&i| active[i]).map(|i| (i, ax.norm(pos[i].0), ay.norm(pos[i].1))).collect();
                    if sender.unbounded_send(InputEvent::Touch(contacts)).is_err() {
                        return;
                    }
                }
                _ => {}
            }
        }
    }
}

/// Test hook: replay scripted samples from a file or FIFO, one per line,
/// normalized panel coordinates —
///   pen <x> <y> <pressure> <touching 0|1> <in_range 0|1> [eraser 0|1]
///   touch [<x> <y>]...        (no pairs = all fingers up)
///   sleep <ms>
fn fake_loop(path: std::path::PathBuf, sender: UnboundedSender<InputEvent>) {
    use std::io::BufRead;
    loop {
        let Ok(f) = std::fs::File::open(&path) else { return };
        for line in std::io::BufReader::new(f).lines().map_while(Result::ok) {
            let w: Vec<&str> = line.split_whitespace().collect();
            let num = |i: usize| w.get(i).and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0);
            let ev = match w.first().copied() {
                Some("pen") => InputEvent::Pen {
                    x: num(1),
                    y: num(2),
                    pressure: num(3),
                    touching: num(4) != 0.0,
                    in_range: num(5) != 0.0,
                    eraser: num(6) != 0.0,
                },
                Some("touch") => InputEvent::Touch((0..(w.len() - 1) / 2).map(|i| (i, num(1 + 2 * i), num(2 + 2 * i))).collect()),
                Some("sleep") => {
                    thread::sleep(std::time::Duration::from_millis(num(1) as u64));
                    continue;
                }
                _ => continue,
            };
            if sender.unbounded_send(ev).is_err() {
                return;
            }
            thread::sleep(std::time::Duration::from_millis(4));
        }
    }
}
