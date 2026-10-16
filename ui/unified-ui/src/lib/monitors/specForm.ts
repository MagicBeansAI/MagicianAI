/**
 * Monitor create/edit composer logic (Phase 4) — pure, component-free.
 *
 * `validateAndNormalizeSpec` mirrors the backend admission gate
 * (`magician/src/magician_v2/monitors/monitor_spec.rs`
 * `validate_and_normalize`) field-for-field and reason-for-reason, so the
 * composer can reject locally with the SAME stable snake_case reasons the
 * server would return. The server remains authoritative — this mirror only
 * gives the form instant, consistent feedback (URL parsing differs at the
 * WHATWG-vs-rust-url margins; anything the mirror wrongly admits still 400s
 * with the same reason string on POST).
 *
 * `cadenceSummary` mirrors `monitors_api::cadence_summary` so the
 * review-before-activate card shows exactly the summary the list rows and
 * chat preview show.
 */

import type {
	MonitorMatchMode,
	MonitorNotificationPolicy,
	MonitorSpecV1,
	TaskScheduleWire
} from '$lib/types/monitor';

// Bounds mirrored from monitor_spec.rs.
export const MAX_OBJECTIVE_CHARS = 2000;
export const MAX_LIST_ENTRY_CHARS = 500;
export const MAX_LIST_ENTRIES = 50;
export const MAX_SOURCE_URLS = 100;

export type SpecValidationResult =
	| { ok: true; spec: MonitorSpecV1 }
	| { ok: false; reason: string };

function charCount(value: string): number {
	// Mirror Rust's chars().count() (code points, not UTF-16 units).
	return [...value].length;
}

/** Trim, drop empties, bound entry length, dedupe first-seen, bound count. */
function normalizeStringList(
	entries: string[],
	field: string
): { ok: true; list: string[] } | { ok: false; reason: string } {
	const normalized: string[] = [];
	for (const raw of entries) {
		const entry = raw.trim();
		if (!entry) continue;
		if (charCount(entry) > MAX_LIST_ENTRY_CHARS) {
			return { ok: false, reason: `monitor_${field}_entry_too_long` };
		}
		if (!normalized.includes(entry)) normalized.push(entry);
	}
	if (normalized.length > MAX_LIST_ENTRIES) {
		return { ok: false, reason: `monitor_${field}_too_many_entries` };
	}
	return { ok: true, list: normalized };
}

function normalizeSourceUrls(
	urls: string[]
): { ok: true; list: string[] } | { ok: false; reason: string } {
	const normalized: string[] = [];
	for (const raw of urls) {
		const url = raw.trim();
		if (!url) continue;
		let parsed: URL;
		try {
			parsed = new URL(url);
		} catch {
			return { ok: false, reason: 'monitor_source_url_invalid' };
		}
		if (parsed.protocol !== 'http:' && parsed.protocol !== 'https:') {
			return { ok: false, reason: 'monitor_source_url_scheme_unsupported' };
		}
		if (!normalized.includes(url)) normalized.push(url);
	}
	if (normalized.length > MAX_SOURCE_URLS) {
		return { ok: false, reason: 'monitor_source_urls_too_many' };
	}
	return { ok: true, list: normalized };
}

/** The client mirror of the backend admission gate. Never mutates its input. */
export function validateAndNormalizeSpec(spec: MonitorSpecV1): SpecValidationResult {
	if (spec.schema_version !== 1) {
		return { ok: false, reason: 'monitor_schema_version_unsupported' };
	}
	const objective = spec.objective.trim();
	if (!objective) return { ok: false, reason: 'monitor_objective_required' };
	if (charCount(objective) > MAX_OBJECTIVE_CHARS) {
		return { ok: false, reason: 'monitor_objective_too_long' };
	}

	const querySeeds = normalizeStringList(spec.query_seeds, 'query_seeds');
	if (!querySeeds.ok) return querySeeds;
	const includeRules = normalizeStringList(spec.include_rules, 'include_rules');
	if (!includeRules.ok) return includeRules;
	const excludeRules = normalizeStringList(spec.exclude_rules, 'exclude_rules');
	if (!excludeRules.ok) return excludeRules;
	const domains = normalizeStringList(spec.sources.domains, 'domains');
	if (!domains.ok) return domains;
	const authenticated = normalizeStringList(
		spec.sources.authenticated_sources,
		'authenticated_sources'
	);
	if (!authenticated.ok) return authenticated;
	const urls = normalizeSourceUrls(spec.sources.urls);
	if (!urls.ok) return urls;

	if (urls.list.length === 0 && domains.list.length === 0 && querySeeds.list.length === 0) {
		return { ok: false, reason: 'monitor_sources_required' };
	}

	return {
		ok: true,
		spec: {
			schema_version: 1,
			objective,
			query_seeds: querySeeds.list,
			sources: {
				urls: urls.list,
				domains: domains.list,
				authenticated_sources: authenticated.list
			},
			include_rules: includeRules.list,
			exclude_rules: excludeRules.list,
			match_mode: spec.match_mode,
			notification_policy: spec.notification_policy,
			notify_initial_baseline: spec.notify_initial_baseline
		}
	};
}

/** Human message for the stable reasons (fallback: the raw reason). */
export function specReasonLabel(reason: string): string {
	const labels: Record<string, string> = {
		monitor_objective_required: 'Describe what to monitor.',
		monitor_objective_too_long: `Keep the objective under ${MAX_OBJECTIVE_CHARS} characters.`,
		monitor_sources_required: 'Add at least one URL, domain, or search phrase.',
		monitor_source_url_invalid: 'One of the URLs is not a valid URL.',
		monitor_source_url_scheme_unsupported: 'Only http(s) URLs can be monitored.',
		monitor_source_urls_too_many: `Keep it to ${MAX_SOURCE_URLS} URLs or fewer.`,
		// Phase 7 convert-to-monitor server refusals.
		monitor_already_exists: 'This task is already a monitor.',
		task_not_eligible_for_monitor: "This task can't be converted to a monitor.",
		task_not_found: 'This task no longer exists.'
	};
	if (labels[reason]) return labels[reason];
	if (reason.endsWith('_entry_too_long')) {
		return `One entry is longer than ${MAX_LIST_ENTRY_CHARS} characters.`;
	}
	if (reason.endsWith('_too_many_entries')) {
		return `Keep each list to ${MAX_LIST_ENTRIES} entries or fewer.`;
	}
	return reason;
}

// ── Form model ───────────────────────────────────────────────────────────────

/** One textarea-friendly line-list per spec list field. */
export interface MonitorFormValue {
	title: string;
	objective: string;
	urlsText: string;
	domainsText: string;
	authenticatedText: string;
	querySeedsText: string;
	includeRulesText: string;
	excludeRulesText: string;
	matchMode: MonitorMatchMode;
	notificationPolicy: MonitorNotificationPolicy;
	notifyInitialBaseline: boolean;
	/** A CADENCE_PRESETS id, 'custom', or 'none' (run on demand only). */
	cadence: string;
	cronExpression: string;
	timezone: string;
}

export function emptyMonitorForm(): MonitorFormValue {
	return {
		title: '',
		objective: '',
		urlsText: '',
		domainsText: '',
		authenticatedText: '',
		querySeedsText: '',
		includeRulesText: '',
		excludeRulesText: '',
		matchMode: 'balanced',
		notificationPolicy: 'material_changes',
		notifyInitialBaseline: false,
		cadence: 'daily-9',
		cronExpression: '',
		timezone: ''
	};
}

export function splitLines(text: string): string[] {
	return text
		.split(/\r?\n/)
		.map((line) => line.trim())
		.filter((line) => line.length > 0);
}

/** Build the (unvalidated) spec a form describes; validate separately. */
export function specFromForm(form: MonitorFormValue): MonitorSpecV1 {
	return {
		schema_version: 1,
		objective: form.objective,
		query_seeds: splitLines(form.querySeedsText),
		sources: {
			urls: splitLines(form.urlsText),
			domains: splitLines(form.domainsText),
			authenticated_sources: splitLines(form.authenticatedText)
		},
		include_rules: splitLines(form.includeRulesText),
		exclude_rules: splitLines(form.excludeRulesText),
		match_mode: form.matchMode,
		notification_policy: form.notificationPolicy,
		notify_initial_baseline: form.notifyInitialBaseline
	};
}

/** Pre-fill the edit form from a monitor detail's spec + schedule. */
export function formFromSpec(
	title: string,
	spec: MonitorSpecV1,
	schedule?: TaskScheduleWire | null
): MonitorFormValue {
	const cron = cronOf(schedule ?? null);
	const preset = cron ? CADENCE_PRESETS.find((p) => p.cron === cron.expression) : undefined;
	return {
		title,
		objective: spec.objective,
		urlsText: spec.sources.urls.join('\n'),
		domainsText: spec.sources.domains.join('\n'),
		authenticatedText: spec.sources.authenticated_sources.join('\n'),
		querySeedsText: spec.query_seeds.join('\n'),
		includeRulesText: spec.include_rules.join('\n'),
		excludeRulesText: spec.exclude_rules.join('\n'),
		matchMode: spec.match_mode,
		notificationPolicy: spec.notification_policy,
		notifyInitialBaseline: spec.notify_initial_baseline,
		cadence: cron ? (preset?.id ?? 'custom') : 'none',
		cronExpression: cron?.expression ?? '',
		timezone: cron?.timezone ?? ''
	};
}

// ── Schedule ────────────────────────────────────────────────────────────────

export interface CadencePreset {
	id: string;
	label: string;
	cron: string;
}

/** Simple-by-default cadence choices; "custom" exposes the raw cron field. */
export const CADENCE_PRESETS: CadencePreset[] = [
	{ id: 'hourly', label: 'Every hour', cron: '0 * * * *' },
	{ id: 'daily-9', label: 'Every day at 9:00 AM', cron: '0 9 * * *' },
	{ id: 'daily-18', label: 'Every day at 6:00 PM', cron: '0 18 * * *' },
	{ id: 'weekdays-9', label: 'Every weekday at 9:00 AM', cron: '0 9 * * 1-5' },
	{ id: 'weekly-mon-9', label: 'Every Monday at 9:00 AM', cron: '0 9 * * 1' },
	{ id: 'monthly-1-9', label: 'First of the month at 9:00 AM', cron: '0 9 1 * *' }
];

function cronOf(
	schedule: TaskScheduleWire | null
): { expression: string; timezone: string | null } | null {
	const kind = schedule?.kind as { Cron?: { expression?: unknown; timezone?: unknown } } | undefined;
	const cron = kind?.Cron;
	if (!cron || typeof cron.expression !== 'string') return null;
	const kindTimezone = typeof cron.timezone === 'string' ? cron.timezone : null;
	const scheduleTimezone = typeof schedule?.timezone === 'string' ? schedule.timezone : null;
	return { expression: cron.expression, timezone: kindTimezone ?? scheduleTimezone };
}

/**
 * Build the `Task.schedule` wire object from the form. Returns `null` for
 * the explicit "no schedule / run on demand" choice, or an error for an
 * empty/invalid custom cron.
 */
export function scheduleFromForm(
	form: MonitorFormValue
): { ok: true; schedule: TaskScheduleWire | null } | { ok: false; reason: string } {
	if (form.cadence === 'none') return { ok: true, schedule: null };
	const preset = CADENCE_PRESETS.find((candidate) => candidate.id === form.cadence);
	const expression = preset ? preset.cron : form.cronExpression.trim();
	if (!expression) return { ok: false, reason: 'monitor_schedule_required' };
	if (expression.split(/\s+/).length !== 5) {
		return { ok: false, reason: 'monitor_schedule_invalid' };
	}
	const timezone = form.timezone.trim();
	return {
		ok: true,
		schedule: {
			kind: { Cron: { expression, timezone: timezone || null } }
		}
	};
}

/**
 * Mirror of `monitors_api::cadence_summary` — the one human cadence string
 * used by list rows, the chat preview, and this composer's review card.
 */
export function cadenceSummary(schedule: TaskScheduleWire | null | undefined): string {
	if (!schedule || typeof schedule !== 'object') return 'unscheduled';
	const kind = schedule.kind as Record<string, unknown> | undefined;
	if (!kind || typeof kind !== 'object') return 'unscheduled';
	const cron = cronOf(schedule);
	if (cron) {
		const timezone = cron.timezone?.trim();
		return timezone ? `Cron ${cron.expression} (${timezone})` : `Cron ${cron.expression}`;
	}
	const interval = (kind as { Interval?: { seconds?: unknown } }).Interval;
	if (interval && typeof interval.seconds === 'number') {
		return `Every ${interval.seconds}s`;
	}
	const once = (kind as { Once?: { at?: unknown } }).Once;
	if (once && typeof once.at === 'string') return `Once at ${once.at}`;
	const onEvent = (kind as { OnEvent?: { event_pattern?: unknown } }).OnEvent;
	if (onEvent && typeof onEvent.event_pattern === 'string') {
		return `On event ${onEvent.event_pattern}`;
	}
	return 'unscheduled';
}

// ── Presentation helpers shared by list/detail/composer ────────────────────

export function notificationPolicyLabel(policy: MonitorNotificationPolicy): string {
	switch (policy) {
		case 'material_changes':
			return 'Material changes only';
		case 'every_run':
			return 'Every run';
		case 'never':
			return 'Never';
	}
}

export function matchModeLabel(mode: MonitorMatchMode): string {
	switch (mode) {
		case 'strict':
			return 'Strict';
		case 'balanced':
			return 'Balanced';
		case 'broad':
			return 'Broad';
	}
}

export function healthBadge(health: string): { text: string; color: 'success' | 'warning' | 'error' | 'default' } {
	switch (health) {
		case 'ok':
			return { text: 'Healthy', color: 'success' };
		case 'needs_attention':
			return { text: 'Needs attention', color: 'warning' };
		case 'failing':
			return { text: 'Failing', color: 'error' };
		default:
			return { text: health, color: 'default' };
	}
}

export function runStatusBadge(status: string): {
	text: string;
	color: 'success' | 'warning' | 'error' | 'info' | 'default';
} {
	switch (status) {
		case 'changed':
			return { text: 'Changed', color: 'info' };
		case 'baseline':
			return { text: 'Baseline', color: 'default' };
		case 'unchanged':
			return { text: 'Unchanged', color: 'success' };
		case 'degraded':
			return { text: 'Degraded', color: 'warning' };
		case 'failed':
			return { text: 'Failed', color: 'error' };
		case 'never_ran':
			return { text: 'Never ran', color: 'default' };
		default:
			return { text: status, color: 'default' };
	}
}
