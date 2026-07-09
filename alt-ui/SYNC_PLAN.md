# alt-ui bidirectional web sync — plan

Status: **awaiting go-ahead to build.** Nothing here touches the VM or
tablet until explicitly deployed.

## Model: mirror out, drop-to-add in

Sync is asymmetric, which is what makes it conflict-free:

- **Outbound (tablet → web): full mirror.** `~/.local/share/alt-ui/`
  rsyncs to the VM every few minutes. The web is always a faithful
  read-view of the tablet. ("Edit in alt-ui → shows on the web.")
- **Inbound (web → tablet): drop-to-add.** A dropped file becomes a NEW
  document (fresh id), so it never collides with anything being edited on
  the tablet. ("Drag-drop on the web → appears in alt-ui.")

Editing happens on the tablet; the web is a viewer + a drop target.

Decisions (from review): dropped PDFs render into books **on the VM**;
the web viewer is **read + drop-to-add now**, architected so **browser
ink-editing can be added later** (a write-back endpoint + per-file
last-writer-wins; the data format is already web-writable).

## Repo layout (new)

Mirror notebook's `sync/` structure inside alt-ui:

```
alt-ui/sync/
  README.md  ARCHITECTURE.md
  deploy/  deploy-tablet.sh  deploy-server.sh
  tablet/
    bin/alt-ui-sync.sh                 push mirror + inbound pull
    systemd/alt-ui-sync.{service,timer}
  server/                              runs on remarkable.exe.xyz
    bin/alt-ui-upload.js               inbound upload + PDF render trigger
    bin/alt-ui-render.sh               mkbook wrapper (pymupdf) for dropped PDFs
    nginx/alt-ui.conf                  root viewer + /alt-ui/data + /alt-ui/upload
    systemd/alt-ui-upload.service
    web/index.html                     the viewer SPA (fork of notebook's)
```

## The five pieces

### 1. Outbound mirror  *(reuse)*
`tablet/bin/alt-ui-sync.sh` runs the same rsync notebook uses, second
target:
```
rsync -az --delete --stats --omit-dir-times --no-perms --no-owner \
  --no-group --exclude '*.tmp' -e "ssh -y -i $KEY" \
  /home/root/.local/share/alt-ui/ \
  exedev@remarkable.exe.xyz:/home/exedev/remarkable-backup/alt-ui/
```
Reuses the existing `id_sync_dropbear_ed25519` key. `--exclude '*.tmp'`
because alt-ui saves tmp-then-rename. Runs on a 3-min timer (its own unit,
independent of notebook's).

### 2. Web viewer at the root  *(fork notebook's viewer)*
`remarkable.exe.xyz/` serves the alt-ui SPA:
- **Home:** doc grid from nginx's JSON autoindex of `.../alt-ui/docs/`,
  showing folders + notebooks + books (reads each `meta.json`; folders
  from the `folder` field, matching the tablet).
- **Open a doc:** notebook pages render as vector canvas (user black, pi
  blue — same renderer/format as notebook's viewer); book pages render
  the pre-rendered `pages/NNNN.png` raster with the ink overlay on top.
- Polls 20s, refetches only mtime-changed files. Read-only for now; a
  hidden write path is stubbed for later editing.
- `/notebook/` and `/updates/` are untouched; only root's `302` changes.

### 3. Inbound drop + VM PDF render  *(net-new)*
- A drop-zone on the page POSTs the file to `POST /alt-ui/upload`.
- `alt-ui-upload.js` (small node service, systemd unit) writes uploads to
  `~/remarkable-backup/alt-ui-inbound/incoming/` and, by type:
  - `.pdf` → `alt-ui-render.sh` runs `mkbook.py` (pymupdf, installed once
    on the VM) → a book bundle in `.../alt-ui-inbound/docs/<slug>/`.
  - a `.tar`/`.zip` of a `docs/<id>/` tree → unpacked into inbound docs/.
  - (later: image → single-page book.)
- nginx adds `location = /` (viewer), `location /alt-ui/data/`
  (JSON autoindex — the viewer's API), `location /alt-ui/upload`
  (proxy to the node service).

### 4. Tablet pull  *(net-new)*
`alt-ui-sync.sh`, BEFORE the push: reverse-rsync
`.../alt-ui-inbound/docs/ → /home/root/.local/share/alt-ui/docs/` (no
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

- **S1 — Outbound + read viewer.** `alt-ui-sync.sh` push, nginx config,
  the viewer SPA. Verify locally: point the viewer at a copy of a real
  `docs/` tree, confirm the grid + notebook-vector + book-raster render.
- **S2 — Inbound.** upload service + `alt-ui-render.sh`, the web drop-zone,
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
