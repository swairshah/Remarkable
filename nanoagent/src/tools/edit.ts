import { readFileSync, writeFileSync } from "fs";
import type { ToolArgs } from "../types";

export function editFile(args: ToolArgs): string {
  const text = readFileSync(args.path!, "utf-8");
  const old = args.old!;
  const replacement = args.new!;

  if (!text.includes(old)) {
    return "error: old_string not found";
  }

  const count = text.split(old).length - 1;
  if (!args.all && count > 1) {
    return `error: old_string appears ${count} times, must be unique (use all=true)`;
  }

  const next = args.all ? text.replaceAll(old, replacement) : text.replace(old, replacement);
  writeFileSync(args.path!, next);
  return "ok";
}
