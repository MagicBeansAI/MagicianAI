const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

function loadBridge() {
  const sockets = [];
  const timeouts = new Map();
  let nextTimer = 1;

  class FakeWebSocket {
    static CONNECTING = 0;
    static OPEN = 1;
    static CLOSED = 3;

    constructor(url) {
      this.url = url;
      this.readyState = FakeWebSocket.CONNECTING;
      this.bufferedAmount = 0;
      this.sent = [];
      sockets.push(this);
    }

    send(data) { this.sent.push(data); }
    close() { this.readyState = FakeWebSocket.CLOSED; }
  }

  const context = {
    WebSocket: FakeWebSocket,
    console: { log() {}, warn() {}, error() {}, debug() {} },
    setTimeout(callback, delay) {
      const id = nextTimer++;
      timeouts.set(id, { callback, delay });
      return id;
    },
    clearTimeout(id) { timeouts.delete(id); },
    setInterval() { return nextTimer++; },
    clearInterval() {},
    Date,
    Math: Object.create(Math),
    Promise,
  };
  context.Math.random = () => 0;
  context.globalThis = context;
  vm.createContext(context);

  const sourcePath = path.join(__dirname, 'bridge.js');
  const source = fs.readFileSync(sourcePath, 'utf8')
    .replace(
      /import\s*\{\s*MAGICUTOR_BRIDGE_URL\s*\}\s*from\s*['"]\.\/config\.js['"];?/,
      "const MAGICUTOR_BRIDGE_URL = 'ws://127.0.0.1:3003/bridge/native';"
    )
    .replace(
      /export\s*\{[\s\S]*?\};\s*$/,
      'globalThis.__bridgeExports = { connectBridge, reconnectBridge, isBridgeConnected, sendBrowserLabelToBridge, sendBridgeMessage, disconnectBridge };'
    );
  vm.runInContext(source, context, { filename: sourcePath });
  return { api: context.__bridgeExports, sockets, timeouts };
}

test('concurrent bridge connects create exactly one WebSocket', async () => {
  const { api, sockets } = loadBridge();
  const config = { checkHttpHealth: async () => true, onRequest: async () => {} };
  await Promise.all(Array.from({ length: 100 }, () => api.connectBridge(config)));
  assert.equal(sockets.length, 1);
});

test('repeated stale close callbacks schedule only one reconnect', async () => {
  const { api, sockets, timeouts } = loadBridge();
  const config = { checkHttpHealth: async () => true, onRequest: async () => {} };
  await api.connectBridge(config);
  assert.equal(sockets.length, 1);
  const socket = sockets[0];
  socket.readyState = socket.constructor.OPEN;
  socket.onopen();

  const close = socket.onclose;
  for (let i = 0; i < 100; i++) {
    close({ code: 1006, reason: '', wasClean: false });
  }
  assert.equal(timeouts.size, 1);
  assert.equal([...timeouts.values()][0].delay, 5000);
});

test('explicit bridge reconnect retires the old socket before connecting again', async () => {
  const { api, sockets } = loadBridge();
  const config = { checkHttpHealth: async () => true, onRequest: async () => {} };
  await api.connectBridge(config);
  sockets[0].readyState = sockets[0].constructor.OPEN;
  sockets[0].onopen();

  await api.reconnectBridge(config);

  assert.equal(sockets.length, 2);
  assert.equal(sockets[0].readyState, sockets[0].constructor.CLOSED);
});

test('rejected handshakes back off without ever reporting an open bridge', async () => {
  const { api, sockets, timeouts } = loadBridge();
  const config = { checkHttpHealth: async () => true, onRequest: async () => {} };
  await api.connectBridge(config);
  for (const delay of [5000, 10000, 20000, 40000, 60000, 60000]) {
    const socket = sockets.at(-1);
    socket.readyState = socket.constructor.CLOSED;
    socket.onclose({ code: 1006, reason: '', wasClean: false });
    assert.equal(api.isBridgeConnected(), false);
    assert.equal(timeouts.size, 1);
    const [id, timer] = [...timeouts.entries()][0];
    assert.equal(timer.delay, delay);
    timeouts.delete(id);
    timer.callback();
    await new Promise((resolve) => setImmediate(resolve));
  }
});

test('the handshake names this browser so several browsers can share the bridge', async () => {
  const { api, sockets } = loadBridge();
  const config = {
    checkHttpHealth: async () => true,
    onRequest: async () => {},
    identityQuery: async () => '&browser_id=install-1&browser_label=Comet',
  };
  await api.connectBridge(config);
  assert.equal(sockets.length, 1);
  assert.match(sockets[0].url, /\?token=[^&]+&browser_id=install-1&browser_label=Comet$/);

  // A reconnect states the same identity, so Magicutor replaces only this
  // browser's own stale socket.
  sockets[0].readyState = sockets[0].constructor.OPEN;
  sockets[0].onopen();
  await api.reconnectBridge(config);
  assert.equal(sockets.length, 2);
  assert.match(sockets[1].url, /&browser_id=install-1&/);
});

test('a failing identity lookup still connects, without an identity', async () => {
  const { api, sockets } = loadBridge();
  await api.connectBridge({
    checkHttpHealth: async () => true,
    onRequest: async () => {},
    identityQuery: async () => { throw new Error('storage unavailable'); },
  });
  assert.equal(sockets.length, 1);
  assert.doesNotMatch(sockets[0].url, /browser_id=/);
});

test('renaming the browser updates the live bridge only while it is open', async () => {
  const { api, sockets } = loadBridge();
  await api.connectBridge({ checkHttpHealth: async () => true, onRequest: async () => {} });
  assert.equal(api.sendBrowserLabelToBridge('Chrome Work'), false);
  sockets[0].readyState = sockets[0].constructor.OPEN;
  sockets[0].onopen();
  assert.equal(api.sendBrowserLabelToBridge('Chrome Work'), true);
  assert.deepEqual(JSON.parse(sockets[0].sent.at(-1)), { type: 'browser_label', label: 'Chrome Work' });
});

test('server notices reach onNotice and a close reports bridge_closed', async () => {
  const { api, sockets } = loadBridge();
  const notices = [];
  const requests = [];
  await api.connectBridge({
    checkHttpHealth: async () => true,
    onRequest: async (request) => requests.push(request),
    onNotice: (envelope) => notices.push(envelope.type),
  });
  const socket = sockets[0];
  socket.readyState = socket.constructor.OPEN;
  socket.onopen();
  await socket.onmessage({ data: JSON.stringify({ type: 'browser_state', state: 'pending', fingerprint: 'f' }) });
  await socket.onmessage({ data: JSON.stringify({ type: 'browser_choice_request', choice_id: 'c', task: 't', timeout_secs: 20 }) });
  await socket.onmessage({ data: JSON.stringify({ type: 'request', data: { requestId: 'r', action: 'list_tabs' } }) });
  assert.deepEqual(notices, ['browser_state', 'browser_choice_request']);
  assert.equal(requests.length, 1);
  assert.equal(api.sendBridgeMessage({ type: 'browser_consent', accepted: true }), true);
  assert.deepEqual(JSON.parse(socket.sent.at(-1)), { type: 'browser_consent', accepted: true });
  socket.onclose({ code: 1000, reason: '', wasClean: true });
  assert.equal(notices.at(-1), 'bridge_closed');
});

test('declining disconnects and schedules no reconnect', async () => {
  const { api, sockets, timeouts } = loadBridge();
  await api.connectBridge({ checkHttpHealth: async () => true, onRequest: async () => {} });
  sockets[0].readyState = sockets[0].constructor.OPEN;
  sockets[0].onopen();
  api.disconnectBridge();
  assert.equal(api.isBridgeConnected(), false);
  assert.equal(sockets[0].readyState, sockets[0].constructor.CLOSED);
  // The retired socket's late close callback is stale and cannot reconnect.
  sockets[0].onclose({ code: 1000, reason: '', wasClean: true });
  assert.equal(timeouts.size, 0);
  assert.equal(api.sendBridgeMessage({ type: 'browser_consent', accepted: false }), false);
});
