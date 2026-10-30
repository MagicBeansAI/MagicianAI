import {
    debuggerSessions,
    clearSession,
    registerSessionTab,
    getTabSessionContext,
    getSessionForTab,
    getSessionTabs,
    getActiveExecutions,
    untrackExecution,
    trackActiveExecution,
    getVisualOverlayState
} from './background_state.js';
import {
    connectBridge,
    reconnectBridge,
    isBridgeConnected,
    sendToBridge,
    sendBrowserLabelToBridge,
    sendBridgeMessage,
    disconnectBridge
} from './bridge.js';
import {
    badgeTextFor,
    createBridgePrompts,
    readBridgeConsent,
    writeBridgeConsent
} from './bridge_prompts.js';
import {
    bridgeIdentityQuery,
    getBrowserIdentity,
    setBrowserLabel
} from './browser_identity.js';
import {
    browserThreadIdForExecution,
    executionIdFromSessionId
} from './automation_session.js';
import { setupTabListeners, setupWindowListeners } from './tabs.js';
import { setupDebuggerListeners } from './debugger.js';
import {
    closeTab,
    closeWindow,
    createWindow,
    createTab,
    listTabs
} from './actions_tabs_windows.js';
import { closeAutomationTabs, handoffAutomationTabs, automationTabOwners, forgetAllAutomationTabs } from './automation_tabs.js';
import { attachDebugger, detachDebugger, sendDebuggerCommand } from './debugger_actions.js';
import { forgetAllSetup } from './debugger_setup.js';
import {
    rendersInBackground,
    moveSessionToOwnWindow,
    recheckInWindowSessions,
    releaseSessionGroup,
    forgetAllGroups,
    noteActivatedTab
} from './automation_surface.js';

// CDP on one tab's main session, through the same path as the proxy's
// commands (so emulation lands in the replayed setup too).
function cdpSenderForTab(tabId) {
    return (method, params) => sendDebuggerCommand({ tabId, method, params });
}

chrome.tabs.onActivated.addListener(info => { void noteActivatedTab(info).catch(() => {}); });
import { createExtensionMessageHandler, pollExecutionStatus } from './extension_messages.js';
import { activityState, recordActionComplete, recordActionStart, recordDecision, summarizeParams } from './activity_state.js';
import {
    MAGICUTOR_API_BASE,
    MAGICIAN_API_BASE,
    refreshRuntimeEndpoints
} from './config.js';
import { magicianFetch } from './magician_scope.js';
import {
    isWhatsAppPromptTab,
    planRingApply,
    readWhatsAppCallRing,
    reportWhatsAppAttendance,
    reportWhatsAppCallRing
} from './whatsapp_call_ring.mjs';
// WEG Phase 2: ambient browsing capture collector. Side-effect import — the
// module registers its onMessage + alarm listeners at top level (MV3-safe).
import './ambient_collector.js';
import { setupLivePreviewMessaging, handleLivePreviewMessage } from './live_preview_background.js';
import {
    captureContextualAssistTab,
    probeContextualAssist
} from './contextual_assist_background.js';
import { setupContextualAssistContextMenu } from './contextual_assist_menu.js';
import { setupNotesCaptureContextMenu } from './notes_capture_menu.js';
import {
    buildTerminalOverlayStatus,
    isPausedExecutionStatus,
    isTerminalExecutionStatus,
    isWaitingChildrenStatus,
    normalizeExecutionStatus
} from './execution_status.js';

// ============================================================================
// VISUAL OVERLAY HELPERS
// ============================================================================

const VISUAL_DELAY_MS = 80;
const POINTER_DIAGNOSTICS_STORAGE_KEY = 'pointerDiagnosticsEnabled';
let pointerDiagnosticsEnabled = false;

chrome.storage.local.get([POINTER_DIAGNOSTICS_STORAGE_KEY], (result) => {
    if (chrome.runtime.lastError) {
        console.debug('[Magicutor] Could not load pointer diagnostics setting:', chrome.runtime.lastError.message);
        return;
    }
    pointerDiagnosticsEnabled = result?.[POINTER_DIAGNOSTICS_STORAGE_KEY] === true;
});

chrome.storage.onChanged.addListener((changes, areaName) => {
    if (areaName !== 'local' || !changes[POINTER_DIAGNOSTICS_STORAGE_KEY]) {
        return;
    }
    pointerDiagnosticsEnabled = changes[POINTER_DIAGNOSTICS_STORAGE_KEY].newValue === true;
});

function executionIdForTab(tabId) {
    if (!tabId) return null;
    const sessionId = getSessionForTab(tabId);
    return executionIdFromSessionId(sessionId);
}

function scheduleOverlayPanelShow(tabId, payload, delays = [250, 750, 1500, 3000]) {
    if (!tabId) return;
    const executionId = payload.executionId || executionIdForTab(tabId);
    if (!executionId) return;

    for (const delay of delays) {
        setTimeout(() => {
            try {
                chrome.tabs.sendMessage(tabId, {
                    action: 'overlay_panel_show',
                    ...payload,
                    executionId
                }).catch(() => {});
            } catch (_) {
                // Content script may not be ready yet.
            }
        }, delay);
    }
}

async function sendOverlayStatus(tabId, status, text) {
    if (!tabId || !status) return;
    const executionId = executionIdForTab(tabId);
    await chrome.tabs.sendMessage(tabId, {
        action: 'overlay_status',
        executionId,
        status,
        text
    });
}

async function showOverlayPreview(tabId, type, rect, params = {}, options = {}) {
    if (!rect || !tabId) return;

    // Check if visual overlay is enabled for this session
    const sessionId = getSessionForTab(tabId);
    // Default to enabled if no session (e.g. debugging or legacy)
    if (sessionId && !getVisualOverlayState(sessionId)) {
        console.log(`[VisualOverlay] Skipped (disabled for session ${sessionId})`);
        return;
    }

    // Build status text for the panel
    const statusText = buildStatusText(type, params);
    const executionId = executionIdFromSessionId(sessionId);

    try {
        console.log(`[VisualOverlay] Sending preview to tab ${tabId}: ${type}`, rect);
        // Send message to content script
        await chrome.tabs.sendMessage(tabId, {
            action: 'overlay_preview',
            executionId,
            type,
            target: { rect },
            params,
            duration: VISUAL_DELAY_MS
        });

        // Also update status panel
        await chrome.tabs.sendMessage(tabId, {
            action: 'overlay_status',
            executionId,
            status: 'running',
            text: statusText
        });

        // Wait for animation to play
        if (options.waitForAnimation !== false) {
            await new Promise(resolve => setTimeout(resolve, VISUAL_DELAY_MS));
        }
    } catch (e) {
        // Content script might not be injected or ready
        // console.warn('[VisualOverlay] Failed to show preview:', e.message);
    }
}

/**
 * Build human-readable status text for the floating panel
 */
function buildStatusText(type, params = {}) {
    switch (type) {
        case 'click':
            const clickType = params.button === 'right' ? 'Right-clicking' :
                              params.clickCount === 2 ? 'Double-clicking' : 'Clicking';
            return params.label ? `${clickType} "${params.label}"` : `${clickType}...`;
        case 'type':
            const text = params.text || '';
            const truncated = text.length > 25 ? text.substring(0, 25) + '…' : text;
            return params.label ? `Typing in "${params.label}"` : `Typing "${truncated}"`;
        case 'scroll':
            const dir = params.direction || '';
            return dir ? `Scrolling ${dir}` : 'Scrolling...';
        case 'navigate':
            if (params.url) {
                try {
                    const domain = new URL(params.url).hostname;
                    return `Navigating to ${domain}`;
                } catch (e) {}
            }
            return 'Navigating...';
        case 'hover':
            return params.label ? `Hovering "${params.label}"` : 'Hovering...';
        case 'select':
            return params.value ? `Selecting "${params.value}"` : 'Selecting...';
        case 'wait':
            return params.text || 'Waiting...';
        case 'press_key':
        case 'key':
            const key = params.key || params.text || '';
            return key ? `Pressing ${key}` : 'Pressing key...';
        case 'drag_and_drop':
        case 'drag_path':
        case 'drag':
            return 'Dragging...';
        case 'observe':
            return 'Observing page...';
        case 'screenshot':
        case 'vision':
            return 'Vision observing...';
        case 'wait_selector':
            return params.selector ? `Waiting for element...` : 'Waiting for element...';
        case 'wait_text':
            const waitText = params.text || '';
            const waitTrunc = waitText.length > 20 ? waitText.substring(0, 20) + '…' : waitText;
            return waitTrunc ? `Waiting for "${waitTrunc}"` : 'Waiting for text...';
        case 'wait_network_idle':
        case 'wait_network':
            return 'Waiting for network...';
        case 'focus':
            return params.label ? `Focusing "${params.label}"` : 'Focusing...';
        case 'clear':
            return params.label ? `Clearing "${params.label}"` : 'Clearing field...';
        case 'upload_file':
        case 'upload':
            return 'Uploading file...';
        case 'download_file':
        case 'download':
            return 'Downloading file...';
        case 'thinking':
        case 'llm':
        case 'deciding':
            return 'Thinking...';
        case 'api_replay':
            return 'Replaying API...';
        default:
            return 'Working...';
    }
}

/**
 * Build past-tense completion text for actions.
 * Shows briefly after action completes to confirm what happened.
 */
function buildCompletedStatusText(type, params = {}) {
    switch (type) {
        case 'click':
            const clickType = params.button === 'right' ? 'Right-clicked' :
                              params.clickCount === 2 ? 'Double-clicked' : 'Clicked';
            return params.label ? `${clickType} "${params.label}"` : `${clickType}!`;
        case 'type':
            return params.label ? `Typed in "${params.label}"` : 'Typed!';
        case 'scroll':
            const dir = params.direction || '';
            return dir ? `Scrolled ${dir}` : 'Scrolled!';
        case 'navigate':
            if (params.url) {
                try {
                    const domain = new URL(params.url).hostname;
                    return `Navigated to ${domain}`;
                } catch (e) {}
            }
            return 'Navigated!';
        case 'hover':
            return params.label ? `Hovered "${params.label}"` : 'Hovered!';
        case 'select':
            return params.value ? `Selected "${params.value}"` : 'Selected!';
        case 'press_key':
        case 'key':
            const key = params.key || params.text || '';
            return key ? `Pressed ${key}` : 'Key pressed!';
        case 'drag_and_drop':
        case 'drag_path':
        case 'drag':
            return 'Dragged!';
        case 'focus':
            return params.label ? `Focused "${params.label}"` : 'Focused!';
        case 'clear':
            return params.label ? `Cleared "${params.label}"` : 'Cleared!';
        case 'upload_file':
        case 'upload':
            return 'File uploaded!';
        case 'download_file':
        case 'download':
            return 'File downloaded!';
        // These actions take longer, so don't need past-tense (user already sees them)
        case 'observe':
        case 'screenshot':
        case 'vision':
        case 'wait':
        case 'wait_selector':
        case 'wait_text':
        case 'wait_network_idle':
        case 'wait_network':
        case 'thinking':
        case 'llm':
        case 'deciding':
        case 'api_replay':
            return null; // No past-tense needed for slow actions
        default:
            return 'Done!';
    }
}

async function showOverlayComplete(tabId, success = true, actionType = null, actionParams = {}) {
    if (!tabId) return;
    const executionId = executionIdForTab(tabId);
    try {
        await chrome.tabs.sendMessage(tabId, {
            action: 'overlay_complete',
            executionId,
            success
        });

        if (success && actionType) {
            // Show past-tense completion text for fast actions
            const completedText = buildCompletedStatusText(actionType, actionParams);
            if (completedText) {
                // Show "Clicked!", "Typed!" etc
                await chrome.tabs.sendMessage(tabId, {
                    action: 'overlay_status',
                    executionId,
                    status: 'running',
                    text: completedText
                });
                // Let it display for a moment - the next action will update the status
                // (observe, thinking, next click, etc.)
                return; // Don't set another status - let the next action do it
            }
        }

        // Only show error if action failed (success case is handled above or by next action)
        if (!success) {
            await chrome.tabs.sendMessage(tabId, {
                action: 'overlay_status',
                status: 'error',
                text: 'Action failed'
            });
        }
    } catch (e) {}
}

async function showOverlayError(tabId, message) {
    if (!tabId) return;
    const executionId = executionIdForTab(tabId);
    try {
        await chrome.tabs.sendMessage(tabId, {
            action: 'overlay_error',
            executionId,
            message
        });
        // Update status panel
        await chrome.tabs.sendMessage(tabId, {
            action: 'overlay_status',
            executionId,
            status: 'error',
            text: message || 'Error occurred'
        });
    } catch (e) {}
}

function cdpCommandOverlayDescriptor(params = {}) {
    const method = params.method;
    const commandParams = params.params || {};

    if (method === 'Input.dispatchMouseEvent') {
        const x = Number(commandParams.x);
        const y = Number(commandParams.y);
        if (!Number.isFinite(x) || !Number.isFinite(y)) return null;

        const rect = { x, y, width: 0, height: 0 };
        const eventType = commandParams.type;

        if (eventType === 'mouseWheel') {
            return {
                rect,
                type: 'scroll',
                actionParams: {
                    direction: Number(commandParams.deltaY || 0) >= 0 ? 'down' : 'up'
                },
                complete: false
            };
        }

        if (eventType === 'mousePressed') {
            return {
                rect,
                type: 'click',
                actionParams: {
                    button: commandParams.button || 'left',
                    clickCount: commandParams.clickCount || 1
                },
                complete: false
            };
        }

        if (eventType === 'mouseReleased') {
            return {
                rect,
                type: commandParams.buttons ? 'drag' : 'click',
                actionParams: {
                    button: commandParams.button || 'left',
                    clickCount: commandParams.clickCount || 1
                },
                complete: true
            };
        }

        if (eventType === 'mouseMoved') {
            return {
                rect,
                type: Number(commandParams.buttons || 0) > 0 ? 'drag' : 'hover',
                actionParams: {},
                complete: false
            };
        }
    }

    if (method === 'Input.insertText') {
        return {
            statusOnly: true,
            type: 'type',
            actionParams: { text: commandParams.text || '' }
        };
    }

    if (method === 'Input.dispatchKeyEvent') {
        if (commandParams.type === 'keyUp') return null;
        return {
            statusOnly: true,
            type: 'key',
            actionParams: { key: commandParams.key || commandParams.text || commandParams.code || '' }
        };
    }

    if (method === 'Page.navigate') {
        return {
            statusOnly: true,
            type: 'navigate',
            actionParams: { url: commandParams.url || '' }
        };
    }

    if (method === 'Page.captureScreenshot') {
        return {
            statusOnly: true,
            type: 'screenshot',
            actionParams: {}
        };
    }

    return null;
}

async function showCdpCommandOverlayBefore(params = {}) {
    const descriptor = cdpCommandOverlayDescriptor(params);
    if (!descriptor || !params.tabId) return null;

    if (descriptor.statusOnly) {
        sendOverlayStatus(
            params.tabId,
            'running',
            buildStatusText(descriptor.type, descriptor.actionParams)
        ).catch(() => {});
        return descriptor;
    }

    showOverlayPreview(
        params.tabId,
        descriptor.type,
        descriptor.rect,
        descriptor.actionParams,
        { waitForAnimation: false }
    ).catch(() => {});
    return descriptor;
}

/**
 * Magicutor Browser Control Extension - Background Service Worker
 *
 * Architecture:
 *   Magician Orchestrator --> Magicutor HTTP Server (port 3003)
 *                                     |
 *                               WebSocket Bridge
 *                                     |
 *                            Chrome Extension (this)
 *
 * Responsibilities:
 * - Maintain WebSocket bridge connection to Magicutor HTTP server
 * - Handle window and tab management
 * - Execute browser actions via chrome.debugger API
 * - Route requests/responses between server and browser
 */

// Pairing and per-run browser-choice prompts (see bridge_prompts.js).
const bridgePrompts = createBridgePrompts();

async function refreshBridgeBadge() {
    const snapshot = bridgePrompts.snapshot({ consent: await readBridgeConsent() });
    try {
        await chrome.action.setBadgeText({ text: badgeTextFor(snapshot) });
        await chrome.action.setBadgeBackgroundColor({ color: '#d97706' });
    } catch (_) {
        // The badge is a hint; the panel still shows the prompt.
    }
    return snapshot;
}

function handleBridgeNotice(envelope) {
    bridgePrompts.applyNotice(envelope);
    if (envelope?.type === 'browser_state') {
        // Remote acceptance (from Magician) or revocation keeps the stored
        // consent in step with Magicutor.
        if (envelope.state === 'accepted') void writeBridgeConsent('accepted');
        if (envelope.state === 'pending') void writeBridgeConsent('pending');
    }
    void refreshBridgeBadge();
}

const bridgePromptActions = {
    async snapshot() {
        return bridgePrompts.snapshot({ consent: await readBridgeConsent() });
    },
    async accept() {
        await writeBridgeConsent('accepted');
        bridgePrompts.setPairing('accepted');
        sendBridgeMessage({ type: 'browser_consent', accepted: true });
        return refreshBridgeBadge();
    },
    async decline() {
        await writeBridgeConsent('declined');
        sendBridgeMessage({ type: 'browser_consent', accepted: false });
        disconnectBridge();
        return refreshBridgeBadge();
    },
    async connect() {
        await writeBridgeConsent('pending');
        void connectBridge(createBridgeConfig());
        return refreshBridgeBadge();
    },
    async choose(choiceId, always) {
        const sent = sendBridgeMessage({ type: 'browser_choice_answer', choice_id: choiceId, always: !!always });
        bridgePrompts.answered(choiceId);
        await refreshBridgeBadge();
        return { sent };
    }
};

/** Whether the owner declined pairing: then the bridge stays disconnected. */
async function bridgeDeclined() {
    return (await readBridgeConsent()) === 'declined';
}

const extensionMessageHandler = createExtensionMessageHandler({
    isBridgeConnected,
    checkHttpHealth,
    activityState,
    recordDecision,
    getBrowserIdentity,
    async renameBrowser(label) {
        const identity = await setBrowserLabel(label);
        sendBrowserLabelToBridge(identity.label);
        return identity;
    },
    bridgePrompts: bridgePromptActions,
});

/**
 * Bridge connection settings. The handshake names this browser so Magicutor
 * can hold several browsers at once and let this one's reconnect replace its
 * own stale socket.
 */
function createBridgeConfig() {
    return {
        checkHttpHealth,
        onRequest: handleBridgeRequest,
        identityQuery: async () => bridgeIdentityQuery(await getBrowserIdentity()),
        onNotice: handleBridgeNotice
    };
}

/**
 * Initialize extension
 */
// ============================================================================
// STALE EXECUTION CLEANUP (via chrome.alarms)
// ============================================================================
// Periodically checks tracked executions in memory and cleans up sessions for
// executions that have completed/failed. This handles the case where popup is
// never opened after an execution finishes. State is lost on service worker restart,
// which is acceptable - orphaned windows will need manual closure in that case.

const EXECUTION_CLEANUP_ALARM = 'execution_cleanup';
const EXECUTION_CLEANUP_INTERVAL_MINUTES = 1;  // Check every minute
const BRIDGE_HEALTH_ALARM = 'bridge_health';   // reconnect backstop, survives SW termination

function executionTargetTabs(sessionAlias, fallbackTabId) {
    const sessionTabs = getSessionTabs(sessionAlias);
    if (sessionTabs && sessionTabs.size > 0) {
        return [...sessionTabs];
    }
    return fallbackTabId ? [fallbackTabId] : [];
}

/**
 * Every tab that may be showing an execution's overlay: the session's
 * registered tabs, the tab tracked at window creation, and every tab in the
 * execution's dedicated window. The session list alone is not enough —
 * detach_debugger clears it the moment the CDP session ends, and the tracked
 * tabId is the window's first tab, which the run may have navigated away from
 * or replaced (Target.createTarget opens a second one). Until 0.2.2 the
 * terminal overlay_status went to that stale id and the panel + aurora stayed
 * on the page the run actually used until the user pressed Stop.
 */
async function executionOverlayTabs(sessionAlias, tracked) {
    const tabs = new Set(executionTargetTabs(sessionAlias, tracked?.tabId));
    if (tracked?.windowId) {
        try {
            for (const tab of await chrome.tabs.query({ windowId: tracked.windowId })) {
                if (tab.id) tabs.add(tab.id);
            }
        } catch (_) {
            // Window already closed.
        }
    }
    return [...tabs];
}

/**
 * The run's CDP session ended (magicutor's proxy saw the client's websocket
 * close — the run's cleanup closed the agent-browser session, or the CLI
 * died). End the automation UI for it: the page's status panel and aurora
 * take the execution's real terminal state when Magician can say it, a plain
 * hide when it cannot, and stay as they are when the execution is merely
 * waiting (a paused run keeps its browser). Then the execution is untracked
 * and the session's tab registry cleared. This runs before detach_debugger,
 * while the session still knows its tabs.
 */
async function endAutomationSession(sessionId) {
    const executionId = executionIdFromSessionId(sessionId);
    const tracked = executionId ? getActiveExecutions().get(executionId) : null;
    const tabs = await executionOverlayTabs(sessionId, tracked);

    let status = null;
    if (executionId) {
        try {
            const response = await magicianFetch(`${MAGICIAN_API_BASE}/executions/${executionId}`, {
                method: 'GET',
                signal: AbortSignal.timeout(2000)
            });
            if (response.ok) {
                status = normalizeExecutionStatus(await response.json());
            }
        } catch (_) {
            // Magician unreachable: the UI still ends, without a verdict.
        }
    }

    if (status && !isTerminalExecutionStatus(status) && (isPausedExecutionStatus(status) || isWaitingChildrenStatus(status))) {
        console.log(`[SessionEnded] ${sessionId}: execution ${executionId} is ${status}; keeping its panel`);
        return { ended: false, executionId, status, tabs: tabs.length };
    }

    if (status && isTerminalExecutionStatus(status)) {
        const terminalOverlay = buildTerminalOverlayStatus(status);
        await sendOverlayMessageToTabs(tabs, {
            action: 'overlay_status',
            executionId,
            status: terminalOverlay.status,
            text: terminalOverlay.text
        });
    } else {
        await sendOverlayMessageToTabs(tabs, { action: 'overlay_panel_hide', executionId });
        await sendOverlayMessageToTabs(tabs, { action: 'overlay_hide' });
    }
    if (executionId) {
        untrackExecution(executionId);
    }
    clearSession(sessionId);
    console.log(`[SessionEnded] ${sessionId}: execution=${executionId} status=${status || 'unknown'} tabs=${tabs.length}`);
    return { ended: true, executionId, status, tabs: tabs.length };
}

async function sendOverlayMessageToTabs(tabIds, message) {
    for (const tabId of tabIds) {
        try {
            await chrome.tabs.sendMessage(tabId, message);
        } catch (_) {
            // Content script may not be present in closed or restricted tabs.
        }
    }
}

async function clearTrackedExecutionSession(executionId, sessionAlias) {
    untrackExecution(executionId);
    clearSession(sessionAlias);
}

/**
 * Clean up stale execution entries from in-memory tracking
 * Handles two types of staleness:
 * 1. TabId no longer exists (tab closed manually)
 * 2. Execution has reached any terminal state (normal completion/failure/cancel)
 */
async function cleanupStaleExecutions() {
    try {
        const executions = getActiveExecutions();
        const executionIds = Array.from(executions.keys());

        if (executionIds.length === 0) return;

        console.log(`[ExecutionCleanup] Checking ${executionIds.length} tracked execution(s)`);

        for (const executionId of executionIds) {
            const info = executions.get(executionId);
            if (!info) continue;

            const sessionAlias = browserThreadIdForExecution(executionId);
            const targetTabs = await executionOverlayTabs(sessionAlias, info);

            // Check 1: Verify the tabId still exists (handles tab closed manually)
            if (info.tabId) {
                try {
                    await chrome.tabs.get(info.tabId);
                    // Tab exists - continue to API check
                } catch (e) {
                    // Tab doesn't exist - user closed it manually
                    console.log(`[ExecutionCleanup] Tab ${info.tabId} no longer exists, untracking execution ${executionId}`);
                    await clearTrackedExecutionSession(executionId, sessionAlias);
                    continue;
                }
            }

            // Check 2: Verify execution status via Magician API
            try {
                const response = await magicianFetch(`${MAGICIAN_API_BASE}/executions/${executionId}`, {
                    method: 'GET',
                    signal: AbortSignal.timeout(5000)  // 5s timeout per execution
                });

                if (!response.ok) {
                    // Execution not found (404) or API error - mark for removal
                    console.log(
                        `[ExecutionCleanup] Execution ${executionId} not found (${response.status}), untracking`
                    );
                    await sendOverlayMessageToTabs(targetTabs, {
                        action: 'overlay_panel_hide',
                        executionId
                    });
                    await clearTrackedExecutionSession(executionId, sessionAlias);
                    continue;
                }

                const execution = await response.json();
                const status = normalizeExecutionStatus(execution);

                if (isTerminalExecutionStatus(status)) {
                    console.log(
                        `[ExecutionCleanup] Execution ${executionId} is ${status}, untracking`
                    );
                    const terminalOverlay = buildTerminalOverlayStatus(status);
                    await sendOverlayMessageToTabs(targetTabs, {
                        action: 'overlay_status',
                        executionId,
                        status: terminalOverlay.status,
                        text: terminalOverlay.text
                    });
                    await clearTrackedExecutionSession(executionId, sessionAlias);
                }
            } catch (e) {
                // Network error - Magician might be down, don't remove
                console.debug(`[ExecutionCleanup] Could not check execution ${executionId}: ${e.message}`);
            }
        }
    } catch (e) {
        console.error('[ExecutionCleanup] Error during cleanup:', e.message);
    }
}

/**
 * Setup chrome.alarms listener for periodic execution cleanup
 */
function setupExecutionCleanupAlarm() {
    // Listen for alarm
    chrome.alarms.onAlarm.addListener((alarm) => {
        if (alarm.name === EXECUTION_CLEANUP_ALARM) {
            cleanupStaleExecutions();
        }
    });

    // Create periodic alarm (will replace existing if any)
    chrome.alarms.create(EXECUTION_CLEANUP_ALARM, {
        delayInMinutes: 0.5,  // First run in 30 seconds
        periodInMinutes: EXECUTION_CLEANUP_INTERVAL_MINUTES
    });

    console.log('[ExecutionCleanup] Alarm scheduled for periodic cleanup');
}

/**
 * Setup a chrome.alarms backstop that reconnects the bridge when it is down.
 *
 * The setInterval-based reconnect in bridge.js dies when the MV3 service worker
 * is terminated (timers do not survive SW shutdown), so a bridge that dropped
 * while the worker was idle would never come back on its own — requiring a manual
 * extension reload. A chrome.alarms tick WAKES the worker (re-running this script's
 * top-level `initialize()` → `connectBridge`, and firing this listener), so the
 * bridge self-heals after a magicutor restart. `isBridgeConnected()` reflects the
 * pong watchdog in bridge.js, so a half-open "connected" socket is also caught.
 */
function setupBridgeHealthAlarm() {
    chrome.alarms.onAlarm.addListener((alarm) => {
        if (alarm.name !== BRIDGE_HEALTH_ALARM) return;
        void recheckInWindowSessions(cdpSenderForTab, tabId => debuggerSessions.has(tabId))
            .catch(error => console.warn(`[Magicutor] In-window recheck failed: ${error.message}`));
        void refreshRuntimeEndpoints().then(async (resolution) => {
            if (await bridgeDeclined()) return undefined;
            const bridgeConfig = createBridgeConfig();
            if (resolution.changed) {
                console.log('[Magicutor] Runtime service endpoints changed — reconnecting bridge');
                return reconnectBridge(bridgeConfig);
            }
            if (!isBridgeConnected()) {
                console.log('[Magicutor] Bridge health alarm: not connected — reconnecting');
                return connectBridge(bridgeConfig);
            }
            return undefined;
        });
    });
    chrome.alarms.create(BRIDGE_HEALTH_ALARM, {
        delayInMinutes: 0.5,   // first check in 30s
        periodInMinutes: 0.5   // every 30s (chrome.alarms production minimum)
    });
    console.log('[Magicutor] Bridge health alarm scheduled');
}

async function initialize() {
    console.log('[Magicutor] Extension initializing...');

    const endpointResolution = await refreshRuntimeEndpoints();
    console.log(`[Magicutor] Runtime endpoints resolved from ${endpointResolution.source}`);

    // Connect to WebSocket bridge (primary method for HTTP server communication),
    // unless the owner declined pairing for this browser.
    if (!(await bridgeDeclined())) {
        void connectBridge(createBridgeConfig());
    }

    // The WebSocket bridge is the only runtime channel. Magician/Magicutor are
    // expected to already be running.

    // Register listeners
    setupMessageListeners();
    setupLivePreviewMessaging();
    setupTabListeners({ attachDebugger });
    setupWindowListeners();
    setupDebuggerListeners();
    setupContextualAssistContextMenu();
    setupNotesCaptureContextMenu();

    await configureActionSidePanel();

    // Setup periodic cleanup of stale execution tracking
    setupExecutionCleanupAlarm();

    // Setup the bridge-health reconnect backstop (survives SW termination).
    setupBridgeHealthAlarm();

    // Tabs that were already open did not receive the call-banner script.
    void publishWhatsAppCallPrompts();

    console.log('[Magicutor] Extension initialized');
}

const FALLBACK_POPUP_PATH = 'popup.html';

async function setActionPopup(popupPath) {
    if (!chrome.action?.setPopup) {
        return;
    }

    try {
        await chrome.action.setPopup({ popup: popupPath });
    } catch (error) {
        console.warn('[Magicutor] Failed to update action popup:', error.message);
    }
}

async function configureActionSidePanel() {
    if (!chrome.sidePanel?.setPanelBehavior) {
        await setActionPopup(FALLBACK_POPUP_PATH);
        return;
    }

    try {
        await chrome.sidePanel.setPanelBehavior({
            openPanelOnActionClick: true
        });
        await setActionPopup('');
    } catch (error) {
        console.warn('[Magicutor] Failed to configure side panel action click:', error.message);
        await setActionPopup(FALLBACK_POPUP_PATH);
    }
}

chrome.runtime.onInstalled.addListener(() => {
    setupContextualAssistContextMenu();
    setupNotesCaptureContextMenu();
    void configureActionSidePanel();
});

chrome.runtime.onStartup.addListener(() => {
    // A new browser session reuses tab ids; no recorded debugger setup applies.
    void forgetAllSetup().catch(() => {});
    void forgetAllAutomationTabs().catch(() => {});
    void forgetAllGroups().catch(() => {});
    setupContextualAssistContextMenu();
    setupNotesCaptureContextMenu();
    void configureActionSidePanel();
});


/**
 * Handle request from bridge
 * Note: Server sends camelCase fields (requestId, not request_id)
 */
async function handleBridgeRequest(request) {
    const timestamp = () => new Date().toISOString();
    const logPrefix = `[Magicutor][${request.requestId}]`;

    // Fast-path: record_decision is fire-and-forget metadata, not a browser action
    if (request.action === 'record_decision' && request.params) {
        recordDecision(request.params);
        return { ok: true };
    }

    console.log(`${logPrefix} ${timestamp()} ▶ PROCESSING START: action=${request.action}`);
    if (request.params) {
        // Log params (sanitized for screenshots/large data)
        const safeParams = summarizeParams(request.params);
        console.log(`${logPrefix} ${timestamp()}   params:`, safeParams);
    }

    // Track action start
    recordActionStart(request.action, request.requestId, request.params);

    const startTime = Date.now();

    try {
        // Reuse the same action dispatcher as the CDP bridge.
        const result = await processAction(request.action, request.params);

        const elapsed = Date.now() - startTime;

        // Check if action returned an error object instead of throwing.
        if (result && result.error) {
            console.error(`${logPrefix} ${timestamp()} ✗ ACTION FAILED after ${elapsed}ms:`, result.error);

            recordActionComplete(request.requestId, false, result.error);

            // Send error response but INCLUDE impact data for verification
            await sendToBridge({
                requestId: request.requestId,
                success: false,
                error: result.error
            });
        } else {
            console.log(`${logPrefix} ${timestamp()} ✓ PROCESSING COMPLETE: ${elapsed}ms`);

            // Track success
            recordActionComplete(request.requestId, true);

            // Send response back over bridge (await to ensure buffer is flushed)
            // Use camelCase to match what server expects
            await sendToBridge({
                requestId: request.requestId,
                success: true,
                result
            });
        }
    } catch (error) {
        const elapsed = Date.now() - startTime;
        console.error(`${logPrefix} ${timestamp()} ✗ PROCESSING FAILED after ${elapsed}ms:`, error.message);

        // Track failure
        recordActionComplete(request.requestId, false, error.message);

        // Include impact from error if available (thrown errors may have impact attached)
        await sendToBridge({
            requestId: request.requestId,
            success: false,
            error: error.message
        });
    }
}


const WHATSAPP_OFFER_STORAGE_KEY = 'whatsAppCallOffer';

let whatsAppRingApplied = null;
let whatsAppRingWatching = false;
let whatsAppRingTimer = 0;
let whatsAppOffer = null;
let whatsAppOfferDismissed = false;
let whatsAppOfferEpoch = 0;
let whatsAppOfferReady = false;
let whatsAppBrowserBar = true;
let whatsAppBrowserBarReady = false;
let whatsAppBrowserBarGeneration = 0;

function whatsAppOfferMessage() {
    if (!whatsAppBrowserBar || !whatsAppOffer || whatsAppOfferDismissed) {
        return { type: 'whatsapp_call_offer', kind: 'hide', browserBar: whatsAppBrowserBar };
    }
    return {
        type: 'whatsapp_call_offer',
        kind: whatsAppOffer.kind,
        caller: whatsAppOffer.caller || '',
        url: whatsAppOffer.url || 'https://web.whatsapp.com/',
        title: whatsAppOffer.title || '',
        browserBar: true
    };
}

function persistWhatsAppBrowserBar() {
    const storage = chrome.storage && chrome.storage.local;
    if (!storage || typeof storage.set !== 'function') return;
    Promise.resolve(storage.set({ whatsAppBrowserBar })).catch(() => {});
}

async function restoreWhatsAppBrowserBar() {
    if (whatsAppBrowserBarReady) return;
    whatsAppBrowserBarReady = true;
    const generation = whatsAppBrowserBarGeneration;
    const storage = chrome.storage && chrome.storage.local;
    if (!storage || typeof storage.get !== 'function') return;
    let stored;
    try {
        stored = await storage.get('whatsAppBrowserBar');
    } catch (_) {
        return;
    }
    if (generation !== whatsAppBrowserBarGeneration) return;
    if (stored && stored.whatsAppBrowserBar === false) {
        whatsAppBrowserBar = false;
        await publishWhatsAppBrowserBar();
    }
}

async function publishWhatsAppBrowserBar() {
    const payload = { type: 'whatsapp_browser_bar', enabled: whatsAppBrowserBar };
    if (!chrome.tabs || typeof chrome.tabs.query !== 'function') return;
    const tabs = await chrome.tabs.query({});
    await Promise.all(tabs.map((tab) => {
        if (!tab.id) return Promise.resolve();
        const url = tab.url || '';
        const whatsApp = url.startsWith('https://web.whatsapp.com/');
        if (!whatsApp && !isWhatsAppPromptTab(url)) return Promise.resolve();
        return chrome.tabs.sendMessage(tab.id, payload).catch(() => {});
    }));
    if (whatsAppBrowserBar && whatsAppOffer && !whatsAppOfferDismissed) {
        await broadcastWhatsAppOffer(whatsAppOfferMessage());
    }
}

function persistWhatsAppOffer() {
    const storage = chrome.storage && chrome.storage.session;
    if (!storage || typeof storage.set !== 'function') return;
    Promise.resolve(storage.set({
        [WHATSAPP_OFFER_STORAGE_KEY]: {
            epoch: whatsAppOfferEpoch,
            offer: whatsAppOffer,
            dismissed: whatsAppOfferDismissed
        }
    })).catch(() => {});
}

async function restoreWhatsAppOffer() {
    if (whatsAppOfferReady) return;
    whatsAppOfferReady = true;
    const storage = chrome.storage && chrome.storage.session;
    if (!storage || typeof storage.get !== 'function') return;
    const epoch = whatsAppOfferEpoch;
    let stored;
    try {
        stored = await storage.get(WHATSAPP_OFFER_STORAGE_KEY);
    } catch (_) {
        return;
    }
    if (epoch !== whatsAppOfferEpoch) return;
    const record = stored && stored[WHATSAPP_OFFER_STORAGE_KEY];
    if (!record || !record.offer) return;
    whatsAppOffer = record.offer;
    whatsAppOfferDismissed = record.dismissed === true;
}

function sameWhatsAppOffer(kind, caller) {
    return Boolean(
        whatsAppOffer
        && !whatsAppOfferDismissed
        && whatsAppOffer.kind === kind
        && (whatsAppOffer.caller || '') === (caller || '')
    );
}

function whatsAppAttendanceMessage(message) {
    const kind = whatsAppOffer && whatsAppOffer.kind;
    return {
        decision: message && message.decision,
        url: (message && message.url) || (whatsAppOffer && whatsAppOffer.url) || 'https://web.whatsapp.com/',
        title: (message && message.title) || (whatsAppOffer && whatsAppOffer.title) || '',
        caller: (message && message.caller) || (whatsAppOffer && whatsAppOffer.caller) || '',
        surface: (message && message.surface) || (kind === 'ongoing' ? 'ongoing' : ''),
        profile: message && message.profile
    };
}

function noteWhatsAppRingDecision(message) {
    whatsAppOfferEpoch += 1;
    const decision = message && message.decision;
    if (decision === 'incoming' || decision === 'ongoing') {
        const kind = decision === 'ongoing' ? 'ongoing' : 'incoming';
        const first = !whatsAppOffer || whatsAppOffer.kind !== kind;
        whatsAppRingWatching = true;
        whatsAppRingApplied = null;
        if (first) whatsAppOfferDismissed = false;
        whatsAppOffer = {
            kind,
            caller: (message && message.caller) || (whatsAppOffer && whatsAppOffer.caller) || '',
            url: (message && message.url) || 'https://web.whatsapp.com/',
            title: (message && message.title) || ''
        };
        if (!whatsAppOfferDismissed) {
            broadcastWhatsAppOffer({
                type: 'whatsapp_call_offer',
                kind,
                caller: whatsAppOffer.caller,
                url: whatsAppOffer.url,
                title: whatsAppOffer.title
            }).catch(() => {});
        }
    } else if (decision === 'join' || decision === 'approved' || decision === 'listen') {
        whatsAppRingWatching = true;
        whatsAppRingApplied = decision === 'listen' ? 'listen' : 'join';
        whatsAppOfferDismissed = true;
        broadcastWhatsAppOffer({ type: 'whatsapp_call_offer', kind: 'hide' }).catch(() => {});
    } else if (decision === 'cancel') {
        whatsAppRingWatching = false;
        whatsAppRingApplied = 'cancel';
        whatsAppOfferDismissed = true;
        whatsAppOffer = null;
        broadcastWhatsAppOffer({ type: 'whatsapp_call_offer', kind: 'hide' }).catch(() => {});
    } else if (decision === 'ignored') {
        whatsAppRingWatching = false;
        whatsAppRingApplied = 'ignored';
        whatsAppOfferDismissed = true;
        broadcastWhatsAppOffer({ type: 'whatsapp_call_offer', kind: 'hide' }).catch(() => {});
    } else if (decision === 'ended' || decision === 'cleared') {
        whatsAppRingWatching = false;
        whatsAppRingApplied = null;
        whatsAppOfferDismissed = false;
        whatsAppOffer = null;
        broadcastWhatsAppOffer({ type: 'whatsapp_call_offer', kind: 'hide' }).catch(() => {});
    }
    persistWhatsAppOffer();
    ensureWhatsAppRingPoll();
}

async function ensureWhatsAppPrompt(tabId) {
    if (!chrome.scripting || typeof chrome.scripting.executeScript !== 'function') return;
    await chrome.scripting.executeScript({
        target: { tabId },
        files: ['whatsapp_call_watch.js', 'whatsapp_call_prompt.js']
    });
}

async function deliverWhatsAppOffer(tabId, payload) {
    try {
        await chrome.tabs.sendMessage(tabId, payload);
        return;
    } catch (_) {
        // This tab was already open, so the banner script is not in it yet.
    }
    try {
        await ensureWhatsAppPrompt(tabId);
        await chrome.tabs.sendMessage(tabId, payload);
    } catch (_) {
        // Chrome blocks scripting on some pages. The next navigation can ask.
    }
}

async function broadcastWhatsAppOffer(payload) {
    if (!chrome.tabs || typeof chrome.tabs.query !== 'function') return;
    const tabs = await chrome.tabs.query({});
    await Promise.all(tabs.map((tab) => {
        if (!tab.id || !isWhatsAppPromptTab(tab.url)) return Promise.resolve();
        return deliverWhatsAppOffer(tab.id, payload);
    }));
}

async function askWhatsAppTab(tabId) {
    try {
        return {
            response: await chrome.tabs.sendMessage(tabId, { type: 'whatsapp_call_snapshot' }),
            injected: false
        };
    } catch (_) {
        // The WhatsApp tab was open before this version loaded.
    }
    try {
        await chrome.scripting.executeScript({
            target: { tabId },
            files: ['whatsapp_call_watch.js', 'whatsapp_call_page.js']
        });
        return {
            response: await chrome.tabs.sendMessage(tabId, { type: 'whatsapp_call_snapshot' }),
            injected: true
        };
    } catch (_) {
        return { response: null, injected: false };
    }
}

async function snapshotWhatsAppCall() {
    if (!chrome.tabs || typeof chrome.tabs.query !== 'function') return 'none';
    const tabs = await chrome.tabs.query({ url: 'https://web.whatsapp.com/*' });
    for (const tab of tabs) {
        if (!tab.id) continue;
        const asked = await askWhatsAppTab(tab.id);
        const response = asked && asked.response;
        if (!response || typeof response.kind !== 'string') continue;
        if (response.kind === 'incoming' || response.kind === 'ongoing') {
            if (sameWhatsAppOffer(response.kind, response.caller || '')) return 'same';
            noteWhatsAppRingDecision({
                decision: response.kind,
                caller: response.caller || '',
                url: response.url || 'https://web.whatsapp.com/',
                title: response.title || ''
            });
            return 'fresh';
        }
        if (whatsAppOffer) noteWhatsAppRingDecision({ decision: 'cleared' });
        return 'clear';
    }
    return 'none';
}

async function publishWhatsAppCallPrompts() {
    try {
        await restoreWhatsAppBrowserBar();
        await restoreWhatsAppOffer();
        const snap = await snapshotWhatsAppCall();
        if (snap === 'none' && whatsAppOffer && !whatsAppOfferDismissed) {
            await broadcastWhatsAppOffer(whatsAppOfferMessage());
        }
    } catch (_) {
        // A tab can close while the offer is delivered. The next call asks again.
    }
}

if (chrome.tabs && chrome.tabs.onActivated) {
    chrome.tabs.onActivated.addListener((info) => {
        if (!whatsAppOffer || whatsAppOfferDismissed || !info || !info.tabId) return;
        chrome.tabs.get(info.tabId).then((tab) => {
            if (!tab || !isWhatsAppPromptTab(tab.url)) return;
            return deliverWhatsAppOffer(info.tabId, whatsAppOfferMessage());
        }).catch(() => {});
    });
}

if (chrome.tabs && chrome.tabs.onUpdated) {
    chrome.tabs.onUpdated.addListener((tabId, info, tab) => {
        if (!info || info.status !== 'complete') return;
        if (!whatsAppOffer || whatsAppOfferDismissed) return;
        if (!isWhatsAppPromptTab(tab && tab.url)) return;
        deliverWhatsAppOffer(tabId, whatsAppOfferMessage()).catch(() => {});
    });
}

function ensureWhatsAppRingPoll() {
    if (whatsAppRingTimer) return;
    whatsAppRingTimer = setInterval(() => {
        pollWhatsAppRingOnce().catch(() => {});
    }, 2000);
    pollWhatsAppRingOnce().catch(() => {});
}

function stopWhatsAppRingPoll() {
    if (!whatsAppRingTimer) return;
    clearInterval(whatsAppRingTimer);
    whatsAppRingTimer = 0;
}

async function applyWhatsAppRingDecision(decision) {
    const tabs = await chrome.tabs.query({ url: 'https://web.whatsapp.com/*' });
    await Promise.all(tabs.map(async (tab) => {
        if (!tab.id) return;
        const message = { type: 'whatsapp_call_apply', decision };
        try {
            await chrome.tabs.sendMessage(tab.id, message);
        } catch (_) {
            // The WhatsApp tab was open before this version loaded.
            try {
                await chrome.scripting.executeScript({
                    target: { tabId: tab.id },
                    files: ['whatsapp_call_watch.js', 'whatsapp_call_page.js']
                });
                await chrome.tabs.sendMessage(tab.id, message);
            } catch (_) {}
        }
    }));
}

async function pollWhatsAppRingOnce() {
    const view = await readWhatsAppCallRing({
        fetchImpl: magicianFetch,
        apiBase: MAGICIAN_API_BASE
    });
    const plan = planRingApply(whatsAppRingApplied, view && view.phase);
    whatsAppRingApplied = plan.applied;
    if (plan.apply) await applyWhatsAppRingDecision(plan.apply);
    if (!plan.keepPolling && !whatsAppRingWatching) stopWhatsAppRingPoll();
    return plan;
}

/**
 * Setup message listeners
 */
function setupMessageListeners() {
    chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
        // Route offscreen document frames and retry_capture before general handler
        if (handleLivePreviewMessage(message, sender, sendResponse)) {
            return true;
        }

        // WEG ambient-collector messages (`weg_*`) have their own dedicated
        // listener in `ambient_collector.js`. Do NOT claim the response channel
        // for them here: returning `true` below would route them through
        // `extensionMessageHandler` (which doesn't handle them), and that
        // response wins the race over the real handler's `{ ok: true }` — so the
        // pairing UI saw a false "Pairing failed" even though the token stored.
        // Bail so the dedicated handler is the sole responder.
        if (typeof message?.type === 'string' && message.type.startsWith('weg_')) {
            return false;
        }

        // WhatsApp ring prompts are answered in the page. Claim the channel
        // here so the generic handler does not swallow the response.
        if (message?.type === 'whatsapp_browser_bar_sync') {
            restoreWhatsAppBrowserBar()
                .then(() => sendResponse({ enabled: whatsAppBrowserBar }))
                .catch(() => sendResponse({ enabled: whatsAppBrowserBar }));
            return true;
        }

        if (message?.type === 'whatsapp_browser_bar') {
            whatsAppBrowserBarGeneration += 1;
            whatsAppBrowserBarReady = true;
            whatsAppBrowserBar = message.enabled !== false;
            persistWhatsAppBrowserBar();
            publishWhatsAppBrowserBar()
                .then(() => sendResponse({ ok: true, enabled: whatsAppBrowserBar }))
                .catch(() => sendResponse({ ok: false, enabled: whatsAppBrowserBar }));
            return true;
        }

        if (message?.type === 'whatsapp_call_sync') {
            restoreWhatsAppOffer()
                .then(() => sendResponse(whatsAppOfferMessage()))
                .catch(() => sendResponse({ type: 'whatsapp_call_offer', kind: 'hide' }));
            return true;
        }

        if (message?.type === 'whatsapp_call_choose') {
            const decision = message.decision;
            if (decision === 'ignore') {
                whatsAppOfferDismissed = true;
                broadcastWhatsAppOffer({ type: 'whatsapp_call_offer', kind: 'hide' })
                    .then(() => applyWhatsAppRingDecision('ignore'))
                    .then(() => sendResponse({ ok: true, stored: false }))
                    .catch(error => sendResponse({
                        ok: false,
                        stored: false,
                        error: error.message
                    }));
                return true;
            }
            if (decision === 'leave' || decision === 'ended') {
                const report = {
                    type: 'whatsapp_call_ring',
                    decision: 'ended',
                    url: (message && message.url) || 'https://web.whatsapp.com/',
                    title: (message && message.title) || '',
                    caller: (message && message.caller) || ''
                };
                reportWhatsAppCallRing(report, {
                    fetchImpl: magicianFetch,
                    apiBase: MAGICIAN_API_BASE
                }).then(async (result) => {
                    noteWhatsAppRingDecision(report);
                    await applyWhatsAppRingDecision('leave');
                    sendResponse(result);
                }).catch(async (error) => {
                    await applyWhatsAppRingDecision('leave');
                    sendResponse({
                        ok: false,
                        stored: false,
                        error: error.message
                    });
                });
                return true;
            }
            if (decision === 'join' || decision === 'listen' || decision === 'cancel') {
                const report = whatsAppAttendanceMessage(message);
                const post = decision === 'cancel' ? reportWhatsAppCallRing : reportWhatsAppAttendance;
                post(report, {
                    fetchImpl: magicianFetch,
                    apiBase: MAGICIAN_API_BASE
                }).then(async (result) => {
                    if (result && result.ok) noteWhatsAppRingDecision(report);
                    await applyWhatsAppRingDecision(decision);
                    sendResponse(result);
                }).catch(error => sendResponse({
                    ok: false,
                    stored: false,
                    error: error.message
                }));
                return true;
            }
        }

        if (message?.type === 'whatsapp_call_ring') {
            // Show the banner on other tabs even when the server post fails.
            const decision = message.decision;
            const attendance = decision === 'join' || decision === 'approved' || decision === 'listen';
            const report = attendance ? whatsAppAttendanceMessage(message) : message;
            if (
                decision === 'incoming' || decision === 'ongoing' || decision === 'join'
                || decision === 'approved' || decision === 'listen' || decision === 'cancel'
                || decision === 'ignored' || decision === 'ended' || decision === 'cleared'
            ) {
                noteWhatsAppRingDecision(message);
            }
            const post = attendance ? reportWhatsAppAttendance : reportWhatsAppCallRing;
            post(report, {
                fetchImpl: magicianFetch,
                apiBase: MAGICIAN_API_BASE,
                sender
            }).then(result => {
                sendResponse(result);
            }).catch(error => sendResponse({
                ok: false,
                stored: false,
                error: error.message
            }));
            return true;
        }

        if (message?.type === 'whatsapp_call_poll') {
            pollWhatsAppRingOnce()
                .then(plan => sendResponse({ apply: plan.apply }))
                .catch(error => sendResponse({ apply: null, error: error.message }));
            return true;
        }

        extensionMessageHandler(message, sender)
            .then(sendResponse)
            .catch(error => sendResponse({ error: error.message }));
        return true; // Keep channel open for async response
    });
}

/**
 * Process an action for the CDP bridge.
 */
async function processAction(action, params) {
    console.log(`[Magicutor][processAction] Routing action: ${action}`);
    return await processActionInner(action, params);
}

/**
 * CDP bridge action processor.
 */
async function processActionInner(action, params) {
    switch (action) {
        case 'ping':
            return { status: 'ok' };

        case 'create_window': {
            const sessionId = params.sessionId;
            if (!sessionId) {
                throw new Error('create_window requires sessionId');
            }
            clearSession(sessionId);

            const result = await createWindow(params);
            const primaryTabId = result.tabs?.[0]?.tabId;

            if (primaryTabId && !debuggerSessions.has(primaryTabId)) {
                try {
                    await attachDebugger({
                        tabId: primaryTabId,
                        windowId: result.windowId,
                        sessionId,
                        defaultDownloadPath: params.defaultDownloadPath
                    });
                } catch (e) {
                    console.warn(`[Magicutor] Failed to eagerly attach debugger to tab ${primaryTabId}: ${e.message}`);
                }
            }

            // A tab in the owner's window is a background tab: it stays there
            // only if, with focus emulation, the page reports visible and
            // draws a frame. Otherwise it moves to a window of its own now.
            if (result.surface === 'in_window' && primaryTabId) {
                const renders = debuggerSessions.has(primaryTabId)
                    && await rendersInBackground(cdpSenderForTab(primaryTabId));
                if (!renders) {
                    result.windowId = await moveSessionToOwnWindow(sessionId, [primaryTabId], 'background rendering check failed');
                    result.surface = 'own_window_group';
                    result.surfaceNote = 'background rendering check failed';
                }
            }

            if (sessionId && primaryTabId) {
                registerSessionTab(sessionId, primaryTabId, true);
                result.sessionContext = getTabSessionContext(primaryTabId);
                result.tabId = primaryTabId;
                console.log(`[SessionTabs] Registered window tab ${primaryTabId} as primary for session ${sessionId}`);

                const executionId = executionIdFromSessionId(sessionId);
                if (executionId) {
                    const trackedExecution = getActiveExecutions().get(executionId);
                    if (!trackedExecution?.tabId) {
                        trackActiveExecution(executionId, {
                            tabId: primaryTabId,
                            windowId: result.windowId
                        });

                        scheduleOverlayPanelShow(primaryTabId, {
                            executionId,
                            status: 'running',
                            text: 'Starting automation...'
                        });

                        pollExecutionStatus(executionId, primaryTabId);
                        console.log(
                            `[SessionTabs] Auto-tracked execution ${executionId}, showing panel and started polling for execution tab ${primaryTabId}`
                        );
                    }
                }
            }

            return result;
        }

        case 'create_tab': {
            const result = await createTab(params);
            registerSessionTab(params.sessionId, result.tabId);
            return result;
        }

        case 'close_session_tabs': {
            const { sessionId } = params;
            if (!sessionId) throw new Error('sessionId is required');
            const ownedTabs = [...await automationTabOwners()].filter(([, owner]) => owner === sessionId).map(([tabId]) => tabId);
            const sessionTabs = [...new Set([...getSessionTabs(sessionId), ...ownedTabs, ...(params.preserveTabs || [])])];
            const executionId = executionIdFromSessionId(sessionId);
            // Terminal cleanup is authoritative even if the status request
            // made during the CLI disconnect still saw the run as executing.
            sendOverlayMessageToTabs(sessionTabs, { action: 'overlay_panel_hide', executionId }).catch(() => {});
            sendOverlayMessageToTabs(sessionTabs, { action: 'overlay_hide' }).catch(() => {});
            if (executionId) untrackExecution(executionId);
            // Handed-off tabs leave the task's group; closed ones take it along.
            await releaseSessionGroup(sessionId, { ungroup: Boolean(params.keepTabs) })
                .catch(error => console.warn(`[Magicutor] Could not release tab group of ${sessionId}: ${error.message}`));
            const result = params.keepTabs
                ? await handoffAutomationTabs(sessionId)
                : await closeAutomationTabs(sessionId, params.preserveTabs || []);
            // Existing owner tabs remain open, but no debugger/session stays attached.
            for (const tabId of sessionTabs) {
                const owner = getSessionForTab(tabId);
                if (!owner || owner === sessionId) await detachDebugger({ tabId, sessionId });
            }
            clearSession(sessionId);
            return { ...result, cleared: true };
        }

        case 'close_window':
            return await closeWindow(params);

        case 'close_tab':
            return await closeTab(params);

        case 'list_tabs':
            return await listTabs(params);

        case 'probe_contextual_assist':
            return await probeContextualAssist(params);

        case 'capture_contextual_assist_tab':
            return await captureContextualAssistTab(params);

        case 'session_ended': {
            const { sessionId } = params;
            if (!sessionId) {
                throw new Error('sessionId is required');
            }
            return await endAutomationSession(sessionId);
        }

        case 'clear_session': {
            const { sessionId } = params;

            if (!sessionId) {
                throw new Error('sessionId is required');
            }

            clearSession(sessionId);

            console.log(`[SessionTabs] Cleared session ${sessionId}`);

            return { cleared: true, sessionId };
        }

        case 'attach_tab': {
            const tabId = params.tabId;
            const sessionId = params.sessionId;

            if (!tabId) {
                throw new Error('tabId is required');
            }
            if (!sessionId) {
                throw new Error('attach_tab requires sessionId');
            }

            const tab = await chrome.tabs.get(tabId);
            if (!tab || !tab.id) {
                throw new Error('Tab not found');
            }

            if (!tab.url || !/^https?:/i.test(tab.url)) {
                throw new Error(`Unsupported tab URL: ${tab.url || 'unknown'}`);
            }

            try {
                await chrome.windows.update(tab.windowId, { focused: true });
            } catch (_) {}

            try {
                await chrome.tabs.update(tabId, { active: true });
            } catch (_) {}

            clearSession(sessionId);

            if (debuggerSessions.has(tabId)) {
                if (sessionId) {
                    registerSessionTab(sessionId, tabId, true);
                }
            } else {
                await attachDebugger({
                    tabId,
                    windowId: tab.windowId,
                    sessionId,
                    defaultDownloadPath: params.defaultDownloadPath
                });
            }

            const result = {
                windowId: tab.windowId,
                tabId,
                url: tab.url,
                title: tab.title || null
            };

            if (sessionId) {
                result.sessionContext = getTabSessionContext(tabId);
                console.log(`[SessionTabs] Attached tab ${tabId} as primary for session ${sessionId}`);

                const executionId = executionIdFromSessionId(sessionId);
                if (executionId) {
                    const trackedExecution = getActiveExecutions().get(executionId);
                    if (trackedExecution && !trackedExecution.tabId) {
                        trackActiveExecution(executionId, {
                            tabId,
                            windowId: tab.windowId
                        });
                        scheduleOverlayPanelShow(tabId, {
                            executionId,
                            status: 'running',
                            text: 'Starting automation...'
                        });
                        pollExecutionStatus(executionId, tabId);
                    } else if (trackedExecution) {
                        scheduleOverlayPanelShow(tabId, {
                            executionId,
                            status: 'running',
                            text: 'Running...'
                        });
                    }
                }
            }

            return result;
        }

        case 'attach_debugger':
            return await attachDebugger(params);

        case 'detach_debugger':
            return await detachDebugger(params);

        case 'debugger_command': {
            const overlayDescriptor = await showCdpCommandOverlayBefore(params);
            try {
                const result = await sendDebuggerCommand(params);
                if (overlayDescriptor?.complete) {
                    await showOverlayComplete(
                        params.tabId,
                        true,
                        overlayDescriptor.type,
                        overlayDescriptor.actionParams
                    );
                }
                return result;
            } catch (error) {
                if (overlayDescriptor) {
                    await showOverlayError(params.tabId, error.message);
                }
                throw error;
            }
        }

        case 'api_replay':
            return await apiReplay(params);

        default:
            throw new Error(`Unknown action: ${action}`);
    }
}

async function apiReplay(params = {}) {
    const tabId = params.tabId;
    if (!tabId) {
        throw new Error('api_replay requires tabId');
    }

    const capabilityId = params.capabilityId ?? '';
    const method = (params.method ?? 'GET').toUpperCase();
    const url = params.url;
    const headers = params.headers ?? {};
    const body = typeof params.body === 'string' ? params.body : null;
    const MAX_REPLAY_TIMEOUT_MS = 30000;
    const MAX_REPLAY_BODY_BYTES = 256 * 1024;
    const timeoutMs = Math.min(params.timeoutMs ?? 10000, MAX_REPLAY_TIMEOUT_MS);

    if (!url) {
        throw new Error('api_replay requires url');
    }

    try {
        const urlObj = new URL(url);
        if (urlObj.protocol !== 'http:' && urlObj.protocol !== 'https:') {
            return {
                success: false,
                capabilityId,
                error: `api_replay only supports http/https URLs, got: ${urlObj.protocol}`
            };
        }

        const hostname = urlObj.hostname.toLowerCase();
        const bareHost = hostname.replace(/^\[|\]$/g, '');

        const isPrivateIPv4 = (ip) => {
            if (ip === 'localhost' || ip === '0.0.0.0') return true;
            const parts = ip.split('.');
            if (parts.length !== 4) return false;
            const octets = parts.map(Number);
            if (octets.some(o => isNaN(o) || o < 0 || o > 255)) return false;
            const [a, b] = octets;
            return (
                a === 127 ||
                a === 0 ||
                a === 10 ||
                (a === 172 && b >= 16 && b <= 31) ||
                (a === 192 && b === 168) ||
                (a === 169 && b === 254) ||
                (a === 100 && b >= 64 && b <= 127) ||
                (a === 198 && (b === 18 || b === 19))
            );
        };

        const isPrivateIPv6 = (ip) => {
            const normalized = ip.replace(/^\[|\]$/g, '').toLowerCase();
            return (
                normalized === '::1' ||
                normalized.match(/^0*:0*:0*:0*:0*:0*:0*:0*1$/) ||
                normalized.startsWith('fc') ||
                normalized.startsWith('fd') ||
                normalized.startsWith('fe80') ||
                normalized.match(/^::ffff:[0-9a-f]{1,4}:[0-9a-f]{1,4}$/)
            );
        };

        const v4MappedMatch = bareHost.match(/^::ffff:(\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3})$/);
        const effectiveHost = v4MappedMatch ? v4MappedMatch[1] : bareHost;

        const v4HexMatch = bareHost.match(/^::ffff:([0-9a-f]{1,4}):([0-9a-f]{1,4})$/);
        let v4FromHex = null;
        if (v4HexMatch) {
            const hi = parseInt(v4HexMatch[1], 16);
            const lo = parseInt(v4HexMatch[2], 16);
            v4FromHex = `${(hi >> 8) & 0xff}.${hi & 0xff}.${(lo >> 8) & 0xff}.${lo & 0xff}`;
        }

        const isDangerousHostname = (h) => {
            return h === 'localhost' ||
                h.endsWith('.local') ||
                h.endsWith('.localhost') ||
                h.endsWith('.internal') ||
                h.endsWith('.arpa');
        };

        if (isPrivateIPv4(effectiveHost) ||
            (v4FromHex && isPrivateIPv4(v4FromHex)) ||
            isPrivateIPv6(bareHost) ||
            isDangerousHostname(bareHost)) {
            return {
                success: false,
                capabilityId,
                error: `api_replay blocked private/internal network address (SSRF protection): ${bareHost}`
            };
        }
    } catch (e) {
        return {
            success: false,
            capabilityId,
            error: `Invalid URL for api_replay: ${e.message}`
        };
    }

    const allowedMethods = new Set(['GET', 'HEAD', 'POST', 'PUT', 'PATCH', 'DELETE', 'OPTIONS']);
    if (!allowedMethods.has(method)) {
        return {
            success: false,
            capabilityId,
            error: `api_replay only supports GET/HEAD/POST/PUT/PATCH/DELETE/OPTIONS, got: ${method}`
        };
    }

    if ((method === 'GET' || method === 'HEAD') && body !== null) {
        return {
            success: false,
            capabilityId,
            error: `api_replay does not allow a request body for ${method}`
        };
    }

    if (body !== null) {
        const bodyBytes = new TextEncoder().encode(body).length;
        if (bodyBytes > MAX_REPLAY_BODY_BYTES) {
            return {
                success: false,
                capabilityId,
                error: `api_replay request body exceeds ${MAX_REPLAY_BODY_BYTES} bytes`
            };
        }
    }

    const startTime = Date.now();

    try {
        if (!chrome.scripting?.executeScript) {
            return {
                success: false,
                capabilityId,
                status: 0,
                timingMs: Date.now() - startTime,
                error: 'chrome.scripting.executeScript is unavailable'
            };
        }

        const results = await chrome.scripting.executeScript({
            target: { tabId },
            world: 'MAIN',
            func: async (fetchUrl, fetchMethod, fetchHeaders, fetchBody, fetchTimeout) => {
                const controller = new AbortController();
                const timer = setTimeout(() => controller.abort(), fetchTimeout);

                try {
                    const fetchOptions = {
                        method: fetchMethod,
                        headers: fetchHeaders,
                        credentials: 'include',
                        redirect: 'manual',
                        signal: controller.signal,
                    };
                    if (fetchBody !== null && fetchMethod !== 'GET' && fetchMethod !== 'HEAD') {
                        fetchOptions.body = fetchBody;
                    }
                    const resp = await fetch(fetchUrl, fetchOptions);

                    clearTimeout(timer);

                    if (resp.status >= 300 && resp.status < 400) {
                        return {
                            success: false,
                            status: resp.status,
                            error: `Redirect detected (${resp.status}) - api_replay does not follow redirects for security`,
                        };
                    }

                    const respHeaders = {};
                    resp.headers.forEach((v, k) => { respHeaders[k] = v; });

                    const maxBody = 256 * 1024;
                    let bodyText = '';
                    let bodyTruncated = false;

                    if (resp.body) {
                        const reader = resp.body.getReader();
                        const decoder = new TextDecoder();
                        try {
                            while (true) {
                                const { done, value } = await reader.read();
                                if (done) break;
                                bodyText += decoder.decode(value, { stream: true });
                                if (bodyText.length > maxBody) {
                                    bodyTruncated = true;
                                    bodyText = bodyText.substring(0, maxBody);
                                    reader.cancel();
                                    break;
                                }
                            }
                            if (!bodyTruncated) {
                                bodyText += decoder.decode(new Uint8Array(), { stream: false });
                            }
                        } catch (streamErr) {
                            if (!bodyText) bodyText = `[Stream error: ${streamErr.message}]`;
                        }
                    }

                    return {
                        success: true,
                        status: resp.status,
                        headers: respHeaders,
                        body: bodyText,
                        bodyTruncated,
                    };
                } catch (e) {
                    clearTimeout(timer);
                    return {
                        success: false,
                        status: 0,
                        error: e.name === 'AbortError'
                            ? `Request timed out after ${fetchTimeout}ms`
                            : e.message,
                    };
                }
            },
            args: [url, method, headers, body, timeoutMs],
        });

        const elapsed = Date.now() - startTime;

        if (!results || results.length === 0) {
            return {
                success: false,
                capabilityId,
                status: 0,
                timingMs: elapsed,
                error: 'Extension script injection failed (tab may be closed or CSP-blocked)',
            };
        }

        if (results[0].error) {
            return {
                success: false,
                capabilityId,
                status: 0,
                timingMs: elapsed,
                error: `Script execution error: ${results[0].error}`,
            };
        }

        const fetchResult = results[0].result;
        if (!fetchResult) {
            return {
                success: false,
                capabilityId,
                status: 0,
                timingMs: elapsed,
                error: 'Extension fetch returned no result',
            };
        }

        return {
            success: fetchResult.success,
            capabilityId,
            status: fetchResult.status || 0,
            headers: fetchResult.headers || {},
            body: fetchResult.body ?? null,
            bodyTruncated: fetchResult.bodyTruncated || false,
            timingMs: elapsed,
            error: fetchResult.error ?? null,
        };
    } catch (e) {
        const elapsed = Date.now() - startTime;
        console.error(`[Magicutor][api_replay] Error: ${e.message}`);
        return {
            success: false,
            capabilityId,
            status: 0,
            timingMs: elapsed,
            error: e.message,
        };
    }
}

async function checkHttpHealth() {
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), 1500);
    try {
        const resp = await fetch(`${MAGICUTOR_API_BASE}/health`, {
            method: 'GET',
            cache: 'no-store',
            signal: controller.signal
        });
        clearTimeout(timeout);
        return resp.ok;
    } catch (error) {
        clearTimeout(timeout);
        return false;
    }
}

// Initialize on load
initialize();
