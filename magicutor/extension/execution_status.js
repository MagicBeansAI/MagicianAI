const TERMINAL_SUCCESS_STATES = new Set([
    'completed',
    'complete',
    'success',
    'succeeded',
    'done',
    'finished'
]);

const TERMINAL_FAILURE_STATES = new Set([
    'failed',
    'failure',
    'error',
    'cancelled',
    'canceled',
    'timeout',
    'timedout',
    'timed_out',
    'aborted'
]);

const PAUSED_STATES = new Set(['waitinguser', 'waiting_user', 'paused']);
const WAITING_CHILDREN_STATES = new Set(['waitingchildren', 'waiting_children']);

function normalizeExecutionStatus(executionOrStatus) {
    const rawStatus = typeof executionOrStatus === 'string'
        ? executionOrStatus
        : executionOrStatus?.waiting_state
            || executionOrStatus?.status
            || executionOrStatus?.state
            || executionOrStatus?.execution_status
            || executionOrStatus?.execution?.waiting_state
            || '';

    return String(rawStatus).trim().toLowerCase();
}

function isSuccessExecutionStatus(status) {
    return TERMINAL_SUCCESS_STATES.has(normalizeExecutionStatus(status));
}

function isFailureExecutionStatus(status) {
    return TERMINAL_FAILURE_STATES.has(normalizeExecutionStatus(status));
}

function isTerminalExecutionStatus(status) {
    return isSuccessExecutionStatus(status) || isFailureExecutionStatus(status);
}

function isPausedExecutionStatus(status) {
    return PAUSED_STATES.has(normalizeExecutionStatus(status));
}

function isWaitingChildrenStatus(status) {
    return WAITING_CHILDREN_STATES.has(normalizeExecutionStatus(status));
}

function buildTerminalOverlayStatus(status) {
    const normalized = normalizeExecutionStatus(status);
    const isSuccess = isSuccessExecutionStatus(normalized);
    return {
        status: isSuccess ? 'success' : 'error',
        text: isSuccess ? 'Completed!' : `Failed: ${normalized || 'unknown'}`
    };
}

export {
    TERMINAL_SUCCESS_STATES,
    TERMINAL_FAILURE_STATES,
    PAUSED_STATES,
    WAITING_CHILDREN_STATES,
    normalizeExecutionStatus,
    isSuccessExecutionStatus,
    isFailureExecutionStatus,
    isTerminalExecutionStatus,
    isPausedExecutionStatus,
    isWaitingChildrenStatus,
    buildTerminalOverlayStatus
};
