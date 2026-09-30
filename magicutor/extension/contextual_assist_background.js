const PROBE_MESSAGE = { type: 'contextual_assist_probe' };
const PROBE_TIMEOUT_MS = 650;

function isHttpUrl(url) {
    return typeof url === 'string' && /^https?:\/\//i.test(url);
}

function withTimeout(promise, timeoutMs, fallback) {
    let timeoutId = null;
    const timeout = new Promise(resolve => {
        timeoutId = setTimeout(() => resolve(fallback), timeoutMs);
    });
    return Promise.race([promise, timeout]).finally(() => {
        if (timeoutId) clearTimeout(timeoutId);
    });
}

async function activeTab() {
    let tabs = await chrome.tabs.query({ active: true, lastFocusedWindow: true });
    if (!tabs.length) {
        tabs = await chrome.tabs.query({ active: true, currentWindow: true });
    }
    return tabs.find(tab => tab?.id) || null;
}

async function framesForTab(tabId) {
    try {
        const frames = await chrome.webNavigation.getAllFrames({ tabId });
        if (Array.isArray(frames) && frames.length > 0) {
            return frames;
        }
    } catch (_) {
        // Some pages disallow frame inspection; main-frame probing still works
        // when the content script is available.
    }
    return [{ frameId: 0 }];
}

async function probeFrame(tabId, frameId) {
    try {
        const response = await withTimeout(
            chrome.tabs.sendMessage(tabId, PROBE_MESSAGE, { frameId }),
            PROBE_TIMEOUT_MS,
            null
        );
        return response?.ok ? { ...response, frameId } : null;
    } catch (_) {
        return null;
    }
}

function probeRank(result) {
    if (!result?.eligible) return 0;
    if (result.state === 'selection-field') return 40;
    if (result.state === 'selection') return 35;
    if (result.hasSelection) return 30;
    if (result.state === 'draft') return 20;
    if (result.isWritable) return 15;
    return 1;
}

function summarizeIneligible(results) {
    if (results.some(result => result?.reason === 'secure_field')) {
        return 'secure_field';
    }
    if (results.some(result => result?.ok)) {
        return 'no_eligible_dom_context';
    }
    return 'content_script_unavailable';
}

export async function probeContextualAssist(_params = {}) {
    const tab = await activeTab();
    if (!tab?.id) {
        return { ok: true, eligible: false, reason: 'no_active_tab' };
    }

    if (!isHttpUrl(tab.url)) {
        return {
            ok: true,
            eligible: false,
            reason: 'unsupported_tab_url',
            tabId: tab.id,
            windowId: tab.windowId,
            url: tab.url || null,
            title: tab.title || null
        };
    }

    const frames = await framesForTab(tab.id);
    const probes = await Promise.all(
        frames.map(frame => probeFrame(tab.id, frame.frameId ?? 0))
    );
    const results = probes.filter(Boolean);
    const best = results
        .slice()
        .sort((a, b) => probeRank(b) - probeRank(a))[0];

    if (best?.eligible) {
        return {
            ...best,
            tabId: tab.id,
            windowId: tab.windowId,
            tabUrl: tab.url || null,
            tabTitle: tab.title || null,
            title: best.title || tab.title || null,
            url: best.url || tab.url || null
        };
    }

    return {
        ok: true,
        eligible: false,
        reason: summarizeIneligible(results),
        tabId: tab.id,
        windowId: tab.windowId,
        url: tab.url || null,
        title: tab.title || null
    };
}

export async function captureContextualAssistTab(params = {}) {
    let tab = null;
    if (Number.isInteger(params.tabId)) {
        try {
            tab = await chrome.tabs.get(params.tabId);
        } catch (_) {
            tab = null;
        }
    }
    if (!tab?.id) {
        tab = await activeTab();
    }
    if (!tab?.id) {
        return { ok: true, captured: false, reason: 'no_active_tab' };
    }
    if (!isHttpUrl(tab.url)) {
        return {
            ok: true,
            captured: false,
            reason: 'unsupported_tab_url',
            tabId: tab.id,
            windowId: tab.windowId,
            url: tab.url || null,
            title: tab.title || null
        };
    }

    let dataUrl = null;
    try {
        dataUrl = await chrome.tabs.captureVisibleTab(tab.windowId, {
            format: 'png'
        });
    } catch (error) {
        return {
            ok: true,
            captured: false,
            reason: 'capture_visible_tab_failed',
            error: error?.message || String(error),
            tabId: tab.id,
            windowId: tab.windowId,
            url: tab.url || null,
            title: tab.title || null
        };
    }
    if (typeof dataUrl !== 'string' || !dataUrl.startsWith('data:image/png;base64,')) {
        throw new Error('captureVisibleTab returned no PNG data');
    }

    return {
        ok: true,
        captured: true,
        captureMode: 'chrome_visible_tab',
        mimeType: 'image/png',
        imageB64: dataUrl.slice('data:image/png;base64,'.length),
        tabId: tab.id,
        windowId: tab.windowId,
        url: tab.url || null,
        title: tab.title || null,
        timestamp: Date.now()
    };
}
