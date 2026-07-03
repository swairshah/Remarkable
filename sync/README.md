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
 │                         │  every 5 min via systemd timer                          │
 │                         ▼                                                         │
 │   tablet/bin/remarkable-push-sync.sh                                              │
 │     · rsync --delete to exedev@remarkable.exe.xyz:~/remarkable-backup/xochitl/    │
 │     · ssh exedev@remarkable.exe.xyz ~/bin/remarkable-post-sync.sh auto Notebook   │
 │                                                                                   │
 │   Scheduled by:                                                                   │
 │     tablet/systemd/remarkable-push-sync.timer   (OnUnitActiveSec=5min)            │
 │     tablet/systemd/remarkable-push-sync.service (ExecStart=.../push-sync.sh)      │
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
│   │   └── remarkable-push-sync.sh          rsync + remote export trigger
│   └── systemd/
│       ├── remarkable-push-sync.service
│       └── remarkable-push-sync.timer       cadence (OnUnitActiveSec=)
└── server/                                  ── runs on remarkable.exe.xyz ──
    ├── bin/
    │   ├── remarkable-post-sync-by-name.sh    pick latest doc by visibleName, export
    │   ├── remarkable-post-sync.sh            export + activity agent hook (active)
    │   ├── remarkable-activity-agent.ts       LLM digest agent (bundled to .js at deploy)
    │   └── remarkable-activity-agent-hook.sh  hook entrypoint (supports -p prompt)
    ├── nginx/
    │   └── default.conf                     nginx site (installed to sites-available)
    └── web/
        └── raw/
            └── index.html                   swipe/sidebar viewer
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

## Where to make a change

| I want to change... | Edit | Deploy |
| --- | --- | --- |
| How often the tablet pushes | `tablet/systemd/remarkable-push-sync.timer` | `deploy/deploy-tablet.sh` |
| What/how the tablet rsyncs | `tablet/bin/remarkable-push-sync.sh` | `deploy/deploy-tablet.sh` |
| Which doc gets exported (default `Notebook`) | `server/bin/remarkable-post-sync-by-name.sh` (`DOC_NAME=...`) | `deploy/deploy-server.sh` |
| Export format (JPG quality, PDF, etc.) | `server/bin/remarkable-post-sync-by-name.sh` | `deploy/deploy-server.sh` |
| URL routing under `remarkable.exe.xyz` | `server/nginx/default.conf` | `deploy/deploy-server.sh` (auto-reloads nginx) |
| Viewer look/feel, shortcuts, sidebar | `server/web/raw/index.html` | `deploy/deploy-server.sh` |
| Digest page: UI, fonts, history, `--rerender` | `server/bin/remarkable-activity-agent.ts` | `deploy/deploy-server.sh` (builds the bundle) |
| Activity summary prompt/model/output | `server/bin/remarkable-activity-agent-hook.sh` (`-p`, `MODEL`, `OUTPUT_HTML`) | `deploy/deploy-server.sh` |

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

# 3. Secrets for the activity agent
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

# Verify the digest + viewer
curl -sI https://remarkable.exe.xyz/updates/
curl -sI https://remarkable.exe.xyz/raw/
```

## Logs

- reMarkable sync log: `/home/root/.local/state/remarkable-sync/push.log`
- Server export log: `~/remarkable-exports/<DocName>/export.log`
- Activity agent log: `~/remarkable-exports/activity-agent/run.log`
- nginx logs: `sudo tail -f /var/log/nginx/access.log` on the VM
