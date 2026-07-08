# collab — handwrite to pi on the reMarkable 2, with instant ink

The [pi-collab](../pi-collab/) product on the [riddle](https://github.com/MaximeRivest/riddle)
tech stack. You write a message with the pen, tap **SEND**, and your ink is
handed (as an image) to a pi instance running headless in the background.
pi's reply streams back as text into a scrollable conversation — markdown
rendered by content type, SVG diagrams rasterized. pi keeps its normal
tools, so handwritten instructions actually run commands on the tablet.

What's new versus pi-collab is everything under the drawing: **full
takeover**. Instead of living inside xochitl as an AppLoad/qtfb window, the
launcher stops xochitl, hosts the vendor e-ink engine with a bundled rm2fb
server (timower/rM2-stuff), and drives the panel directly:

- pen strokes go to the glass on the **DU waveform** (1-bit, the fastest
  the panel has) with ~8ms coalescing — the lowest-latency ink there is;
- conversation text still paints with the flash-free quality waveform
  (GL16), scrolling with DU, deghosting with a GC16 flash — chosen
  **per update**, not via qtfb's sticky global mode;
- input is read raw and grabbed: the Wacom digitizer (4096-level pressure,
  eraser), the touch panel (scroll, taps), and the power button.

The windowed qtfb backend is still in the binary (picked automatically when
`QTFB_KEY` is set), which is what the no-tablet preview harness runs.

## Prerequisites

- The xovi + AppLoad stack: `../pi/pi-appload/install.sh`.
- pi itself on the tablet: `../pi/pi-harness/install.sh` (collab runs
  `/home/root/bin/pi --mode rpc` under the hood).
- WiFi on — pi talks to model APIs at runtime.

## Quick start

```sh
rustup target add armv7-unknown-linux-musleabihf   # once
make fetch-server                                  # once: rm2fb server -> vendor/
make deploy                                        # build + push (HOST=root@<ip>)
```

Then on the tablet: AppLoad menu → **collab**. The screen flashes once as
xochitl stops and the takeover starts. Write in the bottom strip, tap SEND.

> ⚠️ **Takeover mode stops the vendor UI and runs as root.** The launch
> script always restarts xochitl when collab exits, but keep SSH access
> working before you install anything — that is your escape hatch:
> `ssh root@<tablet> 'systemctl start xochitl'` (or `make restore-ui`).

## Gestures

| Do this | And |
|---------|-----|
| Write in the canvas, tap SEND | pi reads your handwriting and replies |
| Flip the marker | Erase |
| Drag the conversation with a finger | Scroll (auto-follows while pi types) |
| Swipe down from the top edge, tap CLOSE | Exit to xochitl (a 5-finger tap still works as a fallback) |
| Power button | Sleep page + real suspend; press again to wake exactly where you were (WiFi is re-healed for pi) |

Header buttons: `A-`/`A+` step pi's text size, `REFRESH` is the manual
deghosting flash. The three dots pick the nib size.

## How it works

```
 pen (raw evdev, grabbed: 4096-level pressure, eraser, hardware rate)
   │ strokes                      touch (raw evdev: scroll/taps/5-finger)
   ▼                              power button (sleep page + suspend)
 collab ── SEND → snapshot ink → PNG ──► pi --mode rpc  (resident child)
   │                                          │ JSONL over stdin/stdout
   ▼ per-update waveform choice               ▼
 display backend                        text deltas, tool notices
   ├── rm2fb  — takeover: xochitl stopped, bundled rm2fb_server hosts the
   │           vendor engine; DU ink / GL16 text / GC16 deghost per update
   └── qtfb   — windowed fallback inside xochitl (QTFB_KEY set); also what
               `make preview` exercises under qemu
```

- **One pi process, headless, for the app's whole life** (`src/pi_rpc.rs`),
  with sessions in `~/.local/share/collab/sessions` and `--continue` across
  restarts. A UI-side transcript (`src/history.rs`) restores scrollback.
- **Your handwriting → an image**: the canvas is cropped to the ink,
  downscaled, PNG-encoded (`src/png.rs`, no deps), sent as base64.
- **pi's reply → typed blocks** (`src/md.rs`): EB Garamond prose/headings,
  Google Sans Code in code boxes, bullets, and a tiny SVG rasterizer
  (`src/svg.rs`) with a code-box fallback.
- **The takeover display** (`src/rm2fb.rs`, `src/display.rs`): a client of
  the rm2fb server's shm framebuffer + update socket, one 32-byte
  UpdateParams per region with the waveform in the message.

## Touch calibration

Raw rM2 touch coordinates are pixel-scale with only the Y axis inverted
(per rM2-stuff's rMlib, the library the rm2fb server itself is built from).
If taps ever land mirrored on a different panel revision, override with
`COLLAB_TOUCH_FLIP=none|x|y|xy` (default `y`) in `scripts/takeover.sh`.

## Preview without the tablet

```sh
make preview        # docker + qemu, fake pi, scripted strokes -> build/preview.png
```

This drives the windowed qtfb backend end to end (draw → SEND → streamed
reply → render). The takeover path can only be exercised on the device.

## Deploy notes

- `make fetch-server` downloads `rm2display.ipk` from timower/rM2-stuff's
  latest release and extracts `rm2fb_server` + `librm2fb_server.so` into
  `vendor/` (gitignored). `make deploy` pushes them next to the binary.
- Logs: the app writes to `/tmp/collab.log` (`make log`), the display
  server to `/tmp/rm2fb.log` (`make log-server`).
- `make kill` stops the transient `collab-takeover` unit; its trap restarts
  xochitl.
- Tested target: reMarkable 2, OS with libqsgepaper-based display stack
  (3.20+), xovi + AppLoad installed.
