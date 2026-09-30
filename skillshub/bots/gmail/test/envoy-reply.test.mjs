import assert from 'node:assert/strict';
import test from 'node:test';
import { prepareEnvoyReply } from '../dist/envoy-reply.js';
import { GmailAdapter } from '../dist/adapter.js';

test('the watcher never attributes a mailbox embedded in a display name or selects one of multiple senders', async () => {
  const adapter = new GmailAdapter({ gwsBinary: '/unused-test-gws' });
  const received = [];
  adapter.handlers = { async onTextMessage(message) { received.push(message); } };
  for (const from of ['"Owner <owner@example.com>" <attacker@example.net>', 'Owner <owner@example.com>, Attacker <attacker@example.net>']) {
    await adapter.handleWatchLine(JSON.stringify({ id: 'message-2', from, body: 'Test words' }));
  }
  assert.equal(received.length, 1);
  assert.equal(received[0].channelAddress, 'attacker@example.net');
});

function fixture(extraHeaders = []) {
  const calls = [];
  const exec = async (args) => {
    calls.push(args);
    if (args.includes('getProfile')) return JSON.stringify({ emailAddress: 'owner@example.com' });
    if (args.includes('get')) return JSON.stringify({ id: 'message-2', threadId: 'thread-1', payload: { headers: [
      { name: 'From', value: 'Guest <guest@example.com>' },
      { name: 'Subject', value: 'Proposal' },
      { name: 'Message-ID', value: '<original@example.com>' }, ...extraHeaders
    ] }});
    return JSON.stringify({ id: 'accepted-1' });
  };
  return { calls, exec };
}

test('preparation only reads; the granted send preserves exact words, recipient, and threading', async () => {
  const { calls, exec } = fixture();
  const text = '  Exact words 🌼\nwith whitespace.  ';
  const delivery = await prepareEnvoyReply(exec, 'message-2', text);
  assert.equal(delivery.channelAddress, 'guest@example.com');
  assert.ok(calls.every((args) => !args.includes('send')));
  assert.deepEqual(await delivery.send(), { accepted: true });
  const args = calls.at(-1);
  assert.deepEqual(args.slice(0, 4), ['gmail', 'users', 'messages', 'send']);
  const body = JSON.parse(args[args.indexOf('--json') + 1]);
  assert.equal(body.threadId, 'thread-1');
  const mime = Buffer.from(body.raw, 'base64url').toString('utf8');
  assert.match(mime, /To: guest@example.com\r\n/);
  assert.match(mime, /Subject: Proposal\r\n/);
  assert.match(mime, /In-Reply-To: <original@example.com>/);
  assert.equal(Buffer.from(mime.split('\r\n\r\n')[1], 'base64').toString('utf8'), text);
});

test('Reply-To is presented to the host as the real recipient, and header injection is refused', async () => {
  const { exec } = fixture([{ name: 'Reply-To', value: 'other@example.com' }]);
  assert.equal((await prepareEnvoyReply(exec, 'message-2', 'Hi')).channelAddress, 'other@example.com');
  for (const value of ['guest@example.com\r\nBcc: hidden@example.com', 'a@example.com, b@example.com']) {
    const { calls, exec } = fixture([{ name: 'Reply-To', value }]);
    await assert.rejects(prepareEnvoyReply(exec, 'message-2', 'Hi'));
    assert.ok(calls.every((args) => !args.includes('send')));
  }
});

test('prepared Gmail recipient uses the same mailbox normalization as inbound attribution', async () => {
  const { calls, exec } = fixture([{ name: 'Reply-To', value: '"Owner <owner@example.com>" <Guest@Example.COM>' }]);
  const delivery = await prepareEnvoyReply(exec, 'message-2', 'Exact words');
  assert.equal(delivery.channelAddress, 'guest@example.com');
  await delivery.send();
  const args = calls.at(-1);
  const body = JSON.parse(args[args.indexOf('--json') + 1]);
  const mime = Buffer.from(body.raw, 'base64url').toString('utf8');
  assert.match(mime, /\r\nTo: guest@example.com\r\n/);
});

test('missing provider acceptance and mismatched message metadata never attest delivery', async () => {
  const { exec } = fixture();
  await assert.rejects(prepareEnvoyReply(exec, 'different-message', 'Hi'), /does not match/);
  const delivery = await prepareEnvoyReply(async (args) => args.includes('send') ? '{}' : exec(args), 'message-2', 'Hi');
  assert.deepEqual(await delivery.send(), { accepted: false });
});

test('the watcher uses the message id, not its containing thread id, as the reply target', async () => {
  const adapter = new GmailAdapter({ gwsBinary: '/unused-test-gws' });
  const received = [];
  adapter.handlers = { async onTextMessage(message) { received.push(message); } };
  await adapter.handleWatchLine(JSON.stringify({ id: 'message-2', threadId: 'thread-1', from: 'Guest <guest@example.com>', body: 'Hello' }));
  assert.equal(received[0].target, 'message-2');
  assert.equal(received[0].channelAddress, 'guest@example.com');
  await adapter.handleWatchLine(JSON.stringify({ threadId: 'thread-only', from: 'guest@example.com', body: 'Hello' }));
  assert.equal(received.length, 1);
});
