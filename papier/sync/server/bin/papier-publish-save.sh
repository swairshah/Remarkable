#!/usr/bin/env bash
# papier-publish-save.sh <jobdir> — save one website editor revision and
# publish that exact revision to swair.dev.
#
# The job dir is prepared by papier-upload.js:
#   slug.txt       existing post slug
#   post.md        complete replacement Markdown (including frontmatter)
#   status.txt     phase string, polled by the website editor
#   outcome.txt    published | unchanged
#   url.txt        public post URL
#
# Env matches papier-publish.sh: PAPIER_BACKUP, PAPIER_PY, PUBLISH_REPO,
# PUBLISH_OUT, PUBLISH_TARGET, PUBLISH_KEY, PUBLISH_SITE_URL,
# PUBLISH_SITE, PUBLISH_NO_PUSH=1.
set -euo pipefail

JOB="${1:?usage: papier-publish-save.sh <jobdir>}"
BACKUP="${PAPIER_BACKUP:-$HOME/remarkable-backup}"
REPO="${PUBLISH_REPO:-$BACKUP/papier-publish/site}"
OUT="${PUBLISH_OUT:-$BACKUP/papier-publish/out}"
TARGET="${PUBLISH_TARGET:-swair@swair.dev:/}"
KEY="${PUBLISH_KEY:-$HOME/.ssh/id_papier_publish}"
SITE_URL="${PUBLISH_SITE_URL:-https://swair.dev}"
PY="${PAPIER_PY:-$HOME/papier-venv/bin/python3}"
[ -x "$PY" ] || PY=python3
SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
SITE_PY="${PUBLISH_SITE:-$SCRIPT_DIR/papier-publish-site.py}"
[ -f "$SITE_PY" ] || SITE_PY="$HOME/bin/papier-publish-site.py"
GIT=(git -c user.name=papier -c user.email=papier@localhost)

status() { printf '%s' "$1" > "$JOB/status.txt"; echo "[website] $1" >&2; }
die() { status "failed: $1"; echo "papier-website: $1" >&2; exit 1; }

# The service runs under systemd without a login env; pick up publish config.
if [ -f "$HOME/.env" ]; then set +u; set -a; . "$HOME/.env"; set +a; set -u; fi

SLUG="$(tr -d '[:space:]' < "$JOB/slug.txt")"
printf '%s' "$SLUG" | grep -Eq '^[a-z0-9]([a-z0-9-]{0,78}[a-z0-9])?$' || die "bad post slug"
[ -d "$REPO/.git" ] || die "publish repository does not exist"
DST="$REPO/posts/$SLUG"
[ -f "$DST/post.md" ] || die "unknown post"
[ -f "$JOB/post.md" ] || die "missing post content"
[ -z "$(git -C "$REPO" status --porcelain)" ] || die "publish repository is not clean"

# Validate the same contract the notebook publisher gives its agent.
"$PY" - "$JOB/post.md" <<'PY'
import re, sys
text = open(sys.argv[1], encoding="utf-8").read()
if len(text.encode("utf-8")) > 750_000:
    raise SystemExit("post is too large")
match = re.match(r"^---\r?\n([\s\S]*?)\r?\n---(?:\r?\n|$)", text)
if not match or not re.search(r"^title:\s*\S.*$", match.group(1), re.M):
    raise SystemExit("post needs YAML title frontmatter")
PY

if cmp -s "$JOB/post.md" "$DST/post.md"; then
  printf 'unchanged' > "$JOB/outcome.txt"
  printf '%s/posts/%s/' "${SITE_URL%/}" "$SLUG" > "$JOB/url.txt"
  status "nothing changed"
  exit 0
fi

rollback_repo() {
  local rc=$?
  if [ "$rc" -ne 0 ]; then
    git -C "$REPO" reset --hard -q HEAD 2>/dev/null || true
    git -C "$REPO" clean -fdq 2>/dev/null || true
  fi
  exit "$rc"
}
trap rollback_repo EXIT

status "saving $SLUG"
cp "$JOB/post.md" "$DST/post.md"
NOW="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
"$PY" - "$DST/post.md" "$DST/meta.json" "$SLUG" "$NOW" <<'PY'
import json, re, sys
post, meta_path, slug, now = sys.argv[1:]
text = open(post, encoding="utf-8").read()
frontmatter = re.match(r"^---\r?\n([\s\S]*?)\r?\n---", text).group(1)
raw = re.search(r"^title:\s*(.*?)\s*$", frontmatter, re.M).group(1)
try:
    title = json.loads(raw) if raw.startswith('"') else raw.strip("'")
except (ValueError, TypeError):
    title = raw.strip('"\'')
try:
    meta = json.load(open(meta_path, encoding="utf-8"))
except (OSError, ValueError):
    meta = {}
meta.update({"id": slug, "title": title, "updated": now})
with open(meta_path, "w", encoding="utf-8") as out:
    json.dump(meta, out, indent=1)
PY
"${GIT[@]}" -C "$REPO" add -- "posts/$SLUG/post.md" "posts/$SLUG/meta.json"

status "building the site"
"$PY" "$SITE_PY" "$REPO" "$OUT" >&2
if [ "${PUBLISH_NO_PUSH:-0}" != "1" ]; then
  status "publishing to swair.dev"
  [ -f "$KEY" ] || die "publish key not found: $KEY"
  ssh_cmd="ssh -i $KEY -o BatchMode=yes -o StrictHostKeyChecking=accept-new -o ConnectTimeout=20"
  rsync -az --omit-dir-times --no-perms --no-owner --no-group \
    -e "$ssh_cmd" "$OUT/index.html" "$OUT/writing.css" "$TARGET" >&2 \
    || die "rsync of the home page failed"
  rsync -az --delete --omit-dir-times --no-perms --no-owner --no-group \
    -e "$ssh_cmd" "$OUT/posts/" "${TARGET%/}/posts/" >&2 \
    || die "rsync of posts failed"
else
  echo "[website] PUBLISH_NO_PUSH=1: skipping rsync to $TARGET" >&2
fi

"${GIT[@]}" -C "$REPO" commit -qm "edit post: $SLUG" >/dev/null
trap - EXIT
printf 'published' > "$JOB/outcome.txt"
printf '%s/posts/%s/' "${SITE_URL%/}" "$SLUG" > "$JOB/url.txt"
status "done"
echo "${SITE_URL%/}/posts/$SLUG/"
