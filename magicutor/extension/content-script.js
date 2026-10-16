/**
 * Content script for Magicutor extension dashboard/live-preview bridging.
 */

// Detect if we're in an iframe
const isTopFrame = window === window.top;
const LIVE_PREVIEW_CHANNEL = 'magicutor-live-preview';
const LIVE_PREVIEW_DASHBOARD_PORT = 'live_preview_dashboard';
const livePreviewDashboardViewers = new Map();

function getLivePreviewTargetOrigin() {
    return window.location.origin && window.location.origin !== 'null'
        ? window.location.origin
        : '*';
}

function isAllowedLivePreviewPage() {
    if (!isTopFrame) {
        return false;
    }

    const hostname = window.location.hostname;
    const isLocalMagicianUi =
        hostname === 'localhost' ||
        hostname === '127.0.0.1' ||
        hostname === '[::1]' ||
        hostname === '::1';

    if (!isLocalMagicianUi) {
        return false;
    }

    const pathname = window.location.pathname || '/';
    return (
        pathname === '/' ||
        pathname.startsWith('/home') ||
        pathname.startsWith('/tasks') ||
        pathname.startsWith('/internal-tasks') ||
        pathname.startsWith('/t/') ||
        pathname.startsWith('/debug') ||
        pathname.startsWith('/crew') ||
        pathname.startsWith('/dashboard') ||
        pathname.startsWith('/presto')
    );
}

function postLivePreviewMessage(message) {
    if (!isAllowedLivePreviewPage()) {
        return;
    }

    window.postMessage({
        channel: LIVE_PREVIEW_CHANNEL,
        sender: 'extension',
        ...message
    }, getLivePreviewTargetOrigin());
}

function disconnectLivePreviewViewer(viewerId) {
    const viewer = livePreviewDashboardViewers.get(viewerId);
    if (!viewer) {
        return;
    }

    viewer.disconnecting = true;
    livePreviewDashboardViewers.delete(viewerId);

    try {
        viewer.port.onMessage.removeListener(viewer.handleMessage);
        viewer.port.onDisconnect.removeListener(viewer.handleDisconnect);
    } catch (_) {
        // Ignore listener cleanup failures during unload/disconnect.
    }

    try {
        viewer.port.postMessage({ action: 'unsubscribe_execution' });
    } catch (_) {
        // Ignore unsubscribe failures if the background worker is already gone.
    }

    try {
        viewer.port.disconnect();
    } catch (_) {
        // Ignore disconnect failures.
    }
}

function relayLivePreviewPortMessage(viewer, message) {
    if (!viewer || !message?.type) {
        return;
    }

    if (message.type === 'ready') {
        postLivePreviewMessage({
            type: 'magicutor_live_preview_ready',
            viewerId: viewer.viewerId,
            executionId: viewer.executionId,
            extensionVersion: chrome.runtime.getManifest().version
        });
        return;
    }

    if (message.type === 'thread_sources') {
        postLivePreviewMessage({
            type: 'magicutor_live_preview_sources',
            viewerId: viewer.viewerId,
            executionId: message.executionId || viewer.executionId,
            tabs: Array.isArray(message.tabs) ? message.tabs : []
        });
        return;
    }

    if (message.type === 'thread_frame') {
        postLivePreviewMessage({
            type: 'magicutor_live_preview_frame',
            viewerId: viewer.viewerId,
            executionId: message.executionId || viewer.executionId,
            tabId: message.tabId,
            mimeType: message.mimeType || 'image/jpeg',
            data: message.data,
            receivedAt: message.receivedAt
        });
        return;
    }

    if (message.type === 'thread_tab_closed') {
        postLivePreviewMessage({
            type: 'magicutor_live_preview_tab_closed',
            viewerId: viewer.viewerId,
            executionId: message.executionId || viewer.executionId,
            tabId: message.tabId,
            reason: message.reason || 'tab_closed'
        });
        return;
    }

    if (message.type === 'thread_error') {
        postLivePreviewMessage({
            type: 'magicutor_live_preview_error',
            viewerId: viewer.viewerId,
            executionId: message.executionId || viewer.executionId,
            code: message.code || 'preview_error',
            message: message.error || 'Failed to stream live preview.'
        });
        return;
    }

    if (message.type === 'thread_capture_pending') {
        postLivePreviewMessage({
            type: 'magicutor_live_preview_capture_pending',
            viewerId: viewer.viewerId,
            executionId: message.executionId || viewer.executionId,
            tabId: message.tabId,
            message: message.message || 'Click to start live preview'
        });
    }
}

function ensureLivePreviewViewer(viewerId, executionId) {
    const existing = livePreviewDashboardViewers.get(viewerId);
    if (existing && existing.executionId === executionId) {
        return existing;
    }

    if (existing) {
        disconnectLivePreviewViewer(viewerId);
    }

    const port = chrome.runtime.connect({ name: LIVE_PREVIEW_DASHBOARD_PORT });
    const viewer = {
        viewerId,
        executionId,
        port,
        disconnecting: false,
        handleMessage: null,
        handleDisconnect: null
    };

    viewer.handleMessage = (message) => {
        relayLivePreviewPortMessage(viewer, message);
    };

    viewer.handleDisconnect = () => {
        const currentViewer = livePreviewDashboardViewers.get(viewerId);
        if (currentViewer !== viewer) {
            return;
        }

        livePreviewDashboardViewers.delete(viewerId);
        if (viewer.disconnecting) {
            return;
        }

        postLivePreviewMessage({
            type: 'magicutor_live_preview_error',
            viewerId,
            executionId,
            code: 'disconnected',
            message: 'Live preview bridge disconnected.'
        });
    };

    port.onMessage.addListener(viewer.handleMessage);
    port.onDisconnect.addListener(viewer.handleDisconnect);
    livePreviewDashboardViewers.set(viewerId, viewer);
    port.postMessage({
        action: 'subscribe_execution',
        executionId
    });

    return viewer;
}

function handleLivePreviewPageMessage(event) {
    if (!isAllowedLivePreviewPage() || event.source !== window) {
        return;
    }

    const message = event.data;
    if (
        !message
        || message.channel !== LIVE_PREVIEW_CHANNEL
        || message.sender !== 'dashboard'
        || typeof message.viewerId !== 'string'
        || message.viewerId.length === 0
    ) {
        return;
    }

    if (message.type === 'magicutor_live_preview_ping') {
        postLivePreviewMessage({
            type: 'magicutor_live_preview_ready',
            viewerId: message.viewerId,
            extensionVersion: chrome.runtime.getManifest().version
        });
        return;
    }

    if (message.type === 'magicutor_live_preview_unsubscribe') {
        disconnectLivePreviewViewer(message.viewerId);
        return;
    }

    if (typeof message.executionId !== 'string' || message.executionId.length === 0) {
        postLivePreviewMessage({
            type: 'magicutor_live_preview_error',
            viewerId: message.viewerId,
            code: 'missing_execution_id',
            message: 'Missing execution ID for live preview.'
        });
        return;
    }

    try {
        const viewer = ensureLivePreviewViewer(message.viewerId, message.executionId);
        if (!viewer) {
            return;
        }

        if (message.type === 'magicutor_live_preview_subscribe') {
            postLivePreviewMessage({
                type: 'magicutor_live_preview_ready',
                viewerId: message.viewerId,
                executionId: message.executionId,
                extensionVersion: chrome.runtime.getManifest().version
            });
            return;
        }

        if (message.type === 'magicutor_live_preview_refresh') {
            viewer.port.postMessage({
                action: 'refresh_execution',
                executionId: message.executionId
            });
            return;
        }

        if (message.type === 'magicutor_live_preview_retry_capture') {
            chrome.runtime.sendMessage({ action: 'retry_capture' });
        }
    } catch (error) {
        postLivePreviewMessage({
            type: 'magicutor_live_preview_error',
            viewerId: message.viewerId,
            executionId: message.executionId,
            code: 'bridge_failure',
            message: error?.message || 'Failed to connect live preview bridge.'
        });
    }
}

if (isTopFrame) {
    window.addEventListener('message', handleLivePreviewPageMessage);
}

console.log('[Magicutor] Content script loaded');
