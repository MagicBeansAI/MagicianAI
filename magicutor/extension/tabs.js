import {
    debuggerSessions,
    removeTab,
    isTabInSession,
    getSessionForTab,
    addTabToSession,
    getActiveExecutions
} from './background_state.js';
import { executionIdFromSessionId } from './automation_session.js';

/**
 * Register a new tab with a session and send overlay_panel_show so the
 * floating panel + aurora border appear immediately in the new window.
 */
function registerAndNotifyTab(sessionId, tabId, openerTabId, source) {
    addTabToSession(sessionId, tabId, openerTabId);
    console.log(`[SessionTabs] Auto-registered tab ${tabId} (opener=${openerTabId}, session=${sessionId}, source=${source})`);

    const executionId = executionIdFromSessionId(sessionId);
    if (!executionId) return;

    // Only send panel_show if the execution is still active
    if (!getActiveExecutions().has(executionId)) return;

    // Send overlay_panel_show to the new tab so the floating panel appears.
    // Use a short delay to let the content script (visual_overlay.js) initialize first.
    setTimeout(() => {
        try {
            chrome.tabs.sendMessage(tabId, {
                action: 'overlay_panel_show',
                executionId,
                status: 'running',
                text: 'Running...'
            }).catch(() => {});
        } catch (e) {
            // Content script might not be ready yet
        }
    }, 500);
}

/**
 * Setup tab listeners.
 */
function setupTabListeners({ attachDebugger } = {}) {
    chrome.tabs.onCreated.addListener(tab => {
        // Auto-register tabs opened by automation session tabs (window.open, target="_blank")
        if (tab.openerTabId) {
            const sessionId = getSessionForTab(tab.openerTabId);
            if (sessionId) {
                registerAndNotifyTab(sessionId, tab.id, tab.openerTabId, 'onCreated');
            }
        }
    });

    // webNavigation.onCreatedNavigationTarget is more reliable than openerTabId for
    // CDP-dispatched clicks that trigger window.open() or target="_blank" navigations.
    chrome.webNavigation.onCreatedNavigationTarget.addListener(details => {
        const { sourceTabId, tabId } = details;
        if (sourceTabId && tabId && !isTabInSession(tabId)) {
            const sessionId = getSessionForTab(sourceTabId);
            if (sessionId) {
                registerAndNotifyTab(sessionId, tabId, sourceTabId, 'webNavigation');
            }
        }
    });

    chrome.tabs.onRemoved.addListener(async (tabId, _removeInfo) => {
        // Cleanup debugger session
        if (debuggerSessions.has(tabId)) {
            debuggerSessions.delete(tabId);
        }

        // SESSION-BASED TAB TRACKING: If this tab was in a session, remove it and switch back
        if (isTabInSession(tabId)) {
            // Get session before removing (removeTab clears the mapping)
            const sessionId = getSessionForTab(tabId);
            const openerTabId = removeTab(tabId);

            if (openerTabId !== null) {
                console.log(`[SessionTabs] Switching back to tab ${openerTabId} after tab ${tabId} closed`);

                try {
                    // Focus the opener tab
                    await chrome.tabs.update(openerTabId, { active: true });

                    // Re-attach debugger if we have the attachDebugger function
                    if (typeof attachDebugger === 'function' && !debuggerSessions.has(openerTabId)) {
                        await attachDebugger({ tabId: openerTabId, sessionId });
                        console.log(`[SessionTabs] Re-attached debugger to tab ${openerTabId}`);
                    }
                } catch (e) {
                    console.warn(`[SessionTabs] Failed to switch to tab ${openerTabId}: ${e.message}`);
                }
            }
        }
    });

    chrome.tabs.onUpdated.addListener((tabId, changeInfo, _tab) => {
        // URL changes are tracked via chrome.tabs API, not internally
        // We only need to know if a tab is in a session for debugging purposes
        if (changeInfo.url && isTabInSession(tabId)) {
            console.log(`[SessionTabs] Tab ${tabId} URL changed to: ${changeInfo.url}`);
        }
    });
}

/**
 * Setup window listeners.
 */
function setupWindowListeners() {
    chrome.windows.onCreated.addListener(_window => {
        // Window created - could send event over bridge if needed
    });

    chrome.windows.onRemoved.addListener(_windowId => {
        // Window closed - could send event over bridge if needed
    });
}

export { setupTabListeners, setupWindowListeners };
