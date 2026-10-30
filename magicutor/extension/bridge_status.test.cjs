const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

function statusHandler({
  connected = false, occupied = false, serviceDown = null, onFetch, browsers, maxBrowsers, identity,
  bridgePrompts,
} = {}) {
  const context = {
    AbortController, setTimeout, clearTimeout,
    console: { log() {}, warn() {} },
    MAGICUTOR_API_BASE: 'http://127.0.0.1:3003',
    MAGICIAN_HEALTH_URL: 'http://127.0.0.1:3002/health',
    chrome: { runtime: { getManifest: () => ({ version: 'test' }) } },
    fetch: async (url) => {
      onFetch?.(url);
      if (url.includes(serviceDown || 'never-match')) throw new Error('unavailable');
      const body = { version: 'test', extension_connected: occupied };
      if (browsers) {
        // Like Magicutor: ids stay server-side; the caller's own entry is
        // marked when the request names its id.
        const asked = new URL(url).searchParams.get('browser_id');
        Object.assign(body, {
          max_browsers: maxBrowsers,
          browsers: browsers.map(({ serverId, ...visible }) => ({ ...visible, this_browser: serverId === asked })),
        });
      }
      return { ok: true, json: async () => body };
    },
  };
  vm.createContext(context);
  const file = path.join(__dirname, 'extension_messages.js');
  const source = fs.readFileSync(file, 'utf8')
    .replace(/^import[\s\S]*?from ['"][^'"]+['"];?/gm, '')
    .replace(/export\s*\{[\s\S]*?\};\s*$/, '')
    .concat('\nglobalThis.createHandler = createExtensionMessageHandler;');
  vm.runInContext(source, context, { filename: file });
  return context.createHandler({
    isBridgeConnected: () => typeof connected === 'function' ? connected() : connected,
    checkHttpHealth: async () => true,
    activityState: {}, recordDecision() {},
    ...(identity ? { getBrowserIdentity: async () => identity } : {}),
    ...(bridgePrompts ? { bridgePrompts } : {}),
  });
}

test('panel prompt actions reach the worker and a choice needs its id', async () => {
  const calls = [];
  const handler = statusHandler({
    bridgePrompts: {
      snapshot: async () => ({ pairing: 'pending', choices: [] }),
      accept: async () => { calls.push('accept'); return {}; },
      decline: async () => { calls.push('decline'); return {}; },
      connect: async () => { calls.push('connect'); return {}; },
      choose: async (choiceId, always) => { calls.push(`choose:${choiceId}:${always}`); return { sent: true }; },
    },
  });
  assert.equal((await handler({ action: 'get_bridge_prompts' })).pairing, 'pending');
  await handler({ action: 'bridge_pairing_accept' });
  await handler({ action: 'bridge_pairing_decline' });
  await handler({ action: 'bridge_connect' });
  await handler({ action: 'bridge_choice_answer', choiceId: 'c1', always: true });
  await handler({ action: 'bridge_choice_answer', choiceId: 'c2' });
  assert.deepEqual(calls, ['accept', 'decline', 'connect', 'choose:c1:true', 'choose:c2:false']);
  await assert.rejects(handler({ action: 'bridge_choice_answer' }), /choiceId is required/);
});

function slots(count) {
  const labels = ['Chrome', 'Chrome Home', 'Comet', 'Dia', 'Edge'];
  return labels.slice(0, count).map((label, index) => ({
    serverId: `id-${index + 1}`, fingerprint: `fp00000${index + 1}`,
    label, slot: index + 1, default: index === 0, pinned_threads: 0,
  }));
}

test('a connected browser learns its own slot from the health list', async () => {
  const urls = [];
  const result = await statusHandler({
    connected: true, browsers: slots(3), maxBrowsers: 4,
    identity: { id: 'id-3', key: 'secret-key-0123456789', label: 'Comet' },
    onFetch: (url) => urls.push(url),
  })({ action: 'ping' });
  assert.equal(result.connected, true);
  assert.equal(result.slot, 3);
  assert.equal(result.max_browsers, 4);
  assert.equal(result.browser.label, 'Comet');
  assert.equal(result.bridge_full, false);
  assert.equal(result.bridge_id_in_use, false);
  assert.equal(result.browsers.length, 3);
  assert.ok(urls.some((url) => url.endsWith('/health?browser_id=id-3')));
  // The secret key never leaves the extension, not even to its own panel.
  assert.doesNotMatch(JSON.stringify(result), /secret-key/);
  assert.doesNotMatch(JSON.stringify(result.browsers), /id-\d/);
});

test('other browsers holding some slots is not a full bridge', async () => {
  const result = await statusHandler({
    browsers: slots(3), maxBrowsers: 4, identity: { id: 'id-new', label: 'Edge' },
  })({ action: 'ping' });
  assert.equal(result.bridge_full, false);
  assert.equal(result.bridge_busy, false);
  assert.equal(result.slot, null);
});

test('every slot held by other browsers reports a full bridge', async () => {
  const result = await statusHandler({
    browsers: slots(4), maxBrowsers: 4, identity: { id: 'id-new', label: 'Edge' },
  })({ action: 'ping' });
  assert.equal(result.bridge_full, true);
  assert.equal(result.bridge_busy, true);
});

test('our own stale entry does not make the bridge full for us', async () => {
  const result = await statusHandler({
    browsers: slots(4), maxBrowsers: 4, identity: { id: 'id-2', label: 'Chrome Home' },
  })({ action: 'ping' });
  assert.equal(result.bridge_full, false);
  assert.equal(result.bridge_id_in_use, true);
});

test('the browser identity handed to panels carries no key', async () => {
  const handler = statusHandler({ identity: { id: 'id-1', key: 'secret-key-0123456789', label: 'Dia' } });
  const identity = await handler({ action: 'get_browser_identity' });
  assert.equal(identity.label, 'Dia');
  assert.equal(identity.key, undefined);
});

test('an occupied bridge is distinguished from a failed server', async () => {
  const result = await statusHandler({ occupied: true })({ action: 'ping' });
  assert.equal(result.connected, false);
  assert.equal(result.bridge_busy, true);
  assert.equal(result.http, true);
  assert.equal(result.magician, true);
});

test('the owning extension remains ready', async () => {
  const result = await statusHandler({ connected: true, occupied: true })({ action: 'ping' });
  assert.equal(result.connected, true);
  assert.equal(result.bridge_busy, false);
});

test('an empty bridge is disconnected rather than occupied', async () => {
  const result = await statusHandler()({ action: 'ping' });
  assert.equal(result.bridge, false);
  assert.equal(result.bridge_busy, false);
});

test('a failed Magicutor health request is not reported as an occupied bridge', async () => {
  const result = await statusHandler({ occupied: true, serviceDown: ':3003' })({ action: 'ping' });
  assert.equal(result.http, false);
  assert.equal(result.bridge_busy, false);
});

test('bridge status is sampled after asynchronous health checks', async () => {
  let connected = true;
  const result = await statusHandler({
    connected: () => connected, occupied: true, onFetch: () => { connected = false; },
  })({ action: 'ping' });
  assert.equal(result.connected, false);
  assert.equal(result.bridge_busy, true);
});

async function panelStatus(response) {
  const elements = Object.fromEntries(['status', 'status-text', 'status-details'].map((id) => [id, {}]));
  const context = {
    chrome: { runtime: { sendMessage: async () => response } },
    document: { getElementById: (id) => elements[id] },
    automationServerReady: false,
    updateAutomationControlState: async () => {},
  };
  vm.createContext(context);
  const file = path.join(__dirname, 'control_panel.js');
  const source = fs.readFileSync(file, 'utf8');
  const start = source.indexOf('async function checkStatus()');
  const end = source.indexOf('async function updateActivity()', start);
  assert.ok(start >= 0 && end > start);
  vm.runInContext(source.slice(start, end), context, { filename: file });
  await context.checkStatus();
  return { ...context, elements };
}

test('waiting browser panel reports the conflict and keeps automation disabled', async () => {
  const result = await panelStatus({ bridge: false, bridge_busy: true, http: true, magician: true });
  assert.equal(result.elements.status.className, 'status waiting');
  assert.equal(result.elements['status-text'].textContent, 'Another browser is connected');
  assert.equal(result.automationServerReady, false);
  assert.match(result.elements['status-details'].innerHTML, /In use by another browser/);
});

test('actual server failure is still offline even if a stale conflict flag is present', async () => {
  const result = await panelStatus({ bridge: false, bridge_busy: true, http: true, magician: false });
  assert.equal(result.elements.status.className, 'status disconnected');
  assert.equal(result.automationServerReady, false);
});

test('owning browser panel stays ready', async () => {
  const result = await panelStatus({ bridge: true, bridge_busy: false, http: true, magician: true });
  assert.equal(result.elements['status-text'].textContent, '✓ Server Ready');
  assert.equal(result.automationServerReady, true);
});

test('connected panel names this browser and its slot', async () => {
  const result = await panelStatus({
    bridge: true, http: true, magician: true, bridge_full: false,
    browser: { label: 'Comet' }, slot: 2, max_browsers: 4,
    browsers: [
      { fingerprint: 'fp000001', label: 'Chrome', slot: 1, default: true },
      { fingerprint: 'fp000003', label: 'Comet', slot: 2, default: false },
    ],
  });
  assert.equal(result.elements['status-text'].textContent, '✓ Connected as Comet (2 of 4)');
  assert.equal(result.automationServerReady, true);
  assert.match(result.elements['status-details'].innerHTML, /Comet, slot 2 of 4/);
  assert.match(result.elements['status-details'].innerHTML, /Chrome \(default\), Comet/);
});

test('full bridge panel names the slot limit and keeps automation disabled', async () => {
  const result = await panelStatus({
    bridge: false, bridge_full: true, bridge_busy: true, http: true, magician: true,
    max_browsers: 4, browsers: [{ label: 'Chrome' }, { label: 'Chrome' }, { label: 'Comet' }, { label: 'Dia' }],
  });
  assert.equal(result.elements.status.className, 'status waiting');
  assert.equal(result.elements['status-text'].textContent, 'All 4 browser slots are in use');
  assert.equal(result.automationServerReady, false);
  assert.match(result.elements['status-details'].innerHTML, /Full: 4 of 4 browsers connected/);
});

test('a still-open previous connection of ours is not reported as offline or full', async () => {
  const result = await panelStatus({
    bridge: false, bridge_full: false, bridge_id_in_use: true, http: true, magician: true, max_browsers: 4,
  });
  assert.equal(result.elements.status.className, 'status waiting');
  assert.equal(result.elements['status-text'].textContent, 'Reconnecting: previous connection still open');
  assert.equal(result.automationServerReady, false);
  assert.match(result.elements['status-details'].innerHTML, /previous connection to close/);
});

test('browser labels from the server are escaped in the details', async () => {
  const result = await panelStatus({
    bridge: true, http: true, magician: true,
    browser: { label: '<img src=x>' }, slot: 1, max_browsers: 4,
    browsers: [{ label: '<img src=x>', slot: 1, default: true }],
  });
  assert.doesNotMatch(result.elements['status-details'].innerHTML, /<img/);
  assert.match(result.elements['status-details'].innerHTML, /&lt;img src=x&gt;/);
});
