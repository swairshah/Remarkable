# pi on the reMarkable 2

[pi](https://pi.dev) — a terminal AI coding agent — running natively **on** a
reMarkable 2 e-ink tablet, launched from an icon in the stock UI, with tools
that let it read and write the tablet's own notebooks.

Everything installs from Mac over SSH. Nothing modifies the OS partition
beyond two systemd files; a reboot without our boot unit returns the tablet to
100% stock, and every project has an uninstall script.

```
┌─────────────────────────── reMarkable 2 ───────────────────────────┐
│                                                                    │
│  ┌──────────────── xochitl (stock UI, Qt) ─────────────────┐       │
│  │  xovi (LD_PRELOAD) ─ loads extensions into xochitl      │       │
│  │   ├─ qt-resource-rebuilder ─ patches the UI's QML       │       │
│  │   └─ AppLoad ─ launcher menu + qtfb window server ──┐   │       │
│  └──────────────────────────────────────────────▲──────┼───┘       │
│                                        display  │      │ input     │
│                                       (shm fb)  │      ▼ (socket)  │
│  ┌───────────────────────────────────────────────────────┐         │
│  │ yaft terminal (patched)     LD_PRELOAD=qtfb-shim.so    │         │
│  │  · draws to a fake rM1 /dev/fb0 → qtfb shared memory   │         │
│  │  · reads fake /dev/input/event* ← qtfb input packets   │         │
│  │  · on-screen keyboard, 24x48 cells, throttled refresh  │         │
│  └──────────────────────────┬────────────────────────────┘         │
│                             │ pty                                  │
│  ┌──────────────────────────▼────────────────────────────┐         │
│  │ pi-session.sh → pi wrapper → node 22 → pi coding agent │         │
│  │  · auth: OAuth tokens copied from the Mac              │         │
│  │  · remarkable-notes extension: tools + env prompt      │────────┼──▶ model APIs
│  └──────────────────────────┬────────────────────────────┘  (WiFi) │
│                             │ remarkable_list/read/write           │
│  ┌──────────────────────────▼────────────────────────────┐         │
│  │ xochitl document storage (~/.local/share/remarkable)   │         │
│  │  uuid.metadata / uuid.content / uuid/<page>.rm (v6)    │         │
│  └────────────────────────────────────────────────────────┘        │
└─────────────────────────────────────────────────────────────────────┘
```

## Repo layout

| Directory | What | Details |
|---|---|---|
| `pi-harness/` | pi itself on the tablet: Node runtime + pi npm tree + auth/settings | [README](pi-harness/README.md) |
| `pi-appload/` | the GUI layer: xovi + AppLoad + patched yaft + the launcher app | [README](pi-appload/README.md) |
| `pi-extensions/` | pi extensions shipped to the tablet (`remarkable-notes.ts`) | below |
| `ghostwriter/` | unrelated: pen-input → vision-model assistant | [README](ghostwriter/README.md) |
| `terminal/` | earlier xovi+literm terminal experiment, superseded by `pi-appload/` | |
| `probe.sh` | device diagnostics (`./probe.sh root@<ip>`) | |

## Install

```sh
./pi-harness/install.sh  root@<tablet-ip>   # pi + node + auth + extensions
./pi-appload/install.sh  root@<tablet-ip>   # icon + terminal (runs harness first if needed)
```

Then tap the Pi icon in the sidebar's AppLoad menu. WiFi must be on — the
tablet talks to the model APIs directly.

### 1. Running pi at all (`pi-harness/`)

The rM2 is armv7 (32-bit) with 1 GB RAM and a busybox userland. pi needs
Node ≥ 22, and official Node dropped 32-bit ARM builds — so the harness uses
the [nodejs unofficial-builds](https://unofficial-builds.nodejs.org)
`linux-armv6l` tarball, which runs fine on the Cortex-A7. Two OS gaps are
filled by shipping libraries next to the binaries (never into `/usr`):

- Node needs `libatomic.so.1` → Debian armhf build at `/home/root/opt/node/lib`
- yaft needs `libevdev.so.2` → same trick at `/home/root/opt/yaft/lib`

pi's npm tree is pure JS on Linux, staged once and pushed as a tarball —
nothing compiles on the tablet. Your Mac's `~/.pi/agent/auth.json` (OAuth for
Claude/Codex) is copied over, so pi is logged in from the first run. Its
`settings.json` is **sanitized**, not copied: the Mac's packages list
(git-cloned extensions, npm packages, local paths) doesn't exist on the tablet
and crashed pi at startup (`spawn git ENOENT` — there is no git on the device).
The tablet gets an empty packages list plus the extensions in `pi-extensions/`.

The wrapper `/home/root/bin/pi` caps V8's heap at 384 MB so pi and xochitl can
coexist in 1 GB.

### 2. An icon in the stock UI (`pi-appload/`)

The reMarkable has no app concept, and the classic hack ecosystem (Toltec)
supports only OS ≤ 3.3. On OS 3.24 the working stack is
[asivery](https://github.com/asivery)'s:

- **xovi** — an `LD_PRELOAD` extension framework injected into xochitl. Its
  systemd drop-in lives on a **tmpfs** mount, so any reboot reverts to stock;
  our `xovi-boot.service` re-enables it after boot (disable it and the mod is
  fully tethered).
- **qt-resource-rebuilder** — patches xochitl's compiled QML at runtime. It
  resolves UI symbols through a `hashtab` built **on the device from the
  installed firmware**, which is what makes it survive across OS versions.
- **AppLoad** — adds the launcher menu to the sidebar and hosts apps. For
  external (non-QML) apps it allocates a **qtfb** framebuffer: a shared-memory
  canvas composited into the UI as a window, with input forwarded over a
  socket. ⚠️ AppLoad's QML hooks are tied to the xochitl version —
  `install.sh` picks the release per OS (3.24→v0.4.1, 3.26→v0.5.1, else
  latest). Symptom of a mismatch: `Couldn't resolve the hashed identifier …`
  in the journal and no AppLoad menu.

The "app" itself is just a directory —
`/home/root/xovi/exthome/appload/pi/{external.manifest.json,icon.png,pi-session.sh}`.
The manifest launches yaft under **qtfb-shim.so**, which emulates a reMarkable
**1**: it spoofs `/sys/devices/soc0/machine`, fakes an rM1 `/dev/fb0`
(forwarding damage rectangles to qtfb), and serves fake
`/dev/input/event*` devices fed by the window's input packets. yaft's rM1 code
path then works unmodified inside a window.

Two input subtleties cost real debugging time and are pinned in the manifest:

- `QTFB_SHIM_MODEL=1` — in AppLoad v0.4.1 this env var is a *boolean*
  (`"RM1"` parses as false and disables the spoof); newer shims parse it as a
  model name. `"1"` means the right thing in both.
- `QTFB_SHIM_INPUT_PATH_TOUCHSCREEN=/dev/input/event2` — on the rM2,
  `/dev/input/touchscreen0` is a symlink to the **pen** device, so the shim's
  default touch role matched two devices and delivered every tap twice
  ("hhii" typing).

### 3. A terminal that works on e-ink (patched yaft)

`payload/yaft` is [timower/rM2-stuff](https://github.com/timower/rM2-stuff)'s
yaft (framebuffer terminal with an on-screen keyboard, Type Folio support)
plus `assets/yaft-tablet.patch`:

- **`font-scale` + `padding`** — integer glyph scaling and a blank border.
  The built-in font is also swapped from 16x32 to **Terminus 12x24**
  (`tools/gen-glyphs.py` regenerates `glyph.h` from BDF), so scale 2 gives
  24x48 px cells ≈ 57 columns.
- **`full-refresh-interval`** — TUIs clear the screen on every frame, and
  yaft mapped each clear to a full GC16 refresh: a full-panel black/white
  flash, several times a second. Refreshes are now throttled (device config:
  ≥ 120 s apart); partial updates use non-flashing DU/A2 waveforms, and a
  **five-finger tap** forces a clean refresh whenever ghosting builds up.
- **tap dedup** in the keyboard, as a second line of defense for mirrored
  pen/touch events.

Config lives at `/home/root/.config/yaft/config.toml` and hot-reloads.

### 4. pi ↔ the tablet's own documents (`pi-extensions/remarkable-notes.ts`)

xochitl stores documents as uuid-named file groups: `uuid.metadata` (name,
parent folder), `uuid.content` (page list), and `uuid/<page>.rm` — reMarkable's
proprietary **v6 lines format** (typed text + pen strokes). The extension
gives pi three tools instead of letting it poke at that with bash:

| Tool | What it does |
|---|---|
| `remarkable_list` | search/browse documents & folders (names, paths, page counts, dates) |
| `remarkable_read` | reads notebooks: **typed** text extracted from the v6 blocks (best effort), and **handwritten** pages parsed from raw pen strokes and rendered to PNGs attached to the tool result — pi's multimodal model reads the handwriting from the image (no OCR engine involved) |
| `remarkable_write` | markdown → minimal EPUB (hand-rolled stored zip, no deps) → uploaded via xochitl's USB web-interface API (appears instantly) or dropped into storage (appears after the next xochitl restart) |

The extension also uses pi's extension API for two environment fixes:

- **`before_agent_start`** appends a system-prompt section so pi *knows* it's
  on a reMarkable: prefer the tools above for anything note-related, never
  restart xochitl (this terminal lives inside it), busybox flags only, no
  git, 1 GB RAM. Without this pi reached for `ls`/`find` by default.
- **`session_start`** replaces the animated working spinner with one static
  frame (`setWorkingIndicator`) — the braille animation redrew several times
  per second, which flickers on e-ink.

## On-device layout

```
/home/root/opt/node/            Node 22 (+ lib/libatomic.so.1)
/home/root/opt/pi/              pi npm tree
/home/root/opt/yaft/            patched terminal (+ lib/libevdev.so.2)
/home/root/bin/pi               wrapper (heap cap, PATH, LD_LIBRARY_PATH)
/home/root/.pi/agent/           auth.json, settings.json, extensions/
/home/root/.config/yaft/        terminal config (hot-reloads)
/home/root/.terminfo/y/         yaft-256color terminfo
/home/root/xovi/                xovi + extensions.d/{qt-resource-rebuilder,appload}.so
/home/root/xovi/exthome/appload/pi/   the launcher app (manifest, icon, session script)
/home/root/shims/qtfb-shim.so   rM1 emulation shim
/etc/systemd/system/xovi-boot.service   re-enables xovi after reboot
```

## Lifecycle, rescue, updates

- **Launch**: sidebar → AppLoad → tap Pi. Long-press = windowed. Close
  fullscreen: drag from top-center to screen center. Keyboard: long-press Esc
  to hide; five-finger tap to force an e-ink refresh.
- **UI misbehaves**: `ssh root@<ip> /home/root/xovi/stock` → instant stock UI.
  `systemctl disable xovi-boot` keeps it off across reboots. SSH always works
  (USB `10.11.99.1` even when WiFi/UI are down).
- **After a reMarkable OS update**: xovi's QML patches likely break — rerun
  `./pi-appload/install.sh` (rebuilds the hashtab, re-picks the AppLoad
  version).
- **Updating pi**: on the device,
  `cd /home/root/opt/pi && PATH=/home/root/opt/node/bin:$PATH npm install @earendil-works/pi-coding-agent@latest --ignore-scripts`.
- **Uninstall**: `pi-appload/uninstall.sh` (GUI layer),
  `pi-harness/uninstall.sh [--purge-auth]` (pi itself).

## Known limits

- `remarkable_read`'s typed-text extraction is a heuristic over the v6 format
  (fine for linearly written notes). Handwriting is delivered as rendered
  page images (capped per call — use `firstPage`/`maxPages` to window big
  notebooks), so reading it costs image tokens and needs a vision model.
- `remarkable_write`'s instant path needs the USB web interface enabled
  (Settings → Storage); otherwise new documents appear on the next xochitl
  restart.
- No `git` on the tablet yet, so pi can edit files but not manage repos.
- Deep sleep kills the session: the terminal is a child of xochitl, so
  letting the tablet sleep for hours ends the pi conversation (pi's session
  files survive; `pi --continue` in a new terminal resumes it).
