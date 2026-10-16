import type { AgentSummary } from '$lib/stores/agentStore';
import type { CitizenVM, Vibe } from '$lib/magician/square/engine/types';
import {
	displayNameOf,
	citizenVibeOf,
	guildIdOf,
	guildNameOf,
	isEnvoyOf
} from '$lib/magician/square/derive';
import {
	fleetStateCurrentWork,
	fleetStateProgramIds,
	fleetStateQuests,
	type FleetStateSnapshot
} from '$lib/magician/square/fleetState';
import type { AgentHealthProjection } from './health';

export interface CrewLeaderboardRow {
	id: string;
	rank: number;
	name: string;
	isPrimary: boolean;
	isEnvoy: boolean;
	program: string;
	vibe: Vibe;
	health: number | null;
	health7d: number | null;
	healthDelta7d: number | null;
	spendUsd7d: number | null;
	calls7d: number | null;
	successRate7d: number | null;
	lastActiveAt: number;
}

type UnrankedCrewLeaderboardRow = Omit<CrewLeaderboardRow, 'rank'>;

function rankRows(rows: UnrankedCrewLeaderboardRow[]): CrewLeaderboardRow[] {
	return [...rows]
		.sort((left, right) => {
			const calls = (right.calls7d ?? -1) - (left.calls7d ?? -1);
			if (calls !== 0) return calls;
			const spend = (right.spendUsd7d ?? -1) - (left.spendUsd7d ?? -1);
			return spend !== 0 ? spend : left.name.localeCompare(right.name);
		})
		.map((row, index) => ({ ...row, rank: index + 1 }));
}

export function crewLeaderboardRowsFromAgents(
	agents: AgentSummary[],
	healthByAgent: Map<string, AgentHealthProjection> | null,
	fleetState: FleetStateSnapshot | null = null,
	attentionAgentIds: ReadonlySet<string> = new Set()
): CrewLeaderboardRow[] {
	const fleetCitizenById = new Map(
		(fleetState?.citizens ?? []).map((citizen) => [citizen.citizen_id, citizen])
	);
	const blockedQuestIds = new Set(
		fleetStateQuests(fleetState)
			.filter(
				(quest) =>
					quest.isBlocked
					|| quest.pendingQuestions.length > 0
					|| quest.state === 'awaiting_orders'
					|| quest.state === 'blocked'
			)
			.map((quest) => quest.id)
	);
	return rankRows(
		agents.map((agent) => {
			const health = healthByAgent?.get(agent.agent_id);
			const projected = fleetCitizenById.get(agent.agent_id);
			const currentWork = fleetStateCurrentWork(projected, blockedQuestIds);
			const projectedPrograms = fleetStateProgramIds(projected);
			return {
				id: agent.agent_id,
				name: projected?.display_name || displayNameOf(agent),
				isPrimary: agent.is_primary ?? false,
				isEnvoy: isEnvoyOf(agent),
				program: guildNameOf(projectedPrograms[0] ?? guildIdOf(agent)),
				vibe: citizenVibeOf(agent, currentWork, attentionAgentIds.has(agent.agent_id)),
				health: health?.overall.score ?? null,
				health7d: health?.rolling_7d.score_average ?? null,
				healthDelta7d: health?.rolling_7d.score_delta ?? null,
				spendUsd7d: health?.rolling_7d.spend_usd ?? null,
				calls7d: health?.rolling_7d.calls ?? null,
				successRate7d: health?.rolling_7d.success_rate ?? null,
				lastActiveAt: Math.max(
					health?.overall.inputs.last_activity_at_ms ?? 0,
					agent.updated_at || 0
				)
			};
		})
	);
}

export function crewLeaderboardRowsFromCitizens(
	citizens: CitizenVM[]
): CrewLeaderboardRow[] {
	return rankRows(
		citizens.map((citizen) => ({
			id: citizen.id,
			name: citizen.name,
			isPrimary: citizen.isPrimary,
			isEnvoy: citizen.isEnvoy,
			program: guildNameOf(citizen.guildId),
			vibe: citizen.vibe,
			health: citizen.health,
			health7d: citizen.healthAverage7d,
			healthDelta7d: citizen.healthDelta7d,
			spendUsd7d: citizen.spendUsd7d,
			calls7d: citizen.calls7d,
			successRate7d: citizen.successRate7d,
			lastActiveAt: citizen.updatedAt
		}))
	);
}
