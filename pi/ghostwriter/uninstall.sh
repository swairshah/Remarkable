#!/usr/bin/env bash
set -euo pipefail
HOST="${1:-root@10.11.99.1}"
ssh "$HOST" 'systemctl disable --now ghostwriter 2>/dev/null; rm -f /etc/systemd/system/ghostwriter.service /home/root/bin/gw /home/root/.ghostwriter.env; rm -rf /home/root/opt/ghostwriter; systemctl daemon-reload'
echo "ghostwriter removed from device"
