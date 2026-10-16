import type { V2WebSocketEvent } from './v2-websocket';

/**
 * Context for filtering V2 WebSocket events to a specific execution or agent execution scope.
 * Both ExecutionPanel and the mirror page use this to ensure identical filtering logic.
 */
export interface EventFilterContext {
    executionId?: string;
    agentId?: string;
    goalId?: string;
    cycleId?: string;
}

function asRecord(value: unknown): Record<string, unknown> | null {
    return typeof value === 'object' && value !== null
        ? (value as Record<string, unknown>)
        : null;
}

function readString(record: Record<string, unknown> | null, key: string): string | undefined {
    const value = record ? record[key] : undefined;
    return typeof value === 'string' && value.trim().length > 0 ? value.trim() : undefined;
}

/**
 * Returns true if the event matches the given filter context.
 *
 * Matching rules (same as ExecutionPanel.processExecutionEvents):
 * 1. If executionId is set and event.data.execution_id matches the execution scope → accept
 * 2. If all of agentId/goalId/cycleId are set and event.data carries matching values → accept
 * 3. Otherwise → reject
 */
export function matchesEventFilter(event: V2WebSocketEvent, ctx: EventFilterContext): boolean {
    const payload = asRecord(event.data);
    const normalizedExecutionId = ctx.executionId?.trim();

    // Execution-scoped match.
    if (normalizedExecutionId && readString(payload, 'execution_id') === normalizedExecutionId) {
        return true;
    }
    if (
        normalizedExecutionId
        && readString(payload, 'parent_execution_id') === normalizedExecutionId
    ) {
        return true;
    }

    // Agent-scoped match — require all three identifiers (trimmed to match readString)
    const normalizedAgentId = ctx.agentId?.trim();
    const normalizedGoalId = ctx.goalId?.trim();
    const normalizedCycleId = ctx.cycleId?.trim();
    if (normalizedAgentId && normalizedGoalId && normalizedCycleId) {
        const eventAgentId = readString(payload, 'agent_id');
        if (!eventAgentId || eventAgentId !== normalizedAgentId) return false;
        const eventGoalId = readString(payload, 'goal_id');
        if (!eventGoalId || eventGoalId !== normalizedGoalId) return false;
        const eventCycleId = readString(payload, 'cycle_id');
        if (!eventCycleId || eventCycleId !== normalizedCycleId) return false;
        return true;
    }

    return false;
}
