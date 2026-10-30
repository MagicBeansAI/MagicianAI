// Where a browser task's tabs open, and the tab group that names them.
//
// Fallback chain (`surface` from the proxy's bridge.automation_surface):
//   1. in_window: a background tab in the owner's last-focused window, in a
//      tab group titled "✱ <label>". Needs chrome.tabGroups and a page that
//      keeps rendering in the background (focus emulation, verified).
//   2. own_window_group: a window of its own that does not take focus, with
//      the task's tabs in the same kind of group.
//   3. own_window: that window without a group, where groups are unavailable.
// `own_window` skips step 1. A request without `surface` (an older proxy)
// keeps the old behaviour: a focused window, no group.
//
// A background tab is hidden by Chrome: no animation frames, timers at one a
// second, missed load events. Focus emulation keeps it rendering (measured
// 10-29: a 6-minute background tab stayed visible with it, went hidden
// without). An in-window tab whose page still reports hidden moves to a window
// of its own, at creation or on the periodic recheck.

const GROUPS_KEY = 'magicutor_automation_groups';
const MARK = '✱';
const MAX_TITLE_CHARS = 28;
const STATUS_MARKS = { waiting: '⏸', done: '✓' };
const COLORS = ['blue', 'red', 'yellow', 'green', 'pink', 'purple', 'cyan', 'orange', 'grey'];
const STORAGE_TIMEOUT_MS = 8_000;
const OWN_WINDOW_SIZE = { width: 1280, height: 800 };
// Resolves true only when the page is visible and draws a frame. `var`, not
// let/const: sendDebuggerCommand wraps let/const expressions in an IIFE that
// would drop the promise.
const RENDERING_PROBE = `new Promise(resolve => {
  if (document.visibilityState !== 'visible') return resolve(false);
  var drew = false;
  requestAnimationFrame(() => { drew = true; resolve(true); });
  setTimeout(() => { if (!drew) resolve(false); }, 1500);
})`;

let pending = Promise.resolve();
// windowId -> the owner's own last active tab there, restored when a task
// popup opens in front of it.
const lastOwnerTab = new Map();

function groupsSupported() {
    return typeof chrome.tabs?.group === 'function'
        && typeof chrome.tabGroups?.update === 'function'
        && typeof chrome.tabGroups?.get === 'function';
}

function colorFor(sessionId) {
    let hash = 2166136261;
    for (const ch of String(sessionId)) hash = Math.imul(hash ^ ch.codePointAt(0), 16777619);
    return COLORS[(hash >>> 0) % COLORS.length];
}

function defaultLabel(sessionId) {
    return /^magician-chat-/.test(String(sessionId)) ? 'Chat' : 'Task';
}

function humanizeAgent(agentId) {
    const words = String(agentId || '').replace(/[-_]+/g, ' ').trim();
    return words ? words[0].toUpperCase() + words.slice(1) : '';
}

function groupTitle(label, status) {
    const suffix = STATUS_MARKS[status] ? ` ${STATUS_MARKS[status]}` : '';
    const room = MAX_TITLE_CHARS - MARK.length - 1 - suffix.length;
    let text = String(label || 'Task').replace(/\s+/g, ' ').trim() || 'Task';
    if (text.length > room) {
        const cut = text.slice(0, room - 1);
        const space = cut.lastIndexOf(' ');
        text = `${(space > room / 2 ? cut.slice(0, space) : cut).replace(/[\s,;:.-]+$/, '')}…`;
    }
    return `${MARK} ${text}${suffix}`;
}

function bounded(promise, label) {
    let timer;
    return Promise.race([
        promise,
        new Promise((_, reject) => {
            timer = setTimeout(() => reject(new Error(`Automation groups: ${label} timed out`)), STORAGE_TIMEOUT_MS);
        })
    ]).finally(() => clearTimeout(timer));
}

function updateRecords(change) {
    const next = pending.then(async () => {
        const stored = await bounded(chrome.storage.local.get(GROUPS_KEY), 'storage read');
        const records = Object.assign(Object.create(null), stored[GROUPS_KEY] || {});
        const result = change(records);
        await bounded(chrome.storage.local.set({ [GROUPS_KEY]: records }), 'storage write');
        return result;
    });
    pending = next.catch(() => {});
    return next;
}

async function readRecords() {
    await pending;
    const stored = await bounded(chrome.storage.local.get(GROUPS_KEY), 'storage read');
    return stored[GROUPS_KEY] || {};
}

async function liveGroup(groupId) {
    if (groupId === null || groupId === undefined || !groupsSupported()) return null;
    try {
        return await chrome.tabGroups.get(groupId);
    } catch (_) {
        return null;
    }
}

// The session's record with its group's current window, or null.
async function sessionSurface(sessionId) {
    const record = (await readRecords())[sessionId];
    if (!record) return null;
    const group = await liveGroup(record.groupId);
    return { ...record, groupId: group ? group.id : null, windowId: group ? group.windowId : record.windowId };
}

// Put `tabIds` in the session's group, creating it in `windowId` if the
// session has none (or the owner closed it). Returns the group id.
async function groupSessionTabs(sessionId, tabIds, windowId, { label, surface } = {}) {
    const record = (await readRecords())[sessionId] || {};
    const existing = await liveGroup(record.groupId);
    const groupId = existing
        ? await chrome.tabs.group({ groupId: existing.id, tabIds })
        : await chrome.tabs.group({ tabIds, createProperties: { windowId } });
    const title = label || record.label || defaultLabel(sessionId);
    // A name from Magician (the CDP URL's ?label=) is never replaced by the
    // agent-name fallback.
    const named = Boolean(label) || Boolean(record.named);
    await chrome.tabGroups.update(groupId, {
        title: groupTitle(title, record.status),
        color: colorFor(sessionId),
        collapsed: false
    });
    const group = await chrome.tabGroups.get(groupId);
    await updateRecords(records => {
        records[sessionId] = {
            groupId,
            windowId: group.windowId,
            surface: surface || record.surface || 'own_window_group',
            label: title,
            named,
            status: record.status || null
        };
    });
    return groupId;
}

async function ownerWindow() {
    try {
        const window = await chrome.windows.getLastFocused({ windowTypes: ['normal'] });
        return window && !window.incognito && window.type === 'normal' ? window : null;
    } catch (_) {
        return null;
    }
}

// Step 1 of the chain. Returns a create_window result, or null to fall back.
async function openInOwnerWindow(params, rememberTabs) {
    if (!groupsSupported()) return null;
    const host = await ownerWindow();
    if (!host) return null;
    const url = params.url || 'about:blank';
    const tab = await chrome.tabs.create({ windowId: host.id, url, active: false });
    try {
        await rememberTabs([tab.id]);
    } catch (error) {
        await chrome.tabs.remove(tab.id).catch(() => {});
        throw error;
    }
    let groupId;
    try {
        groupId = await groupSessionTabs(params.sessionId, [tab.id], host.id, { label: params.label, surface: 'in_window' });
    } catch (error) {
        console.warn(`[Magicutor] Tab group in the owner's window failed (${error.message}); using a window of its own`);
        await chrome.tabs.remove(tab.id).catch(() => {});
        return null;
    }
    return { windowId: host.id, tabs: [{ tabId: tab.id, url }], surface: 'in_window', groupId };
}

// Steps 2 and 3: group a freshly created own window's tabs where possible.
async function groupOwnWindow(params, window) {
    if (!groupsSupported()) return { surface: 'own_window', surfaceNote: 'tab groups unavailable' };
    try {
        const groupId = await groupSessionTabs(params.sessionId, window.tabs.map(tab => tab.id), window.id, {
            label: params.label,
            surface: 'own_window_group'
        });
        return { surface: 'own_window_group', groupId };
    } catch (error) {
        return { surface: 'own_window', surfaceNote: `tab group failed: ${error.message}` };
    }
}

// `send(method, params)` runs a CDP command on the tab's main session.
async function rendersInBackground(send) {
    try {
        await send('Emulation.setFocusEmulationEnabled', { enabled: true });
        await send('Page.setWebLifecycleState', { state: 'active' });
        const reply = await send('Runtime.evaluate', {
            expression: RENDERING_PROBE,
            awaitPromise: true,
            returnByValue: true
        });
        const value = reply?.result?.result?.value ?? reply?.result?.value;
        return value === true;
    } catch (_) {
        return false;
    }
}

// Move a session's tabs out of the owner's window into one of their own,
// regrouped there. Returns the new window id.
async function moveSessionToOwnWindow(sessionId, tabIds, reason) {
    const [first, ...rest] = tabIds;
    const window = await chrome.windows.create({ tabId: first, focused: false, ...OWN_WINDOW_SIZE });
    if (rest.length) await chrome.tabs.move(rest, { windowId: window.id, index: -1 });
    const record = (await readRecords())[sessionId] || {};
    try {
        await groupSessionTabs(sessionId, tabIds, window.id, { surface: 'own_window_group' });
    } catch (_) {
        await updateRecords(records => {
            records[sessionId] = { ...record, groupId: null, windowId: window.id, surface: 'own_window' };
        });
    }
    console.warn(`[Magicutor] Moved session ${sessionId} to its own window ${window.id}: ${reason}`);
    return window.id;
}

async function sessionTabIds(groupId) {
    if (!(await liveGroup(groupId))) return [];
    return (await chrome.tabs.query({ groupId })).map(tab => tab.id);
}

// Periodic check: an in-window task tab that stopped rendering gets its
// emulation back, and moves out if that does not help.
async function recheckInWindowSessions(sendFor, isAttached) {
    const records = await readRecords();
    for (const [sessionId, record] of Object.entries(records)) {
        if (record.surface !== 'in_window') continue;
        const tabIds = await sessionTabIds(record.groupId);
        const probeTab = tabIds.find(isAttached);
        if (probeTab === undefined) continue;
        if (await rendersInBackground(sendFor(probeTab))) continue;
        await moveSessionToOwnWindow(sessionId, tabIds, 'page stopped rendering in the background')
            .catch(error => console.warn(`[Magicutor] Could not move session ${sessionId}: ${error.message}`));
    }
}

async function retitle(sessionId, change) {
    const record = (await readRecords())[sessionId];
    if (!record) return;
    const next = { ...record, ...change };
    if (next.label === record.label && next.status === record.status) return;
    await updateRecords(records => { if (records[sessionId]) records[sessionId] = { ...records[sessionId], ...change }; });
    const group = await liveGroup(record.groupId);
    if (group) await chrome.tabGroups.update(group.id, { title: groupTitle(next.label, next.status) }).catch(() => {});
}

function setSessionGroupStatus(sessionId, status) {
    return retitle(sessionId, { status: STATUS_MARKS[status] ? status : null });
}

// `fallback`: only for a group Magician did not name.
async function setSessionGroupLabel(sessionId, label, { fallback = false } = {}) {
    if (!label) return;
    if (fallback && (await readRecords())[sessionId]?.named) return;
    await retitle(sessionId, { label });
}

// Tabs handed to the owner leave the group; closed tabs take it with them.
async function releaseSessionGroup(sessionId, { ungroup = false } = {}) {
    const record = (await readRecords())[sessionId];
    if (!record) return;
    if (ungroup) {
        const tabIds = await sessionTabIds(record.groupId);
        if (tabIds.length) await chrome.tabs.ungroup(tabIds).catch(() => {});
    }
    await updateRecords(records => { delete records[sessionId]; });
}

function forgetAllGroups() {
    return updateRecords(records => { for (const id of Object.keys(records)) delete records[id]; });
}

// A popup or new tab of a task joins its group. One that opened in front of
// the owner's tab in their window gives the owner their tab back.
async function adoptIntoSessionGroup(sessionId, tabId) {
    const surface = await sessionSurface(sessionId);
    if (!surface) return;
    const tab = await chrome.tabs.get(tabId).catch(() => null);
    if (!tab) return;
    await groupSessionTabs(sessionId, [tabId], surface.windowId).catch(() => {});
    if (surface.surface === 'in_window' && tab.active) {
        const ownerTab = lastOwnerTab.get(tab.windowId);
        if (ownerTab !== undefined && ownerTab !== tabId) await chrome.tabs.update(ownerTab, { active: true }).catch(() => {});
    }
}

async function noteActivatedTab({ tabId, windowId }) {
    const tab = await chrome.tabs.get(tabId).catch(() => null);
    if (!tab) return;
    const ours = Object.values(await readRecords().catch(() => ({}))).some(record => record.groupId === tab.groupId);
    if (!ours) lastOwnerTab.set(windowId, tabId);
}

export {
    MARK,
    groupsSupported,
    colorFor,
    humanizeAgent,
    groupTitle,
    sessionSurface,
    openInOwnerWindow,
    groupOwnWindow,
    groupSessionTabs,
    rendersInBackground,
    moveSessionToOwnWindow,
    recheckInWindowSessions,
    setSessionGroupStatus,
    setSessionGroupLabel,
    releaseSessionGroup,
    forgetAllGroups,
    adoptIntoSessionGroup,
    noteActivatedTab
};
