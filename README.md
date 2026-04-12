# reMarkable Sync + Export + Viewer

End-to-end pipeline for: reMarkable tablet → server backup → JPG/PDF export → web
viewer at `https://blog.swair.dev/raw/`.

Everything needed to rebuild this pipeline from scratch lives in this directory.

## System diagram

```
 ┌──────────────────────────────── reMarkable device ───────────────────────────────┐
 │                                                                                   │
 │   /home/root/.local/share/remarkable/xochitl/      (raw notebook state)           │
 │                         │                                                         │
 │                         │  every 5 min via systemd timer                          │
 │                         ▼                                                         │
 │   remarkable/bin/remarkable-push-sync.sh                                          │
 │     · rsync --delete to server:/home/swair/remarkable-backup/xochitl/             │
 │     · ssh swair@swair.dev ~/bin/remarkable-post-sync-by-name.sh Notebook          │
 │                                                                                   │
 │   Scheduled by:                                                                   │
 │     remarkable/systemd/remarkable-push-sync.timer   (OnUnitActiveSec=5min)        │
 │     remarkable/systemd/remarkable-push-sync.service (ExecStart=.../push-sync.sh)  │
 │                                                                                   │
 └───────────────────────────────────┬───────────────────────────────────────────────┘
                                     │  rsync + ssh trigger
                                     ▼
 ┌────────────────────────────────── swair.dev ─────────────────────────────────────┐
 │                                                                                   │
 │   ~/remarkable-backup/xochitl/           (raw sync destination)                   │
 │                 │                                                                 │
 │                 ▼                                                                 │
 │   server/bin/remarkable-post-sync-by-name.sh Notebook                             │
 │     · scans *.metadata for visibleName == "Notebook"                              │
 │     · picks highest lastModified → UUID                                           │
 │     · copies UUID.thumbnails/*.png → pages_png/NNN-<pid>.png                      │
 │     · magick PNG → JPG at quality 92                                              │
 │     · img2pdf PNG → single PDF                                                    │
 │                 │                                                                 │
 │                 ▼                                                                 │
 │   ~/remarkable-exports/Notebook/                                                  │
 │       ├── pages_png/NNN-<pid>.png                                                 │
 │       ├── pages_jpg/NNN-<pid>.jpg   ◄─── what the web viewer serves               │
 │       ├── Notebook.pdf                                                            │
 │       └── export.log                                                              │
 │                 │                                                                 │
 │                 │   bind-mounted read-only into                                   │
 │                 ▼                                                                 │
 │   ┌──── docker container: notes_app  (nginx:alpine) ──────────────────────┐      │
 │   │                                                                        │      │
 │   │   nginx config (bind-mounted from ~/notes-server/default.conf):        │      │
 │   │     server/nginx/default.conf                                          │      │
 │   │       location /            -> ~/notes  (generic autoindex)            │      │
 │   │       location /raw/        -> viewer html                             │      │
 │   │       location /raw/pages/  -> pages_jpg/  (autoindex_format json)     │      │
 │   │                                                                        │      │
 │   │   viewer (bind-mounted from ~/notes-server/raw/index.html):            │      │
 │   │     server/web/raw/index.html                                          │      │
 │   │       · fetch('pages/') → nginx returns JSON file list                 │      │
 │   │       · vanilla JS: sidebar, swipe, arrow keys, Home/End, ?p=N         │      │
 │   │                                                                        │      │
 │   │   network alias `blog` on docker network `website_network`             │      │
 │   └─────────────────────────────┬──────────────────────────────────────────┘      │
 │                                 │ http://blog:80                                  │
 │                                 ▼                                                 │
 │   ┌──── docker container: nginx_proxy  (the site front door) ────────────┐       │
 │   │   Terminates TLS for blog.swair.dev using Let's Encrypt certs         │       │
 │   │   proxy_pass http://blog:80 (that alias resolves to notes_app)        │       │
 │   └───────────────────────────────────────────────────────────────────────┘       │
 │                                                                                   │
 └───────────────────────────────────┬───────────────────────────────────────────────┘
                                     │ HTTPS
                                     ▼
                              https://blog.swair.dev/raw/
```

## Layout

```
devices/remarkable/
├── README.md                                this file
├── scripts/
│   ├── deploy-remarkable.sh                 push device files + reload timer
│   └── deploy-server.sh                     push server files + reload nginx
├── remarkable/                              ── runs on the tablet ──
│   ├── bin/
│   │   └── remarkable-push-sync.sh          rsync + remote export trigger
│   └── systemd/
│       ├── remarkable-push-sync.service
│       └── remarkable-push-sync.timer       cadence (OnUnitActiveSec=)
└── server/                                  ── runs on swair.dev ──
    ├── bin/
    │   ├── remarkable-post-sync-by-name.sh    pick latest doc by visibleName, export
    │   ├── remarkable-post-sync.sh            export + activity hooks (active)
    │   ├── remarkable-activity-agent.ts       TS source (LLM summary + page publish)
    │   ├── remarkable-activity-agent.js       runtime JS deployed to server
    │   ├── remarkable-activity-agent-hook.sh  hook entrypoint (supports -p prompt)
    │   └── remarkable-activity-diff.py        legacy python diff helper
    ├── nginx/
    │   └── default.conf                     notes_app nginx config
    └── web/
        └── raw/
            └── index.html                   swipe/sidebar viewer
```

## Where to make a change

| I want to change... | Edit | Deploy |
| --- | --- | --- |
| How often the tablet pushes | `remarkable/systemd/remarkable-push-sync.timer` | `scripts/deploy-remarkable.sh` |
| What/how the tablet rsyncs | `remarkable/bin/remarkable-push-sync.sh` | `scripts/deploy-remarkable.sh` |
| Which doc gets exported (default `Notebook`) | `server/bin/remarkable-post-sync-by-name.sh` (`DOC_NAME=...`) | `scripts/deploy-server.sh` |
| Export format (JPG quality, PDF, etc.) | `server/bin/remarkable-post-sync-by-name.sh` | `scripts/deploy-server.sh` |
| URL routing under `blog.swair.dev` | `server/nginx/default.conf` | `scripts/deploy-server.sh` (auto-reloads nginx) |
| Viewer look/feel, shortcuts, sidebar | `server/web/raw/index.html` | `scripts/deploy-server.sh` |
| Add a new content path (e.g. `/photos/`) | new `location` in `default.conf` **and** add a `-v` mount to the `docker run` in this README | redeploy + recreate `notes_app` |
| Activity summary page prompt/model/output | `server/bin/remarkable-activity-agent-hook.sh` (`-p`, `MODEL`, `OUTPUT_HTML`) | `scripts/deploy-server.sh` |

## Deploy

Both deploy scripts assume SSH is pre-configured:
- `ssh remarkable` reaches the tablet (add an alias in `~/.ssh/config` if needed)
- `ssh swair@swair.dev` reaches the server (override host with `SERVER_HOST=user@host`)

From the repo root:

```bash
# Deploy tablet-side changes (push script, systemd timer/service)
devices/remarkable/scripts/deploy-remarkable.sh

# Deploy server-side changes (post-sync scripts, nginx config, viewer html)
# → automatically runs `nginx -t` and `nginx -s reload` inside notes_app
devices/remarkable/scripts/deploy-server.sh

# Deploy + immediately trigger a manual export run so you can see the result
devices/remarkable/scripts/deploy-server.sh --run
```

Env overrides: `REMARKABLE_HOST=...`, `SERVER_HOST=user@host`, `DOC_NAME=Foo`.

## One-time setup

### Tablet

The deploy script handles `chmod`, `daemon-reload`, and `enable --now` on the
timer, so `deploy-remarkable.sh` is also the install command.

### Server — export pipeline

`deploy-server.sh` ships the scripts and makes them executable. The backup
destination `~/remarkable-backup/xochitl/` just needs to exist; `rsync` creates
it on first push, but you can pre-create it:

```bash
ssh swair@swair.dev 'mkdir -p ~/remarkable-backup/xochitl ~/remarkable-exports'
```

Requires on the server: `python3`, `rsync`, `magick` (ImageMagick), `img2pdf`.

### Server — notes_app container (one-time)

`deploy-server.sh` only pushes files and reloads nginx — it does not create the
container. Run this once on the server:

```bash
ssh swair@swair.dev 'mkdir -p ~/notes ~/notes-server/raw && \
  docker run -d --name notes_app \
    --network website_network --network-alias blog \
    --restart unless-stopped \
    -v /home/swair/notes:/usr/share/nginx/html:ro \
    -v /home/swair/notes-server/default.conf:/etc/nginx/conf.d/default.conf:ro \
    -v /home/swair/notes-server/raw:/srv/raw-app:ro \
    -v /home/swair/remarkable-exports/Notebook/pages_jpg:/srv/raw-pages:ro \
    nginx:alpine'
```

Then run `deploy-server.sh` to populate the mounted config/html.

Mount map:
- `~/notes` → `/` (generic autoindex)
- `~/notes-server/default.conf` → `/etc/nginx/conf.d/default.conf`
- `~/notes-server/raw` → `/srv/raw-app` (`/raw/` viewer html)
- `~/remarkable-exports/Notebook/pages_jpg` → `/srv/raw-pages` (`/raw/pages/` images)

The `--network-alias blog` is critical: the existing `nginx_proxy` container
has `proxy_pass http://blog:80;` for `blog.swair.dev`, and the alias is what
makes `notes_app` answer to that name on the `website_network` docker network.

## Current defaults

- `DOC_NAME=Notebook`
- Source on reMarkable: `/home/root/.local/share/remarkable/xochitl/`
- Destination on server: `/home/swair/remarkable-backup/xochitl/`
- Export output on server: `/home/swair/remarkable-exports/`
- Activity page output (default): `/home/swair/notes/index.html`
- Activity state dir: `/home/swair/remarkable-exports/activity-agent/`
- Viewer URL: `https://blog.swair.dev/raw/`

## Test manually

```bash
# Force a push from the tablet
ssh remarkable /home/root/bin/remarkable-push-sync.sh

# Force an export on the server
ssh swair@swair.dev '~/bin/remarkable-post-sync-by-name.sh Notebook'

# Verify the viewer
curl -sI https://blog.swair.dev/raw/
```

## Logs

- reMarkable sync log: `/home/root/.local/state/remarkable-sync/push.log`
- Server export log: `/home/swair/remarkable-exports/<DocName>/export.log`
- TS activity hook log: `/home/swair/remarkable-exports/activity-agent/run.log`
- notes_app nginx log: `docker logs notes_app`
