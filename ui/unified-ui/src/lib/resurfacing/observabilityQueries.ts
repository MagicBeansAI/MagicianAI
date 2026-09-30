/**
 * Proactive Resurfacing — observability client for the operator glance surface
 * (`/resurfacing`). Reads the engine's pipeline health, candidate funnel, recent
 * background-pass runs, table sizes, corpus watermarks, and per-lane engagement
 * from a single endpoint.
 *
 * Scope handling mirrors the sibling Phase-1 client (`today/resurfacingQueries`):
 * plain `fetch` against `/api/magician/…` — the `installScopedApiFetch`
 * window.fetch patch injects the workspace-bound bearer, so this
 * module never builds principal/workspace params itself.
 *
 * `mapObservabilityPayload` is a PURE, network-free mapper so the raw JSON →
 * typed-shape coercion is unit-testable in isolation; `fetchResurfacingObservability`
 * just wraps it (returns `null` on any error, and an empty-but-valid shape when the
 * engine hasn't run yet — every field defaults so a missing section never throws).
 */

/** Per-kind background-pass health (scorer / curator / retention). */
export interface PipelineKindStats {
	/** The pass name — the object key in the payload's `pipeline` map. */
	kind: string;
	total: number;
	successes: number;
	failures: number;
	total_produced: number;
	avg_duration_ms: number;
	/** Epoch-ms of the last run start, or null when the pass has never run. */
	last_started_at: number | null;
	/** Error string of the most recent FAILING run, or null when none failed. */
	last_error: string | null;
}

/** One recent background-pass run (newest first, as ordered by the backend). */
export interface ResurfacingRunRecord {
	kind: string;
	started_at: number | null;
	duration_ms: number;
	produced: number;
	success: boolean;
	error: string | null;
}

export interface ResurfacingFunnelDetail {
	state: string;
	source_kind: string;
	count: number;
}

/** Candidate lifecycle funnel — per-state + per-lane totals and headline counts. */
export interface ResurfacingFunnel {
	by_state: Record<string, number>;
	by_lane: Record<string, number>;
	/** Active queue: candidate-state rows eligible for curation now. */
	pending: number;
	/** Alias of pending, kept explicit because the backend exposes both names. */
	eligible: number;
	/** Raw candidate-state inventory, including cooled-down rows. */
	candidate_pool: number;
	/** Candidate-state rows temporarily held back by cooldown. */
	cooling: number;
	surfaced: number;
	detail: ResurfacingFunnelDetail[];
}

/** Standalone queue snapshot; duplicates funnel headline fields for direct consumers. */
export interface ResurfacingQueue {
	candidate_pool: number;
	pending: number;
	eligible: number;
	cooling: number;
	surfaced: number;
	acted: number;
	dismissed: number;
	snoozed: number;
}

export interface ResurfacingWatermark {
	corpus_kind: string;
	cursor: number;
}

/** Per-table row counts. */
export interface ResurfacingSizes {
	candidates: number;
	embeddings: number;
	phrasing: number;
	dismissed_signals: number;
	affinity_signals: number;
	runs: number;
	recommendations: number;
	action_claims: number;
	action_events: number;
	routing_repairs: number;
}

export interface ResurfacingBriefCoverage {
	comm_total: number;
	comm_surfaced: number;
	with_brief: number;
	legacy: number;
	complete: number;
	partial: number;
	source_omits_details: number;
}

export interface ResurfacingRecommendationStats {
	shown: number;
	selected: number;
	completed: number;
	acceptance_rate: number;
	completion_rate: number;
	by_kind: Record<string, Record<string, number>>;
}

export interface ResurfacingActionStats {
	started: number;
	completed: number;
	failed: number;
	completion_rate: number;
	by_kind: Record<string, Record<string, number>>;
	errors: Record<string, number>;
}

export interface ResurfacingRoutingRepairStats {
	total: number;
	by_outcome: Record<string, number>;
}

/** Per-lane engagement + derived utility (identical shape to `/resurfacing/stats`). */
export interface ResurfacingEngagementRow {
	source_kind: string;
	positive: number;
	negative: number;
	engagement_rate: number;
	utility_multiplier: number;
}

export interface ResurfacingObservability {
	/** Ordered scorer → curator → retention, then any extra kinds the backend adds. */
	pipeline: PipelineKindStats[];
	recent_runs: ResurfacingRunRecord[];
	funnel: ResurfacingFunnel;
	queue: ResurfacingQueue;
	watermarks: ResurfacingWatermark[];
	sizes: ResurfacingSizes;
	engagement: ResurfacingEngagementRow[];
	briefs: ResurfacingBriefCoverage;
	recommendations: ResurfacingRecommendationStats;
	actions: ResurfacingActionStats;
	routing_repair: ResurfacingRoutingRepairStats;
}

function isObject(value: unknown): value is Record<string, unknown> {
	return !!value && typeof value === 'object' && !Array.isArray(value);
}

function asNum(value: unknown): number {
	if (typeof value === 'number' && Number.isFinite(value)) return value;
	if (typeof value === 'string') {
		const n = Number(value);
		return Number.isFinite(n) ? n : 0;
	}
	return 0;
}

/** Finite number → number; anything else → null (for "never" timestamps). */
function asOptNum(value: unknown): number | null {
	if (typeof value === 'number' && Number.isFinite(value)) return value;
	if (typeof value === 'string') {
		const n = Number(value);
		return Number.isFinite(n) ? n : null;
	}
	return null;
}

function asString(value: unknown): string {
	return typeof value === 'string' ? value : '';
}

function asOptString(value: unknown): string | null {
	return typeof value === 'string' && value.length > 0 ? value : null;
}

function asBool(value: unknown): boolean {
	return value === true;
}

/** Coerce a `{ key: number }` map defensively; drops non-numeric entries to 0. */
function asNumberMap(value: unknown): Record<string, number> {
	const out: Record<string, number> = {};
	if (!isObject(value)) return out;
	for (const [k, v] of Object.entries(value)) {
		out[k] = asNum(v);
	}
	return out;
}

// Stable operator ordering for the pipeline cards; extra kinds append after.
const PIPELINE_KIND_ORDER = ['scorer', 'routing_repair', 'curator', 'retention'];

function mapPipelineKind(kind: string, raw: unknown): PipelineKindStats {
	const record = isObject(raw) ? raw : {};
	return {
		kind,
		total: asNum(record.total),
		successes: asNum(record.successes),
		failures: asNum(record.failures),
		total_produced: asNum(record.total_produced),
		avg_duration_ms: asNum(record.avg_duration_ms),
		last_started_at: asOptNum(record.last_started_at),
		last_error: asOptString(record.last_error)
	};
}

function emptyFunnel(): ResurfacingFunnel {
	return {
		by_state: {},
		by_lane: {},
		pending: 0,
		eligible: 0,
		candidate_pool: 0,
		cooling: 0,
		surfaced: 0,
		detail: []
	};
}

function emptyQueue(): ResurfacingQueue {
	return {
		candidate_pool: 0,
		pending: 0,
		eligible: 0,
		cooling: 0,
		surfaced: 0,
		acted: 0,
		dismissed: 0,
		snoozed: 0
	};
}

function emptySizes(): ResurfacingSizes {
	return {
		candidates: 0,
		embeddings: 0,
		phrasing: 0,
		dismissed_signals: 0,
		affinity_signals: 0,
		runs: 0,
		recommendations: 0,
		action_claims: 0,
		action_events: 0,
		routing_repairs: 0
	};
}

function nestedNumberMap(value: unknown): Record<string, Record<string, number>> {
	const out: Record<string, Record<string, number>> = {};
	if (!isObject(value)) return out;
	for (const [key, raw] of Object.entries(value)) out[key] = asNumberMap(raw);
	return out;
}

/**
 * Pure raw-JSON → typed-observability mapper. Every section defaults to an
 * empty/zeroed shape so a partial payload (e.g. the engine has never run) maps
 * cleanly instead of throwing, and malformed rows are coerced field-by-field.
 */
export function mapObservabilityPayload(json: unknown): ResurfacingObservability {
	const root = isObject(json) ? json : {};

	// Pipeline: keyed { scorer, curator, retention } object → ordered array.
	const pipelineRaw = isObject(root.pipeline) ? root.pipeline : {};
	const seen = new Set<string>();
	const pipeline: PipelineKindStats[] = [];
	for (const kind of PIPELINE_KIND_ORDER) {
		if (kind in pipelineRaw) {
			pipeline.push(mapPipelineKind(kind, pipelineRaw[kind]));
			seen.add(kind);
		}
	}
	for (const [kind, raw] of Object.entries(pipelineRaw)) {
		if (seen.has(kind)) continue;
		pipeline.push(mapPipelineKind(kind, raw));
	}

	// Recent runs.
	const recent_runs: ResurfacingRunRecord[] = [];
	if (Array.isArray(root.recent_runs)) {
		for (const raw of root.recent_runs) {
			if (!isObject(raw)) continue;
			recent_runs.push({
				kind: asString(raw.kind),
				started_at: asOptNum(raw.started_at),
				duration_ms: asNum(raw.duration_ms),
				produced: asNum(raw.produced),
				success: asBool(raw.success),
				error: asOptString(raw.error)
			});
		}
	}

	// Funnel.
	let funnel = emptyFunnel();
	if (isObject(root.funnel)) {
		const f = root.funnel;
		const detail: ResurfacingFunnelDetail[] = [];
		if (Array.isArray(f.detail)) {
			for (const raw of f.detail) {
				if (!isObject(raw)) continue;
				detail.push({
					state: asString(raw.state),
					source_kind: asString(raw.source_kind),
					count: asNum(raw.count)
				});
			}
		}
		funnel = {
			by_state: asNumberMap(f.by_state),
			by_lane: asNumberMap(f.by_lane),
			pending: asNum(f.pending),
			eligible: asNum(f.eligible ?? f.pending),
			candidate_pool: asNum(f.candidate_pool ?? f.pending),
			cooling: asNum(f.cooling),
			surfaced: asNum(f.surfaced),
			detail
		};
	}

	const queueRaw = isObject(root.queue) ? root.queue : {};
	const queue: ResurfacingQueue = {
		candidate_pool: asNum(queueRaw.candidate_pool ?? funnel.candidate_pool),
		pending: asNum(queueRaw.pending ?? funnel.pending),
		eligible: asNum(queueRaw.eligible ?? queueRaw.pending ?? funnel.eligible),
		cooling: asNum(queueRaw.cooling ?? funnel.cooling),
		surfaced: asNum(queueRaw.surfaced ?? funnel.surfaced),
		acted: asNum(queueRaw.acted ?? funnel.by_state.acted),
		dismissed: asNum(queueRaw.dismissed ?? funnel.by_state.dismissed),
		snoozed: asNum(queueRaw.snoozed ?? funnel.by_state.snoozed)
	};

	// Watermarks.
	const watermarks: ResurfacingWatermark[] = [];
	if (Array.isArray(root.watermarks)) {
		for (const raw of root.watermarks) {
			if (!isObject(raw)) continue;
			watermarks.push({
				corpus_kind: asString(raw.corpus_kind),
				cursor: asNum(raw.cursor)
			});
		}
	}

	// Sizes.
	const sizesRaw = isObject(root.sizes) ? root.sizes : {};
	const sizes: ResurfacingSizes = {
		candidates: asNum(sizesRaw.candidates),
		embeddings: asNum(sizesRaw.embeddings),
		phrasing: asNum(sizesRaw.phrasing),
		dismissed_signals: asNum(sizesRaw.dismissed_signals),
		affinity_signals: asNum(sizesRaw.affinity_signals),
		runs: asNum(sizesRaw.runs),
		recommendations: asNum(sizesRaw.recommendations),
		action_claims: asNum(sizesRaw.action_claims),
		action_events: asNum(sizesRaw.action_events),
		routing_repairs: asNum(sizesRaw.routing_repairs)
	};

	// Engagement.
	const engagement: ResurfacingEngagementRow[] = [];
	if (Array.isArray(root.engagement)) {
		for (const raw of root.engagement) {
			if (!isObject(raw)) continue;
			engagement.push({
				source_kind: asString(raw.source_kind),
				positive: asNum(raw.positive),
				negative: asNum(raw.negative),
				engagement_rate: asNum(raw.engagement_rate),
				utility_multiplier: asNum(raw.utility_multiplier)
			});
		}
	}

	const briefsRaw = isObject(root.briefs) ? root.briefs : {};
	const briefs: ResurfacingBriefCoverage = {
		comm_total: asNum(briefsRaw.comm_total),
		comm_surfaced: asNum(briefsRaw.comm_surfaced),
		with_brief: asNum(briefsRaw.with_brief),
		legacy: asNum(briefsRaw.legacy),
		complete: asNum(briefsRaw.complete),
		partial: asNum(briefsRaw.partial),
		source_omits_details: asNum(briefsRaw.source_omits_details)
	};
	const recommendationsRaw = isObject(root.recommendations) ? root.recommendations : {};
	const recommendations: ResurfacingRecommendationStats = {
		shown: asNum(recommendationsRaw.shown),
		selected: asNum(recommendationsRaw.selected),
		completed: asNum(recommendationsRaw.completed),
		acceptance_rate: asNum(recommendationsRaw.acceptance_rate),
		completion_rate: asNum(recommendationsRaw.completion_rate),
		by_kind: nestedNumberMap(recommendationsRaw.by_kind)
	};
	const actionsRaw = isObject(root.actions) ? root.actions : {};
	const actions: ResurfacingActionStats = {
		started: asNum(actionsRaw.started),
		completed: asNum(actionsRaw.completed),
		failed: asNum(actionsRaw.failed),
		completion_rate: asNum(actionsRaw.completion_rate),
		by_kind: nestedNumberMap(actionsRaw.by_kind),
		errors: asNumberMap(actionsRaw.errors)
	};
	const routingRepairRaw = isObject(root.routing_repair) ? root.routing_repair : {};
	const routing_repair: ResurfacingRoutingRepairStats = {
		total: asNum(routingRepairRaw.total),
		by_outcome: asNumberMap(routingRepairRaw.by_outcome)
	};

	return {
		pipeline,
		recent_runs,
		funnel,
		queue,
		watermarks,
		sizes,
		engagement,
		briefs,
		recommendations,
		actions,
		routing_repair
	};
}

/** An empty-but-valid shape — used as the safe default before the first load. */
export function emptyObservability(): ResurfacingObservability {
	return {
		pipeline: [],
		recent_runs: [],
		funnel: emptyFunnel(),
		queue: emptyQueue(),
		watermarks: [],
		sizes: emptySizes(),
		engagement: [],
		briefs: {
			comm_total: 0,
			comm_surfaced: 0,
			with_brief: 0,
			legacy: 0,
			complete: 0,
			partial: 0,
			source_omits_details: 0
		},
		recommendations: {
			shown: 0,
			selected: 0,
			completed: 0,
			acceptance_rate: 0,
			completion_rate: 0,
			by_kind: {}
		},
		actions: { started: 0, completed: 0, failed: 0, completion_rate: 0, by_kind: {}, errors: {} },
		routing_repair: { total: 0, by_outcome: {} }
	};
}

const RESURFACING_OBSERVABILITY_ENDPOINT =
	'/api/magician/v2/channel-assist/resurfacing/observability';

/**
 * Fetch the resurfacing engine's observability snapshot. Returns `null` on any
 * error (network / non-OK / parse) so the page can show a graceful error state;
 * a successful-but-empty engine maps to a zeroed shape, never a throw.
 */
export async function fetchResurfacingObservability(): Promise<ResurfacingObservability | null> {
	try {
		const res = await fetch(RESURFACING_OBSERVABILITY_ENDPOINT);
		if (!res.ok) return null;
		return mapObservabilityPayload(await res.json());
	} catch {
		return null;
	}
}
