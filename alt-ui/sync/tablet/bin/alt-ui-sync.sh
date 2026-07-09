#!/bin/sh
# alt-ui two-way web sync (tablet side). Runs on a timer.
#
#   PULL: VM inbound docs  -> tablet docs/   (add-only; consumes inbound)
#   PUSH: tablet data       -> VM mirror     (--delete: the tablet is truth)
#
# The web viewer reads the mirror + the inbound area. A web-dropped doc
# first shows from inbound ("syncing to tablet…"); this pull moves it onto
# the device, then the push mirrors it, so it stays visible and drops the
# "syncing" tag. Uploaded docs get fresh ids, so the add-only pull never
# clobbers a doc you're editing. The app re-scans its home screen, so a
# pulled doc appears on its own.

KEY=/home/root/.ssh/id_sync_dropbear_ed25519
VM=exedev@remarkable.exe.xyz
# dropbear ssh: -y accepts+saves the host key; it doesn't take -o options
SSH="ssh -y -i $KEY"
LOCAL=/home/root/.local/share/alt-ui
INBOUND=/home/exedev/remarkable-backup/alt-ui-inbound
MIRROR=/home/exedev/remarkable-backup/alt-ui
LOG=/home/root/.local/state/alt-ui-sync.log

mkdir -p "$(dirname "$LOG")" "$LOCAL/docs"
log() { echo "$(date '+%Y-%m-%d %H:%M:%S') $*" >> "$LOG"; }
# keep the log bounded
[ -f "$LOG" ] && [ "$(wc -l < "$LOG")" -gt 2000 ] && tail -n 1000 "$LOG" > "$LOG.t" && mv "$LOG.t" "$LOG"

# 1. PULL: bring web-dropped docs onto the tablet, consuming them on the VM
if rsync -az --remove-source-files --omit-dir-times --no-perms \
     -e "$SSH" "$VM:$INBOUND/docs/" "$LOCAL/docs/" >> "$LOG" 2>&1; then
    log "pull ok"
    # prune the now-empty inbound dirs on the VM
    $SSH "$VM" "find $INBOUND/docs -mindepth 1 -type d -empty -delete 2>/dev/null" >> "$LOG" 2>&1 || true
else
    log "pull failed (nothing to pull, or VM unreachable)"
fi

# 2. PUSH: mirror the tablet's alt-ui data to the web (true mirror)
if rsync -az --delete --omit-dir-times --no-perms --no-owner --no-group \
     --exclude '*.tmp' --exclude 'sessions' \
     -e "$SSH" "$LOCAL/" "$VM:$MIRROR/" >> "$LOG" 2>&1; then
    log "push ok"
else
    log "push failed"
fi
