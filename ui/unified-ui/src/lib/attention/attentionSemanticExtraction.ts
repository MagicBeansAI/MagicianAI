export type AttentionSemanticExtractionPauseReason =
	| 'model_unavailable'
	| 'foreground_pressure'
	| 'manual'
	| 'disabled';

export interface AttentionSemanticExtractionCounts {
	active_total: number;
	/** Absent in older servers; these sources do not use this extractor. */
	not_applicable?: number;
	compatible_revision: number;
	succeeded: number;
	missing: number;
	invalid: number;
	pending: number;
	in_flight: number;
	retry: number;
	dead: number;
	coverage: number;
}

export interface AttentionSemanticExtractionHealth {
	schema_version: 1;
	enabled: boolean;
	paused: boolean;
	pause_reason: AttentionSemanticExtractionPauseReason | null;
	degradation_reason: string | null;
	contract: {
		semantic_schema_version: number;
		extractor_contract: string;
		prompt_version: string;
		model: string | null;
		profile: string | null;
	};
	checkpoint: {
		cursor: string | null;
		updated_at: number | null;
		lease_owner: string | null;
		lease_expires_at: number | null;
	};
	totals: AttentionSemanticExtractionCounts;
	surfaces: {
		follow_up: AttentionSemanticExtractionCounts;
		worth_a_look: AttentionSemanticExtractionCounts;
	};
	queue: {
		pending: number;
		in_flight: number;
		active_in_flight: number;
		expired_in_flight: number;
		retry: number;
		dead: number;
		next_retry_at: number | null;
		oldest_ready_at: number | null;
		last_succeeded_at: number | null;
	};
}

function record(value: unknown): Record<string, unknown> | null {
	return value !== null && typeof value === 'object' && !Array.isArray(value)
		? (value as Record<string, unknown>)
		: null;
}

function nonNegativeInteger(value: unknown): number | null {
	return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0
		? value
		: null;
}

function probability(value: unknown): number | null {
	return typeof value === 'number' && Number.isFinite(value) && value >= 0 && value <= 1
		? value
		: null;
}

function boundedString(value: unknown, maxCharacters = 200): string | null {
	return typeof value === 'string' &&
		value.trim().length > 0 &&
		[...value].length <= maxCharacters &&
		![...value].some((character) => /\p{Cc}/u.test(character))
		? value
		: null;
}

function optionalString(
	value: unknown,
	maxCharacters = 200
): string | null | undefined {
	if (value === null || value === undefined) return null;
	return boundedString(value, maxCharacters) ?? undefined;
}

function optionalInteger(value: unknown): number | null | undefined {
	if (value === null || value === undefined) return null;
	return nonNegativeInteger(value) ?? undefined;
}

function parseCounts(value: unknown): AttentionSemanticExtractionCounts | null {
	const input = record(value);
	if (!input) return null;
	const activeTotal = nonNegativeInteger(input.active_total);
	const notApplicable = input.not_applicable === undefined ? 0 : nonNegativeInteger(input.not_applicable);
	const compatibleRevision = nonNegativeInteger(input.compatible_revision);
	const succeeded = nonNegativeInteger(input.succeeded);
	const missing = nonNegativeInteger(input.missing);
	const invalid = nonNegativeInteger(input.invalid);
	const pending = nonNegativeInteger(input.pending);
	const inFlight = nonNegativeInteger(input.in_flight);
	const retry = nonNegativeInteger(input.retry);
	const dead = nonNegativeInteger(input.dead);
	const coverage = probability(input.coverage);
	if (
		activeTotal === null ||
		notApplicable === null ||
		notApplicable > activeTotal ||
		compatibleRevision === null ||
		compatibleRevision > activeTotal - notApplicable ||
		succeeded === null ||
		compatibleRevision > succeeded ||
		missing === null ||
		invalid === null ||
		pending === null ||
		inFlight === null ||
		retry === null ||
		dead === null ||
		coverage === null
	) {
		return null;
	}
	return {
		active_total: activeTotal,
		...(input.not_applicable === undefined ? {} : {not_applicable: notApplicable}),
		compatible_revision: compatibleRevision,
		succeeded,
		missing,
		invalid,
		pending,
		in_flight: inFlight,
		retry,
		dead,
		coverage
	};
}

const PAUSE_REASONS = new Set<AttentionSemanticExtractionPauseReason>([
	'model_unavailable',
	'foreground_pressure',
	'manual',
	'disabled'
]);

export function parseAttentionSemanticExtractionHealth(
	value: unknown
): AttentionSemanticExtractionHealth | null {
	const root = record(value);
	const input = record(root?.semantic_extraction_health);
	const contract = record(input?.contract);
	const checkpoint = record(input?.checkpoint);
	const surfaces = record(input?.surfaces);
	const queue = record(input?.queue);
	if (!input || input.schema_version !== 1 || !contract || !checkpoint || !surfaces || !queue) {
		return null;
	}
	const pauseReason = optionalString(input.pause_reason, 32);
	const degradationReason = optionalString(input.degradation_reason, 200);
	const semanticSchemaVersion = nonNegativeInteger(contract.semantic_schema_version);
	const extractorContract = boundedString(contract.extractor_contract, 200);
	const promptVersion = boundedString(contract.prompt_version, 200);
	const model = optionalString(contract.model, 200);
	const profile = optionalString(contract.profile, 200);
	const cursor = optionalString(checkpoint.cursor, 500);
	const updatedAt = optionalInteger(checkpoint.updated_at);
	const leaseOwner = optionalString(checkpoint.lease_owner, 200);
	const leaseExpiresAt = optionalInteger(checkpoint.lease_expires_at);
	const totals = parseCounts(input.totals);
	const followUp = parseCounts(surfaces.follow_up);
	const worthALook = parseCounts(surfaces.worth_a_look);
	const queuePending = nonNegativeInteger(queue.pending);
	const queueInFlight = nonNegativeInteger(queue.in_flight);
	const queueActiveInFlight = Object.prototype.hasOwnProperty.call(queue, 'active_in_flight')
		? nonNegativeInteger(queue.active_in_flight)
		: queueInFlight;
	const queueExpiredInFlight = Object.prototype.hasOwnProperty.call(queue, 'expired_in_flight')
		? nonNegativeInteger(queue.expired_in_flight)
		: 0;
	const queueRetry = nonNegativeInteger(queue.retry);
	const queueDead = nonNegativeInteger(queue.dead);
	const nextRetryAt = optionalInteger(queue.next_retry_at);
	const oldestReadyAt = optionalInteger(queue.oldest_ready_at);
	const lastSucceededAt = optionalInteger(queue.last_succeeded_at);
	if (
		typeof input.enabled !== 'boolean' ||
		typeof input.paused !== 'boolean' ||
		pauseReason === undefined ||
		(pauseReason !== null &&
			!PAUSE_REASONS.has(pauseReason as AttentionSemanticExtractionPauseReason)) ||
		degradationReason === undefined ||
		semanticSchemaVersion === null ||
		semanticSchemaVersion < 1 ||
		!extractorContract ||
		!promptVersion ||
		model === undefined ||
		profile === undefined ||
		cursor === undefined ||
		updatedAt === undefined ||
		leaseOwner === undefined ||
		leaseExpiresAt === undefined ||
		!totals ||
		!followUp ||
		!worthALook ||
		queuePending === null ||
		queueInFlight === null ||
		queueActiveInFlight === null ||
		queueExpiredInFlight === null ||
		queueActiveInFlight + queueExpiredInFlight !== queueInFlight ||
		queueRetry === null ||
		queueDead === null ||
		nextRetryAt === undefined ||
		oldestReadyAt === undefined ||
		lastSucceededAt === undefined
	) {
		return null;
	}
	const parsed: AttentionSemanticExtractionHealth = {
		schema_version: 1,
		enabled: input.enabled,
		paused: input.paused,
		pause_reason: pauseReason as AttentionSemanticExtractionPauseReason | null,
		degradation_reason: degradationReason,
		contract: {
			semantic_schema_version: semanticSchemaVersion,
			extractor_contract: extractorContract,
			prompt_version: promptVersion,
			model,
			profile
		},
		checkpoint: {
			cursor,
			updated_at: updatedAt,
			lease_owner: leaseOwner,
			lease_expires_at: leaseExpiresAt
		},
		totals,
		surfaces: { follow_up: followUp, worth_a_look: worthALook },
		queue: {
			pending: queuePending,
			in_flight: queueInFlight,
			active_in_flight: queueActiveInFlight,
			expired_in_flight: queueExpiredInFlight,
			retry: queueRetry,
			dead: queueDead,
			next_retry_at: nextRetryAt,
			oldest_ready_at: oldestReadyAt,
			last_succeeded_at: lastSucceededAt
		}
	};
	if (
		!semanticExtractionCountsReconcile(parsed.totals) ||
		!semanticExtractionCountsReconcile(parsed.surfaces.follow_up) ||
		!semanticExtractionCountsReconcile(parsed.surfaces.worth_a_look) ||
		!semanticExtractionCoverageReconciles(parsed.totals) ||
		!semanticExtractionCoverageReconciles(parsed.surfaces.follow_up) ||
		!semanticExtractionCoverageReconciles(parsed.surfaces.worth_a_look) ||
		!semanticExtractionSurfacesReconcile(parsed)
	) {
		return null;
	}
	return parsed;
}

export function semanticExtractionCountsReconcile(
	counts: AttentionSemanticExtractionCounts
): boolean {
	return (
		counts.succeeded +
			counts.missing +
			counts.invalid +
			counts.pending +
			counts.in_flight +
			counts.retry +
			counts.dead + (counts.not_applicable ?? 0) ===
		counts.active_total
	);
}

export function semanticExtractionCoverageReconciles(
	counts: AttentionSemanticExtractionCounts
): boolean {
	const applicable = counts.active_total - (counts.not_applicable ?? 0);
	if (applicable < 0) return false;
	const expected = applicable === 0 ? 1 : counts.compatible_revision / applicable;
	return Math.abs(counts.coverage - expected) <= 0.000001;
}

export function semanticExtractionSurfacesReconcile(
	health: AttentionSemanticExtractionHealth | null
): boolean {
	if (!health) return false;
	const fields: Array<keyof Omit<AttentionSemanticExtractionCounts, 'coverage'>> = [
		'active_total',
		'not_applicable',
		'compatible_revision',
		'succeeded',
		'missing',
		'invalid',
		'pending',
		'in_flight',
		'retry',
		'dead'
	];
	return fields.every(
		(field) =>
			(health.surfaces.follow_up[field] ?? 0) + (health.surfaces.worth_a_look[field] ?? 0) ===
			(health.totals[field] ?? 0)
	);
}
