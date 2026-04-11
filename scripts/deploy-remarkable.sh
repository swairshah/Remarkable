#!/usr/bin/env bash
#
# Push reMarkable-device files to the tablet and (re)load the systemd timer.
# Assumes `ssh remarkable` works (override with REMARKABLE_HOST=...).
#
# Files shipped:
#   remarkable/bin/*.sh          -> /home/root/bin/
#   remarkable/systemd/*.service -> /etc/systemd/system/
#   remarkable/systemd/*.timer   -> /etc/systemd/system/
#
set -euo pipefail

HOST="${REMARKABLE_HOST:-remarkable}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
SRC="$HERE/remarkable"

echo "[deploy-remarkable] target=$HOST"

ssh "$HOST" 'mkdir -p /home/root/bin'

echo "[deploy-remarkable] scp bin scripts"
scp -q "$SRC"/bin/*.sh "$HOST:/home/root/bin/"

echo "[deploy-remarkable] scp systemd units"
scp -q "$SRC"/systemd/*.service "$SRC"/systemd/*.timer "$HOST:/etc/systemd/system/"

echo "[deploy-remarkable] chmod + daemon-reload + enable timer"
ssh "$HOST" '
  set -e
  chmod 700 /home/root/bin/*.sh
  systemctl daemon-reload
  systemctl enable --now remarkable-push-sync.timer
  systemctl restart remarkable-push-sync.timer
  systemctl list-timers --all | grep remarkable-push-sync || true
'

echo "[deploy-remarkable] done"
