#!/bin/sh
# papier two-way web sync (tablet side). Event-driven — no timer:
#
#   on edit  (Papier, debounced)  -> both     pull inbound + push mirror
#   on sleep (rm-sync-flush.sh)  -> push     mirror only, radio then rests
#   on wake  (rm-sync-wake.sh)   -> pull     inbound only
#
#   usage: papier-sync.sh [pull|push|both]     (default: both)
#
#   PULL: VM inbound docs  -> tablet docs/   (add-only; consumes inbound)
#   PUSH: tablet data       -> VM mirror     (--delete: the tablet is truth)
#
# The web viewer reads the mirror + the inbound area. A web-dropped doc
# first shows from inbound ("syncing to tablet…"); the pull moves it onto
# the device, then the next push mirrors it, so it stays visible and drops
# the "syncing" tag. Uploaded docs get fresh ids, so the add-only pull never
# clobbers a doc you're editing. The app re-scans its home screen, so a
# pulled doc appears on its own.

MODE="${1:-both}"

KEY=/home/root/.ssh/id_sync_dropbear_ed25519
VM=exedev@remarkable.exe.xyz
# dropbear ssh: -y accepts+saves the host key; it doesn't take -o options
SSH="ssh -y -i $KEY"
LOCAL=/home/root/.local/share/papier
INBOUND=/home/exedev/remarkable-backup/papier-inbound
MIRROR=/home/exedev/remarkable-backup/papier
LOG=/home/root/.local/state/papier-sync.log
DELETE_QUEUE=/home/root/.local/state/papier-delete-queue
LOCAL_TRASH=/home/root/.local/share/papier-trash

mkdir -p "$(dirname "$LOG")" "$LOCAL/docs" "$DELETE_QUEUE" "$LOCAL_TRASH"
log() { echo "$(date '+%Y-%m-%d %H:%M:%S') $*" >> "$LOG"; }
# keep the log bounded
[ -f "$LOG" ] && [ "$(wc -l < "$LOG")" -gt 2000 ] && tail -n 1000 "$LOG" > "$LOG.t" && mv "$LOG.t" "$LOG"

apply_deletions() {
    # Deletion markers are retained on the VM until a later push proves the
    # tablet removed the document. Move locally rather than purging so an
    # accidental delete can still be recovered by hand.
    if rsync -az --omit-dir-times --no-perms \
         -e "$SSH" "$VM:$INBOUND/deletions/" "$DELETE_QUEUE/" >> "$LOG" 2>&1; then
        for marker in "$DELETE_QUEUE"/*; do
            [ -f "$marker" ] || continue
            id=${marker##*/}
            case "$id" in
                ''|*[!a-z0-9_-]*) log "ignored invalid delete marker: $id"; continue ;;
            esac
            if [ -d "$LOCAL/docs/$id" ]; then
                stamp=$(date '+%Y%m%d-%H%M%S')
                mv "$LOCAL/docs/$id" "$LOCAL_TRASH/$stamp-$id"
                log "moved deleted doc to trash: $id"
            fi
        done
    else
        log "delete queue pull failed"
    fi
}

ack_deletions() {
    # Only acknowledge after the --delete mirror push succeeds. Until then
    # the VM tombstone keeps the stale mirrored bundle out of web/iOS lists.
    for marker in "$DELETE_QUEUE"/*; do
        [ -f "$marker" ] || continue
        id=${marker##*/}
        case "$id" in ''|*[!a-z0-9_-]*) continue ;; esac
        if $SSH "$VM" "rm -f $INBOUND/deletions/$id" >> "$LOG" 2>&1; then
            rm -f "$marker"
            log "acknowledged delete: $id"
        fi
    done
}

do_pull() {
    # bring web-dropped docs onto the tablet, consuming them on the VM
    if rsync -az --remove-source-files --omit-dir-times --no-perms \
         -e "$SSH" "$VM:$INBOUND/docs/" "$LOCAL/docs/" >> "$LOG" 2>&1; then
        log "pull ok ($MODE)"
        # prune the now-empty inbound dirs on the VM
        $SSH "$VM" "find $INBOUND/docs -mindepth 1 -type d -empty -delete 2>/dev/null" >> "$LOG" 2>&1 || true
    else
        log "pull failed (nothing to pull, or VM unreachable)"
    fi
}

do_push() {
    # mirror the tablet's papier data to the web (true mirror)
    if rsync -az --delete --omit-dir-times --no-perms --no-owner --no-group \
         --exclude '*.tmp' --exclude 'sessions' \
         -e "$SSH" "$LOCAL/" "$VM:$MIRROR/" >> "$LOG" 2>&1; then
        log "push ok ($MODE)"
        ack_deletions
    else
        log "push failed"
    fi
}

case "$MODE" in
    pull) apply_deletions; do_pull ;;
    push) apply_deletions; do_push ;;
    both) apply_deletions; do_pull; do_push ;;
    *)    echo "usage: $0 [pull|push|both]" >&2; exit 2 ;;
esac
