# reMarkable Sync + Export

This directory contains version-controlled config/scripts for:

1. **reMarkable device**: push diff sync to server on a timer (systemd)
2. **Server (export)**: post-sync hook that finds the most recently modified document by `visibleName` (e.g. `Notebook`) and exports PNG/JPG/PDF
3. **Server (web viewer)**: nginx container that serves the exported JPGs at `https://blog.swair.dev/raw/` with a keyboard/swipe/sidebar viewer

## Layout

- `remarkable/bin/remarkable-push-sync.sh` – rsync push + remote hook trigger
- `remarkable/systemd/remarkable-push-sync.service` – runs push script
- `remarkable/systemd/remarkable-push-sync.timer` – schedule (every 5 min)
- `server/bin/remarkable-post-sync-by-name.sh` – resolves latest UUID by name and exports
- `server/nginx/default.conf` – nginx config for the `notes_app` container (serves `/` autoindex and `/raw/` viewer)
- `server/web/raw/index.html` – static swipe viewer (vanilla JS, fetches file list from nginx `autoindex_format json`)

## Current defaults

- `DOC_NAME=Notebook`
- Source on reMarkable: `/home/root/.local/share/remarkable/xochitl/`
- Destination on server: `/home/swair/remarkable-backup/xochitl/`
- Export output on server: `/home/swair/remarkable-exports/`

## Install / update

### 1) Deploy to reMarkable

From this repo root:

```bash
scp devices/remarkable/remarkable/bin/remarkable-push-sync.sh remarkable:/home/root/bin/
scp devices/remarkable/remarkable/systemd/remarkable-push-sync.{service,timer} remarkable:/etc/systemd/system/
ssh remarkable 'chmod 700 /home/root/bin/remarkable-push-sync.sh && systemctl daemon-reload && systemctl enable --now remarkable-push-sync.timer'
```

### 2) Deploy to server

```bash
scp devices/remarkable/server/bin/remarkable-post-sync-by-name.sh swair@swair.dev:/home/swair/bin/
ssh swair@swair.dev 'chmod +x /home/swair/bin/remarkable-post-sync-by-name.sh'
```

### 2b) Deploy web viewer to server

The viewer runs in a small nginx container (`notes_app`) on the existing
`website_network`, behind the `nginx_proxy` container that terminates TLS for
`blog.swair.dev`. Files are bind-mounted live, so updates are just `scp` — no
container restart.

```bash
# Push config + viewer
scp devices/remarkable/server/nginx/default.conf    swair@swair.dev:/home/swair/notes-server/default.conf
scp devices/remarkable/server/web/raw/index.html    swair@swair.dev:/home/swair/notes-server/raw/index.html

# Reload nginx (only needed if default.conf changed)
ssh swair@swair.dev 'docker exec notes_app nginx -t && docker exec notes_app nginx -s reload'
```

One-time container setup (only if `notes_app` doesn't exist yet):

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

Mount map:
- `~/notes` → `/` (generic autoindex — drop html files here to surface them at `blog.swair.dev/<file>.html`)
- `~/notes-server/raw/index.html` → `/raw/` viewer
- `~/remarkable-exports/Notebook/pages_jpg` → `/raw/pages/` (images, served as JSON listing for the viewer)

The `blog` network alias lets the existing `nginx_proxy` keep its unchanged
`proxy_pass http://blog:80;` directive for `blog.swair.dev`.

### 3) Test manually

On reMarkable:

```bash
/home/root/bin/remarkable-push-sync.sh
```

On server:

```bash
/home/swair/bin/remarkable-post-sync-by-name.sh Notebook
```

## Logs

- reMarkable sync log: `/home/root/.local/state/remarkable-sync/push.log`
- server export log: `/home/swair/remarkable-exports/<DocName>/export.log`
