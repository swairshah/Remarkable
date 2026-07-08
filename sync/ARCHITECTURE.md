# reMarkable → remarkable.exe.xyz sync architecture

How handwritten notes on the reMarkable tablet end up as a browsable web page and
an LLM-generated activity digest at **https://remarkable.exe.xyz/updates/**.
Covers everything in `sync/`: the deploy scripts (`deploy/`), the server-side
pipeline (`server/`), and the device-side files (`tablet/`).

> Historical note: this stack originally lived on swair.dev (dockerized nginx,
> digest at blog.swair.dev). It was migrated to an exe.dev VM in July 2026;
> the old server is no longer part of the pipeline.

## The big picture

```
┌─ reMarkable tablet ────────────────────────────────────────────────┐
│  systemd timer (every 5 min)                                       │
│    └─ remarkable-push-sync.sh                                      │
│         rsync -az --delete  ~/.local/share/remarkable/xochitl/     │
│              │                                                     │
└──────────────┼─────────────────────────────────────────────────────┘
               ▼  ssh (tablet key, registered with the exe.dev account)
┌─ remarkable.exe.xyz (exe.dev VM, user exedev) ─────────────────────┐
│  ~/remarkable-backup/xochitl/            (mirror of the tablet)    │
│              │                                                     │
│    ~/bin/remarkable-post-sync.sh auto Notebook   (invoked by the   │
│         │                              tablet right after rsync)   │
│         ├─ export "Notebook" pages → PNG / JPG / PDF               │
│         │    → ~/remarkable-exports/Notebook/                      │
│         └─ remarkable-activity-agent-hook.sh → node agent (LLM)    │
│              → ~/notes/updates/index.html   (activity digest)      │
│              │                                                     │
│  nginx (native, port 8000)                                         │
│    /            → 302 /updates/                                    │
│    /updates/    → activity digest                                  │
│    /raw/        → notebook viewer SPA                              │
│    /raw/pages/  → pages_jpg/  (JSON autoindex = the viewer's API)  │
└──────────────┼─────────────────────────────────────────────────────┘
               ▼
   exe.dev proxy: TLS termination, https://remarkable.exe.xyz/ → :8000
   (proxy is set public: no exe.dev login required)
```

Two hosts, one direction: the tablet pushes; the server never contacts the
tablet. The tablet also triggers all server-side processing by running the
post-sync hook over the same SSH connection it used to rsync.

## exe.dev specifics (worth knowing before touching anything)

- **SSH goes through exe.dev's front door.** `ssh exedev@remarkable.exe.xyz`
  is authenticated against the SSH keys registered to the exe.dev *account*,
  not the VM's `authorized_keys`. The tablet's sync key is registered there as
  `remarkable-tablet-sync` (`ssh exe.dev ssh-key list`). An unregistered key
  gets a "Please complete registration" banner that corrupts rsync streams.
- **The HTTP proxy only targets ports 3000–9999**, so nginx listens on **8000**
  (not 80). Configured with `ssh exe.dev share port remarkable 8000`; made
  public with `ssh exe.dev share set-public remarkable`.
- exe.dev terminates TLS. nginx sets `absolute_redirect off` so the internal
  port/scheme never leak into `Location` headers.
- The VM is Ubuntu 24.04, user `exedev` (passwordless sudo), no docker needed.

## Stage 1 — Tablet push (`tablet/`)

Deployed by `deploy/deploy-tablet.sh` to the tablet (`ssh remarkable`,
override with `REMARKABLE_HOST=`). The `remarkable` alias in `~/.ssh/config`
points at the tablet's LAN IP (changes occasionally — update `Hostname` there).

| File | Installed to | Role |
|---|---|---|
| `tablet/bin/remarkable-push-sync.sh` | `/home/root/bin/` | The push itself |
| `tablet/systemd/remarkable-push-sync.timer` | `/etc/systemd/system/` | Fires 3 min after boot, then every 5 min |
| `tablet/systemd/remarkable-push-sync.service` | `/etc/systemd/system/` | Oneshot, low priority (`Nice=10`, best-effort IO) |

`remarkable-push-sync.sh` does three things:

1. Ensures the destination directory exists on the server.
2. `rsync -az --delete` of the **entire xochitl document store**
   (`/home/root/.local/share/remarkable/xochitl/`) to
   `exedev@remarkable.exe.xyz:/home/exedev/remarkable-backup/xochitl/`.
   Because of `--delete`, the server copy is a true mirror — deletions on the
   tablet propagate. After the first baseline only diffs transfer.
3. Runs the remote hook `/home/exedev/bin/remarkable-post-sync.sh auto Notebook`
   over SSH (failure tolerated — the sync still counts as done).

It authenticates with a dedicated key (`/home/root/.ssh/id_sync_dropbear_ed25519`)
and uses dropbear's `-y` flag since the tablet's SSH client is dropbear.
Logs append to `/home/root/.local/state/remarkable-sync/push.log`.

## Stage 2 — Server-side export (`server/bin/`)

Deployed by `deploy/deploy-server.sh` to `~/bin` on the VM
(`ssh exedev@remarkable.exe.xyz`, override with `SERVER_HOST=`). All scripts
resolve paths from `$HOME` (overridable via `REMARKABLE_BASE` /
`REMARKABLE_OUT`), so nothing is hardcoded to a particular user.

### `remarkable-post-sync.sh` — the main hook

Called by the tablet after every sync as `remarkable-post-sync.sh auto Notebook`.

1. **Resolve the document.** `auto` means: scan every `*.metadata` file in the
   backup for `visibleName == "Notebook"` and pick the most recently modified
   match (xochitl stores documents as UUIDs, and names aren't unique). A UUID
   can also be passed explicitly as the first argument.
2. **Export pages.** Reads the document's `.content` JSON for the ordered page
   list (`cPages.pages[].id`), then copies xochitl's own thumbnail PNGs
   (`<uuid>.thumbnails/<pageId>.png`) into
   `~/remarkable-exports/Notebook/pages_png/NNN-<pageId>.png`. **No .rm stroke
   rendering happens** — the pipeline deliberately reuses the tablet's
   pre-rendered thumbnails, which is why no rendering toolchain is needed.
3. **Derive formats.** PNG → JPG (quality 92, via `magick` or `convert`) into
   `pages_jpg/`, and all PNGs → a single `Notebook.pdf` via `img2pdf`. Both
   steps are skipped gracefully if the tool is missing.
4. **Run the activity agent** via `remarkable-activity-agent-hook.sh`
   (always — even on the early-exit paths where the document isn't found,
   so the digest still updates).

Everything logs to `~/remarkable-exports/Notebook/export.log`.

### `remarkable-post-sync-by-name.sh` — manual export

Same resolve-and-export logic, but takes a document name as `$1`, does **not**
run the activity agent, and is what `deploy-server.sh --run` calls. Use it
to (re)export any document by name without waiting for the tablet's timer.

### `remarkable-activity-agent.ts` — the LLM activity agent

The `.ts` is the only committed source; `deploy/deploy-server.sh` bundles it
with bun (`--target=node --format=cjs`) at deploy time, and the server runs
the resulting `node ~/bin/remarkable-activity-agent.js` — so the server needs
Node but no TypeScript toolchain, and git never sees generated code.

`remarkable-activity-agent-hook.sh` is a thin wrapper supplying the production
flags (source dir, state dir, output `~/notes/updates/index.html`, `~/.env`
for secrets, model `anthropic/claude-sonnet-4-6`).

Per run:

1. **Snapshot.** Builds a state record for every document in the backup:
   `lastModified`, `lastOpened`, `lastOpenedPage`, bookmark-file hash + count,
   a highlights signature (hash of every highlight file's name/mtime/size),
   and the newest mtime among the document's `.rm` stroke files.
2. **Diff** against `last-state.json` from the previous run. First-seen
   documents are ignored (avoids a flood on the baseline run). Each changed
   doc gets human-readable "bits": `page 12 -> 15`, `opened`, `modified`,
   `bookmarks 2 -> 3`, `highlights 5 -> 7`, `handwriting changed`.
3. **Summarize.** If anything changed, it takes the 10 most recent changes and
   calls OpenRouter's chat-completions API (key: `OPENROUTER_API_KEY` from
   `~/.env`). If the "Notebook" doc's handwriting changed, it attaches up to
   6 of its most recently modified thumbnail images as base64 data URLs so the
   model can see *what* was written. Missing key or API failure degrades to a
   plain bullet list — the page still publishes.
4. **Publish.** Writes `latest.md` and appends a JSONL record to
   `history.jsonl` (state dir: `~/remarkable-exports/activity-agent/`), then
   renders a self-contained dark-themed HTML dashboard — summary, change list,
   and a hamburger-toggled sidebar of the last 20 runs, each clickable to view
   that run's summary — to `~/notes/updates/index.html`, served at `/updates/`.
   If nothing changed, nothing is published and the previous page stays up.
   `--rerender` regenerates the page from stored state (no diff, no LLM) —
   used after design changes.

**Typography** (dashboard and viewer alike): body text is set in
`Iowan Old Style` (falling back through Palatino/Georgia to any serif — it's
an Apple system font, not a webfont), and code/monospace runs (the summary
`<pre>`, uuid chips, page counters) in **Google Sans Code**, loaded from
Google Fonts.

### Shelley trigger — agentic post-processing

After the digest agent runs, `remarkable-post-sync.sh` pings **Shelley** (the
exe.dev coding agent resident on every VM, port 9999) via the exe.dev HTTPS
API: `POST https://exe.dev/exec` with body `shelley prompt remarkable '...'`,
authenticated by `EXE_API_TOKEN` from `~/.env` — a bearer token scoped to the
`shelley prompt` subcommand only (note: exe.dev token scopes name subcommands
as a single string; granting the parent `shelley` does NOT cover them):
`ssh exe.dev "ssh-key generate-api-key --label=remarkable-shelley-trigger
'--cmds=shelley prompt' --exp=1y"`.

- Fires only when the digest agent actually published (it writes a
  `last-published` marker; the hook compares it to `last-shelley-trigger`).
- Cooldown: at most one trigger per `SHELLEY_COOLDOWN_MIN` (default 30) so an
  active writing session doesn't spawn an agent run every 5 minutes.
- Shelley's standing instructions live in `server/shelley/AGENTS.md`,
  deployed to `~/.config/shelley/AGENTS.md` on the VM. Its duties: read the
  latest `diffs.jsonl` entries + `changed-pages/` images, keep a journal at
  `~/remarkable-journal.md` (its memory across runs), and maintain a daily
  exercises post under `~/notes/notes/` → https://remarkable.exe.xyz/notes/.
- Trigger transcript: `~/remarkable-exports/activity-agent/shelley-trigger.log`.

The diff feed Shelley consumes is written by the digest agent on every
publishing run: `diffs.jsonl` (per-run JSON with per-page detail) and
`changed-pages/<runstamp>/` (PNGs of exactly the pages that changed, newest
15 runs kept).

## Stage 3 — Web serving (`server/nginx/`, `server/web/`)

nginx runs natively on the VM (no docker), listening on **port 8000**; the
exe.dev proxy terminates TLS for `remarkable.exe.xyz` and forwards to it.
The site config is installed by `deploy-server.sh` to
`/etc/nginx/sites-available/remarkable` (symlinked into `sites-enabled/`,
stock `default` site removed).

`server/nginx/default.conf` routes:

| Location | Serves | Notes |
|---|---|---|
| `/` | `302 → /updates/` | The digest is the front page |
| `/updates/` | `~/notes/updates/` | LLM activity digest |
| other paths under `/` | `~/notes/` | Generic autoindex for ad-hoc files |
| `/raw/` | viewer app (`~/notes-server/raw/`) | `server/web/raw/index.html` |
| `/raw/pages/` | `~/remarkable-exports/Notebook/pages_jpg/` | `autoindex_format json` — this JSON listing **is** the viewer's API |
| `/notebook/` | live viewer app (`~/notes-server/notebook/`) | `server/web/notebook/index.html` |
| `/notebook/data/` | `~/remarkable-backup/notebook-app/` | JSON autoindex + `no-cache`; the viewer's API |
| `/notes/` | `~/notes/notes/` | Shelley's exercises posts (served via the generic `/` root, no extra config) |

### `/notebook/` — notebook-app live viewer

The custom notebook app (`notebook/` in this repo — the "notebook that writes
back") keeps its whole world in `/home/root/.local/share/notebook/` on the
tablet: `pages/page-NNNN.json` (vector strokes, coords ×10, `g:0` = user ink,
`g>0` = pi's ink), `library/*.md` (pi-curated articles with frontmatter),
`AGENT.md` (pi's standing instructions), `sessions/*.jsonl` (raw pi
transcripts), `settings.json`. The push script mirrors that directory to
`~/remarkable-backup/notebook-app/` on every 5-minute sync (`*.tmp` excluded —
the app writes-then-renames page saves).

No server-side processing at all: the viewer SPA fetches the nginx JSON
autoindex, renders page vectors itself on `<canvas>` (user ink black, pi ink
blue — toggleable), renders library/AGENT.md markdown client-side (KaTeX for
math, pinned by SRI), and lists sessions for download. It polls the autoindex
every 20 s and re-fetches only files whose mtime changed, so an open browser
tab live-updates within one sync cycle of writing on the tablet.

`server/web/raw/index.html` is a single-file, dependency-free notebook viewer:
it `fetch`es the `pages/` JSON autoindex, filters to image files, sorts by name
(the `NNN-` prefix from the export gives page order), and opens on the **last**
page — the assumption being you want to see what you wrote most recently.
Navigation: thumbnail sidebar, click zones, arrow keys / Home / End / `b`, and
touch swipe. New JPGs appear on the next page load with no server action.

## Deploy scripts (`deploy/`)

Both assume SSH is already configured and are safe to re-run (idempotent copies).

### `deploy/deploy-tablet.sh`

Ships tablet files: scp `tablet/bin/*.sh` → `/home/root/bin/` (chmod 700),
systemd units → `/etc/systemd/system/`, then `daemon-reload` and
`enable --now` + restart of the timer. Prints the timer status at the end.

### `deploy/deploy-server.sh`

Builds the agent bundle with bun (required locally), then ships server files:
`server/bin/*.sh` + the built `remarkable-activity-agent.js` → `~/bin/`
(chmod +x on the entrypoints), apt-installs missing runtime deps (`nodejs`,
`img2pdf`, `imagemagick`), nginx config → `~/notes-server/default.conf` →
`/etc/nginx/sites-available/remarkable` (+ symlink, `nginx -t`,
`systemctl reload`), viewer → `~/notes-server/raw/index.html`. It also
`chmod o+x $HOME` so the nginx worker (`www-data`) can traverse into the
content dirs. Flags/env:

- `--run` — after deploying, trigger a manual export
  (`remarkable-post-sync-by-name.sh "$DOC_NAME"`)
- `DOC_NAME` — document to export with `--run` (default `Notebook`)
- `SERVER_HOST` — deploy target (default `exedev@remarkable.exe.xyz`)

## State & data locations (on the VM, user `exedev`)

| Path | What |
|---|---|
| `~/remarkable-backup/xochitl/` | rsync mirror of the tablet's document store |
| `~/remarkable-exports/Notebook/` | `pages_png/`, `pages_jpg/`, `Notebook.pdf`, copied `.content`/`.metadata`, `export.log` |
| `~/remarkable-exports/activity-agent/` | agent state: `last-state.json`, `latest.md`, `history.jsonl`, `run.log` |
| `~/notes/updates/index.html` | published activity digest (`/updates/`) |
| `~/notes-server/` | nginx config + viewer html |
| `~/.env` | `OPENROUTER_API_KEY` for the activity agent |

All agent state and the document mirror were migrated from swair.dev, so
digest history is continuous across the move and the first sync after the
migration was an incremental diff, not a re-upload.

## Design notes

- **Push-only, tablet-driven.** The server holds no credentials for the tablet
  and runs no scheduler of its own; the tablet's systemd timer is the single
  clock driving the whole pipeline, including server-side processing.
- **Thumbnails as the render.** Exports reuse xochitl's page thumbnails rather
  than parsing `.rm` stroke files, trading resolution for zero rendering
  dependencies.
- **Diff-based publishing.** The activity page only re-renders when the
  snapshot diff is non-empty, so a sync with no activity changes nothing.
- **Graceful degradation everywhere.** Missing document, missing ImageMagick,
  missing `img2pdf`, missing API key, LLM failure — each step logs and skips
  rather than aborting, and the activity agent runs even when the export
  bails early.
- **`--delete` cuts both ways.** The backup is a mirror, not an archive — a
  document deleted on the tablet disappears from the server on the next sync
  (already-exported PNG/JPG/PDF outputs survive, though).
