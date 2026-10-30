// CDP setup a debugger re-attach loses. Chrome resets every domain when the
// debugger attaches. After a service-worker restart the extension re-attaches
// lazily (sendDebuggerCommand), and the CDP client never learns it has to
// re-enable Page/Network/...: its session keeps answering commands but gets no
// events, so every agent-browser `open` waits out its load timeout. An
// extension reload detaches every tab the same way. The tab's main-session
// setup commands are kept in chrome.storage.local, which outlives both the
// worker and a reload (chrome.storage.session does not survive a reload), and
// replayed on that re-attach before the command that triggered it. Tab ids
// restart with the browser, so the whole record is cleared on browser startup.

const STORAGE_KEY = 'magicutor_debugger_setup';
const MAX_COMMANDS_PER_TAB = 64;
const SETUP_METHODS = new Set([
    'Page.setLifecycleEventsEnabled',
    'Page.addScriptToEvaluateOnNewDocument',
    'Page.setBypassCSP',
    'Page.setWebLifecycleState',
    'Runtime.addBinding',
    'Target.setAutoAttach',
    'Target.setDiscoverTargets',
    'Network.setExtraHTTPHeaders',
    'Network.setCacheDisabled',
    'Network.setBypassServiceWorker',
    'Network.setUserAgentOverride'
]);

let pending = Promise.resolve();

function isSetupCommand(method) {
    return /\.(enable|disable)$/.test(method)
        || /^Emulation\.(set|clear)/.test(method)
        || SETUP_METHODS.has(method);
}

// Commands that undo each other share a key; a later setting replaces an
// earlier one, and `X.disable` / `Emulation.clearY` remove the entry.
function setupKey(method, params) {
    if (/\.(enable|disable)$/.test(method)) return method.replace(/\.(enable|disable)$/, '');
    if (method.startsWith('Emulation.clear')) return method.replace('Emulation.clear', 'Emulation.set');
    if (method === 'Runtime.addBinding') return `${method}:${params?.name}`;
    if (method === 'Page.addScriptToEvaluateOnNewDocument') return `${method}:${JSON.stringify(params ?? {})}`;
    return method;
}

function removesSetup(method) {
    return method.endsWith('.disable') || method.startsWith('Emulation.clear');
}

function update(change) {
    const next = pending.then(async () => {
        const stored = await chrome.storage.local.get(STORAGE_KEY);
        const byTab = Object.assign(Object.create(null), stored[STORAGE_KEY] || {});
        const result = change(byTab);
        await chrome.storage.local.set({ [STORAGE_KEY]: byTab });
        return result;
    });
    pending = next.catch(() => {});
    return next;
}

function recordSetupCommand(tabId, method, params) {
    if (!isSetupCommand(method)) return Promise.resolve();
    const key = setupKey(method, params);
    return update(byTab => {
        const commands = (byTab[tabId] || []).filter(command => command.key !== key);
        if (!removesSetup(method)) commands.push({ key, method, params: params ?? {} });
        if (commands.length) byTab[tabId] = commands.slice(-MAX_COMMANDS_PER_TAB);
        else delete byTab[tabId];
    });
}

async function setupCommandsFor(tabId) {
    await pending;
    const stored = await chrome.storage.local.get(STORAGE_KEY);
    return (stored[STORAGE_KEY] || {})[tabId] || [];
}

function forgetSetup(tabId) {
    return update(byTab => { delete byTab[tabId]; });
}

function forgetAllSetup() {
    return update(byTab => { for (const tabId of Object.keys(byTab)) delete byTab[tabId]; });
}

// Replays in recorded order; one failing command never blocks the rest or
// the command that triggered the re-attach.
async function replaySetupCommands(tabId, send) {
    const commands = await setupCommandsFor(tabId);
    let replayed = 0;
    for (const command of commands) {
        try {
            await send(command.method, command.params);
            replayed++;
        } catch (error) {
            console.warn(`[Magicutor] Re-attach replay of ${command.method} on tab ${tabId} failed: ${error.message}`);
        }
    }
    if (commands.length) {
        console.log(`[Magicutor] Re-attached tab ${tabId}: replayed ${replayed}/${commands.length} setup commands`);
    }
    return replayed;
}

export { isSetupCommand, recordSetupCommand, setupCommandsFor, forgetSetup, forgetAllSetup, replaySetupCommands };
