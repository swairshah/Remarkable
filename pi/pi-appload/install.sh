#!/usr/bin/env bash
set -euo pipefail

# Installs a "pi" icon into the reMarkable 2 UI (xochitl) that opens the pi
# coding agent in an on-screen terminal.
#
# Stack: xovi (LD_PRELOAD extension loader for xochitl)
#        + qt-resource-rebuilder (patches xochitl QML at runtime)
#        + AppLoad (launcher menu with icons inside xochitl)
#        + yaft (framebuffer terminal with on-screen keyboard, running
#          under AppLoad's qtfb shim, which emulates an rM1 framebuffer)
#        -> pi (installed separately by ../pi-harness/install.sh)

HOST="${1:-root@10.11.99.1}"
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CACHE="$DIR/.cache"
SSH_OPTS=(-o ConnectTimeout=8)

XOVI_EXT_URL="https://github.com/asivery/rm-xovi-extensions/releases/download/v19-23052026/xovi-arm32.tar.gz"
# yaft: patched build from timower/rM2-stuff (see assets/yaft-tablet.patch) with
# font-scale + padding config options and AppLoad pen/touch tap dedup.
# payload/ also carries its terminfo and libevdev.so.2 (Debian armhf), which
# the reMarkable OS doesn't ship.
# AppLoad's QML hooks are tied to the xochitl version (see appload.qmd history):
#   OS 3.24.x -> v0.4.1   OS 3.26.x -> v0.5.0/v0.5.1   OS 3.27+ -> v0.5.3
appload_version_for_os() {
    case "$1" in
        3.2[0-4].*) echo "v0.4.1" ;;
        3.2[5-6].*) echo "v0.5.1" ;;
        *)          echo "v0.5.3" ;;
    esac
}

step() { printf '\n\033[1m==> %s\033[0m\n' "$*"; }
rsh() { ssh "${SSH_OPTS[@]}" "$HOST" "$@"; }
fetch() { # fetch <url> <dest>
    [ -f "$2" ] && { echo "cached: $2"; return; }
    curl -fL --progress-bar -o "$2.part" "$1" && mv "$2.part" "$2"
}

step "Checking SSH connection to $HOST"
rsh true || { echo "Cannot ssh to $HOST. Tablet awake and reachable?"; exit 1; }

step "Probing device"
ARCH=$(rsh uname -m)
OSVER=$(rsh 'grep -hs REMARKABLE_RELEASE_VERSION /usr/share/remarkable/update.conf 2>/dev/null | head -n1 | cut -d= -f2' || echo unknown)
FREE_MB=$(rsh "df -m /home | awk 'NR==2{print \$4}'")
echo "arch: $ARCH   os: $OSVER   free: ${FREE_MB} MB"
[ "$ARCH" = "armv7l" ] || { echo "Expected armv7l (reMarkable 2), got '$ARCH' - aborting."; exit 1; }
case "$OSVER" in
    3.*) ;;
    *) echo "WARNING: built and tested against OS 3.24; you are on '$OSVER'. Continuing anyway." ;;
esac

step "Checking pi on the device"
if rsh 'test -x /home/root/bin/pi'; then
    echo "pi already installed: $(rsh '/home/root/bin/pi --version 2>/dev/null' || echo '?')"
else
    echo "pi not installed - running pi-harness installer first"
    "$DIR/../pi-harness/install.sh" "$HOST"
fi

step "Downloading components to local cache"
APPLOAD_VER=$(appload_version_for_os "$OSVER")
APPLOAD_URL="https://github.com/asivery/rm-appload/releases/download/$APPLOAD_VER/appload-arm32.zip"
echo "AppLoad $APPLOAD_VER for OS $OSVER"
mkdir -p "$CACHE"
fetch "$XOVI_EXT_URL" "$CACHE/xovi-ext-arm32.tar.gz"
fetch "$APPLOAD_URL" "$CACHE/appload-$APPLOAD_VER-arm32.zip"

step "Extracting appload locally"
STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT
unzip -o -q "$CACHE/appload-$APPLOAD_VER-arm32.zip" -d "$STAGE/appload"
# zip layout varies between releases (flat vs shims/ subdir) - locate the files
APPLOAD_SO=$(find "$STAGE/appload" -name appload.so | head -n1)
QTFB_SHIM=$(find "$STAGE/appload" -name qtfb-shim.so | head -n1)
QTFB_SHIM32=$(find "$STAGE/appload" -name qtfb-shim-32bit.so | head -n1)
[ -f "$DIR/payload/yaft" ] || { echo "payload/yaft missing (patched build - see README)"; exit 1; }
[ -n "$APPLOAD_SO" ] && [ -n "$QTFB_SHIM" ] || { echo "appload extraction failed"; exit 1; }

step "Installing xovi tree to /home/root/xovi"
# Preserve coexisting xovi content (e.g. rm-hacks qmd patches, extra
# extensions): timestamped backup that is never overwritten, then seed the
# new tree from the old one and extract our bundle over it - only files we
# ship get replaced, foreign files stay in place.
if rsh 'test -d /home/root/xovi'; then
    rsh 'BAK="/home/root/xovi.bak.$(date +%s)" && mv /home/root/xovi "$BAK" && cp -a "$BAK" /home/root/xovi && echo "backed up existing tree to $BAK (foreign files preserved in place)"'
fi
cat "$CACHE/xovi-ext-arm32.tar.gz" | rsh 'tar xzf - -C /home/root'
rsh 'chmod +x /home/root/xovi/start /home/root/xovi/stock /home/root/xovi/debug /home/root/xovi/rebuild_hashtable /home/root/xovi/xovi.so 2>/dev/null || true'

step "Installing appload extension + qtfb shims"
cat "$APPLOAD_SO" | rsh 'cat > /home/root/xovi/extensions.d/appload.so && chmod +x /home/root/xovi/extensions.d/appload.so'
rsh 'mkdir -p /home/root/shims'
cat "$QTFB_SHIM" | rsh 'cat > /home/root/shims/qtfb-shim.so'
[ -n "$QTFB_SHIM32" ] && cat "$QTFB_SHIM32" | rsh 'cat > /home/root/shims/qtfb-shim-32bit.so'

step "Installing yaft terminal (patched: font-scale, padding, tap dedup)"
rsh 'mkdir -p /home/root/opt/yaft/lib /home/root/.terminfo/y /home/root/.config/yaft'
cat "$DIR/payload/yaft" | rsh 'cat > /home/root/opt/yaft/yaft && chmod +x /home/root/opt/yaft/yaft'
cat "$DIR/payload/libevdev.so.2" | rsh 'cat > /home/root/opt/yaft/lib/libevdev.so.2'
cat "$DIR/payload/yaft-256color" | rsh 'cat > /home/root/.terminfo/y/yaft-256color'
cat "$DIR/assets/yaft-config.toml" | rsh 'cat > /home/root/.config/yaft/config.toml'

step "Installing the pi app (icon + manifest + session script)"
rsh 'mkdir -p /home/root/xovi/exthome/appload/pi'
for f in external.manifest.json icon.png pi-session.sh; do
    cat "$DIR/assets/$f" | rsh "cat > /home/root/xovi/exthome/appload/pi/$f"
done
rsh 'chmod +x /home/root/xovi/exthome/appload/pi/pi-session.sh'

step "Building qmldiff hashtable (xochitl restarts once; takes ~1 min)"
rsh 'bash -s' <<'REMOTE'
set -uo pipefail
export XOVI_ROOT="/tmp/xovi-hashtab"
xovi=/home/root/xovi
hashtab=$xovi/exthome/qt-resource-rebuilder/hashtab

systemctl stop xochitl 2>/dev/null || true
pid=$(pidof xochitl 2>/dev/null) && kill -15 $pid 2>/dev/null

rm -rf "$XOVI_ROOT"
mkdir -p "$XOVI_ROOT/extensions.d"
ln -s "$xovi/extensions.d/qt-resource-rebuilder.so" "$XOVI_ROOT/extensions.d/"
mkdir -p "$xovi/exthome/qt-resource-rebuilder"
rm -f "$hashtab"

# busybox has no `timeout`: run xochitl in the background and poll its log
log=/tmp/hashtab-build.log
rm -f "$log"
QMLDIFF_HASHTAB_CREATE="$hashtab" \
QML_DISABLE_DISK_CACHE=1 \
LD_PRELOAD="$xovi/xovi.so" \
/usr/bin/xochitl > "$log" 2>&1 &
XPID=$!

waited=0
while [ "$waited" -lt 180 ]; do
    grep -q "Hashtab saved to" "$log" 2>/dev/null && break
    kill -0 "$XPID" 2>/dev/null || break
    sleep 2
    waited=$((waited + 2))
done

kill -15 "$XPID" 2>/dev/null
pid=$(pidof xochitl 2>/dev/null) && kill -15 $pid 2>/dev/null
sleep 2

rm -rf "$XOVI_ROOT"
if [ ! -s "$hashtab" ]; then
    echo "HASHTAB-FAILED"
    systemctl start xochitl
    exit 1
fi
echo "HASHTAB-OK"
REMOTE

step "Starting xovi (restarts xochitl with extensions)"
rsh '/home/root/xovi/start'
sleep 6
rsh 'systemctl is-active xochitl' || { echo "xochitl did not come back - check: ssh $HOST journalctl -u xochitl -n 50"; exit 1; }
echo "xochitl active"

step "Recent xochitl log (xovi/appload lines)"
rsh 'journalctl -u xochitl -n 200 --no-pager 2>/dev/null | grep -iE "xovi|appload|qmldiff" | tail -n 12' || true

step "Enabling xovi at boot (xovi-boot.service)"
cat "$DIR/assets/xovi-boot.service" | rsh 'cat > /etc/systemd/system/xovi-boot.service'

step "Installing library-refresh units (docs dropped by pi appear when the terminal closes)"
cat "$DIR/assets/pi-rm-refresh.service" | rsh 'cat > /etc/systemd/system/pi-rm-refresh.service'
cat "$DIR/assets/pi-rm-refresh.path" | rsh 'cat > /etc/systemd/system/pi-rm-refresh.path'
rsh 'systemctl daemon-reload && systemctl enable xovi-boot >/dev/null 2>&1 && systemctl enable --now pi-rm-refresh.path >/dev/null 2>&1'

printf '\n\033[1mDone.\033[0m On the tablet:\n'
printf '  1. WiFi on (pi needs internet at runtime).\n'
printf '  2. Open the AppLoad menu in the xochitl sidebar (new entry in the hamburger menu).\n'
printf '  3. Tap the pi icon: fullscreen terminal + on-screen keyboard, running pi.\n'
printf '     (Long-press the icon to open it as a window instead.)\n'
printf '  4. Close a fullscreen app: drag one finger from the top-center of the screen to the center.\n'
printf '     Hide/show keyboard: long-press Esc in yaft.\n\n'
printf 'Disable everything temporarily:  ssh %s /home/root/xovi/stock\n' "$HOST"
printf 'Re-enable:                       ssh %s /home/root/xovi/start\n' "$HOST"
printf 'Full uninstall:                  ./uninstall.sh %s\n' "$HOST"
