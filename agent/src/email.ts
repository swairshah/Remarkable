// Standalone email utilities (no external Result dependency)

export type AppError = { _tag: 'InvalidInput' | 'Unauthorized'; message: string };
export type AppResult<T> = { ok: true; value: T } | { ok: false; error: AppError };

function ok<T>(value: T): AppResult<T> {
  return { ok: true, value };
}

function err<T>(error: AppError): AppResult<T> {
  return { ok: false, error };
}

function invalidInput(message: string): AppError {
  return { _tag: 'InvalidInput', message };
}

function unauthorized(message: string): AppError {
  return { _tag: 'Unauthorized', message };
}

export function normalizeEmailAddress(input: string | undefined | null): AppResult<string> {
  if (!input) {
    return err<string>(invalidInput('Missing email address'));
  }

  const bracketMatch = input.match(/<([^<>]+)>/);
  const candidate = (bracketMatch?.[1] ?? input).trim().toLowerCase();

  if (!/^[^\s@<>]+@[^\s@<>]+\.[^\s@<>]+$/.test(candidate)) {
    return err<string>(invalidInput('Invalid email address'));
  }

  return ok(candidate);
}

export function requireAllowlistedSender(input: string, allowlist: string[]): AppResult<string> {
  const normalized = normalizeEmailAddress(input);
  if (!normalized.ok) {
    return normalized;
  }

  if (!allowlist.includes(normalized.value)) {
    return err<string>(unauthorized('Sender is not allowlisted'));
  }

  return ok(normalized.value);
}

export function emailErrorMessage(error: AppError): string {
  if (error._tag === 'Unauthorized') {
    return 'This address is not authorized.';
  }
  return 'The email could not be processed safely.';
}

export async function sendEmail(
  env: { AGENT_FROM_EMAIL?: string; EMAIL: { send: (input: unknown) => Promise<unknown> } },
  message: { to: string | string[]; subject: string; text: string; headers?: Record<string, string> },
) {
  const from = env.AGENT_FROM_EMAIL ?? 'agent@your-domain.example';
  return env.EMAIL.send({
    from: { email: from, name: 'SwairBot' },
    to: message.to,
    subject: message.subject,
    text: message.text,
    headers: message.headers,
  });
}
