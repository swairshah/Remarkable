import { globFiles } from "./globTool";
import { grepFiles } from "./grep";
import { readFile } from "./read";
import { runBash } from "./bash";
import { writeFile } from "./write";
import { editFile } from "./edit";
import type { ToolArgs, ToolDefinition } from "../types";

export const TOOLS: Record<string, ToolDefinition> = {
  read: {
    description: "Read file with line numbers (file path, not directory)",
    params: { path: "string", offset: "integer?", limit: "integer?" },
    fn: readFile,
  },
  write: {
    description: "Write content to file",
    params: { path: "string", content: "string" },
    fn: writeFile,
  },
  edit: {
    description: "Replace old with new in file (old must be unique unless all=true)",
    params: { path: "string", old: "string", new: "string", all: "boolean?" },
    fn: editFile,
  },
  glob: {
    description: "Find files by pattern, sorted by mtime",
    params: { pat: "string", path: "string?" },
    fn: globFiles,
  },
  grep: {
    description: "Search files for regex pattern",
    params: { pat: "string", path: "string?" },
    fn: grepFiles,
  },
  bash: {
    description: "Run shell command",
    params: { cmd: "string" },
    fn: runBash,
  },
};

export function runTool(name: string, args: ToolArgs): string {
  const tool = TOOLS[name];
  if (!tool) {
    return `error: unknown tool ${name}`;
  }

  try {
    return tool.fn(args);
  } catch (err) {
    return `error: ${err}`;
  }
}

export function buildToolSchema() {
  return Object.entries(TOOLS).map(([name, { description, params }]) => {
    const properties: Record<string, { type: string }> = {};
    const required: string[] = [];

    for (const [paramName, rawType] of Object.entries(params)) {
      const isOptional = rawType.endsWith("?");
      const baseType = rawType.replace("?", "");
      properties[paramName] = { type: baseType };
      if (!isOptional) {
        required.push(paramName);
      }
    }

    return {
      name,
      description,
      input_schema: {
        type: "object",
        properties,
        required,
      },
    };
  });
}
