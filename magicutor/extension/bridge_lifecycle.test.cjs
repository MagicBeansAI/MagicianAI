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
      sockets.push(this);
    }

    send() {}
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
      'globalThis.__bridgeExports = { connectBridge, reconnectBridge, isBridgeConnected };'
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
