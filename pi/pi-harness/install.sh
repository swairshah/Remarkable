#!/usr/bin/env bash
set -euo pipefail

HOST="${1:-root@10.11.99.1}"
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PAYLOAD="$DIR/payload/pi-0.80.3-linux-any.tar.gz"
NODE_VERSION="v22.22.3"
NODE_TARBALL="node-${NODE_VERSION}-linux-armv6l.tar.gz"
NODE_URL="https://unofficial-builds.nodejs.org/download/release/${NODE_VERSION}/${NODE_TARBALL}"
# Node needs libatomic.so.1, which the reMarkable OS doesn't ship. Debian's armhf build works.
LIBATOMIC_DEB="libatomic1_12.2.0-14+deb12u1_armhf.deb"
LIBATOMIC_URL="http://deb.debian.org/debian/pool/main/g/gcc-12/${LIBATOMIC_DEB}"
CACHE="$DIR/.cache"
SSH_OPTS=(-o ConnectTimeout=8)

step() { printf '\n\033[1m==> %s\033[0m\n' "$*"; }
rsh() { ssh "${SSH_OPTS[@]}" "$HOST" "$@"; }

[ -f "$PAYLOAD" ] || { echo "payload missing: $PAYLOAD"; exit 1; }

step "Checking SSH connection to $HOST"
rsh true || { echo "Cannot ssh to $HOST. Tablet plugged in, awake, and USB connection enabled?"; exit 1; }

step "Probing device"
ARCH=$(rsh uname -m)
GLIBC=$(rsh 'ldd --version 2>&1 | head -n1' || true)
OSVER=$(rsh 'grep -hs REMARKABLE_RELEASE_VERSION /usr/share/remarkable/update.conf 2>/dev/null | head -n1 | cut -d= -f2' || true)
[ -n "$OSVER" ] || OSVER=$(rsh 'cat /etc/version 2>/dev/null' || echo unknown)
FREE_MB=$(rsh "df -m /home | awk 'NR==2{print \$4}'")
DEV_TIME=$(rsh date +%s); MAC_TIME=$(date +%s)
echo "arch:    $ARCH"
echo "os:      $OSVER"
echo "libc:    $GLIBC"
echo "free:    ${FREE_MB} MB on /home"

[ "$ARCH" = "armv7l" ] || { echo "Expected armv7l (reMarkable 2), got '$ARCH' - aborting."; exit 1; }
[ "$FREE_MB" -ge 450 ] || { echo "Need ~450 MB free on /home, only ${FREE_MB} MB - aborting."; exit 1; }

DRIFT=$(( DEV_TIME > MAC_TIME ? DEV_TIME - MAC_TIME : MAC_TIME - DEV_TIME ))
if [ "$DRIFT" -ge 300 ]; then
  echo "WARNING: device clock off by ${DRIFT}s - TLS will fail. Fixing..."
  rsh "date -s @$(date +%s)" >/dev/null || echo "  (could not set clock, fix manually)"
fi

GLIBC_MINOR=$(printf '%s' "$GLIBC" | grep -o '2\.[0-9][0-9]*' | head -n1 | cut -d. -f2 || true)
if [ -n "${GLIBC_MINOR:-}" ] && [ "$GLIBC_MINOR" -lt 28 ]; then
  echo "WARNING: glibc ($GLIBC) is older than 2.28; Node 22 may not start. If the node check below fails, report back."
fi

step "Downloading Node ${NODE_VERSION} (linux-armv6l) to local cache"
mkdir -p "$CACHE"
if [ ! -f "$CACHE/$NODE_TARBALL" ]; then
  curl -fL --progress-bar -o "$CACHE/$NODE_TARBALL.part" "$NODE_URL"
  mv "$CACHE/$NODE_TARBALL.part" "$CACHE/$NODE_TARBALL"
else
  echo "cached: $CACHE/$NODE_TARBALL"
fi

step "Installing Node runtime to /home/root/opt/node"
rsh 'rm -rf /home/root/opt/node.new && mkdir -p /home/root/opt/node.new'
cat "$CACHE/$NODE_TARBALL" | rsh 'tar xzf - -C /home/root/opt/node.new'
rsh 'rm -rf /home/root/opt/node && mv /home/root/opt/node.new/node-* /home/root/opt/node && rm -rf /home/root/opt/node.new'

step "Installing libatomic (missing on the reMarkable OS)"
if [ ! -f "$CACHE/libatomic.so.1" ]; then
  curl -fL --progress-bar -o "$CACHE/$LIBATOMIC_DEB" "$LIBATOMIC_URL"
  ( cd "$CACHE" && ar x "$LIBATOMIC_DEB" && tar xf data.tar.* ./usr/lib/arm-linux-gnueabihf/libatomic.so.1.2.0 \
    && mv usr/lib/arm-linux-gnueabihf/libatomic.so.1.2.0 libatomic.so.1 \
    && rm -rf usr debian-binary control.tar.* data.tar.* "$LIBATOMIC_DEB" )
fi
cat "$CACHE/libatomic.so.1" | rsh 'mkdir -p /home/root/opt/node/lib && cat > /home/root/opt/node/lib/libatomic.so.1'

step "Verifying node runs on the device"
NODE_V=$(rsh 'LD_LIBRARY_PATH=/home/root/opt/node/lib /home/root/opt/node/bin/node --version') || {
  echo "node binary failed to run on the device (likely glibc/arch issue)."
  echo "Report this output back and we will fall back to Node 20 + pi legacy-node20."
  exit 1
}
echo "node on device: $NODE_V"

step "Installing pi 0.80.3 to /home/root/opt/pi"
rsh 'rm -rf /home/root/opt/pi && mkdir -p /home/root/opt/pi'
cat "$PAYLOAD" | rsh 'tar xzf - -C /home/root/opt/pi'

step "Creating /home/root/bin/pi wrapper + PATH"
printf '%s\n' \
  '#!/bin/sh' \
  'export PATH="/home/root/opt/node/bin:/home/root/bin:$PATH"' \
  'export LD_LIBRARY_PATH="/home/root/opt/node/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"' \
  'exec /home/root/opt/node/bin/node --max-old-space-size=384 /home/root/opt/pi/node_modules/@earendil-works/pi-coding-agent/dist/cli.js "$@"' \
  | rsh 'mkdir -p /home/root/bin && cat > /home/root/bin/pi && chmod +x /home/root/bin/pi'
rsh 'for f in /home/root/.profile /home/root/.bashrc; do grep -qs "home/root/bin" "$f" 2>/dev/null || { echo "export PATH=/home/root/bin:/home/root/opt/node/bin:\$PATH"; echo "export LD_LIBRARY_PATH=/home/root/opt/node/lib\${LD_LIBRARY_PATH:+:\$LD_LIBRARY_PATH}"; } >> "$f"; done'

step "Copying ~/.pi auth + settings from this machine"
COPIED=0
for f in auth.json models.json; do
  if [ -f "$HOME/.pi/agent/$f" ]; then
    cat "$HOME/.pi/agent/$f" | rsh "mkdir -p /home/root/.pi/agent && cat > /home/root/.pi/agent/$f && chmod 600 /home/root/.pi/agent/$f"
    echo "copied: ~/.pi/agent/$f"
    COPIED=1
  fi
done
# settings.json is sanitized, not copied verbatim: Mac packages/extensions
# (git:/npm:/local paths) don't exist on the tablet and crash pi at startup
if [ -f "$HOME/.pi/agent/settings.json" ]; then
  python3 - "$HOME/.pi/agent/settings.json" <<'PY' | rsh 'mkdir -p /home/root/.pi/agent && cat > /home/root/.pi/agent/settings.json && chmod 600 /home/root/.pi/agent/settings.json'
import json, sys
s = json.load(open(sys.argv[1]))
s["packages"] = []
s["extensions"] = []
print(json.dumps(s, indent=2))
PY
  echo "copied: ~/.pi/agent/settings.json (packages/extensions stripped for the tablet)"
  COPIED=1
fi
[ "$COPIED" = 1 ] || echo "NOTE: no ~/.pi/agent/auth.json found locally; run /login inside pi on the device instead."

step "Smoke test"
rsh '/home/root/bin/pi --version'

printf '\n\033[1mDone.\033[0m Run it with:\n\n  ssh -t %s pi\n\nNotes:\n- The tablet needs WiFi on to reach the model APIs (USB alone is not internet for the device).\n- If the TUI renders oddly: ssh -t %s "TERM=xterm pi"\n' "$HOST" "$HOST"
