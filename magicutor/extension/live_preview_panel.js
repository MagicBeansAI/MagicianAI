const LIVE_PREVIEW_PORT = 'live_preview';
const SOURCE_REFRESH_MS = 3000;

let livePreviewPort = null;
let livePreviewElements = null;
let livePreviewCleanup = [];
let livePreviewRefreshId = null;
let currentSources = [];
let selectedExecutionId = null;
let currentPreviewTabId = null;
let sourceButtonCleanup = [];

function bindLivePreviewElements() {
    livePreviewElements = {
        empty: document.getElementById('live-preview-empty'),
        sources: document.getElementById('live-preview-sources'),
        image: document.getElementById('live-preview-image'),
        viewport: document.getElementById('live-preview-viewport'),
        status: document.getElementById('live-preview-status'),
        meta: document.getElementById('live-preview-meta'),
        stop: document.getElementById('live-preview-stop'),
        refresh: document.getElementById('live-preview-refresh')
    };
}

function addCleanup(callback) {
    livePreviewCleanup.push(callback);
}

function destroyLivePreviewPanel() {
    while (sourceButtonCleanup.length > 0) {
        const cleanup = sourceButtonCleanup.pop();
        try {
            cleanup();
        } catch (_) {
            // Ignore cleanup failures during side panel unload.
        }
    }

    while (livePreviewCleanup.length > 0) {
        const callback = livePreviewCleanup.pop();
        try {
            callback();
        } catch (_) {
            // Ignore cleanup failures during side panel unload.
        }
    }

    if (livePreviewRefreshId) {
        clearInterval(livePreviewRefreshId);
        livePreviewRefreshId = null;
    }

    if (livePreviewPort) {
        try {
            livePreviewPort.postMessage({ action: 'stop_preview' });
            livePreviewPort.disconnect();
        } catch (_) {
            // Ignore disconnect errors.
        }
        livePreviewPort = null;
    }
}

function setLivePreviewStatus(text, tone = '') {
    if (!livePreviewElements?.status) {
        return;
    }

    livePreviewElements.status.textContent = text;
    livePreviewElements.status.className = 'live-preview-status';
    if (tone) {
        livePreviewElements.status.classList.add(tone);
    }
}

function setLivePreviewMeta(source) {
    if (!livePreviewElements?.meta) {
        return;
    }

    if (!source) {
        livePreviewElements.meta.textContent = '';
        return;
    }

    const location = source.url || 'Unavailable';
    livePreviewElements.meta.textContent = `${source.title} · ${location}`;
}

function updateLivePreviewEmptyState() {
    const hasSources = currentSources.length > 0;
    if (livePreviewElements?.empty) {
        livePreviewElements.empty.style.display = hasSources ? 'none' : 'block';
    }
    if (livePreviewElements?.viewport) {
        livePreviewElements.viewport.style.display = hasSources ? 'block' : 'none';
    }
}

function renderLivePreviewSources() {
    if (!livePreviewElements?.sources) {
        return;
    }

    while (sourceButtonCleanup.length > 0) {
        const cleanup = sourceButtonCleanup.pop();
        try {
            cleanup();
        } catch (_) {
            // Ignore stale button cleanup failures.
        }
    }

    updateLivePreviewEmptyState();

    if (currentSources.length === 0) {
        livePreviewElements.sources.innerHTML = '';
        selectedExecutionId = null;
        currentPreviewTabId = null;
        if (livePreviewElements.image) {
            livePreviewElements.image.removeAttribute('src');
        }
        setLivePreviewStatus('No active automation sessions.', '');
        setLivePreviewMeta(null);
        return;
    }

    if (!selectedExecutionId || !currentSources.some((source) => source.executionId === selectedExecutionId)) {
        selectedExecutionId = currentSources[0].executionId;
    }

    livePreviewElements.sources.innerHTML = currentSources.map((source) => {
        const selected = source.executionId === selectedExecutionId ? ' selected' : '';
        const safeTitle = escapeHtml(source.title || `Execution ${source.executionId.slice(0, 8)}`);
        const safeUrl = escapeHtml(source.url || 'Unavailable');
        const safeThreadId = escapeHtml(source.executionId);
        return `
            <button class="live-source-card${selected}" data-execution-id="${safeThreadId}" type="button">
                <div class="live-source-title">${safeTitle}</div>
                <div class="live-source-url">${safeUrl}</div>
                <div class="live-source-meta">Execution ${escapeHtml(source.executionId.slice(0, 8))} · ${source.sessionTabCount} tab${source.sessionTabCount === 1 ? '' : 's'}</div>
            </button>
        `;
    }).join('');

    for (const button of livePreviewElements.sources.querySelectorAll('[data-execution-id]')) {
        const handleClick = () => {
            selectedExecutionId = button.dataset.executionId;
            renderLivePreviewSources();
            requestPreviewStart();
        };
        button.addEventListener('click', handleClick);
        sourceButtonCleanup.push(() => button.removeEventListener('click', handleClick));
    }
}

function escapeHtml(str) {
    return String(str || '')
        .replace(/&/g, '&amp;')
        .replace(/</g, '&lt;')
        .replace(/>/g, '&gt;')
        .replace(/"/g, '&quot;');
}

function showCapturePermissionButton() {
    setLivePreviewStatus('Live preview needs a user gesture to start.', 'warn');

    if (!livePreviewElements?.viewport) {
        return;
    }

    // Remove existing button if any
    hideCapturePermissionButton();

    const button = document.createElement('button');
    button.id = 'live-preview-capture-start';
    button.textContent = 'Click to start live preview';
    button.className = 'live-preview-action';
    button.style.cssText = 'display:block;margin:16px auto;padding:10px 20px;font-size:13px;cursor:pointer;';

    const handleClick = () => {
        chrome.runtime.sendMessage({ action: 'retry_capture' });
        button.textContent = 'Starting...';
        button.disabled = true;
        setTimeout(() => {
            hideCapturePermissionButton();
        }, 2000);
    };
    button.addEventListener('click', handleClick);
    addCleanup(() => {
        button.removeEventListener('click', handleClick);
    });

    livePreviewElements.viewport.parentNode.insertBefore(button, livePreviewElements.viewport);
}

function hideCapturePermissionButton() {
    const existing = document.getElementById('live-preview-capture-start');
    if (existing) {
        existing.remove();
    }
}

async function refreshLivePreviewSources() {
    try {
        const response = await chrome.runtime.sendMessage({ action: 'get_live_preview_sources' });
        currentSources = response?.sources || [];
        renderLivePreviewSources();

        const selectedSource = currentSources.find((source) => source.executionId === selectedExecutionId);
        if (!selectedSource) {
            if (livePreviewPort) {
                livePreviewPort.postMessage({ action: 'stop_preview' });
            }
            return;
        }

        if (currentPreviewTabId !== selectedSource.tabId) {
            requestPreviewStart();
        } else {
            setLivePreviewMeta(selectedSource);
        }
    } catch (error) {
        setLivePreviewStatus(error.message || 'Failed to refresh preview sources.', 'err');
    }
}

function requestPreviewStart() {
    if (!livePreviewPort || !selectedExecutionId) {
        return;
    }

    const source = currentSources.find((item) => item.executionId === selectedExecutionId);
    if (!source) {
        return;
    }

    setLivePreviewStatus('Starting preview...', 'warn');
    setLivePreviewMeta(source);
    livePreviewPort.postMessage({
        action: 'start_preview',
        executionId: source.executionId
    });
}

function handleLivePreviewPortMessage(message) {
    if (!message?.type) {
        return;
    }

    if (message.type === 'ready') {
        void refreshLivePreviewSources();
        return;
    }

    if (message.type === 'preview_started') {
        currentPreviewTabId = message.source?.tabId || null;
        hideCapturePermissionButton();
        setLivePreviewStatus('Streaming live preview.', 'ok');
        setLivePreviewMeta(message.source || null);
        return;
    }

    if (message.type === 'frame') {
        hideCapturePermissionButton();
        if (livePreviewElements?.image && message.data) {
            livePreviewElements.image.src = `data:${message.mimeType || 'image/jpeg'};base64,${message.data}`;
        }
        setLivePreviewStatus('Streaming live preview.', 'ok');
        return;
    }

    if (message.type === 'preview_error') {
        currentPreviewTabId = null;
        if (livePreviewElements?.image) {
            livePreviewElements.image.removeAttribute('src');
        }
        setLivePreviewStatus(message.error || 'Failed to start live preview.', 'err');
        return;
    }

    if (message.type === 'preview_stopped') {
        currentPreviewTabId = null;
        if (livePreviewElements?.image) {
            livePreviewElements.image.removeAttribute('src');
        }
        setLivePreviewStatus('Preview stopped.', 'warn');
        return;
    }

    if (message.type === 'capture_pending') {
        showCapturePermissionButton();
        return;
    }
}

function connectLivePreviewPort() {
    livePreviewPort = chrome.runtime.connect({ name: LIVE_PREVIEW_PORT });

    const handleMessage = (message) => {
        handleLivePreviewPortMessage(message);
    };
    const handleDisconnect = () => {
        livePreviewPort = null;
        currentPreviewTabId = null;
        setLivePreviewStatus('Live preview disconnected.', 'err');
    };

    livePreviewPort.onMessage.addListener(handleMessage);
    livePreviewPort.onDisconnect.addListener(handleDisconnect);

    addCleanup(() => {
        if (!livePreviewPort) {
            return;
        }
        livePreviewPort.onMessage.removeListener(handleMessage);
        livePreviewPort.onDisconnect.removeListener(handleDisconnect);
    });
}

function setupLivePreviewUi() {
    if (livePreviewElements?.stop) {
        const handleStop = () => {
            if (livePreviewPort) {
                livePreviewPort.postMessage({ action: 'stop_preview' });
            }
            currentPreviewTabId = null;
            if (livePreviewElements.image) {
                livePreviewElements.image.removeAttribute('src');
            }
            setLivePreviewStatus('Preview stopped.', 'warn');
        };
        livePreviewElements.stop.addEventListener('click', handleStop);
        addCleanup(() => livePreviewElements.stop.removeEventListener('click', handleStop));
    }

    if (livePreviewElements?.refresh) {
        const handleRefresh = () => {
            void refreshLivePreviewSources();
        };
        livePreviewElements.refresh.addEventListener('click', handleRefresh);
        addCleanup(() => livePreviewElements.refresh.removeEventListener('click', handleRefresh));
    }
}

export function initLivePreviewPanel() {
    bindLivePreviewElements();
    setupLivePreviewUi();
    connectLivePreviewPort();

    livePreviewRefreshId = setInterval(() => {
        void refreshLivePreviewSources();
    }, SOURCE_REFRESH_MS);

    const handleUnload = () => {
        destroyLivePreviewPanel();
    };
    window.addEventListener('pagehide', handleUnload, { once: true });
    window.addEventListener('beforeunload', handleUnload, { once: true });
    addCleanup(() => window.removeEventListener('pagehide', handleUnload, { once: true }));
    addCleanup(() => window.removeEventListener('beforeunload', handleUnload, { once: true }));

    setLivePreviewStatus('Loading live preview sources...', 'warn');
    updateLivePreviewEmptyState();
}
