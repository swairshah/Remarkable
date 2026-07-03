# reMarkable 2 — two projects

Everything runs from this Mac over SSH to the tablet (default `root@10.11.99.1`, USB). Nothing here updates or modifies
the reMarkable's OS; both projects live under `/home/root` (plus one systemd unit file) and have uninstall scripts.

| Project | What | Install |
|---|---|---|
| `pi-harness/` | [pi](https://pi.dev) coding agent running **on** the tablet, using your `~/.pi` auth | `./pi-harness/install.sh` then `ssh -t root@10.11.99.1 pi` |
| `ghostwriter/` | Pen-input assistant: handwrite → image → vision model → reply written back on the page | `./ghostwriter/install.sh` then finger-tap top-right corner |
| `terminal/` | Terminal inside the tablet's GUI (XOVI + literm, tethered — reboot = stock) so you can run `pi` on-device | `./terminal/install.sh` then open Terminal in xochitl |

`probe.sh` prints device diagnostics (arch, OS version, glibc, disk, input devices) — run it and share the output if
anything misbehaves.

Both projects need the tablet's WiFi on at runtime to reach model APIs; the USB cable alone gives SSH but not internet
to the device.
