import { chmod, readFile, writeFile } from 'node:fs/promises';
import { homedir } from 'node:os';
import { join } from 'node:path';
import { configureProvider } from '@flue/runtime';
import { getOAuthApiKey, type OAuthCredentials } from '@earendil-works/pi-ai/oauth';

type PiAuthFile = Record<string, OAuthCredentials>;

const OPENAI_CODEX_PROVIDER = 'openai-codex';
const DEFAULT_PI_AUTH_PATH = join(homedir(), '.pi', 'agent', 'auth.json');

async function readPiAuth(path: string): Promise<PiAuthFile> {
  try {
    return JSON.parse(await readFile(path, 'utf8')) as PiAuthFile;
  } catch (error) {
    throw new Error(
      `Could not read Pi auth at ${path}. Run \`pi\`, use /login, and choose ChatGPT Plus/Pro (Codex).`,
      { cause: error },
    );
  }
}

async function writePiAuth(path: string, auth: PiAuthFile) {
  await writeFile(path, `${JSON.stringify(auth, null, 2)}\n`, { mode: 0o600 });
  await chmod(path, 0o600).catch(() => undefined);
}

export async function configurePiCodexProvider() {
  const explicitApiKey = process.env.OPENAI_CODEX_API_KEY;
  if (explicitApiKey) {
    configureProvider(OPENAI_CODEX_PROVIDER, { apiKey: explicitApiKey });
    return;
  }

  const authPath = process.env.PI_AUTH_FILE ?? DEFAULT_PI_AUTH_PATH;
  const auth = await readPiAuth(authPath);
  const before = JSON.stringify(auth[OPENAI_CODEX_PROVIDER]);
  const resolved = await getOAuthApiKey(OPENAI_CODEX_PROVIDER, auth);

  if (!resolved?.apiKey) {
    throw new Error(
      `No ${OPENAI_CODEX_PROVIDER} OAuth credentials found in ${authPath}. Run \`pi\`, use /login, and choose ChatGPT Plus/Pro (Codex).`,
    );
  }

  auth[OPENAI_CODEX_PROVIDER] = resolved.newCredentials;
  if (JSON.stringify(auth[OPENAI_CODEX_PROVIDER]) !== before) {
    await writePiAuth(authPath, auth);
  }

  configureProvider(OPENAI_CODEX_PROVIDER, { apiKey: resolved.apiKey });
}
