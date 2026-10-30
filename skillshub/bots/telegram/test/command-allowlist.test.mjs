import assert from 'node:assert/strict';
import test from 'node:test';
import { Telegraf } from 'telegraf';
import { TelegramAdapter } from '../dist/adapter.js';

// Constructing Telegraf makes no network call; only the allowlist is exercised.
const adapter = new TelegramAdapter(new Telegraf('12345:fixture-token'), { allowedCommandNames: ['/magic'] });

test('the owner god-mode commands reach the backend', () => {
  for (const text of ['/god', '/godoff', '/GOD', '/god off', '/god@MagicianBot', '/godoff@MagicianBot']) {
    assert.equal(adapter.acceptsText(text), true, text);
  }
});

test('the owner claim command reaches the backend', () => {
  for (const text of ['/claim ABCD2345', '/claim', '/CLAIM abcd2345', '/claim@MagicianBot ABCD2345']) {
    assert.equal(adapter.acceptsText(text), true, text);
  }
});

test('SDK commands, control prefixes and plain text still pass; unknown commands do not', () => {
  for (const text of ['/new', '/stop', '/queue clear', '/magic /god', 'hello /god']) {
    assert.equal(adapter.acceptsText(text), true, text);
  }
  for (const text of ['/gods', '/godmode', '/claims', '/unknown']) {
    assert.equal(adapter.acceptsText(text), false, text);
  }
});
