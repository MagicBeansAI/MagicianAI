const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

function loadPrompts() {
  const context = { console: { log() {}, warn() {} } };
  context.globalThis = context;
  vm.createContext(context);
  const file = path.join(__dirname, 'bridge_prompts.js');
  const source = fs.readFileSync(file, 'utf8')
    .replace(/export\s*\{[\s\S]*?\};\s*$/, '')
    .concat(`
globalThis.__prompts = {
  BRIDGE_CONSENT_STORAGE_KEY, badgeTextFor, bridgePromptsHtml, createBridgePrompts,
  readBridgeConsent, writeBridgeConsent,
};`);
  vm.runInContext(source, context, { filename: file });
  return context.__prompts;
}

function memoryStorage() {
  const data = {};
  return {
    data,
    async get(key) { return key in data ? { [key]: data[key] } : {}; },
    async set(values) { Object.assign(data, values); },
  };
}

test('a pending browser shows the pairing prompt and a badge', () => {
  const api = loadPrompts();
  const prompts = api.createBridgePrompts();
  assert.equal(api.badgeTextFor(prompts.snapshot()), '');
  prompts.applyNotice({ type: 'browser_state', state: 'pending', fingerprint: 'a1b2c3d4' });
  const snapshot = prompts.snapshot();
  assert.equal(snapshot.pairing, 'pending');
  assert.equal(snapshot.needsAttention, true);
  assert.equal(api.badgeTextFor(snapshot), '!');
  const html = api.bridgePromptsHtml(snapshot);
  assert.match(html, /Connect this browser to Magician\?/);
  assert.match(html, /data-bridge-action="accept"/);
  assert.match(html, /data-bridge-action="decline"/);
  assert.match(html, /a1b2c3d4/);

  prompts.applyNotice({ type: 'browser_state', state: 'accepted', fingerprint: 'a1b2c3d4' });
  assert.equal(prompts.snapshot().needsAttention, false);
  assert.equal(api.bridgePromptsHtml(prompts.snapshot()), '');
});

test('a declined browser offers only Connect and no badge', () => {
  const api = loadPrompts();
  const prompts = api.createBridgePrompts();
  prompts.applyNotice({ type: 'browser_state', state: 'pending', fingerprint: 'a1b2c3d4' });
  const snapshot = prompts.snapshot({ consent: 'declined' });
  assert.equal(snapshot.needsAttention, false);
  const html = api.bridgePromptsHtml(snapshot);
  assert.match(html, /not connected to Magician/);
  assert.match(html, /data-bridge-action="connect"/);
  assert.doesNotMatch(html, /data-bridge-action="accept"/);
});

test('choice cards show Accept and Always, and go when withdrawn or answered', () => {
  const api = loadPrompts();
  let clock = 1000;
  const prompts = api.createBridgePrompts({ now: () => clock });
  prompts.applyNotice({ type: 'browser_state', state: 'accepted', fingerprint: 'a1b2c3d4' });
  prompts.applyNotice({ type: 'browser_choice_request', choice_id: 'c1', task: 'Book <a> table', timeout_secs: 20 });
  prompts.applyNotice({ type: 'browser_choice_request', choice_id: 'c2', task: 'Pay', timeout_secs: 20 });
  let snapshot = prompts.snapshot();
  assert.equal(snapshot.choices.length, 2);
  assert.equal(api.badgeTextFor(snapshot), '!');
  const html = api.bridgePromptsHtml(snapshot);
  assert.match(html, /Use this browser for “Book &lt;a&gt; table”\?/);
  assert.match(html, /data-choice-id="c1" data-always="false">Accept/);
  assert.match(html, /data-choice-id="c1" data-always="true">Always use this browser/);

  assert.equal(prompts.applyNotice({ type: 'browser_choice_withdrawn', choice_id: 'c1' }), true);
  assert.equal(prompts.answered('c2'), true);
  assert.equal(prompts.snapshot().choices.length, 0);

  // A card the server never withdrew still expires with its timeout.
  prompts.applyNotice({ type: 'browser_choice_request', choice_id: 'c3', task: 'Late', timeout_secs: 5 });
  clock += 6000;
  assert.equal(prompts.snapshot().choices.length, 0);
});

test('a closed bridge drops its prompts', () => {
  const api = loadPrompts();
  const prompts = api.createBridgePrompts();
  prompts.applyNotice({ type: 'browser_state', state: 'accepted', fingerprint: 'f' });
  prompts.applyNotice({ type: 'browser_choice_request', choice_id: 'c1', task: 'x', timeout_secs: 20 });
  prompts.applyNotice({ type: 'bridge_closed' });
  const snapshot = prompts.snapshot();
  assert.equal(snapshot.pairing, null);
  assert.equal(snapshot.choices.length, 0);
});

test('consent is stored and unknown values read as pending', async () => {
  const api = loadPrompts();
  const storage = memoryStorage();
  assert.equal(await api.readBridgeConsent(storage), 'pending');
  await api.writeBridgeConsent('declined', storage);
  assert.equal(await api.readBridgeConsent(storage), 'declined');
  storage.data[api.BRIDGE_CONSENT_STORAGE_KEY] = 'bogus';
  assert.equal(await api.readBridgeConsent(storage), 'pending');
});
