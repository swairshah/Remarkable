#!/bin/bash
# Launch Papier in full-takeover mode on the reMarkable 2: stop xochitl,
# host the e-ink engine with the bundled rm2fb server (timower/rM2-stuff),
# run Papier against it (instant ink), ALWAYS restore xochitl on exit.
#
# Exit papier: top-edge swipe -> CLOSE, or SIGTERM; the power button
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
# PAPIER_EXT points pi at the extensions shipped next to the binary
# (colon-separated; the app passes each to pi with -e): canvas tools +
# per-turn metrics (latency/tokens/payload -> ~/.local/share/papier/metrics.jsonl).
# PAPIER_FONT is the default face for pi's writing: serif (formal roman),
# script (natural cursive handwriting), or sans (plain plotter).
# PAPIER_FONT_DIR holds the device-extracted UI fonts (make fonts), optional.
cd /home/root
HOME=/home/root PATH="/home/root/bin:/home/root/opt/node/bin:/usr/bin:/bin:/usr/sbin:/sbin" \
    PAPIER_EXT="$HERE/papier-canvas.ts:$HERE/papier-transcribe.ts:$HERE/papier-metrics.ts" \
    PAPIER_FONT="${PAPIER_FONT:-serif}" \
    PAPIER_FONT_DIR="${PAPIER_FONT_DIR:-$HERE/fonts}" \
    "$HERE/papier" >>/tmp/papier.log 2>&1
echo "papier-takeover: closed ($?), restoring xochitl"
