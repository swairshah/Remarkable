#!/usr/bin/env bash
set -euo pipefail
HOST="${1:-root@10.11.99.1}"
ssh "$HOST" '/home/root/xovi/stock 2>/dev/null || true; rm -rf /home/root/xovi'
echo "XOVI + literm removed; tablet is stock (a reboot alone also always returns it to stock)"
