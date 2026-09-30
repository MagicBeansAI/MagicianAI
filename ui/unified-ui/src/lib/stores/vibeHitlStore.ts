/**
 * VibeDev HITL store — diff/decision review state + run-chain scoping.
 *
 * Lifts the HITL orchestration out of the `+page.svelte` monolith: derive
 * review rows from `pendingHitlEntries`, scope/sort them to the active run's
 * task chain, and apply / reject / approve-all / auto-apply them. The logic is
 * moved verbatim; only the host changes. The cockpit's StageDiff +
 * inline DiffCards + HitlResponder all read these.
 */
import { derived, get, writable, type Readable } from 'svelte/store';
import {
	pendingHitlEntries,
	type HitlPendingEntry
} from '$lib/stores/pendingHitlStore';
import { hitlRequestFromCanonicalEvent, postHitlResponse } from '$lib/hitl/adapters';
import { respondToHitl } from '$lib/hitl/respondToHitl';
import type { HitlRequest, HitlResolveOutcome } from '$lib/hitl/types';
import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import type { Task } from '$lib/stores/taskStore';
import { browser } from '$app/environment';
import { timedFetch, LONG_FETCH_TIMEOUT_MS } from '$lib/shared/fetch';

export interface VibeRow {
	key: string;
	entry: HitlPendingEntry;
	request: HitlRequest;
}

export interface VibeChangedFile {
	path: string;
	additions: number;
	deletions: number;
	active: boolean;
}

const VIBEDEV_PARENT_TASK_PREFIX = 'Parent task:';
const VIBEDEV_PROJECT_PREFIX = 'VibeDev project:';

// ── pure helpers (verbatim from the monolith) ────────────────────────────────

export function rowKey(request: HitlRequest): string {
	return (
		request.schema.proposal_id ??
		request.schema.transaction_id ??
		request.identifiers.correlation_id ??
		request.id
	);
}

export function isProposalDiff(request: HitlRequest): boolean {
	return (
		request.input_type === 'diff_approval' &&
		(request.schema.approval_source === 'proposal' || Boolean(request.schema.proposal_id))
	);
}

export function parentTaskIdFromDescription(description: string | undefined): string | null {
	if (!description) return null;
	for (const line of description.split('\n')) {
		const trimmed = line.trim();
		if (!trimmed.startsWith(VIBEDEV_PARENT_TASK_PREFIX)) continue;
		const value = trimmed.slice(VIBEDEV_PARENT_TASK_PREFIX.length).trim();
		return value.length > 0 ? value : null;
	}
	return null;
}

export function projectIdFromDescription(description: string | undefined): string | null {
	if (!description) return null;
	for (const line of description.split('\n')) {
		const trimmed = line.trim();
		if (!trimmed.startsWith(VIBEDEV_PROJECT_PREFIX)) continue;
		const value = trimmed.slice(VIBEDEV_PROJECT_PREFIX.length).trim();
		return value.length > 0 ? value : null;
	}
	return null;
}

/** Walk the parent/child task graph from the active run, collecting its ids. */
export function buildVibeRunChainIds(activeTask: Task | null, tasks: Task[]): Set<string> {
	const ids = new Set<string>();
	if (!activeTask) return ids;
	const byId = new Map(tasks.map((task) => [task.id, task]));
	const childrenByParent = new Map<string, string[]>();
	for (const task of tasks) {
		const parentId = parentTaskIdFromDescription(task.description);
		if (!parentId) continue;
		const children = childrenByParent.get(parentId) ?? [];
		children.push(task.id);
		childrenByParent.set(parentId, children);
	}
	const visit = (taskId: string, depth: number) => {
		if (!taskId || ids.has(taskId) || depth > 24) return;
		ids.add(taskId);
		const task = byId.get(taskId);
		const parentId = parentTaskIdFromDescription(task?.description);
		if (parentId) visit(parentId, depth + 1);
		for (const childId of childrenByParent.get(taskId) ?? []) visit(childId, depth + 1);
	};
	visit(activeTask.id, 0);
	return ids;
}

function rowTaskId(row: VibeRow): string | null {
	return row.request.scope.task_id ?? null;
}

export function sortRowsForActiveRun(rows: VibeRow[], chainIds: Set<string>): VibeRow[] {
	if (chainIds.size === 0) return rows;
	return [...rows].sort((a, b) => {
		const aActive = rowTaskId(a) ? chainIds.has(rowTaskId(a) as string) : false;
		const bActive = rowTaskId(b) ? chainIds.has(rowTaskId(b) as string) : false;
		if (aActive !== bActive) return aActive ? -1 : 1;
		return b.entry.at - a.entry.at;
	});
}

export function filterRowsForActiveRun(rows: VibeRow[], chainIds: Set<string>): VibeRow[] {
	if (chainIds.size === 0) return rows;
	return rows.filter((row) => {
		const taskId = rowTaskId(row);
		return taskId ? chainIds.has(taskId) : false;
	});
}

export function buildChangedFiles(rows: VibeRow[], chainIds: Set<string>): VibeChangedFile[] {
	const byPath = new Map<string, VibeChangedFile>();
	for (const row of rows) {
		const taskId = rowTaskId(row);
		const active = Boolean(taskId && chainIds.has(taskId));
		for (const file of row.request.schema.files ?? []) {
			if (!file.path) continue;
			const existing = byPath.get(file.path);
			if (existing) {
				existing.additions += file.additions;
				existing.deletions += file.deletions;
				existing.active = existing.active || active;
			} else {
				byPath.set(file.path, {
					path: file.path,
					additions: file.additions,
					deletions: file.deletions,
					active
				});
			}
		}
	}
	return Array.from(byPath.values()).sort((a, b) => {
		if (a.active !== b.active) return a.active ? -1 : 1;
		return a.path.localeCompare(b.path);
	});
}

// ── derived review rows ──────────────────────────────────────────────────────

/** All HITL entries as VibeRows, newest-first (the monolith's `rows`). */
export const vibeHitlRows: Readable<VibeRow[]> = derived(pendingHitlEntries, ($entries) =>
	$entries
		.map((entry): VibeRow | null => {
			const request = entry.raw ? hitlRequestFromCanonicalEvent(entry.raw) : null;
			if (!request) return null;
			return {
				key:
					request.schema.proposal_id ??
					request.schema.transaction_id ??
					request.identifiers.correlation_id ??
					request.id,
				entry,
				request
			};
		})
		.filter((row): row is VibeRow => row !== null)
		.sort((a, b) => b.entry.at - a.entry.at)
);

export const vibeDiffRows: Readable<VibeRow[]> = derived(vibeHitlRows, ($rows) =>
	$rows.filter((row) => row.request.input_type === 'diff_approval')
);

export const vibeOtherRows: Readable<VibeRow[]> = derived(vibeHitlRows, ($rows) =>
	$rows.filter((row) => row.request.input_type !== 'diff_approval')
);

// ── action state + methods ───────────────────────────────────────────────────

export interface HitlActionState {
	actingKeys: Set<string>;
	bulkApplying: boolean;
	statusText: string;
	errorText: string;
	autoApplyInFlight: boolean;
	autoApplyAttemptedKeys: Set<string>;
}

function emptyActionState(): HitlActionState {
	return {
		actingKeys: new Set(),
		bulkApplying: false,
		statusText: '',
		errorText: '',
		autoApplyInFlight: false,
		autoApplyAttemptedKeys: new Set()
	};
}

export const hitlActionState = writable<HitlActionState>(emptyActionState());

function scopeHeaders(): HeadersInit {
	const scope = get(scopeIdentityStore);
	const headers: Record<string, string> = {};
	return headers;
}

function outcomeError(outcome: HitlResolveOutcome): string | null {
	if (outcome.ok) return null;
	if ('cancelled' in outcome) return 'cancelled';
	return outcome.message || `HTTP ${outcome.status}`;
}

function setActing(key: string, active: boolean): void {
	hitlActionState.update((state) => {
		const next = new Set(state.actingKeys);
		if (active) next.add(key);
		else next.delete(key);
		return { ...state, actingKeys: next };
	});
}

export async function resolveDiff(
	request: HitlRequest,
	selectedId: 'apply' | 'reject',
	selectedPaths?: string[]
): Promise<boolean> {
	const key = rowKey(request);
	hitlActionState.update((s) => ({ ...s, errorText: '', statusText: '' }));
	setActing(key, true);
	try {
		const outcome = await postHitlResponse(
			request,
			{ type: 'choice', selected_id: selectedId },
			scopeHeaders(),
			{ selectedPaths }
		);
		const err = outcomeError(outcome);
		if (err) {
			hitlActionState.update((s) => ({ ...s, errorText: err }));
			return false;
		}
		hitlActionState.update((s) => ({
			...s,
			statusText: selectedId === 'apply' ? 'Applied.' : 'Rejected.'
		}));
		return true;
	} finally {
		setActing(key, false);
	}
}

export async function applyDiffFile(request: HitlRequest, path: string): Promise<void> {
	await resolveDiff(request, 'apply', [path]);
}

export async function rejectDiffFile(request: HitlRequest, path: string): Promise<void> {
	const remaining = (request.schema.files ?? [])
		.map((file) => file.path)
		.filter((candidate) => candidate && candidate !== path);
	if (remaining.length === 0) {
		await resolveDiff(request, 'reject');
		return;
	}
	await resolveDiff(request, 'apply', remaining);
}

export async function approveAll(rows: VibeRow[]): Promise<void> {
	const current = get(hitlActionState);
	if (current.bulkApplying || rows.length === 0) return;
	hitlActionState.update((s) => ({ ...s, bulkApplying: true, errorText: '', statusText: '' }));
	let applied = 0;
	const failures: string[] = [];
	for (const row of rows) {
		const key = rowKey(row.request);
		setActing(key, true);
		const outcome = await postHitlResponse(
			row.request,
			{ type: 'choice', selected_id: 'apply' },
			scopeHeaders()
		);
		const err = outcomeError(outcome);
		if (err) failures.push(`${key}: ${err}`);
		else applied += 1;
		setActing(key, false);
	}
	hitlActionState.update((s) => ({
		...s,
		bulkApplying: false,
		errorText: failures.length > 0 ? failures.slice(0, 3).join(' · ') : '',
		statusText:
			failures.length === 0
				? `Applied ${applied} change set${applied === 1 ? '' : 's'}.`
				: `Applied ${applied}; ${failures.length} failed.`
	}));
}

export async function respondToVibeRow(row: VibeRow): Promise<void> {
	hitlActionState.update((s) => ({ ...s, errorText: '', statusText: '' }));
	const outcome = await respondToHitl(row.request, scopeHeaders());
	const err = outcomeError(outcome);
	if (err && err !== 'cancelled') {
		hitlActionState.update((s) => ({ ...s, errorText: err }));
	}
}

/** Auto-apply eligible proposal diffs once each (used when auto-apply is ON). */
export async function autoApplyEligibleDiffs(rows: VibeRow[]): Promise<void> {
	const current = get(hitlActionState);
	if (current.autoApplyInFlight) return;
	hitlActionState.update((s) => ({ ...s, autoApplyInFlight: true }));
	try {
		let applied = 0;
		for (const row of rows) {
			const key = rowKey(row.request);
			const state = get(hitlActionState);
			if (state.autoApplyAttemptedKeys.has(key) || state.actingKeys.has(key) || state.bulkApplying) {
				continue;
			}
			hitlActionState.update((s) => {
				const attempted = new Set(s.autoApplyAttemptedKeys);
				attempted.add(key);
				return { ...s, autoApplyAttemptedKeys: attempted };
			});
			const ok = await resolveDiff(row.request, 'apply');
			if (ok) applied += 1;
		}
		if (applied > 0) {
			hitlActionState.update((s) => ({
				...s,
				statusText: `Auto-applied ${applied} code proposal${applied === 1 ? '' : 's'}.`
			}));
		}
	} finally {
		hitlActionState.update((s) => ({ ...s, autoApplyInFlight: false }));
	}
}

/** Clear the attempted-keys memo (e.g. when re-enabling auto-apply). */
export function resetAutoApplyMemo(): void {
	hitlActionState.update((s) => ({ ...s, autoApplyAttemptedKeys: new Set() }));
}

// ── historical (durable) proposal rows — finished-run diff/code/tests ─────────
// The pending HITL store DROPS a diff_approval on resolution, so a finished run's
// diffs vanish from `vibeDiffRows`. These rows are reconstructed from the durable
// `CodeChangeProposal` store (`GET /vibedev/runs/{task_id}/proposals`) on run-open,
// so the Diff/Code panels repopulate on refresh. They are DISPLAY-ONLY (read-only,
// already resolved) — they must NEVER feed auto-apply (`proposalDiffRows`).

interface DurableProposalFile {
	path: string;
	status?: string;
	additions?: number;
	deletions?: number;
	unified_diff?: string;
}
interface DurableProposal {
	id: string;
	summary?: string;
	status?: string;
	files?: DurableProposalFile[];
	test_evidence?: unknown[];
	created_at?: string;
}

export const historicalProposalRows = writable<VibeRow[]>([]);
let historicalProposalsTaskId: string | null = null;

function proposalToVibeRow(p: DurableProposal, taskId: string): VibeRow {
	const atMs = p.created_at ? Date.parse(p.created_at) : NaN;
	const files = (p.files ?? []).map((f) => ({
		path: f.path,
		status: f.status ?? 'M',
		additions: f.additions ?? 0,
		deletions: f.deletions ?? 0,
		unified_diff: f.unified_diff ?? ''
	}));
	const request: HitlRequest = {
		id: p.id,
		source: 'diff_approval',
		input_type: 'diff_approval',
		schema: { proposal_id: p.id, approval_source: 'proposal', rationale: p.summary, files },
		prompt: p.summary ?? '',
		scope: { task_id: taskId },
		identifiers: { correlation_id: p.id },
		at: Number.isFinite(atMs) ? atMs : undefined,
		// `historical: true` marks the row read-only for render surfaces; `status`
		// lets them show Applied/Rejected instead of Apply/Reject affordances.
		raw: { historical: true, status: p.status ?? 'resolved', test_evidence: p.test_evidence ?? [] }
	};
	const entry: HitlPendingEntry = {
		correlation_id: p.id,
		at: request.at ?? 0,
		raw: request.raw as Record<string, unknown>
	};
	return { key: p.id, entry, request };
}

/**
 * Hydrate the durable diff/code/tests for a run on open. Best-effort + guarded
 * per-task (one fetch per run). Clears the rows when switching to a run with no
 * durable proposals (or to no run). A stale response after a fast run switch is
 * dropped via the `taskId` recheck.
 */
export async function hydrateRunProposals(taskId: string | null): Promise<void> {
	if (!browser || !taskId) {
		historicalProposalRows.set([]);
		historicalProposalsTaskId = null;
		return;
	}
	if (historicalProposalsTaskId === taskId) return;
	historicalProposalsTaskId = taskId;
	try {
		const response = await timedFetch(
			`/api/magician/v2/vibedev/runs/${encodeURIComponent(taskId)}/proposals`,
			{ headers: scopeHeaders(), timeoutMs: LONG_FETCH_TIMEOUT_MS }
		);
		if (historicalProposalsTaskId !== taskId) return; // run switched mid-flight
		if (!response.ok) {
			historicalProposalRows.set([]);
			return;
		}
		const body = await response.json();
		if (historicalProposalsTaskId !== taskId) return;
		const proposals: DurableProposal[] = Array.isArray(body?.proposals) ? body.proposals : [];
		historicalProposalRows.set(proposals.map((p) => proposalToVibeRow(p, taskId)));
	} catch {
		if (historicalProposalsTaskId === taskId) historicalProposalRows.set([]);
	}
}

/**
 * Merge durable (historical) rows with live `vibeDiffRows`, deduped by proposal/
 * transaction/correlation key. LIVE rows win — a still-pending diff keeps its
 * actionable HITL row rather than the read-only historical copy. For DISPLAY
 * surfaces only (diff cards / changed files); auto-apply must use live rows.
 */
export function mergeDiffRowsByProposal(historical: VibeRow[], live: VibeRow[]): VibeRow[] {
	const byKey = new Map<string, VibeRow>();
	for (const row of historical) byKey.set(rowKey(row.request), row);
	for (const row of live) byKey.set(rowKey(row.request), row);
	return [...byKey.values()];
}
