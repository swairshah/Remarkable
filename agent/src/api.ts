import { API_URL, MODEL } from "./constants";
import { buildToolSchema } from "./tools/registry";
import type { ApiToolResponse, ChatMessage, TextBlock } from "./types";

interface ApiErrorPayload {
  error?: {
    message?: string;
  } | string;
}

interface CallApiOptions {
  includeTools?: boolean;
}

export async function callApi(
  messages: ChatMessage[],
  systemPrompt: string,
  options: CallApiOptions = {}
): Promise<ApiToolResponse> {
  const includeTools = options.includeTools ?? true;

  const response = await fetch(API_URL, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      "x-api-key": process.env.ANTHROPIC_API_KEY ?? "",
      "anthropic-version": "2023-06-01",
    },
    body: JSON.stringify({
      model: MODEL,
      max_tokens: 8192,
      system: systemPrompt,
      messages,
      ...(includeTools ? { tools: buildToolSchema() } : {}),
    }),
  });

  const text = await response.text();
  let parsed: ApiToolResponse;

  try {
    parsed = JSON.parse(text) as ApiToolResponse;
  } catch {
    throw new Error(`Invalid API response: ${text || "empty"}`);
  }

  if (!response.ok) {
    const apiError = parsed as ApiErrorPayload;
    const errMsg =
      typeof apiError.error === "string"
        ? apiError.error
        : apiError.error?.message || text || "Request failed";
    throw new Error(`Anthropic API error (${response.status} ${response.statusText}): ${errMsg}`);
  }

  return parsed;
}

export function extractText(response: ApiToolResponse): string {
  return (response.content ?? [])
    .map((block) => {
      const typed = block as { type?: string; text?: string };
      if (typed.type === "text") {
        return (block as TextBlock).text;
      }
      return "";
    })
    .filter(Boolean)
    .join("\n")
    .trim();
}
