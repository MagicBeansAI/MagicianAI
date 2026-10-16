/**
 * Fleet Civilization engine — shared types.
 *
 * The engine renders CitizenVM/GuildVM view-models (derived from AgentSummary in
 * FleetWorld.svelte) and knows nothing about stores or endpoints. Targets passed
 * across the engine/HUD boundary are strings: `agent:<id>` or `guild:<id>`.
 */

export type Vibe = 'working' | 'needs' | 'paused' | 'offline' | 'idle';

/** A focus area distilled for the character sheet's Quests block. */
export interface CitizenQuest {
	name: string;
	program: string | null;
	schedule: string | null;
	priority: string | null;
}

export interface CitizenVM {
	id: string;
	/** Display name — persona > name > agent_id. */
	name: string;
	/** One-line role/subtitle (agent description or id). */
	role: string;
	/** Short role TITLE for chips/sheet ("Personal Agent", "CEO"). */
	title: string;
	/** Trust level from the definition (e.g. local). */
	trustLevel: string | null;
	/** First-seen timestamp ms ("born"), when known. */
	bornAt: number | null;
	/** Focus areas distilled for the sheet's Quests block. */
	quests: CitizenQuest[];
	vibe: Vibe;
	guildId: string;
	/** Every authoritative program membership, not only the primary district. */
	guildIds: string[];
	executionId?: string;
	/** Current work joined from the fleet-state task projection. */
	currentWork: CitizenCurrentWorkVM[];
	pendingApprovals: number;
	lastGoal?: string;
	lastOutcome?: string;
	/** ms timestamp of last status update (for "last active"). */
	updatedAt: number;
	/** Canonical overall health 0-100 from the backend health projection. */
	health: number | null;
	/** Available-input ratio for the overall health projection (0-1). */
	healthCoverage: number | null;
	/** Average of available durable daily health snapshots in the rolling 7d window. */
	healthAverage7d: number | null;
	/** Oldest-to-newest health delta in the rolling 7d window. */
	healthDelta7d: number | null;
	/** Number of daily health snapshots currently represented in the 7d average. */
	healthSampleDays7d: number;
	/** LLM spend over the last 7 days (USD), from analytics. */
	spendUsd7d: number | null;
	/** Success rate 0..1 over the last 7 days; null when no calls. */
	successRate7d: number | null;
	/** LLM calls over the last 7 days (activity — the leaderboard rank key). */
	calls7d: number | null;
	/** Idle with no recent activity — naps at their bench with a 💤. */
	resting: boolean;
	/** The primary agent (Presto) — golden halo, ⭐ in the HUD. */
	isPrimary: boolean;
	/** The envoy (Presto's public face) — smaller silver halo, ✨ in the HUD. */
	isEnvoy: boolean;
	/** The CEO — target of strategic-goal decomposition quests. */
	isCeo: boolean;
	/** Goal id of the agent's review-flavoured focus area (null = none) —
	 * drives the CEO panel's "Board review" trigger. */
	boardReviewGoalId: string | null;
	/** Tools this citizen wields (drives the Armory inventory). */
	tools: string[];
	/** Standing delegation targets (drives the Council command network). */
	delegationTargets: string[];
}

export interface CitizenCurrentWorkVM {
	questId: string;
	title: string;
	status: string;
	executionId: string | null;
	currentStep: string | null;
	currentSubstep: string | null;
	isBlocked: boolean;
	updatedAt: string;
}

/** Civic landmarks — interactive structures beyond guilds. The Hall (you) is
 * targetable too (quest-drops land there for auto-routed work). */
export type LandmarkId = 'armory' | 'council' | 'hall';

export interface GuildVM {
	id: string;
	name: string;
	memberIds: string[];
	/** Authoritative managed mission section when available. */
	missionsMarkdown?: string | null;
	activeQuestCount: number;
	blockedQuestCount: number;
	deliveredQuestCount: number;
}

/** World palette read from the --fleet-* game-theme tokens (plain hex). */
export interface WorldPalette {
	skyTop: string;
	skyBottom: string;
	ground: string;
	groundAlt: string;
	path: string;
	plaza: string;
	foliage: string;
	trunk: string;
	rock: string;
	wall: string;
	roof: string;
	roofHall: string;
	ink: string;
	handoff: string;
	status: Record<Vibe, string>;
}

export interface GodHandDrop {
	citizenId: string;
	/** Guild the citizen was dropped on; null = dropped on open ground. */
	guildId: string | null;
	/** Drop point in host-local pixels (for anchoring the steer composer). */
	screenX: number;
	screenY: number;
}

export interface EngineCallbacks {
	/** Hover target changed (raycast). Null = nothing hovered. */
	onHover?: (target: string | null) => void;
	/** Selection changed (click / Esc / empty-ground click / API). */
	onSelect?: (target: string | null) => void;
	/** Camera left / returned to its home framing (drives the Home control). */
	onCameraMoved?: (moved: boolean) => void;
	/** God-Hand: a citizen was picked up and released. */
	onGodHand?: (drop: GodHandDrop) => void;
}

export function agentTarget(id: string): string {
	return `agent:${id}`;
}
export function guildTarget(id: string): string {
	return `guild:${id}`;
}
export function landmarkTarget(id: LandmarkId): string {
	return `landmark:${id}`;
}
export function parseTarget(
	target: string
): { kind: 'agent' | 'guild' | 'landmark'; id: string } | null {
	if (target.startsWith('agent:')) return { kind: 'agent', id: target.slice(6) };
	if (target.startsWith('guild:')) return { kind: 'guild', id: target.slice(6) };
	if (target.startsWith('landmark:')) return { kind: 'landmark', id: target.slice(9) };
	return null;
}
