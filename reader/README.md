# reader — a PDF reader with a resident reading companion (reMarkable 2)

Books are PDFs. You flip pages with a finger, scribble on them with the
pen, erase with the rubber end. When you pause, the page — printed text,
your ink, everything — goes to a background [pi](https://github.com/earendil-works/pi-coding-agent)
agent that reads along and may quietly mark the book:

- **underline a printed phrase** — resolved against the page's real word
  geometry (`reader_underline`), so the line lands exactly under the words;
- **write short margin notes** in single-stroke plotter ink, sized and
  placed from *measured* margins, animated in like a ghost hand;
- **insert a blank note page** after the current page for anything longer
  (`reader_insert_note`), and write there with the full canvas;
- **read any page as text** (`reader_page_text`) — the bundle carries the
  PDF's true extracted text, so it genuinely reads with you;
- **erase its own patches** (`reader_erase`) and **turn pages**
  (`reader_goto`, refused while you're writing).

Most pauses it replies `pass` — the default is silence.

## How PDFs get in

**On the tablet itself (no computer):** the home screen's `IMPORT +`
button lists every PDF in the stock app's library — tap one and the
tablet renders it in the background (~0.5 s/page) with a bundled `mutool`
(MuPDF, the same engine the desk pipeline uses; one-time
`make fetch-mutool deploy-mutool`). Better yet, make a folder called
**`Reader`** in the stock app: anything you drop there — from the phone
app, the browser extension, the desktop app — auto-imports the next time
the reader checks (at launch and every few minutes). That's the sync
story: reMarkable's own cloud carries the file to the tablet, the tablet
does the rest. `reader --import-cli <name>` runs the same pipeline
headless over ssh.

**From your computer** (faster for bulk, and the only way to get custom
margins today):

The tablet never parses a PDF. `make book FILE=paper.pdf HOST=root@<ip>`
runs `tools/mkbook.py` on your computer (via `uv`, pymupdf + numpy): every
page is rendered to a 1404x1872 raster, **dithered to pure black/white**
(the pen's 1-bit DU waveform stays instant everywhere), and the text +
word boxes are extracted into JSON, all in device pixel coordinates. The
bundle is pushed over ssh; the app decodes the PNGs with its own
dependency-free decoder (`src/png_dec.rs`).

    make book FILE=~/papers/attention.pdf HOST=root@192.168.1.3
    make books HOST=...          # list
    make book-rm SLUG=... HOST=...   # delete (incl. ink + position)

Your strokes and pi's patches live in a vector ink overlay per page
(`ink/` inside the bundle), composited darkest-wins over the raster —
erasing never touches the print. Inserted note pages are entries in the
book's reading sequence (`state.json`); re-pushing an updated PDF keeps
your ink, notes and reading position.

## Driving it

- **home screen**: tap a book. `ALL BOOKS` in the sidebar returns there.
- **sidebar** (tap the top-left corner): first/last/active page, GO TO
  numpad, `+ NOTE PAGE HERE`, INSTRUCTIONS (the AGENT.md pi maintains from
  your handwritten feedback), pi text zoom, refresh.
- **exit**: swipe down from the top edge → CLOSE. Power button sleeps.

## Build / dev

    make                # cross-compile (armv7 musl, rust-lld)
    make preview        # no tablet: qemu + fake pi -> build/preview*.png
    make test-host      # PNG decoder round-trip vs python zlib
    make fetch-server   # rm2fb server (timower/rM2-stuff) into vendor/
    make deploy HOST=root@<ip>
    make trace HOST=... # pi session traces + device log -> build/trace.html
    make log HOST=...

Escape hatch if the panel wedges: `make restore-ui HOST=...`
(= `systemctl start xochitl`).

Shared DNA with `../notebook` (the freeform notebook variant): display
stack, input, Hershey ink, SVG parsing, LaTeX-lite math, pi RPC.
