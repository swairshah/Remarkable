#!/usr/bin/env bash
set -euo pipefail

HOST="${1:-root@10.11.99.1}"
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CACHE="$DIR/.cache"
GW_URL="https://github.com/awwaiid/ghostwriter/releases/latest/download/ghostwriter-rm2"
DEFAULT_MODEL="claude-sonnet-4-5"
SSH_OPTS=(-o ConnectTimeout=8)

step() { printf '\n\033[1m==> %s\033[0m\n' "$*"; }
rsh() { ssh "${SSH_OPTS[@]}" "$HOST" "$@"; }

step "Checking SSH connection to $HOST"
rsh true || { echo "Cannot ssh to $HOST."; exit 1; }
ARCH=$(rsh uname -m)
[ "$ARCH" = "armv7l" ] || { echo "Expected armv7l (reMarkable 2), got '$ARCH' - aborting."; exit 1; }

step "Downloading ghostwriter-rm2 (latest release)"
mkdir -p "$CACHE"
curl -fL --progress-bar -o "$CACHE/ghostwriter-rm2.part" "$GW_URL"
mv "$CACHE/ghostwriter-rm2.part" "$CACHE/ghostwriter-rm2"

step "Looking for API keys (in ~/.pi/agent/auth.json, then env)"
AUTH="$HOME/.pi/agent/auth.json"
ANT_KEY=""
OAI_KEY=""
if [ -f "$AUTH" ] && command -v python3 >/dev/null; then
  KEYS=$(python3 - "$AUTH" <<'PY' || true
import json, re, sys
data = json.load(open(sys.argv[1]))
ant, oai = "", ""
def walk(o, path):
    global ant, oai
    if isinstance(o, dict):
        for k, v in o.items():
            walk(v, path + [str(k).lower()])
    elif isinstance(o, list):
        for v in o:
            walk(v, path)
    elif isinstance(o, str):
        if re.match(r"^sk-ant-api[\w-]+$", o):
            ant = ant or o
        elif re.match(r"^sk-[\w-]{30,}$", o) and not o.startswith("sk-ant") and any("openai" in p for p in path):
            oai = oai or o
walk(data, [])
print(ant)
print(oai)
PY
)
  ANT_KEY=$(printf '%s\n' "$KEYS" | sed -n 1p)
  OAI_KEY=$(printf '%s\n' "$KEYS" | sed -n 2p)
fi
[ -n "$ANT_KEY" ] || ANT_KEY="${ANTHROPIC_API_KEY:-}"
[ -n "$OAI_KEY" ] || OAI_KEY="${OPENAI_API_KEY:-}"

if [ -n "$ANT_KEY" ]; then
  echo "found Anthropic API key (${ANT_KEY:0:14}...)"
else
  echo "No Anthropic API key found in ~/.pi (your pi auth is likely OAuth-only, which only the pi harness itself may use)."
  printf 'Paste an Anthropic API key (sk-ant-api...), or press Enter to skip: '
  read -r ANT_KEY
fi
[ -n "$OAI_KEY" ] && echo "found OpenAI API key (${OAI_KEY:0:10}...)"

step "Installing binary to /home/root/opt/ghostwriter"
rsh 'mkdir -p /home/root/opt/ghostwriter /home/root/bin'
cat "$CACHE/ghostwriter-rm2" | rsh 'cat > /home/root/opt/ghostwriter/ghostwriter && chmod +x /home/root/opt/ghostwriter/ghostwriter'
rsh '/home/root/opt/ghostwriter/ghostwriter --help >/dev/null 2>&1' && echo "binary runs OK" || { echo "binary failed to execute on device - report back"; exit 1; }

step "Writing /home/root/.ghostwriter.env and launcher"
{
  [ -n "$ANT_KEY" ] && printf 'export ANTHROPIC_API_KEY=%s\n' "$ANT_KEY"
  [ -n "$OAI_KEY" ] && printf 'export OPENAI_API_KEY=%s\n' "$OAI_KEY"
  printf 'export GW_OPTS="--model %s"\n' "$DEFAULT_MODEL"
} | rsh 'cat > /home/root/.ghostwriter.env && chmod 600 /home/root/.ghostwriter.env'

printf '%s\n' \
  '#!/bin/sh' \
  '[ -f /home/root/.ghostwriter.env ] && . /home/root/.ghostwriter.env' \
  'exec /home/root/opt/ghostwriter/ghostwriter $GW_OPTS "$@"' \
  | rsh 'cat > /home/root/bin/gw && chmod +x /home/root/bin/gw'

step "Installing systemd service (starts now + on boot)"
printf '%s\n' \
  '[Unit]' \
  'Description=ghostwriter (handwriting -> vision LLM)' \
  'After=network-online.target home.mount' \
  '' \
  '[Service]' \
  'ExecStart=/home/root/bin/gw' \
  'Restart=on-failure' \
  'RestartSec=5' \
  'WorkingDirectory=/home/root' \
  '' \
  '[Install]' \
  'WantedBy=multi-user.target' \
  | rsh 'cat > /etc/systemd/system/ghostwriter.service'

if [ -n "$ANT_KEY" ]; then
  rsh 'systemctl daemon-reload && systemctl enable --now ghostwriter'
  sleep 2
  rsh 'systemctl is-active ghostwriter' || { echo "service not active, recent log:"; rsh 'journalctl -u ghostwriter -n 20 --no-pager'; exit 1; }
  echo "service is active"
else
  rsh 'systemctl daemon-reload'
  echo "SKIPPED enabling the service (no API key). Add one to /home/root/.ghostwriter.env on the device,"
  echo "then: ssh $HOST 'systemctl enable --now ghostwriter'"
fi

printf '\n\033[1mDone.\033[0m Usage:\n'
printf '  1. WiFi on, open any notebook page, write a question with the pen\n'
printf '  2. Tap the TOP-RIGHT corner of the screen with a finger\n'
printf '  3. Watch the reply get written/typed onto the page\n\n'
printf 'Logs:   ssh %s "journalctl -u ghostwriter -f"\n' "$HOST"
printf 'Config: /home/root/.ghostwriter.env (model via GW_OPTS)\n'
