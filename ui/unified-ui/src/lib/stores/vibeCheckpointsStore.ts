/**
 * VibeDev major checkpoints — one per checks-passing applied code change (see
 * `docs/archive/plans/2026-06-18-major-checkpoints.md`). Each is a known-good, rewindable state
 * carrying its git side-ref sha / apply snapshot id / Pi session id. Read straight from the
 * durable `CheckpointStore` via `GET /vibedev/runs/{task}/checkpoints` (best-effort), so a
 * refreshed or finished run repopulates its rail nodes.
 */
import { get, writable } from 'svelte/store';

import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';

export interface VibeCheckpoint {
	id: string;
	name: string;
	kind: 'baseline' | 'major' | string;
	proposal_id?: string | null;
	/** git side-ref commit (rewindable via checkout); absent when the project isn't a git repo. */
	git_sha?: string | null;
	snapshot_id?: string | null;
	pi_session_id?: string | null;
	applied_files?: string[];
	task_id: string;
	execution_id?: string | null;
	created_at: string;
}

/** The active run's checkpoints, newest first. */
export const vibeCheckpoints = writable<VibeCheckpoint[]>([]);

let inFlight: string | null = null;

function scopeHeaders(): HeadersInit {
	const scope = get(scopeIdentityStore);
	const headers: Record<string, string> = {};
	return headers;
}

/**
 * Fetch the run's checkpoints. Clears on no run. Guards against a run-switch race (a stale
 * response for a previous task is dropped). Never throws — the rail just shows no nodes.
 */
export async function fetchVibeCheckpoints(taskId: string | null): Promise<void> {
	if (!taskId) {
		inFlight = null;
		vibeCheckpoints.set([]);
		return;
	}
	inFlight = taskId;
	try {
		const res = await fetch(
			`/api/magician/v2/vibedev/runs/${encodeURIComponent(taskId)}/checkpoints`,
			{ headers: scopeHeaders() }
		);
		if (inFlight !== taskId) return; // run switched mid-flight
		if (!res.ok) {
			vibeCheckpoints.set([]);
			return;
		}
		const body = await res.json();
		vibeCheckpoints.set(Array.isArray(body?.checkpoints) ? body.checkpoints : []);
	} catch {
		if (inFlight === taskId) vibeCheckpoints.set([]);
	}
}

/**
 * Fetch checkpoints for a whole run-CHAIN (the root run + its threaded follow-up turns) and
 * merge them, newest first. A "run" in the cockpit is a chain of turns on one repo; checkpoints
 * are stamped per turn (`task_id`), so fetching only the active turn shows just that turn's
 * checkpoints and the earlier turns' rewind points look "overwritten". This unions every turn's
 * checkpoints (deduped by checkpoint id), so all of a run's rewind points stay visible regardless
 * of which turn minted them. Shares the `inFlight` latest-call token with `fetchVibeCheckpoints`
 * so whichever call fired last wins (no stale overwrite). Never throws.
 */
export async function fetchVibeCheckpointsForChain(taskIds: string[]): Promise<void> {
	const ids = Array.from(new Set(taskIds.filter((id): id is string => Boolean(id))));
	if (ids.length === 0) {
		inFlight = null;
		vibeCheckpoints.set([]);
		return;
	}
	const token = `chain:${ids.slice().sort().join(',')}`;
	inFlight = token;
	try {
		const lists = await Promise.all(
			ids.map(async (id) => {
				try {
					const res = await fetch(
						`/api/magician/v2/vibedev/runs/${encodeURIComponent(id)}/checkpoints`,
						{ headers: scopeHeaders() }
					);
					if (!res.ok) return [] as VibeCheckpoint[];
					const body = await res.json();
					return Array.isArray(body?.checkpoints) ? (body.checkpoints as VibeCheckpoint[]) : [];
				} catch {
					return [] as VibeCheckpoint[];
				}
			})
		);
		if (inFlight !== token) return; // run switched mid-flight
		const merged = new Map<string, VibeCheckpoint>();
		for (const list of lists) {
			for (const cp of list) {
				if (cp?.id) merged.set(cp.id, cp);
			}
		}
		vibeCheckpoints.set(
			Array.from(merged.values()).sort((a, b) =>
				(b.created_at || '').localeCompare(a.created_at || '')
			)
		);
	} catch {
		if (inFlight === token) vibeCheckpoints.set([]);
	}
}

export interface RevertResult {
	ok: boolean;
	/** git side-ref of the pre-revert working tree — lets the user undo the rewind. */
	undo_ref?: string | null;
	error?: string;
}

/**
 * Rewind the run's project to a checkpoint (POST .../checkpoints/{id}/revert). The backend first
 * snapshots the CURRENT tree to a side-ref (`undo_ref`, so the rewind is undoable), restores the
 * checkpoint's files, and queues its Pi session to resume on the next coding turn. Re-fetches the
 * checkpoint list on success so the rail reflects the new state. Never throws.
 */
export async function revertVibeCheckpoint(
	taskId: string,
	checkpointId: string
): Promise<RevertResult> {
	try {
		const res = await fetch(
			`/api/magician/v2/vibedev/runs/${encodeURIComponent(taskId)}/checkpoints/${encodeURIComponent(
				checkpointId
			)}/revert`,
			{ method: 'POST', headers: scopeHeaders() }
		);
		const body = await res.json().catch(() => ({}));
		if (!res.ok) {
			return { ok: false, error: body?.message || body?.error || `HTTP ${res.status}` };
		}
		await fetchVibeCheckpoints(taskId);
		return { ok: true, undo_ref: body?.undo_ref ?? null };
	} catch (error) {
		return { ok: false, error: error instanceof Error ? error.message : String(error) };
	}
}
