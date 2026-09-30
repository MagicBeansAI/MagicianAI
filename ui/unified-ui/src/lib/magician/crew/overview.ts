import type { AgentSummary } from '$lib/stores/agentStore';
import type { CrewNativeComponent } from './nativeSurface';

export const CREW_MEMBER_OVERVIEW_COMPONENT_ID = 'presto-double-detail-overview';

export interface CrewMemberOverviewModel {
	agentKind: string | null;
	capabilityPacks: string[] | null;
	excludedPacks: string[] | null;
	delegationTargets: string[] | null;
	maxDelegationDepth: number | null;
	userMemoryIsolation: string | null;
	readableAgents: string[] | null;
}

function listValue(values: string[] | null): string {
	if (values === null) return 'Unavailable';
	return values.length > 0 ? values.join(', ') : 'none';
}

export function crewMemberOverviewFromSummary(summary: AgentSummary): CrewMemberOverviewModel {
	return {
		agentKind: summary.kind ?? null,
		capabilityPacks: summary.tools ?? null,
		excludedPacks: summary.excluded_tools ?? null,
		delegationTargets: summary.delegation_targets ?? null,
		maxDelegationDepth: summary.max_delegation_depth ?? null,
		userMemoryIsolation: summary.user_memory_isolation ?? null,
		readableAgents: summary.readable_agents ?? null
	};
}

export function buildCrewMemberOverviewComponent(
	overview: CrewMemberOverviewModel
): CrewNativeComponent {
	const kind = overview.agentKind ?? 'Unavailable';
	const items = [
		{ id: 'overview-kind', key: 'Kind', value: kind },
		{ id: 'overview-caps', key: 'Capability packs', value: listValue(overview.capabilityPacks) },
		{ id: 'overview-excluded', key: 'Excluded packs', value: listValue(overview.excludedPacks) },
		{ id: 'overview-delegates', key: 'Delegates to', value: listValue(overview.delegationTargets) },
		{
			id: 'overview-depth',
			key: 'Max delegation depth',
			value: overview.maxDelegationDepth === null ? 'Unavailable' : String(overview.maxDelegationDepth)
		}
	];

	if (overview.agentKind === 'Personal') {
		const isolationLabel = overview.userMemoryIsolation === null
			? 'Unavailable'
			: overview.userMemoryIsolation === 'fully_isolated'
				? 'Fully Isolated'
				: 'Shared';
		items.push(
			{ id: 'overview-memory-isolation', key: 'User memory isolation', value: isolationLabel },
			{ id: 'overview-readable-agents', key: 'Readable agents', value: listValue(overview.readableAgents) }
		);
	}

	const kindDescriptions: Record<string, string> = {
		Personal: 'User-facing agent that appears in the task creation picker. Can delegate work to Worker agents.',
		Worker: 'Handles delegated work from Personal or other agents. Has capability packs for specialized tasks.'
	};

	return {
		id: CREW_MEMBER_OVERVIEW_COMPONENT_ID,
		component_type: 'Card',
		props: {
			title: 'Crew member overview',
			subtitle: kindDescriptions[kind] || 'Agent configuration and coordination overview.',
			body: ''
		},
		children: [
			{
				id: 'presto-double-detail-overview-data',
				component_type: 'DataList',
				props: { items }
			}
		]
	};
}
