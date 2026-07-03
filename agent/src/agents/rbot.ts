import { createAgent, type AgentRouteHandler } from '@flue/runtime';
import { configurePiCodexProvider } from '../pi-codex-auth.ts';
import { BROWSER_RUN_AGENT_INSTRUCTIONS, createBrowserRunTools } from '../tools/browser-run.ts';

await configurePiCodexProvider();

export const route: AgentRouteHandler = async (_c, next) => next();

export default createAgent((context) => ({
  model: 'openai-codex/gpt-5.5',
  thinkingLevel: 'high',
  skills: [],
  tools: [...createBrowserRunTools(context.env as any)],
  instructions: `Use the browser_run tool for live web browsing and URL inspection.

${BROWSER_RUN_AGENT_INSTRUCTIONS}`,
}));
