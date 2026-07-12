# sketchbook — draw rough, get it rendered, on the reMarkable 2

[notebook](../notebook/)'s takeover tech reshaped into an artist's
sketchbook. Every page is a **spread**: the left panel is yours — sketch
with the pen, erase with the marker's rubber — and the right panel belongs
to pi. When you **pause sketching**, the spread is photographed to a pi
agent running headless in the background. If the sketch has taken shape,
pi describes what it sees ("a cat sitting upright, tail curled right"),
sends your ink to a Gemini image model with a prompt tuned for monochrome
graphite rendition, and the result — pencil shading, hatching, confident
linework, quantized to the panel's 16 grays — appears on the right panel,
vertically mirroring your sketch. Draw more, pause again: the render
updates. Rub the right panel with the rubber to wipe a render you dislike.

```
 you sketch (left panel, vector ink)          pi renders (right panel, raster)
   │ pause (2.8s) → spread snapshot ──► pi --mode rpc (resident child)
   │                                        │ sketchbook_render {subject, style?}
   │                                        ▼
   │                              {cmd:"sketch"} → left-panel PNG (ink bbox crop)
   │                                        │
   │                                        ▼
   │                              Gemini image model (nano-banana class):
   │                              "rough stylus sketch depicting <subject>;
   │                               redraw as graphite pencil, same pose..."
   │                                        │ PNG → gray (node:zlib, no deps)
   │                                        ▼
   │                              {cmd:"render", w, h, raw} → bilinear fit,
   │                              16-gray quantize, blit right panel (GL16)
   ▼
 display: rm2fb takeover — DU for your ink, Wave::Text for renders,
          render layer persists per page (render-NNNN.skr) and re-renders
          under strokes on every flip/erase (vector-first model intact)
```

## How pi renders

pi is spawned with `-e sketchbook-canvas.ts` (shipped next to the binary),
which registers tools that call back into the app over a unix socket
(`$SKETCHBOOK_SOCK`, JSON-lines):

| tool | wire | does |
|------|------|------|
| `sketchbook_render {subject, style?, page?}` | `sketch` + `render` | captures your sketch, generates with Gemini, places the result on the right panel |
| `sketchbook_draw {svg, page?}` | `{cmd:"draw",...}` | small annotations as pen strokes (labels, arrows) — left panel only by convention |
| `sketchbook_erase {id, page?}` | `{cmd:"erase",...}` | remove one of pi's ink patches |
| `sketchbook_view {page?}` | `{cmd:"view",...}` | fresh half-scale PNG of any spread (render layer included) |
| `sketchbook_goto {page}` | `{cmd:"goto",...}` | flip the tablet to a page |

The `subject` parameter is the interesting part: pi *reads* your rough
sketch and tells the image model what it depicts. A wobbly blob with
triangles becomes "a cat sitting upright, facing the viewer" — the
description disambiguates the strokes and materially improves the render.
Style defaults to graphite pencil; write "make it a watercolor" on the
page and pi passes it through.

The image model is `gemini-3.1-flash-image` (override with
`$SKETCHBOOK_IMG_MODEL`). The PNG that comes back is decoded **in the
extension** — a ~70-line PNG reader over `node:zlib`, no npm deps — and
handed to the app as raw grayscale; the app bilinear-fits it into the
right panel (mirroring your sketch's vertical extent), snaps it to the 16
gray levels GC16 can show, and refreshes with the flash-free 16-level
waveform.

## The render layer

The page model stays vector-first (your strokes + pi's ink patches, from
libreink-page); the render is a **raster layer underneath the strokes** —
exactly how the reader app paints book pages under ink. Erasing a stroke
re-renders raster + remaining ink intact; wiping the render (rubber on the
right panel) never touches your sketch. Per page, on disk:
`page-NNNN.json` (vectors) + `render-NNNN.skr` (raw gray + placement).

## Gestures

| Do this | And |
|---------|-----|
| Sketch in the left panel | Pausing ~3s offers the spread to pi |
| Cross the divider with the pen | The stroke ends at the divider — the right panel is pi's |
| Flip the marker, rub your ink | Erase (whole strokes) |
| Flip the marker, rub the right panel | Wipe the render (your sketch survives) |
| Swipe left / right with a finger | Next / previous spread (past the last = new page) |
| Tap the top-left corner | Sidebar: pages, go-to, INSTRUCTIONS, LIBRARY, quiet mode |
| Sidebar → PI: AUTO / QUIET | Quiet mode: sketch in peace, nothing is sent |
| Power button | Sleep + real suspend; wake resumes in place |

## Build & deploy

Prereqs are notebook's: xovi + AppLoad (`../pi/pi-appload/install.sh`), pi
on the tablet (`../pi/pi-harness/install.sh`), WiFi on, and
`rustup target add armv7-unknown-linux-musleabihf`.

```sh
make                # cross-compile (cargo + rust-lld)
make preview        # no tablet: qemu + fake pi + fake render → build/preview*.png
make fetch-server   # rm2fb_server (timower/rM2-stuff) into vendor/
make deploy         # push binary/scripts/extension/manifest to the device
make push-key       # one-time: ship $GEMINI_API_KEY (renders need it)
make log            # tail /tmp/sketchbook.log on the device
make kill           # stop a running session (restores xochitl)
```

Then tap **sketchbook** in the AppLoad menu. Renders cost one image-model
call each (~a cent); pauses that don't warrant a render cost one cheap
vision look. Quiet mode costs nothing.

## Desktop pipeline prototype

`tools/render.py` is the standalone pipeline this app grew from: sketch
PNG in → Gemini → e-ink-ready render out.

```sh
python3 tools/render.py test/cat-sketch.png build/cat-render.png --hint "a cat"
```
