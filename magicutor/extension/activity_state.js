// Activity tracking for popup display.

const activityState = {
    current: null,           // { action, requestId, startTime, params }
    recentActions: [],       // Last 10 completed actions
    recentDecisions: [],     // Last 20 LLM decisions (from AgenticDecisionMade events)
    stats: {
        total: 0,
        success: 0,
        failed: 0,
        startedAt: Date.now()
    }
};

const MAX_RECENT_ACTIONS = 10;
const MAX_RECENT_DECISIONS = 20;
const MAX_COMMAND_TOOLTIP_CHARS = 800;
const MAX_COMMAND_EXCERPT_CHARS = 90;

function truncate(value, maxChars) {
    const text = String(value ?? '');
    return text.length > maxChars ? `${text.slice(0, maxChars)}...` : text;
}

function compactValue(value, maxChars = MAX_COMMAND_EXCERPT_CHARS) {
    if (value === null || value === undefined) return '';
    if (typeof value === 'string') return truncate(value.replace(/\s+/g, ' ').trim(), maxChars);
    if (typeof value === 'number' || typeof value === 'boolean') return String(value);
    try {
        return truncate(JSON.stringify(value), maxChars);
    } catch (_) {
        return '[object]';
    }
}

function compactJson(value, maxChars = MAX_COMMAND_TOOLTIP_CHARS) {
    try {
        return truncate(JSON.stringify(value ?? {}, null, 2), maxChars);
    } catch (_) {
        return '[unserializable params]';
    }
}

function describeCdpCommand(params = {}) {
    const method = params.method || 'debugger_command';
    const commandParams = params.params || {};

    if (method === 'Input.dispatchMouseEvent') {
        const eventType = commandParams.type || 'mouse';
        const x = Number.isFinite(Number(commandParams.x)) ? Math.round(Number(commandParams.x)) : '?';
        const y = Number.isFinite(Number(commandParams.y)) ? Math.round(Number(commandParams.y)) : '?';
        const button = commandParams.button ? ` ${commandParams.button}` : '';
        const wheel = eventType === 'mouseWheel'
            ? ` delta(${commandParams.deltaX || 0}, ${commandParams.deltaY || 0})`
            : '';
        return `${method} ${eventType}${button} @ ${x},${y}${wheel}`;
    }

    if (method === 'Input.dispatchKeyEvent') {
        const key = commandParams.key || commandParams.text || commandParams.code || '';
        return `${method} ${commandParams.type || 'key'}${key ? ` ${key}` : ''}`;
    }

    if (method === 'Input.insertText') {
        return `${method} "${compactValue(commandParams.text, 60)}"`;
    }

    if (method === 'Page.navigate') {
        return `${method} ${compactValue(commandParams.url, 100)}`;
    }

    if (method === 'Runtime.evaluate') {
        const expression = compactValue(commandParams.expression, 100);
        return expression ? `${method} ${expression}` : method;
    }

    if (method === 'Page.captureScreenshot') {
        const format = commandParams.format ? ` ${commandParams.format}` : '';
        return `${method}${format}`;
    }

    return method;
}

function exactCdpCommand(params = {}) {
    const method = params.method || 'debugger_command';
    return `${method} ${compactJson(params.params || {})}`;
}

function buildActionDisplay(action, params = {}) {
    if (action === 'debugger_command') {
        const detail = describeCdpCommand(params);
        return {
            displayName: 'Debugger Command',
            displayDetails: detail,
            tooltip: exactCdpCommand(params)
        };
    }

    return {};
}

/**
 * Summarize params for display (avoid sensitive data, truncate large values).
 */
function summarizeParams(params) {
    if (!params) return {};
    const summary = {};
    for (const [key, value] of Object.entries(params)) {
        if (key === 'screenshot') {
            summary[key] = value ? '[data]' : false;
        } else if (typeof value === 'string' && value.length > 50) {
            summary[key] = value.substring(0, 50) + '...';
        } else if (typeof value === 'object') {
            summary[key] = '[object]';
        } else {
            summary[key] = value;
        }
    }
    return summary;
}

/**
 * Record start of an action.
 */
function recordActionStart(action, requestId, params) {
    const display = buildActionDisplay(action, params);
    activityState.current = {
        action,
        requestId,
        startTime: Date.now(),
        params: summarizeParams(params),
        ...display
    };
    activityState.stats.total++;
}

/**
 * Record completion of an action.
 */
function recordActionComplete(requestId, success, error = null) {
    const current = activityState.current;
    if (current && current.requestId === requestId) {
        const completedAction = {
            ...current,
            endTime: Date.now(),
            duration: Date.now() - current.startTime,
            success,
            error: error ? String(error).substring(0, 100) : null
        };

        // Add to recent actions (keep last N)
        activityState.recentActions.unshift(completedAction);
        if (activityState.recentActions.length > MAX_RECENT_ACTIONS) {
            activityState.recentActions.pop();
        }

        // Update stats
        if (success) {
            activityState.stats.success++;
        } else {
            activityState.stats.failed++;
        }

        activityState.current = null;
    }
}

/**
 * Record an LLM decision event (from AgenticDecisionMade WebSocket events).
 */
function recordDecision(decision) {
    activityState.recentDecisions.unshift({
        timestamp: decision.timestamp || Date.now(),
        decisionType: decision.decision_type,
        actionSummary: decision.action_summary || null,
        reasoning: (decision.reasoning || '').substring(0, 200),
        confidence: decision.confidence,
        thinking: decision.thinking || null,
        evidence: decision.evidence || null,
        toolName: decision.tool_name || null,
        actionType: decision.action_type || null,
        elementId: decision.element_id ?? null,
        candidatesCount: decision.candidates_count ?? null,
    });
    if (activityState.recentDecisions.length > MAX_RECENT_DECISIONS) {
        activityState.recentDecisions.pop();
    }
}

export { activityState, recordActionComplete, recordActionStart, recordDecision, summarizeParams };
