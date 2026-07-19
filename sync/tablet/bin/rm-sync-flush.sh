#!/bin/sh
# Flush local changes to the VM. Called by the takeover apps right before
# suspend (bounded by the caller — power::sync_flush kills the process
# group at its deadline), and harmless to run any time by hand.
#
# Pure push: no pulls here — wake handles those (rm-sync-wake.sh).

[ -x /home/root/bin/papier-sync.sh ] && /home/root/bin/papier-sync.sh push
[ -x /home/root/bin/remarkable-push-sync.sh ] && /home/root/bin/remarkable-push-sync.sh
exit 0
