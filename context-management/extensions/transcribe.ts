/**
 * transcribe.ts — image→text compression for Paper's pi session.
 *
 * Three compounding ideas:
 *
 *   1) TRANSCRIBE  — the current page stays an image; right after the model
 *      answers, a second "fork" call asks it to transcribe that page's
 *      handwriting IN THE SAME CHAT CONTEXT. Because pi puts its Anthropic
 *      cache breakpoint on the last user message (= the page image), the fork's
 *      prefix matches the just-made main call byte-for-byte → near-total cache
 *      hit, and the model can use the conversation to decipher the ink. On
 *      later turns the image is swapped for that text. (`complete()` from
 *      @earendil-works/pi-ai lets an extension call the model directly — see
 *      the shipped custom-compaction.ts example.)
 *
 *   2) DEDUP BY PAGE — a page you flip back to is re-sent verbatim every pause.
 *      In context we keep only the MOST RECENT occurrence of each page with real
 *      content; every earlier occurrence collapses to a one-line pointer that
 *      still names the page + a content snippet, so pi can follow the history.
 *      If a page hasn't changed, its old copies carry no payload at all.
 *
 *   3) RECALL TOOL — compression is non-destructive: the originals stay in the
 *      session. `recall_page(page)` fetches a page's full image back on demand,
 *      so pi can look at / draw on a page that's currently only text.
 *
 * Store persists to reports/transcriptions.json keyed by image hash.
 * Transcription uses the CURRENT session model by default (same subscription/auth
 * as everything else). Env: PI_TRANSCRIBE_MODEL (+ PI_TRANSCRIBE_PROVIDER) to
 * force a specific model; PI_TRANSCRIBE_MODE (ink|full, default ink);
 * PI_TRANSCRIBE_BACKFILL=1; PI_KEEP_IMAGES (newest images kept as images, default 1).
 */
import { complete } from "@earendil-works/pi-ai/compat";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";
import { createHash } from "node:crypto";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";
import { fileURLToPath } from "node:url";

const STORE = fileURLToPath(new URL("../reports/transcriptions.json", import.meta.url));
// By DEFAULT transcribe with the SAME model pi is running (ctx.model) — same
// auth/subscription as everything else, no separate metered bill. Set
// PI_TRANSCRIBE_MODEL (+ optional PI_TRANSCRIBE_PROVIDER) only if you want to
// force a specific cheaper model (worthwhile only when billing is per-token).
const OVERRIDE_MODEL = process.env.PI_TRANSCRIBE_MODEL;
const OVERRIDE_PROVIDER = process.env.PI_TRANSCRIBE_PROVIDER;
const BACKFILL = process.env.PI_TRANSCRIBE_BACKFILL === "1";
const KEEP = Math.max(1, parseInt(process.env.PI_KEEP_IMAGES ?? "1", 10) || 1);
const MODE = process.env.PI_TRANSCRIBE_MODE ?? "ink";

// Standalone prompt: image attached to this very message (backfill / stragglers).
const PROMPT = MODE === "full"
  ? "Transcribe this page image into clean Markdown. Include the printed text in " +
    "reading order AND any handwritten annotations, marks, or drawings (describe " +
    "non-text ink briefly in [square brackets]). Output ONLY the transcription."
  : "This page's PRINTED text is already known to the reader, so IGNORE it. " +
    "Transcribe ONLY handwritten annotations, marks, underlines, and drawings — " +
    "give the words, and describe non-text ink in [square brackets] with rough " +
    "location. If the page has no handwriting or marks at all, output exactly: none";

// In-context prompt: the image is ALREADY in the conversation (last page shown),
// so we don't resend it — the fork rides the provider cache. The chat gives the
// model context to decipher ambiguous handwriting.
const CTX_PROMPT = MODE === "full"
  ? "Administrative request (not the notebook user): transcribe the page image in the " +
    "most recent page message above into clean Markdown — printed text in reading order " +
    "AND any handwritten ink (describe non-text ink in [square brackets]). " +
    "Do not use tools. Output ONLY the transcription."
  : "Administrative request (not the notebook user): the most recent page message above " +
    "contains a page image. Its PRINTED text is already in the conversation, so transcribe " +
    "ONLY the handwritten annotations, marks, underlines, and drawings on it — use the " +
    "conversation to help decipher the handwriting; describe non-text ink in [square " +
    "brackets] with rough location. Do not use tools. If the page has no handwriting or " +
    "marks at all, output exactly: none";

type Store = Record<string, string>;
const loadStore = (): Store => { try { return JSON.parse(readFileSync(STORE, "utf8")); } catch { return {}; } };
const saveStore = (s: Store) => { mkdirSync(dirname(STORE), { recursive: true }); writeFileSync(STORE, JSON.stringify(s, null, 2)); };

const keyOf = (b64: string) =>
  createHash("sha1").update(b64.slice(0, 2048)).update("|").update(b64.slice(-2048)).update("|" + b64.length).digest("hex").slice(0, 16);
const sk = (k: string) => `${MODE}:${k}`;

/** Parse a Paper page-pause user message: page number, doc title, content snippet. */
function parsePage(text: string): { page: number; doc: string; snippet: string } | null {
  const pm = text.match(/page (\d+) of/);
  if (!pm) return null;
  const dm = text.match(/^"([^"]+)"/);
  const em = text.match(/Extracted text of this page:\s*-*\s*([\s\S]{0,80})/);
  const snippet = (em ? em[1] : text).replace(/\s+/g, " ").trim().slice(0, 60);
  return { page: Number(pm[1]), doc: dm ? dm[1] : "", snippet };
}

/** All image blocks in a message list, with positions + hash. */
function imagesIn(messages: any[]) {
  const out: Array<{ mi: number; bi: number; b64: string; mime: string; key: string }> = [];
  messages.forEach((m, mi) => {
    if (!Array.isArray(m?.content)) return;
    m.content.forEach((b: any, bi: number) => {
      if (b?.type === "image" && b.data) out.push({ mi, bi, b64: b.data, mime: b.mimeType || "image/png", key: keyOf(b.data) });
    });
  });
  return out;
}

/** Newest-first scan of session entries for a page's original image. */
function findPageImage(entries: any[], page: number): { b64: string; mime: string } | null {
  for (let i = entries.length - 1; i >= 0; i--) {
    const m = entries[i]?.message;
    if (!m || m.role !== "user" || !Array.isArray(m.content)) continue;
    const tb = m.content.find((b: any) => b?.type === "text");
    const ib = m.content.find((b: any) => b?.type === "image");
    if (!tb || !ib) continue;
    const p = parsePage(tb.text || "");
    if (p && p.page === page) return { b64: ib.data, mime: ib.mimeType || "image/png" };
  }
  return null;
}

/** Default: the current session model. Override only via env. */
function resolveModel(ctx: any) {
  if (OVERRIDE_MODEL) {
    const m = ctx.modelRegistry?.find?.(OVERRIDE_PROVIDER ?? "anthropic", OVERRIDE_MODEL);
    if (m) return m;
    ctx.ui?.notify?.(`transcribe: ${OVERRIDE_PROVIDER ?? ""}/${OVERRIDE_MODEL} not found; using session model`, "warning");
  }
  return ctx.model ?? null; // whatever pi is already running
}

/** One model call with the current session model + auth; returns the text (or null). */
async function callModel(ctx: any, context: { systemPrompt?: string; tools?: any[]; messages: any[] }, signal?: AbortSignal): Promise<string | null> {
  const model = resolveModel(ctx);
  if (!model) { ctx.ui?.notify?.("transcribe: no model available", "warning"); return null; }
  const auth = await ctx.modelRegistry.getApiKeyAndHeaders(model);
  if (!auth?.ok || !auth.apiKey) { ctx.ui?.notify?.(`transcribe: no auth for ${model.provider ?? "model"}`, "warning"); return null; }
  const res = await complete(model, context, { apiKey: auth.apiKey, headers: auth.headers, env: auth.env, maxTokens: 1500, signal });
  const text = (res?.content ?? []).filter((c: any) => c.type === "text").map((c: any) => c.text).join("\n").trim();
  return text || null;
}

/** Standalone transcription: image attached to the request itself (backfill path). */
async function transcribeOne(ctx: any, b64: string, mime: string, signal?: AbortSignal): Promise<string | null> {
  return callModel(ctx, {
    messages: [{ role: "user", content: [{ type: "text", text: PROMPT }, { type: "image", data: b64, mimeType: mime }], timestamp: Date.now() }],
  }, signal);
}

/** The tools active on the main call, so the fork's request prefix can match it. */
function activeTools(pi: any): any[] | undefined {
  try {
    const names = new Set(pi.getActiveTools?.() ?? []);
    const t = (pi.getAllTools?.() ?? []).filter((x: any) => names.has(x.name));
    return t.length ? t : undefined;
  } catch { return undefined; }
}

export default function (pi: ExtensionAPI) {
  const store = loadStore();
  let backfilled = false;

  // Snapshot of the exact (transformed) request pi last sent — systemPrompt,
  // tools, messages, and which image hashes are still present as images. The
  // in-context fork reuses this verbatim so its prefix hits the provider cache.
  let lastLlm: { systemPrompt?: string; tools?: any[]; messages: any[]; imageKeys: Set<string> } | null = null;

  async function ensureTranscribed(ctx: any, imgs: ReturnType<typeof imagesIn>, signal?: AbortSignal) {
    const todo = imgs.filter((im) => !store[sk(im.key)]);
    if (!todo.length) return 0;
    const results = await Promise.all(todo.map((im) => transcribeOne(ctx, im.b64, im.mime, signal).catch(() => null)));
    let n = 0;
    todo.forEach((im, i) => { if (results[i]) { store[sk(im.key)] = results[i]!; n++; } });
    if (n) saveStore(store);
    return n;
  }

  // ---- recall tool: pull a page's original image back on demand ----
  pi.registerTool({
    name: "recall_page",
    label: "Recall page image",
    description:
      "Fetch the full-resolution image of a specific page. Older pages appear in the " +
      "conversation only as text to save space; call this when you need to SEE a page " +
      "(layout, figures, exact ink positions) or before drawing on it.",
    promptSnippet: "Recall the original image of a page shown earlier only as text",
    promptGuidelines: ["Use recall_page before drawing on or closely inspecting a page that currently appears only as text."],
    parameters: Type.Object({ page: Type.Number({ description: "1-based page number to recall" }) }),
    async execute(_id: string, params: any, _signal: any, _onUpdate: any, ctx: any) {
      const page = Number(params?.page);
      const entries = ctx?.sessionManager?.getEntries?.() ?? [];
      const img = findPageImage(entries, page);
      if (!img) return { content: [{ type: "text", text: `No stored image for page ${page}.` }], isError: true };
      return {
        content: [{ type: "image", data: img.b64, mimeType: img.mime }, { type: "text", text: `Full image of page ${page}.` }],
        details: { page },
      };
    },
  });

  // ---- backfill: transcribe every image already in a bloated session ----
  pi.on("session_start", async (_e: any, ctx: any) => {
    if (!BACKFILL || backfilled) return;
    backfilled = true;
    const msgs = (ctx.sessionManager?.getEntries?.() ?? []).map((e: any) => e.message).filter(Boolean);
    const n = await ensureTranscribed(ctx, imagesIn(msgs));
    process.stderr.write(`[transcribe] backfill: +${n} transcriptions [mode=${MODE}] (store ${Object.keys(store).length})\n`);
  });

  // ---- online: after the model answers, fork IN CONTEXT to transcribe the
  // current page. The fork = last request prefix (cached!) + the answer + a
  // transcribe prompt — the image is already in that prefix, so it isn't resent.
  pi.on("message_end", async (event: any, ctx: any) => {
    const am = event?.message;
    if (am?.role !== "assistant") return;
    // mid-turn assistant messages end in toolCalls; forking there would leave a
    // dangling tool_use (API error). Wait for the final text-only answer.
    if (Array.isArray(am.content) && am.content.some((b: any) => b?.type === "toolCall")) return;

    const msgs = (ctx.sessionManager?.getEntries?.() ?? []).map((e: any) => e.message).filter(Boolean);
    const imgs = imagesIn(msgs);
    if (!imgs.length) return;
    let n = 0;

    // current page image → in-context fork (rides the cache, sees the chat)
    const cur = imgs[imgs.length - 1];
    if (!store[sk(cur.key)] && lastLlm?.imageKeys.has(cur.key)) {
      const text = await callModel(ctx, {
        systemPrompt: lastLlm.systemPrompt,
        tools: lastLlm.tools,
        messages: [...lastLlm.messages, am, { role: "user", content: [{ type: "text", text: CTX_PROMPT }], timestamp: Date.now() }],
      }, ctx.signal).catch(() => null);
      if (text) { store[sk(cur.key)] = text; saveStore(store); n++; }
    }

    // stragglers (fast flips, missed turns) → standalone per-image calls
    n += await ensureTranscribed(ctx, imgs, ctx.signal);
    if (n) process.stderr.write(`[transcribe] +${n} transcription(s) after turn\n`);
    return undefined;
  });

  // ---- context: dedup-by-page + image→text swap, every LLM call ----
  pi.on("context", async (event: any, ctx: any) => {
    const messages: any[] = event.messages ?? [];

    // snapshot what this request will actually contain (called before returning
    // on every path) so the in-context fork can replay the same cached prefix
    const capture = () => {
      lastLlm = {
        systemPrompt: ctx?.getSystemPrompt?.(),
        tools: activeTools(pi),
        messages,
        imageKeys: new Set(imagesIn(messages).map((im) => im.key)),
      };
    };

    // gather page-pause occurrences (in order)
    const occ: Array<{ mi: number; page: number; doc: string; snippet: string; imgKey: string | null }> = [];
    messages.forEach((m, mi) => {
      if (m?.role !== "user" || !Array.isArray(m.content)) return;
      const tb = m.content.find((b: any) => b?.type === "text");
      if (!tb) return;
      const p = parsePage(tb.text || "");
      if (!p) return;
      const ib = m.content.find((b: any) => b?.type === "image");
      occ.push({ mi, page: p.page, doc: p.doc, snippet: p.snippet, imgKey: ib ? keyOf(ib.data) : null });
    });
    if (!occ.length) { capture(); return { messages }; }

    // which occurrence is the latest for each page; which images stay as images
    const latestIdx = new Map<string, number>();
    occ.forEach((o, i) => latestIdx.set(o.doc + "|" + o.page, i));
    const imgOccKeys = occ.filter((o) => o.imgKey).map((o) => o.imgKey!);
    const keepImages = new Set(imgOccKeys.slice(-KEEP)); // newest KEEP images

    let collapsed = 0, swapped = 0;
    occ.forEach((o, i) => {
      const m = messages[o.mi];
      const isLatest = latestIdx.get(o.doc + "|" + o.page) === i;

      if (!isLatest) {
        // superseded by a newer view of the same page → bare labeled pointer (no payload)
        m.content = [{ type: "text", text: `[page ${o.page}${o.doc ? ` of "${o.doc}"` : ""}: "${o.snippet}" — current state shown later]` }];
        collapsed++;
        return;
      }
      // latest view of this page: keep the current image, else swap image→ink transcription
      if (o.imgKey && !keepImages.has(o.imgKey)) {
        const ib = m.content.findIndex((b: any) => b?.type === "image");
        if (ib >= 0) {
          const t = store[sk(o.imgKey)];
          const body = !t ? `[page ${o.page} image omitted]`
            : MODE === "ink"
              ? (t.trim().toLowerCase() === "none"
                  ? `[page ${o.page}: image omitted — printed text above, no handwriting]`
                  : `[page ${o.page} handwritten ink]\n${t}`)
              : `[page ${o.page} → transcription]\n${t}`;
          m.content[ib] = { type: "text", text: body };
          swapped++;
        }
      }
    });

    // general sweep: any leftover image (e.g. a toolResult/canvas_view image) that
    // isn't one of the newest KEEP → swap to its transcription if we have one.
    for (const im of imagesIn(messages)) {
      if (keepImages.has(im.key)) continue;
      const t = store[sk(im.key)];
      if (!t) continue;
      const body = MODE === "ink"
        ? (t.trim().toLowerCase() === "none" ? "[image omitted — no handwriting]" : `[handwritten ink]\n${t}`)
        : `[image → transcription]\n${t}`;
      messages[im.mi].content[im.bi] = { type: "text", text: body };
      swapped++;
    }

    if (collapsed || swapped) process.stderr.write(`[transcribe] dedup: ${collapsed} page(s) collapsed, ${swapped} swapped→text\n`);
    capture();
    return { messages };
  });
}
