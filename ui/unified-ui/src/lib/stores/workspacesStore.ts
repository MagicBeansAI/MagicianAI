/**
 * Workspace management — `GET/POST /workspaces`, `PATCH/DELETE /workspaces/{id}`.
 *
 * Every call rides the bearer the scoped fetch installs; the server resolves
 * the principal from it, so these functions never name one.
 */
import { timedFetch } from '$lib/shared/fetch';

const WORKSPACES_URL = '/api/magician/v2/workspaces';

/** One row of `GET /workspaces` — the summary card shape. */
export interface WorkspaceCard {
	id: string;
	display_name: string;
	description?: string | null;
	is_default: boolean;
	/** Best-effort directory-level counts. */
	agent_count: number;
	active_task_count: number;
	frozen: boolean;
	last_activity?: string | null;
}

/**
 * A failed workspace request, keeping the server's status and error code.
 * The codes the UI acts on:
 * - `workspace_has_live_state` (409, delete) — the workspace still holds data;
 *   only a delete *with* data can remove it
 * - `workspace_pending_purge` (409, create) — the name belongs to a workspace
 *   deleted with its data, free again after Magician restarts
 * - `workspace_exists` (409), `invalid_workspace_slug` (400),
 *   `default_workspace_protected` (400), `workspace_not_found` (404)
 */
export class WorkspaceRequestError extends Error {
	readonly status: number;
	readonly code: string | null;

	constructor(message: string, status: number, code: string | null) {
		super(message);
		this.name = 'WorkspaceRequestError';
		this.status = status;
		this.code = code;
	}
}

async function requestError(response: Response, fallback: string): Promise<WorkspaceRequestError> {
	let message = `${fallback} (${response.status})`;
	let code: string | null = null;
	try {
		const payload = (await response.json()) as { message?: unknown; error?: unknown };
		if (typeof payload.error === 'string' && payload.error.trim()) code = payload.error;
		if (typeof payload.message === 'string' && payload.message.trim()) message = payload.message;
	} catch {
		// Keep the status-derived message.
	}
	return new WorkspaceRequestError(message, response.status, code);
}

function asCard(value: unknown): WorkspaceCard | null {
	if (!value || typeof value !== 'object') return null;
	const row = value as Record<string, unknown>;
	if (typeof row.id !== 'string' || !row.id) return null;
	return {
		id: row.id,
		display_name: typeof row.display_name === 'string' && row.display_name ? row.display_name : row.id,
		description: typeof row.description === 'string' ? row.description : null,
		is_default: row.is_default === true,
		agent_count: typeof row.agent_count === 'number' ? row.agent_count : 0,
		active_task_count: typeof row.active_task_count === 'number' ? row.active_task_count : 0,
		frozen: row.frozen === true,
		last_activity: typeof row.last_activity === 'string' ? row.last_activity : null
	};
}

/** The default first, then by display name. */
export function sortWorkspaces(cards: WorkspaceCard[]): WorkspaceCard[] {
	return [...cards].sort((a, b) => {
		if (a.is_default !== b.is_default) return a.is_default ? -1 : 1;
		return a.display_name.localeCompare(b.display_name);
	});
}

export async function listWorkspaces(): Promise<WorkspaceCard[]> {
	const response = await timedFetch(WORKSPACES_URL, { headers: { accept: 'application/json' } });
	if (!response.ok) throw await requestError(response, 'Could not load workspaces');
	const body = (await response.json()) as unknown;
	const rows = Array.isArray(body)
		? body
		: Array.isArray((body as { workspaces?: unknown })?.workspaces)
			? (body as { workspaces: unknown[] }).workspaces
			: [];
	return sortWorkspaces(rows.map(asCard).filter((card): card is WorkspaceCard => card !== null));
}

export async function createWorkspace(input: {
	slug: string;
	display_name: string;
	description?: string | null;
}): Promise<void> {
	const response = await timedFetch(WORKSPACES_URL, {
		method: 'POST',
		headers: { 'content-type': 'application/json' },
		body: JSON.stringify({
			slug: input.slug.trim(),
			display_name: input.display_name.trim(),
			description: input.description?.trim() || null
		})
	});
	if (!response.ok) throw await requestError(response, 'Could not create the workspace');
}

/**
 * Rename, and set or clear the description. `description: null` clears it;
 * leaving it out keeps it (the server distinguishes absent from null).
 */
export async function updateWorkspace(
	id: string,
	input: { display_name?: string; description?: string | null }
): Promise<void> {
	const body: Record<string, unknown> = {};
	if (input.display_name !== undefined) body.display_name = input.display_name.trim();
	if (input.description !== undefined) body.description = input.description?.trim() || null;
	const response = await timedFetch(`${WORKSPACES_URL}/${encodeURIComponent(id)}`, {
		method: 'PATCH',
		headers: { 'content-type': 'application/json' },
		body: JSON.stringify(body)
	});
	if (!response.ok) throw await requestError(response, 'Could not update the workspace');
}

/**
 * Delete a workspace. Without `purge` the server refuses one that still holds
 * data (`workspace_has_live_state`). With `purge` it accepts it: access ends at
 * once and the files are removed the next time Magician starts — never on the
 * spot, because the running service still holds handles into them.
 */
export async function deleteWorkspace(
	id: string,
	options: { purge?: boolean } = {}
): Promise<{ dataRemoval: 'none' | 'scheduled_for_next_start' }> {
	const suffix = options.purge ? '?purge=true' : '';
	const response = await timedFetch(`${WORKSPACES_URL}/${encodeURIComponent(id)}${suffix}`, {
		method: 'DELETE'
	});
	if (!response.ok) throw await requestError(response, 'Could not delete the workspace');
	return { dataRemoval: options.purge ? 'scheduled_for_next_start' : 'none' };
}

/**
 * The server's rule, exactly: 1–32 characters of `a-z`, `0-9`, `_` or `-`.
 * Workspace ids are directory names, so the rule is not negotiable.
 */
export function isValidWorkspaceSlug(slug: string): boolean {
	return /^[a-z0-9_-]{1,32}$/.test(slug);
}

/** A valid slug derived from a display name, or '' when nothing usable is left. */
export function suggestWorkspaceSlug(displayName: string): string {
	return displayName
		.normalize('NFKD')
		.replace(/[̀-ͯ]/g, '')
		.toLowerCase()
		.replace(/[^a-z0-9_-]+/g, '-')
		.replace(/-{2,}/g, '-')
		.replace(/^-+|-+$/g, '')
		.slice(0, 32)
		.replace(/-+$/g, '');
}
