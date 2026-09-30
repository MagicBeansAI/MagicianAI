const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const http = require('node:http');
const path = require('node:path');
const vm = require('node:vm');

function loadConfig(initialStorage = {}) {
  const storage = { ...initialStorage };
  const context = {
    AbortController,
    URL,
    Promise,
    setTimeout,
    clearTimeout,
    console: { log() {}, warn() {}, error() {}, debug() {} },
    chrome: {
      storage: {
        local: {
          async get(key) { return { [key]: storage[key] }; },
          async set(values) { Object.assign(storage, values); }
        }
      }
    }
  };
  context.globalThis = context;
  vm.createContext(context);

  const sourcePath = path.join(__dirname, 'config.js');
  const source = fs.readFileSync(sourcePath, 'utf8')
    .replace(/export\s+(let|const)\s+/g, '$1 ')
    .replace(/export\s+async\s+function\s+/g, 'async function ')
    .concat(`
      globalThis.__configExports = {
        refreshRuntimeEndpoints,
        values: () => ({
          magicianApiBase: MAGICIAN_API_BASE,
          magicianHealthUrl: MAGICIAN_HEALTH_URL,
          magicutorApiBase: MAGICUTOR_API_BASE,
          magicutorBridgeUrl: MAGICUTOR_BRIDGE_URL
        })
      };
    `);
  vm.runInContext(source, context, { filename: sourcePath });
  return { api: context.__configExports, storage };
}

const discovered = {
  schemaVersion: 1,
  magicianApiBase: 'http://127.0.0.1:4102/api/magician/v2',
  magicianHealthUrl: 'http://127.0.0.1:4102/health',
  magicutorApiBase: 'http://127.0.0.1:4103',
  magicutorBridgeUrl: 'ws://127.0.0.1:4103/bridge/native'
};

async function listen(handler) {
  const server = http.createServer(handler);
  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', resolve);
  });
  return {
    server,
    port: server.address().port,
    close: () => new Promise((resolve, reject) => {
      server.closeAllConnections?.();
      server.close((error) => error ? reject(error) : resolve());
    })
  };
}

test('runtime discovery applies configured loopback service ports', async () => {
  const { api, storage } = loadConfig();
  const result = await api.refreshRuntimeEndpoints({
    fetchImpl: async () => ({ ok: true, json: async () => discovered })
  });

  assert.equal(result.source, 'gateway');
  assert.equal(result.changed, true);
  const values = api.values();
  assert.equal(values.magicianApiBase, discovered.magicianApiBase);
  assert.equal(values.magicianHealthUrl, discovered.magicianHealthUrl);
  assert.equal(values.magicutorApiBase, discovered.magicutorApiBase);
  assert.equal(values.magicutorBridgeUrl, discovered.magicutorBridgeUrl);
  assert.equal(storage.magician_runtime_endpoints_v1.magicutorApiBase, discovered.magicutorApiBase);
});

test('runtime discovery uses cached endpoints when the gateway is unavailable', async () => {
  const { api } = loadConfig({ magician_runtime_endpoints_v1: discovered });
  const result = await api.refreshRuntimeEndpoints({
    fetchImpl: async () => { throw new Error('offline'); }
  });

  assert.equal(result.source, 'cache');
  assert.equal(result.changed, true);
  assert.equal(api.values().magicutorBridgeUrl, discovered.magicutorBridgeUrl);
});

test('runtime discovery rejects non-loopback endpoint contracts', async () => {
  const { api } = loadConfig();
  const result = await api.refreshRuntimeEndpoints({
    fetchImpl: async () => ({
      ok: true,
      json: async () => ({ ...discovered, magicianApiBase: 'https://example.com/api' })
    })
  });

  assert.equal(result.source, 'default');
  assert.equal(api.values().magicianApiBase, 'http://127.0.0.1:3002/api/magician/v2');
});

test('runtime discovery qualifies real non-default loopback service ports', async (t) => {
  const magician = await listen((request, response) => {
    response.writeHead(request.url === '/health' ? 200 : 404, {
      'connection': 'close',
      'content-type': 'application/json'
    });
    response.end(JSON.stringify({ ok: request.url === '/health' }));
  });
  const magicutor = await listen((_request, response) => {
    response.writeHead(204, { 'connection': 'close' });
    response.end();
  });
  const contract = {
    schemaVersion: 1,
    magicianApiBase: `http://127.0.0.1:${magician.port}/api/magician/v2`,
    magicianHealthUrl: `http://127.0.0.1:${magician.port}/health`,
    magicutorApiBase: `http://127.0.0.1:${magicutor.port}`,
    magicutorBridgeUrl: `ws://127.0.0.1:${magicutor.port}/bridge/native`
  };
  const gateway = await listen((_request, response) => {
    response.writeHead(200, { 'connection': 'close', 'content-type': 'application/json' });
    response.end(JSON.stringify(contract));
  });
  t.after(async () => {
    await Promise.all([gateway.close(), magicutor.close(), magician.close()]);
  });

  const { api } = loadConfig();
  const result = await api.refreshRuntimeEndpoints({
    fetchImpl: fetch,
    discoveryUrl: `http://127.0.0.1:${gateway.port}/host/runtime/endpoints`
  });

  assert.equal(result.source, 'gateway');
  assert.equal(api.values().magicianApiBase, contract.magicianApiBase);
  assert.equal(api.values().magicianHealthUrl, contract.magicianHealthUrl);
  assert.equal(api.values().magicutorApiBase, contract.magicutorApiBase);
  assert.equal(api.values().magicutorBridgeUrl, contract.magicutorBridgeUrl);
  assert.equal((await fetch(api.values().magicianHealthUrl)).status, 200);
  assert.equal((await fetch(api.values().magicutorApiBase)).status, 204);
});
