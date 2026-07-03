import { execSync } from "child_process";
import type { ToolArgs } from "../types";

export function runBash(args: ToolArgs): string {
  try {
    const result = execSync(args.cmd!, {
      encoding: "utf-8",
      timeout: 30000,
      stdio: ["pipe", "pipe", "pipe"],
    });

    return result.trim() || "(empty)";
  } catch (err: unknown) {
    const execErr = err as { stdout?: string; stderr?: string };
    return ((execErr.stdout ?? "") + (execErr.stderr ?? "")).trim() || "(empty)";
  }
}
