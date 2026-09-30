import {
    debuggerSessions,
    getActiveExecutions,
    getSessionForTab,
    getSessionPrimaryTab,
    getSessionTabs
} from './background_state.js';
import {
    browserThreadIdForExecution,
    executionIdFromSessionId
} from './automation_session.js';
import { attachDebugger } from './debugger_actions.js';

const LIVE_PREVIEW_PORT = 'live_preview';
const LIVE_PREVIEW_DASHBOARD_PORT = 'live_preview_dashboard';
const CDP_PREVIEW_FPS = 4;
const CDP_PREVIEW_QUALITY = 55;
const livePreviewPorts = new Map();
const livePreviewViewersByTab = new Map();
const livePreviewActiveTabs = new Set();
const livePreviewLastFrameByTab = new Map();
let nextLivePreviewPortId = 1;

// tabCapture + offscreen document state
const pendingCaptureTabIds = new Set();
const livePreviewCaptureModeByTab = new Map();
const livePreviewCaptureIntervalByTab = new Map();

function postToPort(portId, message) {
    const portInfo = livePreviewPorts.get(portId);
    if (!portInfo) {
        return;
    }

    try {
        portInfo.port.postMessage(message);
    } catch (error) {
        console.warn('[LivePreview] Failed to post message to port:', error.message);
    }
}

async function safeGetTab(tabId) {
    if (!tabId) {
        return null;
    }

    try {
        return await chrome.tabs.get(tabId);
    } catch (_) {
        return null;
    }
}

function getSessionAliasForExecution(executionId) {
    return browserThreadIdForExecution(executionId);
}

function getExecutionIdForTab(tabId) {
    const sessionId = getSessionForTab(tabId);
    return executionIdFromSessionId(sessionId);
}

function comparePreviewTabs(left, right, { trackedTabId, trackedWindowId, primaryTabId } = {}) {
    const leftActiveInTrackedWindow = left.active && (trackedWindowId == null || left.windowId === trackedWindowId);
    const rightActiveInTrackedWindow = right.active && (trackedWindowId == null || right.windowId === trackedWindowId);
    if (leftActiveInTrackedWindow !== rightActiveInTrackedWindow) {
        return leftActiveInTrackedWindow ? -1 : 1;
    }

    const leftTracked = trackedTabId != null && left.id === trackedTabId;
    const rightTracked = trackedTabId != null && right.id === trackedTabId;
    if (leftTracked !== rightTracked) {
        return leftTracked ? -1 : 1;
    }

    if (!!left.active !== !!right.active) {
        return left.active ? -1 : 1;
    }

    const leftPrimary = primaryTabId != null && left.id === primaryTabId;
    const rightPrimary = primaryTabId != null && right.id === primaryTabId;
    if (leftPrimary !== rightPrimary) {
        return leftPrimary ? -1 : 1;
    }

    return left.id - right.id;
}

function buildPreviewSource(executionId, sessionId, tab, sessionTabCount) {
    return {
        executionId,
        sessionId,
        tabId: tab.id,
        windowId: tab.windowId,
        title: tab.title || tab.pendingUrl || tab.url || `Tab ${tab.id}`,
        url: tab.url || tab.pendingUrl || '',
        favIconUrl: tab.favIconUrl || '',
        status: tab.status || 'unknown',
        active: !!tab.active,
        sessionTabCount
    };
}

async function buildLivePreviewSourcesForExecution(executionId, trackedInfo) {
    const sessionAlias = getSessionAliasForExecution(executionId);
    const primaryTabId = getSessionPrimaryTab(sessionAlias);
    const sessionTabIds = new Set(getSessionTabs(sessionAlias));
    if (trackedInfo?.tabId) {
        sessionTabIds.add(trackedInfo.tabId);
    }

    const tabCandidates = [];
    for (const tabId of sessionTabIds) {
        const tab = await safeGetTab(tabId);
        if (tab) {
            tabCandidates.push(tab);
        }
    }

    tabCandidates.sort((left, right) => comparePreviewTabs(left, right, {
        trackedTabId: trackedInfo?.tabId || null,
        trackedWindowId: trackedInfo?.windowId || null,
        primaryTabId
    }));

    const sessionTabCount = tabCandidates.length;
    return tabCandidates.map((tab) => buildPreviewSource(executionId, sessionAlias, tab, sessionTabCount));
}

function pickPreviewSource(sources, { trackedTabId, trackedWindowId, primaryTabId } = {}) {
    if (!sources || sources.length === 0) {
        return null;
    }

    const activeInTrackedWindow = sources.find((source) =>
        source.active && (trackedWindowId == null || source.windowId === trackedWindowId)
    );
    if (activeInTrackedWindow) {
        return activeInTrackedWindow;
    }

    const trackedTab = sources.find((source) => source.tabId === trackedTabId);
    if (trackedTab) {
        return trackedTab;
    }

    const primaryTab = sources.find((source) => source.tabId === primaryTabId);
    if (primaryTab) {
        return primaryTab;
    }

    return sources[0];
}

async function buildLivePreviewSource(executionId, trackedInfo) {
    const sessionAlias = getSessionAliasForExecution(executionId);
    const sources = await buildLivePreviewSourcesForExecution(executionId, trackedInfo);
    return pickPreviewSource(sources, {
        trackedTabId: trackedInfo?.tabId || null,
        trackedWindowId: trackedInfo?.windowId || null,
        primaryTabId: getSessionPrimaryTab(sessionAlias)
    });
}

async function resolveLivePreviewSource({ executionId, tabId } = {}) {
    if (tabId) {
        const tab = await safeGetTab(tabId);
        if (!tab) {
            return null;
        }
        const sessionId = getSessionForTab(tabId);
        const resolvedExecutionId = executionId || getExecutionIdForTab(tabId);
        return buildPreviewSource(resolvedExecutionId, sessionId, tab, sessionId ? getSessionTabs(sessionId).size : 1);
    }

    if (!executionId) {
        return null;
    }

    const trackedInfo = getActiveExecutions().get(executionId);
    return buildLivePreviewSource(executionId, trackedInfo);
}

function getTrackedTabIds(portInfo) {
    const tabIds = new Set();
    if (!portInfo) {
        return tabIds;
    }

    if (portInfo.previewTabId) {
        tabIds.add(portInfo.previewTabId);
    }

    if (portInfo.previewTabIds) {
        for (const tabId of portInfo.previewTabIds) {
            tabIds.add(tabId);
        }
    }

    return tabIds;
}

// --- tabCapture + offscreen document management ---

async function ensureOffscreenDocument() {
    // hasDocument() is the authoritative check (Chrome 116+),
    // survives service-worker restarts unlike a module-level boolean.
    const exists = await chrome.offscreen.hasDocument();
    if (exists) return;

    await chrome.offscreen.createDocument({
        url: 'offscreen.html',
        reasons: ['USER_MEDIA'],
        justification: 'Tab capture for live preview frame extraction'
    });
}

async function closeOffscreenDocument() {
    const exists = await chrome.offscreen.hasDocument();
    if (!exists) return;
    await chrome.offscreen.closeDocument();
}

// Tracks tabs with in-flight startTabCapture calls to prevent TOCTOU double-capture.
const pendingStartTabIds = new Set();

async function startTabCapture(tabId) {
    if (livePreviewActiveTabs.has(tabId) || pendingStartTabIds.has(tabId)) return;
    pendingStartTabIds.add(tabId);

    try {
        // Automation tabs are already debugger-backed through the CDP proxy.
        // For those tabs, CDP screenshots are more reliable than tabCapture:
        // no user gesture is needed, inactive/background tabs still render,
        // and we avoid black tabCapture streams.
        if (debuggerSessions.has(tabId)) {
            await startCdpScreenshotCapture(tabId);
            return;
        }

        const streamId = await chrome.tabCapture.getMediaStreamId({ targetTabId: tabId });
        await ensureOffscreenDocument();

        await chrome.runtime.sendMessage({
            target: 'offscreen',
            action: 'startCapture',
            streamId,
            tabId,
            fps: 15,
            quality: 0.50,
            width: 960,
            height: 540
        });

        livePreviewCaptureModeByTab.set(tabId, 'tab_capture');
        livePreviewActiveTabs.add(tabId);
        pendingCaptureTabIds.delete(tabId);

    } catch (error) {
        const fallbackStarted = await startCdpScreenshotCapture(tabId).catch((fallbackError) => {
            console.warn(
                '[LivePreview] CDP screenshot fallback failed after tabCapture error:',
                fallbackError.message
            );
            return false;
        });

        if (fallbackStarted) {
            return;
        }

        console.warn('[LivePreview] tabCapture failed (may need user gesture):', error.message);
        pendingCaptureTabIds.add(tabId);

        // Notify all viewers about pending state
        for (const [portId, portInfo] of livePreviewPorts) {
            try {
                if (portInfo.mode === 'dashboard') {
                    portInfo.port.postMessage({
                        type: 'thread_capture_pending',
                        tabId,
                        executionId: portInfo.previewExecutionId || null,
                        message: 'Click to start live preview'
                    });
                } else {
                    portInfo.port.postMessage({
                        type: 'capture_pending',
                        tabId,
                        message: 'Click to start live preview'
                    });
                }
            } catch (e) {}
        }
    } finally {
        pendingStartTabIds.delete(tabId);
    }
}

async function startCdpScreenshotCapture(tabId) {
    if (livePreviewActiveTabs.has(tabId)) return true;

    if (!debuggerSessions.has(tabId)) {
        const sessionId = getSessionForTab(tabId);
        await attachDebugger({ tabId, sessionId: sessionId || undefined });
    }

    livePreviewCaptureModeByTab.set(tabId, 'cdp_screenshot');
    livePreviewActiveTabs.add(tabId);
    pendingCaptureTabIds.delete(tabId);

    const captureFrame = async () => {
        if (!livePreviewActiveTabs.has(tabId) || livePreviewCaptureModeByTab.get(tabId) !== 'cdp_screenshot') {
            return false;
        }

        try {
            const response = await chrome.debugger.sendCommand(
                { tabId },
                'Page.captureScreenshot',
                {
                    format: 'jpeg',
                    quality: CDP_PREVIEW_QUALITY,
                    captureBeyondViewport: false
                }
            );

            if (!response?.data || typeof response.data !== 'string') {
                throw new Error('Page.captureScreenshot returned no image data');
            }

            handlePreviewFrame({
                tabId,
                data: response.data,
                mimeType: 'image/jpeg',
                timestamp: Date.now(),
                metadata: { captureMode: 'cdp_screenshot' }
            });
            return true;
        } catch (error) {
            handleCaptureError(tabId, error?.message || String(error));
            return false;
        }
    };

    const firstFrameOk = await captureFrame();
    if (
        !firstFrameOk ||
        !livePreviewActiveTabs.has(tabId) ||
        livePreviewCaptureModeByTab.get(tabId) !== 'cdp_screenshot'
    ) {
        return false;
    }

    const interval = setInterval(captureFrame, Math.floor(1000 / CDP_PREVIEW_FPS));
    livePreviewCaptureIntervalByTab.set(tabId, interval);
    return true;
}

async function stopTabCapture(tabId) {
    if (!livePreviewActiveTabs.has(tabId)) return;

    const interval = livePreviewCaptureIntervalByTab.get(tabId);
    if (interval) {
        clearInterval(interval);
        livePreviewCaptureIntervalByTab.delete(tabId);
    }

    const mode = livePreviewCaptureModeByTab.get(tabId);
    if (mode === 'tab_capture') {
        try {
            await chrome.runtime.sendMessage({
                target: 'offscreen',
                action: 'stopCapture',
                tabId
            });
        } catch (e) {
            console.warn('[LivePreview] Failed to stop capture for tab', tabId, e.message);
        }
    }

    livePreviewCaptureModeByTab.delete(tabId);
    livePreviewActiveTabs.delete(tabId);
    livePreviewLastFrameByTab.delete(tabId);

    // Close offscreen doc if no more captures
    if (![...livePreviewCaptureModeByTab.values()].some((value) => value === 'tab_capture')) {
        await closeOffscreenDocument();
    }
}

// --- Frame handling from offscreen document or CDP screenshot polling ---

function handlePreviewFrame(message) {
    const { tabId, data, mimeType, timestamp, metadata } = message;
    if (!tabId || !data || typeof data !== 'string') {
        console.warn('[LivePreview] Dropping invalid frame payload for tab', tabId);
        return;
    }

    // Cache last frame
    livePreviewLastFrameByTab.set(tabId, {
        data,
        mimeType: mimeType || 'image/jpeg',
        metadata: metadata || {},
        receivedAt: timestamp || Date.now()
    });

    // Distribute to viewers
    const viewers = livePreviewViewersByTab.get(tabId);
    if (!viewers || viewers.size === 0) return;

    for (const portId of viewers) {
        const portInfo = livePreviewPorts.get(portId);
        if (!portInfo) continue;

        try {
            if (portInfo.mode === 'dashboard') {
                portInfo.port.postMessage({
                    type: 'thread_frame',
                    executionId: portInfo.previewExecutionId || getExecutionIdForTab(tabId),
                    tabId,
                    data,
                    mimeType: mimeType || 'image/jpeg',
                    metadata: metadata || {},
                    receivedAt: timestamp || Date.now()
                });
            } else {
                portInfo.port.postMessage({
                    type: 'frame',
                    tabId,
                    data,
                    mimeType: mimeType || 'image/jpeg',
                    metadata: metadata || {},
                    receivedAt: timestamp || Date.now()
                });
            }
        } catch (e) {
            // Port disconnected
        }
    }
}

async function handleCaptureEnded(tabId) {
    const interval = livePreviewCaptureIntervalByTab.get(tabId);
    if (interval) {
        clearInterval(interval);
        livePreviewCaptureIntervalByTab.delete(tabId);
    }
    const mode = livePreviewCaptureModeByTab.get(tabId);
    livePreviewCaptureModeByTab.delete(tabId);

    // Tell offscreen to stop this capture's interval before cleanup
    if (mode === 'tab_capture') {
        try {
            await chrome.runtime.sendMessage({
                target: 'offscreen',
                action: 'stopCapture',
                tabId
            });
        } catch (e) {
            // Offscreen may already be gone
        }
    }

    const viewers = livePreviewViewersByTab.get(tabId);
    if (!viewers || viewers.size === 0) {
        livePreviewActiveTabs.delete(tabId);
        livePreviewLastFrameByTab.delete(tabId);

        // Close offscreen doc if no more captures
        if (![...livePreviewCaptureModeByTab.values()].some((value) => value === 'tab_capture')) {
            await closeOffscreenDocument();
        }
        return;
    }

    const portIds = [...viewers];
    livePreviewViewersByTab.delete(tabId);
    livePreviewActiveTabs.delete(tabId);
    livePreviewLastFrameByTab.delete(tabId);

    for (const portId of portIds) {
        const portInfo = livePreviewPorts.get(portId);
        if (!portInfo) continue;

        if (portInfo.previewTabId === tabId) {
            portInfo.previewTabId = null;
        }
        portInfo.previewTabIds.delete(tabId);

        if (portInfo.mode === 'dashboard') {
            postToPort(portId, {
                type: 'thread_tab_closed',
                executionId: portInfo.previewExecutionId || getExecutionIdForTab(tabId),
                tabId,
                reason: 'capture_ended'
            });
            continue;
        }

        portInfo.previewExecutionId = null;
        postToPort(portId, {
            type: 'preview_stopped',
            tabId,
            reason: 'capture_ended'
        });
    }

    // Close offscreen doc if no more captures
    if (![...livePreviewCaptureModeByTab.values()].some((value) => value === 'tab_capture')) {
        await closeOffscreenDocument();
    }
}

function handleCaptureError(tabId, error) {
    console.warn(`[LivePreview] Capture error for tab ${tabId}:`, error);
    handleCaptureEnded(tabId);
}

// --- Port-to-tab attachment ---

async function attachPortToTab(portId, tabId) {
    if (!livePreviewViewersByTab.has(tabId)) {
        livePreviewViewersByTab.set(tabId, new Set());
    }
    livePreviewViewersByTab.get(tabId).add(portId);
    await startTabCapture(tabId);
}

async function detachPortFromTab(portId, tabId) {
    const viewers = livePreviewViewersByTab.get(tabId);
    if (!viewers) {
        return;
    }

    viewers.delete(portId);
    if (viewers.size === 0) {
        livePreviewViewersByTab.delete(tabId);
        await stopTabCapture(tabId);
    }
}

async function detachPortFromPreview(portId) {
    const portInfo = livePreviewPorts.get(portId);
    if (!portInfo) {
        return;
    }

    const trackedTabIds = [...getTrackedTabIds(portInfo)];
    for (const tabId of trackedTabIds) {
        await detachPortFromTab(portId, tabId);
    }

    portInfo.previewTabId = null;
    portInfo.previewExecutionId = null;
    portInfo.previewTabIds.clear();
}

async function startPreviewForPort(portId, request = {}) {
    const portInfo = livePreviewPorts.get(portId);
    if (!portInfo) {
        return;
    }

    const source = await resolveLivePreviewSource(request);
    if (!source) {
        await detachPortFromPreview(portId);
        postToPort(portId, {
            type: 'preview_error',
            error: 'No previewable automation tab found.'
        });
        return;
    }

    await detachPortFromPreview(portId);

    try {
        await attachPortToTab(portId, source.tabId);
        portInfo.previewTabId = source.tabId;
        portInfo.previewExecutionId = source.executionId || null;
        portInfo.previewTabIds = new Set([source.tabId]);

        postToPort(portId, {
            type: 'preview_started',
            source
        });

        const lastFrame = livePreviewLastFrameByTab.get(source.tabId);
        if (lastFrame) {
            postToPort(portId, {
                type: 'frame',
                tabId: source.tabId,
                data: lastFrame.data,
                metadata: lastFrame.metadata,
                receivedAt: lastFrame.receivedAt,
                mimeType: lastFrame.mimeType
            });
        }
    } catch (error) {
        await detachPortFromPreview(portId);
        postToPort(portId, {
            type: 'preview_error',
            error: error.message || 'Failed to start live preview.'
        });
    }
}

async function syncExecutionPreviewForPort(portId, request = {}) {
    const portInfo = livePreviewPorts.get(portId);
    if (!portInfo) {
        return;
    }

    const executionId = request.executionId || portInfo.previewExecutionId;
    if (!executionId) {
        postToPort(portId, {
            type: 'thread_error',
            code: 'missing_execution_id',
            error: 'Missing execution ID for live preview.'
        });
        return;
    }

    const trackedInfo = getActiveExecutions().get(executionId);
    const sources = await buildLivePreviewSourcesForExecution(executionId, trackedInfo);
    const currentTabIds = getTrackedTabIds(portInfo);
    const nextTabIds = new Set(sources.map((source) => source.tabId));
    const attachedTabIds = new Set();

    for (const tabId of currentTabIds) {
        if (!nextTabIds.has(tabId)) {
            await detachPortFromTab(portId, tabId);
            postToPort(portId, {
                type: 'thread_tab_closed',
                executionId,
                tabId,
                reason: 'tab_no_longer_tracked'
            });
            continue;
        }

        attachedTabIds.add(tabId);
    }

    for (const source of sources) {
        if (attachedTabIds.has(source.tabId)) {
            continue;
        }

        try {
            await attachPortToTab(portId, source.tabId);
            attachedTabIds.add(source.tabId);
        } catch (error) {
            postToPort(portId, {
                type: 'thread_error',
                executionId,
                code: 'attach_failed',
                error: error.message || `Failed to attach preview to tab ${source.tabId}.`
            });
        }
    }

    portInfo.previewTabId = null;
    portInfo.previewExecutionId = executionId;
    portInfo.previewTabIds = attachedTabIds;

    postToPort(portId, {
        type: 'thread_sources',
        executionId,
        tabs: sources
    });

    for (const source of sources) {
        const lastFrame = livePreviewLastFrameByTab.get(source.tabId);
        if (!lastFrame) {
            continue;
        }

        postToPort(portId, {
            type: 'thread_frame',
            executionId,
            tabId: source.tabId,
            data: lastFrame.data,
            metadata: lastFrame.metadata,
            receivedAt: lastFrame.receivedAt,
            mimeType: lastFrame.mimeType
        });
    }
}

function handlePortMessage(portId, message) {
    if (!message?.action) {
        return;
    }

    const portInfo = livePreviewPorts.get(portId);
    if (!portInfo) {
        return;
    }

    if (portInfo.mode === 'dashboard') {
        if (message.action === 'subscribe_execution' || message.action === 'refresh_execution') {
            void syncExecutionPreviewForPort(portId, message);
            return;
        }

        if (message.action === 'unsubscribe_execution' || message.action === 'stop_preview') {
            void detachPortFromPreview(portId);
        }
        return;
    }

    if (message.action === 'start_preview') {
        void startPreviewForPort(portId, message);
        return;
    }

    if (message.action === 'stop_preview') {
        void detachPortFromPreview(portId);
    }
}

function handlePortDisconnect(portId) {
    void detachPortFromPreview(portId);
    livePreviewPorts.delete(portId);
}

function setupLivePreviewMessaging() {
    chrome.runtime.onConnect.addListener((port) => {
        if (port.name !== LIVE_PREVIEW_PORT && port.name !== LIVE_PREVIEW_DASHBOARD_PORT) {
            return;
        }

        const portId = nextLivePreviewPortId++;
        livePreviewPorts.set(portId, {
            port,
            mode: port.name === LIVE_PREVIEW_DASHBOARD_PORT ? 'dashboard' : 'single',
            previewTabId: null,
            previewExecutionId: null,
            previewTabIds: new Set()
        });

        port.onMessage.addListener((message) => {
            handlePortMessage(portId, message);
        });

        port.onDisconnect.addListener(() => {
            handlePortDisconnect(portId);
        });

        port.postMessage({ type: 'ready' });
    });
}

async function getLivePreviewSources() {
    const sources = [];
    for (const [executionId, trackedInfo] of getActiveExecutions()) {
        const source = await buildLivePreviewSource(executionId, trackedInfo);
        if (source) {
            sources.push(source);
        }
    }

    sources.sort((left, right) => {
        if (left.active !== right.active) {
            return left.active ? -1 : 1;
        }
        return left.executionId.localeCompare(right.executionId);
    });

    return sources;
}

/**
 * Handle runtime messages from the offscreen document and retry_capture requests.
 * This should be called from the main background onMessage listener.
 * Returns true if the message was handled, false otherwise.
 */
function handleLivePreviewMessage(message, sender, sendResponse) {
    // Handle messages from offscreen document
    if (message.target === 'background' && message.type === 'offscreen_frame') {
        handlePreviewFrame(message);
        return true;
    }
    if (message.target === 'background' && message.type === 'offscreen_capture_ended') {
        handleCaptureEnded(message.tabId);
        return true;
    }
    if (message.target === 'background' && message.type === 'offscreen_capture_error') {
        handleCaptureError(message.tabId, message.error);
        return true;
    }

    // Handle retry_capture from sidepanel or dashboard (user gesture context)
    if (message.action === 'retry_capture' || message.action === 'retry_live_preview_capture') {
        const tabIds = [...pendingCaptureTabIds];
        pendingCaptureTabIds.clear();
        for (const tabId of tabIds) {
            void startTabCapture(tabId);
        }
        if (sendResponse) {
            sendResponse({ success: true });
        }
        return true;
    }

    return false;
}

/**
 * Handle debugger detach events that affect live preview tabs.
 * Even though we no longer use the debugger for preview capture,
 * a tab being closed or debugger being detached may indicate
 * the tab is gone, so we clean up capture state.
 */
function handleLivePreviewDebuggerDetach(tabId, reason) {
    if (!livePreviewActiveTabs.has(tabId) && !livePreviewViewersByTab.has(tabId)) {
        return;
    }

    // The tab's debugger was detached -- stop capture and notify viewers
    handleCaptureEnded(tabId);
}

export {
    getLivePreviewSources,
    handleLivePreviewDebuggerDetach,
    handleLivePreviewMessage,
    setupLivePreviewMessaging
};
