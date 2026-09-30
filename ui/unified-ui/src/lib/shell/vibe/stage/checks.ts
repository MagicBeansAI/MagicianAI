/**
 * VibeDev self-heal client (S1/S2). Talks to the project-info + check
 * endpoints: project shape (kind / previewable / available checks) and running
 * build/test/lint/typecheck in the shadow workspace.
 */
import { timedFetch, LONG_FETCH_TIMEOUT_MS } from '$lib/shared/fetch';
import { appendCurrentScopeQuery, scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';

export type ProjectKind = 'node' | 'rust' | 'python' | 'static' | 'unknown';

export interface CheckSpec {
	kind: string;
	display: string;
}

export interface ProjectInfo {
	project_kind: ProjectKind;
	previewable: boolean;
	checks: CheckSpec[];
}

export interface CheckResult {
	kind: string;
	command: string;
	ok: boolean;
	exit_code: number | null;
	timed_out: boolean;
	output_tail: string;
}

function scopedUrl(path: string): string {
	const query = appendCurrentScopeQuery().toString();
	return query ? `${path}?${query}` : path;
}

/** A repo file's content for the Code tab's browse view (#5). */
export interface ProjectFile {
	path: string;
	content?: string;
	/** Set when the file has a NUL byte in its first chunk — not rendered as text. */
	binary?: boolean;
	/** Set when the file exceeds the server's preview cap. */
	too_large?: boolean;
	size?: number;
}

/** The repo's full file tree (sorted repo-relative paths) for the Code tab's Browse mode. */
export async function fetchProjectFiles(
	projectId: string
): Promise<{ files: string[]; truncated: boolean } | null> {
	if (!projectId) return null;
	try {
		const response = await timedFetch(
			scopedUrl(`/api/magician/v2/vibedev/projects/${encodeURIComponent(projectId)}/files`),
			{ headers: scopedRequestHeaders() }
		);
		if (!response.ok) return null;
		const body = (await response.json()) as { files?: string[]; truncated?: boolean };
		return { files: body.files ?? [], truncated: Boolean(body.truncated) };
	} catch {
		return null;
	}
}

/** One repo file's content (repo-scoped on the server). Returns null on error/not-found. */
export async function fetchProjectFile(projectId: string, path: string): Promise<ProjectFile | null> {
	if (!projectId || !path) return null;
	try {
		const query = appendCurrentScopeQuery();
		query.set('path', path);
		const response = await timedFetch(
			`/api/magician/v2/vibedev/projects/${encodeURIComponent(projectId)}/file?${query.toString()}`,
			{ headers: scopedRequestHeaders() }
		);
		if (!response.ok) return null;
		return (await response.json()) as ProjectFile;
	} catch {
		return null;
	}
}

/** A captured preview screenshot in a project's visual-correction history (#25). */
export interface Screenshot {
	id: string;
	label?: string | null;
	created_at_ms: number;
	stored_name: string;
	session_id: string;
	/** Path the existing `/chat/sessions/{id}/outputs/{name}` route serves the PNG from. */
	outputs_path: string;
}

/** The project's screenshot history, newest first. Empty on error / none captured. */
export async function fetchProjectScreenshots(projectId: string): Promise<Screenshot[]> {
	if (!projectId) return [];
	try {
		const response = await timedFetch(
			scopedUrl(`/api/magician/v2/vibedev/projects/${encodeURIComponent(projectId)}/screenshots`),
			{ headers: scopedRequestHeaders() }
		);
		if (!response.ok) return [];
		const body = (await response.json()) as { screenshots?: Screenshot[] };
		return body.screenshots ?? [];
	} catch {
		return [];
	}
}

/** Append the current scope as query params to an output/image path so `<img src>` can load it. */
export function scopedImageUrl(path: string): string {
	return scopedUrl(path);
}

export async function fetchProjectInfo(projectId: string): Promise<ProjectInfo | null> {
	if (!projectId) return null;
	try {
		const response = await timedFetch(
			scopedUrl(`/api/magician/v2/vibedev/projects/${encodeURIComponent(projectId)}/info`),
			{ headers: scopedRequestHeaders() }
		);
		if (!response.ok) return null;
		return (await response.json()) as ProjectInfo;
	} catch {
		return null;
	}
}

export async function runCheck(
	projectId: string,
	kind?: string
): Promise<{ results: CheckResult[]; note?: string } | { error: string }> {
	if (!projectId) return { error: 'missing project id' };
	try {
		const response = await timedFetch(
			scopedUrl(`/api/magician/v2/vibedev/projects/${encodeURIComponent(projectId)}/check`),
			{
				method: 'POST',
				headers: scopedRequestHeaders({ 'Content-Type': 'application/json' }),
				body: JSON.stringify({ kind: kind ?? 'all' }),
				timeoutMs: LONG_FETCH_TIMEOUT_MS
			}
		);
		const payload = (await response.json().catch(() => null)) as
			| { results?: CheckResult[]; note?: string; message?: string; error?: string }
			| null;
		if (!response.ok) {
			return { error: payload?.message || payload?.error || `HTTP ${response.status}` };
		}
		return { results: payload?.results ?? [], note: payload?.note };
	} catch (error) {
		return { error: error instanceof Error ? error.message : String(error) };
	}
}
