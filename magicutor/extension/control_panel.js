/**
 * Shared control panel script for Magicutor extension surfaces.
 *
 * State management: All state is in-memory (background_state.js), no local storage.
 * - Goal input and checkbox preferences are stored per-window in background state
 * - Execution tracking remains session-based in background state
 * - State is lost on service worker restart, which is acceptable
 */

import { MAGICIAN_API_BASE } from './config.js';
import { executionIdFromSessionId } from './automation_session.js';
import { magicianFetch } from './magician_scope.js';
import {
    isTerminalExecutionStatus,
    normalizeExecutionStatus
} from './execution_status.js';

let automationInput = null;
let automationRun = null;
let automationResume = null;
let automationStop = null;
let maxIterationsInput = null;
let automationStatus = null;
let automationIncludeUrl = null;
let automationUseTab = null;
let automationSkipPlanning = null;
let automationTabUrl = null;
let automationCleanup = null;
let stopOptions = null;
let settingVisualOverlay = null;
let settingDebugOverlay = null;
let settingPointerDiagnostics = null;

let activeTab = null;
let surfaceWindowId = null;
let automationServerReady = false;
let automationTabReady = false;
let goalSaveTimeoutId = null;
let initialized = false;
let surfaceName = 'popup';

const intervalIds = [];
const cleanupCallbacks = [];

function logPrefix() {
    return `[ControlPanel:${surfaceName}]`;
}

function bindElements() {
    automationInput = document.getElementById('automation-input');
    automationRun = document.getElementById('automation-run');
    automationResume = document.getElementById('automation-resume');
    automationStop = document.getElementById('automation-stop');
    maxIterationsInput = document.getElementById('max-iterations');
    automationStatus = document.getElementById('automation-status');
    automationIncludeUrl = document.getElementById('automation-include-url');
    automationUseTab = document.getElementById('automation-use-tab');
    automationSkipPlanning = document.getElementById('automation-skip-planning');
    automationTabUrl = document.getElementById('automation-tab-url');
    automationCleanup = document.getElementById('automation-cleanup');
    stopOptions = document.getElementById('stop-options');
    settingVisualOverlay = document.getElementById('setting-visual-overlay');
    settingDebugOverlay = document.getElementById('setting-debug-overlay');
    settingPointerDiagnostics = document.getElementById('setting-pointer-diagnostics');

    const extensionId = document.getElementById('extension-id');
    if (extensionId) {
        extensionId.textContent = chrome.runtime.id;
    }

    const version = document.getElementById('version');
    if (version) {
        version.textContent = chrome.runtime.getManifest().version;
    }
}

function registerCleanup(callback) {
    cleanupCallbacks.push(callback);
}

function registerInterval(callback, intervalMs) {
    const intervalId = setInterval(callback, intervalMs);
    intervalIds.push(intervalId);
    return intervalId;
}

function addDomListener(target, eventName, listener, options) {
    if (!target) return;

    target.addEventListener(eventName, listener, options);
    registerCleanup(() => {
        target.removeEventListener(eventName, listener, options);
    });
}

function addChromeListener(eventObject, listener) {
    if (!eventObject?.addListener) return;

    eventObject.addListener(listener);
    registerCleanup(() => {
        try {
            eventObject.removeListener(listener);
        } catch (_) {
            // Ignore listener teardown errors during page disposal.
        }
    });
}

function clearIntervalsAndListeners() {
    while (intervalIds.length > 0) {
        clearInterval(intervalIds.pop());
    }

    while (cleanupCallbacks.length > 0) {
        const callback = cleanupCallbacks.pop();
        try {
            callback();
        } catch (_) {
            // Ignore cleanup failures from page teardown.
        }
    }

    if (goalSaveTimeoutId) {
        clearTimeout(goalSaveTimeoutId);
        goalSaveTimeoutId = null;
    }
}

export function destroyControlPanel() {
    if (!initialized) return;

    initialized = false;
    clearIntervalsAndListeners();
    activeTab = null;
    surfaceWindowId = null;
    automationServerReady = false;
    automationTabReady = false;
}

function getStateWindowId() {
    return surfaceWindowId ?? activeTab?.windowId ?? null;
}

async function resolveSurfaceWindowId() {
    if (surfaceName !== 'sidepanel') {
        surfaceWindowId = null;
        return surfaceWindowId;
    }

    try {
        const currentWindow = await chrome.windows.getCurrent();
        surfaceWindowId = currentWindow?.id ?? null;
    } catch (error) {
        surfaceWindowId = null;
        console.debug(`${logPrefix()} Could not resolve surface window:`, error.message);
    }

    return surfaceWindowId;
}

async function getCurrentSessionId() {
    if (!activeTab?.id) return null;

    try {
        const response = await chrome.runtime.sendMessage({
            action: 'get_tab_session',
            tabId: activeTab.id
        });
        return response?.sessionId || null;
    } catch (_) {
        return null;
    }
}

async function loadPopupState() {
    const stateWindowId = getStateWindowId();
    if (!stateWindowId) return;

    try {
        const state = await chrome.runtime.sendMessage({
            action: 'get_popup_state',
            windowId: stateWindowId
        });

        if (automationInput && state.goal !== undefined && document.activeElement !== automationInput) {
            automationInput.value = state.goal;
        }

        if (automationIncludeUrl && state.includeUrl !== undefined) {
            automationIncludeUrl.checked = state.includeUrl;
        }
        if (automationUseTab && state.useThisTab !== undefined) {
            automationUseTab.checked = state.useThisTab;
        }

        const sessionId = await getCurrentSessionId();
        const overlayState = await chrome.runtime.sendMessage({
            action: 'get_visual_overlay',
            sessionId
        });

        if (settingVisualOverlay && overlayState) {
            settingVisualOverlay.checked = overlayState.enabled;
            settingVisualOverlay.disabled = false;
            settingVisualOverlay.title = sessionId
                ? 'Controls overlay effects during automation'
                : 'Sets default for future automation sessions';
        }
    } catch (error) {
        console.debug(`${logPrefix()} Could not load panel state:`, error.message);
    }
}

async function saveVisualOverlayState() {
    const sessionId = await getCurrentSessionId();

    try {
        await chrome.runtime.sendMessage({
            action: 'set_visual_overlay',
            sessionId,
            enabled: settingVisualOverlay?.checked || false
        });
    } catch (error) {
        console.debug(`${logPrefix()} Could not save visual overlay state:`, error.message);
    }
}

async function savePopupState(partial) {
    const stateWindowId = getStateWindowId();
    if (!stateWindowId) return;

    try {
        await chrome.runtime.sendMessage({
            action: 'set_popup_state',
            windowId: stateWindowId,
            ...partial
        });
    } catch (error) {
        console.debug(`${logPrefix()} Could not save panel state:`, error.message);
    }
}

async function clearPopupGoal() {
    const stateWindowId = getStateWindowId();
    if (!stateWindowId) return;

    try {
        await chrome.runtime.sendMessage({
            action: 'set_popup_state',
            windowId: stateWindowId,
            goal: ''
        });
    } catch (error) {
        console.debug(`${logPrefix()} Could not clear goal:`, error.message);
    }

    if (automationInput) {
        automationInput.value = '';
    }
}

async function getActiveExecution() {
    if (!activeTab?.id) return null;

    try {
        const response = await chrome.runtime.sendMessage({
            action: 'get_tab_session',
            tabId: activeTab.id
        });

        if (response?.sessionId) {
            const executionId = executionIdFromSessionId(response.sessionId);
            if (executionId) {
                const sessionId = response.sessionId;

                try {
                    const apiResponse = await magicianFetch(
                        `${MAGICIAN_API_BASE}/executions/${executionId}`,
                        { signal: AbortSignal.timeout(2000) }
                    );

                    if (apiResponse.ok) {
                        const execution = await apiResponse.json();
                        const status = normalizeExecutionStatus(execution);

                        if (!isTerminalExecutionStatus(status)) {
                            return executionId;
                        }

                        await clearStaleSession(sessionId, executionId, `terminal state: ${status}`);
                    } else if (apiResponse.status === 404) {
                        await clearStaleSession(sessionId, executionId, 'execution not found (404)');
                    }
                } catch (error) {
                    console.debug(`${logPrefix()} API error checking execution ${executionId}:`, error.message);
                    return executionId;
                }
            }
        }
    } catch (_) {
        // Ignore background unavailability and fall through to active execution list.
    }

    try {
        const response = await chrome.runtime.sendMessage({
            action: 'get_active_executions'
        });
        if (response?.executions?.length > 0) {
            const execution = response.executions[0];
            try {
                const apiResponse = await magicianFetch(
                    `${MAGICIAN_API_BASE}/executions/${execution.executionId}`,
                    { signal: AbortSignal.timeout(2000) }
                );
                if (apiResponse.ok) {
                    const executionData = await apiResponse.json();
                    const status = normalizeExecutionStatus(executionData);
                    if (!isTerminalExecutionStatus(status)) {
                        return execution.executionId;
                    }
                }
            } catch (_) {
                return execution.executionId;
            }
        }
    } catch (_) {
        // Ignore background unavailability.
    }

    return null;
}

async function clearStaleSession(sessionId, executionId, reason) {
    console.log(`${logPrefix()} Clearing stale session ${sessionId} (${reason})`);
    try {
        await chrome.runtime.sendMessage({
            action: 'clear_session',
            sessionId
        });
    } catch (error) {
        console.debug(`${logPrefix()} Could not clear session ${executionId}:`, error.message);
    }
}

async function checkStatus() {
    try {
        const response = await chrome.runtime.sendMessage({ action: 'ping' });

        const statusDiv = document.getElementById('status');
        const statusText = document.getElementById('status-text');

        const bridgeOk = !!response?.bridge;
        const httpOk = !!response?.http;
        const magicianOk = !!response?.magician;
        const allOk = bridgeOk && httpOk && magicianOk;

        automationServerReady = allOk;
        await updateAutomationControlState();

        if (response && allOk) {
            statusDiv.className = 'status connected';
            statusText.textContent = '✓ Server Ready';
        } else {
            statusDiv.className = 'status disconnected';
            statusText.textContent = '✗ Server Offline';
        }

        const detailsDiv = document.getElementById('status-details');
        const versions = response?.versions || {};
        if (detailsDiv) {
            const magicutorVersion = versions.magicutor ? ` v${versions.magicutor}` : '';
            const magicianVersion = versions.magician ? ` v${versions.magician}` : '';

            detailsDiv.innerHTML = `
                <div class="detail-row">
                    <span>Bridge:</span>
                    <span class="${bridgeOk ? 'ok' : 'err'}">${bridgeOk ? '✓ Connected' : '✗ Disconnected'}</span>
                </div>
                <div class="detail-row">
                    <span>Magicutor${magicutorVersion}:</span>
                    <span class="${httpOk ? 'ok' : 'err'}">${httpOk ? '✓ Running' : '✗ Not running'}</span>
                </div>
                <div class="detail-row">
                    <span>Magician${magicianVersion}:</span>
                    <span class="${magicianOk ? 'ok' : 'err'}">${magicianOk ? '✓ Running' : '✗ Not running'}</span>
                </div>
            `;
        }
    } catch (error) {
        const statusDiv = document.getElementById('status');
        const statusText = document.getElementById('status-text');
        statusDiv.className = 'status disconnected';
        statusText.textContent = `✗ Error: ${error.message}`;
        automationServerReady = false;
        await updateAutomationControlState();
    }
}

async function updateActivity() {
    try {
        const activity = await chrome.runtime.sendMessage({ action: 'get_activity' });

        const currentActionDiv = document.getElementById('current-action');
        const idleStatusDiv = document.getElementById('idle-status');
        const currentActionName = document.getElementById('current-action-name');
        const currentActionDetails = document.getElementById('current-action-details');

        if (activity.current) {
            currentActionDiv.classList.add('active');
            idleStatusDiv.classList.add('hidden');
            const displayName = activity.current.displayName || formatActionName(activity.current.action);
            const displayTooltip = activity.current.tooltip || activity.current.displayDetails || '';
            currentActionName.textContent = displayName;
            currentActionName.title = displayTooltip;

            const elapsed = Math.round((Date.now() - activity.current.startTime) / 1000);
            const params = activity.current.params || {};
            const paramStr = Object.entries(params)
                .filter(([key, value]) => value && key !== 'tabId' && key !== 'windowId')
                .map(([key, value]) => `${key}: ${value}`)
                .slice(0, 2)
                .join(', ');

            const details = activity.current.displayDetails || paramStr;
            currentActionDetails.textContent = `Running for ${elapsed}s${details ? ` • ${details}` : ''}`;
            currentActionDetails.title = displayTooltip || details || '';
        } else {
            currentActionDiv.classList.remove('active');
            idleStatusDiv.classList.remove('hidden');
        }

        document.getElementById('stat-total').textContent = activity.stats.total;
        document.getElementById('stat-success').textContent = activity.stats.success;
        document.getElementById('stat-failed').textContent = activity.stats.failed;

        const recentActionsDiv = document.getElementById('recent-actions');
        if (activity.recentActions && activity.recentActions.length > 0) {
            recentActionsDiv.innerHTML = activity.recentActions.map((action) => {
                const timeAgo = formatTimeAgo(action.endTime);
                const duration = action.duration < 1000
                    ? `${action.duration}ms`
                    : `${(action.duration / 1000).toFixed(1)}s`;
                const actionName = action.displayName || formatActionName(action.action);
                const actionDetail = action.displayDetails || '';
                const actionTooltip = action.tooltip || actionDetail || actionName;

                return `
                    <div class="action-item ${action.success ? '' : 'failed'}" title="${escapeHtml(actionTooltip)}">
                        <div class="action-info">
                            <div class="status-dot ${action.success ? 'success' : 'failed'}"></div>
                            <div class="action-text">
                                <span class="action-name">${escapeHtml(actionName)}</span>
                                ${actionDetail ? `<span class="action-detail">${escapeHtml(actionDetail)}</span>` : ''}
                            </div>
                        </div>
                        <span class="action-time">${duration} • ${timeAgo}</span>
                    </div>
                    ${action.error ? `<div class="action-item failed" style="padding-left: 22px; margin-top: -4px;"><span class="error-msg">${escapeHtml(action.error)}</span></div>` : ''}
                `;
            }).join('');
        } else if (activity.stats.total === 0) {
            recentActionsDiv.innerHTML = '<div class="action-item" style="color: #888; justify-content: center;">No actions yet</div>';
        }

        const decisionsTitle = document.getElementById('decisions-title');
        const decisionsDiv = document.getElementById('recent-decisions');
        if (activity.recentDecisions && activity.recentDecisions.length > 0) {
            decisionsTitle.style.display = '';
            decisionsDiv.innerHTML = activity.recentDecisions.map((decision) => {
                const timeStr = new Date(decision.timestamp).toLocaleTimeString();
                const confPct = decision.confidence != null ? `${(decision.confidence * 100).toFixed(0)}%` : '';
                const safeToolName = escapeHtml(decision.toolName || '');
                const toolBadge = decision.toolName ? `<span class="decision-tool">${safeToolName}</span>` : '';
                const safeDecisionType = escapeHtml(decision.decisionType || '');
                const validTypes = ['execute', 'goal_reached', 'cannot_proceed', 'need_user_input', 'spawn_sub_goal', 'delegate_to_agent', 'delegate_to_agent_async', 'create_task'];
                const safeTypeClass = validTypes.includes(decision.decisionType) ? decision.decisionType : '';
                const thinkingHtml = decision.thinking ? `<div class="decision-thinking">${escapeHtml(decision.thinking)}</div>` : '';
                const evidenceHtml = decision.evidence ? `<div class="decision-detail-label">Evidence</div><div class="decision-detail-value">${escapeHtml(decision.evidence)}</div>` : '';
                const reasoningHtml = decision.reasoning && decision.reasoning !== decision.thinking
                    ? `<div class="decision-detail-label">Reasoning</div><div class="decision-detail-value">${escapeHtml(decision.reasoning)}</div>`
                    : '';
                const actionHtml = decision.actionSummary
                    ? `<div class="decision-detail-label">Action</div><div class="decision-detail-value" style="font-family: monospace;">${escapeHtml(decision.actionSummary)}</div>`
                    : '';

                return `
                    <div class="decision-card ${safeTypeClass}" onclick="this.classList.toggle('expanded')">
                        <div class="decision-header">
                            <span class="decision-type-badge ${safeTypeClass}">${safeDecisionType}</span>
                            ${toolBadge}
                            <span class="decision-confidence">${confPct} ${timeStr}</span>
                        </div>
                        ${thinkingHtml}
                        <div class="decision-details">
                            ${actionHtml}
                            ${evidenceHtml}
                            ${reasoningHtml}
                        </div>
                    </div>
                `;
            }).join('');
        } else {
            decisionsTitle.style.display = 'none';
            decisionsDiv.innerHTML = '';
        }
    } catch (error) {
        console.error(`${logPrefix()} Failed to get activity:`, error);
    }
}

function escapeHtml(str) {
    if (!str) return '';
    return String(str)
        .replace(/&/g, '&amp;')
        .replace(/</g, '&lt;')
        .replace(/>/g, '&gt;')
        .replace(/"/g, '&quot;');
}

function formatActionName(action) {
    if (!action) return 'Unknown';

    return action
        .split('_')
        .map((word) => word.charAt(0).toUpperCase() + word.slice(1))
        .join(' ');
}

function formatTimeAgo(timestamp) {
    const seconds = Math.floor((Date.now() - timestamp) / 1000);
    if (seconds < 60) return `${seconds}s ago`;
    const minutes = Math.floor(seconds / 60);
    if (minutes < 60) return `${minutes}m ago`;
    const hours = Math.floor(minutes / 60);
    return `${hours}h ago`;
}

function setAutomationStatus(text, tone = '') {
    if (!automationStatus) return;

    automationStatus.textContent = text;
    automationStatus.className = tone ? tone : '';
    if (tone) {
        automationStatus.classList.add(tone);
    }
}

async function fetchPauseInputType(executionId) {
    const response = await magicianFetch(
        `${MAGICIAN_API_BASE}/executions/${executionId}/pause-state`,
        { signal: AbortSignal.timeout(2000) }
    );

    if (!response.ok) {
        return null;
    }

    const payload = await response.json().catch(() => null);
    const inputType =
        payload?.active_pause_state?.input_type
        || payload?.input_type
        || null;

    if (typeof inputType === 'string') {
        return inputType.toLowerCase();
    }

    if (inputType && typeof inputType === 'object' && typeof inputType.type === 'string') {
        return inputType.type.toLowerCase();
    }

    return null;
}

async function updateAutomationControlState() {
    const serverReady = automationServerReady;
    const tabReady = automationTabReady;

    if (automationInput) {
        automationInput.disabled = !serverReady;
    }

    if (automationRun) {
        automationRun.disabled = !serverReady;
    }

    if (automationIncludeUrl) {
        automationIncludeUrl.disabled = !serverReady || !tabReady;
        if (!tabReady) {
            automationIncludeUrl.checked = false;
        }
    }

    if (automationUseTab) {
        automationUseTab.disabled = !serverReady || !tabReady;
        if (!tabReady) {
            automationUseTab.checked = false;
        }
    }

    if (automationSkipPlanning) {
        automationSkipPlanning.checked = true;
        automationSkipPlanning.disabled = true;
    }

    const activeExecutionId = await getActiveExecution();
    const hasActiveExecution = !!activeExecutionId;

    let isPaused = false;
    let isWaitingChildren = false;
    let requiresWebInput = false;
    if (hasActiveExecution) {
        try {
            const resp = await magicianFetch(
                `${MAGICIAN_API_BASE}/executions/${activeExecutionId}`,
                { signal: AbortSignal.timeout(2000) }
            );
            if (resp.ok) {
                const execution = await resp.json();
                const status = (
                    execution.waiting_state ||
                    execution.status ||
                    execution.state ||
                    ''
                ).toLowerCase();
                isWaitingChildren = status === 'waitingchildren'
                    || status === 'waiting_children';
                if (status === 'waitinguser' || status === 'waiting_user') {
                    const pauseInputType = await fetchPauseInputType(activeExecutionId);
                    requiresWebInput = pauseInputType !== 'external_action';
                    isPaused = !requiresWebInput;
                } else {
                    isPaused = status === 'paused';
                }
            }
        } catch (_) {
            // Ignore execution polling failures during control refresh.
        }
    }

    if (automationRun) {
        automationRun.disabled = !serverReady || hasActiveExecution;
        automationRun.textContent = hasActiveExecution
            ? (
                isWaitingChildren
                    ? 'Waiting...'
                    : requiresWebInput
                        ? 'Needs Input'
                        : isPaused
                            ? 'Paused'
                            : 'Running...'
            )
            : "Let's Do It";
    }

    if (automationResume) {
        if (isPaused) {
            automationResume.classList.add('visible');
        } else {
            automationResume.classList.remove('visible');
        }
        automationResume.disabled = !serverReady;
    }

    if (automationStop) {
        if (hasActiveExecution) {
            automationStop.classList.add('visible');
        } else {
            automationStop.classList.remove('visible');
        }
        automationStop.disabled = !serverReady;
    }

    if (stopOptions) {
        stopOptions.style.display = hasActiveExecution ? 'flex' : 'none';
    }

    if (!serverReady) {
        setAutomationStatus('Server not running.', 'err');
    } else if (automationStatus?.textContent === 'Server not running.') {
        setAutomationStatus('');
    }
}

async function loadActiveTab() {
    try {
        if (surfaceName === 'sidepanel' && surfaceWindowId == null) {
            await resolveSurfaceWindowId();
        }

        const query = surfaceName === 'sidepanel' && surfaceWindowId != null
            ? { active: true, windowId: surfaceWindowId }
            : { active: true, currentWindow: true };
        const tabs = await chrome.tabs.query(query);
        activeTab = tabs && tabs.length > 0 ? tabs[0] : null;
        const url = activeTab?.url || 'Unavailable';

        if (automationTabUrl) {
            automationTabUrl.textContent = url;
            automationTabUrl.title = url;
        }

        const isHttp = typeof url === 'string' && /^https?:/i.test(url);
        automationTabReady = isHttp;

        await updateAutomationControlState();
    } catch (_) {
        activeTab = null;
        if (automationTabUrl) {
            automationTabUrl.textContent = 'Unavailable';
            automationTabUrl.title = 'Unavailable';
        }
        if (automationUseTab) {
            automationUseTab.checked = false;
            automationUseTab.disabled = true;
        }
        automationTabReady = false;
        await updateAutomationControlState();
    }
}

async function startAutomation() {
    if (!automationInput || !automationRun) return;

    if (!automationServerReady) {
        setAutomationStatus('Server not running.', 'err');
        return;
    }

    const goal = automationInput.value.trim();
    if (!goal) {
        setAutomationStatus('Please enter a goal.', 'err');
        return;
    }

    const includeTabUrl = !!(automationIncludeUrl && automationIncludeUrl.checked);
    const useThisTab = !!(automationUseTab && automationUseTab.checked);
    const tabUrl = activeTab?.url || null;
    const tabTitle = activeTab?.title || null;
    const tabId = activeTab?.id ?? null;
    const windowId = activeTab?.windowId ?? null;

    automationRun.disabled = true;
    setAutomationStatus('Starting automation...', 'warn');

    try {
        const maxIterations = maxIterationsInput ? parseInt(maxIterationsInput.value, 10) || 40 : 40;

        const response = await chrome.runtime.sendMessage({
            action: 'start_automation',
            goal,
            includeTabUrl,
            useThisTab,
            maxIterations,
            tabUrl,
            tabTitle,
            tabId,
            windowId
        });

        if (response?.error) {
            setAutomationStatus(response.error, 'err');
        } else {
            const idSnippet = response?.executionId ? ` (${response.executionId.slice(0, 8)})` : '';
            setAutomationStatus(`Automation started${idSnippet}`, 'ok');
            await clearPopupGoal();
            await updateAutomationControlState();
        }
    } catch (error) {
        setAutomationStatus(error.message || 'Failed to start automation.', 'err');
    } finally {
        automationRun.disabled = false;
    }
}

async function stopAutomation() {
    const executionId = await getActiveExecution();
    if (!executionId) {
        setAutomationStatus('No active automation to stop.', 'warn');
        return;
    }

    const closeWindows = automationCleanup ? automationCleanup.checked : true;

    if (automationStop) {
        automationStop.disabled = true;
    }
    setAutomationStatus('Stopping automation...', 'warn');

    try {
        const response = await chrome.runtime.sendMessage({
            action: 'stop_automation',
            executionId,
            closeWindows
        });

        if (response?.error) {
            setAutomationStatus(response.error, 'err');
        } else {
            setAutomationStatus('Automation stopped.', 'ok');
            await updateAutomationControlState();
        }
    } catch (error) {
        setAutomationStatus(error.message || 'Failed to stop automation.', 'err');
    } finally {
        if (automationStop) {
            automationStop.disabled = false;
        }
    }
}

async function resumeAutomation() {
    const executionId = await getActiveExecution();
    if (!executionId) {
        setAutomationStatus('No paused automation to resume.', 'warn');
        return;
    }

    if (automationResume) {
        automationResume.disabled = true;
    }
    setAutomationStatus('Resuming automation...', 'warn');

    try {
        const response = await chrome.runtime.sendMessage({
            action: 'continue_automation',
            executionId
        });

        if (response?.error) {
            setAutomationStatus(response.error, 'err');
        } else {
            setAutomationStatus('Automation resumed.', 'ok');
            await updateAutomationControlState();
        }
    } catch (error) {
        setAutomationStatus(error.message || 'Failed to resume automation.', 'err');
    } finally {
        if (automationResume) {
            automationResume.disabled = false;
        }
    }
}

async function saveDebugOverlayState() {
    const enabled = settingDebugOverlay?.checked || false;

    try {
        await chrome.storage.local.set({ debugOverlayEnabled: enabled });

        const tabs = await chrome.tabs.query({});
        for (const tab of tabs) {
            try {
                await chrome.tabs.sendMessage(tab.id, {
                    action: 'overlay_debug_mode',
                    enabled
                });
            } catch (_) {
                // Tab might not have the content script yet.
            }
        }

        console.log(`${logPrefix()} Debug overlay mode ${enabled ? 'enabled' : 'disabled'}`);
    } catch (error) {
        console.debug(`${logPrefix()} Could not save debug overlay state:`, error.message);
    }
}

async function loadDebugOverlayState() {
    try {
        const result = await chrome.storage.local.get(['debugOverlayEnabled']);
        if (settingDebugOverlay) {
            settingDebugOverlay.checked = result.debugOverlayEnabled || false;
        }
    } catch (error) {
        console.debug(`${logPrefix()} Could not load debug overlay state:`, error.message);
    }
}

async function savePointerDiagnosticsState() {
    const enabled = settingPointerDiagnostics?.checked || false;

    try {
        await chrome.storage.local.set({ pointerDiagnosticsEnabled: enabled });
        console.log(`${logPrefix()} Pointer hit diagnostics ${enabled ? 'enabled' : 'disabled'}`);
    } catch (error) {
        console.debug(`${logPrefix()} Could not save pointer diagnostics state:`, error.message);
    }
}

async function loadPointerDiagnosticsState() {
    try {
        const result = await chrome.storage.local.get(['pointerDiagnosticsEnabled']);
        if (settingPointerDiagnostics) {
            settingPointerDiagnostics.checked = result.pointerDiagnosticsEnabled || false;
        }
    } catch (error) {
        console.debug(`${logPrefix()} Could not load pointer diagnostics state:`, error.message);
    }
}

function setupPersistentSurfaceListeners() {
    addChromeListener(chrome.tabs.onActivated, (activeInfo) => {
        if (surfaceWindowId != null && activeInfo.windowId !== surfaceWindowId) {
            return;
        }
        void loadActiveTab();
    });

    addChromeListener(chrome.tabs.onUpdated, (tabId, changeInfo, tab) => {
        if (surfaceWindowId != null && tab?.windowId !== surfaceWindowId) {
            return;
        }
        const affectsActiveTab = tabId === activeTab?.id || tab?.active;
        const meaningfulChange = changeInfo.url !== undefined || changeInfo.title !== undefined || changeInfo.status === 'complete';
        if (affectsActiveTab && meaningfulChange) {
            void loadActiveTab();
        }
    });

    addChromeListener(chrome.windows.onFocusChanged, (windowId) => {
        if (windowId !== chrome.windows.WINDOW_ID_NONE && windowId === surfaceWindowId) {
            void loadActiveTab();
        }
    });

    addDomListener(document, 'visibilitychange', () => {
        if (document.visibilityState === 'visible') {
            void loadActiveTab();
        }
    });
}

function setupUiListeners() {
    addDomListener(automationRun, 'click', () => {
        void startAutomation();
    });

    addDomListener(automationStop, 'click', () => {
        void stopAutomation();
    });

    addDomListener(automationResume, 'click', () => {
        void resumeAutomation();
    });

    addDomListener(automationInput, 'keydown', (event) => {
        if ((event.ctrlKey || event.metaKey) && event.key === 'Enter') {
            void startAutomation();
        }
    });

    addDomListener(automationInput, 'input', () => {
        if (goalSaveTimeoutId) {
            clearTimeout(goalSaveTimeoutId);
        }
        goalSaveTimeoutId = setTimeout(() => {
            void savePopupState({ goal: automationInput?.value || '' });
        }, 300);
    });

    addDomListener(automationIncludeUrl, 'change', () => {
        void savePopupState({ includeUrl: automationIncludeUrl.checked });
    });

    addDomListener(automationUseTab, 'change', () => {
        void savePopupState({ useThisTab: automationUseTab.checked });
    });

    addDomListener(settingVisualOverlay, 'change', () => {
        void saveVisualOverlayState();
    });

    addDomListener(settingDebugOverlay, 'change', () => {
        void saveDebugOverlayState();
    });

    addDomListener(settingPointerDiagnostics, 'change', () => {
        void savePointerDiagnosticsState();
    });
}

export async function initControlPanel(options = {}) {
    if (initialized) {
        return destroyControlPanel;
    }

    surfaceName = options.surface || 'popup';
    initialized = true;

    bindElements();
    setupUiListeners();

    if (surfaceName === 'sidepanel') {
        await resolveSurfaceWindowId();
    }

    if (options.trackActiveTab ?? surfaceName === 'sidepanel') {
        setupPersistentSurfaceListeners();
    }

    const handleUnload = () => {
        destroyControlPanel();
    };
    addDomListener(window, 'pagehide', handleUnload, { once: true });
    addDomListener(window, 'beforeunload', handleUnload, { once: true });

    await loadActiveTab();
    await loadPopupState();
    await loadDebugOverlayState();
    await loadPointerDiagnosticsState();
    await updateAutomationControlState();

    void checkStatus();
    void updateActivity();

    registerInterval(() => {
        void checkStatus();
    }, 5000);
    registerInterval(() => {
        void updateActivity();
    }, 500);
    registerInterval(() => {
        void updateAutomationControlState();
    }, 3000);

    return destroyControlPanel;
}
