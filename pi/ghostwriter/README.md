# Handwriting → vision model on the reMarkable 2

Deploys [ghostwriter](https://github.com/awwaiid/ghostwriter) (MIT) as an always-on service: write with the pen on any
notebook page, tap the **top-right corner with a finger**, and the page is screenshotted, sent to a vision model
(Claude by default), and the reply is written back onto the page (typed text or drawn SVG).

## Why this and not a screen-takeover app

The RM2 has no directly writable framebuffer — the e-ink is driven in software by xochitl. A standalone full-screen app
would need rm2fb/Toltec, which is pinned to specific OS versions and against the "don't modify the system" constraint.
Ghostwriter instead uses stock xochitl as the canvas: it reads the screen out of xochitl's memory and writes replies by
injecting virtual pen/keyboard input. Nothing about the OS is altered; uninstall restores everything.

## Install

```sh
./install.sh    # defaults to root@10.11.99.1
```

The installer pulls the prebuilt RM2 binary from GitHub releases, finds an Anthropic/OpenAI **API key** in
`~/.pi/agent/auth.json` (OAuth subscription tokens can't be used outside their harness — if you only have OAuth, it
prompts for a key), and installs a systemd unit `ghostwriter.service` that starts now and on boot.

## Use

WiFi on → write on a page → finger-tap top-right corner. Progress dots appear while it thinks.

- Logs: `ssh root@10.11.99.1 "journalctl -u ghostwriter -f"`
- Model/options: edit `GW_OPTS` in `/home/root/.ghostwriter.env` on the device (e.g. `--model gpt-4o-mini`,
  `--trigger-corner UL`, `--thinking`, `--web-search`), then `systemctl restart ghostwriter`
- Stop/start: `systemctl stop|start ghostwriter`

## Uninstall

```sh
./uninstall.sh
```
