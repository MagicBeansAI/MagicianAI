/**
 * VibeDev interactive control-plane client.
 *
 * Steer / follow-up / stop a LIVE coding run by hitting
 * `POST /api/magician/v2/vibedev/runs/{runId}/control`. The backend reaches the
 * in-flight Pi turn through its process-global control registry (the key is
 * scope-qualified server-side). `delivered: false` means there's no live turn
 * for that id right now (it finished, or is between turns) — the caller decides
 * the fallback (e.g. queue a follow-up task instead).
 */
import { timedFetch } from '$lib/shared/fetch';
import { appendCurrentScopeQuery, scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';

export type CodingControlAction = 'steer' | 'follow_up' | 'stop';

export interface ControlResult {
	ok: boolean;
	delivered: boolean;
	status: number;
	error?: string;
}

export async function controlRun(
	runId: string,
	action: CodingControlAction,
	message?: string
): Promise<ControlResult> {
	if (!runId) return { ok: false, delivered: false, status: 0, error: 'missing run id' };
	const params = appendCurrentScopeQuery();
	const query = params.toString();
	const url = `/api/magician/v2/vibedev/runs/${encodeURIComponent(runId)}/control${query ? `?${query}` : ''}`;
	try {
		const response = await timedFetch(url, {
			method: 'POST',
			headers: scopedRequestHeaders({ 'Content-Type': 'application/json' }),
			body: JSON.stringify({ action, message })
		});
		const payload = (await response.json().catch(() => null)) as
			| { delivered?: boolean; reason?: string; message?: string; error?: string }
			| null;
		if (response.ok) {
			return { ok: true, delivered: payload?.delivered === true, status: response.status };
		}
		// 409 = no live run for this id (a normal, expected outcome).
		return {
			ok: false,
			delivered: false,
			status: response.status,
			error: payload?.reason || payload?.message || payload?.error || `HTTP ${response.status}`
		};
	} catch (error) {
		return {
			ok: false,
			delivered: false,
			status: 0,
			error: error instanceof Error ? error.message : String(error)
		};
	}
}
