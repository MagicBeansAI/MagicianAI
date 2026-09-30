import assert from 'node:assert/strict';
import test from 'node:test';
import { Webhook } from 'svix';
import { AgentMailAdapter } from '../dist/adapter.js';

const secret = `whsec_${Buffer.from('local-test-signing-key-not-a-credential').toString('base64')}`;

test('sender attribution uses the mailbox outside a quoted display name and refuses multiple senders', async () => {
  const adapter = new AgentMailAdapter({ apiKey: 'unused-test-key', webhookPort: 0 });
  const forwarded = [];
  adapter.handlers = { async onTextMessage(message) { forwarded.push(message); } };
  for (const from of ['"Owner <owner@example.com>" <attacker@example.net>', 'Owner <owner@example.com>, Attacker <attacker@example.net>']) {
    await adapter.handleEvent({ event_type: 'message.received', message: {
      from, message_id: 'source', inbox_id: 'inbox', labels: [], text: 'Test words',
    } });
  }
  assert.equal(forwarded.length, 1);
  assert.equal(forwarded[0].channelAddress, 'attacker@example.net');
  assert.equal(forwarded[0].target, 'attacker@example.net');
  assert.equal(adapter.replyContexts.has('owner@example.com'), false);
});
function signed(body, signingSecret = secret) {
  const id = 'test-event-1';
  const now = new Date();
  return { headers: {
    'svix-id': id, 'svix-timestamp': String(Math.floor(now.getTime() / 1000)),
    'svix-signature': new Webhook(signingSecret).sign(id, now, body.toString()),
  } };
}

test('missing webhook authority refuses forged email even if sender labels claim authenticity', () => {
  const adapter = new AgentMailAdapter({ apiKey: 'unused-test-key', webhookPort: 0 });
  const forged = Buffer.from(JSON.stringify({ event_type: 'message.received', message: {
    from: 'owner@example.com', text: 'Do this as the owner', labels: [],
  } }));
  assert.equal(adapter.verifySignature({ headers: {} }, forged), false);
  assert.equal(adapter.verifySignature(signed(forged), forged), false);
});

test('configured signatures verify the exact body and refuse tampering', () => {
  const adapter = new AgentMailAdapter({ apiKey: 'unused-test-key', webhookPort: 0, webhookSecret: secret });
  const body = Buffer.from('{"event_type":"message.received"}');
  const request = signed(body);
  assert.equal(adapter.verifySignature(request, body), true);
  assert.equal(adapter.verifySignature(request, Buffer.from('{"event_type":"message.received","forged":true}')), false);
  assert.equal(adapter.verifySignature({ headers: {} }, body), false);
});

test('automatic webhook registration installs its signing secret before accepting events', async () => {
  const adapter = new AgentMailAdapter({ apiKey: 'unused-test-key', webhookPort: 0, webhookUrl: 'https://example.invalid' });
  adapter.client = { webhooks: { async create() { return { webhook_id: 'test-hook', secret }; } } };
  const body = Buffer.from('{"event_type":"message.received"}');
  assert.equal(adapter.verifySignature(signed(body), body), false);
  await adapter.selfRegisterWebhook();
  assert.equal(adapter.verifySignature(signed(body), body), true);
  assert.equal(adapter.verifySignature({ headers: {} }, body), false);
});

test('an explicit signing secret is retained when registration returns another webhook secret', async () => {
  const adapter = new AgentMailAdapter({ apiKey: 'unused-test-key', webhookPort: 0, webhookUrl: 'https://example.invalid', webhookSecret: secret });
  adapter.client = { webhooks: { async create() { return { webhook_id: 'test-hook', secret: `whsec_${Buffer.from('other-test-key').toString('base64')}` }; } } };
  await adapter.selfRegisterWebhook();
  const body = Buffer.from('{}');
  assert.equal(adapter.verifySignature(signed(body), body), true);
});

test('the HTTP receiver forwards only verified events and cannot acquire reply context from unsigned requests', async () => {
  const adapter = new AgentMailAdapter({ apiKey: 'unused-test-key', webhookPort: 0, webhookSecret: secret });
  const forwarded = [];
  try {
    await adapter.start({ async onTextMessage(message) { forwarded.push(message); } });
    const url = `http://127.0.0.1:${adapter.server.address().port}/agentmail-webhook`;
    const body = Buffer.from(JSON.stringify({ event_type: 'message.received', message: {
      from: 'owner@example.com', text: 'Test inbound message', labels: [], inbox_id: 'inbox', message_id: 'source',
    } }));
    const unsigned = await fetch(url, { method: 'POST', headers: { 'content-type': 'application/json' }, body: body.toString() });
    assert.equal(unsigned.status, 401);
    assert.equal(forwarded.length, 0);
    assert.equal(adapter.replyContexts.size, 0);
    const verified = await fetch(url, { method: 'POST', headers: { 'content-type': 'application/json', ...signed(body).headers }, body: body.toString() });
    assert.equal(verified.status, 200);
    assert.equal(forwarded.length, 1);
    assert.equal(forwarded[0].channelVerified, true);
    assert.equal(adapter.replyContexts.size, 1);
  } finally { await adapter.stop(); }
});

test('missing configuration or a registration without a secret cannot leave an unverified receiver running', async () => {
  const unconfigured = new AgentMailAdapter({ apiKey: 'unused-test-key', webhookPort: 0 });
  await assert.rejects(unconfigured.start({}), /requires AGENTMAIL_WEBHOOK_SECRET/);
  assert.equal(unconfigured.server, undefined);
  const adapter = new AgentMailAdapter({ apiKey: 'unused-test-key', webhookPort: 0, webhookUrl: 'https://example.invalid' });
  adapter.client = { webhooks: { async create() { return { webhook_id: 'test-hook' }; } } };
  try {
    await assert.rejects(adapter.start({}), /no signing secret/);
    assert.equal(adapter.server, undefined);
    assert.equal(adapter.keepaliveTimer, undefined);
  } finally { await adapter.stop(); }
});

test('a listener bind failure rejects startup without reporting a ready receiver or keeping a timer alive', async () => {
  const running = new AgentMailAdapter({ apiKey: 'unused-test-key', webhookPort: 0, webhookSecret: secret });
  let conflicting;
  try {
    await running.start({});
    conflicting = new AgentMailAdapter({ apiKey: 'unused-test-key', webhookPort: running.server.address().port, webhookSecret: secret });
    await assert.rejects(conflicting.start({}), { code: 'EADDRINUSE' });
    assert.equal(conflicting.server, undefined);
    assert.equal(conflicting.keepaliveTimer, undefined);
  } finally {
    await conflicting?.stop();
    await running.stop();
  }
});
