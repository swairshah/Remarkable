# Terminal in the reMarkable GUI (run pi on-device)

Installs [XOVI](https://github.com/asivery/xovi) (runtime extension framework for xochitl) plus
[rm-literm](https://github.com/asivery/rm-literm) (a QtQuick terminal emulator that loads into the stock UI). Combined
with `../pi-harness`, this lets you open a terminal from the tablet's own GUI and run `pi` there — no computer attached.

## Safety model (why this fits "don't touch the OS")

XOVI is **tethered by design**: it restarts xochitl with extensions injected, but modifies nothing on the boot path.
Any reboot returns the tablet to 100% stock. Do **not** try to make xovi start on boot by editing xochitl's systemd
units — that's the one way to bootloop the device. If you want to start it from the tablet without a computer, install
[xovi-tripletap](https://github.com/rmitchellscott/xovi-tripletap) (triple-press the power button), which uses its own
service and is the supported way.

## Install

```sh
./install.sh          # prompts before touching anything; tablet WiFi should be ON
```

Pinned + sha256-verified: `xovi-arm32.tar.gz` v19-23052026, `literm-arm32.so` v0.1.6.

## Use

After `xovi/start`, xochitl restarts with a Terminal available in the UI (literm; tap the top-right corner inside it
for the settings/keyboard toolbar). Run `pi` there. An on-screen keyboard is built in; a Type Folio also works.

- After any reboot: `ssh root@10.11.99.1 /home/root/xovi/start` (or triple-tap power with tripletap installed)
- Back to stock instantly: `ssh root@10.11.99.1 /home/root/xovi/stock` or just reboot

## Caveat

literm patches xochitl's QML, so it is firmware-version-sensitive (the hashtab rebuild handles most of it). If no
Terminal shows up after install, run `../probe.sh` and report the firmware version — worst case the tablet is just
stock again after a reboot; nothing can be left broken.

## Uninstall

```sh
./uninstall.sh
```
