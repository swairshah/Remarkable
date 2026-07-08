# notebook — model cost: measurements & options

Status (2026-07-08): **biting the cost for now** — none of the options
below are implemented yet. This file is the plan for when it starts to
hurt. Measure first: `make trace` renders per-turn tokens + cost from the
session files; the numbers below came from the first real day.

## Where the money goes

Each pause sends ONE user message: the page snapshot (half scale, 702x936
grayscale PNG ≈ **~900 input tokens**) + ~150 tokens of text (patch list,
measured layout). Everything ever sent stays in the session (`--continue`
forever), so at pause N the model re-reads all N images:

- **Warm pause** (< ~5 min since the last one): history comes from the
  provider's prompt cache at ~10x discount. Marginal cost ≈ new image +
  0.1 x history. Measured: **$0.01–0.02 per response** (gpt-5.5).
- **Cold pause** (first one after a break — the common case for a
  notebook!): the whole history is re-read at full price. Measured at 14k
  context: **$0.03–0.07**. At a multi-day 50–100k history this becomes
  $0.25–0.50 *per return to the notebook*, i.e. a heavy day could reach
  **$2–3/day** and keep growing.
- Outputs are trivial (`pass` = ~5 tokens; a draw = a few hundred).
- pi auto-compacts eventually, but only past its (high) threshold; cold
  reads below that threshold pay full price.

First real day, for scale: 21 responses, 12 pauses, 5 draws + erases —
**$0.49 total**.

## Options, by leverage

1. **Send the new ink, not the page** (~5–10x on the recurring block).
   We know exactly which strokes are new since the last send: crop the
   snapshot to them (+ padding), ~100–300 tokens instead of ~900. The
   model keeps the measured layout text for placement and can call
   `notebook_view` (already exists) when it truly needs the full page.
   No product change.

2. **Cap the history: session-per-day** (kills unbounded growth).
   `--continue` only same-day / under a size cap; else start fresh.
   Old days stay on disk and pi is already told it may read those files
   with its tools when the user refers to the past. Optionally seed each
   day with a 2-line summary of yesterday.

3. **`NOTEBOOK_MODEL` knob** (3–10x on everything, 5-line change).
   Pass `--model` through to pi. Placement now runs on measured numbers,
   so a cheaper vision model (gpt-5-mini / haiku-class) may hold up for
   the pass/draw judgment. Try and compare traces.

4. **Summon mode** (near-zero idle cost, product change).
   `NOTEBOOK_TRIGGER=auto|summon`: in summon, pauses send nothing; only a
   deliberate cue (trailing "?", addressing "pi", a corner gesture) sends
   the page. Loses the spontaneous chime-in — keep as a mode, not default.

5. **Cheap triage model** (biggest build, do last).
   A tiny model looks at the new-ink crop and decides whether to wake the
   big one. ~80% of pauses would end at ~$0.001. Only worth it if 1–3
   leave a gap.

## Recommended bundle when the time comes

1 + 2 + 3 together: independent, compounding, no feel change. Estimated
heavy day (30 pauses, several cold starts): **$1.5–3 → ~$0.10–0.25**, flat
across days instead of growing.
