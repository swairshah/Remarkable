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

ssh "$HOST" 'mkdir -p ~/bin ~/notes-server/raw ~/notes/updates ~/remarkable-backup/xochitl ~/remarkable-exports'

echo "[deploy-server] scp server/bin scripts + agent bundle"
scp -q "$HERE"/server/bin/*.sh "$BUILD_DIR/remarkable-activity-agent.js" "$HOST:bin/"
ssh "$HOST" 'chmod +x ~/bin/remarkable-post-sync.sh ~/bin/remarkable-post-sync-by-name.sh ~/bin/remarkable-activity-agent-hook.sh 2>/dev/null || true'

echo "[deploy-server] ensure runtime deps (node, img2pdf, imagemagick)"
ssh "$HOST" '
  set -e
  missing=""
  command -v node    >/dev/null 2>&1 || missing="$missing nodejs"
  command -v img2pdf >/dev/null 2>&1 || missing="$missing img2pdf"
  command -v convert >/dev/null 2>&1 || missing="$missing imagemagick"
  if [ -n "$missing" ]; then
    sudo apt-get update -y >/dev/null
    sudo apt-get install -y $missing >/dev/null
  fi
'

echo "[deploy-server] scp nginx config + viewer html"
scp -q "$HERE"/server/nginx/default.conf "$HOST:notes-server/default.conf"
scp -q "$HERE"/server/web/raw/index.html "$HOST:notes-server/raw/index.html"

echo "[deploy-server] scp shelley AGENTS.md"
ssh "$HOST" 'mkdir -p ~/.config/shelley'
scp -q "$HERE"/server/shelley/AGENTS.md "$HOST:.config/shelley/AGENTS.md"

echo "[deploy-server] install nginx site + reload"
ssh "$HOST" '
  set -e
  sudo install -m 644 ~/notes-server/default.conf /etc/nginx/sites-available/remarkable
  sudo ln -sf /etc/nginx/sites-available/remarkable /etc/nginx/sites-enabled/remarkable
  sudo rm -f /etc/nginx/sites-enabled/default
  # nginx (www-data) must be able to traverse the home dir to reach content
  chmod o+x "$HOME"
  sudo nginx -t
  sudo systemctl enable --now nginx >/dev/null 2>&1
  sudo systemctl reload nginx
'

if [[ "$RUN_EXPORT" -eq 1 ]]; then
  echo "[deploy-server] running post-sync export for doc='$DOC_NAME'"
  ssh "$HOST" "~/bin/remarkable-post-sync-by-name.sh '$DOC_NAME'"
fi

echo "[deploy-server] done"
