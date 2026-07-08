# notebook — a paper notebook that writes back, on the reMarkable 2

[collab](../collab/)'s takeover tech, reshaped into a notebook. The whole
screen is a page: write with the pen, flip pages with a finger swipe, erase
with the marker's rubber end — like a quick-sheets notebook. When you
**pause writing**, the page is photographed to a pi agent running headless
in the background, which decides whether to respond. If it does, it
responds *on the page*: freeform gray ink — text in a single-stroke plotter
font, sketches, arrows, underlines, boxes — animated in stroke by stroke
like a ghost hand. If not, it stays silent (`pass`).

pi's contributions are **patches**: id-tracked stroke sets it can erase or
replace later with its tools. The page model is vector-first (your strokes
+ its patches), so erasing a patch that crosses your handwriting re-renders
your ink intact underneath. Its tools can never erase *your* ink; your
rubber can erase anything.

## How pi draws

pi is spawned with `-e notebook-canvas.ts` (shipped next to the binary),
which registers three tools that call back into the app over a unix socket
(`$NOTEBOOK_SOCK`, JSON-lines):

| tool | wire | does |
|------|------|------|
| `notebook_draw {svg, page?}` | `{cmd:"draw",...}` | SVG → pen strokes → patch id; animated onto the panel |
| `notebook_erase {id, page?}` | `{cmd:"erase",...}` | remove a patch; region re-rendered from vectors |
| `notebook_view {page?}` | `{cmd:"view",...}` | fresh half-scale PNG + patch list of any page |

The SVG's coordinate space **is the page** (1404x1872, y down). Everything
becomes strokes (`svg_ink.rs`): paths with real bezier flattening,
scanline-hatched small fills (arrowheads), and `<text>` via Hershey
single-stroke fonts (`hershey.rs`) — so text *draws*, it doesn't rasterize.
Three faces: **script** (Script Simplex — natural cursive handwriting),
**serif** (Times Roman — formal, the stroke cousin of collab's Garamond),
**sans** (Simplex plotter). The notebook default is `$NOTEBOOK_FONT`
(`serif` unless you change it in `scripts/takeover.sh`); pi picks per
element with `font-family`.

Placement is **measured, not guessed**: each pause message includes the
page's ink rows, free bands, and a font-size matched to the user's
handwriting height (`Page::ink_bands` in `ink.rs`), and the system prompt
tells pi to trust those numbers over reading the image. The
pause→respond policy lives in the system prompt too (`pi_rpc.rs`): default
is `pass`; draw only when addressed, asked, or correcting something.

```
 pen (raw evdev: pressure, rubber)      touch (flips)      power (sleep)
   │ strokes → page model (ink.rs, vector source of truth)
   ▼
 notebook ── pause (2.8s) → snapshot PNG ──► pi --mode rpc (resident child)
   │  ▲                                          │ JSONL stdin/stdout
   │  └── unix socket $NOTEBOOK_SOCK ◄── notebook_draw/erase/view tools
   ▼        (ipc.rs: draw=svg→strokes→animate, erase=re-render region)
 display: rm2fb takeover — DU for your ink, GL16 for pi's gray ink,
          GC16 flash on page turns (doubles as deghost)
```

## Gestures

| Do this | And |
|---------|-----|
| Write anywhere | It's a page. Pausing ~3s offers the page to pi |
| Flip the marker | Erase (whole strokes — yours or pi's) |
| Swipe left / right with a finger | Next / previous page (past the last page = new page) |
| Tap the top-left corner (pen or finger) | Toggle the sidebar: first/last/active page, go-to-page number pad, INSTRUCTIONS (AGENT.md), LIBRARY, pi text size [-]/[+], refresh |
| Sidebar → LIBRARY | Browse pi's saved material (`~/.local/share/notebook/library/*.md`): tap an item to read it, swipe left/right to page through, swipe right at the start to go back |
| Swipe right on page 1 | The INSTRUCTIONS page (AGENT.md as text): write feedback on it, pause — pi rewrites the file; swipe left to return |
| Swipe down from the top edge, tap CLOSE | Exit to xochitl |
| Power button | Sleep page + real suspend; wake resumes in place (WiFi re-healed) |

A small gray dot top-right = pi is looking at the page / drawing. A `3 / 7`
box flashes after each flip.

## Build & deploy

Prereqs are collab's: xovi + AppLoad (`../pi/pi-appload/install.sh`), pi on
the tablet (`../pi/pi-harness/install.sh`), WiFi on.

```sh
rustup target add armv7-unknown-linux-musleabihf   # once
make fetch-server    # once: rm2fb server -> vendor/ (or copies collab's)
make deploy          # build + push (HOST=root@<ip>)
```

Tablet: AppLoad menu → **notebook**. Same takeover warning as collab: it
stops xochitl and runs as root; the launcher always restarts xochitl on
exit, but keep SSH working — `make restore-ui` is the escape hatch.

Pages persist as JSON (strokes, x10 int coords) in
`~/.local/share/notebook/pages/`, pi's session in
`~/.local/share/notebook/sessions/` (`--continue` across restarts: the
notebook is one long conversation).

## Memory: AGENT.md

`~/.local/share/notebook/AGENT.md` on the tablet is the agent's standing
instructions, prepended to its system prompt at every launch — and **pi
maintains it itself**: write feedback in the notebook ("pi: always answer
in script font", "pi: stop commenting on my todo lists") and it updates
the file with its shell tools and applies it immediately. Ask it to "show
your instructions" and it draws a summary. You can also edit the file over
SSH; `$NOTEBOOK_AGENT_MD` overrides the path.

## Logs & traces

`make trace HOST=root@<ip>` pulls the device log AND pi's session files,
and renders one HTML (build/trace.html): the app log up top, then every
pause — the exact page image pi saw, its thinking, per-turn tokens/cost,
PASS badges, and every notebook_draw overlaid in red on the page it was
drawn against. This is the debugging view for "why did it write there?".
Costs and the optimization plan live in [cost.md](cost.md).

## Preview without the tablet

```sh
make preview    # docker + qemu, fake pi -> build/preview*.png
```

The fake pi (test/fake-pi.py) exercises the real tool socket: draws a
circle patch + a text/curve/arrow patch, views the page, then erases the
circle mid-animation. Screenshots: `-thinking` (working dot), the response,
`-page2` (flip forward), `-back` (flip back = full re-render from disk).

## Notes / current choices

- **Your ink is black (DU, ~8ms); pi's is gray (GL16, flash-free)** — the
  waveform choice is per update, and any region containing gray re-renders
  with GL16 so it never dithers to noise.
- The pause trigger only fires after a real change (stroke or erase), never
  mid-contact, and pi's animation holds while your pen is on the glass.
- `pass` replies and all of pi's prose go to `/tmp/notebook.log` only —
  the page is the sole UI.
- Erasing with the rubber removes whole strokes (stroke-level, like
  xochitl's stroke eraser), including pi's — if that guts a patch, the
  patch is gone for pi too (it's told bboxes each pause).
- Not yet: pinch-zoom, stroke-through selection, exporting pages to
  xochitl/PDF, a gray nib for the user, pi drawing on non-visible pages
  during long absences (the plumbing — `page` params — already exists).
