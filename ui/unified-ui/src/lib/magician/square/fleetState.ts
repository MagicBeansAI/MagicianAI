import { get } from 'svelte/store';
import { timedFetch } from '$lib/shared/fetch';
import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import type { HandoffEdge } from './engine/handoffs';
import type { CitizenCurrentWorkVM } from './engine/types';
import { normalizeProgram } from './derive';
import { taskToQuest, type Quest } from './fleetQuests';

export type FleetSectionStatus = 'available' | 'partial' | 'unavailable';

export interface FleetSectionAvailability {
	status: FleetSectionStatus;
	sources: string[];
	limitations: string[];
}

export interface FleetStateAvailability {
	citizens: FleetSectionAvailability;
	guilds: FleetSectionAvailability;
	quests: FleetSectionAvailability;
	attention: FleetSectionAvailability;
	handoffs: FleetSectionAvailability;
	deliveries: FleetSectionAvailability;
	economy: FleetSectionAvailability;
	social?: FleetSectionAvailability;
}

export interface FleetStateCitizen {
	citizen_id: string;
	display_name: string;
	aliases: string[];
	role: string;
	description: string;
	version: number;
	disabled: boolean;
	is_primary: boolean;
	program_refs: string[];
	current_work: Array<{
		quest_id: string;
		title: string;
		status: string;
		execution_id: string | null;
		current_step: string | null;
		current_substep: string | null;
		is_blocked: boolean;
		updated_at: string;
	}>;
}

export interface FleetStateGuild {
	program_id: string;
	title: string;
	missions_markdown: string | null;
}

export interface FleetStateAttention {
	id: string;
	kind: string;
	title: string;
	summary: string | null;
	task_id: string | null;
	citizen_id: string | null;
	actions: Array<{ id: string; label: string; type: string; payload: unknown }>;
	untyped_action_count: number;
	created_at: number;
	updated_at: number;
}

export interface FleetStateHandoff {
	quest_id: string;
	parent_execution_id: string;
	child_execution_id: string | null;
	parent_step_id: string;
	from: { citizen_id: string; display_name: string | null };
	to: { citizen_id: string; display_name: string | null };
	reason: string;
	status: string;
	active: boolean;
	outcome_type: string | null;
	requested_at: string;
	updated_at: string;
}

export interface FleetStateDelivery {
	id: string;
	kind: string;
	title: string;
	summary: string | null;
	status: string;
	task_id: string | null;
	citizen_id: string | null;
	created_at: number;
	updated_at: number;
	metadata: unknown;
}

export interface FleetStateEconomy {
	enabled: boolean;
	freeze: unknown;
	accounts: Array<{ account_id: string; commodity: string; balance: string | number }>;
	active_reservation_count: number;
	journal_entry_count: number;
	spend_token_count: number;
}

export interface FleetStateSocialPost {
	post_id: string;
	parent_id: string | null;
	author_id: string;
	display_name: string;
	recent_activity: string;
	created_at: string;
}

export interface FleetStateSnapshot {
	schema_version: string;
	generated_at: string;
	scope: { principal: string; workspace: string };
	availability: FleetStateAvailability;
	citizens: FleetStateCitizen[];
	guilds: FleetStateGuild[];
	quests: unknown[];
	attention: FleetStateAttention[];
	handoffs: FleetStateHandoff[];
	deliveries: FleetStateDelivery[];
	economy: FleetStateEconomy | null;
	/** Public-feed rows. Absent on older snapshots cached across a remount. */
	ambient_social_activity?: FleetStateSocialPost[];
}

export async function fetchFleetState(): Promise<FleetStateSnapshot | null> {
	const scope = get(scopeIdentityStore);
	if (!scope.isResolved) return null;
	try {
		const response = await timedFetch('/api/magician/v2/fleet-state');
		if (!response.ok) return null;
		return (await response.json()) as FleetStateSnapshot;
	} catch {
		return null;
	}
}

export function fleetStateQuests(snapshot: FleetStateSnapshot | null): Quest[] {
	return (snapshot?.quests ?? [])
		.map(taskToQuest)
		.filter((quest): quest is Quest => quest != null);
}

export function fleetStateCurrentWork(
	citizen: FleetStateCitizen | undefined,
	blockedQuestIds: ReadonlySet<string> = new Set()
): CitizenCurrentWorkVM[] {
	return (citizen?.current_work ?? []).map((work) => ({
		questId: work.quest_id,
		title: work.title,
		status: work.status,
		executionId: work.execution_id,
		currentStep: work.current_step,
		currentSubstep: work.current_substep,
		isBlocked: work.is_blocked || blockedQuestIds.has(work.quest_id),
		updatedAt: work.updated_at
	}));
}

export function fleetStateProgramIds(citizen: FleetStateCitizen | undefined): string[] {
	return Array.from(
		new Set((citizen?.program_refs ?? []).map(normalizeProgram).filter(Boolean))
	);
}

export function fleetStateHandoffEdges(snapshot: FleetStateSnapshot | null): HandoffEdge[] {
	return (snapshot?.handoffs ?? [])
		.filter((handoff) => handoff.active && handoff.from.citizen_id && handoff.to.citizen_id)
		.map((handoff) => ({
			key: `${handoff.quest_id}:${handoff.parent_execution_id}:${handoff.child_execution_id ?? handoff.to.citizen_id}`,
			fromAgent: handoff.from.citizen_id,
			toAgent: handoff.to.citizen_id
		}));
}
