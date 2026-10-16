/**
 * THE single status → color-token mapping for the whole UI.
 * Consumers: SpellsSurface card chips, ExecutionPanel, ChatPanel task cards,
 * Badge `status` mode. Never map a status to a color anywhere else.
 * Tokens are defined in app.css (`--status-{tone}` / `--status-{tone}-soft`,
 * themed per-palette via the cascade).
 */
export type SemanticTone =
	| 'running'
	| 'paused'
	| 'failed'
	| 'attention'
	| 'completed'
	| 'neutral';

/**
 * The five non-neutral tones — exactly the values Badge's `status` mode
 * accepts. Canonical single source: Badge's prop type and MuijRenderer's
 * allowlist both derive from this, so the union is written once.
 */
export type BadgeStatusTone = Exclude<SemanticTone, 'neutral'>;
export const BADGE_STATUS_TONES: readonly BadgeStatusTone[] = [
	'running',
	'paused',
	'failed',
	'attention',
	'completed',
];

const TONE_BY_STATUS: Record<string, SemanticTone> = {
	// Activity-flavored → running
	running: 'running',
	in_progress: 'running',
	executing: 'running',
	planning: 'running',
	synthesizing: 'running',
	building: 'running', // vibe StudioRail RunStatus

	// Deliberately on hold / retry scheduled → paused
	paused: 'paused',
	waiting: 'paused',
	awaiting_input: 'paused', // ExecutionPanel overview status (was in its local map)
	snoozed: 'paused',
	deferred: 'paused', // TaskStatus: precondition not met, retry scheduled

	// Terminal-bad → failed
	failed: 'failed',
	error: 'failed',
	rejected: 'failed',
	cancelled: 'failed',

	// Needs a human → attention
	needs_attention: 'attention',
	attention: 'attention',
	pending_approval: 'attention',
	escalated: 'attention',
	blocked: 'attention',
	needs_action: 'attention', // today/feed/ExecutionPanel action items
	eliciting: 'attention', // TaskPlanStatus: plan is asking the user questions

	// Terminal-good → completed
	completed: 'completed',
	done: 'completed',
	succeeded: 'completed',
	approved: 'completed', // TaskPlanStatus / approval flows

	// Genuinely idle → neutral: unifies today's green-vs-blue split for
	// `ready`; idle states get no color shout (judgment call, confirmed
	// with the plan owner — ready stays neutral, not success-green).
	pending: 'neutral',
	ready: 'neutral',
	draft: 'neutral',
	idle: 'neutral',
	queued: 'neutral',
	archived: 'neutral',
	skipped: 'neutral', // intentionally not run — not a failure, no shout
	info: 'neutral', // FeedItemStatus member (feed/types.ts) — informational, no color shout
};

export interface StatusToneInfo {
	tone: SemanticTone;
	/** e.g. var(--status-running) — safe in any theme */
	colorVar: string;
	/** soft background variant, e.g. var(--status-running-soft) */
	softVar: string;
}

export function statusTone(status: string | null | undefined): StatusToneInfo {
	const key = (status ?? '').toLowerCase().trim();
	// Own-key guard: prototype-chain names ('constructor', 'toString', …)
	// must not leak Object.prototype members out of the plain-object map.
	const tone = Object.hasOwn(TONE_BY_STATUS, key) ? TONE_BY_STATUS[key] : 'neutral';
	if (tone === 'neutral') {
		return { tone, colorVar: 'var(--text-muted)', softVar: 'var(--bg-soft)' };
	}
	return {
		tone,
		colorVar: `var(--status-${tone})`,
		softVar: `var(--status-${tone}-soft)`,
	};
}

export const statusToneVar = (s: string | null | undefined) => statusTone(s).colorVar;

/**
 * Adapter for Badge's `status` mode (which accepts only the five non-neutral
 * tones): returns the tone, or null for neutral statuses so callers fall back
 * to Badge's default gray rendering. Keeps neutral-handling in one place —
 * never re-derive "is this status neutral?" at a call site.
 */
export function statusBadgeTone(
	status: string | null | undefined
): BadgeStatusTone | null {
	const { tone } = statusTone(status);
	return tone === 'neutral' ? null : tone;
}
