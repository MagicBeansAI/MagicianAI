const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

function loadIdentity() {
  const context = { console: { log() {}, warn() {} }, Math, encodeURIComponent };
  context.globalThis = context;
  vm.createContext(context);
  const file = path.join(__dirname, 'browser_identity.js');
  const source = fs.readFileSync(file, 'utf8')
    .replace(/export\s*\{[\s\S]*?\};\s*$/, '')
    .concat(`
globalThis.__identity = {
  BROWSER_IDENTITY_STORAGE_KEY, bridgeIdentityQuery, detectBrowserBrand, getBrowserIdentity,
  normalizeBrowserLabel, publicBrowserIdentity, resetBrowserIdentityCacheForTests, setBrowserLabel,
};`);
  vm.runInContext(source, context, { filename: file });
  return context.__identity;
}

function memoryStorage(initial = {}) {
  const data = { ...initial };
  return {
    data,
    async get(key) { return key in data ? { [key]: data[key] } : {}; },
    async set(values) { Object.assign(data, values); },
  };
}

const chromeNav = {
  userAgentData: { brands: [{ brand: 'Not.A/Brand' }, { brand: 'Chromium' }, { brand: 'Google Chrome' }] },
  userAgent: 'Mozilla/5.0 Chrome/141.0.0.0 Safari/537.36',
};

function counter() {
  let minted = 0;
  return { next: () => `uuid-${++minted}-0123456789abcdef`, count: () => minted };
}

test('an install id and secret key are generated once and reused across worker restarts', async () => {
  const api = loadIdentity();
  const storage = memoryStorage();
  const uuids = counter();
  const first = await api.getBrowserIdentity({ storage, nav: chromeNav, randomUUID: uuids.next });
  assert.equal(first.id, 'uuid-1-0123456789abcdef');
  assert.equal(first.key, 'uuid-2-0123456789abcdef');
  assert.notEqual(first.key, first.id);
  assert.equal(first.label, 'Chrome');
  assert.equal(storage.data[api.BROWSER_IDENTITY_STORAGE_KEY].id, first.id);
  assert.equal(storage.data[api.BROWSER_IDENTITY_STORAGE_KEY].key, first.key);

  api.resetBrowserIdentityCacheForTests();
  const again = await api.getBrowserIdentity({ storage, nav: chromeNav, randomUUID: uuids.next });
  assert.equal(again.id, first.id);
  assert.equal(again.key, first.key);
  assert.equal(uuids.count(), 2);
});

test('concurrent first calls share one load and mint one identity', async () => {
  const api = loadIdentity();
  const storage = memoryStorage();
  const uuids = counter();
  const [a, b, c] = await Promise.all([
    api.getBrowserIdentity({ storage, nav: chromeNav, randomUUID: uuids.next }),
    api.getBrowserIdentity({ storage, nav: chromeNav, randomUUID: uuids.next }),
    api.getBrowserIdentity({ storage, nav: chromeNav, randomUUID: uuids.next }),
  ]);
  assert.equal(a.id, b.id);
  assert.equal(b.id, c.id);
  assert.equal(uuids.count(), 2);
});

test('the key is never part of the public identity', async () => {
  const api = loadIdentity();
  const identity = await api.getBrowserIdentity({
    storage: memoryStorage(), nav: chromeNav, randomUUID: counter().next,
  });
  const visible = api.publicBrowserIdentity(identity);
  assert.equal(visible.id, identity.id);
  assert.equal(visible.key, undefined);
  assert.equal(api.publicBrowserIdentity(null), null);
});

test('the brand names Chromium forks before the engine they report', () => {
  const api = loadIdentity();
  const brands = (...names) => ({ userAgentData: { brands: names.map((brand) => ({ brand })) } });
  assert.equal(api.detectBrowserBrand(brands('Chromium', 'Comet', 'Not)A;Brand')), 'Comet');
  assert.equal(api.detectBrowserBrand(brands('Microsoft Edge', 'Chromium')), 'Edge');
  assert.equal(api.detectBrowserBrand(brands('Brave', 'Chromium')), 'Brave');
  assert.equal(api.detectBrowserBrand(brands('Chromium')), 'Chromium');
  assert.equal(api.detectBrowserBrand({ userAgent: 'Mozilla/5.0 Chrome/141 Safari/537.36 Edg/141' }), 'Edge');
  assert.equal(api.detectBrowserBrand({}), 'Browser');
});

test('an owner label overrides the brand and an empty one restores it', async () => {
  const api = loadIdentity();
  const storage = memoryStorage({
    [api.BROWSER_IDENTITY_STORAGE_KEY]: { id: 'install-7', key: 'install-7-secret-0123', customLabel: '' },
  });
  const renamed = await api.setBrowserLabel('  Chrome\u0007 Work  ', { storage, nav: chromeNav });
  assert.equal(renamed.id, 'install-7');
  assert.equal(renamed.label, 'Chrome Work');
  assert.equal(storage.data[api.BROWSER_IDENTITY_STORAGE_KEY].customLabel, 'Chrome Work');
  // Renaming keeps the install's key.
  assert.equal(storage.data[api.BROWSER_IDENTITY_STORAGE_KEY].key, 'install-7-secret-0123');

  api.resetBrowserIdentityCacheForTests();
  assert.equal((await api.getBrowserIdentity({ storage, nav: chromeNav })).label, 'Chrome Work');

  const reset = await api.setBrowserLabel('', { storage, nav: chromeNav });
  assert.equal(reset.label, 'Chrome');
});

test('a stored id Magicutor would refuse is replaced', async () => {
  const api = loadIdentity();
  const storage = memoryStorage({
    [api.BROWSER_IDENTITY_STORAGE_KEY]: { id: '../not valid', customLabel: '' },
  });
  const identity = await api.getBrowserIdentity({
    storage, nav: chromeNav, randomUUID: () => 'fresh-id-0123456789abcdef',
  });
  assert.equal(identity.id, 'fresh-id-0123456789abcdef');
});

test('the handshake query carries the id, the key and the encoded label', () => {
  const api = loadIdentity();
  assert.equal(
    api.bridgeIdentityQuery({ id: 'install-7', key: 'install-7-secret-0123', label: 'Chrome Work & Play' }),
    '&browser_id=install-7&browser_key=install-7-secret-0123&browser_label=Chrome%20Work%20%26%20Play'
  );
  // A missing or malformed key is left out rather than sent.
  assert.equal(
    api.bridgeIdentityQuery({ id: 'install-7', key: 'short', label: 'Dia' }),
    '&browser_id=install-7&browser_label=Dia'
  );
  assert.equal(api.bridgeIdentityQuery(null), '');
});
