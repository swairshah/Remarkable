---
title: Writing for the blog pipeline
description: Handwriting conventions and proofreader's marks the Writings-notebook publish agent understands.
tags:
  - papier
  - publishing
  - blog
---
# Writing for the blog pipeline

How to write in the **Writings** notebook so [papier-publish.sh](./server/bin/papier-publish.sh) typesets it well. The agent reads diff pages, merges your ink into `posts/<slug>/post.md`, and pushes to swair.dev; these conventions are the contract between your pen and that agent.

## What the agent now does on its own

- Re-paragraphs your ink: merges fragments into flowing prose, breaks where the argument turns, and avoids one-sentence orphan paragraphs.
- Converts long discursive numbered lists into prose; keeps only short, parallel lists.
- Sets verbatim prompts/quotes as blockquotes without quotation marks.
- Limits `##` sections to a few per post, each with at least two paragraphs.

By default it still preserves your words verbatim — it edits structure, not voice.

## Proofreader's marks

Write these on the page; the agent treats them as typesetting commands, not prose:

| Mark | Meaning |
| --- | --- |
| `¶` before a line | Force a paragraph break exactly here |
| Curved line connecting two blocks | Run them together into one paragraph |
| Double-underlined line | Post title (starts/names a post) |
| Single-underlined short line, or `##` | Section heading |
| Line starting with `>` | Blockquote |
| Boxed text, or `[aside]` | Margin sidenote |
| `[polish]` at top of a page/section | Allow light copy-editing of that material |
| `[do: …]` | Authoring directive fulfilled in place (code block, SVG, crop, …) |
| `$…$` and `$$…$$` | Inline / display math |
| ```` ```lang ```` fence | Code block |
| Erased ink | Delete the corresponding published content |
| Deleted page | Remove that page's content from its post |

## Habits that read well

- One new topic → write a fresh double-underlined title; the agent then makes a separate post instead of merging.
- Margin notes next to a paragraph become sidenotes — use them for qualifications and references instead of parenthetical asides.
- Don't hand-wrap thoughts to the page edge mid-sentence and start a new visual block: the agent joins them, but a `¶` or connecting line removes any ambiguity.
- If a passage is a rough thought you want smoothed, head it `[polish]` — otherwise it is transcribed as written.
