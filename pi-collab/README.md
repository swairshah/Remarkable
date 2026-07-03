# pi-collab — handwrite to pi on the reMarkable 2

An AppLoad app that turns the tablet into a handwriting chat with the
[pi coding agent](https://pi.dev). You write a message with the pen, tap
**SEND**, and your ink is handed to a pi instance running headless in the
background (as an image). pi's reply streams back as text into a scrollable
conversation. pi keeps its normal tools, so you can handwrite instructions
and it will actually run commands, read/edit files, use your extensions —
all from the tablet, no keyboard.

It's the offspring of two siblings in this repo:
[`../sample-app-rust/`](../sample-app-rust/) (the qtfb framebuffer + direct
Wacom pen plumbing) and [`../pi/`](../pi/) (pi on the tablet). The
low-latency inking, palm rejection, and eraser all carry over from the
sample app; read its README for how the framebuffer/pen layers work.

## Prerequisites

- The xovi + AppLoad stack: `../pi/pi-appload/install.sh`.
- pi itself on the tablet: `../pi/pi-harness/install.sh` (pi-collab runs
  `/home/root/bin/pi --mode rpc` under the hood).
- WiFi on — pi talks to model APIs at runtime.

## Quick start

```sh
rustup target add armv7-unknown-linux-musleabihf   # once
make deploy                                        # build + push (HOST=root@<ip>)
```

Then on the tablet: AppLoad menu → **pi-collab**. Write in the bottom strip,
tap SEND. Drag the conversation with a finger to scroll.

## How it works

```
   pen ink ──SEND──▶ snapshot (crop+downscale) ──PNG──▶ pi --mode rpc
                                                            │  (stdin JSONL)
   conversation ◀── text deltas ◀── events ◀───────────────┘  (stdout JSONL)
```

- **One pi process, headless, for the app's whole life.** Spawned as
  `pi --mode rpc --session-id pi-collab` with stdin/stdout as a JSON-lines
  pipe (`src/pi_rpc.rs`). Its stdout fd is poll()ed in the main loop right
  alongside the qtfb socket and the pen device. `--session-id` keeps the
  whole thing one continuous pi session.
- **Your handwriting → an image.** On SEND, the writing canvas is read back
  out of the framebuffer, cropped to the ink's bounding box, downscaled, and
  encoded as a grayscale PNG (`src/png.rs`, no deps) that rides along in the
  RPC `prompt` command as a base64 image attachment.
- **pi's reply → streamed text, rendered by content type.** `message_update`
  text deltas append to the live reply bubble; tool runs and errors show as
  dimmed notes. Each reply is parsed (`md.rs`) into typed blocks and rendered
  in style: prose and headings in **EB Garamond**, fenced code in **Google
  Sans Code** in a gray box (language labelled, indentation preserved),
  bullets dotted, and SVG diagrams rasterized to a bitmap by a tiny built-in
  renderer (`svg.rs`) that falls back to a code box for anything it can't
  draw. Text is real antialiased TrueType (`text.rs`, via the pure-Rust
  `fontdue`); the small UI chrome (buttons, labels, logo) stays on the crisp
  5x7 bitmap font. The conversation viewport is painted with the quality
  e-ink waveform (so the antialiased grays are clean) while pen ink uses the
  fast waveform; it repaints on a throttle and auto-follows the bottom while
  pi types — unless you've scrolled up to read history.

  Fonts are embedded from `assets/fonts/` (both OFL-licensed). To swap the
  body face (e.g. to Iowan Old Style) replace `EBGaramond-*.ttf` and the
  `include_bytes!` paths in `text.rs`. A `.ttc` collection needs a face
  extracted first (`fonttools`), since fontdue loads a single face.
- **pi's replies are plain text.** The prompt asks pi to avoid markdown,
  since the log renders with a plain bitmap font. pi keeps its full tool set
  and whatever model/config your `~/.pi` settings specify (default here was
  `gpt-5.5`, which accepts images).

## Files

```
src/main.rs     the app: layout, input routing, send, pi-event handling
src/conv.rs     conversation model + scrollable layout/rendering + word wrap
src/md.rs       content detection: split a reply into typed blocks + render
src/text.rs     antialiased TrueType text (fontdue) + glyph cache + wrapping
src/svg.rs      minimal SVG rasterizer (rect/line/circle/polygon/path)
assets/fonts/   embedded TrueType fonts (EB Garamond, Google Sans Code)
src/pi_rpc.rs   the pi child process and its JSONL protocol
src/png.rs      grayscale PNG encoder + base64 (no dependencies)
src/qtfb.rs     AppLoad protocol (framebuffer + input)   [from sample-app-rust]
src/pen.rs      direct Wacom digitizer input             [from sample-app-rust]
src/draw.rs     framebuffer primitives (clip band, text, gray blit)
src/font.rs     full printable-ASCII 5x7 bitmap font
app/            AppLoad manifest + icon
test/           fake pi + fake qtfb server for `make preview`
```

## Testing without the tablet

```sh
make preview    # qemu runs the real binary against a FAKE pi -> build/preview.png
open build/preview.png
```

`test/fake-pi.py` speaks just enough of the RPC protocol to stream a canned
reply, so the whole pipeline (ink snapshot → PNG → prompt → streamed text →
wrapped rendering) is exercised end-to-end with no API key. In the container
there's no Wacom device, so the app takes its AppLoad-pen fallback — the same
path a windowed launch uses.

## Controls & limits

- **Write**: pen in the bottom canvas (pressure-sensitive; flip the Marker to
  erase). **SEND** / **CLEAR** buttons, or tap them with the pen tip.
- **Nib size**: the three dot buttons at the bottom-left pick the stroke
  width (small / medium / large); large is the default. Pressure still adds a
  little on top of the chosen base.
- The header shows the pi mark (an 8x8 bitmap traced from the pi.dev icon in
  `../pi/pi-appload`, drawn in `draw_logo`).
- **Scroll**: drag the conversation area with a finger. Palm rejection
  suppresses touch while the pen is near, so scrolling and writing don't fight.
- **Font size**: **A-** / **A+** in the header resize pi's text (scale 2–6)
  and reflow the log. Your handwriting snapshots are unaffected.
- **Deghosting**: e-ink accumulates faint residue from partial updates
  (worst after scrolling). The **REFRESH** button does a full-panel cleanup
  flash on demand. The app also does one automatically — but only once things
  settle (~0.7s after the last scroll, font change, or streamed reply), never
  mid-interaction, so it stays "not too much".
- **Fullscreen recommended.** The pen is read straight from the digitizer,
  which maps to the whole screen; a windowed launch (long-press the icon) is
  detected and falls back to AppLoad's slower pen events.
- The conversation is kept in memory (each ink snapshot is a small
  bitmap). Fine for long chats; it resets when you close the app, but pi's
  session file preserves the actual conversation.

## Troubleshooting

- Status shows `pi gone` / `pi did not start` → pi isn't installed or crashed.
  `make log` tails both the app and pi's stderr from xochitl's journal.
- Icon missing after first install → `make restart-ui`.
- Nothing happens on SEND → did you write in the *bottom* canvas? The status
  line reads `nothing written` if the canvas was blank.
