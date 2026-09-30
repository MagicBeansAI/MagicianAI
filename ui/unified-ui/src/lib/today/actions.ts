import type { FeedAction } from '$lib/feed/types';

export interface TodayActionExecutionResult {
	task_id?: string | null;
	task?: {
		manifest?: {
			task_id?: string | null;
		} | null;
	} | null;
	navigate_to?: {
		kind?: string | null;
		task_id?: string | null;
	} | null;
	reused_task?: boolean;
}

function asRecord(value: unknown): Record<string, unknown> | null {
	return value !== null && typeof value === 'object' && !Array.isArray(value)
		? (value as Record<string, unknown>)
		: null;
}

export function todayActionEndpoint(action: FeedAction): string | null {
	const payload = asRecord(action.payload);
	const endpoint = typeof payload?.endpoint === 'string' ? payload.endpoint.trim() : '';
	const method = typeof payload?.method === 'string' ? payload.method.toUpperCase() : 'POST';
	if (method !== 'POST') return null;
	if (!endpoint.startsWith('/api/magician/v2/today/items/')) return null;
	return endpoint;
}

export function todayActionTaskId(result: TodayActionExecutionResult): string | null {
	for (const candidate of [
		result.navigate_to?.task_id,
		result.task_id,
		result.task?.manifest?.task_id
	]) {
		if (typeof candidate === 'string' && candidate.trim()) return candidate.trim();
	}
	return null;
}
