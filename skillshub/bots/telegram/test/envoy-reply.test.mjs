import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import test from 'node:test';
import { Telegraf } from 'telegraf';
import { ChannelRuntime } from '../../sdk/dist/index.js';
import { TelegramAdapter } from '../dist/adapter.js';

const message = (text = 'Exact words') => ({ id: 'reply-1', session_id: 'session-1', direction: 'assistant',
  content: { type: 'text', text }, created_at: 1 });

// Exercise Telegraf's actual HTTP decoding against a local fixture. Nothing
// authenticates to Telegram or sends an external message.
async function fixture(answer) {
  const posts = [];
  const server = createServer(async (req, res) => {
    let raw = '';
    for await (const chunk of req) raw += chunk;
    const payload = JSON.parse(raw);
    posts.push(payload);
    answer(res, payload, posts.length);
  });
  await new Promise((resolve, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', resolve); });
  const bot = new Telegraf('12345:fixture-token', { telegram: { apiRoot: `http://127.0.0.1:${server.address().port}` } });
  const adapter = new TelegramAdapter(bot);
  const phases = [];
  const runtime = new ChannelRuntime({ channelType: 'telegram', magician: {
    async reportEnvoyDelivery(_session, _message, phase) {
      phases.push(phase);
      return { tracked: true, send: phase === 'begin', status: phase };
    }
  }, adapter: { async start() {}, sendText: adapter.sendText.bind(adapter) } });
  return { posts, phases, runtime, async close() {
    await runtime.stop();
    server.closeAllConnections();
    await new Promise(resolve => server.close(resolve));
  } };
}
function respond(res, result) {
  res.writeHead(200, { 'content-type': 'application/json' });
  res.end(JSON.stringify({ ok: true, result }));
}
const receipt = (payload, id = 1) => ({ message_id: id, chat: { id: payload.chat_id, type: 'private' }, text: payload.text });

test('tracked Telegram replies refuse missing, invalid, or wrong-chat message receipts', async () => {
  for (const result of [undefined, null, {}, { message_id: 0, chat: { id: 42 } },
    { message_id: '1', chat: { id: 42 } }, { message_id: 1 }, { message_id: 1, chat: { id: 99 } }]) {
    const f = await fixture(res => respond(res, result));
    try {
      const reply = message('x'.repeat(8500));
      await f.runtime.deliverChatMessage(42, 'session-1', reply);
      await f.runtime.deliverChatMessage(42, 'session-1', reply);
      assert.equal(f.posts.length, 1);
      assert.deepEqual(f.phases, ['begin', 'unknown']);
    } finally { await f.close(); }
  }
});

test('tracked Telegram replies require a receipt for every chunk and preserve exact text', async () => {
  for (const missingAt of [2, 0]) {
    const f = await fixture((res, payload, count) => respond(res, count === missingAt ? {} : receipt(payload, count)));
    try {
      const text = '  ' + '🌼words\n'.repeat(1400) + '  ';
      await f.runtime.deliverChatMessage(42, 'session-1', message(text));
      const sent = f.posts.map(post => post.text).join('');
      assert.equal(f.posts.length, missingAt || Math.ceil(text.length / 4000));
      assert.equal(sent, text.slice(0, sent.length));
      assert.deepEqual(f.phases, ['begin', missingAt ? 'unknown' : 'provider_accepted']);
      if (!missingAt) assert.equal(sent, text);
    } finally { await f.close(); }
  }
});

test('tracked Telegram sends bound both header and body waits and never resend after abort', async (t) => {
  let deadline;
  t.mock.method(AbortSignal, 'timeout', (ms) => {
    assert.ok(ms > 0 && ms <= 30_000);
    deadline = new AbortController();
    return deadline.signal;
  });
  for (const stall of ['headers', 'body']) {
    deadline = undefined;
    let received;
    const entered = new Promise(resolve => { received = resolve; });
    let pendingResponse;
    const f = await fixture((res) => {
      pendingResponse = res;
      if (stall === 'body') { res.writeHead(200, { 'content-type': 'application/json' }); res.write('{"ok":true,'); }
      received();
    });
    const reply = message('x'.repeat(8500));
    const delivery = f.runtime.deliverChatMessage(42, 'session-1', reply).then(() => null, error => error);
    try {
      await entered;
      assert.ok(deadline, 'tracked sends must install their own bounded deadline');
      deadline.abort();
      assert.ok(await delivery, 'an aborted send must report failure');
      await f.runtime.deliverChatMessage(42, 'session-1', reply);
      assert.equal(f.posts.length, 1);
      assert.deepEqual(f.phases, ['begin', 'unknown']);
      assert.equal(f.runtime.activeDeliveries.size, 0);
    } finally {
      pendingResponse?.destroy();
      await delivery;
      await f.close();
    }
  }
});
