/**
 * compress.ts — a MINIMAL, working baseline compressor. Teaching scaffold, not
 * the finished scheme. It shows the exact mechanics of a `context`-hook
 * compressor; the clever policy (tiered decay, unroll-on-demand, cache
 * stability) is left for you to build in the TODOs.
 *
 * Mechanism (from docs/extensions.md → the `context` event):
 *   - fires before every LLM call
 *   - event.messages is a DEEP COPY — safe to mutate/replace
 *   - return { messages } to send YOUR version to the model
 *   - the persisted session on disk is UNTOUCHED, so nothing is lost and the
 *     originals remain available to "unroll" later.
 *
 * Baseline policy: keep the most-recent page image at full res; replace every
 * older image block with a tiny text stub. That single rule takes each turn
 * from ~43 MB → ~1 MB on the reMarkable trajectory.
 *
 *     pi --fork trajectory/session-sample.jsonl -ne \
 *        -e extensions/compress.ts -e extensions/observe.ts -p "hi"
 *   (compress first, observe second → observe reports the COMPRESSED payload)
 */
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

/** How many of the most-recent images to keep at full resolution. */
const KEEP_FULL = 1;

function isImage(b: any): boolean {
  return b && typeof b === "object" && b.type === "image";
}

/** A cheap stable-ish id for an image so a future unroll tool can find it.
 *  TODO: replace with a real content hash (e.g. sha1 of b.data) so stubs are
 *  deterministic across turns — important for prompt-cache stability. */
function imageRef(b: any, msgIndex: number): string {
  return `img@msg${msgIndex}:${(b.data?.length ?? 0)}b`;
}

export default function (pi: ExtensionAPI) {
  pi.on("context", async (event: any, _ctx: any) => {
    const messages: any[] = event.messages ?? [];

    // 1) locate every image block, in order, with its (message, block) position
    const imgPositions: Array<{ mi: number; bi: number }> = [];
    messages.forEach((m, mi) => {
      if (Array.isArray(m.content)) {
        m.content.forEach((b: any, bi: number) => {
          if (isImage(b)) imgPositions.push({ mi, bi });
        });
      }
    });

    // 2) the last KEEP_FULL images stay; everything earlier gets stubbed
    const demoteBefore = imgPositions.length - KEEP_FULL;
    for (let k = 0; k < demoteBefore; k++) {
      const { mi, bi } = imgPositions[k];
      const block = messages[mi].content[bi];
      messages[mi].content[bi] = {
        type: "text",
        text: `[page image elided to save context — ref ${imageRef(block, mi)}]`,
      };
      // ------------------------------------------------------------------
      // TODO(you): this is where the CLEVER part goes. Options to build:
      //   (a) TIERED DECAY: instead of a hard stub, downscale middle-aged
      //       images to a small JPEG thumbnail (keep {type:"image"} but with
      //       shrunken b.data), and only fully stub the oldest. Age = distance
      //       from the end of imgPositions.
      //   (b) UNROLL-ON-DEMAND: register a `canvas_recall(ref)` tool via
      //       pi.on("tool_call", ...) or the tool API that reads the ORIGINAL
      //       image back out of ctx.sessionManager.getEntries() by `ref` and
      //       returns it as a fresh toolResult image. Then this stub is lossless.
      //   (c) CACHE STABILITY: memoize the demotion decision per stable image
      //       hash so a given image always maps to the SAME stub/thumbnail every
      //       turn — otherwise a moving window rewrites the cached prefix and
      //       busts the prompt cache each turn. Keep exactly one image "hot".
      // ------------------------------------------------------------------
    }

    return { messages };
  });
}
