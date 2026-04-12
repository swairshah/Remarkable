#!/usr/bin/env bash
set -euo pipefail

PROMPT_DEFAULT="Summarize what changed since last sync. If nothing changed, do nothing. If changed, produce a clean personal activity digest."
PROMPT="${1:-$PROMPT_DEFAULT}"

SOURCE_DIR="${SOURCE_DIR:-/home/swair/remarkable-backup/xochitl}"
STATE_DIR="${STATE_DIR:-/home/swair/remarkable-exports/activity-agent}"
# swair.dev redirects to blog.swair.dev, which serves from /home/swair/notes via notes_app
OUTPUT_HTML="${OUTPUT_HTML:-/home/swair/notes/index.html}"
ENV_FILE="${ENV_FILE:-/home/swair/.env}"
MODEL="${MODEL:-anthropic/claude-sonnet-4-6}"

node /home/swair/bin/remarkable-activity-agent.js \
  -p "$PROMPT" \
  -m "$MODEL" \
  --source-dir "$SOURCE_DIR" \
  --state-dir "$STATE_DIR" \
  --output-html "$OUTPUT_HTML" \
  --env-file "$ENV_FILE"
