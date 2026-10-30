const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

// One module instance is one service-worker lifetime (or one extension load);
// `storage` is chrome.storage.local, which the next instance sees.
function load(storage = {}) {
    const chrome = {
        storage: { local: {
            async get(key) { return structuredClone({ [key]: storage[key] }); },
            async set(values) { Object.assign(storage, structuredClone(values)); }
        } }
    };
    const source = fs.readFileSync(path.join(__dirname, 'debugger_setup.js'), 'utf8')
        .replace(/export \{([^}]+)\};/, 'globalThis.api = { $1 };');
    const context = { chrome, console: { log() {}, warn() {} } };
    vm.createContext(context);
    vm.runInContext(source, context);
    return { api: context.api, storage };
}

async function replayed(api, tabId) {
    const sent = [];
    await api.replaySetupCommands(tabId, async (method, params) => { sent.push([method, params]); });
    return sent;
}

test('a restarted worker replays the tab setup the client sent, in order', async () => {
    const before = load();
    await before.api.recordSetupCommand(7, 'Page.enable', {});
    await before.api.recordSetupCommand(7, 'Page.setLifecycleEventsEnabled', { enabled: true });
    await before.api.recordSetupCommand(7, 'Runtime.evaluate', { expression: '1' });
    await before.api.recordSetupCommand(7, 'Network.enable', {});
    await before.api.recordSetupCommand(9, 'DOM.enable', {});

    const after = load(before.storage);
    assert.deepEqual(JSON.parse(JSON.stringify(await replayed(after.api, 7))), [
        ['Page.enable', {}],
        ['Page.setLifecycleEventsEnabled', { enabled: true }],
        ['Network.enable', {}]
    ]);
});

test('disable, clear and a later setting replace the earlier setup', async () => {
    const { api } = load();
    await api.recordSetupCommand(7, 'Network.enable', {});
    await api.recordSetupCommand(7, 'Network.disable', {});
    await api.recordSetupCommand(7, 'Emulation.setDeviceMetricsOverride', { width: 1 });
    await api.recordSetupCommand(7, 'Emulation.clearDeviceMetricsOverride', {});
    await api.recordSetupCommand(7, 'Page.setLifecycleEventsEnabled', { enabled: false });
    await api.recordSetupCommand(7, 'Page.setLifecycleEventsEnabled', { enabled: true });
    await api.recordSetupCommand(7, 'Runtime.addBinding', { name: 'a' });
    await api.recordSetupCommand(7, 'Runtime.addBinding', { name: 'b' });
    assert.deepEqual(JSON.parse(JSON.stringify(await replayed(api, 7))), [
        ['Page.setLifecycleEventsEnabled', { enabled: true }],
        ['Runtime.addBinding', { name: 'a' }],
        ['Runtime.addBinding', { name: 'b' }]
    ]);
});

test('a detached or closed tab has nothing to replay, and a failing command does not stop the rest', async () => {
    const { api } = load();
    await api.recordSetupCommand(7, 'Page.enable', {});
    await api.forgetSetup(7);
    assert.deepEqual(await replayed(api, 7), []);

    await api.recordSetupCommand(8, 'Page.enable', {});
    await api.recordSetupCommand(8, 'Network.enable', {});
    const sent = [];
    const count = await api.replaySetupCommands(8, async method => {
        sent.push(method);
        if (method === 'Page.enable') throw new Error('boom');
    });
    assert.deepEqual(sent, ['Page.enable', 'Network.enable']);
    assert.equal(count, 1);
});

test('setup survives an extension reload and is cleared when the browser starts', async () => {
    const before = load();
    await before.api.recordSetupCommand(7, 'Page.enable', {});
    const reloaded = load(before.storage);
    assert.deepEqual((await replayed(reloaded.api, 7)).map(([method]) => method), ['Page.enable']);
    await reloaded.api.forgetAllSetup();
    assert.deepEqual(await replayed(load(before.storage).api, 7), []);
});
