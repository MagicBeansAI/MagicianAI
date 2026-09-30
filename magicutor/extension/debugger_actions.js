import {
    debuggerSessions,
    clearSession,
    getSessionForTab,
    getSessionPrimaryTab,
    registerSessionTab
} from './background_state.js';

const DEBUGGER_COMMAND_TIMEOUT_MS = 60000;
const RUNTIME_EVALUATE_TIMEOUT_MS = 45000;
const DEBUGGER_ATTACH_TIMEOUT_MS = 10000;

function timeoutForDebuggerCommand(method) {
    return method === 'Runtime.evaluate'
        ? RUNTIME_EVALUATE_TIMEOUT_MS
        : DEBUGGER_COMMAND_TIMEOUT_MS;
}

async function withDebuggerCommandTimeout(promise, method) {
    const timeoutMs = timeoutForDebuggerCommand(method);
    return await withTimeout(promise, method, timeoutMs);
}

async function withTimeout(promise, label, timeoutMs) {
    let timeoutId = null;
    try {
        return await Promise.race([
            promise,
            new Promise((_, reject) => {
                timeoutId = setTimeout(() => {
                    reject(new Error(`${label} timed out after ${timeoutMs}ms`));
                }, timeoutMs);
            })
        ]);
    } finally {
        if (timeoutId !== null) {
            clearTimeout(timeoutId);
        }
    }
}

// ===== Debugger API =====

async function attachDebugger(params) {
    const target = { tabId: params.tabId };

    try {
        if (debuggerSessions.has(params.tabId)) {
            const existing = debuggerSessions.get(params.tabId) || {};
            debuggerSessions.set(params.tabId, {
                ...existing,
                attached: true,
                windowId: params.windowId ?? existing.windowId,
                defaultDownloadPath: params.defaultDownloadPath || existing.defaultDownloadPath || null
            });

            const sessionId = params.sessionId;
            if (sessionId) {
                const existingPrimary = getSessionPrimaryTab(sessionId);
                registerSessionTab(sessionId, params.tabId, existingPrimary === null);
                console.log(`[Magicutor] Reused debugger tab ${params.tabId} for session ${sessionId} (primary=${existingPrimary === null})`);
            }

            return { attached: true, reused: true };
        }

        try {
            await withTimeout(
                chrome.debugger.attach(target, '1.3'),
                `debugger.attach tab ${params.tabId}`,
                DEBUGGER_ATTACH_TIMEOUT_MS
            );
        } catch (firstError) {
            // Recover from a stale-self attachment left behind by a previous
            // extension instance (e.g. service-worker restart). chrome.debugger.detach
            // succeeds only for sessions WE own, so it's a safe probe — if it
            // succeeds we owned it, reattach cleanly. If detach fails the debugger
            // belongs to another tool (DevTools / another extension); surface that
            // as a clear error so the proxy doesn't hand back a fake sessionId.
            const isAlreadyAttached =
                typeof firstError?.message === 'string' &&
                firstError.message.includes('Another debugger is already attached');
            if (!isAlreadyAttached) throw firstError;
            try {
                await withTimeout(
                    chrome.debugger.detach(target),
                    `debugger.detach stale tab ${params.tabId}`,
                    DEBUGGER_ATTACH_TIMEOUT_MS
                );
            } catch (detachError) {
                throw new Error(
                    `Failed to attach debugger: tab ${params.tabId} is held by another tool ` +
                    `(DevTools / different extension); not recoverable from this extension. ` +
                    `Original: ${firstError.message}`
                );
            }
            console.log(`[Magicutor] Recovered stale-self debugger attachment on tab ${params.tabId}; reattaching`);
            await withTimeout(
                chrome.debugger.attach(target, '1.3'),
                `debugger.reattach tab ${params.tabId}`,
                DEBUGGER_ATTACH_TIMEOUT_MS
            );
        }
        debuggerSessions.set(params.tabId, {
            attached: true,
            windowId: params.windowId,
            defaultDownloadPath: params.defaultDownloadPath || null
        });

        const sessionId = params.sessionId;
        if (sessionId) {
            const existingPrimary = getSessionPrimaryTab(sessionId);
            registerSessionTab(sessionId, params.tabId, existingPrimary === null);
            console.log(`[Magicutor] Registered tab ${params.tabId} with session ${sessionId} (primary=${existingPrimary === null})`);
        }

        return { attached: true };
    } catch (error) {
        throw new Error(`Failed to attach debugger: ${error.message}`);
    }
}

async function detachDebugger(params) {
    const target = { tabId: params.tabId };
    const sessionId = params.sessionId || getSessionForTab(params.tabId);

    // Helper to clean up session tracking after detach
    const cleanupSession = () => {
        // Clear session tracking if:
        // 1. Detaching the primary tab (session explicitly ended), OR
        // 2. No debugger sessions remain (session ended from any tab)
        // This prevents stale tab context from persisting to the next session
        const primaryTabId = sessionId ? getSessionPrimaryTab(sessionId) : null;
        const isPrimaryTab = primaryTabId === params.tabId;
        const noSessionsRemain = debuggerSessions.size === 0;

        if (sessionId && (isPrimaryTab || noSessionsRemain)) {
            if (noSessionsRemain && !isPrimaryTab) {
                console.log(`[SessionTabs] Clearing session ${sessionId} - no debugger sessions remain after detaching tab ${params.tabId}`);
            }
            clearSession(sessionId);
        }
    };

    try {
        await chrome.debugger.detach(target);
        debuggerSessions.delete(params.tabId);
        cleanupSession();
        return { detached: true };
    } catch (error) {
        // Ignore errors if already detached
        debuggerSessions.delete(params.tabId);
        cleanupSession();
        return { detached: true };
    }
}

async function sendDebuggerCommand(params) {
    // OOPIF iframe routing: when the magicutor proxy registered a
    // CDP session_id (via Target.attachedToTarget for iframe targets
    // — see `cdp_proxy::update_iframe_sessions_from_event`), it
    // forwards the session_id alongside the tab_id. Pass both to
    // chrome.debugger.sendCommand via the {tabId, sessionId} target
    // shape so the command lands on the right CDP session inside
    // Chrome. Without sessionId, the command runs on the tab's main
    // session and would return parent-frame content for an
    // iframe-intended call.
    const target = params.sessionId
        ? { tabId: params.tabId, sessionId: params.sessionId }
        : { tabId: params.tabId };

    // Ensure debugger is attached
    if (!debuggerSessions.has(params.tabId)) {
        await attachDebugger(params);
    }

    // Clone params to avoid mutating the original
    let commandParams = params.params || {};

    // For Runtime.evaluate, wrap expression in async IIFE to prevent variable redeclaration
    // errors AND support `await` in LLM-generated code. LLMs frequently generate `await` inside
    // synchronous IIFEs — wrapping in async + setting awaitPromise handles this transparently.
    if (params.method === 'Runtime.evaluate' && commandParams.expression) {
        const expr = commandParams.expression.trim();
        const hasAwait = expr.includes('await ');

        if (expr.startsWith('(function') || expr.startsWith('(async function')) {
            // LLM already wrapped in IIFE — fix sync→async if it uses await
            if (hasAwait && expr.startsWith('(function') && !expr.startsWith('(async function')) {
                commandParams = {
                    ...commandParams,
                    expression: expr.replace(/^\(function/, '(async function'),
                    awaitPromise: true
                };
                console.log('[Magicutor][sendDebuggerCommand] Patched sync IIFE → async IIFE (code uses await)');
            } else if (hasAwait) {
                commandParams = { ...commandParams, awaitPromise: true };
            }
        } else if (!/^\(\s*(?:async\s+)?\(\s*\)\s*=>/.test(expr) &&
            (expr.includes('const ') || expr.includes('let ') || expr.includes('class ') || hasAwait)) {
            // Wrap in async IIFE — always async so `await` works if present.
            // Only fires for non-IIFE expressions; IIFE shapes are handled in the
            // sync→async patch branch below regardless of whitespace.
            commandParams = {
                ...commandParams,
                expression: `(async () => { ${expr} })()`,
                awaitPromise: true
            };
            console.log('[Magicutor][sendDebuggerCommand] Wrapped expression in async IIFE');
        } else if (/^\(\s*\(\s*\)\s*=>/.test(expr) && hasAwait) {
            // Patch sync arrow IIFE → async (whitespace-tolerant: matches `(()=>`, `(() =>`, etc.)
            commandParams = {
                ...commandParams,
                expression: expr.replace(/^\(\s*\(\s*\)\s*=>/, '(async () =>'),
                awaitPromise: true
            };
            console.log('[Magicutor][sendDebuggerCommand] Patched sync arrow IIFE → async');
        }
    }

    const executeCommand = async () => {
        const result = await withDebuggerCommandTimeout(
            chrome.debugger.sendCommand(
                target,
                params.method,
                commandParams
            ),
            params.method
        );

        // Check for JavaScript exceptions in Runtime.evaluate results
        // CDP returns exceptions in exceptionDetails rather than throwing
        if (result && result.exceptionDetails) {
            const exception = result.exceptionDetails;
            const errorMessage = exception.exception?.description ||
                                 exception.exception?.value ||
                                 exception.text ||
                                 'JavaScript execution error';
            throw new Error(errorMessage);
        }

        return { result };
    };

    return await executeCommand();
}

export { attachDebugger, detachDebugger, sendDebuggerCommand };
