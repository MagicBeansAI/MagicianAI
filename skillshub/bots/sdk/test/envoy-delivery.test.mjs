import assert from 'node:assert/strict';
import test from 'node:test';
import { createHash } from 'node:crypto';
import { ChannelRuntime, MagicianClient, chunkExactText } from '../dist/index.js';

const message = { id: 'message-1', session_id: 'session-1', direction: 'assistant', content: { type: 'text', text: 'Exact words' }, created_at: 1 };
function fixture({ receipt = { accepted: true }, send = true, failedReports = 0, sendError = false } = {}) {
  const events = [];
  const client = new MagicianClient({ baseUrl: 'http://localhost:3002', bearerToken: 'mag_bot_fixture', fetchImpl: async (url, init) => {
    assert.match(String(url), /sessions\/session-1\/messages\/message-1\/envoy-delivery$/);
    const { phase } = JSON.parse(init.body); events.push(phase);
    if (phase === 'provider_accepted' && failedReports-- > 0) throw new Error('receipt response lost');
    return new Response(JSON.stringify({ tracked: true, send, status: phase }), { headers: { 'content-type': 'application/json' } });
  }});
  const runtime = new ChannelRuntime({ channelType: 'telegram', magician: client, adapter: {
    async start() {}, async sendText(_target, text, options) { assert.equal(options?.preserveExactText, true); events.push(text); if (sendError) throw new Error('partial send'); return receipt; }
  }});
  return { runtime, events };
}

test('Envoy prepares before send and acknowledges only explicit adapter acceptance', async () => {
  const { runtime, events } = fixture();
  await runtime.deliverChatMessage('recipient', 'session-1', message);
  await runtime.deliverChatMessage('recipient', 'session-1', message);
  assert.deepEqual(events, ['begin', 'Exact words', 'provider_accepted']);
});
test('a suppressed or unreceipted send remains unknown', async () => {
  for (const receipt of [{ accepted: false }, null]) {
    const { runtime, events } = fixture({ receipt });
    await runtime.deliverChatMessage('recipient', 'session-1', message);
    assert.deepEqual(events, ['begin', 'Exact words', 'unknown']);
  }
});
test('receipt retry never repeats the external send', async () => {
  const { runtime, events } = fixture({ failedReports: 1 });
  await runtime.deliverChatMessage('recipient', 'session-1', message);
  assert.deepEqual(events, ['begin', 'Exact words', 'provider_accepted', 'provider_accepted']);
});

test('stalled receipt headers and bodies time out, retain recovery proof, and never resend', async (t) => {
  const controllers = [];
  t.mock.method(AbortSignal, 'timeout', (duration) => {
    assert.ok(duration > 0 && duration <= 30_000);
    const controller = new AbortController();
    controllers.push(controller);
    return controller.signal;
  });
  for (const stall of ['headers', 'body']) {
    const bindings = [];
    let stalled = true;
    let sends = 0;
    const client = new MagicianClient({ baseUrl: 'http://localhost:3002', bearerToken: 'mag_bot_fixture', fetchImpl: async (_url, init) => {
      const { phase, binding } = JSON.parse(init.body);
      bindings.push({ phase, binding });
      assert.ok(init.signal, 'receipt transport needs a deadline');
      if (phase === 'begin' || !stalled) return new Response(JSON.stringify({ tracked: true, send: phase === 'begin', status: phase }));
      const hang = () => new Promise((_resolve, reject) => {
        init.signal.addEventListener('abort', () => reject(init.signal.reason), { once: true });
        queueMicrotask(() => controllers.at(-1).abort(new DOMException('Receipt deadline exceeded', 'TimeoutError')));
      });
      return stall === 'headers' ? hang() : { ok: true, async text() { return hang(); } };
    }});
    const runtime = new ChannelRuntime({ channelType: 'telegram', magician: client,
      adapter: { async start() {}, async sendText() { sends++; return { accepted: true }; } } });
    try {
      await assert.rejects(runtime.deliverChatMessage('recipient', 'session-1', message), /Receipt deadline exceeded/);
      assert.equal(sends, 1);
      assert.equal(runtime.envoyReceipts.size, 1);
      assert.equal(runtime.activeReceiptReports.size, 0);
      assert.ok(runtime.envoyReceiptTimer);
      stalled = false;
      await runtime.retryPendingEnvoyReceipts();
      assert.equal(runtime.envoyReceipts.size, 0);
      assert.equal(sends, 1);
      assert.deepEqual(bindings.map(entry => entry.phase), ['begin', 'provider_accepted', 'provider_accepted', 'provider_accepted', 'provider_accepted']);
      assert.ok(bindings.every(entry => JSON.stringify(entry.binding) === JSON.stringify(bindings[0].binding)));
    } finally { await runtime.stop(); }
  }
});
test('an outage beyond the immediate retries recovers the receipt on replay without resending', async () => {
  const { runtime, events } = fixture({ failedReports: 3 });
  try {
    await assert.rejects(runtime.deliverChatMessage('recipient', 'session-1', message), /receipt response lost/);
    assert.equal(runtime.envoyReceipts.size, 1);
    assert.ok(runtime.envoyReceiptTimer);
    runtime.deliveredMessageIds.clear();
    await runtime.deliverChatMessage('recipient', 'session-1', message);
    assert.equal(runtime.envoyReceipts.size, 0);
    assert.equal(events.filter(event => event === 'Exact words').length, 1);
    assert.equal(events.filter(event => event === 'begin').length, 1);
    assert.equal(events.filter(event => event === 'provider_accepted').length, 4);
  } finally { await runtime.stop(); }
});
test('scheduled recovery retains the original receipt proof and never invokes an adapter', async () => {
  const bindings = [];
  let failures = 3;
  let sends = 0;
  const runtime = new ChannelRuntime({ channelType: 'telegram', magician: {
    async reportEnvoyDelivery(_session, _message, phase, binding) {
      bindings.push({ phase, binding });
      if (phase !== 'begin' && failures-- > 0) throw new Error('service offline');
      return { tracked: true, send: phase === 'begin', status: phase };
    }
  }, adapter: { async start() {}, async sendText() { sends++; throw new Error('partial send'); } } });
  try {
    await assert.rejects(runtime.deliverChatMessage('recipient', 'session-1', message), /partial send/);
    assert.equal(runtime.envoyReceipts.size, 1);
    await runtime.retryPendingEnvoyReceipts();
    assert.equal(runtime.envoyReceipts.size, 0);
    assert.equal(sends, 1);
    assert.deepEqual(bindings.map(entry => entry.phase), ['begin', 'unknown', 'unknown', 'unknown', 'unknown']);
    assert.ok(bindings.every(entry => JSON.stringify(entry.binding) === JSON.stringify(bindings[0].binding)));
  } finally { await runtime.stop(); }
  assert.equal(runtime.envoyReceiptTimer, undefined);
});
test('a full receipt recovery queue refuses a new send without losing its resumable attempt', async () => {
  const { runtime, events } = fixture();
  for (let i = 0; i < 2048; i++) runtime.envoyReceipts.set(`held-${i}`, { phase: null });
  await assert.rejects(runtime.deliverChatMessage('recipient', 'session-1', message), /queue is full/);
  assert.deepEqual(events, ['begin']);
  assert.equal(runtime.envoyAttempts.size, 1);
  runtime.envoyReceipts.clear();
  await runtime.deliverChatMessage('recipient', 'session-1', message);
  assert.deepEqual(events, ['begin', 'begin', 'Exact words', 'provider_accepted']);
});
test('an already claimed send is not repeated after a process restart', async () => {
  const { runtime, events } = fixture({ send: false });
  await runtime.deliverChatMessage('recipient', 'session-1', message);
  assert.deepEqual(events, ['begin']);
});
test('partial send errors report unknown and do not create a second send', async () => {
  const { runtime, events } = fixture({ sendError: true });
  await assert.rejects(runtime.deliverChatMessage('recipient', 'session-1', message), /partial send/);
  await runtime.deliverChatMessage('recipient', 'session-1', message);
  assert.deepEqual(events, ['begin', 'Exact words', 'unknown']);
});

test('tracked replies preserve canonical words even when a presentation changes whitespace', async () => {
  const { runtime, events } = fixture();
  await runtime.deliverChatMessage('recipient', 'session-1', {
    ...message, content: { type: 'text', text: 'Exact\nwords' },
    presentation: { schema: 'magician.structured_response', version: 1, plain_text: 'Exact words' }
  });
  assert.deepEqual(events, ['begin', 'Exact\nwords', 'provider_accepted']);
});
test('a refused preparation never reaches the adapter', async () => {
  let sends = 0;
  const client = new MagicianClient({ baseUrl: 'http://localhost:3002', bearerToken: 'mag_bot_fixture', fetchImpl: async () => new Response('{}', { status: 409 }) });
  const runtime = new ChannelRuntime({ channelType: 'telegram', magician: client, adapter: {
    async start() {}, async sendText() { sends += 1; return { accepted: true }; }
  }});
  await assert.rejects(runtime.deliverChatMessage('recipient', 'session-1', message));
  assert.equal(sends, 0);
});

test('lost begin responses retry the same attempt, text digest and adapter recipient before sending', async () => {
  const bindings = [];
  let failures = 3;
  let sends = 0;
  const client = new MagicianClient({ baseUrl: 'http://localhost:3002', bearerToken: 'mag_bot_fixture', fetchImpl: async (_url, init) => {
    const { phase, binding } = JSON.parse(init.body);
    bindings.push(binding);
    if (phase === 'begin' && failures-- > 0) throw new Error('begin response lost');
    return new Response(JSON.stringify({ tracked: true, send: phase === 'begin', status: phase }));
  }});
  const runtime = new ChannelRuntime({ channelType: 'telegram', magician: client, adapter: {
    async start() {}, async sendText(target, text) { sends += 1; assert.equal(target, 42); assert.equal(text, message.content.text); return { accepted: true }; }
  }});
  await assert.rejects(runtime.deliverChatMessage(42, 'session-1', message), /begin response lost/);
  assert.equal(sends, 0);
  await runtime.deliverChatMessage(42, 'session-1', message);
  assert.equal(sends, 1);
  assert.equal(bindings.length, 5);
  assert.ok(bindings.every(value => JSON.stringify(value) === JSON.stringify(bindings[0])));
  assert.deepEqual(bindings[0], {
    attempt_id: bindings[0].attempt_id, channel_type: 'telegram', channel_address: '42',
    payload_sha256: createHash('sha256').update(message.content.text).digest('hex')
  });
  assert.match(bindings[0].attempt_id, /^[\da-f-]{36}$/);
});

test('after adapter entry a cache eviction cannot reuse the old dispatch grant', async () => {
  const attempts = [];
  let sends = 0;
  const runtime = new ChannelRuntime({ channelType: 'telegram', magician: {
    async reportEnvoyDelivery(_session, _message, phase, binding) {
      if (phase === 'begin') attempts.push(binding.attempt_id);
      return { tracked: true, send: phase === 'begin' && attempts.length === 1, status: phase };
    }
  }, adapter: { async start() {}, async sendText() { sends += 1; throw new Error('send may have happened'); } }});
  await assert.rejects(runtime.deliverChatMessage('recipient', 'session-1', message));
  runtime.deliveredMessageIds.clear(); // Simulate normal bounded-cache eviction.
  await runtime.deliverChatMessage('recipient', 'session-1', message);
  assert.equal(sends, 1);
  assert.equal(attempts.length, 2);
  assert.notEqual(attempts[0], attempts[1]);
});

test('a message from a different session never obtains a grant or reaches the adapter', async () => {
  const { runtime, events } = fixture();
  await assert.rejects(runtime.deliverChatMessage('recipient', 'session-1', { ...message, session_id: 'other' }), /session/i);
  assert.deepEqual(events, []);
});

test('opaque reply targets bind the resolved address and send only the prepared closure after a grant', async () => {
  for (const send of [true, false]) {
    const events = [];
    const runtime = new ChannelRuntime({ channelType: 'gmail', magician: {
      async reportEnvoyDelivery(_session, _message, phase, binding) {
        assert.equal(binding.channel_address, 'guest@example.com');
        events.push(phase);
        return { tracked: true, send, status: phase };
      }
    }, adapter: {
      async start() {}, async sendText() { throw new Error('must use exact-body prepared send'); },
      async prepareEnvoyTextDelivery(target, text) {
        assert.equal(target, 'opaque-message-id'); assert.equal(text, message.content.text);
        events.push('read metadata');
        return { channelAddress: 'guest@example.com', async send() { events.push(text); return { accepted: true }; } };
      }
    }});
    await runtime.deliverChatMessage('opaque-message-id', 'session-1', message);
    assert.deepEqual(events, send ? ['read metadata', 'begin', 'Exact words', 'provider_accepted'] : ['read metadata', 'begin']);
  }
});

test('concurrent replay cannot obtain another grant while an active send is evicted from the bounded cache', async () => {
  let release;
  let began;
  const entered = new Promise(resolve => { began = resolve; });
  const gate = new Promise(resolve => { release = resolve; });
  let begins = 0;
  let sends = 0;
  const runtime = new ChannelRuntime({ channelType: 'telegram', magician: {
    async reportEnvoyDelivery(_session, _message, phase) {
      if (phase === 'begin') { begins += 1; began(); await gate; }
      return { tracked: true, send: true, status: phase };
    }
  }, adapter: { async start() {}, async sendText() { sends += 1; return { accepted: true }; } } });
  const first = runtime.deliverChatMessage('recipient', 'session-1', message);
  await entered;
  runtime.deliveredMessageIds.clear();
  await runtime.deliverChatMessage('recipient', 'session-1', message);
  release();
  await first;
  assert.equal(begins, 1);
  assert.equal(sends, 1);
});


test('prepared text chunks preserve every character and Unicode boundary', () => {
  for (const text of ['  Exact\nwords  ', 'a'.repeat(19) + '🌼' + 'b'.repeat(41), 'one '.repeat(30), '🌼'.repeat(20)]) {
    for (const limit of [2, 7, 20]) {
      const chunks = chunkExactText(text, limit);
      assert.equal(chunks.join(''), text);
      assert.ok(chunks.every(chunk => chunk.length > 0 && chunk.length <= limit));
      assert.ok(chunks.every(chunk => !/^[\uDC00-\uDFFF]|[\uD800-\uDBFF]$/u.test(chunk)));
    }
  }
  assert.deepEqual(chunkExactText('', 20), []);
});
