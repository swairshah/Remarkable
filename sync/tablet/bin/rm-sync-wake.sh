#!/bin/sh
# Pull inbound work from the VM after wake. Called (detached) by the
# takeover apps' wifi_heal once wlan0 reports COMPLETED.
#
# Pure pull: web-dropped Papier docs, then rendered daily-notes PDFs
# (the morning import into stock xochitl rides this).

[ -x /home/root/bin/papier-sync.sh ] && /home/root/bin/papier-sync.sh pull
[ -x /home/root/bin/remarkable-notes-pull.sh ] && /home/root/bin/remarkable-notes-pull.sh
exit 0
