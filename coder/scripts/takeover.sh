#!/bin/bash
# Launch coder in full-takeover mode on the reMarkable 2: stop xochitl,
# host the e-ink engine with the bundled rm2fb server (timower/rM2-stuff),
# run coder against it (instant ink), ALWAYS restore xochitl on exit.
#
# Exit coder: top-edge swipe -> CLOSE, or SIGTERM; the power button
# sleeps instead. Escape hatch if anything wedges:
# ssh root@<tablet> 'systemctl start xochitl'.

SERVER_PID=

restore() {
    if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
        kill -INT "$SERVER_PID" 2>/dev/null   # SIGINT = its clean shutdown
        for _ in 1 2 3 4 5 6 7 8 9 10; do
            kill -0 "$SERVER_PID" 2>/dev/null || break
            sleep 0.3
        done
        kill -9 "$SERVER_PID" 2>/dev/null
    fi
    systemctl start xochitl
}
trap restore EXIT INT TERM

HERE=$(cd "$(dirname "$0")" && pwd)

systemctl stop xochitl
sleep 0.5

# The bundled server dlopens the vendor libqsgepaper.so and hosts the panel.
LD_LIBRARY_PATH="$HERE" "$HERE/rm2fb_server" >/tmp/rm2fb.log 2>&1 &
SERVER_PID=$!

# Wait for its control socket (init takes a moment: waveform table load).
for _ in $(seq 1 100); do
    [ -S /var/run/rm2fb.sock ] && break
    kill -0 "$SERVER_PID" 2>/dev/null || { echo "rm2fb server died, see /tmp/rm2fb.log"; exit 1; }
    sleep 0.1
done
sleep 0.5

# pi lives in /home/root/bin and needs node on PATH (pi-harness install).
# CODER_EXT points pi at the canvas-tools extension shipped next to the
# binary; the app passes it to pi with -e.
# CODER_FONT is the default face for pi's writing: serif (formal roman),
# script (natural cursive handwriting), or sans (plain plotter). pi can
# still pick per element with font-family in its SVGs.
# CODER_VM / CODER_VM_DIR: how pi reaches the machine holding the git
# clones (defaults live in pi_rpc.rs: the tablet's sync identity to the
# exe.dev VM, repos in /home/exedev/coder).
[ -f /home/root/.config/coder/env ] && . /home/root/.config/coder/env

cd /home/root
# node's default old-space cap (~400MB on this arm build) is what pi's
# session images kept hitting; give it headroom (the rM2 has 1GB, xochitl
# is stopped while we run)
HOME=/home/root PATH="/home/root/bin:/home/root/opt/node/bin:/usr/bin:/bin:/usr/sbin:/sbin" \
    NODE_OPTIONS="--max-old-space-size=640" \
    CODER_EXT="$HERE/coder-canvas.ts:$HERE/coder-context.ts" \
    CODER_FONT="${CODER_FONT:-serif}" \
    CODER_VM="${CODER_VM:-}" \
    CODER_VM_DIR="${CODER_VM_DIR:-}" \
    "$HERE/coder" >>/tmp/coder.log 2>&1
echo "coder-takeover: closed ($?), restoring xochitl"
