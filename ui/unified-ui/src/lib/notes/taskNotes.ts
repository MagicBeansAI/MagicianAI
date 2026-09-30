export type TaskNotePublishMode = 'compact' | 'standard' | 'diagnostic';

export interface TaskNoteAsset {
	source_output_id: string;
	source_relative_path: string;
	path: string;
	media_type: string;
	bytes: number;
	content_hash: string;
}

export interface TaskNoteItem {
	projection_id: string;
	schema: string;
	task_id: string;
	title: string;
	status: string;
	agent_id: string;
	thread_id: string;
	mode: TaskNotePublishMode;
	requested_provider: string;
	provider: string;
	used_fallback: boolean;
	fallback_reason?: string | null;
	task_created_at: string;
	task_due_date?: string | null;
	task_completed_at?: string | null;
	source_updated_at: string;
	published_at: string;
	date: string;
	tags: string[];
	note_path: string;
	open_url?: string | null;
	assets: TaskNoteAsset[];
}

export interface TaskNotePage {
	items: TaskNoteItem[];
	offset: number;
	limit: number;
	total: number;
	has_more: boolean;
}

export interface TaskNoteBackfillResult {
	published: TaskNoteItem[];
	skipped_task_ids: string[];
	errors: Array<{ task_id: string; error: string }>;
	pagination: {
		total: number;
		completed_total: number;
		offset: number;
		limit: number;
		has_more: boolean;
		next_offset?: number | null;
	};
}

async function apiError(response: Response): Promise<Error> {
	try {
		const payload = await response.json() as { message?: string; error?: string };
		return new Error(payload.message || payload.error || `Request failed (${response.status})`);
	} catch {
		return new Error(`Request failed (${response.status})`);
	}
}

export async function fetchTaskNotes(options: {
	offset?: number;
	limit?: number;
	query?: string;
	signal?: AbortSignal;
} = {}): Promise<TaskNotePage> {
	const params = new URLSearchParams();
	params.set('offset', String(options.offset ?? 0));
	params.set('limit', String(options.limit ?? 20));
	if (options.query?.trim()) params.set('q', options.query.trim());
	const response = await fetch(`/api/magician/v2/notes/published-tasks?${params.toString()}`, {
		signal: options.signal
	});
	if (!response.ok) throw await apiError(response);
	return await response.json() as TaskNotePage;
}

export async function publishTaskToNotes(
	taskId: string,
	options: { mode?: TaskNotePublishMode; includeAssets?: boolean } = {}
): Promise<TaskNoteItem> {
	const response = await fetch(
		`/api/magician/v2/notes/publish/task/${encodeURIComponent(taskId)}`,
		{
			method: 'POST',
			headers: { 'content-type': 'application/json' },
			body: JSON.stringify({
				mode: options.mode,
				include_assets: options.includeAssets
			})
		}
	);
	if (!response.ok) throw await apiError(response);
	return await response.json() as TaskNoteItem;
}

export async function backfillTaskNotes(limit = 25): Promise<TaskNoteBackfillResult> {
	const response = await fetch('/api/magician/v2/notes/publish/tasks/backfill', {
		method: 'POST',
		headers: { 'content-type': 'application/json' },
		body: JSON.stringify({
			limit,
			only_unpublished: true
		})
	});
	if (!response.ok) throw await apiError(response);
	return await response.json() as TaskNoteBackfillResult;
}

export async function promoteTaskNoteToMemory(
	taskId: string,
	summary?: string
): Promise<{ candidate: { id: string; state: string }; note: TaskNoteItem }> {
	const response = await fetch(
		`/api/magician/v2/notes/published-tasks/${encodeURIComponent(taskId)}/promote-memory`,
		{
			method: 'POST',
			headers: { 'content-type': 'application/json' },
			body: JSON.stringify({
				summary: summary?.trim() || undefined
			})
		}
	);
	if (!response.ok) throw await apiError(response);
	return await response.json() as {
		candidate: { id: string; state: string };
		note: TaskNoteItem;
	};
}
