#!/bin/sh
# AppLoad entry point for takeover mode. AppLoad runs this inside xochitl's
# world, which is about to be stopped — so detach the real launch into a
# transient systemd unit (PID-1-owned, survives xochitl) and exit immediately.
HERE=$(cd "$(dirname "$0")" && pwd)
systemctl is-active --quiet reader-takeover && exit 0
systemd-run --unit=reader-takeover --collect /bin/bash "$HERE/takeover.sh"
exit 0
