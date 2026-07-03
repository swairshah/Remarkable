#!/usr/bin/env bash
set -euo pipefail

# Removes xovi + AppLoad + yaft + the pi icon from the tablet.
# Leaves the pi harness (/home/root/bin/pi, node, ~/.pi) alone -
# use ../pi-harness/uninstall.sh for that.

HOST="${1:-root@10.11.99.1}"
SSH_OPTS=(-o ConnectTimeout=8)
rsh() { ssh "${SSH_OPTS[@]}" "$HOST" "$@"; }

echo "==> Reverting xochitl to stock"
rsh '/home/root/xovi/stock 2>/dev/null || true'

echo "==> Removing boot service"
rsh 'systemctl disable xovi-boot 2>/dev/null; rm -f /etc/systemd/system/xovi-boot.service; systemctl daemon-reload'

echo "==> Removing files"
rsh 'rm -rf /home/root/xovi /home/root/xovi.bak /home/root/shims /home/root/opt/yaft /home/root/.terminfo/y/yaft-256color'

echo "==> Restarting xochitl"
rsh 'systemctl restart xochitl'

echo "Done. pi itself (pi-harness) is still installed."
