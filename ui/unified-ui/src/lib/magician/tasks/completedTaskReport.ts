import { readTaskRunState } from './taskAttention';
import { executionIdOf } from './taskTimeline';
import {
	getCurrentScopeCredentialRevision,
	scopeCredentialIdentityIsCurrent
} from '$lib/stores/scopeIdentityStore';

const reports = new Map<string, string>();
const pending = new Map<string, Promise<string | null>>();
const MAX_REPORT_BYTES = 256 * 1024;

/** Immutable completed documents, fenced by task, run, scope and login revision. */
export function readCompletedTaskReport(
	taskId: string, executionId: string, principal: string, workspace: string
): Promise<string | null> {
	const revision = getCurrentScopeCredentialRevision();
	const key = JSON.stringify([revision, principal, workspace, taskId, executionId]);
	const cached = reports.get(key);
	if (cached !== undefined) return Promise.resolve(cached);
	const existing = pending.get(key);
	if (existing) return existing;
	const request = (async () => {
		const result = await readTaskRunState(taskId, principal, workspace, executionId);
		const state = result.ok ? result.state : null;
		if (!state || !scopeCredentialIdentityIsCurrent(revision)
			|| state.overview.task_id !== taskId || executionIdOf(state) !== executionId
			|| state.overview.principal !== principal || state.overview.workspace !== workspace) return null;
		const report = state.output?.result?.report;
		if (typeof report !== 'string' || !report.trim()
			|| new TextEncoder().encode(report).byteLength > MAX_REPORT_BYTES) return null;
		reports.set(key, report);
		if (reports.size > 64) reports.delete(reports.keys().next().value!);
		return report;
	})().catch(() => null).finally(() => pending.delete(key));
	pending.set(key, request);
	return request;
}
