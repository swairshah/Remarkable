#!/bin/sh
set -eu

SRC="/home/root/.local/share/remarkable/xochitl/"
DEST_USER="swair"
DEST_HOST="swair.dev"
DEST_DIR="/home/swair/remarkable-backup/xochitl/"
KEY="/home/root/.ssh/id_sync_dropbear_ed25519"
SSH_BIN="/usr/bin/ssh"
RSYNC_BIN="/usr/bin/rsync"
LOG_DIR="/home/root/.local/state/remarkable-sync"
LOG_FILE="$LOG_DIR/push.log"

# Post-sync hook on server
HOOK_CMD="/home/swair/bin/remarkable-post-sync.sh auto Notebook"

mkdir -p "$LOG_DIR"

echo "[$(date)] sync start" >> "$LOG_FILE"

# Ensure destination exists
"$SSH_BIN" -y -i "$KEY" "${DEST_USER}@${DEST_HOST}" "mkdir -p '$DEST_DIR'"

# Diff-based sync (after first baseline, only changed/new/deleted files transfer)
"$RSYNC_BIN" -az --delete \
  --omit-dir-times --no-perms --no-owner --no-group \
  -e "$SSH_BIN -y -i $KEY" \
  "$SRC" "${DEST_USER}@${DEST_HOST}:${DEST_DIR}" \
  >> "$LOG_FILE" 2>&1

# Run remote post-sync hook
"$SSH_BIN" -y -i "$KEY" "${DEST_USER}@${DEST_HOST}" "$HOOK_CMD" >> "$LOG_FILE" 2>&1 || true

echo "[$(date)] sync done" >> "$LOG_FILE"
