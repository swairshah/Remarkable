#!/usr/bin/env bash
set -euo pipefail

PROMPT_DEFAULT="Summarize what changed since last sync. If nothing changed, do nothing. If changed, produce a clean personal activity digest."
PROMPT="${1:-$PROMPT_DEFAULT}"

SOURCE_DIR="${SOURCE_DIR:-$HOME/remarkable-backup/xochitl}"
STATE_DIR="${STATE_DIR:-$HOME/remarkable-exports/activity-agent}"
# nginx serves $HOME/notes as the web root; the digest lives at /updates/
OUTPUT_HTML="${OUTPUT_HTML:-$HOME/notes/updates/index.html}"
ENV_FILE="${ENV_FILE:-$HOME/.env}"
MODEL="${MODEL:-anthropic/claude-sonnet-4-6}"

node "$HOME/bin/remarkable-activity-agent.js" \
  -p "$PROMPT" \
  -m "$MODEL" \
  --source-dir "$SOURCE_DIR" \
  --state-dir "$STATE_DIR" \
  --output-html "$OUTPUT_HTML" \
  --env-file "$ENV_FILE"
