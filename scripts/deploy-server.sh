#!/usr/bin/env bash
#
# Push server-side files to swair.dev and reload nginx in the notes_app container.
# Assumes `ssh swair@swair.dev` works (override with SERVER_HOST=...).
#
# Files shipped:
#   server/bin/*.sh, *.py, *.js, *.ts -> ~/bin/
#   server/nginx/default.conf          -> ~/notes-server/default.conf
#   server/web/raw/index.html          -> ~/notes-server/raw/index.html
#
# Flags:
#   --run   after deploying, trigger a manual post-sync export run
#           (equivalent to: ~/bin/remarkable-post-sync-by-name.sh Notebook)
#
set -euo pipefail

HOST="${SERVER_HOST:-swair@swair.dev}"
DOC_NAME="${DOC_NAME:-Notebook}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"

RUN_EXPORT=0
for arg in "$@"; do
  case "$arg" in
    --run) RUN_EXPORT=1 ;;
    *) echo "unknown flag: $arg" >&2; exit 2 ;;
  esac
done

echo "[deploy-server] target=$HOST"

ssh "$HOST" 'mkdir -p ~/bin ~/notes-server/raw'

echo "[deploy-server] scp server/bin scripts"
scp -q "$HERE"/server/bin/*.sh "$HERE"/server/bin/*.py "$HERE"/server/bin/*.js "$HERE"/server/bin/*.ts "$HOST:bin/"
ssh "$HOST" 'chmod +x ~/bin/remarkable-post-sync.sh ~/bin/remarkable-post-sync-by-name.sh ~/bin/remarkable-activity-diff.py ~/bin/remarkable-activity-agent-hook.sh 2>/dev/null || true'

echo "[deploy-server] ensure node runtime present"
ssh "$HOST" 'command -v node >/dev/null 2>&1 || (sudo apt-get update -y >/dev/null && sudo apt-get install -y nodejs >/dev/null)'

echo "[deploy-server] scp nginx config"
scp -q "$HERE"/server/nginx/default.conf "$HOST:notes-server/default.conf"

echo "[deploy-server] scp viewer html"
scp -q "$HERE"/server/web/raw/index.html "$HOST:notes-server/raw/index.html"

echo "[deploy-server] nginx -t && nginx -s reload (inside notes_app)"
if ssh "$HOST" 'docker ps --format "{{.Names}}" | grep -qx notes_app'; then
  ssh "$HOST" 'docker exec notes_app nginx -t && docker exec notes_app nginx -s reload'
else
  echo "[deploy-server] WARN: notes_app container not running. See README for one-time docker run."
fi

if [[ "$RUN_EXPORT" -eq 1 ]]; then
  echo "[deploy-server] running post-sync export for doc='$DOC_NAME'"
  ssh "$HOST" "~/bin/remarkable-post-sync-by-name.sh '$DOC_NAME'"
fi

echo "[deploy-server] done"
