# Owning the render model — plan & costs

Goal: replace (or mostly replace) `gemini-*-image` in sketchbook's render step with a model that reliably produces *your* graphite style, cheaply, and that teaches you diffusion along the way.

References: [dino_diffusion](https://madebyoll.in/posts/dino_diffusion/) ([code](https://github.com/madebyollin/dino-diffusion)) — from-scratch minimal diffusion; [isometric-nyc](https://cannoneyed.com/projects/isometric-nyc) — the production distillation recipe (frontier model → curated pairs → Qwen-Image-Edit LoRA → serverless inference).

## 1. Frame the task correctly

What you need is **not** text→image. It's a **prompted image-edit** task:

```
input:  crop of the page (vector ink rendered to raster, maybe + prior output)
        + prompt (subject description, or edit instruction, or "follow the handwriting")
output: clean grayscale graphite render, same composition
```

The prompt does two distinct jobs, and they cost very different amounts to replicate:

1. **Rendering** — "turn these strokes into graphite, in this style." No language needed; pure img2img. A small model can fully own this.
2. **Semantics** — which splits again:
   - *Edit operations* ("darker", "no background", "more shading here"): image-space transforms drawn from a small, closed vocabulary. Learnable by a small model given text conditioning and (input, instruction, output) triples.
   - *Subject disambiguation* ("a literal subject description that disambiguates wobbly strokes"): this works because the big model's world knowledge invents plausible fur, anatomy, perspective from the label. A 30M model has never seen a cat; no conditioning scheme conjures knowledge it doesn't have. This job stays with a pretrained model — full stop.

Three properties make this unusually cheap to own:

- **Single channel, ~16 effective gray levels.** The app quantizes to the panel's grays and applies grain at paint time. The model only ever needs to produce clean 8-bit grayscale. That's a fraction of the entropy of RGB photo generation — a small model can saturate it.
- **One style.** No prompt diversity to cover. The "style adherence" problem you're fighting with nano-banana-class models disappears when the model literally cannot draw anything else. This is the isometric-nyc lesson: he got ~50% style adherence from Nano Banana with heavy prompting, and near-total consistency from a 40-pair LoRA.
- **You already have a data factory.** `{cmd:"crop"}` produces the exact model input; `render-NNNN.skr` rasters are stored **clean** (pre-grain, autocontrast-stretched) — the exact training target. Every generation you keep is a curated pair; every output you rub out is a labeled reject; every "darker"-style annotation round-trip is an edit-training pair.

What a small model will **not** do: read handwriting, invent semantic detail from labels, decide crops/placement. But note pi already reads the page (it writes those prompts today) — so language can live in the *agent* rather than the image model. pi routes per call: default render → small model; disambiguation-heavy or novel instruction → big model. This routing is what makes the small model viable, and it also means the small model's text interface can be much narrower than English (see Track B).

## 2. What each reference contributes

**dino_diffusion** is the learning skeleton: a ~1-file PyTorch notebook, plain conv+ReLU UNet, direct prediction of the clean image, noise level as a scalar in [0,1], generation = 100 steps of "mix in a bit of the denoised prediction". Trained 512×512 RGB in a few hours on one RTX A4000. His addendum is the "serious run" recipe: IADB formulation (gaussian noise, [-1,1] images, predict `image − noise`) + stratified lognorm noise sampling. Your task is easier than his (1 channel, one narrow distribution), so his compute envelope is an upper bound.

**isometric-nyc** is the production playbook, and its problems are literally yours:

- ~40 input/output pairs → Qwen-Image-Edit LoRA on oxen.ai: ~4 h, ~$12, good results.
- Export weights → own GPU (Lambda, then Modal serverless): <$3/h, 200+ gens/h, 50 parallel workers.
- **Flat regions are the pathological case.** His water = your paper white. Edit-diffusion models hallucinate texture into flat areas because flat ≈ noise from the model's perspective. His fix: give flat regions a structured pattern (checkerboard) in training data, post-correct color after generation. Yours: keep grain out of targets (you already do), and if the model invents texture on empty paper, train with a faint deterministic paper tone and snap-to-white in the app.
- Infill conditioning (mask part of the input, model completes adjacent to existing output) is exactly your edit-in-place mechanism, and Qwen-Image-Edit learned it from a small dataset.
- Models can't QA their own outputs; plan for you as reviewer, and make review cheap with tooling.

## 3. Three tracks

### Track A — LoRA fine-tune of an open image-edit model (fastest to quality)

Fine-tune Qwen-Image-Edit (or successor open edit model) with a LoRA on your pairs.

- **Data:** 30–100 curated pairs. At ~1¢/generation you likely already have most of these on the device; otherwise a weekend of deliberate sketching. Curation matters more than count — normalize contrast (your extension already autocontrasts), include line-weight and density variety, include a few mostly-empty pages (the flat-region case).
- **Training:** fal.ai Qwen-Image-Edit trainer at [$0.002/step → ~$2–6 per run](https://fal.ai/models/fal-ai/qwen-image-edit-trainer); oxen.ai ~$12/4 h per the article; or self-run on a rented H100 ($2.9–4.6/h on [RunPod](https://www.runpod.io/pricing)) for ~$10–25. Budget 3–5 experimental runs: **$15–50 total**.
- **Hosting:** three shapes —
  - hosted LoRA endpoint (fal/Replicate): ~$0.02–0.08/image, zero ops, always warm;
  - Modal/RunPod serverless with scale-to-zero: per-second billing (~$0.0005/s for H100-class), a few cents/image warm, but 30–60 s cold starts — noticeable in the pause→render loop;
  - keep-warm worker only while sketching: ~$2–5 per sketching hour.
- **Prompts:** fully preserved. Qwen-Image-Edit is an instruction-following edit model; your LoRA pairs become (input, prompt, output) triples — pi already writes the prompt strings, so log them with the pairs. Disambiguation, edit instructions, even handwriting-reading survive.
- **Learning value:** moderate — dataset curation, LoRA mechanics, deployment. You don't touch the diffusion internals.
- **Risk:** it's a ~20B model; latency and cold starts are the UX tax. Quality risk is low — this recipe is proven on a task harder than yours.

### Track A′ — LoRA on a *small* pretrained edit model (the middle tier)

Same recipe as A, but on an edit model sized for a 24GB consumer GPU — as of mid-2026: [FLUX.2 klein](https://www.thundercompute.com/blog/best-open-source-image-generation-models) (low-latency, LoRA-friendly), [Step1X-Edit v1p2](https://firethering.com/best-open-source-ai-image-editing-models/) (FP8 + LoRA on a single 24GB card), Z-Image-Edit. You keep a real text encoder and pretrained world knowledge — so prompts, disambiguation, and instructions still work — at a size you can self-host on one rented 4090 ($0.69/h) or a local card, with seconds-not-tens-of-seconds latency. Style adherence from a LoRA on a smaller base is somewhat weaker than on Qwen-20B, but your single narrow style is the easy case. If the goal were *only* a cheap, prompt-capable model in your style, this track alone probably suffices; it just teaches you less than B.

### Track B — small conditioned diffusion from scratch (the learning project)

Extend dino_diffusion from unconditional to conditioned: concatenate the sketch as extra input channel(s), keep everything else bare-bones.

- **Architecture:** dino's UNet, ~15–40M params, input = noisy target (1ch) ⊕ sketch raster (1ch) ⊕ optional mask (1ch) for edit/infill later, output = 1ch. Start 256², move to 512². Use the addendum recipe (IADB + stratified lognorm). No attention, no text encoder, no VAE — pixel space is fine at this resolution/entropy.
- **Data:** from-scratch needs volume you won't hand-produce: **2k–20k pairs**. Synthesize them:
  - distill from Track A: run the LoRA over synthetic/collected sketch inputs — 5k pairs ≈ 25 H100-hours ≈ **$60–120** (or the same on fal at ~$0.03/img ≈ $150);
  - reverse-pair trick (pix2pix-era): take good renders, derive fake "sketches" via stroke simulation/XDoG edge extraction — nearly free, great for volume, mix with real pairs;
  - augment aggressively: random crops, stroke dropout, contrast jitter. Real curated pairs stay as the eval set.
- **Compute:** dino hit decent 512² RGB in a few hours on an A4000. Your single-channel narrow task: budget **20–60 GPU-hours including experiments** → $15–45 on a RunPod 4090 ($0.69/h) — or ~$0 marginal on a local 3090/4090/Colab.
- **Hosting — this is where it wins:** 30M params fp16 ≈ 60 MB. 50-step 512² sampling: well under 1 s on a 4090, a few seconds on an M-series Mac via MPS. Run it as a tiny server on your own machine on the same WiFi as the tablet — **$0/month, lower latency than Gemini**. (Not on the rm2 itself; the armv7 CPU is far too weak.) Later, distill to 4–8 steps (consistency/progressive distillation — a second learning arc) for near-instant renders and even CPU viability.
- **Instruction conditioning — two designs:**
  - *Free-text:* bolt a frozen pretrained text encoder (CLIP/T5-small) onto the UNet via cross-attention or FiLM, train on (input, instruction, output) triples. This is exactly the InstructPix2Pix recipe, including synthesizing the triples with the big model. Works for the edit-op distribution that actually appears in your logs; still can't do subject disambiguation (no world knowledge to draw on).
  - *Structured ops (cleaner for this app):* pi already reads handwriting and decides what to do — have it emit a structured op instead of free text: `{op: darker|lighter|remove_bg|more_detail|render, strength}`. Condition the UNet on a learned embedding of that vocabulary (plus the mask channel for edit-in-place, trained from your logged edit chains). Language stays in the agent, where it's already solved; the image model's conditioning space shrinks from "English" to a dozen ops × parameters, which slashes data needs and makes classifier-free guidance on the op embedding a tractable learning exercise.
- **Limitations:** even with conditioning, no world knowledge — subject-disambiguation calls route to Gemini/Track A′. Default renders and routine edits go local.
- **Risk:** medium. Fine-tuning is flimsy and from-scratch is flimsier; expect the flat-region hallucination fight and a few dead-end runs. That fight *is* the learning content.

### Track C — staged (recommended)

The tracks compose; A manufactures B's dataset:

1. **Instrument sketchbook now.** Log every (crop input, prompt string, clean output raster, accept/wipe/edit signal) record to a training dir synced off-device. Small change around the `{cmd:"crop"}` / `{cmd:"place"}` path (`src/pi_rpc.rs`, raster persistence in `src/ink.rs`). Data accrues from normal use from day one.
2. **Weeks 1–2 — Track A.** Curate ~50 pairs, LoRA on fal/oxen. You immediately get style consistency Gemini can't give you, plus a cheap pair-generator.
3. **In parallel — dino warm-up.** Port the notebook to conditional grayscale, train on just your ~50 real pairs at 256². It will memorize; watching *how* it fails teaches you plausibility/proportionality/originality on your own data.
4. **Weeks 3–6 — Track B for real.** Synthesize 5–10k pairs with the LoRA, train the small model, evaluate against held-out real pairs, wire it into pi as the default renderer with Gemini fallback.
5. **Later:** edit/infill conditioning from logged edit chains; few-step distillation; maybe retire the cloud entirely.

## 4. Cost summary

| Item | Track A only | Track C (A then B) |
|---|---|---|
| Hand-produced data | 30–100 pairs (mostly from normal use; ~$1 API) | same |
| Synthetic corpus | — | 5–10k pairs, $60–150 one-time |
| Training compute | $15–50 (LoRA runs) | + $15–45 cloud (or ~$0 local GPU) |
| Hosting | $0.02–0.08/image hosted, or cents/image serverless + cold starts | **~$0/mo** (local Mac/GPU), <1–5 s latency |
| **Total to working v1** | **~$20–60** | **~$100–250 all-cloud, <$100 with any local GPU** |
| Ongoing | scales with usage | ~nothing |

Your time is the real cost: A is a weekend or two; B/C is a 4–8 week evenings-scale project. Which is presumably the point.

## 5. Learning milestones (Track B)

Denoising objective and why x0-prediction works (dino post §"how denoising solves generation") → conditioning as channel concat (no cross-attention needed) → noise schedules and the IADB/stratified-lognorm upgrade → overfitting/memorization on tiny data, originality vs dataset size → the flat-region failure mode and data-augmentation fixes (isometric-nyc water saga) → evaluation without FID (your eye is fast on this domain; keep a fixed sketch test-set and diff outputs across checkpoints) → few-step distillation.

## 6. First concrete step

Add the logging hook. Everything downstream is bottlenecked on pairs, they're free to collect, and the accept/wipe signal you already emit through gestures is curation you'd otherwise have to redo by hand.

Sources: [RunPod pricing](https://www.runpod.io/pricing) · [RunPod serverless pricing docs](https://docs.runpod.io/serverless/pricing) · [fal Qwen-Image-Edit trainer](https://fal.ai/models/fal-ai/qwen-image-edit-trainer) · [fal Qwen-Image-Edit LoRA endpoint](https://fal.ai/models/fal-ai/qwen-image-edit-lora) · [isometric-nyc](https://cannoneyed.com/projects/isometric-nyc) · [dino_diffusion post](https://madebyoll.in/posts/dino_diffusion/) / [repo](https://github.com/madebyollin/dino-diffusion)
