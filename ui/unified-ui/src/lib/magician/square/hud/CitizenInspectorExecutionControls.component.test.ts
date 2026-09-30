import { cleanup, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';
import CitizenInspector from './CitizenInspector.svelte';
import type { CitizenVM } from '../engine/types';

afterEach(() => {
	cleanup();
	vi.unstubAllGlobals();
});

describe('CitizenInspector execution controls', () => {
	it('uses the citizen execution id for the canonical Town Square controls', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn().mockResolvedValue(
				new Response(
					JSON.stringify({
						execution_id: 'exec-square',
						waiting_state: 'executing',
						active: true,
						can_pause: true,
						can_resume: false,
						can_steer: true,
						can_cancel: true
					}),
					{ status: 200, headers: { 'Content-Type': 'application/json' } }
				)
			)
		);
		const citizen: CitizenVM = {
			id: 'researcher',
			name: 'Researcher',
			role: 'Research agent',
			title: 'Researcher',
			trustLevel: 'local',
			bornAt: null,
			quests: [],
			vibe: 'working',
			guildId: 'research',
			guildIds: ['research'],
			executionId: 'exec-square',
			currentWork: [
				{
					questId: 'task-square',
					title: 'Investigate release',
					status: 'running',
					executionId: 'exec-square',
					currentStep: 'Reviewing logs',
					currentSubstep: null,
					isBlocked: false,
					updatedAt: '2026-07-13T00:00:00Z'
				}
			],
			pendingApprovals: 0,
			updatedAt: Date.now(),
			health: 100,
			healthCoverage: 1,
			healthAverage7d: 100,
			healthDelta7d: 0,
			healthSampleDays7d: 7,
			spendUsd7d: 0,
			successRate7d: 1,
			calls7d: 1,
			resting: false,
			isPrimary: false,
			isEnvoy: false,
			isCeo: false,
			boardReviewGoalId: null,
			tools: [],
			delegationTargets: []
		};

		render(CitizenInspector, {
			engine: { characterTemplateFor: () => null } as never,
			citizen
		});

		expect(await screen.findByRole('button', { name: 'Steer run' })).toBeEnabled();
		expect(screen.getByRole('button', { name: 'Pause run' })).toBeEnabled();
		expect(screen.getByRole('button', { name: 'Stop run' })).toBeEnabled();
		expect(screen.getByRole('link', { name: 'Inspect run' })).toHaveAttribute(
			'href',
			'/tasks?selected=task-square'
		);
	});
});
