# Papier

A pixel-faithful replication of the stock reMarkable UI, with the `pi` AI
agent integrated — one unified takeover app that supersedes the separate
`reader` and `notebook` siblings.

(The source tree, on-device data dir, and web-sync paths are still named
`papier` — only the app's identity is `Papier`; the storage keys are kept so
existing books and the sync keep working.)

A xochitl-like **home grid** opens two kinds of documents:

- **notebooks** — blank vector pages; pi is a co-writer that answers in
  the flow of your handwriting.
- **books** — PDFs pre-rendered on the desk side (`make book`); pi is a
  margin companion that underlines, writes short margin notes, and inserts
  note pages, never touching the printed text.

On top of the canvas: a **top status bar** (battery, wifi, clock), a
**right-edge toolbar** (pen / eraser / lasso / undo / redo / page nav /
home), **lasso selection** with move + delete/cut, and full **undo/redo**.

Built on the takeover stack from `reader`/`notebook`/`collab`: it stops
xochitl, hosts the panel with a vendored rm2fb server, reads raw
Wacom/touch/power input, and drives `pi --mode rpc` over a unix socket.

## Build & run

```
rustup target add armv7-unknown-linux-musleabihf   # one time
make                # cross-compile (rust-lld, no docker)
make preview        # run under qemu with a fake pi -> build/preview*.png
make test-host      # host-target unit tests (the PNG decoder)
make deploy HOST=root@<ip>    # push to the tablet (needs make fetch-server once)
```

`make preview SCENARIO=<m0..m5-nb>` drives one scripted session:

| scenario  | exercises |
|-----------|-----------|
| `m0`      | empty home, top-bar reveal, clean CLOSE exit |
| `m1`      | unified Doc model: book + notebook ink, flips, quick-sheets growth, persistence across restarts |
| `m2`      | home grid, status bar, long-press menu, rename/move/folders, legacy import |
| `m3`      | toolbar, tools, undo/redo, erase-undo, stroke-id persistence, numpad go-to |
| `m4`      | lasso select (user + AI ink), drag-move, delete/cut, undo |
| `m5-book` | pi margin companion: underline + margin note + inserted note page |
| `m5-nb`   | pi co-writer + pause suppression while a selection is active |

The preview harness runs an **arm64** container (native python + a single
qemu layer for the armv7 binary); an amd64 container on an Apple-silicon
host nests emulation and breaks MAP_SHARED framebuffer coherency.

## Documents

Books are rendered on the Mac and pushed:

```
make book FILE=~/papers/x.pdf [TITLE="..."] [MARGIN=90] HOST=root@<ip>
make mirror ONLY="frag"      # import the tablet's own xochitl PDFs
```

On-disk under `~/.local/share/papier/`: `docs/<id>/` per document (byte-
compatible with reader bundles), `folders.json`, `settings.json`. Folders
are a `meta.json` field, not subdirectories.

## Web editor

`https://remarkable.exe.xyz/papier/` reads the tablet mirror and pending web
uploads through one server-local `/papier/api/library` manifest. The manifest
includes document metadata, sequence state, content versions, cover URLs and
the existing ink-file set, so the high-latency browser path does not fan out
into per-document metadata/state requests or missing-ink 404s.

The web is **writable**, not just a read view — the same cloud write path the
iPad app uses:

- **Tool rail** (right edge, the device toolbar's controls in the device's
  order): pen, type, eraser (tap again to cycle object/pixel), lasso,
  read-only pointer, touch-draw toggle, undo/redo, page nav + go-to, add
  page. Shortcuts: `p` `t` `e` `l` `r`, `[`/`]` (text size), `⌘Z`/`⇧⌘Z`,
  `⌘S`, `⌘B` (sidebar), `⌫` (delete the lasso selection), `Esc`.
- **Typing** (the keyboard the tablet doesn't have): the text tool puts a
  caret on the page — click, type, `Enter` commits, `Esc` discards, and
  clicking an existing run reopens it. Runs land in the page file's
  top-level `texts` (the USER's typeset layer, black; pi's runs stay inside
  its patches) and behave like any other ink: the eraser rubs them glyph by
  glyph, the lasso moves and deletes them, undo covers them.
- **Drawing** writes the libreink page-ink JSON the tablet writes, with the
  iPad's pressure calibration (1.7–3.1 page units), so a mark made in the
  browser has the weight of one made on the device. The eraser rubs out the
  user's ink *and* pi's — patch strokes by stroke, typeset Garamond runs
  glyph by glyph — and the lasso moves or deletes whatever falls entirely
  inside the loop.
- **Autosave** 2.5s after the pen stops (the tablet's own pause beat), plus
  an explicit *Save now*, a retry loop on failure, a flush when the tab is
  hidden or the document is closed, and a `sendBeacon` on unload.
- **Document actions** in the left sidebar and on each home card: rename /
  move to a folder, add a page, delete, and *New notebook* in the header.
- **Ownership split** (identical to the iPad's): the browser owns the user's
  strokes and POSTs the whole page to `/papier/api/ink`; pi's patches belong
  to the server, so erasing or moving them goes through `/patch-replace`,
  `/patch-erase` and `/patch-move`.

Every write lands in the VM's *inbound* tree, never the mirror — the tablet's
push runs `--delete`, so the mirror is its to own. The tablet's next pull
folds the writes in per-file (last-writer-wins). A **delete** can't be a
mirror deletion for the same reason: `/papier/api/doc-delete` drops a
tombstone under `papier-inbound/tombstones/<id>.json`, which hides the
document from the library immediately and makes `papier-sync.sh` remove it
from the tablet on its next pull.

### The typed-text layer needs one change in `libreink`

`texts` is newer than **`libreink-page`**, the crate that owns the `Page`
model — which lives in the `libreink` repo, not here. Until it carries the
field:

- the **tablet** renders strokes and pi's patches on such a page but not
  the typed runs, and a page it re-saves (you edit it on the device) loses
  them;
- **cloud pi** would lose them the same way, since `papier-cloud-canvas`
  loads and re-saves the whole file through that model — so
  `papier-pi-sessions.js` snapshots the runs around every mutating canvas
  call and puts them back (`typed-text-safety.test.js` pins that, with a
  fake canvas that drops the field exactly as the Rust does today). The
  restore becomes a no-op once the crate knows the field.

What `libreink-page` needs: a `texts: Vec<TextRun>` on `Page` beside
`strokes`, parsed from and serialised to the top-level `texts` key (omit
when empty, so existing files are byte-identical), drawn in the page render
in `USER_GRAY` rather than `AI_GRAY`, and included in the eraser's and
lasso's hit-testing the way patch runs already are. Web and iPad round-trip
the field today, so the tablet is the only surface still blind to it.

Home covers are cached 280×373 WebP derivatives of each document's existing
`thumb.png` (or first raster page before the tablet has generated a thumb).
Versioned covers, page PNGs and ink JSON are immutable in the browser. The
visible home view checks the ETagged manifest every 60 seconds; polling stops
while a document is open or the tab is hidden. Full-book crop renders run as
background jobs and report page progress to the editor.

## Module map (`src/`)

Shared display/input core (from collab, verbatim): `fb draw display qtfb
rm2fb pen touch power font png png_dec svg_ink hershey*`. App-specific:
`ink` (vector page + stroke ids + selection ops), `doc` (unified
Book/Notebook), `store` (scan/folders/import), `home` + `thumbs`,
`statusbar`, `toolbar` + `icons`, `select` (lasso), `undo`, `kb`,
`pi_rpc` + `ipc` (pi), `main`.

## Pixel-fidelity tooling (`make fonts` / `make ref`)

`tools/xsnap.py` grabs reference screenshots of the real xochitl UI,
`tools/extract-fonts.py` carves the `reMarkableSans` faces out of
`/usr/bin/xochitl`, `tools/icon-gen.py` turns cropped glyphs into 1-bit
`Icon` tables. These feed a fidelity pass that swaps the placeholder
polyline icons for exact bitmaps. The on-device framebuffer grab is
firmware-specific — pass `--addr` once the fb region is known.
