import { describe, expect, it } from 'vitest';
import { buildCrewMemberOverviewComponent, type CrewMemberOverviewModel } from './overview';

function valuesFor(overview: CrewMemberOverviewModel): Record<string, string> {
	const component = buildCrewMemberOverviewComponent(overview);
	const items = component.children?.[0]?.props?.items;
	if (!Array.isArray(items)) return {};
	return Object.fromEntries(
		items.map((item) => {
			const record = item as { key: string; value: string };
			return [record.key, record.value];
		})
	);
}

describe('crew member overview', () => {
	it('preserves unavailable, empty, and zero as distinct values', () => {
		const values = valuesFor({
			agentKind: 'Worker',
			capabilityPacks: null,
			excludedPacks: [],
			delegationTargets: [],
			maxDelegationDepth: 0,
			userMemoryIsolation: null,
			readableAgents: null
		});

		expect(values['Capability packs']).toBe('Unavailable');
		expect(values['Excluded packs']).toBe('none');
		expect(values['Delegates to']).toBe('none');
		expect(values['Max delegation depth']).toBe('0');
	});

	it('retains Personal-agent memory isolation fields', () => {
		const values = valuesFor({
			agentKind: 'Personal',
			capabilityPacks: [],
			excludedPacks: [],
			delegationTargets: ['researcher'],
			maxDelegationDepth: 2,
			userMemoryIsolation: 'fully_isolated',
			readableAgents: []
		});

		expect(values['User memory isolation']).toBe('Fully Isolated');
		expect(values['Readable agents']).toBe('none');
		expect(values['Delegates to']).toBe('researcher');
	});

	it('uses product terminology for the shared overview card', () => {
		const component = buildCrewMemberOverviewComponent({
			agentKind: 'Worker',
			capabilityPacks: [],
			excludedPacks: [],
			delegationTargets: [],
			maxDelegationDepth: 1,
			userMemoryIsolation: null,
			readableAgents: null
		});

		expect(component.props?.title).toBe('Crew member overview');
	});
});
