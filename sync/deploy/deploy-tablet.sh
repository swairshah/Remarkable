#!/usr/bin/env bash
#
# Push tablet-side files to the reMarkable and (re)load the systemd timer.
# Assumes `ssh remarkable` works (override with REMARKABLE_HOST=...).
#
# Files shipped:
#   tablet/bin/*.sh          -> /home/root/bin/
#   tablet/systemd/*.service -> /etc/systemd/system/
#   tablet/systemd/*.timer   -> /etc/systemd/system/
#
set -euo pipefail

HOST="${REMARKABLE_HOST:-remarkable}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
SRC="$HERE/tablet"

echo "[deploy-tablet] target=$HOST"

ssh "$HOST" 'mkdir -p /home/root/bin'

echo "[deploy-tablet] scp bin scripts"
scp -q "$SRC"/bin/*.sh "$HOST:/home/root/bin/"

echo "[deploy-tablet] scp systemd units"
scp -q "$SRC"/systemd/*.service "$SRC"/systemd/*.timer "$HOST:/etc/systemd/system/"

echo "[deploy-tablet] chmod + daemon-reload + enable timer"
ssh "$HOST" '
  set -e
  chmod 700 /home/root/bin/*.sh
  systemctl daemon-reload
  systemctl enable --now remarkable-push-sync.timer
  systemctl restart remarkable-push-sync.timer
  systemctl list-timers --all | grep remarkable-push-sync || true
'

echo "[deploy-tablet] done"
