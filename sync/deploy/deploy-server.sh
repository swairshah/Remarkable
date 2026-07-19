#!/usr/bin/env bash
#
# Push server-side files to remarkable.exe.xyz and (re)load nginx.
# Assumes `ssh exedev@remarkable.exe.xyz` works (override with SERVER_HOST=...).
#
# The activity agent ships as a node bundle built here at deploy time
# (bun build server/bin/remarkable-activity-agent.ts); only the .ts is
# committed.
#
# Files shipped:
#   server/bin/*.sh + built remarkable-activity-agent.js -> ~/bin/
#   ../papier/sync/server/{bin,web,systemd} -> Papier viewer + service
#   server/nginx/default.conf -> ~/notes-server/default.conf
#                                -> /etc/nginx/sites-enabled/remarkable
#   server/web/raw/index.html -> ~/notes-server/raw/index.html
#
# Flags:
#   --run   after deploying, trigger a manual post-sync export run
#           (equivalent to: ~/bin/remarkable-post-sync-by-name.sh Notebook)
#
set -euo pipefail

HOST="${SERVER_HOST:-exedev@remarkable.exe.xyz}"
DOC_NAME="${DOC_NAME:-Notebook}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
PAPIER_SERVER="$HERE/../papier/sync/server"

RUN_EXPORT=0
for arg in "$@"; do
  case "$arg" in
    --run) RUN_EXPORT=1 ;;
    *) echo "unknown flag: $arg" >&2; exit 2 ;;
  esac
done

command -v bun >/dev/null 2>&1 || { echo "[deploy-server] bun is required to build the activity agent (https://bun.sh)" >&2; exit 1; }

echo "[deploy-server] target=$HOST"

echo "[deploy-server] build activity agent bundle"
BUILD_DIR="$(mktemp -d)"
trap 'rm -rf "$BUILD_DIR"' EXIT
bun build "$HERE/server/bin/remarkable-activity-agent.ts" \
  --target=node --format=cjs \
  --outfile "$BUILD_DIR/remarkable-activity-agent.js" >/dev/null

echo "[deploy-server] migrate legacy alt-ui dirs/services -> papier (one-time, idempotent)"
ssh "$HOST" '
  set -e
  [ -d ~/remarkable-backup/alt-ui ]         && [ ! -d ~/remarkable-backup/papier ]         && mv ~/remarkable-backup/alt-ui ~/remarkable-backup/papier || true
  [ -d ~/remarkable-backup/alt-ui-inbound ] && [ ! -d ~/remarkable-backup/papier-inbound ] && mv ~/remarkable-backup/alt-ui-inbound ~/remarkable-backup/papier-inbound || true
  [ -d ~/notes-server/alt-ui ]              && [ ! -d ~/notes-server/papier ]              && mv ~/notes-server/alt-ui ~/notes-server/papier || true
  sudo systemctl disable --now alt-ui-upload 2>/dev/null || true
  sudo rm -f /etc/systemd/system/alt-ui-upload.service
  sudo systemctl daemon-reload
  rm -f ~/bin/alt-ui-upload.js ~/bin/alt-ui-library.js ~/bin/alt-ui-preview-page.py ~/bin/alt-ui-render.sh ~/bin/alt-ui-compose.sh ~/bin/alt-ui-make-pdf.py ~/notes-server/alt-ui-upload.service
'

ssh "$HOST" 'mkdir -p ~/bin ~/notes-server/raw ~/notes-server/notebook ~/notes-server/papier ~/notes/updates ~/notes/notes ~/remarkable-backup/xochitl ~/remarkable-backup/notebook-app ~/remarkable-backup/papier ~/remarkable-backup/papier-inbound ~/remarkable-exports/notes-pdf'

echo "[deploy-server] scp server/bin scripts + agent bundle"
scp -q "$HERE"/server/bin/*.sh "$HERE"/server/bin/notebook-live-relay.js "$BUILD_DIR/remarkable-activity-agent.js" "$HOST:bin/"
ssh "$HOST" 'chmod +x ~/bin/remarkable-post-sync.sh ~/bin/remarkable-post-sync-by-name.sh ~/bin/remarkable-activity-agent-hook.sh ~/bin/notebook-live-ingest.sh ~/bin/notes-md2pdf.sh ~/bin/notes-pdf-export.sh 2>/dev/null || true'

echo "[deploy-server] scp Papier viewer + library/upload service"
scp -q "$PAPIER_SERVER"/bin/papier-upload.js "$PAPIER_SERVER"/bin/papier-library.js \
  "$PAPIER_SERVER"/bin/papier-preview-page.py "$PAPIER_SERVER"/bin/papier-render.sh \
  "$PAPIER_SERVER"/bin/papier-compose.sh "$PAPIER_SERVER"/bin/papier-make-pdf.py "$HOST:bin/"
scp -q "$PAPIER_SERVER"/web/index.html "$HOST:notes-server/papier/index.html"
ssh "$HOST" 'chmod +x ~/bin/papier-preview-page.py ~/bin/papier-render.sh ~/bin/papier-compose.sh ~/bin/papier-make-pdf.py'

echo "[deploy-server] ensure runtime deps (node, img2pdf, imagemagick, pandoc, chromium, fonts)"
ssh "$HOST" '
  set -e
  missing=""
  command -v node     >/dev/null 2>&1 || missing="$missing nodejs"
  command -v img2pdf  >/dev/null 2>&1 || missing="$missing img2pdf"
  command -v convert  >/dev/null 2>&1 || missing="$missing imagemagick"
  command -v pandoc   >/dev/null 2>&1 || missing="$missing pandoc"
  command -v fc-cache >/dev/null 2>&1 || missing="$missing fontconfig"
  # math glyphs for Chrome MathML rendering
  fc-list 2>/dev/null | grep -qi "stix\|latinmodern" || missing="$missing fonts-stix fonts-lmodern"
  if [ -n "$missing" ]; then
    sudo apt-get update -y >/dev/null
    sudo apt-get install -y $missing >/dev/null
  fi
  # headless chrome for the notes PDF renderer (.deb, not snap — snapd is
  # not set up on the VM and the deb pulls its own deps via apt)
  if ! command -v google-chrome >/dev/null 2>&1 && ! command -v chromium >/dev/null 2>&1 && ! command -v chromium-browser >/dev/null 2>&1; then
    curl -fsSL -o /tmp/google-chrome.deb https://dl.google.com/linux/direct/google-chrome-stable_current_amd64.deb
    sudo apt-get install -y /tmp/google-chrome.deb >/dev/null
    rm -f /tmp/google-chrome.deb
  fi
'

echo "[deploy-server] ship PDF fonts (Reader, EB Garamond, Google Sans Code)"
FONT_SRC="$HOME/Library/Fonts"
READER_SRC="$FONT_SRC"
[[ -f "$HOME/Desktop/Reader-Fonts/Reader-Regular.ttf" ]] && READER_SRC="$HOME/Desktop/Reader-Fonts"
font_files=()
for f in "$READER_SRC"/Reader-*.ttf "$FONT_SRC"/GoogleSansCode-*.ttf "$FONT_SRC"/EBGaramond-*.ttf; do
  [[ -f "$f" ]] && font_files+=("$f")
done
if [[ ${#font_files[@]} -gt 0 ]]; then
  ssh "$HOST" 'mkdir -p ~/.local/share/fonts'
  scp -q "${font_files[@]}" "$HOST:.local/share/fonts/"
  ssh "$HOST" 'fc-cache -f >/dev/null 2>&1 || true'
else
  echo "  (no local font TTFs found — the renderer will fall back to system serif)"
fi

echo "[deploy-server] ensure STIX Two Math (darker MathML glyphs for e-ink)"
ssh "$HOST" '
  set -e
  if [ ! -f ~/.local/share/fonts/STIXTwoMath-Regular.otf ]; then
    mkdir -p ~/.local/share/fonts
    curl -fsSL -o /tmp/stix2-otf.zip https://mirrors.ctan.org/fonts/stix2-otf.zip
    command -v unzip >/dev/null || sudo apt-get install -y unzip >/dev/null 2>&1
    unzip -o -j -q /tmp/stix2-otf.zip "stix2-otf/STIXTwoMath-Regular.otf" -d ~/.local/share/fonts
    rm -f /tmp/stix2-otf.zip
    fc-cache -f >/dev/null 2>&1 || true
  fi
'

echo "[deploy-server] ensure pdf.js prebuilt viewer"
ssh "$HOST" '
  set -e
  V=4.8.69
  if [ ! -f ~/notes-server/pdfjs/.version-$V ]; then
    curl -fsSL -o /tmp/pdfjs.zip https://github.com/mozilla/pdf.js/releases/download/v$V/pdfjs-$V-dist.zip
    command -v unzip >/dev/null || sudo apt-get install -y unzip >/dev/null 2>&1
    rm -rf ~/notes-server/pdfjs && mkdir -p ~/notes-server/pdfjs
    unzip -q /tmp/pdfjs.zip -d ~/notes-server/pdfjs
    rm /tmp/pdfjs.zip && touch ~/notes-server/pdfjs/.version-$V
  fi
'

echo "[deploy-server] scp nginx config + viewer html + nav"
scp -q "$HERE"/server/nginx/default.conf "$HOST:notes-server/default.conf"
scp -q "$HERE"/server/web/raw/index.html "$HOST:notes-server/raw/index.html"
scp -q "$HERE"/server/web/notebook/index.html "$HOST:notes-server/notebook/index.html"
scp -q "$HERE"/server/web/nav.js "$HOST:notes-server/nav.js"

echo "[deploy-server] scp shelley AGENTS.md"
ssh "$HOST" 'mkdir -p ~/.config/shelley'
scp -q "$HERE"/server/shelley/AGENTS.md "$HOST:.config/shelley/AGENTS.md"

echo "[deploy-server] install live relay service"
scp -q "$HERE"/server/systemd/notebook-live-relay.service "$HOST:notes-server/notebook-live-relay.service"
scp -q "$PAPIER_SERVER"/systemd/papier-upload.service "$HOST:notes-server/papier-upload.service"
ssh "$HOST" '
  set -e
  sudo install -m 644 ~/notes-server/notebook-live-relay.service /etc/systemd/system/notebook-live-relay.service
  sudo install -m 644 ~/notes-server/papier-upload.service /etc/systemd/system/papier-upload.service
  sudo systemctl daemon-reload
  sudo systemctl enable --now notebook-live-relay >/dev/null 2>&1
  sudo systemctl restart notebook-live-relay
  sudo systemctl enable --now papier-upload >/dev/null 2>&1
  sudo systemctl restart papier-upload
'

echo "[deploy-server] install nginx site + reload"
ssh "$HOST" '
  set -e
  sudo install -m 644 ~/notes-server/default.conf /etc/nginx/sites-available/remarkable
  sudo ln -sf /etc/nginx/sites-available/remarkable /etc/nginx/sites-enabled/remarkable
  sudo rm -f /etc/nginx/sites-enabled/default
  # nginx (www-data) must be able to traverse the home dir to reach content;
  # the notebook-app mirror is served directly, so it needs read too
  chmod o+x "$HOME" "$HOME/remarkable-backup"
  chmod -R o+rX "$HOME/remarkable-backup/notebook-app"
  sudo nginx -t
  sudo systemctl enable --now nginx >/dev/null 2>&1
  sudo systemctl reload nginx
'

if [[ "$RUN_EXPORT" -eq 1 ]]; then
  echo "[deploy-server] running post-sync export for doc='$DOC_NAME'"
  ssh "$HOST" "~/bin/remarkable-post-sync-by-name.sh '$DOC_NAME'"
fi

echo "[deploy-server] done"
