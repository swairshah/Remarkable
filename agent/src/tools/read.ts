import { readFileSync } from "fs";
import type { ToolArgs } from "../types";

export function readFile(args: ToolArgs): string {
  const lines = readFileSync(args.path!, "utf-8").split("\n");
  const offset = args.offset ?? 0;
  const limit = args.limit ?? lines.length;
  const selected = lines.slice(offset, offset + limit);

  return selected
    .map((line, idx) => `${String(offset + idx + 1).padStart(4)}| ${line}`)
    .join("\n");
}
