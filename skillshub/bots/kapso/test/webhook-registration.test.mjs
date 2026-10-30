import assert from 'node:assert/strict';
import { createHmac } from 'node:crypto';
import test from 'node:test';

import { KapsoAdapter } from '../dist/adapter.js';
import { ensureKapsoWebhook, resolveKapsoWebhookUrl } from '../dist/webhook-registration.js';

const realFetch = globalThis.fetch;
const OURS = 'https://webhook-studio.example.com/webhook';
const COLLECTION = 'https://api.kapso.ai/platform/v1/whatsapp/phone_numbers/phone-1/webhooks';
const REGISTERED_SECRET = 'registered-test-signing-value';

function hook(fields) {
  return { kind: 'kapso', active: true, buffer_enabled: false, events: ['whatsapp.message.received'],
    payload_version: 'v2', secret_key: 'other-test-signing-value', ...fields };
}

/** Mock the Kapso Platform API; every other URL goes to the real network stack (the local receiver). */
function mockKapso(t, { hooks = [], fail = false, pages } = {}) {
  const calls = [];
  t.mock.method(globalThis, 'fetch', async (input, init = {}) => {
    const url = String(input);
    if (!url.startsWith('https://api.kapso.ai/platform/v1/')) return realFetch(input, init);
    const body = init.body ? JSON.parse(init.body) : undefined;
    calls.push({ method: init.method, url, body, apiKey: init.headers?.['X-API-Key'] });
    if (fail) return new Response('{"error":"down"}', { status: 503 });
    if (init.method === 'GET') {
      if (pages) {
        const page = Number(new URL(url).searchParams.get('page'));
        return Response.json({ data: pages[page - 1] ?? [], meta: {} }, { headers: { 'X-Total-Pages': String(pages.length) } });
      }
      return Response.json({ data: hooks, meta: {} });
    }
    if (init.method === 'POST') {
      return Response.json({ data: { id: 'created-hook', ...body.whatsapp_webhook } }, { status: 201 });
    }
    const id = decodeURIComponent(url.split('/').pop());
    const existing = hooks.find(h => h.id === id) ?? {};
    return Response.json({ data: { ...existing, ...body.whatsapp_webhook } });
  });
  return calls;
}

function quietConsole(t) {
  const lines = [];
  for (const level of ['info', 'warn', 'error']) {
    t.mock.method(console, level, (...args) => lines.push({ level, text: JSON.stringify(args) }));
  }
  return lines;
}

test('webhook URL comes from KAPSO_WEBHOOK_URL, else the machine webhook host', () => {
  assert.equal(resolveKapsoWebhookUrl({ MAGICIAN_WEBHOOK_HOST: 'webhook-studio.example.com' }), OURS);
  assert.equal(resolveKapsoWebhookUrl({ MAGICIAN_WEBHOOK_HOST: 'https://webhook-studio.example.com/' }), OURS);
  assert.equal(resolveKapsoWebhookUrl({ MAGICIAN_WEBHOOK_HOST: 'x.example.com', KAPSO_WEBHOOK_URL: 'https://override.test/hook' }),
    'https://override.test/hook');
  assert.equal(resolveKapsoWebhookUrl({}), undefined);
});

test('an existing registration is re-enabled unbuffered, adopted, and other machines are retired', async (t) => {
  const logs = quietConsole(t);
  const calls = mockKapso(t, { hooks: [
    hook({ id: 'old-machine', url: 'https://webhook-laptop.example.com/webhook' }),
    hook({ id: 'shared-legacy', url: 'https://webhook.example.com/webhook/' }),
    hook({ id: 'mine', url: OURS, active: false, buffer_enabled: true, secret_key: REGISTERED_SECRET }),
    hook({ id: 'inactive-sibling', url: 'https://webhook-mini.example.com/webhook', active: false }),
    hook({ id: 'other-zone', url: 'https://hooks.elsewhere.test/webhook' }),
    hook({ id: 'sibling-other-path', url: 'https://crm.example.com/kapso' }),
    hook({ id: 'meta-forward', kind: 'meta', url: 'https://webhook-laptop.example.com/webhook' }),
  ] });

  const result = await ensureKapsoWebhook({ apiKey: 'test-api-key', phoneNumberId: 'phone-1', url: OURS,
    configuredSecret: 'stale-configured-value' });

  assert.equal(result.webhookId, 'mine');
  assert.equal(result.secret, REGISTERED_SECRET, 'the provider registration is authoritative');
  assert.equal(result.created, false);
  assert.equal(result.updated, true);
  assert.deepEqual(result.deactivated.map(h => h.id), ['old-machine', 'shared-legacy']);
  const patches = calls.filter(c => c.method === 'PATCH');
  assert.deepEqual(patches.map(c => [c.url.split('/').pop(), c.body.whatsapp_webhook]), [
    ['mine', { active: true, buffer_enabled: false }],
    ['old-machine', { active: false }],
    ['shared-legacy', { active: false }],
  ]);
  assert.ok(calls.every(c => c.apiKey === 'test-api-key'));
  assert.equal(calls.filter(c => c.method === 'POST').length, 0);
  assert.ok(logs.every(l => !l.text.includes(REGISTERED_SECRET)), 'a secret never reaches the log');
});

test('a correct registration is left untouched', async (t) => {
  quietConsole(t);
  const calls = mockKapso(t, { hooks: [hook({ id: 'mine', url: OURS, secret_key: REGISTERED_SECRET })] });
  const result = await ensureKapsoWebhook({ apiKey: 'k', phoneNumberId: 'phone-1', url: OURS });
  assert.equal(result.secret, REGISTERED_SECRET);
  assert.equal(result.updated, false);
  assert.deepEqual(calls.map(c => c.method), ['GET']);
});

test('a missing registration is created unbuffered with the configured or a generated secret', async (t) => {
  quietConsole(t);
  let calls = mockKapso(t, { hooks: [] });
  const configured = await ensureKapsoWebhook({ apiKey: 'k', phoneNumberId: 'phone-1', url: OURS, configuredSecret: 'configured-value' });
  const post = calls.find(c => c.method === 'POST');
  assert.equal(post.url, COLLECTION);
  assert.deepEqual(post.body, { whatsapp_webhook: { url: OURS, kind: 'kapso', events: ['whatsapp.message.received'],
    active: true, buffer_enabled: false, payload_version: 'v2', secret_key: 'configured-value' } });
  assert.equal(configured.created, true);
  assert.equal(configured.secret, 'configured-value');

  t.mock.restoreAll();
  quietConsole(t);
  calls = mockKapso(t, { hooks: [] });
  const generated = await ensureKapsoWebhook({ apiKey: 'k', phoneNumberId: 'phone-1', url: OURS });
  assert.match(generated.secret, /^[0-9a-f]{64}$/);
  assert.equal(calls.find(c => c.method === 'POST').body.whatsapp_webhook.secret_key, generated.secret);
});

test('every page of the number\'s webhooks is read', async (t) => {
  quietConsole(t);
  const calls = mockKapso(t, { pages: [
    [hook({ id: 'old-machine', url: 'https://webhook-laptop.example.com/webhook' })],
    [hook({ id: 'mine', url: OURS, secret_key: REGISTERED_SECRET })],
  ] });
  const result = await ensureKapsoWebhook({ apiKey: 'k', phoneNumberId: 'phone-1', url: OURS });
  assert.equal(calls.filter(c => c.method === 'GET').length, 2);
  assert.equal(result.webhookId, 'mine');
  assert.deepEqual(result.deactivated.map(h => h.id), ['old-machine']);
});

function sign(body, secret) {
  return createHmac('sha256', secret).update(body).digest('hex');
}

async function post(adapter, body, secret) {
  return realFetch(`http://127.0.0.1:${adapter.server.address().port}/webhook`, {
    method: 'POST', headers: { 'content-type': 'application/json', 'x-webhook-signature': sign(body, secret) }, body,
  });
}

test('the receiver verifies with the registration\'s secret, not a stale configured one', async (t) => {
  quietConsole(t);
  mockKapso(t, { hooks: [hook({ id: 'mine', url: OURS, secret_key: REGISTERED_SECRET })] });
  const adapter = new KapsoAdapter({ apiKey: 'k', phoneNumberId: 'phone-1', webhookPort: 0, webhookUrl: OURS,
    webhookSecret: 'stale-configured-value' });
  try {
    await adapter.start({ async onTextMessage() {} });
    const body = '{"message":null}';
    assert.equal((await post(adapter, body, 'stale-configured-value')).status, 401);
    assert.equal((await post(adapter, body, REGISTERED_SECRET)).status, 200);
  } finally { await adapter.stop(); }
});

test('an unreachable provider falls back to KAPSO_WEBHOOK_SECRET', async (t) => {
  const logs = quietConsole(t);
  mockKapso(t, { fail: true });
  const adapter = new KapsoAdapter({ apiKey: 'k', phoneNumberId: 'phone-1', webhookPort: 0, webhookUrl: OURS,
    webhookSecret: 'configured-value' });
  try {
    await adapter.start({ async onTextMessage() {} });
    assert.equal((await post(adapter, '{}', 'configured-value')).status, 200);
    assert.ok(logs.some(l => l.level === 'error' && l.text.includes('self-register failed')));
  } finally { await adapter.stop(); }
});

test('without any way to learn a secret the receiver never starts', async (t) => {
  quietConsole(t);
  mockKapso(t, { fail: true });
  const unregistered = new KapsoAdapter({ apiKey: 'k', phoneNumberId: 'phone-1', webhookPort: 0, webhookUrl: OURS });
  await assert.rejects(unregistered.start({ async onTextMessage() {} }), /signing secret unknown/);
  assert.equal(unregistered.server, undefined);
  const noUrl = new KapsoAdapter({ apiKey: 'k', phoneNumberId: 'phone-1', webhookPort: 0 });
  await assert.rejects(noUrl.start({ async onTextMessage() {} }), /signing secret unknown/);
  assert.equal(noUrl.server, undefined);
});
