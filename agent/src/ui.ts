// ANSI helper constants and terminal formatting helpers
export const RESET = "\x1b[0m";
export const BOLD = "\x1b[1m";
export const DIM = "\x1b[2m";
export const BLUE = "\x1b[34m";
export const CYAN = "\x1b[36m";
export const GREEN = "\x1b[32m";
export const RED = "\x1b[31m";

export function separator(): string {
  const cols = process.stdout.columns ?? 80;
  return `${DIM}${"─".repeat(Math.min(cols, 80))}${RESET}`;
}

export function renderMarkdown(text: string): string {
  return text.replace(/\*\*(.+?)\*\*/g, `${BOLD}$1${RESET}`);
}
