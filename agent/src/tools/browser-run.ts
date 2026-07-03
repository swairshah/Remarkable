// Standalone Browser Run tool implementation (adapted for SwairBot)
import { Type, defineTool } from '@flue/runtime';

export type BrowserRunBinding = {
  quickAction: (action: string, options: Record<string, unknown>) => Promise<Response | Record<string, unknown> | string>;
};

const MAX_TEXT_CHARS = 12_000;

const browserRunAction = Type.Union([
  Type.Literal('markdown'),
  Type.Literal('links'),
  Type.Literal('content'),
  Type.Literal('json'),
  Type.Literal('scrape'),
  Type.Literal('crawl'),
  Type.Literal('snapshot'),
  Type.Literal('screenshot'),
  Type.Literal('pdf'),
]);

function validatePublicUrl(input: string | undefined) {
  if (!input) {
    return undefined;
  }

  const url = new URL(input);
  if (url.protocol !== 'https:' && url.protocol !== 'http:') {
    throw new Error('Browser Run only accepts http:// or https:// URLs.');
  }

  return url.toString();
}

function truncateText(text: string) {
  if (text.length <= MAX_TEXT_CHARS) {
    return { text, truncated: false };
  }

  return {
    text: `${text.slice(0, MAX_TEXT_CHARS)}\n\n[truncated: request a narrower page, selector, or URL if more detail is needed]`,
    truncated: true,
  };
}

async function normalizeQuickActionResult(result: Response | Record<string, unknown> | string) {
  if (typeof result === 'string') {
    const truncated = truncateText(result);
    return { output: truncated.text, truncated: truncated.truncated, contentType: 'text/plain' };
  }

  if (result instanceof Response) {
    const contentType = result.headers.get('content-type') ?? 'application/octet-stream';
    const browserMs = result.headers.get('x-browser-ms-used');

    if (/^(text\/|application\/(json|xml|xhtml\+xml))/.test(contentType)) {
      const body = await result.text();
      const truncated = truncateText(body);
      return {
        output: truncated.text,
        truncated: truncated.truncated,
        status: result.status,
        contentType,
        browserMs,
      };
    }

    const bytes = await result.arrayBuffer();
    return {
      output: `Captured ${bytes.byteLength} bytes of ${contentType}. Binary output is available to the application, but only metadata is returned to the model context.`,
      truncated: false,
      status: result.status,
      contentType,
      browserMs,
      bytes: bytes.byteLength,
    };
  }

  const serialized = JSON.stringify(result, null, 2);
  const truncated = truncateText(serialized);
  return {
    output: truncated.text,
    truncated: truncated.truncated,
    contentType: 'application/json',
  };
}

export function createBrowserRunTools(env: { BROWSER?: BrowserRunBinding }) {
  return [
    defineTool({
      name: 'browser_run',
      description:
        'Browse and navigate the live web with Cloudflare Browser Run. Use this when a user asks for current information, asks you to inspect a URL, compare pages, extract links, summarize rendered content, or gather citations.',
      parameters: Type.Object({
        action: browserRunAction,
        url: Type.Optional(Type.String({ description: 'HTTP(S) URL to open or navigate to.' })),
        html: Type.Optional(Type.String({ description: 'Inline HTML to render instead of opening a URL.' })),
        prompt: Type.Optional(Type.String({ description: 'Extraction instructions. Keep narrow and specific.' })),
        selector: Type.Optional(Type.String({ description: 'CSS selector for scrape requests.' })),
        formats: Type.Optional(
          Type.Array(
            Type.Union([
              Type.Literal('html'),
              Type.Literal('markdown'),
              Type.Literal('screenshot'),
              Type.Literal('accessibilityTree'),
            ]),
            { description: 'Snapshot formats. Use at least two formats for snapshot.' },
          ),
        ),
      }),
      async execute(args) {
        if (!env.BROWSER?.quickAction) {
          throw new Error('Browser Run quickAction binding is not available.');
        }

        const params = args as Record<string, any>;
        const url = validatePublicUrl(params.url);
        if (!url && !params.html) {
          throw new Error('Browser Run requires either url or html.');
        }

        const options = {
          ...(url ? { url } : {}),
          ...(params.html ? { html: params.html } : {}),
          ...(params.prompt ? { prompt: params.prompt } : {}),
          ...(params.selector ? { selector: params.selector } : {}),
          ...(params.formats ? { formats: params.formats } : {}),
        };

        const result = await env.BROWSER.quickAction(params.action, options);
        const normalized = await normalizeQuickActionResult(result);
        return JSON.stringify({ action: params.action, url, ...normalized }, null, 2);
      },
    }),
  ];
}

export const BROWSER_RUN_AGENT_INSTRUCTIONS = `
<browser_run_tool>
Use the browser_run tool for live web browsing, URL inspection, rendered-page reading, link discovery, structured extraction, screenshots, PDFs, and small crawls.
</browser_run_tool>`;
