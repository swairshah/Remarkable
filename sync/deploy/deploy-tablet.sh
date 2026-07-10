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

# Paper's web-sync script rides along (its old 90s timer is retired —
# the apps now sync on edit/sleep/wake via rm-sync-flush/rm-sync-wake)
ALT_UI_SYNC="$HERE/../alt-ui/sync/tablet/bin/alt-ui-sync.sh"
if [ -f "$ALT_UI_SYNC" ]; then
  echo "[deploy-tablet] scp alt-ui-sync.sh"
  scp -q "$ALT_UI_SYNC" "$HOST:/home/root/bin/"
fi

echo "[deploy-tablet] scp systemd units"
scp -q "$SRC"/systemd/*.service "$SRC"/systemd/*.timer "$HOST:/etc/systemd/system/"

echo "[deploy-tablet] chmod + daemon-reload + enable timer"
ssh "$HOST" '
  set -e
  chmod 700 /home/root/bin/*.sh
  # retire the old 90s alt-ui timer (sync is event-driven now)
  systemctl disable --now alt-ui-sync.timer 2>/dev/null || true
  rm -f /etc/systemd/system/alt-ui-sync.timer /etc/systemd/system/alt-ui-sync.service
  systemctl daemon-reload
  systemctl enable --now remarkable-push-sync.timer
  systemctl restart remarkable-push-sync.timer
  systemctl enable --now remarkable-notes-pull.timer
  systemctl restart remarkable-notes-pull.timer
  systemctl list-timers --all | grep remarkable || true
'

echo "[deploy-tablet] done"
