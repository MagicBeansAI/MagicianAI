import assert from 'node:assert/strict';
import test from 'node:test';
import { AgentMailAdapter } from '../dist/adapter.js';
import { ChannelRuntime } from '../../sdk/dist/index.js';

test('tracked replies explicitly bind the only recipient and preserve every body character', async () => {
  const adapter = new AgentMailAdapter({ apiKey: 'unused-test-key', webhookPort: 0 });
  const calls = [];
  adapter.client = { inboxes: { messages: { async reply(...args) { calls.push(args); return { message_id: 'accepted' }; } } } };
  adapter.replyContexts.set('guest@example.com', { inboxId: 'inbox', messageId: 'source' });
  assert.deepEqual(await adapter.sendText('guest@example.com', '  Exact\nwords 🌼 ', { preserveExactText: true }), { accepted: true });
  assert.deepEqual(calls[0].slice(0, 3), ['inbox', 'source', { text: '  Exact\nwords 🌼 ', to: ['guest@example.com'], cc: [], bcc: [] }]);
  assert.equal(calls[0][3].maxRetries, 0);
  assert.ok(calls[0][3].abortSignal instanceof AbortSignal);
  await adapter.sendText('guest@example.com', ' Normal reply ');
  assert.deepEqual(calls[1][2], { text: 'Normal reply' });
  assert.equal(calls[1][3], undefined);
});

test('the real AgentMail SDK makes only one POST when a tracked send receives a retryable error', async (t) => {
  let calls = 0;
  t.mock.method(globalThis, 'fetch', async (_url, options) => {
    assert.equal(options.method, 'POST');
    calls++;
    return new Response(JSON.stringify({ error: 'response lost after acceptance' }), { status: 500 });
  });
  t.mock.method(console, 'error', () => {});
  const adapter = new AgentMailAdapter({ apiKey: 'unused-test-key', webhookPort: 0 });
  adapter.replyContexts.set('guest@example.com', { inboxId: 'inbox', messageId: 'source' });
  assert.deepEqual(await adapter.sendText('guest@example.com', 'Exact words', { preserveExactText: true }), { accepted: false });
  assert.equal(calls, 1);
});

test('a response without a provider message id cannot acknowledge a tracked send', async () => {
  const adapter = new AgentMailAdapter({ apiKey: 'unused-test-key', webhookPort: 0 });
  adapter.client = { inboxes: { messages: { async reply() { return {}; } } } };
  adapter.replyContexts.set('guest@example.com', { inboxId: 'inbox', messageId: 'source' });
  assert.deepEqual(await adapter.sendText('guest@example.com', 'Exact words', { preserveExactText: true }), { accepted: false });
});

test('the real SDK deadline includes successful and error response bodies without resending mail', async (t) => {
  t.mock.method(console, 'error', () => {});
  t.mock.timers.enable({ apis: ['setTimeout'] });
  t.mock.method(AbortSignal, 'timeout', (ms) => {
    assert.ok(ms > 0 && ms <= 30_000);
    const controller = new AbortController();
    setTimeout(() => controller.abort(new DOMException('Provider deadline exceeded', 'TimeoutError')), ms);
    return controller.signal;
  });
  for (const status of [200, 500]) {
    let sends = 0;
    let readingBody = false;
    let rejectBody;
    const fetchMock = t.mock.method(globalThis, 'fetch', async (_url, options) => {
      sends++;
      return { status, headers: new Headers(), body: {}, async text() {
        readingBody = true;
        return new Promise((_resolve, reject) => {
          rejectBody = reject;
          options.signal.addEventListener('abort', () => reject(options.signal.reason), { once: true });
        });
      } };
    });
    const adapter = new AgentMailAdapter({ apiKey: 'unused-test-key', webhookPort: 0 });
    adapter.replyContexts.set('guest@example.com', { inboxId: 'inbox', messageId: 'source' });
    const phases = [];
    const runtime = new ChannelRuntime({ channelType: 'agentmail', adapter, magician: {
      async reportEnvoyDelivery(_session, _message, phase) {
        phases.push(phase);
        return { tracked: true, send: phase === 'begin', status: phase };
      }
    } });
    const message = { id: 'reply-1', session_id: 'session-1', direction: 'assistant', content: { type: 'text', text: 'Exact words' }, created_at: 1 };
    let settled = false;
    const delivery = runtime.deliverChatMessage('guest@example.com', 'session-1', message).finally(() => { settled = true; });
    try {
      await new Promise(resolve => setImmediate(resolve));
      assert.equal(readingBody, true);
      t.mock.timers.tick(60_001); // Past even the provider SDK's own header timeout.
      await new Promise(resolve => setImmediate(resolve));
      assert.equal(settled, true, 'a stalled response body must release the delivery');
      await delivery;
      await runtime.deliverChatMessage('guest@example.com', 'session-1', message);
      assert.equal(sends, 1);
      assert.deepEqual(phases, ['begin', 'unknown']);
      assert.equal(runtime.activeDeliveries.size, 0);
    } finally {
      rejectBody?.(new Error('test cleanup'));
      await delivery;
      await runtime.stop(); fetchMock.mock.restore();
    }
  }
});
