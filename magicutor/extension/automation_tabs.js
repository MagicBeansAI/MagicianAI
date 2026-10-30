// Created-tab ownership outlives a CDP connection, a service-worker restart and
// an extension reload or update (chrome.storage.local; chrome.storage.session is
// wiped on reload), but not a Chrome restart: numeric tab IDs can then be
// reused, so background.js clears it on runtime.onStartup.
import { getSessionForTab } from './background_state.js';

const STORAGE_KEY = 'magicutor_automation_tabs';
let pending = Promise.resolve();
const generations = new Map();
// Sessions whose tabs were just closed or handed off. A popup can still open
// from an owned tab while that runs; it must not start a fresh ledger entry.
const LATE_POPUP_WINDOW_MS = 60_000;
const endedSessions = new Map();
// Every tab action that reads or changes ownership (list_tabs, create_window,
// create_tab, close_session_tabs) waits on one queue. A storage call that never
// settled used to hold it, and every one of those actions, until the worker
// died: a 30-minute outage on 10-27 with nothing in the log but bridge
// timeouts. Each storage call now has a deadline, and the error names it.
const STORAGE_TIMEOUT_MS = 8_000;
// A reader stops waiting for queued changes in time to answer before the
// server's 10 s list_tabs timeout.
const READ_QUEUE_WAIT_MS = 4_000;

function bounded(promise, label, ms) {
    let timer;
    return Promise.race([
        promise,
        new Promise((_, reject) => {
            timer = setTimeout(() => reject(new Error(`Automation tab ledger: ${label} timed out after ${ms}ms`)), ms);
        })
    ]).finally(() => clearTimeout(timer));
}

function readOwners() {
    return bounded(chrome.storage.local.get(STORAGE_KEY), 'storage read', STORAGE_TIMEOUT_MS);
}

function markEnded(sessionId, mode) {
    const now = Date.now();
    for (const [id, ended] of endedSessions) if (now - ended.at > LATE_POPUP_WINDOW_MS) endedSessions.delete(id);
    endedSessions.set(sessionId, { mode, at: now });
}

async function removeCreatedTab(tabId) {
    let timer;
    try {
        await Promise.race([
            chrome.tabs.remove(tabId),
            new Promise((_, reject) => { timer = setTimeout(() => reject(new Error('Tab close timed out')), 5000); })
        ]);
    } finally {
        clearTimeout(timer);
    }
}

function beginTabCreation(sessionId) {
    endedSessions.delete(sessionId);
    if (!generations.has(sessionId)) generations.set(sessionId, {});
    return generations.get(sessionId);
}

function updateOwnership(change) {
    const next = pending.then(async () => {
        const stored = await readOwners();
        const owners = Object.assign(Object.create(null), stored[STORAGE_KEY] || {});
        const result = await change(owners);
        await bounded(chrome.storage.local.set({ [STORAGE_KEY]: owners }), 'storage write', STORAGE_TIMEOUT_MS);
        return result;
    });
    pending = next.catch(() => {});
    return next;
}

function rememberCreatedTabs(sessionId, tabIds, generation = beginTabCreation(sessionId)) {
    if (!sessionId) return Promise.resolve();
    return updateOwnership(async owners => {
        if (generation !== generations.get(sessionId)) {
            for (const tabId of tabIds) await removeCreatedTab(tabId);
            throw new Error('Session ended while its browser tab was being created');
        }
        const tabs = new Set(Object.hasOwn(owners, sessionId) ? owners[sessionId] : []);
        for (const tabId of tabIds) tabs.add(tabId);
        owners[sessionId] = [...tabs];
    });
}

// A popup opened by an owned tab joins its session. One that arrives just after
// the session ended is closed after cleanup, and left to the owner after a
// handoff, instead of being recorded under a new generation.
async function rememberPopupTab(sessionId, tabId) {
    if (!sessionId) return;
    const ended = endedSessions.get(sessionId);
    if (ended && Date.now() - ended.at <= LATE_POPUP_WINDOW_MS) {
        if (ended.mode === 'closed') await removeCreatedTab(tabId);
        return;
    }
    await rememberCreatedTabs(sessionId, [tabId], beginTabCreation(sessionId));
}

function forgetAutomationTab(tabId) {
    return updateOwnership(owners => {
        for (const sessionId of Object.keys(owners)) {
            owners[sessionId] = owners[sessionId].filter(id => id !== tabId);
            if (!owners[sessionId].length) delete owners[sessionId];
        }
    });
}

function forgetAllAutomationTabs() {
    generations.clear();
    endedSessions.clear();
    return updateOwnership(owners => { for (const sessionId of Object.keys(owners)) delete owners[sessionId]; });
}

function handoffAutomationTabs(sessionId) {
    generations.delete(sessionId);
    markEnded(sessionId, 'handoff');
    return updateOwnership(owners => { delete owners[sessionId]; });
}

async function automationTabOwners() {
    // Queued changes normally finish in milliseconds; past the wait, answer
    // from what is stored rather than time the caller out.
    await bounded(pending, 'queued ownership change', READ_QUEUE_WAIT_MS).catch(error => {
        console.warn(`[Magicutor] ${error.message}; reading tab ownership without it`);
    });
    const stored = await readOwners();
    return new Map(Object.entries(stored[STORAGE_KEY] || {})
        .flatMap(([sessionId, tabs]) => tabs.map(tabId => [tabId, sessionId])));
}

// Never remove a window: Chrome closes it naturally when its last tab goes.
// A tab handed in by the owner is not recorded as created and stays open.
function closeAutomationTabs(sessionId, preserveTabs = []) {
    generations.delete(sessionId);
    markEnded(sessionId, 'closed');
    return updateOwnership(async owners => {
        const remaining = [];
        const failures = [];
        const closedTabIds = [];
        const deadline = Date.now() + 8000;
        const preserve = new Set(preserveTabs);
        for (const tabId of Object.hasOwn(owners, sessionId) ? owners[sessionId] : []) {
            const currentOwner = getSessionForTab(tabId);
            if (preserve.has(tabId) || (currentOwner && currentOwner !== sessionId)) continue;
            try {
                if (Date.now() >= deadline) throw new Error('Tab cleanup deadline reached');
                await removeCreatedTab(tabId);
                closedTabIds.push(tabId);
            } catch (error) {
                // Already-closed tabs are successful cleanup, other errors are retryable.
                if (!/No tab with id|Invalid tab ID/i.test(error.message || '')) {
                    remaining.push(tabId);
                    failures.push(`${tabId}: ${error.message}`);
                }
            }
        }
        if (remaining.length) owners[sessionId] = remaining;
        else delete owners[sessionId];
        return { closedTabIds, failures };
    }).then(result => {
        if (result.failures.length) throw new Error(`Tab cleanup incomplete: ${result.failures.join('; ')}`);
        return result;
    });
}

export { beginTabCreation, rememberCreatedTabs, rememberPopupTab, forgetAutomationTab, forgetAllAutomationTabs, automationTabOwners, closeAutomationTabs, handoffAutomationTabs };
