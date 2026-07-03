#!/usr/bin/env bash
set -euo pipefail
HOST="${1:-root@10.11.99.1}"
ssh "$HOST" 'rm -rf /home/root/opt/pi /home/root/opt/node /home/root/bin/pi'
if [ "${2:-}" = "--purge-auth" ]; then
  ssh "$HOST" 'rm -rf /home/root/.pi'
  echo "removed /home/root/.pi too"
fi
echo "pi + node removed from device (PATH lines in .profile/.bashrc left in place, harmless)"
