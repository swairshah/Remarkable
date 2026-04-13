import { readFileSync } from "fs";
import type { ToolArgs } from "../types";

export function grepFiles(args: ToolArgs): string {
  const pattern = new RegExp(args.pat!);
  const basePath = args.path ?? ".";
  const files = [...new Bun.Glob(`${basePath}/**/*`).scanSync({ onlyFiles: true })];
  const hits: string[] = [];

  for (const filepath of files) {
    try {
      const lines = readFileSync(filepath, "utf-8").split("\n");
      lines.forEach((line, idx) => {
        if (pattern.test(line)) {
          hits.push(`${filepath}:${idx + 1}:${line}`);
        }
      });
    } catch {
      // Skip unreadable files.
    }

    if (hits.length >= 50) {
      break;
    }
  }

  return hits.slice(0, 50).join("\n") || "none";
}
