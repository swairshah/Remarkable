#!/usr/bin/env bash
set -euo pipefail

HOST="${1:-root@10.11.99.1}"
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CACHE="$DIR/.cache"
XOVI_URL="https://github.com/asivery/rm-xovi-extensions/releases/download/v19-23052026/xovi-arm32.tar.gz"
XOVI_SHA="9aa00537ad41e9be0c3151992bfc25106465318cf5bb4c41cf59b3ddd4866377"
LITERM_URL="https://github.com/asivery/rm-literm/releases/download/v0.1.6/literm-arm32.so"
LITERM_SHA="b908e5445ca4a0390f51c64041810128b1aa4a38d9f5541a01ca101e7bbef69a"
SSH_OPTS=(-o ConnectTimeout=8)

step() { printf '\n\033[1m==> %s\033[0m\n' "$*"; }
rsh() { ssh "${SSH_OPTS[@]}" "$HOST" "$@"; }
fetch() {
  local url="$1" sha="$2" out="$3"
  if [ ! -f "$out" ]; then
    curl -fL --progress-bar -o "$out.part" "$url"
    mv "$out.part" "$out"
  fi
  echo "$sha  $out" | shasum -a 256 -c - >/dev/null || { echo "checksum mismatch for $out"; exit 1; }
}

echo "This loads XOVI + literm into xochitl at runtime (tethered: any reboot returns"
echo "the tablet to fully stock; nothing on the boot path is modified)."
if [ "${2:-}" != "-y" ]; then
  printf 'Continue? [y/N] '
  read -r ANS
  [ "$ANS" = "y" ] || [ "$ANS" = "Y" ] || exit 0
fi

step "Checking SSH connection to $HOST"
rsh true || { echo "Cannot ssh to $HOST."; exit 1; }
ARCH=$(rsh uname -m)
[ "$ARCH" = "armv7l" ] || { echo "Expected armv7l (reMarkable 2), got '$ARCH' - aborting."; exit 1; }
OSVER=$(rsh 'grep -hs REMARKABLE_RELEASE_VERSION /usr/share/remarkable/update.conf 2>/dev/null | head -n1 | cut -d= -f2' || echo unknown)
echo "firmware: ${OSVER:-unknown}"

step "Checking the tablet has internet (needed for hashtab rebuild)"
if ! rsh 'ping -c1 -W3 8.8.8.8 >/dev/null 2>&1'; then
  echo "WARNING: tablet appears offline. Turn WiFi on before continuing; the hashtab step may fail without it."
fi

step "Downloading xovi-arm32.tar.gz + literm-arm32.so (checksummed)"
mkdir -p "$CACHE"
fetch "$XOVI_URL" "$XOVI_SHA" "$CACHE/xovi-arm32.tar.gz"
fetch "$LITERM_URL" "$LITERM_SHA" "$CACHE/literm-arm32.so"

step "Installing XOVI to /home/root/xovi"
cat "$CACHE/xovi-arm32.tar.gz" | rsh 'cat > /tmp/xovi.tar.gz && tar xzf /tmp/xovi.tar.gz -C /home/root && rm /tmp/xovi.tar.gz'
rsh 'test -x /home/root/xovi/start' || { echo "xovi/start missing after extract - aborting"; exit 1; }

step "Installing literm extension"
cat "$CACHE/literm-arm32.so" | rsh 'cat > /home/root/xovi/extensions.d/literm.so'

step "Rebuilding hashtab (takes a while, needed for UI mods)"
rsh '/home/root/xovi/rebuild_hashtable'

step "Starting XOVI (the tablet UI will restart, ~30s)"
rsh '/home/root/xovi/start' || true
sleep 5
rsh 'systemctl is-active xochitl' || { echo "xochitl not back up yet; give it a moment, or check: ssh '"$HOST"' journalctl -u xochitl -n 30"; }

printf '\n\033[1mDone.\033[0m\n'
printf 'Open the terminal from xochitl'"'"'s menu (literm adds a Terminal entry; tap the\n'
printf 'top-right corner inside it for the settings/keyboard toolbar), then run: pi\n'
printf '(if PATH is not picked up: /home/root/bin/pi)\n\n'
printf 'Tethered by design:\n'
printf '  after any reboot the tablet is stock again; bring it back with:  ssh %s /home/root/xovi/start\n' "$HOST"
printf '  back to stock without reboot:                                    ssh %s /home/root/xovi/stock\n' "$HOST"
printf 'For starting XOVI from the tablet alone (no computer), see xovi-tripletap in the README.\n'
