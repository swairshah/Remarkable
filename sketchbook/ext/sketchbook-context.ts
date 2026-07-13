/**
 * sketchbook-context.ts — image→inventory context compression for pi.
 *
 * Adapted from alt-ui's paper-transcribe.ts (context-management/transcribe),
 * reshaped for a drawing app: instead of transcribing handwriting, the
 * second model call writes a SCENE INVENTORY — what the user sketched and
 * where, their handwritten words, and what each of pi's outputs depicts —
 * so later turns keep full knowledge of what was created without carrying
 * a single old image.
 *
 * Three compounding ideas (same architecture as paper-transcribe):
 *
 *   1) INVENTORY FORK — right after the model answers a pause, a fork call
 *      (same context; prefix rides the provider cache byte-for-byte) asks
 *      it to inventory the page it just looked at. On later turns the
 *      image is swapped for that inventory text.
 *
 *   2) DEDUP BY PAGE — only the MOST RECENT occurrence of each page keeps
 *      a payload; earlier occurrences collapse to one-line pointers.
 *      Combined with 1), the context carries at most ONE image (the
 *      current page) plus compact inventories of everything older.
 *
 *   3) LIVE RECALL — compression is non-destructive AND the app is the
 *      better archive: sketchbook_view fetches any page's CURRENT image
 *      on demand (fresher than session history — the user may have erased
 *      or moved things since).
 *
 * Store: ~/.local/share/sketchbook/inventories.json keyed by image hash.
 * Env: PI_KEEP_IMAGES (newest images kept as images, default 1),
 *      PI_INVENTORY_MODEL (+ PI_INVENTORY_PROVIDER) to force a model.
 */
import { complete } from "@earendil-works/pi-ai/compat";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { createHash } from "node:crypto";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";

const STORE =
  process.env.SKETCHBOOK_INVENTORY_STORE ??
  `${process.env.HOME || "/home/root"}/.local/share/sketchbook/inventories.json`;
const OVERRIDE_MODEL = process.env.PI_INVENTORY_MODEL;
const OVERRIDE_PROVIDER = process.env.PI_INVENTORY_PROVIDER;
const KEEP = Math.max(1, parseInt(process.env.PI_KEEP_IMAGES ?? "1", 10) || 1);

const INVENTORY_ASK =
  "write a compact INVENTORY of the page so future turns can work without " +
  "the image. Cover: 1) the user's sketch — what it depicts and roughly " +
  "where (page coordinates, 1404x1872; the image is half scale); 2) any " +
  "handwritten words, verbatim; 3) each AI raster output — its id (the " +
  "accompanying message text lists ids and rects), what it shows, how " +
  "polished; 4) anything that looks mid-progress. Terse lines, no prose. " +
  "Do not use tools. Output ONLY the inventory.";

// Standalone prompt: image attached to this very message (stragglers).
const PROMPT =
  "This is a page of an artist's sketchbook app (user ink black, AI ink " +
  `gray, AI images grayscale): ${INVENTORY_ASK}`;

// In-context prompt: the image is ALREADY in the conversation.
const CTX_PROMPT =
  "Administrative request (not the artist): the most recent page message " +
  `above contains the current sketchbook page image; ${INVENTORY_ASK}`;

type Store = Record<string, string>;
const loadStore = (): Store => { try { return JSON.parse(readFileSync(STORE, "utf8")); } catch { return {}; } };
const saveStore = (s: Store) => { mkdirSync(dirname(STORE), { recursive: true }); writeFileSync(STORE, JSON.stringify(s, null, 2)); };

const keyOf = (b64: string) =>
  createHash("sha1").update(b64.slice(0, 2048)).update("|").update(b64.slice(-2048)).update("|" + b64.length).digest("hex").slice(0, 16);

/** Parse a sketchbook page-pause user message: page number + snippet. */
function parsePage(text: string): { page: number; snippet: string } | null {
  const pm = text.match(/[Ss]ketchbook page (\d+) of/);
  if (!pm) return null;
  const snippet = text.replace(/\s+/g, " ").trim().slice(0, 60);
  return { page: Number(pm[1]), snippet };
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

/** Default: the current session model. Override only via env. */
function resolveModel(ctx: any) {
  if (OVERRIDE_MODEL) {
    const m = ctx.modelRegistry?.find?.(OVERRIDE_PROVIDER ?? "anthropic", OVERRIDE_MODEL);
    if (m) return m;
  }
  return ctx.model ?? null;
}

async function callModel(ctx: any, context: { systemPrompt?: string; tools?: any[]; messages: any[] }, signal?: AbortSignal): Promise<string | null> {
  const model = resolveModel(ctx);
  if (!model) return null;
  const auth = await ctx.modelRegistry.getApiKeyAndHeaders(model);
  if (!auth?.ok || !auth.apiKey) return null;
  const res = await complete(model, context, { apiKey: auth.apiKey, headers: auth.headers, env: auth.env, maxTokens: 1200, signal });
  const text = (res?.content ?? []).filter((c: any) => c.type === "text").map((c: any) => c.text).join("\n").trim();
  return text || null;
}

async function inventoryOne(ctx: any, b64: string, mime: string, signal?: AbortSignal): Promise<string | null> {
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
  if (!process.env.SKETCHBOOK_SOCK) return; // only inside the sketchbook app
  const store = loadStore();

  // Snapshot of the exact (transformed) request pi last sent, so the
  // in-context fork can replay the same cached prefix.
  let lastLlm: { systemPrompt?: string; tools?: any[]; messages: any[]; imageKeys: Set<string> } | null = null;

  // ---- online: after the model answers, fork IN CONTEXT to inventory the
  // current page (this is the "second model call" that records what was
  // just created — it runs after generation turns too, so the fresh raster
  // is described while its image is still in the cached prefix).
  pi.on("message_end", async (event: any, ctx: any) => {
    const am = event?.message;
    if (am?.role !== "assistant") return;
    if (Array.isArray(am.content) && am.content.some((b: any) => b?.type === "toolCall")) return;

    const msgs = (ctx.sessionManager?.getEntries?.() ?? []).map((e: any) => e.message).filter(Boolean);
    const imgs = imagesIn(msgs);
    if (!imgs.length) return;

    const cur = imgs[imgs.length - 1];
    if (!store[cur.key] && lastLlm?.imageKeys.has(cur.key)) {
      const text = await callModel(ctx, {
        systemPrompt: lastLlm.systemPrompt,
        tools: lastLlm.tools,
        messages: [...lastLlm.messages, am, { role: "user", content: [{ type: "text", text: CTX_PROMPT }], timestamp: Date.now() }],
      }, ctx.signal).catch(() => null);
      if (text) {
        store[cur.key] = text;
        saveStore(store);
        process.stderr.write(`[inventory] page captured (${text.length}ch)\n`);
      }
    }

    // stragglers (fast flips, missed turns) → standalone per-image calls
    const todo = imgs.filter((im) => !store[im.key]);
    if (todo.length) {
      const results = await Promise.all(todo.map((im) => inventoryOne(ctx, im.b64, im.mime, ctx.signal).catch(() => null)));
      let n = 0;
      todo.forEach((im, i) => { if (results[i]) { store[im.key] = results[i]!; n++; } });
      if (n) { saveStore(store); process.stderr.write(`[inventory] +${n} straggler(s)\n`); }
    }
    return undefined;
  });

  // ---- context: dedup-by-page + image→inventory swap, every LLM call ----
  pi.on("context", async (event: any, _ctx: any) => {
    const messages: any[] = event.messages ?? [];

    const capture = () => {
      lastLlm = {
        systemPrompt: _ctx?.getSystemPrompt?.(),
        tools: activeTools(pi),
        messages,
        imageKeys: new Set(imagesIn(messages).map((im) => im.key)),
      };
    };

    const occ: Array<{ mi: number; page: number; snippet: string; imgKey: string | null }> = [];
    messages.forEach((m, mi) => {
      if (m?.role !== "user" || !Array.isArray(m.content)) return;
      const tb = m.content.find((b: any) => b?.type === "text");
      if (!tb) return;
      const p = parsePage(tb.text || "");
      if (!p) return;
      const ib = m.content.find((b: any) => b?.type === "image");
      occ.push({ mi, page: p.page, snippet: p.snippet, imgKey: ib ? keyOf(ib.data) : null });
    });
    if (!occ.length) { capture(); return { messages }; }

    const latestIdx = new Map<number, number>();
    occ.forEach((o, i) => latestIdx.set(o.page, i));
    const imgOccKeys = occ.filter((o) => o.imgKey).map((o) => o.imgKey!);
    const keepImages = new Set(imgOccKeys.slice(-KEEP)); // newest KEEP images

    let collapsed = 0, swapped = 0;
    occ.forEach((o, i) => {
      const m = messages[o.mi];
      if (latestIdx.get(o.page) !== i) {
        m.content = [{ type: "text", text: `[page ${o.page} pause — superseded; current state shown later]` }];
        collapsed++;
        return;
      }
      if (o.imgKey && !keepImages.has(o.imgKey)) {
        const ib = m.content.findIndex((b: any) => b?.type === "image");
        if (ib >= 0) {
          const t = store[o.imgKey];
          m.content[ib] = {
            type: "text",
            text: t
              ? `[page ${o.page} image → inventory; sketchbook_view ${o.page} for the live image]\n${t}`
              : `[page ${o.page} image omitted — sketchbook_view ${o.page} to see it]`,
          };
          swapped++;
        }
      }
    });

    // general sweep: any leftover image (e.g. a sketchbook_view result)
    // that isn't among the newest KEEP → swap to inventory / pointer.
    for (const im of imagesIn(messages)) {
      if (keepImages.has(im.key)) continue;
      const t = store[im.key];
      messages[im.mi].content[im.bi] = {
        type: "text",
        text: t ? `[image → inventory]\n${t}` : "[old image omitted — sketchbook_view for a fresh look]",
      };
      swapped++;
    }

    if (collapsed || swapped) process.stderr.write(`[inventory] dedup: ${collapsed} collapsed, ${swapped} swapped\n`);
    capture();
    return { messages };
  });
}
