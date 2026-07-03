import test from 'node:test';
import assert from 'node:assert/strict';
import { normalizeEmailAddress, requireAllowlistedSender } from '../src/email.ts';

test('normalizeEmailAddress accepts simple emails', () => {
  const result = normalizeEmailAddress('USER@example.com');
  assert.equal(result.ok, true);
  if (result.ok) {
    assert.equal(result.value, 'user@example.com');
  }
});

test('requireAllowlistedSender rejects unknown senders', () => {
  const result = requireAllowlistedSender('user@example.com', ['friend@example.com']);
  assert.equal(result.ok, false);
  if (!result.ok) {
    assert.equal(result.error._tag, 'Unauthorized');
  }
});
