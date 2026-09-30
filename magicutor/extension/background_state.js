// Shared state for the background service worker.

// Track active debugger sessions: tabId -> { attached: boolean, windowId: number }
const debuggerSessions = new Map();

// ============================================================================
// SESSION-BASED TAB TRACKING (Multi-session, multi-tab, multi-window support)
// ============================================================================
// Replaces the old global tabStack with session-aware tracking.
// Each session tracks its own tabs, isolating concurrent sessions.

// Track which tabs belong to which session: sessionId -> Set<tabId>
const sessionTabs = new Map();

// Track opener relationships for switch-back-on-close: tabId -> openerTabId
const tabOpeners = new Map();

// Reverse lookup - which session owns this tab: tabId -> sessionId
const tabToSession = new Map();

// Track primary (initial) tab per session: sessionId -> tabId
const sessionPrimaryTab = new Map();

/**
 * Register a tab with a session
 * @param {string} sessionId - The session ID
 * @param {number} tabId - The tab ID
 * @param {boolean} isPrimary - Whether this is the session's primary/initial tab
 */
function registerSessionTab(sessionId, tabId, isPrimary = false) {
    if (!sessionId) {
        console.warn(`[SessionTabs] Cannot register tab ${tabId} - no sessionId provided`);
        return;
    }

    if (!sessionTabs.has(sessionId)) {
        sessionTabs.set(sessionId, new Set());
    }
    sessionTabs.get(sessionId).add(tabId);
    tabToSession.set(tabId, sessionId);

    if (isPrimary) {
        sessionPrimaryTab.set(sessionId, tabId);
    }

    console.log(`[SessionTabs] Registered tab ${tabId} with session ${sessionId} (primary=${isPrimary})`);
}

/**
 * Add a new tab to a session (e.g., opened via target="_blank")
 * @param {string} sessionId - The session ID
 * @param {number} tabId - The new tab ID
 * @param {number} openerTabId - The tab that opened this one
 */
function addTabToSession(sessionId, tabId, openerTabId) {
    registerSessionTab(sessionId, tabId, false);
    if (openerTabId) {
        tabOpeners.set(tabId, openerTabId);
    }
    console.log(`[SessionTabs] Added tab ${tabId} to session ${sessionId} (opener=${openerTabId})`);
}

/**
 * Remove a tab from tracking
 * @param {number} tabId - The tab ID being removed/closed
 * @returns {number|null} The opener tab ID to switch to (if in same session), or null
 */
function removeTab(tabId) {
    const sessionId = tabToSession.get(tabId);
    const openerTabId = tabOpeners.get(tabId);

    // Clean up this tab's tracking
    tabToSession.delete(tabId);
    tabOpeners.delete(tabId);

    if (sessionId && sessionTabs.has(sessionId)) {
        sessionTabs.get(sessionId).delete(tabId);

        // If this was the primary tab, clear that too
        if (sessionPrimaryTab.get(sessionId) === tabId) {
            sessionPrimaryTab.delete(sessionId);
        }
    }

    console.log(`[SessionTabs] Removed tab ${tabId} from session ${sessionId || 'none'}`);

    // Return opener if it's still in the same session (for switch-back)
    if (openerTabId && tabToSession.get(openerTabId) === sessionId) {
        return openerTabId;
    }
    return null;
}

/**
 * Clear all tracking for a session
 * @param {string} sessionId - The session ID to clear
 */
function clearSession(sessionId) {
    if (!sessionId) return;

    const tabs = sessionTabs.get(sessionId);
    if (tabs) {
        for (const tabId of tabs) {
            tabToSession.delete(tabId);
            tabOpeners.delete(tabId);
        }
        console.log(`[SessionTabs] Cleared session ${sessionId} (had ${tabs.size} tabs)`);
    }

    sessionTabs.delete(sessionId);
    sessionPrimaryTab.delete(sessionId);
}

/**
 * Get session context for a tab
 * @param {number} tabId - The tab ID
 * @returns {object|null} Session context or null if tab not in any session
 */
function getTabSessionContext(tabId) {
    const sessionId = tabToSession.get(tabId);
    if (!sessionId) return null;

    const tabs = sessionTabs.get(sessionId);
    const primaryTabId = sessionPrimaryTab.get(sessionId);

    return {
        sessionId,
        tabs: Array.from(tabs || []),
        primaryTabId,
        isPrimaryTab: tabId === primaryTabId,
        openerTabId: tabOpeners.get(tabId) || null
    };
}

/**
 * Check if a tab belongs to any session
 * @param {number} tabId - The tab ID
 * @returns {boolean} True if tab is tracked in a session
 */
function isTabInSession(tabId) {
    return tabToSession.has(tabId);
}

/**
 * Get all tabs for a session
 * @param {string} sessionId - The session ID
 * @returns {Set<number>} Set of tab IDs (empty set if session not found)
 */
function getSessionTabs(sessionId) {
    return sessionTabs.get(sessionId) || new Set();
}

/**
 * Get the session ID for a tab
 * @param {number} tabId - The tab ID
 * @returns {string|null} The session ID or null
 */
function getSessionForTab(tabId) {
    return tabToSession.get(tabId) || null;
}

/**
 * Get the primary tab for a session
 * @param {string} sessionId - The session ID
 * @returns {number|null} The primary tab ID or null
 */
function getSessionPrimaryTab(sessionId) {
    return sessionPrimaryTab.get(sessionId) || null;
}

/**
 * Get tab context for inclusion in action results
 * @param {number} tabId - Specific tab ID
 * @returns {object} Tab context with session info
 */
function getTabContext(tabId = null) {
    if (tabId) {
        const ctx = getTabSessionContext(tabId);
        if (ctx) {
            return {
                activeTabId: tabId,
                sessionId: ctx.sessionId,
                stackDepth: ctx.tabs.length,
                stack: ctx.tabs.map(tid => ({
                    tabId: tid,
                    isPrimary: tid === ctx.primaryTabId,
                    openedFrom: tabOpeners.get(tid) || null
                })),
                isOnPrimaryTab: ctx.isPrimaryTab,
                canGoBack: !!ctx.openerTabId
            };
        }
    }

    return {
        activeTabId: null,
        sessionId: null,
        stackDepth: 0,
        stack: [],
        isOnPrimaryTab: false,
        canGoBack: false
    };
}

// ============================================================================
// POPUP STATE (in-memory, per-window)
// ============================================================================
// Stores popup input state per window. Lost on service worker restart, which is fine.

// windowId → { goal, includeUrl, useThisTab }
const popupState = new Map();

// sessionId → boolean (true = enabled)
const sessionOverlayState = new Map();
let globalOverlayEnabled = true;

/**
 * Get visual overlay state for a session
 * @param {string|null} sessionId - The session ID
 * @returns {boolean} True if enabled (default)
 */
function getVisualOverlayState(sessionId) {
    if (sessionId && sessionOverlayState.has(sessionId)) {
        return sessionOverlayState.get(sessionId);
    }
    return globalOverlayEnabled;
}

/**
 * Set visual overlay state for a session or globally
 * @param {string|null} sessionId - The session ID (null for global default)
 * @param {boolean} enabled - Whether overlay is enabled
 */
function setVisualOverlayState(sessionId, enabled) {
    if (sessionId) {
        sessionOverlayState.set(sessionId, enabled);
    } else {
        globalOverlayEnabled = enabled;
    }
}

// ============================================================================
// ACTIVE EXECUTIONS TRACKING (in-memory, for cleanup)
// ============================================================================
// Tracks active automation executions for background cleanup.
// Lost on service worker restart, which is acceptable - orphaned windows
// will need to be closed manually in that case.

// executionId → { windowId, tabId, startTime }
const activeExecutions = new Map();

/**
 * Track an active execution for cleanup
 * @param {string} executionId - The execution ID
 * @param {object} info - { windowId, tabId }
 */
function trackActiveExecution(executionId, info) {
    activeExecutions.set(executionId, {
        ...info,
        startTime: Date.now()
    });
    console.log(`[ActiveExecutions] Tracking execution ${executionId}`);
}

/**
 * Stop tracking an execution
 * @param {string} executionId - The execution ID
 */
function untrackExecution(executionId) {
    activeExecutions.delete(executionId);
    console.log(`[ActiveExecutions] Untracked execution ${executionId}`);
}

/**
 * Get all tracked executions
 * @returns {Map} Map of executionId → info
 */
function getActiveExecutions() {
    return activeExecutions;
}

/**
 * Get popup state for a window
 * @param {number} windowId - The window ID
 * @returns {object} Popup state or defaults
 */
function getPopupState(windowId) {
    return popupState.get(windowId) || {
        goal: '',
        includeUrl: true,
        useThisTab: false
    };
}

/**
 * Set popup state for a window
 * @param {number} windowId - The window ID
 * @param {object} state - Partial state to merge
 */
function setPopupState(windowId, state) {
    const current = getPopupState(windowId);
    popupState.set(windowId, { ...current, ...state });
}

/**
 * Clear popup state for a window
 * @param {number} windowId - The window ID
 */
function clearPopupState(windowId) {
    popupState.delete(windowId);
}

export {
    debuggerSessions,
    // SESSION-BASED TAB TRACKING (new API)
    registerSessionTab,
    addTabToSession,
    removeTab,
    clearSession,
    getTabSessionContext,
    isTabInSession,
    getSessionTabs,
    getSessionForTab,
    getSessionPrimaryTab,
    getTabContext,
    getPopupState,
    setPopupState,
    clearPopupState,
    // Visual Overlay State
    getVisualOverlayState,
    setVisualOverlayState,
    trackActiveExecution,
    untrackExecution,
    getActiveExecutions
};
