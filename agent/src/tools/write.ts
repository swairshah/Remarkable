import { writeFileSync } from "fs";
import type { ToolArgs } from "../types";

export function writeFile(args: ToolArgs): string {
  writeFileSync(args.path!, args.content!);
  return "ok";
}
