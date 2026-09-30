import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test, { after } from 'node:test';

const home = mkdtempSync(join(tmpdir(), 'claims-whatsapp-'));
process.env.WU_HOME = home;
writeFileSync(join(home, 'config.yaml'), 'constraints:\n  default: full\nwhatsapp:\n  send_delay_ms: 0\n');
after(() => rmSync(home, { recursive: true, force: true }));
const { WhatsAppAdapter } = await import('../dist/adapter.js');
const ackEvent = 'CB:ack,class:message';
const ack = (id, error) => ({ tag: 'ack', attrs: { class: 'message', id, ...(error ? { error } : {}) } });

function fixture(send) {
  const adapter = new WhatsAppAdapter();
  const ws = new EventEmitter();
  const sent = [];
  adapter.sock = { ws, async sendMessage(target, content) {
    sent.push({ target, content });
    return send(ws, sent.length);
  } };
  return { adapter, ws, sent };
}

test('tracked WhatsApp words wait for their server acknowledgement after the socket write', async () => {
  const { adapter, ws, sent } = fixture(async () => ({ key: { id: 'ours' } }));
  let settled = false;
  const sending = adapter.sendText('guest@s.whatsapp.net', 'Exact words', { preserveExactText: true }).then(result => { settled = true; return result; });
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(settled, false);
  ws.emit(ackEvent, ack('another-message'));
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(settled, false);
  ws.emit(ackEvent, ack('ours'));
  assert.deepEqual(await sending, { accepted: true });
  assert.deepEqual(sent, [{ target: 'guest@s.whatsapp.net', content: { text: 'Exact words' } }]);
  assert.equal(ws.listenerCount(ackEvent), 0);
});

test('a rejected WhatsApp acknowledgement arriving before sendMessage returns stays unaccepted', async () => {
  const { adapter, ws, sent } = fixture(async ws => {
    ws.emit(ackEvent, ack('ours', '463'));
    return { key: { id: 'ours' } };
  });
  assert.deepEqual(await adapter.sendText('guest@s.whatsapp.net', 'Exact words', { preserveExactText: true }), { accepted: false });
  assert.equal(sent.length, 1);
  assert.equal(ws.listenerCount(ackEvent), 0);
});

test('disconnect after a partial tracked reply stops the remaining chunks', async () => {
  const { adapter, ws, sent } = fixture(async (ws, n) => {
    if (n === 1) ws.emit(ackEvent, ack('chunk-1'));
    else ws.emit('close');
    return { key: { id: `chunk-${n}` } };
  });
  assert.deepEqual(await adapter.sendText('guest@s.whatsapp.net', 'x'.repeat(9000), { preserveExactText: true }), { accepted: false });
  assert.equal(sent.length, 2);
  assert.equal(ws.listenerCount(ackEvent), 0);
});

test('ordinary WhatsApp sends retain their existing socket-write behavior', async () => {
  const { adapter, ws } = fixture(async () => ({ key: { id: 'ours' } }));
  assert.deepEqual(await adapter.sendText('guest@s.whatsapp.net', 'Ordinary words'), { accepted: true });
  assert.equal(ws.listenerCount(ackEvent), 0);
});

test('missing, timed-out and failed acknowledgements clean up without resending', async () => {
  const { sendWithServerAck } = await import('../dist/server-ack.js');
  let sends = 0;
  const send = async () => { sends++; return { key: { id: 'ours' } }; };
  assert.equal(await sendWithServerAck(undefined, send, 10), false);
  assert.equal(sends, 0);
  for (const outcome of ['timeout', 'missing-id', 'throw']) {
    const ws = new EventEmitter();
    const sending = sendWithServerAck(ws, async () => {
      sends++;
      if (outcome === 'throw') throw new Error('socket write failed');
      return outcome === 'missing-id' ? undefined : { key: { id: 'ours' } };
    }, 10);
    if (outcome === 'throw') await assert.rejects(sending, /socket write failed/);
    else assert.equal(await sending, false);
    assert.deepEqual(ws.eventNames(), []);
  }
  assert.equal(sends, 3);
});

test('timeout and disconnect release a stalled send without waiting for its promise', async () => {
  const { sendWithServerAck } = await import('../dist/server-ack.js');
  for (const outcome of ['timeout', 'disconnect']) {
    const ws = new EventEmitter();
    let finishSend;
    let sends = 0;
    const sending = sendWithServerAck(ws, () => {
      sends++;
      return new Promise(resolve => { finishSend = resolve; });
    }, 10);
    if (outcome === 'disconnect') ws.emit('close');
    let watchdog;
    const result = await Promise.race([
      sending,
      new Promise(resolve => { watchdog = setTimeout(() => resolve('still waiting for send'), 100); }),
    ]);
    clearTimeout(watchdog);
    // A late socket write cannot retroactively turn an unknown send into an
    // acceptance or authorize a second send. Release it even on test failure.
    finishSend({ key: { id: 'late-message' } });
    assert.equal(await sending, false);
    assert.equal(result, false, outcome);
    assert.deepEqual(ws.eventNames(), []);
    ws.emit(ackEvent, ack('late-message'));
    assert.equal(sends, 1);
  }
});

test('a send rejecting after its acknowledgement deadline is observed without leaking listeners', async () => {
  const { sendWithServerAck } = await import('../dist/server-ack.js');
  const ws = new EventEmitter();
  let rejectSend;
  const sending = sendWithServerAck(ws, () => new Promise((_resolve, reject) => { rejectSend = reject; }), 10);
  assert.equal(await sending, false);
  rejectSend(new Error('late socket failure'));
  // The test runner treats an unhandled late rejection as a failure.
  await new Promise(resolve => setImmediate(resolve));
  assert.deepEqual(ws.eventNames(), []);
});
