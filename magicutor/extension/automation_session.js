const CDP_THREAD_PREFIX = 'magician-';
const LEGACY_SESSION_PREFIX = 'agentic-session-';

function sanitizeThreadComponent(value) {
    return String(value || '').replace(/[^a-zA-Z0-9_-]/g, '_');
}

export function browserThreadIdForExecution(executionId) {
    return `${CDP_THREAD_PREFIX}${sanitizeThreadComponent(executionId)}`;
}

export function executionIdFromSessionId(sessionId) {
    if (typeof sessionId !== 'string' || sessionId.length === 0) {
        return null;
    }
    if (sessionId.startsWith(CDP_THREAD_PREFIX)) {
        return sessionId.slice(CDP_THREAD_PREFIX.length) || null;
    }
    if (sessionId.startsWith(LEGACY_SESSION_PREFIX)) {
        return sessionId.slice(LEGACY_SESSION_PREFIX.length) || null;
    }
    return null;
}
