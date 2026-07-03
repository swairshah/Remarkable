#!/usr/bin/env bash
set -uo pipefail
HOST="${1:-root@10.11.99.1}"
ssh -o ConnectTimeout=8 "$HOST" '
echo "== arch ==";      uname -a
echo "== os ==";        grep -hs REMARKABLE_RELEASE_VERSION /usr/share/remarkable/update.conf; cat /etc/version 2>/dev/null
echo "== glibc ==";     ldd --version 2>&1 | head -n1
echo "== disk ==";      df -h /home /
echo "== mem ==";       free -m 2>/dev/null || cat /proc/meminfo | head -3
echo "== date ==";      date
echo "== xochitl ==";   systemctl is-active xochitl
echo "== inputs ==";    grep -E "Name|Handlers" /proc/bus/input/devices
echo "== pi ==";        /home/root/bin/pi --version 2>/dev/null || echo "not installed"
echo "== gw ==";        systemctl is-active ghostwriter 2>/dev/null || echo "not installed"
'
