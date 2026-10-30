import assert from 'node:assert/strict';
import test from 'node:test';
import { Webhook } from 'svix';
import { AgentMailAdapter } from '../dist/adapter.js';
import { deliveryReceipts } from '../dist/delivery-receipts.js';

const secret = `whsec_${Buffer.from('delivery-fixture-key-not-a-credential').toString('base64')}`;
const timestamp = '2026-10-13T10:00:00Z';
function delivered(recipients = ['one@example.test', 'two@example.test']) {
  return { type: 'event', event_type: 'message.delivered', event_id: 'delivery-1', delivery: {
    inbox_id: 'company@example.test', message_id: '<sent@relay.test>', timestamp, recipients,
  }};
}
function signed(raw) {
  const id = 'delivery-event-fixture';
  const now = new Date();
  return { 'svix-id': id, 'svix-timestamp': String(Math.floor(now.getTime() / 1000)),
    'svix-signature': new Webhook(secret).sign(id, now, raw) };
}

test('recipient keys stay separate and stable when the same message later bounces', () => {
  const body = delivered();
  const first = deliveryReceipts(body, JSON.stringify(body));
  assert.equal(first.length, 2);
  assert.notEqual(first[0].provider_message_id, first[1].provider_message_id);
  assert.equal(first[0].sent_message_id, '<sent@relay.test>');
  assert.equal(first[0].observed_at, timestamp);
  assert.equal(first[0].raw_payload, JSON.stringify(body));
  const bounce = { type: 'event', event_type: 'message.bounced', event_id: 'bounce-1', bounce: {
    ...body.delivery, type: 'Permanent', recipients: [{ address: 'ONE@example.test', status: 'bounced' }],
  }};
  const later = deliveryReceipts(bounce, JSON.stringify(bounce));
  assert.equal(later[0].provider_message_id, first[0].provider_message_id);
  assert.equal(later[0].state, 'hard_bounce');
});

test('missing clocks, malformed recipients and unknown bounce classes produce no guessed receipt', () => {
  const original = delivered();
  for (const body of [
    { ...original, delivery: { ...original.delivery, timestamp: undefined } },
    { ...original, delivery: { ...original.delivery, recipients: ['valid@example.test', 'not a mailbox'] } },
    { ...original, event_type: 'message.bounced', bounce: { ...original.delivery, type: 'Unknown', recipients: [{address:'one@example.test'}] } },
  ]) assert.throws(() => deliveryReceipts(body, JSON.stringify(body)));
});

test('signed delivery is acknowledged only after persistence; unsigned or failed intake never is', async () => {
  const adapter = new AgentMailAdapter({ apiKey: 'unused-test-key', webhookPort: 0, webhookSecret: secret });
  let fail = false;
  let calls = 0;
  try {
    await adapter.start({
      async onTextMessage() { assert.fail('delivery event must not enter chat'); },
      async onDeliveryReceipt(receipt) {
        assert.equal(receipt.source, 'provider');
        if (fail) throw new Error('synthetic backend unavailable');
        calls++;
      },
    });
    const url = `http://127.0.0.1:${adapter.server.address().port}/agentmail-webhook`;
    const raw = JSON.stringify(delivered(['one@example.test']));
    const post = headers => fetch(url, { method:'POST', headers:{'content-type':'application/json', ...headers}, body:raw });
    assert.equal((await post({})).status, 401);
    assert.equal(calls, 0);
    assert.equal((await post(signed(raw))).status, 200);
    assert.equal(calls, 1);
    fail = true;
    assert.equal((await post(signed(raw))).status, 503);
    assert.equal(calls, 1);
  } finally { await adapter.stop(); }
});

test('self-registration includes delivery statuses without routing sent events into chat', async () => {
  const adapter = new AgentMailAdapter({ apiKey: 'unused-test-key', webhookPort:0,
    webhookUrl:'https://example.invalid', webhookSecret:secret });
  let subscribed;
  adapter.client = {webhooks:{async create(body) {subscribed=body.event_types;return {webhook_id:'fixture', secret,event_types:subscribed};}}};
  await adapter.selfRegisterWebhook();
  assert.deepEqual(subscribed, ['message.received','message.delivered','message.bounced','message.complained']);
});

test('explicit signed parent references route to Work and a failed admission requests replay', async () => {
  const adapter = new AgentMailAdapter({apiKey:'unused-test-key',webhookPort:0,webhookSecret:secret});
  let fail = false;
  let routed;
  let chat = 0;
  try {
    await adapter.start({
      async onTextMessage() { chat++; },
      async onNegotiationReply(reply) { if (fail) throw new Error('synthetic unavailable'); routed=reply; return true; },
    });
    const url = `http://127.0.0.1:${adapter.server.address().port}/agentmail-webhook`;
    const raw = JSON.stringify({event_type:'message.received',message:{inbox_id:'inbox-1',message_id:'reply-1',in_reply_to:'send-1',from:'Guest <guest@example.test>',timestamp,text:'I need to check first.',labels:['unauthenticated']}});
    const post = () => fetch(url,{method:'POST',headers:{'content-type':'application/json',...signed(raw)},body:raw});
    assert.equal((await post()).status,200);
    assert.equal(routed.reply_to_message_id,'send-1');
    assert.equal(routed.sender_verified,false);
    assert.equal(routed.text,'I need to check first.');
    assert.equal(chat,0);
    fail = true;
    assert.equal((await post()).status,503);
    assert.equal(chat,0);
  } finally { await adapter.stop(); }
});

test('the pinned provider SDK sends the documented status subscription unchanged', async (t) => {
  let subscription;
  t.mock.method(globalThis,'fetch',async (_url,init) => {
    subscription = JSON.parse(init.body);
    return new Response(JSON.stringify({webhook_id:'test-hook',secret,event_types:subscription.event_types}),{status:200,headers:{'content-type':'application/json'}});
  });
  const adapter = new AgentMailAdapter({apiKey:'unused-test-key',webhookPort:0,webhookUrl:'https://example.invalid',webhookSecret:secret});
  await adapter.selfRegisterWebhook();
  assert.deepEqual(subscription.event_types,['message.received','message.delivered','message.bounced','message.complained']);
});

test('an existing incoming-only subscription is extended through the actual PATCH without deleting other events', async (t) => {
  const adapter = new AgentMailAdapter({apiKey:'unused-test-key',webhookPort:0,webhookUrl:'https://example.invalid',webhookSecret:secret});
  let patch;
  let calls = 0;
  t.mock.method(globalThis,'fetch',async (url,init) => {
    calls++;
    assert.equal(url,'https://api.agentmail.to/v0/webhooks/existing');
    assert.equal(init.method,'PATCH');
    assert.equal(init.headers.Authorization,'Bearer unused-test-key');
    assert.equal(init.redirect,'error');
    assert.ok(init.signal instanceof AbortSignal);
    patch = JSON.parse(init.body);
    return new Response(JSON.stringify({webhook_id:'existing',event_types:patch.event_types}),{status:200,headers:{'content-type':'application/json'}});
  });
  adapter.client = {webhooks:{async create() {return {webhook_id:'existing',secret,event_types:['message.received','domain.verified']};}}};
  await adapter.selfRegisterWebhook();
  assert.equal(calls,1);
  assert.deepEqual(patch.event_types,['message.received','domain.verified','message.delivered','message.bounced','message.complained']);
});

test('an existing subscription on a retired host is deleted and registered again at the configured URL', async (t) => {
  // The update endpoint cannot change a URL, so the stale hook is replaced
  // and the replacement's secret wins over the configured, now stale, one.
  const adapter = new AgentMailAdapter({apiKey:'unused-test-key',webhookPort:0,webhookUrl:'https://webhook-new.example.invalid',webhookSecret:secret});
  const types = ['message.received','message.delivered','message.bounced','message.complained'];
  const newSecret = `whsec_${Buffer.from('replacement-fixture-key').toString('base64')}`;
  const requests = [];
  t.mock.method(globalThis,'fetch',async (url,init) => {
    requests.push(`${init.method} ${url}`);
    return new Response(null,{status:204});
  });
  const created = [];
  adapter.client = {webhooks:{async create(body) {
    created.push(body.url);
    return created.length === 1
      ? {webhook_id:'old-hook',secret,url:'https://webhook-old.example.invalid/agentmail-webhook',event_types:types}
      : {webhook_id:'new-hook',secret:newSecret,url:body.url,event_types:types};
  }}};
  await adapter.selfRegisterWebhook();
  assert.deepEqual(requests,['DELETE https://api.agentmail.to/v0/webhooks/old-hook']);
  assert.deepEqual(created,['https://webhook-new.example.invalid/agentmail-webhook','https://webhook-new.example.invalid/agentmail-webhook']);
  assert.equal(adapter.webhookSecret,newSecret);
});

test('an existing subscription already on the configured URL is left alone', async (t) => {
  const adapter = new AgentMailAdapter({apiKey:'unused-test-key',webhookPort:0,webhookUrl:'https://webhook-new.example.invalid',webhookSecret:secret});
  let calls = 0;
  t.mock.method(globalThis,'fetch',async () => { calls++; return new Response('{}'); });
  adapter.client = {webhooks:{async create() {return {webhook_id:'existing',secret,url:'https://webhook-new.example.invalid/agentmail-webhook',event_types:['message.received','message.delivered','message.bounced','message.complained']};}}};
  await adapter.selfRegisterWebhook();
  assert.equal(calls,0);
});
