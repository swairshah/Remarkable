# reMarkable Sync + Export + Viewer

End-to-end pipeline for: reMarkable tablet → server backup → JPG/PDF export +
LLM activity digest → web viewer, all at `https://remarkable.exe.xyz/`
(digest at `/updates/`, page viewer at `/raw/`).

Everything needed to rebuild this pipeline from scratch lives in this directory.
For the full architecture walkthrough see [ARCHITECTURE.md](ARCHITECTURE.md).

## System diagram

```
 ┌──────────────────────────────── reMarkable device ───────────────────────────────┐
 │                                                                                   │
 │   /home/root/.local/share/remarkable/xochitl/      (raw notebook state)           │
 │                         │                                                         │
 │                         │  on sleep-flush / wake, 30-min timer backstop           │
 │                         ▼                                                         │
 │   tablet/bin/remarkable-push-sync.sh                                              │
 │     · rsync --delete to exedev@remarkable.exe.xyz:~/remarkable-backup/xochitl/    │
 │     · ssh exedev@remarkable.exe.xyz ~/bin/remarkable-post-sync.sh auto Notebook   │
 │                                                                                   │
 │   Triggered by (battery-first, event-driven):                                     │
 │     apps' sleep flush        tablet/bin/rm-sync-flush.sh  (push before suspend)   │
 │     apps' wake heal          tablet/bin/rm-sync-wake.sh   (pull after resume)     │
 │     backstop timer           remarkable-push-sync.timer   (OnUnitActiveSec=30min, │
 │                              exits network-free when nothing changed)             │
 │                                                                                   │
 └───────────────────────────────────┬───────────────────────────────────────────────┘
                                     │  rsync + ssh trigger
                                     │  (tablet key registered with exe.dev account)
                                     ▼
 ┌──────────────────────── remarkable.exe.xyz (exe.dev VM) ─────────────────────────┐
 │                                                                                   │
 │   ~/remarkable-backup/xochitl/           (raw sync destination)                   │
 │                 │                                                                 │
 │                 ▼                                                                 │
 │   server/bin/remarkable-post-sync.sh auto Notebook                                │
 │     · scans *.metadata for visibleName == "Notebook"                              │
 │     · picks highest lastModified → UUID                                           │
 │     · copies UUID.thumbnails/*.png → pages_png/NNN-<pid>.png                      │
 │     · convert PNG → JPG at quality 92                                             │
 │     · img2pdf PNG → single PDF                                                    │
 │     · runs the activity agent (LLM digest)                                        │
 │                 │                                                                 │
 │                 ▼                                                                 │
 │   ~/remarkable-exports/Notebook/         (pages_png/, pages_jpg/, Notebook.pdf)   │
 │   ~/notes/updates/index.html             (LLM activity digest page)               │
 │                 │                                                                 │
 │                 ▼                                                                 │
 │   nginx (native, listens on :8000)                                                │
 │     location = /          -> 302 /updates/                                        │
 │     location /updates/    -> ~/notes/updates/  (activity digest)                  │
 │     location /raw/        -> viewer html                                          │
 │     location /raw/pages/  -> pages_jpg/  (autoindex_format json)                  │
 │     location /notebook/       -> notebook-app live viewer html                    │
 │     location /notebook/data/  -> ~/remarkable-backup/notebook-app/ (json index)   │
 │                                                                                   │
 └───────────────────────────────────┬───────────────────────────────────────────────┘
                                     │ exe.dev proxy: TLS termination,
                                     │ https://remarkable.exe.xyz → VM port 8000
                                     ▼
                        https://remarkable.exe.xyz/updates/   (public)
```

## Layout

```
sync/
├── README.md                                this file
├── ARCHITECTURE.md                          architecture deep-dive
├── deploy/
│   ├── deploy-tablet.sh                     push tablet files + reload timer
│   └── deploy-server.sh                     build agent bundle, push server files, reload nginx
├── tablet/                                  ── runs on the reMarkable ──
│   ├── bin/
│   │   ├── remarkable-push-sync.sh          rsync + remote export trigger
│   │   └── remarkable-notes-pull.sh         pull notes PDFs, import into stock app
│   └── systemd/
│       ├── remarkable-push-sync.service
│       └── remarkable-push-sync.timer       cadence (OnUnitActiveSec=)
└── server/                                  ── runs on remarkable.exe.xyz ──
    ├── bin/
    │   ├── remarkable-post-sync-by-name.sh    pick latest doc by visibleName, export
    │   ├── remarkable-post-sync.sh            export + activity agent hook (active)
    │   ├── remarkable-activity-agent.ts       LLM digest agent (bundled to .js at deploy)
    │   ├── remarkable-activity-agent-hook.sh  hook entrypoint (supports -p prompt)
    │   ├── notes-md2pdf.sh                    markdown → reMarkable-ready PDF (pandoc+chrome)
    │   └── notes-pdf-export.sh                render changed notes posts + manifest
    ├── nginx/
    │   └── default.conf                     nginx site (installed to sites-available)
    └── web/
        ├── nav.js                           shared site nav (nginx-injected into every HTML page)
        ├── raw/
        │   └── index.html                   swipe/sidebar viewer
        └── notebook/
            └── index.html                   notebook-app live viewer (vector pages, library, AGENT.md)
```

The agent's runtime `remarkable-activity-agent.js` is **not committed** — 
`deploy-server.sh` builds it from the `.ts` with bun at deploy time.

## Shelley (agentic layer)

Beyond the fixed pipeline, syncs that publish new changes also ping
**Shelley** — the exe.dev agent resident on the VM — through the exe.dev
HTTPS API (token in `~/.env`, scoped to the `shelley prompt` subcommand,
30-min cooldown). Its standing instructions (`server/shelley/AGENTS.md`, deployed
to `~/.config/shelley/AGENTS.md`) tell it to read the machine-readable diff
feed (`diffs.jsonl` + `changed-pages/` images), keep a journal at
`~/remarkable-journal.md`, and maintain a daily post with practice exercises
at https://remarkable.exe.xyz/notes/. See ARCHITECTURE.md for details.

## Notes → PDF → back to the tablet

Shelley's daily posts round-trip to the device as typeset PDFs:

1. Shelley writes each post twice: `index.html` (web) and `index.md`
   (pandoc markdown twin, per `server/shelley/AGENTS.md`).
2. `notes-pdf-export.sh` (end of every post-sync) renders changed
   `index.md` files with `notes-md2pdf.sh` — a port of the local Clippings
   `md2pdf.sh --rm2` preset (157×210mm, Reader/EB Garamond + Google Sans
   Code, native MathML, overflow auto-shrink) — into
   `~/remarkable-exports/notes-pdf/YYYY-MM-DD.pdf` + `manifest.sha256`.
3. On the tablet, `remarkable-notes-pull.sh` (wake path via
   `rm-sync-wake.sh`, plus an hourly `remarkable-notes-pull.timer`
   backstop for stock-app-only days; self-gated to one network pull per
   day) rsync-pulls that directory and imports **finalized posts only**
   (dated before today) into the stock app under a `notes` folder,
   as "Notes YYYY-MM-DD". xochitl is restarted only when something new
   was imported, and never before 5 AM (`IMPORT_AFTER_HOUR`).

Net effect: each morning, yesterday's exercises appear in `notes/` on the
device; the document never changes afterward, so on-tablet annotations
stay aligned.

## Where to make a change

| I want to change... | Edit | Deploy |
| --- | --- | --- |
| How often the tablet pushes | `tablet/systemd/remarkable-push-sync.timer` | `deploy/deploy-tablet.sh` |
| What/how the tablet rsyncs | `tablet/bin/remarkable-push-sync.sh` | `deploy/deploy-tablet.sh` |
| Which doc gets exported (default `Notebook`) | `server/bin/remarkable-post-sync-by-name.sh` (`DOC_NAME=...`) | `deploy/deploy-server.sh` |
| Export format (JPG quality, PDF, etc.) | `server/bin/remarkable-post-sync-by-name.sh` | `deploy/deploy-server.sh` |
| URL routing under `remarkable.exe.xyz` | `server/nginx/default.conf` | `deploy/deploy-server.sh` (auto-reloads nginx) |
| Viewer look/feel, shortcuts, sidebar | `server/web/raw/index.html` | `deploy/deploy-server.sh` |
| Notebook-app live viewer (`/notebook/`) | `server/web/notebook/index.html` | `deploy/deploy-server.sh` |
| Papier web viewer (`/papier/`) | `../papier/sync/server/web/index.html` | `deploy/deploy-server.sh` |
| Papier library/covers/upload/crop jobs | `../papier/sync/server/bin/papier-{upload,library}.js` | `deploy/deploy-server.sh` |
| Papier ✦ Compose agent (research → article → PDF) | `../papier/sync/server/bin/papier-compose.sh` (prompt/pipeline), job runner in `papier-upload.js` | `deploy/deploy-server.sh` |
| Digest page: UI, fonts, history, `--rerender` | `server/bin/remarkable-activity-agent.ts` | `deploy/deploy-server.sh` (builds the bundle) |
| Activity summary prompt/model/output | `server/bin/remarkable-activity-agent-hook.sh` (`-p`, `MODEL`, `OUTPUT_HTML`) | `deploy/deploy-server.sh` |
| Notes-PDF typography (page size, fonts, margins) | `server/bin/notes-md2pdf.sh` (`RM_*` env knobs) | `deploy/deploy-server.sh` |
| Notes-PDF import policy (folder name, quiet hours) | `tablet/bin/remarkable-notes-pull.sh` | `deploy/deploy-tablet.sh` |
| What Shelley puts in the markdown twin | `server/shelley/AGENTS.md` | `deploy/deploy-server.sh` |
| Site nav (links, styling, front-page redirect) | `server/web/nav.js` + `server/nginx/default.conf` | `deploy/deploy-server.sh` |

## Deploy

Both deploy scripts assume SSH is pre-configured:
- `ssh remarkable` reaches the tablet (LAN alias in `~/.ssh/config`; the IP
  changes occasionally — update `Hostname` there)
- `ssh exedev@remarkable.exe.xyz` reaches the VM (override with `SERVER_HOST=`)

`deploy-server.sh` additionally needs [bun](https://bun.sh) locally to build
the agent bundle.

From `sync/`:

```bash
# Deploy tablet-side changes (push script, systemd timer/service)
deploy/deploy-tablet.sh

# Deploy server-side changes (bundle build, scripts, nginx config, viewer)
# → installs missing deps (node, img2pdf, imagemagick), runs nginx -t + reload
deploy/deploy-server.sh

# Deploy + immediately trigger a manual export run so you can see the result
deploy/deploy-server.sh --run
```

Env overrides: `REMARKABLE_HOST=...`, `SERVER_HOST=user@host`, `DOC_NAME=Foo`.

## One-time setup

### Tablet

The deploy script handles `chmod`, `daemon-reload`, and `enable --now` on the
timer, so `deploy-tablet.sh` is also the install command.

### exe.dev VM

`deploy-server.sh` handles everything reproducible: bundle build, scripts,
deps (`nodejs`, `img2pdf`, `imagemagick` via apt), nginx site install + reload.
The pieces done once by hand:

```bash
# 1. Register the tablet's sync key with the exe.dev ACCOUNT (not the VM).
#    SSH to *.exe.xyz goes through exe.dev's front door, which authenticates
#    against account keys; unregistered keys get a registration banner that
#    corrupts rsync. Key name in the account: remarkable-tablet-sync
ssh remarkable 'cat /home/root/.ssh/id_sync_dropbear_ed25519.pub'
ssh exe.dev ssh-key add "<that public key> remarkable-tablet-sync"

# 2. Point the exe.dev HTTP proxy at nginx and make the site public.
#    (The proxy only targets ports 3000-9999, hence nginx on 8000.)
ssh exe.dev share port remarkable 8000
ssh exe.dev share set-public remarkable

# 3. (optional) OpenRouter key — only needed for openrouter/* models; the
#    default gpt-5.5 goes through the exe.dev LLM integration, no key needed
ssh exedev@remarkable.exe.xyz 'echo "OPENROUTER_API_KEY=..." >> ~/.env && chmod 600 ~/.env'
```

## Current defaults

- `DOC_NAME=Notebook`
- Source on reMarkable: `/home/root/.local/share/remarkable/xochitl/`
- Destination on server: `/home/exedev/remarkable-backup/xochitl/`
- Export output on server: `/home/exedev/remarkable-exports/`
- Activity page output: `/home/exedev/notes/updates/index.html`
- Activity state dir: `/home/exedev/remarkable-exports/activity-agent/`
- Digest URL: `https://remarkable.exe.xyz/updates/`
- Viewer URL: `https://remarkable.exe.xyz/raw/`
- Notebook-app live viewer: `https://remarkable.exe.xyz/notebook/`
  (mirror of the tablet's `/home/root/.local/share/notebook/` at
  `/home/exedev/remarkable-backup/notebook-app/`, same 5-min push)
- Fonts: body `Iowan Old Style` (Apple system serif w/ Palatino→Georgia fallback),
  code/mono `Google Sans Code` (Google Fonts)

## Test manually

```bash
# Force a push from the tablet
ssh remarkable /home/root/bin/remarkable-push-sync.sh

# Force an export on the server
ssh exedev@remarkable.exe.xyz '~/bin/remarkable-post-sync-by-name.sh Notebook'

# Re-render the digest page after design changes (no LLM call)
ssh exedev@remarkable.exe.xyz 'node ~/bin/remarkable-activity-agent.js --rerender'

# Render notes posts → PDFs on the server (only changed ones re-render)
ssh exedev@remarkable.exe.xyz '~/bin/notes-pdf-export.sh'

# Force the tablet to pull + import finalized notes PDFs now
ssh remarkable /home/root/bin/remarkable-notes-pull.sh

# Verify the digest + viewer
curl -sI https://remarkable.exe.xyz/updates/
curl -sI https://remarkable.exe.xyz/raw/
```

## Logs

- reMarkable sync log: `/home/root/.local/state/remarkable-sync/push.log`
- Server export log: `~/remarkable-exports/<DocName>/export.log`
- Activity agent log: `~/remarkable-exports/activity-agent/run.log`
- nginx logs: `sudo tail -f /var/log/nginx/access.log` on the VM
