import { statSync } from "fs";
import type { ToolArgs } from "../types";

export function globFiles(args: ToolArgs): string {
  const basePath = args.path ?? ".";
  const pattern = `${basePath}/${args.pat!}`.replace("//", "/");
  const files = [...new Bun.Glob(pattern).scanSync({ onlyFiles: false })];

  const sorted = files
    .map((f) => {
      try {
        return { path: f, mtime: statSync(f).mtimeMs };
      } catch {
        return { path: f, mtime: 0 };
      }
    })
    .sort((a, b) => b.mtime - a.mtime)
    .map((f) => f.path);

  return sorted.join("\n") || "none";
}
