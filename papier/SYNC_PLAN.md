# papier bidirectional web sync — plan

Status: **built and deployed.** One later revision (2026-07-10): the
periodic timer described below was retired for battery life — sync is now
event-driven (Papier's debounced on-edit sync, push via `rm-sync-flush.sh`
before suspend, pull via `rm-sync-wake.sh` after resume), and
`papier-sync.sh` grew `pull|push|both` modes.

Second revision (2026-07-13), three additions to the web viewer:

- **PDF.js reading path.** Books whose source PDF is retained on the VM
  (`hasSource` in the library manifest) are rendered in the browser from
  the source PDF itself — ONE immutable download per document
  (`/papier/api/source-pdf?id=&v=`) instead of a ~150KB PNG + ink JSON round
  trip per page, at DPR-crisp vector quality. Page placement replicates
  mkbook.py's crop/margins math exactly so the ink overlay stays aligned
  with the tablet raster; raster PNGs remain the fallback (notebooks,
  desk-rendered books, PDF.js load failure).
- **Latency**: bigger IntersectionObserver lookahead (2400px raster /
  3600px vector) and the doc's ink JSONs (listed in the manifest) are
  prefetched at open.
- **✦ Compose** (agentic doc creation): a header button posts
  instructions/links to `POST /papier/api/compose`; `papier-compose.sh`
  runs a headless `pi` agent on the VM (research → one teaching article →
  `notes-md2pdf.sh` typesetting, the Clippings enrich style minus the
  reference appendix/quiz), then the PDF takes the exact upload path
  (book bundle → inbound → tablet, source retained). Job phase is polled
  via `GET /papier/api/compose-status?job=` and survives page reloads
  (localStorage) and service restarts (result.json). Jobs run a per-job
  COPY of the script (deploys can't corrupt in-flight runs) and user
  instructions never pass through shell expansion. After writing, a
  review loop checks the article with pandoc's MathML converter
  (unsupported macros like `\colorbox` would print as raw TeX) plus a
  missing-image check, and sends the agent back to repair the file — up
  to two fix rounds.
- **Full PDF.js viewer app** (self-hosted at `/papier/pdfjs/`, pinned
  4.8.69, installed by deploy-server.sh): "PDF viewer" in the doc
  sidebar/docbar opens Mozilla's complete viewer (zoom, smooth scroll,
  text search/selection, thumbnails, outline) in an overlay iframe on
  the same cached `source-pdf` URL. Ink annotations stay in the inline
  reader; the app shows the clean PDF.

## Model: mirror out, drop-to-add in

Sync is asymmetric, which is what makes it conflict-free:

- **Outbound (tablet → web): full mirror.** `~/.local/share/papier/`
  rsyncs to the VM every few minutes. The web is always a faithful
  read-view of the tablet. ("Edit in papier → shows on the web.")
- **Inbound (web → tablet): drop-to-add.** A dropped file becomes a NEW
  document (fresh id), so it never collides with anything being edited on
  the tablet. ("Drag-drop on the web → appears in papier.")

Editing happens on the tablet; the web is a viewer + a drop target.

Decisions (from review): dropped PDFs render into books **on the VM**;
the web viewer is **read + drop-to-add now**, architected so **browser
ink-editing can be added later** (a write-back endpoint + per-file
last-writer-wins; the data format is already web-writable).

## Repo layout (new)

Mirror notebook's `sync/` structure inside papier:

```
papier/sync/
  README.md  ARCHITECTURE.md
  deploy/  deploy-tablet.sh  deploy-server.sh
  tablet/
    bin/papier-sync.sh                 push mirror + inbound pull
    systemd/papier-sync.{service,timer}
  server/                              runs on remarkable.exe.xyz
    bin/papier-upload.js               inbound upload + PDF render trigger
    bin/papier-render.sh               mkbook wrapper (pymupdf) for dropped PDFs
    nginx/papier.conf                  root viewer + /papier/data + /papier/upload
    systemd/papier-upload.service
    web/index.html                     the viewer SPA (fork of notebook's)
```

## The five pieces

### 1. Outbound mirror  *(reuse)*
`tablet/bin/papier-sync.sh` runs the same rsync notebook uses, second
target:
```
rsync -az --delete --stats --omit-dir-times --no-perms --no-owner \
  --no-group --exclude '*.tmp' -e "ssh -y -i $KEY" \
  /home/root/.local/share/papier/ \
  exedev@remarkable.exe.xyz:/home/exedev/remarkable-backup/papier/
```
Reuses the existing `id_sync_dropbear_ed25519` key. `--exclude '*.tmp'`
because papier saves tmp-then-rename. Runs on a 3-min timer (its own unit,
independent of notebook's).

### 2. Web viewer at `/papier/`  *(fork notebook's viewer)*
`remarkable.exe.xyz/papier/` serves the papier SPA:
- **Home:** one server-local `/papier/api/library` request merges the mirror +
  inbound trees and returns metadata, state sequence, versions, covers and
  existing ink paths. Stale/half-consumed directories are ignored locally on
  the VM instead of becoming long-distance 404s.
- **Covers:** the service caches a compact WebP derivative of the tablet's
  existing `thumb.png` (first raster page fallback for a fresh web upload).
- **Open a doc:** notebook pages render as vector canvas (user black, pi
  blue — same renderer/format as notebook's viewer); book pages render
  the pre-rendered `pages/NNNN.png` raster with the ink overlay on top.
- The visible home view conditionally checks the ETagged manifest every 60s;
  polling stops while reading or while the tab is hidden. Page raster + ink
  load concurrently, nonexistent ink is never requested, and versioned assets
  are immutable in the browser.
- `/notes/`, `/notebook/` and `/updates/` are untouched.

### 3. Inbound drop + VM PDF render  *(net-new)*
- A drop-zone on the page POSTs the file to `POST /papier/upload`.
- `papier-upload.js` (small node service, systemd unit) writes uploads to
  `~/remarkable-backup/papier-inbound/incoming/` and, by type:
  - `.pdf` → `papier-render.sh` runs `mkbook.py` (pymupdf, installed once
    on the VM) → a book bundle in `.../papier-inbound/docs/<slug>/`.
  - a `.tar`/`.zip` of a `docs/<id>/` tree → unpacked into inbound docs/.
  - (later: image → single-page book.)
- nginx adds `location /papier/` (viewer), `/papier/data/` + `/papier/inbound/`
  (static document assets), `/papier/api/` (manifest/covers/crop jobs), and
  `location /papier/upload`
  (proxy to the node service).

### 4. Tablet pull  *(net-new)*
`papier-sync.sh`, BEFORE the push: reverse-rsync
`.../papier-inbound/docs/ → /home/root/.local/share/papier/docs/` (no
`--delete`; add-only), then move consumed inbound docs to a `processed/`
area on the VM. Because inbound only ever adds fresh ids, the pull never
overwrites an open doc.

### 5. App live re-read  *(net-new, app-side)*
- On the **home grid**: rescan `docs/` on a ~15s timer (only when no doc
  is open and no dialog is up); rebuild the grid if the id set changed.
  Cheap — reads meta.json only, reuses the lazy-thumbnail queue for any
  new docs.
- While a **doc is open**: untouched (tablet is authoritative; it mirrors
  out). A pulled doc only matters on the home grid.
- Editable-later hook: none needed app-side now; the write path would be a
  new inbound type "doc mutation" that the pull applies when the doc is
  closed.

## Build milestones (each preview-verifiable before deploy)

- **S1 — Outbound + read viewer.** `papier-sync.sh` push, nginx config,
  the viewer SPA. Verify locally: point the viewer at a copy of a real
  `docs/` tree, confirm the grid + notebook-vector + book-raster render.
- **S2 — Inbound.** upload service + `papier-render.sh`, the web drop-zone,
  the tablet pull leg. Verify locally: POST a PDF to the service, confirm
  a bundle appears; simulate the pull into a docs/ dir.
- **S3 — App rescan.** the ~15s home-grid rescan; preview-verify a doc
  dropped into the data dir appears on home without a restart.
- **S4 — Deploy** (only on your word): `deploy-tablet.sh` +
  `deploy-server.sh`, one-time `pip install pymupdf` on the VM, register
  nothing new (reuse the existing key), flip the root block.

## Risks / notes
- Root takeover is one nginx `location = /` edit; `/notebook/` stays.
  Reversible by restoring the old block.
- The VM gains pymupdf + a node upload service (small, `Restart=always`).
- Inbound is add-only ⇒ dropping a doc that shares an id with an existing
  one is de-collided with an id suffix (never overwrites).
- The whole outbound half is proven (notebook runs it today); the net-new
  surface is the inbound leg + the app rescan.
