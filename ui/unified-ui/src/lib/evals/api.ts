/**
 * Evals — scoped web API client for the `/v2/evals` surface.
 *
 * Wire shapes mirror `magician_v2::evals` (`registry.rs` for the lane, the
 * `EvalRunV1` record for a run). Three endpoints:
 *
 * - `GET  /v2/evals/lanes`        lanes + readiness + last run, plus the
 *                                 annotations that bound to nothing
 * - `POST /v2/evals/{lane}/run`   `{task_id, run_id}`; 409 when the lane is
 *                                 already running or is not ready
 * - `GET  /v2/evals/runs`         server-paged run records
 *
 * **Authorization is not set here.** `installScopedApiFetch` patches
 * `window.fetch` for every `/api/magician` request, strips legacy scope
 * selectors, and attaches the workspace-bound bearer automatically. Adding
 * caller-asserted scope by hand would be a second source of truth. `timedFetch`
 * still wraps the patched `fetch`, so
 * a request that never gets sent aborts instead of hanging a spinner forever.
 *
 * **Errors surface the server's own words.** The 409 body is the whole point
 * of the 409 — it names the in-flight task, or the services that are down —
 * and a bare "HTTP 409" would throw exactly the information the operator
 * needs. The status line is the last resort, not the first.
 */

import { timedFetch } from '$lib/shared/fetch';

import { normalizeCost } from './format';

const API_BASE = '/api/magician/v2/evals';

// ── Wire types ────────────────────────────────────────────────────────────────

/** `unknown` = the annotation did not declare a kind. Never runnable. */
export type EvalKind = 'harness' | 'live' | 'unknown';

/** Requirements the backend declares today. */
export type KnownEvalRequirement =
	| 'ollama'
	| 'magician'
	| 'magician_binary'
	| 'magicutor'
	| 'provider_keys';

/**
 * A declared requirement. Widened to `string` on purpose: the Makefile can
 * grow a `requires=` token before this file learns about it, and an unknown
 * token must still render (see `formatRequirement`) rather than vanish.
 */
export type EvalRequirement = KnownEvalRequirement | (string & {});

/**
 * Readiness as this module models it.
 *
 * The server does NOT send it nested: `LaneView` flattens `EvalLane` and puts
 * `ready`/`missing`/`runnable` beside those fields at the top level. This
 * object is assembled from them in `listEvalLanes`, because a lane and the
 * live probe of its services are two different facts with two different
 * lifetimes and reading `lane.readiness.ready` says so at every call site.
 */
export interface EvalReadiness {
	ready: boolean;
	/** Requirement tokens that failed to probe. Unprobeable counts as missing. */
	missing: string[];
}

/** The flat readiness fields `LaneView` actually puts on the wire. */
interface LaneViewReadinessFields {
	ready?: unknown;
	missing?: unknown;
	runnable?: unknown;
}

export type EvalRunStatus = 'passed' | 'failed' | 'interrupted';

/**
 * Cost of a run. `unknown` is a real, distinct outcome — the ledger did not
 * answer — and MUST NOT be rendered as `$0.00`. See `format.ts`.
 */
export type EvalCost = { kind: 'known'; usd: number } | { kind: 'unknown' };

export interface EvalRun {
	options?: EvalRunOptions | null;
	run_id: string;
	lane_id: string;
	/** The execution that ran it; null when the record outlived its task. */
	task_id: string | null;
	started_at_ms: number;
	duration_ms: number;
	status: EvalRunStatus;
	exit_code: number | null;
	services: string[];
	/** Link to the lane's own (non-uniform) report, when it wrote one. */
	report_href: string | null;
	cost: EvalCost;
}

export interface EvalLane {
	run_options?: 'memory_lifecycle' | null;
	id: string;
	target: string;
	kind: EvalKind;
	requires: EvalRequirement[];
	report_dir: string | null;
	desc: string | null;
	/**
	 * Set when the `## eval:` annotation was malformed. The lane is returned
	 * anyway — a lane thinned out of the grid looks identical to a lane that
	 * does not exist, which is the failure this page exists to prevent.
	 */
	parse_error: string | null;
	/** 1-based Makefile line of the annotation, so a fix has an address. */
	line: number;
	readiness: EvalReadiness;
	last_run: EvalRun | null;
}

/** An annotation that never became a lane — an eval silently missing. */
export interface OrphanedAnnotation {
	line: number;
	text: string;
	reason: string;
}

export interface EvalLanesResponse {
	lanes: EvalLane[];
	orphaned: OrphanedAnnotation[];
}

export interface EvalRunStarted {
	task_id: string;
	run_id: string;
}

export interface EvalRunsPage {
	runs: EvalRun[];
	total: number;
}

export interface EvalRunsQuery {
	lane?: string | null;
	fromMs?: number | null;
	toMs?: number | null;
	limit: number;
	offset: number;
}

// ── Error handling ────────────────────────────────────────────────────────────

/**
 * The server's message, or the status line only if there truly isn't one.
 *
 * The body is read exactly once as text and then parsed, rather than branching
 * on `content-type` — a JSON body served without the header still reaches the
 * operator, and a non-JSON body (a proxy's HTML error page) is surfaced as
 * itself instead of being swallowed.
 */
async function readEvalsApiError(response: Response): Promise<string> {
	let body = '';
	try {
		body = await response.text();
	} catch {
		return `HTTP ${response.status}`;
	}

	const trimmed = body.trim();
	if (!trimmed) return `HTTP ${response.status}`;

	try {
		const parsed = JSON.parse(trimmed) as { error?: unknown; message?: unknown };
		const message =
			typeof parsed?.error === 'string'
				? parsed.error
				: typeof parsed?.message === 'string'
					? parsed.message
					: '';
		if (message.trim()) return message.trim();
	} catch {
		// Not JSON — fall through and surface the raw body.
	}

	// Bound it: an HTML error page should inform, not flood the banner.
	return trimmed.length > 600 ? `${trimmed.slice(0, 600)}…` : trimmed;
}

async function requireOk(response: Response): Promise<Response> {
	if (!response.ok) throw new Error(await readEvalsApiError(response));
	return response;
}

async function getJson<T>(
	path: string,
	params?: URLSearchParams,
	signal?: AbortSignal
): Promise<T> {
	const query = params?.toString();
	const response = await requireOk(
		await timedFetch(`${API_BASE}${path}${query ? `?${query}` : ''}`, {
			headers: { Accept: 'application/json' },
			signal
		})
	);
	return (await response.json()) as T;
}

// ── Normalisation ─────────────────────────────────────────────────────────────

function asArray<T>(value: unknown): T[] {
	return Array.isArray(value) ? (value as T[]) : [];
}

/**
 * Re-seats a run's `cost` through `normalizeCost`, so a shape the backend did
 * not promise can only ever become *unknown* — never a fabricated `$0.00`.
 */
function normalizeRun(run: EvalRun): EvalRun {
	return { ...run, cost: normalizeCost((run as { cost?: unknown }).cost) };
}

// ── Endpoints ─────────────────────────────────────────────────────────────────

/**
 * `GET /lanes` — every annotated lane with its readiness and last run, plus
 * `orphaned`: annotations that bound to no target. Both lists default to empty
 * rather than throwing, so one absent field cannot blank the whole page.
 *
 * `ready`/`missing` are lifted out of the flattened `LaneView` into
 * `readiness`. Reading them off the nested object the server never sends is a
 * silent failure, not a loud one: every lane would render "not ready" with no
 * missing service named, and every Run button would be disabled — which looks
 * exactly like a machine with nothing running on it.
 */
export async function listEvalLanes(signal?: AbortSignal): Promise<EvalLanesResponse> {
	const body = await getJson<Partial<EvalLanesResponse>>('/lanes', undefined, signal);
	return {
		lanes: asArray<EvalLane & LaneViewReadinessFields>(body?.lanes).map((lane) => ({
			...lane,
			requires: asArray<EvalRequirement>(lane?.requires),
			readiness: {
				ready: lane?.ready === true,
				missing: asArray<string>(lane?.missing)
			},
			last_run: lane?.last_run ? normalizeRun(lane.last_run) : null
		})),
		orphaned: asArray<OrphanedAnnotation>(body?.orphaned)
	};
}

/**
 * `POST /{lane}/run` — starts the lane through the ordinary task system.
 *
 * 409 means either "already running" (the body names the in-flight task) or
 * "not ready" (the body names the missing services). Both arrive as the thrown
 * message; the caller shows it verbatim.
 */
export interface EvalRunOptions {
	profiles?: string[];
	repeats?: number;
	partition?: 'all' | 'development' | 'validation';
}

export async function runEvalLane(laneId: string, options: EvalRunOptions = {}): Promise<EvalRunStarted> {
	const response = await requireOk(
		await timedFetch(`${API_BASE}/${encodeURIComponent(laneId)}/run`, {
			method: 'POST',
			headers: { 'Content-Type': 'application/json', Accept: 'application/json' },
			body: JSON.stringify(options)
		})
	);
	return (await response.json()) as EvalRunStarted;
}

/**
 * `GET /runs` — server-paged run records, newest first. `total` is the count
 * matching the filter, not the page, so it is a valid pager denominator.
 */
export async function listEvalRuns(
	query: EvalRunsQuery,
	signal?: AbortSignal
): Promise<EvalRunsPage> {
	const params = new URLSearchParams({
		limit: String(query.limit),
		offset: String(query.offset)
	});
	if (query.lane?.trim()) params.set('lane', query.lane.trim());
	if (Number.isFinite(query.fromMs ?? Number.NaN)) params.set('from_ms', String(query.fromMs));
	if (Number.isFinite(query.toMs ?? Number.NaN)) params.set('to_ms', String(query.toMs));

	const body = await getJson<Partial<EvalRunsPage>>('/runs', params, signal);
	const runs = asArray<EvalRun>(body?.runs).map(normalizeRun);
	return {
		runs,
		// A missing or nonsensical total must not shrink the pager below the
		// rows we are actually holding.
		total: typeof body?.total === 'number' && Number.isFinite(body.total)
			? Math.max(body.total, runs.length)
			: runs.length
	};
}
