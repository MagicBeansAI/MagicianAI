const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

// A small model of Chrome's windows, tabs and tab groups: a group lives in one
// window, moving a tab to another window takes it out of its group, and an
// empty group disappears.
function fakeChrome({ groups = true, focusedWindow = 1, storage = {} } = {}) {
    let nextId = 100;
    const windows = new Map([[1, { id: 1, type: 'normal', incognito: false }]]);
    const tabs = new Map([[10, { id: 10, windowId: 1, groupId: -1, active: true, url: 'https://owner.example/' }]]);
    const tabGroups = new Map();
    const dropEmptyGroups = () => {
        for (const id of tabGroups.keys()) {
            if (![...tabs.values()].some(tab => tab.groupId === id)) tabGroups.delete(id);
        }
    };
    const moveTab = (tab, windowId) => {
        if (tab.windowId !== windowId) {
            tab.windowId = windowId;
            tab.groupId = -1;
        }
    };
    const chrome = {
        storage: { local: {
            async get(key) { return structuredClone({ [key]: storage[key] }); },
            async set(values) { Object.assign(storage, structuredClone(values)); }
        } },
        windows: {
            async getLastFocused() { return focusedWindow ? windows.get(focusedWindow) : null; },
            async create({ tabId, url } = {}) {
                const window = { id: nextId++, type: 'normal', incognito: false };
                windows.set(window.id, window);
                if (tabId !== undefined) {
                    moveTab(tabs.get(tabId), window.id);
                    dropEmptyGroups();
                } else {
                    tabs.set(nextId, { id: nextId, windowId: window.id, groupId: -1, active: true, url });
                    nextId++;
                }
                return { ...window, tabs: [...tabs.values()].filter(tab => tab.windowId === window.id) };
            }
        },
        tabs: {
            async create({ windowId, url, active }) {
                const tab = { id: nextId++, windowId, groupId: -1, active: active !== false, url };
                if (tab.active) for (const other of tabs.values()) if (other.windowId === windowId) other.active = false;
                tabs.set(tab.id, tab);
                return { ...tab };
            },
            async get(id) {
                if (!tabs.has(id)) throw new Error(`No tab with id: ${id}`);
                return { ...tabs.get(id) };
            },
            async update(id, { active }) {
                const tab = tabs.get(id);
                if (active) for (const other of tabs.values()) if (other.windowId === tab.windowId) other.active = other.id === id;
                return { ...tab };
            },
            async query({ groupId }) { return [...tabs.values()].filter(tab => tab.groupId === groupId).map(tab => ({ ...tab })); },
            async move(ids, { windowId }) { for (const id of ids) moveTab(tabs.get(id), windowId); dropEmptyGroups(); },
            async remove(id) { tabs.delete(id); dropEmptyGroups(); },
            async ungroup(ids) { for (const id of ids) tabs.get(id).groupId = -1; dropEmptyGroups(); },
            async group({ groupId, tabIds, createProperties }) {
                if (!groups) throw new Error('tab groups unavailable');
                let id = groupId;
                if (id === undefined) {
                    id = nextId++;
                    tabGroups.set(id, { id, windowId: createProperties.windowId, title: '', color: 'grey' });
                }
                const group = tabGroups.get(id);
                for (const tabId of tabIds) {
                    moveTab(tabs.get(tabId), group.windowId);
                    tabs.get(tabId).groupId = id;
                }
                dropEmptyGroups();
                return id;
            }
        },
        tabGroups: groups ? {
            async get(id) {
                if (!tabGroups.has(id)) throw new Error(`No group with id: ${id}`);
                return { ...tabGroups.get(id) };
            },
            async update(id, props) { Object.assign(tabGroups.get(id), props); return { ...tabGroups.get(id) }; }
        } : undefined
    };
    return { chrome, windows, tabs, tabGroups, storage };
}

function load(options) {
    const browser = fakeChrome(options);
    const source = fs.readFileSync(path.join(__dirname, 'automation_surface.js'), 'utf8')
        .replace(/export \{([^}]+)\};/, 'globalThis.api = { $1 };');
    const context = { chrome: browser.chrome, console: { log() {}, warn() {} }, setTimeout, clearTimeout };
    vm.createContext(context);
    vm.runInContext(source, context);
    return { api: context.api, ...browser };
}

const remembered = [];
const remember = async tabIds => { remembered.push(...tabIds); };

test('group titles carry the mark, fit the tab strip and show status', () => {
    const { api } = load();
    assert.equal(api.groupTitle('Reddit launch research'), '✱ Reddit launch research');
    assert.equal(api.groupTitle('Reddit launch research', 'waiting'), '✱ Reddit launch research ⏸');
    assert.equal(api.groupTitle('Reddit launch research', 'done'), '✱ Reddit launch research ✓');
    const long = api.groupTitle('Compare every speedrun accelerator application deadline');
    assert.ok(long.length <= 28, long);
    assert.match(long, /^✱ Compare every speedrun…$/);
    assert.equal(api.colorFor('magician-exec_1'), api.colorFor('magician-exec_1'));
});

test('the agent name is a fallback that never replaces a name from Magician', async () => {
    const { api } = load();
    assert.equal(api.humanizeAgent('web-researcher'), 'Web researcher');
    const named = load();
    await named.api.openInOwnerWindow({ sessionId: 's', url: 'about:blank', label: 'Reddit launch research' }, remember);
    await named.api.setSessionGroupLabel('s', 'Web researcher', { fallback: true });
    assert.equal((await named.api.sessionSurface('s')).label, 'Reddit launch research');
    const unnamed = load();
    await unnamed.api.openInOwnerWindow({ sessionId: 's', url: 'about:blank' }, remember);
    await unnamed.api.setSessionGroupLabel('s', 'Web researcher', { fallback: true });
    assert.equal((await unnamed.api.sessionSurface('s')).label, 'Web researcher');
});

test('auto opens a background tab in the owner window, grouped and named, without taking their tab', async () => {
    const h = load();
    const result = await h.api.openInOwnerWindow({ sessionId: 'magician-exec_1', url: 'about:blank', label: 'Reddit research' }, remember);
    assert.equal(result.surface, 'in_window');
    assert.equal(result.windowId, 1);
    const tab = h.tabs.get(result.tabs[0].tabId);
    assert.equal(tab.active, false);
    assert.equal(h.tabs.get(10).active, true);
    assert.equal(h.tabGroups.get(tab.groupId).title, '✱ Reddit research');
    assert.ok(remembered.includes(tab.id));
});

test('without tab groups or an owner window the chain falls back to a plain or grouped own window', async () => {
    const noGroups = load({ groups: false });
    assert.equal(await noGroups.api.openInOwnerWindow({ sessionId: 's', url: 'about:blank' }, remember), null);
    const window = await noGroups.chrome.windows.create({ url: 'about:blank' });
    assert.deepEqual({ ...(await noGroups.api.groupOwnWindow({ sessionId: 's' }, window)) }, {
        surface: 'own_window',
        surfaceNote: 'tab groups unavailable'
    });

    const noWindow = load({ focusedWindow: null });
    assert.equal(await noWindow.api.openInOwnerWindow({ sessionId: 's', url: 'about:blank' }, remember), null);
    const own = await noWindow.chrome.windows.create({ url: 'about:blank' });
    const grouping = await noWindow.api.groupOwnWindow({ sessionId: 's', label: 'Chat' }, own);
    assert.equal(grouping.surface, 'own_window_group');
    assert.equal(noWindow.tabGroups.get(grouping.groupId).windowId, own.id);
});

test('a tab that does not render in the background moves to its own window and keeps its group name', async () => {
    const h = load();
    const opened = await h.api.openInOwnerWindow({ sessionId: 's', url: 'about:blank', label: 'Reddit research' }, remember);
    const tabId = opened.tabs[0].tabId;
    const visible = await h.api.rendersInBackground(async method => (
        method === 'Runtime.evaluate' ? { result: { result: { value: false } } } : {}
    ));
    assert.equal(visible, false);
    const windowId = await h.api.moveSessionToOwnWindow('s', [tabId], 'test');
    assert.notEqual(windowId, 1);
    assert.equal(h.tabs.get(tabId).windowId, windowId);
    assert.equal(h.tabGroups.get(h.tabs.get(tabId).groupId).title, '✱ Reddit research');
    assert.equal((await h.api.sessionSurface('s')).surface, 'own_window_group');
    assert.equal(await h.api.rendersInBackground(async method => (
        method === 'Runtime.evaluate' ? { result: { result: { value: true } } } : {}
    )), true);
});

test('status marks update the title; a hand-off ungroups the tabs', async () => {
    const h = load();
    const opened = await h.api.openInOwnerWindow({ sessionId: 's', url: 'about:blank', label: 'Task' }, remember);
    const groupId = h.tabs.get(opened.tabs[0].tabId).groupId;
    await h.api.setSessionGroupLabel('s', 'Reddit research');
    await h.api.setSessionGroupStatus('s', 'waiting');
    assert.equal(h.tabGroups.get(groupId).title, '✱ Reddit research ⏸');
    await h.api.setSessionGroupStatus('s', 'done');
    assert.equal(h.tabGroups.get(groupId).title, '✱ Reddit research ✓');
    await h.api.releaseSessionGroup('s', { ungroup: true });
    assert.equal(h.tabs.get(opened.tabs[0].tabId).groupId, -1);
    assert.equal(await h.api.sessionSurface('s'), null);
});

test('a popup joins the task group and gives the owner their tab back', async () => {
    const h = load();
    await h.api.noteActivatedTab({ tabId: 10, windowId: 1 });
    await h.api.openInOwnerWindow({ sessionId: 's', url: 'about:blank', label: 'Task' }, remember);
    const popup = await h.chrome.tabs.create({ windowId: 1, url: 'https://popup.example/', active: true });
    await h.api.adoptIntoSessionGroup('s', popup.id);
    assert.notEqual(h.tabs.get(popup.id).groupId, -1);
    assert.equal(h.tabs.get(10).active, true);
    assert.equal(h.tabs.get(popup.id).active, false);
});
