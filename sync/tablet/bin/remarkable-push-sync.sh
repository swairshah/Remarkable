#!/bin/sh
set -eu

SRC="/home/root/.local/share/remarkable/xochitl/"
DEST_USER="exedev"
DEST_HOST="remarkable.exe.xyz"
DEST_DIR="/home/exedev/remarkable-backup/xochitl/"

# notebook app data (pages, library, sessions, AGENT.md) -> /notebook/ viewer
NB_SRC="/home/root/.local/share/notebook/"
NB_DEST="/home/exedev/remarkable-backup/notebook-app/"
KEY="/home/root/.ssh/id_sync_dropbear_ed25519"
SSH_BIN="/usr/bin/ssh"
RSYNC_BIN="/usr/bin/rsync"
LOG_DIR="/home/root/.local/state/remarkable-sync"
LOG_FILE="$LOG_DIR/push.log"

# Post-sync hook on server
HOOK_CMD="/home/exedev/bin/remarkable-post-sync.sh auto Notebook"

mkdir -p "$LOG_DIR"

# Short-circuit: if nothing changed since the last successful push, don't
# touch the network at all (the timer is only a backstop now — the real
# triggers are edit/sleep/wake, and a clean backstop run must be free).
STAMP="$LOG_DIR/.last-push"
if [ -f "$STAMP" ] && \
   [ -z "$(find "$SRC" "$NB_SRC" -newer "$STAMP" -print 2>/dev/null | head -n 1)" ]; then
  exit 0
fi

# keep the log bounded
if [ -f "$LOG_FILE" ] && [ "$(wc -l < "$LOG_FILE")" -gt 2000 ]; then
  tail -n 1000 "$LOG_FILE" > "$LOG_FILE.trim" && mv "$LOG_FILE.trim" "$LOG_FILE"
fi

echo "[$(date)] sync start" >> "$LOG_FILE"

# Ensure destinations exist
"$SSH_BIN" -y -i "$KEY" "${DEST_USER}@${DEST_HOST}" "mkdir -p '$DEST_DIR' '$NB_DEST'"

# Diff-based sync (after first baseline, only changed/new/deleted files transfer)
RC=0
OUT=$("$RSYNC_BIN" -az --delete --stats \
  --omit-dir-times --no-perms --no-owner --no-group \
  -e "$SSH_BIN -y -i $KEY" \
  "$SRC" "${DEST_USER}@${DEST_HOST}:${DEST_DIR}" 2>&1) || RC=$?
echo "$OUT" | grep -E "transferred:|Total bytes sent" >> "$LOG_FILE" || echo "$OUT" >> "$LOG_FILE"
[ "$RC" -eq 0 ]

# notebook app mirror (tolerated on failure — the xochitl sync still counts).
# *.tmp excluded: the app writes page.json.tmp then renames, and a sweep
# mid-save would ship a torn file.
if [ -d "$NB_SRC" ]; then
  NB_OUT=$("$RSYNC_BIN" -az --delete --stats \
    --omit-dir-times --no-perms --no-owner --no-group \
    --exclude '*.tmp' \
    -e "$SSH_BIN -y -i $KEY" \
    "$NB_SRC" "${DEST_USER}@${DEST_HOST}:${NB_DEST}" 2>&1) || true
  echo "$NB_OUT" | grep -E "transferred:|Total bytes sent" >> "$LOG_FILE" || true
fi

# both trees pushed (xochitl strictly, notebook best-effort) — mark the round
touch "$STAMP"

# Run remote post-sync hook
"$SSH_BIN" -y -i "$KEY" "${DEST_USER}@${DEST_HOST}" "$HOOK_CMD" >> "$LOG_FILE" 2>&1 || true

# (remarkable-notes-pull.sh moved to the wake path — rm-sync-wake.sh —
# so a pure push stays a pure push)

echo "[$(date)] sync done" >> "$LOG_FILE"
