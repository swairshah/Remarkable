#!/usr/bin/env bun

import { existsSync, mkdirSync, readdirSync, readFileSync, statSync, writeFileSync } from "fs";
import { dirname, extname, isAbsolute, join, relative, resolve } from "path";
import { callApi, extractText } from "./api";
import { loadDotEnv } from "./env";

interface CliOptions {
  dir: string;
  statusFile: string;
  userPrompt: string;
}

interface FileSnapshotEntry {
  size: number;
  mtimeMs: number;
}

type DirectorySnapshot = Record<string, FileSnapshotEntry>;

interface DiffResult {
  added: string[];
  removed: string[];
  modified: string[];
}

const SNAPSHOT_START = "<!-- SNAPSHOT_START -->";
const SNAPSHOT_END = "<!-- SNAPSHOT_END -->";
const IMAGE_EXTENSIONS = new Set([".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp", ".tif", ".tiff", ".svg", ".heic"]);

function parseArgs(argv: string[]): CliOptions {
  let dir = "";
  let statusFile = "./monitor-status.md";
  const promptParts: string[] = [];

  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];

    if ((arg === "--dir" || arg === "-d") && argv[i + 1]) {
      dir = argv[i + 1];
      i += 1;
      continue;
    }

    if ((arg === "--status-file" || arg === "-s") && argv[i + 1]) {
      statusFile = argv[i + 1];
      i += 1;
      continue;
    }

    if ((arg === "--prompt" || arg === "-p") && argv[i + 1]) {
      promptParts.push(argv[i + 1]);
      i += 1;
      continue;
    }

    if (arg === "--help" || arg === "-h") {
      printHelpAndExit();
    }
  }

  if (!dir) {
    throw new Error("Missing required argument: --dir <path>");
  }

  return {
    dir,
    statusFile,
    userPrompt: promptParts.join(" ").trim(),
  };
}

function printHelpAndExit(): never {
  console.log(`Directory monitor agent\n\nUsage:\n  bun run src/index.ts --dir <path> [--status-file <path>] [-p \"extra instructions\"]\n\nOptions:\n  -d, --dir          Directory to monitor (required)\n  -s, --status-file  Markdown status file path (default: ./monitor-status.md)\n  -p, --prompt       Extra prompt instructions for activity summary\n  -h, --help         Show this help`);
  process.exit(0);
}

function toAbsolutePath(inputPath: string): string {
  return isAbsolute(inputPath) ? inputPath : resolve(process.cwd(), inputPath);
}

function collectSnapshot(rootDir: string, ignoreAbsolutePath?: string): DirectorySnapshot {
  const snapshot: DirectorySnapshot = {};

  const walk = (currentPath: string) => {
    const entries = readdirSync(currentPath, { withFileTypes: true });

    for (const entry of entries) {
      const fullPath = join(currentPath, entry.name);

      if (ignoreAbsolutePath && fullPath === ignoreAbsolutePath) {
        continue;
      }

      if (entry.isDirectory()) {
        walk(fullPath);
        continue;
      }

      if (!entry.isFile()) {
        continue;
      }

      const stats = statSync(fullPath);
      const relPath = relative(rootDir, fullPath).replaceAll("\\", "/");
      snapshot[relPath] = {
        size: stats.size,
        mtimeMs: Math.round(stats.mtimeMs),
      };
    }
  };

  walk(rootDir);
  return snapshot;
}

function parseSnapshotFromStatus(markdown: string): DirectorySnapshot {
  const startIdx = markdown.indexOf(SNAPSHOT_START);
  const endIdx = markdown.indexOf(SNAPSHOT_END);

  if (startIdx === -1 || endIdx === -1 || endIdx <= startIdx) {
    return {};
  }

  const raw = markdown.slice(startIdx + SNAPSHOT_START.length, endIdx).trim();
  if (!raw) {
    return {};
  }

  try {
    return JSON.parse(raw) as DirectorySnapshot;
  } catch {
    return {};
  }
}

function diffSnapshots(previous: DirectorySnapshot, current: DirectorySnapshot): DiffResult {
  const added: string[] = [];
  const removed: string[] = [];
  const modified: string[] = [];

  for (const path of Object.keys(current)) {
    if (!previous[path]) {
      added.push(path);
      continue;
    }

    const before = previous[path];
    const after = current[path];
    if (before.size !== after.size || before.mtimeMs !== after.mtimeMs) {
      modified.push(path);
    }
  }

  for (const path of Object.keys(previous)) {
    if (!current[path]) {
      removed.push(path);
    }
  }

  return {
    added: added.sort(),
    removed: removed.sort(),
    modified: modified.sort(),
  };
}

function buildPrompt(monitoredDir: string, diff: DiffResult, userPrompt: string): string {
  const changedPaths = [...diff.added, ...diff.modified, ...diff.removed];
  const imageChanges = changedPaths.filter((path) => IMAGE_EXTENSIONS.has(extname(path).toLowerCase()));

  return [
    `Monitored directory: ${monitoredDir}`,
    `Change summary:`,
    `- Added (${diff.added.length}): ${diff.added.slice(0, 20).join(", ") || "none"}`,
    `- Modified (${diff.modified.length}): ${diff.modified.slice(0, 20).join(", ") || "none"}`,
    `- Removed (${diff.removed.length}): ${diff.removed.slice(0, 20).join(", ") || "none"}`,
    `- Image-related changes (${imageChanges.length}): ${imageChanges.slice(0, 20).join(", ") || "none"}`,
    userPrompt ? `Extra instructions (-p): ${userPrompt}` : "Extra instructions (-p): none",
    "Write a concise activity log for this run with suggested next action.",
  ].join("\n");
}

function fallbackLog(diff: DiffResult): string {
  if (diff.added.length === 0 && diff.modified.length === 0 && diff.removed.length === 0) {
    return "No changes detected in this run.";
  }

  return [
    `Detected ${diff.added.length + diff.modified.length + diff.removed.length} changed file(s).`,
    diff.added.length ? `Added: ${diff.added.join(", ")}` : "Added: none",
    diff.modified.length ? `Modified: ${diff.modified.join(", ")}` : "Modified: none",
    diff.removed.length ? `Removed: ${diff.removed.join(", ")}` : "Removed: none",
  ].join("\n");
}

async function generateActivityLog(prompt: string, hasChanges: boolean): Promise<string> {
  if (!process.env.ANTHROPIC_API_KEY) {
    return hasChanges ? "Changes detected. Set ANTHROPIC_API_KEY to generate richer activity logs." : "No changes detected.";
  }

  try {
    const response = await callApi(
      [{ role: "user", content: prompt }],
      "You are a file activity monitor. Summarize changes clearly for an operations log.",
      { includeTools: false }
    );

    const text = extractText(response).trim();
    return text || "No summary generated.";
  } catch (err) {
    return `Activity summary generation failed: ${String(err)}`;
  }
}

function renderMarkdownStatus(
  monitoredDir: string,
  nowIso: string,
  diff: DiffResult,
  activityLog: string,
  promptUsed: string,
  snapshot: DirectorySnapshot,
  previousMarkdown = ""
): string {
  const status = diff.added.length || diff.modified.length || diff.removed.length ? "CHANGED" : "NO_CHANGES";

  const previousActivityMatch = /## Activity Log\n([\s\S]*?)\n## Latest Prompt Used/.exec(previousMarkdown);
  const previousActivity = previousActivityMatch ? previousActivityMatch[1].trim() : "";

  const newEntry = [
    `### ${nowIso}`,
    `- Status: ${status}`,
    `- Added: ${diff.added.length}`,
    `- Modified: ${diff.modified.length}`,
    `- Removed: ${diff.removed.length}`,
    "",
    activityLog,
  ].join("\n");

  const mergedActivity = [newEntry, previousActivity].filter(Boolean).join("\n\n").trim();

  return [
    "# Directory Monitor Status",
    "",
    `- Monitored directory: \`${monitoredDir}\``,
    `- Last run: ${nowIso}`,
    `- Last status: ${status}`,
    "",
    "## Activity Log",
    mergedActivity,
    "",
    "## Latest Prompt Used",
    "```text",
    promptUsed,
    "```",
    "",
    SNAPSHOT_START,
    JSON.stringify(snapshot, null, 2),
    SNAPSHOT_END,
    "",
  ].join("\n");
}

async function main() {
  loadDotEnv();

  const options = parseArgs(process.argv.slice(2));
  const monitoredDir = toAbsolutePath(options.dir);
  const statusFile = toAbsolutePath(options.statusFile);

  if (!existsSync(monitoredDir)) {
    throw new Error(`Directory does not exist: ${monitoredDir}`);
  }

  const statusDir = dirname(statusFile);
  if (!existsSync(statusDir)) {
    mkdirSync(statusDir, { recursive: true });
  }

  const previousMarkdown = existsSync(statusFile) ? readFileSync(statusFile, "utf-8") : "";
  const previousSnapshot = parseSnapshotFromStatus(previousMarkdown);

  const currentSnapshot = collectSnapshot(monitoredDir, statusFile);
  const diff = diffSnapshots(previousSnapshot, currentSnapshot);
  const prompt = buildPrompt(monitoredDir, diff, options.userPrompt);

  const hasChanges = diff.added.length > 0 || diff.modified.length > 0 || diff.removed.length > 0;
  const summary = hasChanges ? await generateActivityLog(prompt, true) : "No changes detected in this run.";
  const activityLog = summary.startsWith("Activity summary generation failed") ? `${summary}\n\n${fallbackLog(diff)}` : summary;

  const nowIso = new Date().toISOString();
  const markdown = renderMarkdownStatus(monitoredDir, nowIso, diff, activityLog, prompt, currentSnapshot, previousMarkdown);

  writeFileSync(statusFile, markdown, "utf-8");

  console.log(`Updated monitor status: ${statusFile}`);
  console.log(`Run status: ${hasChanges ? "CHANGED" : "NO_CHANGES"}`);
}

main().catch((err) => {
  console.error(`Error: ${String(err)}`);
  process.exit(1);
});
