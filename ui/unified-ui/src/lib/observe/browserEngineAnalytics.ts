export type BrowserEngineOutcome = 'all' | 'success' | 'failure';

export interface BrowserEngineUsageRecord {
	id: string;
	occurred_at_ms: number;
	session_id: string;
	execution_id: string | null;
	task_id: string | null;
	work_kind: string;
	work_id: string;
	engine: string;
	fallback_from: string | null;
	connection_mode: string;
	operation: string;
	url: string | null;
	success: boolean;
	elapsed_ms: number;
	error_class: string | null;
}

export interface BrowserEngineSummary {
	engine: string;
	attempts: number;
	successes: number;
	failures: number;
	success_rate: number;
	average_elapsed_ms: number;
}

export interface BrowserEngineUsagePage {
	items: BrowserEngineUsageRecord[];
	total_count: number;
	limit: number;
	offset: number;
	has_more: boolean;
	summary: BrowserEngineSummary[];
}

export interface BrowserEngineUsageRequest {
	page: number;
	pageSize: number;
	engine?: string;
	outcome?: BrowserEngineOutcome;
	signal?: AbortSignal;
}

export async function fetchBrowserEngineUsage(
	request: BrowserEngineUsageRequest
): Promise<BrowserEngineUsagePage> {
	const page = Math.max(0, Math.floor(request.page));
	const pageSize = Math.max(1, Math.floor(request.pageSize));
	const params = new URLSearchParams({
		limit: String(pageSize),
		offset: String(page * pageSize),
		outcome: request.outcome ?? 'all'
	});
	const engine = request.engine?.trim();
	if (engine) params.set('engine', engine);
	const response = await fetch(`/api/magician/v2/browser/engine-usage?${params}`, {
		signal: request.signal
	});
	if (!response.ok) {
		const payload = await response.json().catch(() => ({}));
		throw new Error(
			typeof payload?.error === 'string'
				? payload.error
				: `Browser engine analytics failed (${response.status})`
		);
	}
	return (await response.json()) as BrowserEngineUsagePage;
}
