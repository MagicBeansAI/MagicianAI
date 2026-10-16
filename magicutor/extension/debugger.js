import { debuggerSessions } from './background_state.js';
import { handleLivePreviewDebuggerDetach } from './live_preview_background.js';
import { sendCdpEventToBridge } from './bridge.js';
import { sendDebuggerCommand } from './debugger_actions.js';

// High-frequency CDP notifications that agent-browser's request/response model
// never consumes (it re-queries the DOM/CSS on demand via getDocument/snapshot).
// A `DOM.enable` / `CSS.enable` on a dynamic page (e.g. an infinite-scroll feed
// like x.com) makes Chrome emit thousands of these per second; forwarding each one
// over the bridge floods the WebSocket and can OOM/stall the MV3 service worker —
// the reported "DOM enable → crash". Dropping them is safe; the navigation/
// lifecycle events agent-browser DOES wait on (Page.*, Target.*, Runtime.*,
// Network.*, Fetch.*) are still forwarded.
const FLOOD_PRONE_CDP_EVENTS = new Set([
    'DOM.attributeModified',
    'DOM.attributeRemoved',
    'DOM.characterDataModified',
    'DOM.childNodeCountUpdated',
    'DOM.childNodeInserted',
    'DOM.childNodeRemoved',
    'DOM.setChildNodes',
    'DOM.inlineStyleInvalidated',
    'DOM.pseudoElementAdded',
    'DOM.pseudoElementRemoved',
    'DOM.distributedNodesUpdated',
    'DOM.scrollableFlagUpdated',
    'CSS.styleSheetChanged',
    'CSS.fontsUpdated',
    'CSS.mediaQueryResultChanged',
    'Animation.animationCreated',
    'Animation.animationStarted',
    'Animation.animationUpdated',
]);

// ---------------------------------------------------------------------------
// Response-body capture at the source.
//
// magicutor's api-mining needs the response body of XHR/fetch calls. It used to
// ask for them AFTER the fact: `Network.loadingFinished` travelled extension →
// bridge → magicutor, which then sent `Network.getResponseBody` back over the
// bridge to the extension. Two full WebSocket round-trips elapsed before Chrome
// was asked, and Chrome had usually evicted the buffer by then — measured at a
// ~59% loss on completed 200s, which starves the whole mining pipeline
// downstream (no body ⇒ no data-flow inference ⇒ hollow workflows).
//
// The extension already holds the event at time zero, so it fetches the body
// here with a single in-process call and attaches it to the forwarded params.
// magicutor keeps its round-trip fetch as a fallback, so this is purely
// additive.
//
// Guardrails, given this worker's history of being wedged by event volume:
//   * XHR/fetch ONLY. `loadingFinished` carries no resourceType, so it is
//     remembered from `responseReceived` in a hard-capped map.
//   * The map is evicted on finish/fail and bounded, so it cannot leak on a
//     long-lived tab.
//   * Bodies are size-capped before they reach the WebSocket.
//   * Only XHR/fetch events are ever delayed; navigation-critical Page.* /
//     Target.* events keep forwarding immediately.
const BODY_CAPTURE_TYPES = new Set(['xhr', 'fetch']);
const MAX_CAPTURED_BODY_BYTES = 256 * 1024;
const MAX_TRACKED_REQUESTS = 512;

/** tabId -> Map(requestId -> resourceType), for the XHR/fetch gate. */
const trackedRequestTypes = new Map();

function rememberRequestType(tabId, requestId, resourceType) {
    if (!requestId || !resourceType) return;
    if (!BODY_CAPTURE_TYPES.has(String(resourceType).toLowerCase())) return;
    let perTab = trackedRequestTypes.get(tabId);
    if (!perTab) {
        perTab = new Map();
        trackedRequestTypes.set(tabId, perTab);
    }
    // Bound the map: drop the oldest entry rather than grow without limit on a
    // page that never fires loadingFinished for some requests.
    if (perTab.size >= MAX_TRACKED_REQUESTS) {
        const oldest = perTab.keys().next();
        if (!oldest.done) perTab.delete(oldest.value);
    }
    perTab.set(requestId, resourceType);
}

function takeRequestType(tabId, requestId) {
    const perTab = trackedRequestTypes.get(tabId);
    if (!perTab) return null;
    const type = perTab.get(requestId) || null;
    perTab.delete(requestId);
    if (perTab.size === 0) trackedRequestTypes.delete(tabId);
    return type;
}

function forgetTab(tabId) {
    trackedRequestTypes.delete(tabId);
}

/**
 * Fetch a response body locally, while Chrome still has it buffered.
 * Returns null for binary content, oversized bodies, or any CDP error —
 * magicutor's fallback path then applies and records its own reason.
 */
async function captureResponseBodyNow(tabId, requestId) {
    try {
        const result = await sendDebuggerCommand({
            tabId,
            method: 'Network.getResponseBody',
            params: { requestId }
        });
        const cdp = result?.result ?? result;
        const body = cdp?.body;
        if (typeof body !== 'string') return null;
        // Binary bodies are skipped for the same reason magicutor skips them:
        // they are never useful as api-mining evidence and cost WS bandwidth.
        if (cdp?.base64Encoded === true) return null;
        if (body.length > MAX_CAPTURED_BODY_BYTES) {
            return `${body.slice(0, MAX_CAPTURED_BODY_BYTES)}...[truncated]`;
        }
        return body;
    } catch (_) {
        return null;
    }
}

/**
 * Setup debugger listeners.
 */
function setupDebuggerListeners() {
    chrome.debugger.onDetach.addListener((source, reason) => {
        console.log('[Magicutor] Debugger detached:', source, reason);
        if (source.tabId) {
            debuggerSessions.delete(source.tabId);
            forgetTab(source.tabId);
            handleLivePreviewDebuggerDetach(source.tabId, reason);
        }
    });

    chrome.debugger.onEvent.addListener(async (source, method, params) => {
        if (!source.tabId) return;
        const tabId = source.tabId;

        // Forward every chrome.debugger event to the magicutor CDP proxy so
        // agent-browser receives the canonical CDP event stream
        // (Page.loadEventFired, Target.targetCreated, Page.frameNavigated,
        // Runtime.executionContextCreated, etc.). Without this, agent-browser
        // hangs on `wait for navigation` style operations because the events
        // it expects from real Chrome never arrive over its CDP WebSocket.
        // Fire-and-forget; the bridge function silently no-ops if disconnected.
        // Skip flood-prone mutation notifications (see FLOOD_PRONE_CDP_EVENTS).
        // sendCdpEventToBridge ALSO has a bufferedAmount backstop, so any event not
        // pre-filtered here still cannot grow the WS buffer unbounded.
        if (FLOOD_PRONE_CDP_EVENTS.has(method)) {
            return;
        }

        // Remember which requests are XHR/fetch so `loadingFinished` (which
        // carries no resourceType) knows whether a body is worth fetching.
        if (method === 'Network.responseReceived') {
            rememberRequestType(tabId, params?.requestId, params?.type);
        } else if (method === 'Network.loadingFailed') {
            // Never completed — no body will exist; just release the entry.
            takeRequestType(tabId, params?.requestId);
        } else if (method === 'Network.loadingFinished') {
            const resourceType = takeRequestType(tabId, params?.requestId);
            if (resourceType && params?.requestId) {
                // Fetch before forwarding: this is the whole point — Chrome
                // still has the buffer right now. Only XHR/fetch reach here, so
                // navigation-critical events are never delayed by this await.
                const body = await captureResponseBodyNow(tabId, params.requestId);
                if (body !== null) {
                    // Non-standard param, read by magicutor's trace capture and
                    // ignored by CDP clients that do not know it. Deliberately
                    // NOT a synthetic CDP method: unknown *events* are forwarded
                    // to agent-browser and its handling of them is unknown,
                    // whereas an extra param on a standard event is inert.
                    sendCdpEventToBridge(tabId, method, {
                        ...params,
                        magicutorResponseBody: body
                    });
                    return;
                }
            }
        }

        // Fire-and-forget; the bridge function silently no-ops if disconnected.
        sendCdpEventToBridge(tabId, method, params);
    });
}

export { setupDebuggerListeners };
