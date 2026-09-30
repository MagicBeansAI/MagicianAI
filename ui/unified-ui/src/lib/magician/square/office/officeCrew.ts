/**
 * Roster -> view models for the office floor.
 *
 * FleetWorld builds the full CitizenVM (health, 7-day spend, quests, handoffs,
 * board-review goals) because its HUD renders all of it. The floor renders a
 * strict subset — who, what they are called, what program they are on, and how
 * they are doing right now — so this builds that subset from the SAME shared
 * helpers (derive.ts, fleetState.ts, attentionGlue.ts) rather than re-deriving
 * anything. Any disagreement between the two surfaces about someone's status or
 * program would therefore be a bug in the shared helpers, not a second opinion
 * living here.
 *
 * Kept pure and out of the component so the mapping is inspectable on its own.
 */

import type { AgentSummary } from '$lib/stores/agentStore';
import type { CitizenVM, GuildVM } from '../engine/types';
import {
	citizenVibeOf,
	displayNameOf,
	guildIdOf,
	guildNameOf,
	isCeoOf,
	isEnvoyOf,
	normalizeProgram,
	titleOf
} from '../derive';
import {
	fleetStateCurrentWork,
	fleetStateProgramIds,
	type FleetStateSnapshot
} from '../fleetState';
import type { NeedsItem } from '../attentionGlue';

export interface OfficeCrew {
	citizens: CitizenVM[];
	guilds: GuildVM[];
}

/** Idle this long with no activity and the figure gets a 'z' at their desk —
 * the same threshold the campus naps on. */
const RESTING_AFTER_MS = 30 * 60_000;

export function buildOfficeCrew(
	agents: readonly AgentSummary[],
	fleetState: FleetStateSnapshot | null,
	needsByAgent: ReadonlyMap<string, NeedsItem[]>,
	options: { backendDown?: boolean; now?: number } = {}
): OfficeCrew {
	const backendDown = options.backendDown ?? false;
	const now = options.now ?? Date.now();
	const projected = new Map((fleetState?.citizens ?? []).map((c) => [c.citizen_id, c]));

	const citizens = agents.map((agent): CitizenVM => {
		const snapshot = projected.get(agent.agent_id);
		const currentWork = fleetStateCurrentWork(snapshot);
		const programIds = fleetStateProgramIds(snapshot);
		const guildId = programIds[0] ?? guildIdOf(agent);
		let vibe = citizenVibeOf(agent, currentWork, (needsByAgent.get(agent.agent_id)?.length ?? 0) > 0);
		// Backend down: claimed statuses cannot be trusted, so nobody is "working".
		if (backendDown && vibe !== 'offline') vibe = 'idle';
		const lastActive = agent.updated_at || 0;
		return {
			id: agent.agent_id,
			name: snapshot?.display_name || displayNameOf(agent),
			role: snapshot?.description || agent.description || agent.agent_id,
			title: titleOf(agent),
			trustLevel: agent.trust_level ?? null,
			bornAt: agent.created_at ?? null,
			quests: [],
			vibe,
			guildId,
			guildIds: programIds.length > 0 ? programIds : [guildId],
			currentWork,
			executionId:
				currentWork.find((work) => work.executionId)?.executionId ?? agent.current_execution_id,
			pendingApprovals: agent.pending_approvals ?? 0,
			lastGoal: currentWork[0]?.title ?? agent.last_goal,
			lastOutcome: agent.last_outcome,
			updatedAt: agent.updated_at,
			health: null,
			healthCoverage: null,
			healthAverage7d: null,
			healthDelta7d: null,
			healthSampleDays7d: 0,
			spendUsd7d: null,
			successRate7d: null,
			calls7d: null,
			resting:
				!backendDown && vibe === 'idle' && lastActive > 0 && now - lastActive > RESTING_AFTER_MS,
			isPrimary: agent.is_primary ?? false,
			isEnvoy: isEnvoyOf(agent),
			isCeo: isCeoOf(agent),
			boardReviewGoalId: null,
			tools: agent.tools ?? [],
			delegationTargets: agent.delegation_targets ?? []
		};
	});

	/**
	 * Programs come from EVERY membership, seats come from the primary one.
	 * A program that is only ever someone's second attachment therefore gets a
	 * room with nobody in it — which is the true statement "nobody is working
	 * that program", and the reason the floor can show vacancy at all.
	 */
	const byId = new Map<string, GuildVM>();
	const ensure = (id: string, name?: string): GuildVM => {
		const existing = byId.get(id);
		if (existing) {
			if (name) existing.name = name;
			return existing;
		}
		const created: GuildVM = {
			id,
			name: name || guildNameOf(id),
			memberIds: [],
			activeQuestCount: 0,
			blockedQuestCount: 0,
			deliveredQuestCount: 0
		};
		byId.set(id, created);
		return created;
	};
	if (fleetState?.availability.guilds.status !== 'unavailable') {
		for (const guild of fleetState?.guilds ?? []) {
			ensure(normalizeProgram(guild.program_id), guild.title);
		}
	}
	for (const citizen of citizens) {
		for (const id of citizen.guildIds) ensure(id);
		ensure(citizen.guildId).memberIds.push(citizen.id);
	}

	return {
		citizens,
		guilds: [...byId.values()].sort((a, b) => a.id.localeCompare(b.id))
	};
}
