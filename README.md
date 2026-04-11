# reMarkable Sync + Export

This directory contains version-controlled config/scripts for:

1. **reMarkable device**: push diff sync to server on a timer (systemd)
2. **Server**: post-sync hook that finds the most recently modified document by `visibleName` (e.g. `Notebook`) and exports PNG/JPG/PDF

## Layout

- `remarkable/bin/remarkable-push-sync.sh` – rsync push + remote hook trigger
- `remarkable/systemd/remarkable-push-sync.service` – runs push script
- `remarkable/systemd/remarkable-push-sync.timer` – schedule (every 5 min)
- `server/bin/remarkable-post-sync-by-name.sh` – resolves latest UUID by name and exports

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
