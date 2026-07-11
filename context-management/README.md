# context-management — a sandbox for hand-rolling pi context compression

Goal: understand **exactly what pi ships to the model every time a message is
sent**, then hand-write a custom compression extension that keeps the reMarkable
"Paper" pi session from ballooning (49 × 880 KB page images → 43 MB → "stuck").

Everything here runs against a **real exported trajectory** of the wedged
session, offline, with no changes to the device.

```
context-management/
  trajectory/
    session-full.jsonl     # the real 43 MB device session (179 msgs, 49 images)
    session-sample.jsonl   # header + first 54 entries (16 images, 13 MB) — fast iteration
  extensions/
    observe.ts             # INSTRUMENTATION: logs the payload on every LLM call
    compress.ts            # minimal working compressor (baseline + TODOs for the real scheme)
  reports/                 # observe.ts writes per-call breakdowns here
  README.md
```

## The mental model (one hook does the work)

pi fires a `context` event **before every LLM call**, inside each turn:

```
turn (repeats while the LLM calls tools)
  ├─► turn_start
  ├─► context           ← you get a DEEP COPY of the messages; return a new array
  ├─► before_provider_request
  └─► LLM responds …
```

```ts
pi.on("context", async (event, ctx) => {
  // event.messages — deep copy, safe to mutate
  return { messages: yourRewrittenArray };   // the model sees THIS
});
```

Two properties make this the right tool:

1. **Per-turn.** It runs on *every* send, so compression is continuous, not a
   one-shot threshold event like `session_before_compact` (which would flatten
   the whole history to a text summary and destroy the images).
2. **Non-destructive.** You mutate a copy. The persisted session on disk keeps
   the originals — so nothing is lost, and old images stay available to
   "unroll" on demand later.

Image blocks look like: `{ type: "image", data: "<base64>", mimeType: "image/png" }`.

---

## Step 1 — investigate what actually gets sent

Fork the trajectory (throwaway copy), load **only** the observer, run one turn.
`PI_OBSERVE_ABORT=1` cancels the turn right after logging so you don't fire a
real 13 MB model call:

```bash
cd context-management
PI_OBSERVE_ABORT=1 PI_OBSERVE_TAG=baseline \
  pi --fork trajectory/session-sample.jsonl \
     --no-extensions -e extensions/observe.ts \
     --print "what's on this page?"
```

You'll see, per LLM call, a breakdown like:

```
[observe:baseline] context call #1  ────────────────────────────
  N messages · total 13.0MB · 16 images = 12.9MB (99% of payload)
   # 12 user       1🖼    880KB
   # 18 user       1🖼    880KB
   ...
```

That line — **images = 99% of payload** — is the whole problem, made measurable.
Durable copies land in `reports/baseline-call-01.json` and `reports/baseline-summary.log`.

> Drop `PI_OBSERVE_ABORT=1` (and set a real `--model`/API key) if you want the
> turn to actually complete. The report is written *before* the model call
> either way, so aborting loses nothing.

---

## Step 2 — watch a compressor change the payload

Load `compress.ts` **before** `observe.ts` (handlers run in load order, so the
observer now measures the *compressed* array):

```bash
PI_OBSERVE_ABORT=1 PI_OBSERVE_TAG=compressed \
  pi --fork trajectory/session-sample.jsonl \
     --no-extensions \
     -e extensions/compress.ts -e extensions/observe.ts \
     --print "what's on this page?"
```

The baseline compressor keeps the newest image and stubs the rest, so expect the
`context call #1` line to drop from ~13 MB to ~1 MB. Compare the two runs:

```bash
cat reports/baseline-summary.log
cat reports/compressed-summary.log
```

---

## Step 3 — build the real scheme (this is your part)

`compress.ts` is deliberately dumb: keep 1 image, hard-stub the rest. The
interesting design lives in its TODO markers:

- **(a) Tiered decay** — instead of a hard stub, downscale middle-aged images to
  a small JPEG thumbnail (keep `type:"image"`, shrink `data`); fully stub only
  the oldest. "Age" = distance from the newest image.
- **(b) Unroll-on-demand** — register a `canvas_recall(ref)` tool that reads the
  ORIGINAL image back out of `ctx.sessionManager.getEntries()` and returns it as
  a fresh `toolResult` image. Makes the stubs lossless. (pi's own
  `headroom_retrieve` pattern.)
- **(c) Prompt-cache stability** — memoize each demotion by a stable image hash
  so a given image always maps to the same stub/thumbnail every turn. A moving
  "last-K" window rewrites the cached prefix and busts the cache each turn. Keep
  exactly one image hot.

Complementary, source-side (in the Rust app, not here): emit JPEG/downscaled
snapshots and skip re-sending an unchanged page — cuts the on-disk session too.

---

## Reference (all shipped locally with pi 0.80.5)

```bash
PKG=$(node -e "console.log(require.resolve('@earendil-works/pi-coding-agent').replace(/dist.*/,''))")
$PKG/docs/extensions.md          # the `context` event + full hook list & turn diagram
$PKG/docs/session-format.md      # message / content-block types
$PKG/docs/compaction.md          # session_before_compact (the threshold-only hook)
$PKG/examples/extensions/        # 40+ real extensions (custom-compaction.ts, dynamic-tools.ts, …)
```

Loading recap: `-e <path>` loads one extension, `-ne` disables the rest,
`--fork <session>` replays a trajectory into a throwaway copy, `--print` runs a
single turn. Permanent installs go in `~/.pi/agent/settings.json` → `extensions`.
