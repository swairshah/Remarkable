#!/usr/bin/env bash
set -euo pipefail

PROMPT_DEFAULT="Summarize what changed since last sync. If nothing changed, do nothing. If changed, produce a clean personal activity digest."
PROMPT="${1:-$PROMPT_DEFAULT}"

SOURCE_DIR="${SOURCE_DIR:-$HOME/remarkable-backup/xochitl}"
STATE_DIR="${STATE_DIR:-$HOME/remarkable-exports/activity-agent}"
# nginx serves $HOME/notes as the web root; the digest lives at /updates/
OUTPUT_HTML="${OUTPUT_HTML:-$HOME/notes/updates/index.html}"
ENV_FILE="${ENV_FILE:-$HOME/.env}"
# Routed through the exe.dev LLM integration (llm.int.exe.xyz) using the
# connected ChatGPT subscription. Use "openrouter/<vendor>/<model>" to route
# via OpenRouter instead (requires OPENROUTER_API_KEY).
MODEL="${MODEL:-gpt-5.5}"

node "$HOME/bin/remarkable-activity-agent.js" \
  -p "$PROMPT" \
  -m "$MODEL" \
  --source-dir "$SOURCE_DIR" \
  --state-dir "$STATE_DIR" \
  --output-html "$OUTPUT_HTML" \
  --env-file "$ENV_FILE"
