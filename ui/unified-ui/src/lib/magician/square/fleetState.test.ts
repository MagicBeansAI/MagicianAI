import { describe, expect, it } from 'vitest';
import {
	fleetStateCurrentWork,
	fleetStateHandoffEdges,
	fleetStateProgramIds,
	fleetStateQuests,
	type FleetStateCitizen,
	type FleetStateSnapshot
} from './fleetState';

const citizen: FleetStateCitizen = {
	citizen_id: 'cto',
	display_name: 'CTO',
	aliases: [],
	role: 'personal',
	description: 'Engineering command',
	version: 1,
	disabled: false,
	is_primary: false,
	program_refs: ['programs/Engineering_Strategy.md', 'engineering_strategy'],
	current_work: [
		{
			quest_id: 'task-1',
			title: 'Build fleet state',
			status: 'running',
			execution_id: 'exec-1',
			current_step: 'Compose data',
			current_substep: null,
			is_blocked: false,
			updated_at: '2026-07-10T10:00:00Z'
		}
	]
};

function snapshot(): FleetStateSnapshot {
	return {
		schema_version: 'fleet_state.v1alpha1',
		generated_at: '2026-07-10T10:00:00Z',
		scope: { principal: 'anonymous', workspace: 'default' },
		availability: {
			citizens: { status: 'available', sources: [], limitations: [] },
			guilds: { status: 'available', sources: [], limitations: [] },
			quests: { status: 'available', sources: [], limitations: [] },
			attention: { status: 'available', sources: [], limitations: [] },
			handoffs: { status: 'available', sources: [], limitations: [] },
			deliveries: { status: 'available', sources: [], limitations: [] },
			economy: { status: 'available', sources: [], limitations: [] },
			social: { status: 'available', sources: [], limitations: [] }
		},
		citizens: [citizen],
		guilds: [],
		quests: [{ id: 'task-1', title: 'Build fleet state', status: 'running', created_at: '2026-07-10T09:00:00Z', updated_at: '2026-07-10T10:00:00Z' }],
		attention: [],
		handoffs: [
			{
				quest_id: 'task-1',
				parent_execution_id: 'exec-1',
				child_execution_id: 'exec-2',
				parent_step_id: 'step-1',
				from: { citizen_id: 'cto', display_name: 'CTO' },
				to: { citizen_id: 'frontend-engineer', display_name: 'Frontend Engineer' },
				reason: 'UI implementation',
				status: 'running',
				active: true,
				outcome_type: null,
				requested_at: '2026-07-10T09:00:00Z',
				updated_at: '2026-07-10T10:00:00Z'
			}
		],
		deliveries: [],
		economy: null
	};
}

describe('fleet state projection', () => {
	it('normalizes memberships and current work', () => {
		expect(fleetStateProgramIds(citizen)).toEqual(['engineering_strategy']);
		expect(fleetStateCurrentWork(citizen)[0]).toMatchObject({
			questId: 'task-1',
			currentStep: 'Compose data'
		});
	});

	it('projects rich tasks and active handoffs without guessing identities', () => {
		const value = snapshot();
		expect(fleetStateQuests(value)[0]).toMatchObject({ id: 'task-1', state: 'active' });
		expect(fleetStateHandoffEdges(value)).toEqual([
			{
				key: 'task-1:exec-1:exec-2',
				fromAgent: 'cto',
				toAgent: 'frontend-engineer'
			}
		]);
	});
});
