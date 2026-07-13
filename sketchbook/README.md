# sketchbook — draw rough, get it rendered, on the reMarkable 2

[notebook](../notebook/)'s takeover tech reshaped into an artist's
sketchbook. The **whole page is a shared canvas**: sketch, write and erase
anywhere; pi's generated images land on the same page, wherever pi places
them. When you **pause**, the page is photographed to a pi agent running
headless in the background. pi is the **art director** between your page
and a Gemini image model: it decides *which region* of the page to ship
(your sketch alone — or your sketch plus handwritten notes inside the
crop, which the model reads natively), *what to say* about it (a literal
subject description that disambiguates wobbly strokes, or "follow the
handwritten instructions in the image"), and *where the output lands*
(aspect-fit into a free-space rect it picks from the measured layout).
Results come back as grainy graphite — quantized to the panel's 16 grays
with paper-tooth grain applied at paint time.

Iterate by just writing on the page: "darker", "no background", an arrow
at the bit you want changed — pi sends the *existing output* back to the
model as the base image (edit-in-place: same drawing, same strokes
elsewhere) together with your annotations, and replaces it where it
stood. Rub any of pi's outputs with the rubber (in empty space) to wipe
it; your ink is only ever yours.

```
 you sketch / write / annotate (vector ink, anywhere on the page)
   │ pause (2.8s) → page snapshot ──► pi --mode rpc (resident child)
   │                                     │ sketchbook_generate
   │                                     │   {region, prompt, edit_raster?,
   │                                     │    dest, replace?}
   │                                     ▼
   │                       {cmd:"crop"}       → region PNG (ink + rasters)
   │                       {cmd:"raster_get"} → prior output (edit base)
   │                                     │
   │                                     ▼
   │                       Gemini image model (nano-banana class):
   │                       reads handwriting inside the crop natively;
   │                       edit mode updates the SAME image in place
   │                                     │ PNG/JPEG → gray (no npm deps)
   │                                     ▼
   │                       {cmd:"place", rect} → aspect-fit, raster patch
   ▼
 display: rm2fb takeover — DU for ink, Wave::Text for rasters; grain
          (paper-tooth value noise) applied at blit/snapshot time on page
          coordinates, stored rasters stay CLEAN so edit round-trips
          never compound grain; rasters persist per page (render-NNNN.skr,
          id-tracked) and re-render under strokes on every flip/erase
```

## The tools pi gets

| tool | does |
|------|------|
| `sketchbook_generate {region, prompt, edit_raster?, dest, replace?}` | the star: crop → model → place. `region` frames what the model sees (handwriting inside is read and followed); `edit_raster` bases the generation on an existing output (in-place edit); `dest` is where it lands; `replace` swaps the old output out |
| `sketchbook_draw {svg, page?}` | small ink annotations (labels, arrows, answers) |
| `sketchbook_erase {id, page?}` | remove one of pi's ink patches |
| `sketchbook_view {page?}` | fresh half-scale PNG of any page (rasters included) |
| `sketchbook_goto {page}` | flip the tablet to a page |

The image model is `gemini-3.1-flash-image` (override with
`$SKETCHBOOK_IMG_MODEL`; `gemini-3-pro-image` gives richer marks).
Returned PNG *or JPEG* is decoded in the extension (node:zlib PNG reader
+ vendored jpeg-js), autocontrast-stretched so paper reads true white,
and handed to the app as raw grayscale. `$SKETCHBOOK_GRAIN` scales the
graphite tooth (0 off, 1 default, 1.4 grittier).

## Gestures

| Do this | And |
|---------|-----|
| Sketch or write anywhere | Pausing ~3s offers the page to pi |
| Tap the ⊙ button (top-right) | Unfold the toolbar: lasso select, eraser mode, generate now, pi watch on/off, refresh |
| Toolbar → lasso, circle things, drag | Select strokes AND pi's outputs; drag the marquee to move them |
| Toolbar → eraser icon (tap to cycle) | OBJECT: whole strokes · PIXEL: splits strokes where you rub, erases raster pixels · REGION: circle with the rubber, lift deletes everything inside |
| Flip the marker, rub your ink | Erase (whole strokes) |
| Flip the marker, rub one of pi's outputs (empty space) | Wipe that output (your ink survives) |
| Write feedback next to an output | pi edits it in place on the next pause |
| Swipe left / right with a finger | Next / previous page (past the last = new page) |
| Tap the top-left corner | Sidebar: pages, go-to, INSTRUCTIONS, LIBRARY, quiet mode |
| Sidebar → PI: AUTO / QUIET | Quiet mode: sketch in peace, nothing is sent |
| Power button | Sleep + real suspend; wake resumes in place |

## Build & deploy

Prereqs are notebook's: xovi + AppLoad (`../pi/pi-appload/install.sh`), pi
on the tablet (`../pi/pi-harness/install.sh`), WiFi on, and
`rustup target add armv7-unknown-linux-musleabihf`.

```sh
make                # cross-compile (cargo + rust-lld)
make preview        # no tablet: qemu + fake pi + fake generate → build/preview*.png
make fetch-server   # rm2fb_server (timower/rM2-stuff) into vendor/
make deploy         # push binary/scripts/extension/manifest to the device
make push-key       # one-time: ship $GEMINI_API_KEY (renders need it)
make log            # tail /tmp/sketchbook.log on the device
make kill           # stop a running session (restores xochitl)
```

Then tap **sketchbook** in the AppLoad menu. Generations cost one image-model
call each (~a cent); pauses that don't warrant a render cost one cheap
vision look. Quiet mode costs nothing.

## Desktop pipeline prototype

`tools/render.py` is the standalone pipeline this app grew from: sketch
PNG in → Gemini → e-ink-ready render out.

```sh
python3 tools/render.py test/cat-sketch.png build/cat-render.png --hint "a cat"
```
