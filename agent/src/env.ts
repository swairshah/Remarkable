import { existsSync, readFileSync } from "fs";

function stripQuotes(value: string): string {
  if (
    (value.startsWith('"') && value.endsWith('"')) ||
    (value.startsWith("'") && value.endsWith("'"))
  ) {
    return value.slice(1, -1);
  }
  return value;
}

export function loadDotEnv(path = ".env"): void {
  if (!existsSync(path)) {
    return;
  }

  const content = readFileSync(path, "utf-8");

  for (const line of content.split(/\r?\n/)) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith("#")) {
      continue;
    }

    const match = /^([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.*)$/.exec(trimmed);
    if (!match) {
      continue;
    }

    const [, key, rawVal] = match;
    const value = stripQuotes(rawVal.trim());

    if (!(key in process.env)) {
      process.env[key] = value;
    }
  }
}
