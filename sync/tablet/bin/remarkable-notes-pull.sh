#!/bin/sh
# remarkable-notes-pull.sh — pull rendered daily-notes PDFs from the server
# and import finalized ones into the STOCK xochitl app, inside a "notes"
# folder.
#
# Called by remarkable-push-sync.sh after every push (failure tolerated).
# Import policy:
#   - only posts dated BEFORE today (Shelley keeps editing today's post,
#     and re-importing a changing PDF would shift pages under annotations)
#   - only when the manifest hash differs from what was already imported
#   - xochitl is restarted (required for it to see new documents) only
#     when something was actually imported, and never before
#     IMPORT_AFTER_HOUR (default 5) so a midnight writing session is
#     never interrupted.
#
# busybox sh only — no bash, no python on the device.

set -u

# Defaults are the device paths; env overrides exist so the import logic
# can be exercised off-device (see sync/README.md "Test manually").
SERVER="${NOTES_PULL_SERVER:-exedev@remarkable.exe.xyz}"
KEY="/home/root/.ssh/id_sync_dropbear_ed25519"
SSH_BIN="/usr/bin/ssh"
RSYNC_BIN="/usr/bin/rsync"
REMOTE_DIR="/home/exedev/remarkable-exports/notes-pdf/"
STAGE="${NOTES_PULL_STAGE:-/home/root/.local/state/remarkable-sync/notes-pdf}"
STATE="${NOTES_PULL_STATE:-/home/root/.local/state/remarkable-sync/notes-import.state}"  # "<name>.pdf <sha256> <uuid>"
XOCHITL="${NOTES_PULL_XOCHITL:-/home/root/.local/share/remarkable/xochitl}"
FOLDER_NAME="${NOTES_PULL_FOLDER:-notes}"
IMPORT_AFTER_HOUR="${IMPORT_AFTER_HOUR:-5}"
SKIP_PULL="${NOTES_PULL_SKIP_PULL:-0}"      # 1 = import from an existing stage dir
RESTART_CMD="${NOTES_PULL_RESTART_CMD:-systemctl restart xochitl}"

gen_uuid() {
  if [ -r /proc/sys/kernel/random/uuid ]; then
    cat /proc/sys/kernel/random/uuid
  else
    uuidgen | tr 'A-Z' 'a-z'
  fi
}

log() { echo "[$(date)] notes-pull: $*"; }

# Quiet hours: skip entirely (state untouched, so the next daytime sync
# picks everything up).
HOUR="$(date +%H)"; HOUR="${HOUR#0}"; [ -n "$HOUR" ] || HOUR=0
if [ "$HOUR" -lt "$IMPORT_AFTER_HOUR" ]; then
  exit 0
fi

# One successful pull per day is enough — posts finalize daily, and
# yesterday's PDF is always on the server before the quiet hours end. This
# makes the hourly backstop timer and every wake-path call network-free
# after the morning import. NOTES_PULL_FORCE=1 bypasses (manual reruns,
# e.g. after re-rendering an old post server-side).
DAY_MARKER="${NOTES_PULL_DAY_MARKER:-/home/root/.local/state/remarkable-sync/.notes-pull-day}"
if [ "${NOTES_PULL_FORCE:-0}" != 1 ] && \
   [ "$(cat "$DAY_MARKER" 2>/dev/null)" = "$(date +%Y-%m-%d)" ]; then
  exit 0
fi

mkdir -p "$STAGE"
touch "$STATE"

# Pull the rendered PDFs + manifest (no --delete: old PDFs staying around is
# harmless and keeps already-imported sources available).
if [ "$SKIP_PULL" != 1 ]; then
  "$RSYNC_BIN" -az --omit-dir-times --no-perms --no-owner --no-group \
    -e "$SSH_BIN -y -i $KEY" \
    "$SERVER:$REMOTE_DIR" "$STAGE/" || exit 1
  # Heartbeat for the digest page's staleness flag: on idle days no push
  # (and thus no hook) runs, so this daily pull is the only proof of life.
  # Once per day thanks to the day-marker gate above.
  "$SSH_BIN" -y -i "$KEY" "$SERVER" "touch notes/updates/last-sync" 2>/dev/null || true
fi

MANIFEST="$STAGE/manifest.sha256"
[ -f "$MANIFEST" ] || exit 0

TODAY="$(date +%Y-%m-%d | tr -d -)"

# Find (or lazily create) the "notes" folder in xochitl. Folders are just
# metadata entries with type CollectionType.
FOLDER_UUID=""
for m in $(grep -l "\"visibleName\": \"$FOLDER_NAME\"" "$XOCHITL"/*.metadata 2>/dev/null); do
  grep -q '"type": "CollectionType"' "$m" || continue
  grep -q '"deleted": true' "$m" && continue
  FOLDER_UUID="$(basename "$m" .metadata)"
  break
done

NOW_MS="$(date +%s)000"

ensure_folder() {
  [ -n "$FOLDER_UUID" ] && return 0
  FOLDER_UUID="$(gen_uuid)"
  cat > "$XOCHITL/$FOLDER_UUID.metadata" <<EOF
{
    "deleted": false,
    "lastModified": "$NOW_MS",
    "metadatamodified": false,
    "modified": false,
    "parent": "",
    "pinned": false,
    "synced": false,
    "type": "CollectionType",
    "version": 1,
    "visibleName": "$FOLDER_NAME"
}
EOF
  echo '{}' > "$XOCHITL/$FOLDER_UUID.content"
  log "created folder '$FOLDER_NAME' ($FOLDER_UUID)"
}

write_doc_metadata() {
  # $1 = uuid, $2 = visibleName
  cat > "$XOCHITL/$1.metadata" <<EOF
{
    "deleted": false,
    "lastModified": "$NOW_MS",
    "lastOpened": "",
    "lastOpenedPage": 0,
    "metadatamodified": false,
    "modified": false,
    "parent": "$FOLDER_UUID",
    "pinned": false,
    "synced": false,
    "type": "DocumentType",
    "version": 1,
    "visibleName": "$2"
}
EOF
}

IMPORTED=0
while read -r hash name; do
  [ -n "${name:-}" ] || continue
  case "$name" in *.pdf) ;; *) continue ;; esac
  date_part="${name%.pdf}"
  date_num="$(echo "$date_part" | tr -d -)"
  case "$date_num" in *[!0-9]*|'') continue ;; esac
  # Finalized posts only: strictly before today.
  [ "$date_num" -lt "$TODAY" ] || continue
  [ -f "$STAGE/$name" ] || continue

  entry="$(grep "^$name " "$STATE" || true)"
  prev_hash="$(echo "$entry" | awk '{print $2}')"
  prev_uuid="$(echo "$entry" | awk '{print $3}')"

  if [ "$hash" = "$prev_hash" ]; then
    continue
  fi

  if [ -n "$prev_uuid" ] && [ -f "$XOCHITL/$prev_uuid.metadata" ]; then
    # Already imported, content changed (rare — a finalized post was edited):
    # swap the PDF under the same document so no duplicate appears.
    cp "$STAGE/$name" "$XOCHITL/$prev_uuid.pdf"
    uuid="$prev_uuid"
    log "updated $name in place ($uuid)"
  else
    ensure_folder
    uuid="$(gen_uuid)"
    cp "$STAGE/$name" "$XOCHITL/$uuid.pdf"
    cat > "$XOCHITL/$uuid.content" <<EOF
{
    "coverPageNumber": 0,
    "documentMetadata": {},
    "extraMetadata": {},
    "fileType": "pdf",
    "fontName": "",
    "lineHeight": -1,
    "margins": 100,
    "orientation": "portrait",
    "pageCount": 0,
    "pages": [],
    "textScale": 1,
    "transform": {}
}
EOF
    write_doc_metadata "$uuid" "Notes $date_part"
    log "imported $name as 'Notes $date_part' ($uuid)"
  fi

  grep -v "^$name " "$STATE" > "$STATE.tmp" 2>/dev/null || true
  echo "$name $hash $uuid" >> "$STATE.tmp"
  mv "$STATE.tmp" "$STATE"
  IMPORTED=1
done < "$MANIFEST"

# Mark today's pull done only after the manifest was fetched and processed,
# so a failed rsync retries on the next timer fire.
date +%Y-%m-%d > "$DAY_MARKER"

# xochitl scans its store only at startup — restart to surface new imports.
if [ "$IMPORTED" = 1 ]; then
  log "restarting xochitl to pick up imports"
  $RESTART_CMD
fi
