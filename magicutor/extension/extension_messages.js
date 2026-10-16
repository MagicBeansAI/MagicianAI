import {
    MAGICIAN_API_BASE,
    MAGICIAN_API_TIMEOUT_MS,
    MAGICIAN_HEALTH_URL,
    MAGICUTOR_API_BASE,
    MAGICUTOR_API_TIMEOUT_MS
} from './config.js';
import {
    browserThreadIdForExecution,
    executionIdFromSessionId
} from './automation_session.js';
import { magicianFetch } from './magician_scope.js';
import {
    getSessionForTab,
    getPopupState,
    setPopupState,
    clearPopupState,
    trackActiveExecution,
    untrackExecution,
    getVisualOverlayState,
    setVisualOverlayState,
    getSessionTabs,
    getSessionPrimaryTab,
    registerSessionTab,
    getActiveExecutions,
    clearSession
} from './background_state.js';
import { getLivePreviewSources } from './live_preview_background.js';
import {
    buildTerminalOverlayStatus,
    isPausedExecutionStatus,
    isSuccessExecutionStatus,
    isTerminalExecutionStatus,
    isWaitingChildrenStatus,
    normalizeExecutionStatus
} from './execution_status.js';

function buildInitialMessage({ goal, tabUrl, tabTitle, includeTabUrl }) {
    const trimmedGoal = (goal || '').trim();
    if (!includeTabUrl || !tabUrl) {
        return trimmedGoal;
    }

    const titleSuffix = tabTitle ? ` (${tabTitle})` : '';
    return [
        `You are already on this page${titleSuffix}: ${tabUrl}`,
        '',
        `Goal: ${trimmedGoal}`
    ].join('\n');
}

async function createExecution({ title }) {
    // Extension "Do It" automation is user-initiated but internal-scoped:
    // mark the V3 task Internal so it lands in /internal-tasks rather than
    // cluttering the user-facing /tasks list.
    const payload = { title, internal: true };

    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), MAGICIAN_API_TIMEOUT_MS);

    try {
        const response = await magicianFetch(`${MAGICIAN_API_BASE}/executions`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(payload),
            signal: controller.signal
        });

        if (!response.ok) {
            const errorText = await response.text().catch(() => '');
            const detail = errorText ? `: ${errorText.slice(0, 240)}` : '';
            throw new Error(`Magician API error (${response.status})${detail}`);
        }

        const data = await response.json().catch(() => ({}));
        const execution = data.execution || data;
        const executionId = data.execution_id || execution.id || data.id || null;

        return { status: 'ok', executionId, execution };
    } finally {
        clearTimeout(timeout);
    }
}

function extractPauseInputType(pausePayload) {
    const inputType =
        pausePayload?.active_pause_state?.input_type
        || pausePayload?.input_type
        || pausePayload?.pause_state?.input_type
        || null;

    if (typeof inputType === 'string') {
        return inputType.toLowerCase();
    }

    if (inputType && typeof inputType === 'object') {
        if (typeof inputType.type === 'string') {
            return inputType.type.toLowerCase();
        }
        if (typeof inputType.input_type === 'string') {
            return inputType.input_type.toLowerCase();
        }
    }

    return null;
}

async function fetchExecutionSnapshot(executionId, controller) {
    const response = await magicianFetch(`${MAGICIAN_API_BASE}/executions/${executionId}`, {
        method: 'GET',
        headers: { 'Content-Type': 'application/json' },
        signal: controller.signal
    });

    if (!response.ok) {
        const errorText = await response.text().catch(() => '');
        throw new Error(`Execution lookup failed (${response.status}): ${errorText.slice(0, 240)}`);
    }

    return await response.json();
}

async function fetchPauseStateSnapshot(executionId, controller) {
    const response = await magicianFetch(`${MAGICIAN_API_BASE}/executions/${executionId}/pause-state`, {
        method: 'GET',
        headers: { 'Content-Type': 'application/json' },
        signal: controller.signal
    });

    if (!response.ok) {
        const errorText = await response.text().catch(() => '');
        throw new Error(`Pause lookup failed (${response.status}): ${errorText.slice(0, 240)}`);
    }

    return await response.json();
}

async function postExecutionControl(executionId, controlAction) {
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), MAGICIAN_API_TIMEOUT_MS);

    try {
        const execution = await fetchExecutionSnapshot(executionId, controller);
        const status = normalizeExecutionStatus(execution);

        let endpoint = `${MAGICIAN_API_BASE}/executions/${executionId}/execution/agentic-continue`;
        let body = {};

        if (controlAction === 'resume') {
            if (status === 'waitinguser' || status === 'waiting_user') {
                const pauseState = await fetchPauseStateSnapshot(executionId, controller);
                const pauseInputType = extractPauseInputType(pauseState);
                if (pauseInputType !== 'external_action') {
                    throw new Error('This execution is waiting for user input that must be provided in the Magician web UI.');
                }
                endpoint = `${MAGICIAN_API_BASE}/executions/${executionId}/execution/agentic-resume`;
                body = {
                    input_type: 'external_action',
                    value: { type: 'external_action_completed' }
                };
            } else if (status === 'waitingchildren' || status === 'waiting_children') {
                throw new Error('Execution is waiting on delegated work and cannot be resumed manually.');
            }
        } else if (controlAction === 'cancel') {
            if (status === 'waitinguser' || status === 'waiting_user' || status === 'paused') {
                endpoint = `${MAGICIAN_API_BASE}/executions/${executionId}/execution/agentic-cancel`;
            } else {
                endpoint = `${MAGICIAN_API_BASE}/executions/${executionId}/cancel`;
            }
        } else {
            throw new Error(`Unknown execution control action: ${controlAction}`);
        }

        const sendControlRequest = (target, payload) => magicianFetch(target, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(payload),
            signal: controller.signal
        });

        let response = await sendControlRequest(endpoint, body);
        if (
            !response.ok &&
            controlAction === 'cancel' &&
            endpoint.endsWith('/execution/agentic-cancel')
        ) {
            response = await sendControlRequest(
                `${MAGICIAN_API_BASE}/executions/${executionId}/cancel`,
                {}
            );
        }

        if (!response.ok) {
            const errorText = await response.text().catch(() => '');
            const actionLabel = controlAction === 'resume' ? 'Resume' : 'Cancel';
            throw new Error(`${actionLabel} failed (${response.status}): ${errorText.slice(0, 240)}`);
        }

        return await response.json().catch(() => ({ status: 'ok' }));
    } finally {
        clearTimeout(timeout);
    }
}

async function checkMagicianHealth() {
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), 1500);

    try {
        const response = await fetch(MAGICIAN_HEALTH_URL, {
            method: 'GET',
            cache: 'no-store',
            signal: controller.signal
        });
        if (!response.ok) {
            return { ok: false, version: null };
        }
        const data = await response.json().catch(() => ({}));
        return { ok: true, version: data.version || null };
    } catch (error) {
        return { ok: false, version: null };
    } finally {
        clearTimeout(timeout);
    }
}

async function checkMagicutorHealth() {
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), 1500);

    try {
        const response = await fetch(`${MAGICUTOR_API_BASE}/health`, {
            method: 'GET',
            cache: 'no-store',
            signal: controller.signal
        });
        if (!response.ok) {
            return { ok: false, version: null };
        }
        const data = await response.json().catch(() => ({}));
        return { ok: true, version: data.version || null };
    } catch (error) {
        return { ok: false, version: null };
    } finally {
        clearTimeout(timeout);
    }
}

async function startExecution({ executionId, goal, maxIterations }) {
    const payload = { goal };
    if (maxIterations) {
        payload.max_iterations = maxIterations;
    }

    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), MAGICIAN_API_TIMEOUT_MS);

    try {
        const response = await magicianFetch(`${MAGICIAN_API_BASE}/executions/${executionId}/start`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(payload),
            signal: controller.signal
        });

        if (!response.ok) {
            const errorText = await response.text().catch(() => '');
            const detail = errorText ? `: ${errorText.slice(0, 240)}` : '';
            throw new Error(`Magician API error (${response.status})${detail}`);
        }

        return await response.json().catch(() => ({}));
    } finally {
        clearTimeout(timeout);
    }
}

async function cancelExecution({ executionId }) {
    return await postExecutionControl(executionId, 'cancel');
}

async function clearCdpThread({ threadId }) {
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), MAGICUTOR_API_TIMEOUT_MS);

    try {
        const response = await fetch(`${MAGICUTOR_API_BASE}/cdp/threads/${encodeURIComponent(threadId)}`, {
            method: 'DELETE',
            headers: { 'Content-Type': 'application/json' },
            signal: controller.signal
        });

        if (!response.ok) {
            const errorText = await response.text().catch(() => '');
            const detail = errorText ? `: ${errorText.slice(0, 240)}` : '';
            throw new Error(`Failed to clear CDP thread (${response.status})${detail}`);
        }

        return await response.json().catch(() => ({ cleared: true }));
    } finally {
        clearTimeout(timeout);
    }
}

// Poll execution status and update panel when completed
function pollExecutionStatus(executionId, tabId) {
    const POLL_INTERVAL = 3000; // 3 seconds
    const MAX_POLLS = 600; // 30 minutes max
    let pollCount = 0;
    const resolveTargetTabs = (sessionAlias) => {
        const sessionTabs = getSessionTabs(sessionAlias);
        if (sessionTabs && sessionTabs.size > 0) {
            return [...sessionTabs];
        }
        return tabId ? [tabId] : [];
    };

    const poll = async () => {
        pollCount++;
        if (pollCount > MAX_POLLS) {
            console.log(`[PollExecution] Max polls reached for ${executionId}`);
            return;
        }

        try {
            const response = await magicianFetch(`${MAGICIAN_API_BASE}/executions/${executionId}`, {
                signal: AbortSignal.timeout(5000)
            });

            if (!response.ok) {
                console.log(
                    `[PollExecution] Execution ${executionId} not found (${response.status}), stopping poll`
                );
                const sessionAlias = browserThreadIdForExecution(executionId);
                const targetTabs = resolveTargetTabs(sessionAlias);
                for (const tid of targetTabs) {
                    try {
                        await chrome.tabs.sendMessage(tid, {
                            action: 'overlay_panel_hide',
                            executionId
                        });
                    } catch (_) {
                        // Tab might not have content script
                    }
                }
                untrackExecution(executionId);
                clearSession(sessionAlias);
                return;
            }

            const execution = await response.json();
            const status = normalizeExecutionStatus(execution);

            console.log(
                `[PollExecution] Execution ${executionId} status: "${status}" (poll #${pollCount})`
            );

            // Detect paused state (waiting for user input at max iterations or escalation)
            if (isPausedExecutionStatus(status)) {
                const sessionAlias = browserThreadIdForExecution(executionId);
                const targetTabs = resolveTargetTabs(sessionAlias);
                let pauseInputType = null;
                if (status === 'waitinguser' || status === 'waiting_user') {
                    try {
                        pauseInputType = extractPauseInputType(
                            await fetchPauseStateSnapshot(executionId, {
                                signal: AbortSignal.timeout(5000)
                            })
                        );
                    } catch (_) {
                        pauseInputType = null;
                    }
                }

                // Distinguish escalation pauses from generic paused executions.
                if (execution.escalation_trigger) {
                    console.log(
                        `[PollExecution] Execution ${executionId} escalation pause: ${execution.escalation_trigger}`
                    );
                    for (const tid of targetTabs) {
                        try {
                            await chrome.tabs.sendMessage(tid, {
                                action: 'overlay_status',
                                executionId: executionId,
                                status: 'escalation',
                                text: execution.escalation_trigger === 'loop_detected'
                                    ? 'Agent stuck in loop — help needed'
                                    : 'Agent needs help'
                            });
                        } catch (e) {
                            // Tab might not have content script
                        }
                    }
                } else {
                    console.log(
                        `[PollExecution] Execution ${executionId} paused`
                    );
                    for (const tid of targetTabs) {
                        try {
                            await chrome.tabs.sendMessage(tid, {
                                action: 'overlay_status',
                                executionId: executionId,
                                status: (status === 'waitinguser' || status === 'waiting_user')
                                    && pauseInputType !== 'external_action'
                                    ? 'waiting_user_input'
                                    : 'paused',
                                text: (status === 'waitinguser' || status === 'waiting_user')
                                    && pauseInputType !== 'external_action'
                                    ? 'Input needed in Magician'
                                    : 'Execution paused'
                            });
                        } catch (e) {
                            // Tab might not have content script
                        }
                    }
                }

                // Continue polling — execution is not terminal, user may resume
                setTimeout(poll, POLL_INTERVAL);
                return;
            }

            if (isWaitingChildrenStatus(status)) {
                const sessionAlias = browserThreadIdForExecution(executionId);
                const targetTabs = resolveTargetTabs(sessionAlias);
                for (const tid of targetTabs) {
                    try {
                        await chrome.tabs.sendMessage(tid, {
                            action: 'overlay_status',
                            executionId: executionId,
                            status: 'waiting_children',
                            text: 'Waiting on delegated work...'
                        });
                    } catch (e) {
                        // Tab might not have content script
                    }
                }
                setTimeout(poll, POLL_INTERVAL);
                return;
            }

            if (isTerminalExecutionStatus(status)) {
                const isSuccess = isSuccessExecutionStatus(status);
                const terminalOverlay = buildTerminalOverlayStatus(status);
                console.log(
                    `[PollExecution] Execution ${executionId} terminal: ${status} (success=${isSuccess})`
                );
                const sessionAlias = browserThreadIdForExecution(executionId);
                const targetTabs = resolveTargetTabs(sessionAlias);
                untrackExecution(executionId);

                // Clear session tracking.
                clearSession(sessionAlias);

                // Send panel update to all tabs that were part of this session
                for (const tid of targetTabs) {
                    try {
                        await chrome.tabs.sendMessage(tid, {
                            action: 'overlay_status',
                            executionId: executionId,
                            status: terminalOverlay.status,
                            text: terminalOverlay.text
                        });
                        console.log(`[PollExecution] Sent overlay_status to tab ${tid}`);
                    } catch (e) {
                        console.warn(`[PollExecution] Failed to send to tab ${tid}:`, e.message);
                    }
                }
                return;
            }

            // For non-terminal, non-paused: send running status on first few polls
            // to ensure overlay shows even if the initial overlay_panel_show was missed
            if (pollCount <= 3) {
                const sessionAlias = browserThreadIdForExecution(executionId);
                const targetTabs = resolveTargetTabs(sessionAlias);
                for (const tid of targetTabs) {
                    try {
                        await chrome.tabs.sendMessage(tid, {
                            action: 'overlay_status',
                            executionId: executionId,
                            status: 'running',
                            text: `Running...`
                        });
                    } catch (e) { /* tab might not have content script */ }
                }
            }

            // Continue polling
            setTimeout(poll, POLL_INTERVAL);
        } catch (e) {
            console.warn(`[PollExecution] Error polling ${executionId}:`, e.message);
            setTimeout(poll, POLL_INTERVAL);
        }
    };

    // Start polling after a short delay
    console.log(`[PollExecution] Starting poll for execution ${executionId}, tab ${tabId}`);
    setTimeout(poll, POLL_INTERVAL);
}

async function bindThreadToTab({ threadId, tabId, windowId }) {
    const payload = {
        tabId,
    };

    if (windowId) {
        payload.windowId = windowId;
    }

    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), MAGICUTOR_API_TIMEOUT_MS);

    try {
        const response = await fetch(`${MAGICUTOR_API_BASE}/cdp/threads/${encodeURIComponent(threadId)}/bind-tab`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(payload),
            signal: controller.signal
        });

        if (!response.ok) {
            const errorText = await response.text().catch(() => '');
            const detail = errorText ? `: ${errorText.slice(0, 240)}` : '';
            throw new Error(`Magicutor CDP bind error (${response.status})${detail}`);
        }

        return await response.json().catch(() => ({}));
    } finally {
        clearTimeout(timeout);
    }
}

function createExtensionMessageHandler({ isBridgeConnected, checkHttpHealth, activityState, recordDecision }) {
    if (typeof isBridgeConnected !== 'function' || typeof checkHttpHealth !== 'function') {
        throw new Error('createExtensionMessageHandler requires isBridgeConnected and checkHttpHealth');
    }

    return async function handleExtensionMessage(message, sender) {
        console.log('[Magicutor] Extension message:', message);

        switch (message.action) {
            case 'ping': {
                const bridgeOk = isBridgeConnected();
                const magicutorHealth = await checkMagicutorHealth();
                const magicianHealth = await checkMagicianHealth();
                return {
                    status: 'ok',
                    connected: bridgeOk && magicutorHealth.ok && magicianHealth.ok,
                    bridge: bridgeOk,
                    http: magicutorHealth.ok,
                    magician: magicianHealth.ok,
                    versions: {
                        magicutor: magicutorHealth.version,
                        magician: magicianHealth.version,
                        extension: chrome.runtime.getManifest().version
                    }
                };
            }

            case 'get_activity': {
                return {
                    current: activityState.current,
                    recentActions: activityState.recentActions,
                    recentDecisions: activityState.recentDecisions,
                    stats: activityState.stats
                };
            }

            case 'record_decision': {
                // Record an LLM decision for popup display
                if (message.decision && typeof recordDecision === 'function') {
                    recordDecision(message.decision);
                }
                return { ok: true };
            }

            case 'get_tab_session': {
                // Get the session ID for a tab (used by popup to detect spawned windows)
                const tabId = message.tabId;
                if (!tabId) {
                    return { sessionId: null };
                }
                const sessionId = getSessionForTab(tabId);
                return { sessionId };
            }

            case 'get_active_executions': {
                // Return all tracked active executions (used by popup as fallback
                // when current tab has no session, e.g. debug-page-initiated flows)
                const executions = getActiveExecutions();
                const result = [];
                for (const [executionId, data] of executions) {
                    result.push({ executionId, tabId: data.tabId, windowId: data.windowId });
                }
                return { executions: result };
            }

            case 'get_live_preview_sources': {
                const sources = await getLivePreviewSources();
                return { sources };
            }

            case 'register_debug_execution': {
                // Register an execution started by the debug page (bypassed start_automation).
                // Keep enough state for dashboards to discover the execution, but do NOT bind
                // the current debug tab to the automation session. The real automation window
                // should become the primary session tab when Magicutor creates it.
                const executionId = message.executionId;
                const tabId = sender?.tab?.id;
                if (!executionId || !tabId) {
                    return { ok: false, error: 'Missing executionId or tabId' };
                }

                // Skip if already tracked (idempotent)
                if (getActiveExecutions().has(executionId)) {
                    return { ok: true, already_tracked: true };
                }
                // Track as pending only. create_window/create_tab will upgrade this execution
                // to the real automation tab once the browser session is created.
                trackActiveExecution(executionId, {
                    windowId: null,
                    tabId: null
                });

                console.log(
                    `[ExtMsg] Registered debug-page execution ${executionId} without binding current tab ${tabId}`
                );
                return { ok: true, pending_binding: true };
            }

            case 'get_popup_state': {
                // Get popup input state for a window (in-memory)
                const windowId = message.windowId;
                if (!windowId) {
                    return { goal: '', includeUrl: true, useThisTab: false };
                }
                return getPopupState(windowId);
            }

            case 'set_popup_state': {
                // Set popup input state for a window (in-memory)
                const windowId = message.windowId;
                if (!windowId) {
                    return { ok: false };
                }
                const { goal, includeUrl, useThisTab } = message;
                const update = {};
                if (goal !== undefined) update.goal = goal;
                if (includeUrl !== undefined) update.includeUrl = includeUrl;
                if (useThisTab !== undefined) update.useThisTab = useThisTab;
                setPopupState(windowId, update);
                return { ok: true };
            }

            case 'clear_popup_state': {
                // Clear popup state for a window (e.g., after automation starts)
                const windowId = message.windowId;
                if (windowId) {
                    clearPopupState(windowId);
                }
                return { ok: true };
            }

            case 'get_visual_overlay': {
                const sessionId = message.sessionId;
                const enabled = getVisualOverlayState(sessionId);
                return { enabled };
            }

            case 'check_overlay_state': {
                // For content scripts to pull state on init
                const tabId = sender?.tab?.id;
                const sessionId = tabId ? getSessionForTab(tabId) : null;
                const enabled = getVisualOverlayState(sessionId);
                return { enabled };
            }

            case 'check_active_panel': {
                // For content scripts to check if this tab is part of an active automation session
                // Only tabs explicitly registered through the CDP thread/window flow should show overlay
                const tabId = sender?.tab?.id;
                if (!tabId) {
                    return { active: false };
                }

                // Try to find executionId via two methods:
                // 1. Session-based: parse magician-{executionId} from tab's session mapping
                // 2. Reverse-lookup from activeExecutions by tabId
                let executionId = null;
                const sessionId = getSessionForTab(tabId);

                if (sessionId) {
                    // Check if visual overlay is enabled for this session
                    const visualOverlayEnabled = getVisualOverlayState(sessionId);
                    if (!visualOverlayEnabled) {
                        return { active: false, reason: 'visual_overlay_disabled' };
                    }

                    executionId = executionIdFromSessionId(sessionId);
                }

                // Fallback: reverse-lookup executionId from active executions by tabId
                if (!executionId) {
                    const activeExecutions = getActiveExecutions();
                    for (const [tid, data] of activeExecutions) {
                        if (data.tabId === tabId) {
                            executionId = tid;
                            break;
                        }
                    }
                }

                if (executionId && getActiveExecutions().has(executionId)) {
                    // Fetch actual execution status to avoid showing stale 'running'
                    // when execution is actually paused (prevents panel flickering)
                    let panelStatus = 'running';
                    let panelText = 'Running...';
                    try {
                        const resp = await magicianFetch(
                            `${MAGICIAN_API_BASE}/executions/${executionId}`,
                            { signal: AbortSignal.timeout(2000) }
                        );
                        if (resp.ok) {
                            const execution = await resp.json();
                            const executionStatus = normalizeExecutionStatus(execution);
                            if (isTerminalExecutionStatus(executionStatus)) {
                                const sessionAlias = browserThreadIdForExecution(executionId);
                                const terminalOverlay = buildTerminalOverlayStatus(executionStatus);
                                untrackExecution(executionId);
                                clearSession(sessionAlias);
                                try {
                                    await chrome.tabs.sendMessage(tabId, {
                                        action: 'overlay_status',
                                        executionId,
                                        status: terminalOverlay.status,
                                        text: terminalOverlay.text
                                    });
                                } catch (_) {
                                    // The caller is the content script, so a response is enough if messaging fails.
                                }
                                return { active: false };
                            }
                            if (isPausedExecutionStatus(executionStatus)) {
                                if (execution.escalation_trigger) {
                                    panelStatus = 'escalation';
                                    panelText = execution.escalation_trigger === 'loop_detected'
                                        ? 'Agent stuck in loop — help needed'
                                        : 'Agent needs help';
                                } else {
                                    let pauseInputType = null;
                                    if (executionStatus === 'waitinguser' || executionStatus === 'waiting_user') {
                                        try {
                                            pauseInputType = extractPauseInputType(
                                                await fetchPauseStateSnapshot(executionId, {
                                                    signal: AbortSignal.timeout(2000)
                                                })
                                            );
                                        } catch (_) {
                                            pauseInputType = null;
                                        }
                                    }
                                    panelStatus = (executionStatus === 'waitinguser' || executionStatus === 'waiting_user')
                                        && pauseInputType !== 'external_action'
                                        ? 'waiting_user_input'
                                        : 'paused';
                                    panelText = (executionStatus === 'waitinguser' || executionStatus === 'waiting_user')
                                        && pauseInputType !== 'external_action'
                                        ? 'Input needed in Magician'
                                        : 'Execution paused';
                                }
                            } else if (isWaitingChildrenStatus(executionStatus)) {
                                panelStatus = 'waiting_children';
                                panelText = 'Waiting on delegated work...';
                            }
                        }
                    } catch (_) {
                        // On fetch failure, default to 'running' — poll will correct
                    }
                    return {
                        active: true,
                        executionId,
                        status: panelStatus,
                        text: panelText
                    };
                }
                return { active: false };
            }

            case 'set_visual_overlay': {
                const sessionId = message.sessionId;
                if (sessionId) {
                    setVisualOverlayState(sessionId, message.enabled);
                    console.log(`[Magicutor] Broadcasting debug mode to session ${sessionId}`);
                    
                    // Broadcast debug mode to all tabs in session
                    const tabs = getSessionTabs(sessionId);
                    if (tabs) {
                        for (const tabId of tabs) {
                            try {
                                chrome.tabs.sendMessage(tabId, { 
                                    action: 'overlay_debug', 
                                    enabled: message.enabled 
                                }).catch(() => {});
                            } catch (e) {}
                        }
                    }
                } else {
                    // Update global default
                    setVisualOverlayState(null, message.enabled);
                    console.log(`[Magicutor] Broadcasting global debug mode (${message.enabled})`);

                    // Global mode: broadcast to the tab that opened the popup (active tab)
                    if (sender && sender.tab && sender.tab.id) {
                         try {
                            chrome.tabs.sendMessage(sender.tab.id, { 
                                action: 'overlay_debug', 
                                enabled: message.enabled 
                            }).catch(() => {});
                        } catch (e) {}
                    } else {
                        // If sender tab is missing (popup might not have tab context in some cases),
                        // try to query active tab
                        chrome.tabs.query({active: true, lastFocusedWindow: true}, (tabs) => {
                            if (tabs && tabs[0]) {
                                console.log(`[Magicutor] Sending debug to active tab ${tabs[0].id}`);
                                chrome.tabs.sendMessage(tabs[0].id, { 
                                    action: 'overlay_debug', 
                                    enabled: message.enabled 
                                }).catch((e) => console.warn('[Magicutor] Debug send failed:', e));
                            } else {
                                console.warn('[Magicutor] No active tab found for debug broadcast');
                            }
                        });
                    }
                }
                return { ok: true };
            }

            case 'start_automation': {
                const goal = message.goal || '';
                if (!goal.trim()) {
                    throw new Error('Goal is required');
                }

                if (message.useThisTab && !message.tabId) {
                    throw new Error('Active tab ID is required to bind automation to this tab');
                }

                const goalText = buildInitialMessage({
                    goal,
                    tabUrl: message.tabUrl || null,
                    tabTitle: message.tabTitle || null,
                    includeTabUrl: !!message.includeTabUrl
                });

                const titleBase = goal.trim().slice(0, 80);
                const title = `Extension: ${titleBase}${goal.trim().length > 80 ? '…' : ''}`;

                const executionResponse = await createExecution({ title });

                if (!executionResponse.executionId) {
                    throw new Error('Failed to create execution');
                }

                if (message.useThisTab) {
                    const sessionAlias = browserThreadIdForExecution(executionResponse.executionId);

                    await bindThreadToTab({
                        threadId: sessionAlias,
                        tabId: message.tabId,
                        windowId: message.windowId || null
                    });

                    // Register the tab-session relationship in extension memory
                    // This allows getActiveExecution() to find the execution for this tab
                    registerSessionTab(sessionAlias, message.tabId, true);
                }

                await startExecution({
                    executionId: executionResponse.executionId,
                    goal: goalText,
                    maxIterations: message.maxIterations || null
                });

                // Track the execution for background cleanup
                trackActiveExecution(executionResponse.executionId, {
                    windowId: message.windowId || null,
                    tabId: message.useThisTab ? (message.tabId || null) : null
                });

                // Show status panel on the page
                if (message.useThisTab && message.tabId) {
                    try {
                        await chrome.tabs.sendMessage(message.tabId, {
                            action: 'overlay_panel_show',
                            executionId: executionResponse.executionId,
                            status: 'running',
                            text: 'Starting automation...'
                        });
                    } catch (e) {
                        // Content script might not be ready
                    }

                    // Start polling for execution completion
                    pollExecutionStatus(executionResponse.executionId, message.tabId);
                }

                return executionResponse;
            }

            case 'stop_automation': {
                const executionId = message.executionId;
                const closeTabs = message.closeWindows !== false; // default true

                if (!executionId) {
                    throw new Error('executionId is required');
                }
                const sessionAlias = browserThreadIdForExecution(executionId);

                // 1. Cancel the execution in Magician
                await cancelExecution({ executionId });

                // 2. Stop tracking this execution
                untrackExecution(executionId);

                // 3. Optionally close automation tabs
                let tabsClosed = 0;
                if (closeTabs) {
                    const tabs = getSessionTabs(sessionAlias);

                    if (tabs && tabs.size > 0) {
                        console.log(`[StopAutomation] Closing ${tabs.size} tabs for session ${sessionAlias}`);
                        for (const tabId of tabs) {
                            try {
                                await chrome.tabs.remove(tabId);
                                tabsClosed++;
                                console.log(`[StopAutomation] Closed tab ${tabId}`);
                            } catch (e) {
                                // Tab might already be closed
                                console.warn(`[StopAutomation] Could not close tab ${tabId}: ${e.message}`);
                            }
                        }
                    }

                    // Clear the session tracking
                    clearSession(sessionAlias);

                }

                try {
                    await clearCdpThread({ threadId: sessionAlias });
                } catch (e) {
                    // Not fatal - Magician cancellation already happened.
                    console.debug(`[StopAutomation] CDP thread cleanup: ${e.message}`);
                }

                return { status: 'ok', executionId, tabsClosed };
            }

            case 'continue_automation': {
                const executionId = message.executionId;
                if (!executionId) {
                    throw new Error('executionId is required');
                }
                return await postExecutionControl(executionId, 'resume');
            }

            case 'escalation_done': {
                // Note: Extension does not send guidance text (no text input in overlay).
                // Users who need to provide guidance should use the web UI.
                const executionId = message.executionId;
                if (!executionId) throw new Error('executionId is required');

                const controller = new AbortController();
                const timeout = setTimeout(() => controller.abort(), MAGICIAN_API_TIMEOUT_MS);

                try {
                    const response = await magicianFetch(
                        `${MAGICIAN_API_BASE}/executions/${executionId}/execution/agentic-resume`,
                        {
                            method: 'POST',
                            headers: { 'Content-Type': 'application/json' },
                            body: JSON.stringify({
                                input_type: 'external_action',
                                value: { type: 'external_action_completed' }
                            }),
                            signal: controller.signal
                        }
                    );
                    if (!response.ok) {
                        const errorText = await response.text().catch(() => '');
                        throw new Error(`Done failed (${response.status}): ${errorText.slice(0, 240)}`);
                    }
                    return await response.json().catch(() => ({ status: 'ok' }));
                } finally {
                    clearTimeout(timeout);
                }
            }

            case 'escalation_keep_trying': {
                const executionId = message.executionId;
                if (!executionId) throw new Error('executionId is required');

                const controller = new AbortController();
                const timeout = setTimeout(() => controller.abort(), MAGICIAN_API_TIMEOUT_MS);

                try {
                    const response = await magicianFetch(
                        `${MAGICIAN_API_BASE}/executions/${executionId}/execution/agentic-continue`,
                        {
                            method: 'POST',
                            headers: { 'Content-Type': 'application/json' },
                            body: JSON.stringify({}),
                            signal: controller.signal
                        }
                    );
                    if (!response.ok) {
                        const errorText = await response.text().catch(() => '');
                        throw new Error(`Keep Trying failed (${response.status}): ${errorText.slice(0, 240)}`);
                    }
                    return await response.json().catch(() => ({ status: 'ok' }));
                } finally {
                    clearTimeout(timeout);
                }
            }

            case 'cancel_automation': {
                const executionId = message.executionId;
                if (!executionId) {
                    throw new Error('executionId is required');
                }
                const result = await postExecutionControl(executionId, 'cancel');

                // Clean up tracking
                untrackExecution(executionId);
                const sessionAlias = browserThreadIdForExecution(executionId);
                clearSession(sessionAlias);

                return result;
            }

            default:
                throw new Error(`Unknown action: ${message.action}`);
        }
    };
}

export { createExtensionMessageHandler, pollExecutionStatus };
