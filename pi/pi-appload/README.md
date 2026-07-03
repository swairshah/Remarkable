# pi icon on the reMarkable 2 home screen

Adds a launcher icon inside the stock reMarkable UI (xochitl) that opens the
[pi coding agent](https://pi.dev) in a fullscreen e-ink terminal with an
on-screen keyboard. Requires [`../pi-harness/`](../pi-harness/) (the installer
runs it automatically if pi is missing).

## Stack

| Layer | What | Source |
|---|---|---|
| [xovi](https://github.com/asivery/xovi) | extension loader, `LD_PRELOAD`ed into xochitl | rm-xovi-extensions v19-23052026 (arm32) |
| qt-resource-rebuilder | patches xochitl's QML at runtime (via a device-built `hashtab`) | same bundle |
| [AppLoad](https://github.com/asivery/rm-appload) | launcher menu with app icons inside xochitl | picked by OS: 3.24→v0.4.1, 3.26→v0.5.1, newer→v0.5.3 |
| qtfb shim | emulates an rM1 framebuffer + inputs, renders into an AppLoad window | ships with AppLoad |
| [yaft](https://github.com/timower/rM2-stuff) | framebuffer terminal with on-screen touch keyboard | patched build in `payload/yaft` (see below) |

Tested against reMarkable 2, OS 3.24.0.149. Nothing modifies the OS partition
except two files under `/etc/systemd/system/` (the boot unit; the xochitl
drop-in is a tmpfs mount that vanishes on reboot).

## Install

```sh
./install.sh root@<tablet-ip>     # tablet awake, SSH key set up
```

The installer restarts xochitl twice (hashtab build + xovi start); the tablet
UI goes away for ~1 minute total.

## Use

- Open the AppLoad menu in xochitl (new entry in the side/hamburger menu),
  tap **pi**.
- Long-press the icon to run windowed instead of fullscreen.
- Close fullscreen: drag one finger from the top-center of the screen to the center.
- Hide/show the keyboard: long-press Esc. Type Folio works too (landscape).
- WiFi must be on: pi talks to model APIs at runtime.

## The patched yaft

`payload/yaft` is built from [timower/rM2-stuff](https://github.com/timower/rM2-stuff)
master with `assets/yaft-tablet.patch` applied, which adds:

- `font-scale` config option (integer glyph scaling; the stock 16x32 cells are
  tiny at 1404x1872) and `padding` (blank border around the terminal),
- dedup of mirrored key presses: AppLoad's window forwards every finger tap as
  both a touch *and* a synthesized pen event, which typed every key twice.

Terminal settings live in `/home/root/.config/yaft/config.toml` on the device
(source: `assets/yaft-config.toml`; yaft hot-reloads it on save). To rebuild:

```sh
git clone https://github.com/timower/rM2-stuff && cd rM2-stuff
git apply ../assets/yaft-tablet.patch
docker run --rm --platform linux/amd64 -v "$PWD:/src" -w /src \
  ghcr.io/toltec-dev/base:v4.0 bash -c \
  ". /opt/x-tools/switch-arm.sh && cmake --preset release-toltec && cmake --build build/release-toltec --target yaft"
cp build/release-toltec/apps/yaft/yaft ../payload/yaft
```

## On-device layout

```
/home/root/xovi/                      xovi + extensions (appload.so, qt-resource-rebuilder.so)
/home/root/xovi/exthome/appload/pi/   the app: manifest, icon, pi-session.sh
/home/root/shims/qtfb-shim.so         rM1 framebuffer/input emulation
/home/root/opt/yaft/yaft              terminal
/home/root/.terminfo/y/yaft-256color  terminfo for the shell inside
/etc/systemd/system/xovi-boot.service re-enables xovi after reboot
```

## If something breaks

The device UI misbehaving does not affect SSH (USB `10.11.99.1` always works,
even if xochitl is down):

```sh
ssh root@<ip> /home/root/xovi/stock    # back to stock UI immediately
ssh root@<ip> systemctl disable xovi-boot   # don't re-enable at boot
./uninstall.sh root@<ip>               # remove everything (keeps pi itself)
```

After an OS update, xovi's QML patches usually break (symptom in the journal:
`Couldn't resolve the hashed identifier ... required by AppLoad hooks`).
Re-run `./install.sh` - it rebuilds the hashtab AND picks the AppLoad build
matching the new OS version (AppLoad's hooks are OS-specific, see the
`appload_version_for_os` table in install.sh).

## Memory note

xochitl stays resident while pi runs (window mode), so the ~1 GB of RAM is
shared: xochitl ~200 MB + node (capped at 384 MB old-space by the pi wrapper).
Fine in practice; close other AppLoad apps if things get slow.
