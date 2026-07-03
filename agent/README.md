# RBot (standalone Flue agent)

This is a minimal standalone recreation of the Charles agent wiring. It runs as a
Flue Node target and uses your local Pi ChatGPT/Codex OAuth credentials from
`~/.pi/agent/auth.json`.

## Run it

```bash
bun install
bun run connect
```

Then type a prompt and press Enter. `Ctrl-D` exits.

Useful commands:

```bash
bun run typecheck
bun test
bun run build
bun run dev
```

## Auth

The agent model is `openai-codex/gpt-5.5`. On startup it reads Pi's local OAuth
entry for `openai-codex`, refreshes it if needed, and registers the resulting
access token with Flue. If auth is missing, run `pi`, use `/login`, and choose
ChatGPT Plus/Pro (Codex).

You can override the local Pi auth file path with `PI_AUTH_FILE`, or bypass Pi
OAuth by setting `OPENAI_CODEX_API_KEY`.

## Notes

- Flue discovers agents from `src/agents/`; the runnable agent is
  `src/agents/rbot.ts`.
- The Browser Run tool implementation is local under `src/tools/browser-run.ts`.
  It still needs a runtime `BROWSER` binding to actually browse.
