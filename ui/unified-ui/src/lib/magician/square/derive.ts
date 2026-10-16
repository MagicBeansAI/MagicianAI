import type { AgentSummary } from '$lib/stores/agentStore';
import type { CitizenCurrentWorkVM, CitizenVM, Vibe } from './engine/types';

/**
 * Shared AgentSummary -> citizen derivations, used by both the game world
 * (FleetWorld) and the crew Board so the two surfaces never disagree about a
 * citizen's status or guild.
 */

export function vibeOf(a: AgentSummary): Vibe {
	const s = (a.status ?? '').toString().toLowerCase();
	if (a.disabled || s.includes('disabl') || s.includes('offline')) return 'offline';
	if (s.includes('paus')) return 'paused';
	if (a.current_execution_id || s.includes('run') || s.includes('exec'))
		return 'working';
	if ((a.pending_approvals ?? 0) > 0) return 'needs';
	return 'idle';
}

const WORKING_TASK_STATUSES = new Set([
	'planning',
	'running',
	'in_progress',
	'active',
	'delivering',
	'synthesizing'
]);
const NEEDS_TASK_STATUSES = new Set(['waiting', 'waiting_for_user', 'blocked']);

/**
 * Resolve the game-world pose from authoritative roster state plus the fleet
 * task projection. Ready and terminal tasks are not live execution. Global
 * Attention is the only "needs" signal; blocked or waiting work is paused.
 */
export function citizenVibeOf(
	agent: AgentSummary,
	currentWork: CitizenCurrentWorkVM[],
	hasAttention: boolean
): Vibe {
	const rosterVibe = vibeOf(agent);
	if (rosterVibe === 'offline') return 'offline';
	if (hasAttention) return 'needs';
	if (
		rosterVibe === 'paused'
		|| currentWork.some(
			(work) =>
				work.isBlocked
				|| work.status.trim().toLowerCase() === 'paused'
				|| NEEDS_TASK_STATUSES.has(work.status.trim().toLowerCase())
		)
	) {
		return 'paused';
	}
	if (
		rosterVibe === 'working'
		|| currentWork.some((work) => WORKING_TASK_STATUSES.has(work.status.trim().toLowerCase()))
	) {
		return 'working';
	}
	return 'idle';
}

/** programs/engineering_strategy.md -> "engineering_strategy" */
export function normalizeProgram(p: string): string {
	return p
		.replace(/^programs\//i, '')
		.replace(/\.md$/i, '')
		.trim()
		.toLowerCase();
}

/**
 * A citizen's guild = the program behind its focus areas (the real org
 * structure): first a high-priority focus area with a program, then any
 * program'd focus area, then the harness program_section, else the Commons.
 */
export function guildIdOf(a: AgentSummary): string {
	const areas = a.autonomous_config?.focus_areas ?? [];
	const high = areas.find((f) => f.priority === 'high' && f.program?.trim());
	const any = high ?? areas.find((f) => f.program?.trim());
	if (any?.program) return normalizeProgram(any.program);
	const sec = a.harness?.program_section?.trim();
	if (sec) return sec.toLowerCase();
	return 'commons';
}

export function guildNameOf(id: string): string {
	return id
		.split(/[_\-\s]+/)
		.filter(Boolean)
		.map((w) => w.charAt(0).toUpperCase() + w.slice(1))
		.join(' ');
}

export function displayNameOf(a: AgentSummary): string {
	const explicitName = a.name?.trim();
	if (explicitName) return explicitName;
	return a.agent_id.trim() || 'Agent';
}

/** Short ROLE title for chips and the character sheet — "Presto · Personal
 * Agent", "Atlas · CEO". Single short ids read as acronyms (CEO, CTO, SRE);
 * multi-word ids prettify ("web-researcher" → "Web Researcher"). */
export function titleOf(a: AgentSummary): string {
	if (a.is_primary) return 'Personal Agent';
	const words = a.agent_id.split(/[_\-\s]+/).filter(Boolean);
	if (words.length === 1 && words[0].length <= 4) return words[0].toUpperCase();
	return words.map((w) => w.charAt(0).toUpperCase() + w.slice(1)).join(' ');
}

/** The envoy — Presto's public-facing agent. Identified by id/alias (the
 * backend's envoy_agent_id defaults to "envoy"; no roster flag exists). */
export function isEnvoyOf(a: AgentSummary): boolean {
	return a.agent_id === 'envoy' || (a.aliases ?? []).includes('envoy');
}

/** The CEO — the decomposition officer for strategic goals. Identified by
 * id/alias (the C-suite template's agent id). */
export function isCeoOf(a: AgentSummary): boolean {
	return a.agent_id === 'ceo' || (a.aliases ?? []).includes('ceo');
}

/** Mirror of the backend's `slugify_name` (alnum kept, everything else '-',
 * runs collapsed) — goal ids are `harness:{agent}:{slug(focus area name)}`. */
function goalSlug(name: string): string {
	const collapsed = name
		.toLowerCase()
		.replace(/[^a-z0-9]+/g, '-')
		.replace(/^-+|-+$/g, '');
	return collapsed || 'unnamed';
}

/** The agent's board-review goal id, when it has a review-flavoured focus
 * area (the CEO template's "board review"). Null hides the affordance —
 * rosters without a reviewing focus area simply don't get the button. */
export function boardReviewGoalIdOf(a: AgentSummary): string | null {
	const areas = a.autonomous_config?.focus_areas ?? [];
	const fa = areas.find((f) => /board|review/i.test(f.name));
	if (!fa) return null;
	return `harness:${a.agent_id}:${goalSlug(fa.name)}`;
}

/** Party-frame order = THE LEADERBOARD: 7d activity (calls) descending,
 * unknowns last, name tiebreak. Shared by the roster strip, the 1–9 selection
 * hotkeys, and the Crew Board sheet (all must agree). */
export function rosterOrder(citizens: CitizenVM[]): CitizenVM[] {
	return [...citizens].sort((a, b) => {
		const r = (b.calls7d ?? -1) - (a.calls7d ?? -1);
		return r !== 0 ? r : a.name.localeCompare(b.name);
	});
}

/** Deterministic heraldic hue per guild id — roster discs and the guild
 * banner must agree. */
export function guildHueOf(guildId: string): number {
	let h = 0;
	for (let i = 0; i < guildId.length; i++) h = (h * 31 + guildId.charCodeAt(i)) >>> 0;
	return h % 360;
}

export const VIBE_LABEL: Record<Vibe, string> = {
	working: 'Working',
	needs: 'Needs you',
	paused: 'Paused',
	offline: 'Offline',
	idle: 'Idle'
};

/** The longest whole word a role chip will carry. Real head nouns land well
 * under it (FACILITATOR is the longest on the current roster at 11). */
const PLAQUE_ROLE_MAX = 12;

/**
 * The standing plaque's role chip: what this citizen IS, in ONE whole word.
 *
 * A multi-word title collapses to its head noun — "Android Operator" reads
 * OPERATOR, "Harness Sre" reads SRE — which is the part that tells one crew
 * member from another; the qualifier is what the character sheet is for.
 * Acronym titles (CEO, CMO) are already one word and pass through.
 *
 * Nothing is ever cut mid-word: a title whose head noun is still too long
 * falls back to the live status, which is short by construction and is what
 * the chip's colour already says. "BRAINSTORM FA…" was never a label.
 *
 * The same fallback catches the head noun that merely repeats the name — the
 * Envoy's title IS "Envoy", and a plaque reading "ENVOY ENVOY" spends its
 * second half saying nothing. The status is the informative thing to put
 * there instead.
 *
 * Shared by the campus HUD and the office floor: both hang the same plaque on
 * the same crew, and two copies of this rule would eventually disagree about
 * what someone's role is on one surface and not the other. `guildName` is the
 * caller's own lookup, because the two surfaces hold their guilds differently.
 */
export function plaqueRole(citizen: CitizenVM, guildName?: string): string {
	const title = (citizen.title || guildName || '').trim();
	const words = title.split(/\s+/);
	const head = words[words.length - 1] ?? '';
	const stutters = head.toLowerCase() === citizen.name.trim().toLowerCase();
	return head && head.length <= PLAQUE_ROLE_MAX && !stutters ? head : VIBE_LABEL[citizen.vibe];
}

export function relativeTime(ms: number): string {
	if (!ms) return '—';
	const d = Date.now() - ms;
	const m = Math.floor(d / 60000);
	if (m < 1) return 'just now';
	if (m < 60) return `${m}m ago`;
	const h = Math.floor(m / 60);
	if (h < 24) return `${h}h ago`;
	return `${Math.floor(h / 24)}d ago`;
}
