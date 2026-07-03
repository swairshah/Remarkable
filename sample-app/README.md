# sample-app — a minimal AppLoad app for the reMarkable 2

A doodle pad in plain C, meant as a **starting point for your own apps**.
It shows up as an icon in the tablet's AppLoad menu (the launcher that
[`../pi/pi-appload/`](../pi/pi-appload/) installs into xochitl) and
demonstrates the complete life of an app: map the framebuffer, draw pixels,
refresh the e-ink, react to pen/touch, exit cleanly. No toolkit, no
dependencies — one socket, one mmap, one event loop.

On the canvas: the pen inks (pressure-sensitive), the Marker's tail end
erases, fingers draw when the pen is away (palm rejection suppresses them
otherwise), CLEAR wipes, EXIT quits, and tapping the title cycles the
e-ink refresh mode so you can feel the latency/quality trade-off live.

**Prerequisite:** the xovi + AppLoad stack from `../pi/pi-appload/install.sh`
must already be on the tablet. This project only adds an app to it.

## Quick start

```sh
make            # cross-compile (docker, ghcr.io/toltec-dev/base image)
make deploy     # push to the tablet (default HOST=root@10.11.99.1)
```

Then on the tablet: open the AppLoad menu in the xochitl sidebar → tap
**sample**. Draw with pen or finger; CLEAR wipes the canvas; EXIT quits.
Long-press the icon to run windowed instead of fullscreen; close a
fullscreen app by dragging one finger from the top-center toward the center.

The dev loop is: edit `src/main.c` → `make deploy` → reopen the app.
No xochitl restart needed once the icon exists (first install may need
`make restart-ui` for the menu to notice the new app).

## Files

```
src/main.c                  the app — read this top to bottom, it's the tutorial
src/qtfb.h                  the AppLoad/qtfb wire protocol (framebuffer + input)
src/font5x7.h               tiny bitmap font so you can put text on screen
app/external.manifest.json  tells AppLoad what to launch (name, binary, env)
app/icon.png                the launcher icon (512x512 PNG)
test/fake-qtfb.py           fake server: run the app on your Mac, no tablet
Makefile                    build / preview / deploy / log / uninstall
```

## How an AppLoad app works

AppLoad (a xovi extension inside xochitl) scans
`/home/root/xovi/exthome/appload/*/` for app directories. For "external"
apps like this one it reads `external.manifest.json`, and on tap it spawns
the binary with the env var `QTFB_KEY` set. The app then talks to AppLoad
over `/tmp/qtfb.sock`:

1. send `MESSAGE_INITIALIZE` (its key + pixel format `FBFMT_RM2FB`)
2. get back the name of a shared-memory object → `mmap()` it: that buffer
   is the screen, 1404×1872 pixels, 16-bit RGB565
3. write pixels, send `MESSAGE_UPDATE` (whole screen or a dirty rectangle)
4. `recv()` gives you `MESSAGE_USERINPUT` packets: touch, pen (with
   pressure), and AppLoad's on-screen keyboard if enabled

Everything above is defined in `src/qtfb.h`. Protocol and AppLoad itself:
<https://github.com/asivery/rm-appload> (also has C++/Rust client libs and
QML-frontend examples — apps don't have to be external binaries).

## The latency story (why the pen is read directly)

AppLoad forwards input through xochitl's Qt loop, which stalls while the
e-ink refreshes — measured on-device, pen positions arrive from it in
bursts up to ~50 ms apart, and pressure is quantized to 0/100. So this app
opens the Wacom digitizer itself (`/dev/input`, we're root) and inks from
hardware events: ~1 ms input latency, real 0–4095 pressure, and the eraser
end of the Marker — none of which AppLoad's protocol carries. Fingers and
window plumbing still go through the socket.

Three consequences worth knowing:

- The digitizer maps to the whole screen, so direct inking is only correct
  fullscreen. The app detects windowed launches (AppLoad's own pen events
  disagree with the mapping) and falls back automatically.
- Ink updates are batched (one refresh per ~12 ms, `FLUSH_MS`) and drawn
  with the UFAST waveform — both essential; per-event updates with the
  default UI waveform queue up seconds of lag.
- The floor that remains (update → AppLoad texture → Qt render → e-ink
  driver) belongs to xochitl. Stock notebooks draw straight into the
  display driver and will always feel faster. True notebook-grade ink
  means leaving AppLoad entirely (swtcon, see timower/rM2-stuff).

## Testing without the tablet

```sh
make preview    # runs the app under qemu against test/fake-qtfb.py,
                # scripts some strokes, writes build/preview.png
open build/preview.png
```

This exercises the real binary end-to-end (handshake, drawing, input,
clean exit) — great for iterating on layout/UI logic away from the device.

## Other targets

```sh
make build-zig            # build with zig instead of docker (musl, static)
make deploy HOST=root@<ip>  # deploy over WiFi instead of USB
make log                  # tail xochitl journal (printf output lands there)
make kill                 # kill a running instance
make uninstall            # remove the app from the tablet
```

## Ideas from here

- Save/load the canvas to a file (you have a whole Linux under you;
  `workingDirectory` is the app dir)
- Talk to the network — WiFi must be on; USB alone has no internet
- Draw the current time and make a clock (the poll() loop already has a
  timeout path to hang periodic work on)
- Undo: keep strokes as point lists instead of only pixels
- Debug input: `make CFLAGS_EXTRA=-DDEBUG_INPUT deploy`, then `make log`
  prints every AppLoad input packet

## Troubleshooting

- Icon missing → `make restart-ui` (restarts xochitl with xovi; ~30 s)
- App dies instantly → `make log` while tapping the icon; the binary must
  be executable and built for arm32 (`file build/sample-app`)
- Screen frozen after OS update → the whole xovi stack needs re-installing:
  re-run `../pi/pi-appload/install.sh` (rebuilds the QML hashtab)
- Tablet UI completely broken → `ssh root@10.11.99.1 /home/root/xovi/stock`
  brings back the stock UI immediately
