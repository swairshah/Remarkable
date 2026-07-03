# pi on the reMarkable 2

Runs the [pi coding agent](https://pi.dev) (`@earendil-works/pi-coding-agent` 0.80.3) on the tablet itself, over SSH.

## Why it's packaged this way

- Official Node dropped 32-bit ARM builds in v22, and pi latest requires Node >= 22.19. The installer uses the
  [nodejs unofficial-builds](https://unofficial-builds.nodejs.org) `linux-armv6l` tarball, which runs fine on the RM2's
  Cortex-A7 (armv7).
- pi's npm tree is pure JS on Linux (native modules only exist for darwin/win32/arm64 clipboard), so `payload/` was staged
  once on a Linux box and is pushed as-is. Nothing is compiled or installed from the internet on the tablet.
- Your local `~/.pi/agent/auth.json` (Claude Code / Codex OAuth + any API keys) is copied to the device, so pi is
  logged in from the first run.

## Install

Tablet plugged in via USB (or reachable over WiFi), SSH key already set up:

```sh
./install.sh                  # defaults to root@10.11.99.1
./install.sh root@<wifi-ip>   # alternative
```

## Run

```sh
ssh -t root@10.11.99.1 pi
```

The tablet's WiFi must be on for pi to reach the model APIs.

## Layout on device

```
/home/root/opt/node   Node runtime
/home/root/opt/pi     pi npm tree
/home/root/bin/pi     wrapper (also on PATH for login shells)
/home/root/.pi/       auth + settings (copied from your Mac)
```

## Updating pi later

On the device (WiFi on): `cd /home/root/opt/pi && PATH=/home/root/opt/node/bin:$PATH npm install @earendil-works/pi-coding-agent@latest --ignore-scripts`

## Uninstall

```sh
./uninstall.sh                      # removes node + pi
./uninstall.sh root@10.11.99.1 --purge-auth   # also removes /home/root/.pi
```
