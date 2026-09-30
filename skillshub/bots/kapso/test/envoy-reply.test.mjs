import assert from 'node:assert/strict';
import test from 'node:test';
import { KapsoAdapter } from '../dist/adapter.js';
import { ChannelRuntime } from '../../sdk/dist/index.js';

function fixture(failAt = 1) {
  const adapter = new KapsoAdapter({ apiKey: 'unused-test-key', phoneNumberId: 'phone', webhookPort: 0,
    webhookSecret: 'test-secret', fallbackTemplateName: 'fallback' });
  const calls = [];
  let texts = 0;
  adapter.client = { messages: {
    async sendText(input) {
      calls.push({ kind: 'text', ...input });
      if (++texts === failAt) throw { category: 'reengagementWindow', code: 131047 };
      return { messages: [{ id: `message-${texts}` }] };
    },
    async sendTemplate(input) { calls.push({ kind: 'template', ...input }); }
  } };
  return { adapter, calls };
}

test('tracked replies never substitute a template, including after partial chunk delivery', async (t) => {
  t.mock.method(console, 'info', () => {});
  for (const failAt of [1, 2]) {
    const { adapter, calls } = fixture(failAt);
    const text = 'x'.repeat(4500);
    assert.deepEqual(await adapter.sendText('guest', text, { preserveExactText: true, allowOutsideWindowTemplate: true }), { accepted: false });
    assert.equal(calls.length, failAt);
    assert.ok(calls.every(call => call.kind === 'text'));
    assert.equal(calls.map(call => call.body).join(''), text.slice(0, failAt * 4000));
  }
});

test('ordinary channel replies retain their configured template fallback', async (t) => {
  t.mock.method(console, 'info', () => {});
  const { adapter, calls } = fixture();
  assert.deepEqual(await adapter.sendText('guest', 'Normal words'), { accepted: false });
  assert.deepEqual(calls.map(call => call.kind), ['text', 'template']);
});

// Keep the installed provider SDK in the path: it parses HTTP responses but
// does not validate a send receipt or impose a request deadline.
function runtimeFixture() {
  const adapter = new KapsoAdapter({ apiKey: 'unused-test-key', phoneNumberId: 'phone', webhookPort: 0,
    webhookSecret: 'test-secret', fallbackTemplateName: 'fallback' });
  const phases = [];
  const runtime = new ChannelRuntime({ channelType: 'kapso', adapter, magician: {
    async reportEnvoyDelivery(_session, _message, phase) {
      phases.push(phase);
      return { tracked: true, send: phase === 'begin', status: phase === 'begin' ? 'dispatching' : phase === 'unknown' ? 'dispatch_unknown' : phase };
    }
  } });
  return { runtime, phases };
}
function reply(text = 'Exact words') {
  return { id: 'reply-1', session_id: 'session-1', direction: 'assistant', content: { type: 'text', text }, created_at: 1 };
}
function providerResponse(body) {
  return new Response(JSON.stringify(body), { headers: { 'content-type': 'application/json' } });
}

test('HTTP success without a message receipt remains unknown and cannot send later chunks or replay', async (t) => {
  t.mock.method(console, 'error', () => {});
  for (const body of [null, {}, { error: { message: 'upstream unavailable' } }, { messages: [] },
    { messages: [{}] }, { messages: [{ id: '' }] }, { messages: [{ id: '  ' }] }, { messages: [{ id: 123 }] }]) {
    let posts = 0;
    const fetchMock = t.mock.method(globalThis, 'fetch', async () => {
      posts++;
      return providerResponse(body);
    });
    const { runtime, phases } = runtimeFixture();
    try {
      await runtime.deliverChatMessage('15555550123', 'session-1', reply('x'.repeat(8500)));
      await runtime.deliverChatMessage('15555550123', 'session-1', reply('x'.repeat(8500)));
      assert.equal(posts, 1, 'a missing receipt must stop chunk dispatch');
      assert.deepEqual(phases, ['begin', 'unknown']);
    } finally { await runtime.stop(); fetchMock.mock.restore(); }
  }
});

test('each chunk needs its own provider receipt before the complete reply can be accepted', async (t) => {
  for (const missingAt of [2, 0]) {
    const bodies = [];
    const fetchMock = t.mock.method(globalThis, 'fetch', async (_url, init) => {
      const body = JSON.parse(init.body);
      assert.equal(body.type, 'text');
      assert.equal(body.to, '15555550123');
      bodies.push(body.text.body);
      return providerResponse(bodies.length === missingAt ? {} : { messages: [{ id: `wamid-${bodies.length}` }] });
    });
    const { runtime, phases } = runtimeFixture();
    try {
      const text = '  ' + '🌼words\n'.repeat(1400) + '  ';
      await runtime.deliverChatMessage('15555550123', 'session-1', reply(text));
      assert.equal(bodies.length, missingAt || Math.ceil(text.length / 4000));
      assert.equal(bodies.join(''), text.slice(0, bodies.join('').length));
      assert.deepEqual(phases, ['begin', missingAt ? 'unknown' : 'provider_accepted']);
      if (!missingAt) assert.equal(bodies.join(''), text);
    } finally { await runtime.stop(); fetchMock.mock.restore(); }
  }
});

test('stalled provider headers or bodies time out without sending another chunk or replaying', async (t) => {
  t.mock.method(console, 'error', () => {});
  let deadline;
  t.mock.method(AbortSignal, 'timeout', (ms) => {
    assert.ok(ms > 0 && ms <= 30_000);
    deadline = new AbortController();
    return deadline.signal;
  });
  for (const stall of ['headers', 'body']) {
    let posts = 0;
    let requestSignal;
    let rejectPending;
    const fetchMock = t.mock.method(globalThis, 'fetch', async (_url, init) => {
      posts++;
      requestSignal = init.signal;
      const hang = () => new Promise((_resolve, reject) => {
        rejectPending = reject;
        init.signal?.addEventListener('abort', () => reject(init.signal.reason), { once: true });
      });
      return stall === 'headers' ? hang() : { ok: true,
        headers: new Headers({ 'content-type': 'application/json' }), json: hang };
    });
    const { runtime, phases } = runtimeFixture();
    const delivery = runtime.deliverChatMessage('15555550123', 'session-1', reply('x'.repeat(8500)));
    try {
      await new Promise(resolve => setImmediate(resolve));
      assert.ok(requestSignal, 'the provider request must carry a deadline');
      deadline.abort(new DOMException('Provider deadline exceeded', 'TimeoutError'));
      await delivery;
      await runtime.deliverChatMessage('15555550123', 'session-1', reply('x'.repeat(8500)));
      assert.deepEqual(phases, ['begin', 'unknown']);
      assert.equal(posts, 1);
      assert.equal(runtime.activeDeliveries.size, 0);
    } finally {
      rejectPending?.(new Error('test cleanup'));
      await delivery;
      await runtime.stop(); fetchMock.mock.restore();
    }
  }
});
