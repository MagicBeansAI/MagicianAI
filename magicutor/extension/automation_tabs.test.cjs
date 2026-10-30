const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

function load({ storage = {}, tabs = new Map(), sessions = new Map(), failing = new Set() } = {}) {
    const removed = [];
    const closedWindows = [];
    const chrome = {
        storage: { local: {
            async get(key) { return structuredClone({ [key]: storage[key] }); },
            async set(values) { Object.assign(storage, structuredClone(values)); }
        } },
        tabs: { async remove(id) {
            if (failing.has(id)) throw new Error('Temporary bridge error');
            if (!tabs.has(id)) throw new Error(`No tab with id: ${id}`);
            const window = tabs.get(id);
            tabs.delete(id);
            removed.push(id);
            if (![...tabs.values()].includes(window)) closedWindows.push(window);
        } },
        windows: { remove() { throw new Error('Must never close a whole window'); } }
    };
    const source = fs.readFileSync(path.join(__dirname, 'automation_tabs.js'), 'utf8')
        .replace(/import[^;]+;/g, '')
        .replace(/export \{([^}]+)\};/, 'globalThis.api = { $1 };');
    const context = { chrome, getSessionForTab: id => sessions.get(id), console, setTimeout, clearTimeout };
    vm.createContext(context);
    vm.runInContext(source, context);
    return { api: context.api, context, storage, tabs, sessions, failing, removed, closedWindows };
}

test('task cleanup preserves unrelated tabs in the same window and other sessions', async () => {
    const h = load({ tabs: new Map([[1, 10], [2, 10], [3, 20]]) });
    await h.api.rememberCreatedTabs('task-a', [1]);
    await h.api.rememberCreatedTabs('task-b', [3]);
    await h.api.closeAutomationTabs('task-a');
    assert.deepEqual(h.removed, [1]);
    assert.deepEqual([...h.tabs.keys()], [2, 3]);
    assert.deepEqual(h.closedWindows, []);
});

test('closing the last task tab closes only its now-empty window, including popup windows', async () => {
    const h = load({ tabs: new Map([[1, 10], [2, 20], [3, 30]]) });
    await h.api.rememberCreatedTabs('task-a', [1, 2]);
    await h.api.closeAutomationTabs('task-a');
    assert.deepEqual(h.removed, [1, 2]);
    assert.deepEqual(h.closedWindows, [10, 20]);
    assert.deepEqual([...h.tabs.keys()], [3]);
});

test('an owner-bound existing tab and an adopted tab stay open', async () => {
    const h = load({ tabs: new Map([[1, 10], [2, 10], [3, 10]]), sessions: new Map([[2, 'new-owner']]) });
    await h.api.rememberCreatedTabs('task', [2, 3]);
    await h.api.closeAutomationTabs('task', [1]);
    assert.deepEqual(h.removed, [3]);
    assert.deepEqual([...h.tabs.keys()], [1, 2]);
});

test('ownership survives a worker restart or extension update but not a browser restart with reused tab IDs', async () => {
    const h = load({ tabs: new Map([[1, 10]]) });
    await h.api.rememberCreatedTabs('task', [1]);
    const resumed = load({ storage: h.storage, tabs: h.tabs });
    await resumed.api.closeAutomationTabs('task');
    assert.deepEqual(resumed.removed, [1]);
    // Browser startup clears the persisted ledger before tab ids repeat.
    await h.api.rememberCreatedTabs('task', [1]);
    const started = load({ storage: h.storage, tabs: new Map([[1, 99]]) });
    await started.api.forgetAllAutomationTabs();
    await started.api.closeAutomationTabs('task');
    assert.deepEqual(started.removed, []);
});

test('partial failure preserves only failed tabs for retry and tolerates already closed tabs', async () => {
    const h = load({ tabs: new Map([[1, 10], [2, 10]]), failing: new Set([2]) });
    await h.api.rememberCreatedTabs('task', [1, 2, 3]);
    await assert.rejects(h.api.closeAutomationTabs('task'), /cleanup incomplete/);
    assert.deepEqual(h.removed, [1]);
    assert.deepEqual(Array.from(await h.api.automationTabOwners(), entry => Array.from(entry)), [[2, 'task']]);
    h.failing.clear();
    await h.api.closeAutomationTabs('task');
    assert.deepEqual(h.removed, [1, 2]);
});

test('a hung tab-close API cannot block ownership updates indefinitely', async () => {
    const h = load({ tabs: new Map([[1, 10]]) });
    await h.api.rememberCreatedTabs('task', [1]);
    h.context.chrome.tabs.remove = () => new Promise(() => {});
    // Only the 5 s tab-close deadline fires at once; storage deadlines run real.
    h.context.setTimeout = (callback, ms) => (ms === 5000 ? (queueMicrotask(callback), 0) : setTimeout(callback, ms));
    h.context.clearTimeout = id => clearTimeout(id);
    await assert.rejects(h.api.closeAutomationTabs('task'), /timed out/);
    await h.api.rememberCreatedTabs('another-task', [2]);
    assert.equal((await h.api.automationTabOwners()).get(1), 'task');
    assert.equal((await h.api.automationTabOwners()).get(2), 'another-task');
});

test('an explicit owner handoff releases ownership without closing pages', async () => {
    const h = load({ tabs: new Map([[1, 10]]) });
    await h.api.rememberCreatedTabs('task', [1]);
    await h.api.handoffAutomationTabs('task');
    await h.api.closeAutomationTabs('task');
    assert.deepEqual(h.removed, []);
});

test('concurrent tab registration does not drop ownership', async () => {
    const h = load();
    await Promise.all(Array.from({ length: 20 }, (_, id) => h.api.rememberCreatedTabs('task', [id])));
    assert.equal((await h.api.automationTabOwners()).size, 20);
});

test('cancellation during creation removes late-created tabs instead of orphaning them', async () => {
    const h = load({ tabs: new Map([[1, 10], [2, 10]]) });
    const ticket = h.api.beginTabCreation('task');
    await h.api.closeAutomationTabs('task');
    await assert.rejects(h.api.rememberCreatedTabs('task', [1], ticket), /Session ended/);
    assert.deepEqual(h.removed, [1]);
    // A subsequent task may deliberately reuse the released logical ID.
    const fresh = h.api.beginTabCreation('task');
    await h.api.rememberCreatedTabs('task', [2], fresh);
    await h.api.closeAutomationTabs('task');
    assert.deepEqual(h.removed, [1, 2]);
});

test('the terminal bridge action clears overlays and detaches a preserved owner tab', async () => {
    const h = load({ tabs: new Map([[1, 10], [2, 10]]) });
    await h.api.rememberCreatedTabs('task', [2]);
    const detached = [];
    const messages = [];
    let cleared = 0;
    let untracked = 0;
    Object.assign(h.context, h.api, {
        getSessionTabs: () => new Set([1, 2]),
        detachDebugger: async ({ tabId }) => { detached.push(tabId); },
        clearSession: () => { cleared++; },
        executionIdFromSessionId: () => 'execution',
        untrackExecution: () => { untracked++; },
        sendOverlayMessageToTabs: async (tabs, message) => { messages.push(message.action); },
        releaseSessionGroup: async () => {},
    });
    const source = fs.readFileSync(path.join(__dirname, 'background.js'), 'utf8');
    vm.runInContext(source.slice(source.indexOf('async function processActionInner('), source.indexOf('async function apiReplay(')), h.context);
    const result = await h.context.processActionInner('close_session_tabs', { sessionId: 'task', preserveTabs: [1] });
    assert.equal(result.cleared, true);
    assert.deepEqual(h.removed, [2]);
    assert.ok(detached.includes(1));
    assert.deepEqual(messages, ['overlay_panel_hide', 'overlay_hide']);
    assert.equal(cleared, 1);
    assert.equal(untracked, 1);
});

test('clearing an old session does not remove a tab adopted by another session', () => {
    const context = { console: { log() {}, warn() {} } };
    vm.createContext(context);
    const source = fs.readFileSync(path.join(__dirname, 'background_state.js'), 'utf8')
        .replace(/export \{[\s\S]*?\};\s*$/, 'globalThis.api = { registerSessionTab, clearSession, getSessionForTab };');
    vm.runInContext(source, context);
    context.api.registerSessionTab('old', 1);
    context.api.registerSessionTab('new', 1);
    context.api.clearSession('old');
    assert.equal(context.api.getSessionForTab(1), 'new');
});

test('a popup that opens just after cleanup is closed, after a handoff left alone', async () => {
    const h = load({ tabs: new Map([[1, 10], [2, 10], [3, 10]]) });
    await h.api.rememberCreatedTabs('task', [1]);
    await h.api.closeAutomationTabs('task');
    await h.api.rememberPopupTab('task', 2);
    assert.deepEqual(h.removed, [1, 2]);
    assert.equal((await h.api.automationTabOwners()).size, 0);
    await h.api.rememberCreatedTabs('handed', [3]);
    await h.api.handoffAutomationTabs('handed');
    await h.api.rememberPopupTab('handed', 4);
    assert.deepEqual(h.removed, [1, 2]);
    assert.equal((await h.api.automationTabOwners()).size, 0);
});

test('a popup from a live session joins its ledger and closes with it', async () => {
    const h = load({ tabs: new Map([[1, 10], [2, 11]]) });
    await h.api.rememberCreatedTabs('task', [1]);
    await h.api.rememberPopupTab('task', 2);
    await h.api.closeAutomationTabs('task');
    assert.deepEqual(h.removed, [1, 2]);
});

test('a storage call that never settles fails its own step and does not hold every later tab action', async () => {
    const h = load({ tabs: new Map([[1, 10], [2, 10]]) });
    // Deadlines shortened; the hang is real: this promise never settles.
    h.context.setTimeout = (fn, ms) => setTimeout(fn, Math.min(ms, 30));
    const realGet = h.context.chrome.storage.local.get;
    let hangs = 1;
    h.context.chrome.storage.local.get = key => (hangs-- > 0 ? new Promise(() => {}) : realGet(key));

    await assert.rejects(h.api.rememberCreatedTabs('stuck', [1]), /storage read timed out/);
    await h.api.rememberCreatedTabs('task', [2]);
    assert.deepEqual(Array.from(await h.api.automationTabOwners(), entry => Array.from(entry)), [[2, 'task']]);
    await h.api.closeAutomationTabs('task');
    assert.deepEqual(h.removed, [2]);
});
