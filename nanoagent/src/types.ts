export interface ToolArgs {
  path?: string;
  content?: string;
  offset?: number;
  limit?: number;
  old?: string;
  new?: string;
  all?: boolean;
  pat?: string;
  cmd?: string;
}

export type ToolFunction = (args: ToolArgs) => string;

export interface ToolDefinition {
  description: string;
  params: Record<string, string>;
  fn: ToolFunction;
}

export interface ChatMessage {
  role: "user" | "assistant";
  content: unknown;
}

export interface TextBlock {
  type: "text";
  text: string;
}

export interface ToolUseBlock {
  type: "tool_use";
  id: string;
  name: string;
  input: Record<string, unknown>;
}

export interface ToolResultBlock {
  type: "tool_result";
  tool_use_id: string;
  content: string;
}

export interface ApiToolResponse {
  content?: Array<TextBlock | ToolUseBlock | unknown>;
}
