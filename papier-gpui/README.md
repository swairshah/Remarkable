# Papier for the Framework laptop

The papier UX from the reMarkable — the xochitl-style home grid, the
right-edge toolbar, pen-first pages with pi in the margin — on the
Framework 12's pen touchscreen, in [GPUI](https://www.gpui.rs). It talks to
the same papier cloud as the tablet, the web viewer and the iPad, so
everything in the web app is here and what you write here reaches the rest.

```
make run          # release build, windowed
make fullscreen   # the tablet feel (F11 toggles too)
make install      # ~/.local/bin/papier + a launcher entry
make test
```

## Sync

Local-first: every document lives on this laptop. After each good library
sync a background pass mirrors the whole library to disk — ink, the
source PDF (or page images), covers — and afterwards only re-fetches
documents whose version changed (`~/.cache/papier-gpui/<server>/synced.json`
records what's on disk; a replaced PDF's old file is deleted). Pages
always open from disk first and refresh when the server answers; with no
network, requests fail fast and everything reads from the mirror. Your
writes are kept locally and sent when the server is reachable.
`PAPIER_NO_MIRROR=1` turns the background pass off.


Same endpoints as the iPad app (`papier-ios`), over the tailnet
(`http://100.90.235.68:8000` by default; change it in Settings):

- **Library**: the ETagged `/papier/api/library` manifest, polled every 60s
  at home and every 20s inside a document. Covers and page rasters are
  version-stamped and cached on disk; the library is snapshotted so the app
  opens offline.
- **Ink**: pages load from the merged truth (`/papier/api/ink`). Edits save
  after papier's 2.8s writing pause (`POST /ink`), and on page turn, close
  and quit. A saved page stays as a local *pending* copy until the doc's
  version moves, same rule as the iPad. A save that fails offline is kept as
  *unsent* and retried on the next good sync.
- **Remote changes**: when the manifest shows the open document's version
  moved (web viewer, tablet, iPad, cloud pi), pages you aren't editing
  reload in place.
- **pi**: `/papier/api/pi/*`: open, mode (quiet by default, per doc),
  nudge, font, pause-after-save in auto mode, and event polling (0.5s busy
  / 1.5s idle). New pi patches draw themselves in over 1.4s.
- The page JSON round-trips exactly: user strokes, pi patches, the tablet's
  `highlights`, and any keys this app doesn't know.

## Input

GPUI's Wayland backend has no tablet or touch support, so (like
`../sketchpad`) the pen and touchscreen are read raw from evdev (you must
be in the `input` group) and mapped through the window's Hyprland
geometry. The pen on paper inks directly. Anything else — a finger, or the
nib on the toolbar, home grid or a dialog — becomes synthetic GPUI mouse
events: tap = click, long-press = right-click, drag = wheel with momentum.
Pointer events the compositor emulates from the pen are swallowed so
nothing fires twice.

| | |
|---|---|
| pen | write; rubber end or barrel button erases |
| one finger | scroll the pages with momentum; in two-page view, turn the leaf |
| two fingers down, from the top half | library (or out of a folder) |
| two fingers up, from the bottom half | previous document |
| pinch | zoom |
| swipe down from the top edge | status peek: title, page, sync, battery, CLOSE |
| long-press | doc menu (library), remove a highlight (page) |
| palm | ignored while the pen hovers and for 1.5s after |

Toolbar: pen (tap again for fine / medium / bold), eraser, highlighter,
finger-draws, undo, redo, pi mode, nudge, pi font, page up/down, go to
(numpad), add page, fit page / fit width, home. The round button collapses it.

Two views, toggled from the toolbar (or `v`), remembered across launches:

- **Scroll**: pages are one continuous vertical strip at a comfortable
  reading width (the fit button toggles whole-page). Touchpad and finger
  scrolls carry on with momentum; page up/down and the toolbar arrows glide.
- **Two pages**: a fitted spread, turned like a book with a real page
  curl. A finger (or a two-finger touchpad swipe) takes hold of the leaf
  where you touched it: that point follows your hand, the paper wraps a
  small cylinder along the fold between them (grab low and the corner
  peels diagonally), the next page shows on its back, the roll is shaded
  by its angle to the light, and it casts a shadow on the page beneath.
  The leaf stays bound at the spine. Let go a quarter of the way over (or
  flick) and it completes; otherwise it falls back. Arrows, page keys and
  the toolbar peel the lower corner over. The curl is rendered on the CPU
  (`src/curl.rs`; GPUI can't warp an image) across six threads, ~2–5ms a
  frame for the full spread in release.

Keys: `↑/↓` scroll · `PgUp/PgDn` `Space` a screenful · `←/→` previous/next
page (spread: turn) · `v` scroll / two pages · `Home/End` · `Esc` library ·
`Ctrl+Z` / `Ctrl+Shift+Z` · `p e h` tools · `f` finger-draws · `t` toolbar ·
`g` go to · `n` add page · `m` pi mode · `.` nudge · `0 w + -` zoom ·
`Ctrl+P` or `/` find · `Ctrl+N` notebook · `Ctrl+,` settings ·
`Ctrl+R` sync now · `Ctrl+Shift+D` light/dark · `F11` fullscreen. A mouse draws too; the wheel turns
pages, Ctrl+wheel zooms.

## Dark mode

Follows the system by default (System / Light / Dark in Settings). It's
sketchpad's "same paper by lamplight": warm charcoal where the tablet is
white, cream where it's black, pi's ink a lifted blue. Book pages are
tone-mapped so text reads cream on charcoal, but pictures are left as
they are, slightly dimmed: each print is segmented once on decode (dense
mid-tone or ink cells → connected boxes → merged → tightened to the
exact pixel and grown over a scan's plate), so every figure stays one
clean rectangle.

## Book pages from the PDF

When a book has its source PDF on the server (as the web viewer uses with
PDF.js), each page is rendered locally with poppler's `pdftoppm` at exactly
the size it's shown, and re-rendered after a zoom settles: vector text,
crisp at any size, in both views and through the page curl. Pages are laid
onto the 1404×1872 papier page exactly as mkbook does (crop inside a 30px
border, or the page inside its margins), so tablet ink lines up. The
tablet's pre-rendered PNG shows instantly until the sharp render lands,
and is the fallback for books without a source PDF. Needs `poppler`
(`pdftoppm`, `pdfinfo`).

## Rendering

Committed ink is composited with the paper tone and the book raster into
one page image (tiny-skia), so a heavily annotated page costs one textured
quad per frame. Full renders (a page coming into view, zoom, theme, pi
patches, undo) run on a background thread while the previous image stays
on screen; a pen stroke or eraser rub patches the raster in place. The
next pages are warmed ahead of the scroll. Only the stroke under the nib is
a live GPU path. Scrolling, glides and momentum advance on vsync frames
(`request_animation_frame`), and pen samples arrive on an async channel, so
the first frame after a sample is the one that shows it.

## Vendored GPUI

`vendor/gpui` is gpui 0.2.2 from crates.io with three small patches, all
marked `papier-gpui`:

1. `DispatchEventResult` is `pub` (it was `pub(crate)`, which made the
   public `Window::dispatch_event` uncallable; we need it for touch).
2. The Blade atlas skips a texture that was created and emptied in the same
   frame instead of panicking.
3. The Wayland client reports `axis_stop` (touchpad fingers lifted) as a
   `TouchPhase::Ended` scroll event, so the app can do kinetic scrolling.

## Testing hooks

- `PAPIER_OPEN=<doc id>` opens straight into a document.
- `PAPIER_FULLSCREEN=1` starts fullscreen.
- `PAPIER_READONLY=1` keeps ink saves local (no POSTs).
- `PAPIER_FAKE_INPUT=<file or fifo>` replays scripted samples through the
  real input path (`pen x y pressure touching in_range [eraser]`,
  `touch [x y]...`, `sleep ms`; coordinates normalized to the panel).
- `PAPIER_PRINT=page.png PAPIER_OUT=out.png cargo test dark_preview -- --ignored`
  renders a book page in dark mode to a PNG (checks picture detection).
- `CURL_FRONT=a.png CURL_BACK=b.png CURL_UNDER=c.png CURL_OUT=dir cargo test --release curl::preview -- --ignored --nocapture`
  renders curl frames from real pages and prints timings.
- `PAPIER_INPUT_TRANSFORM=0..3` overrides the panel rotation used to map
  the digitizer (mapping for a rotated panel is untested).

## Not yet ported

Lasso select/move/cut-paste, the pi standing-instructions page, folder
management (rename/move), and creating folders.
