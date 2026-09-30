import { get } from 'svelte/store';
import { beforeEach, describe, expect, it } from 'vitest';

import { readStudioFlag, vibeStudioStore } from './vibeStudioStore';

beforeEach(() => {
	localStorage.clear();
	vibeStudioStore.resetView();
});

describe('vibeStudioStore browser preferences', () => {
	it('starts with manual proposal review and automatic visual correction', () => {
		expect(get(vibeStudioStore)).toMatchObject({
			stageTab: 'preview',
			mode: 'build',
			autoApplyCodeProposals: false,
			visualSelfCorrect: true,
			costBudgetUsd: null
		});
		expect(vibeStudioStore.policyPromptLine()).toContain('manual review is enabled');
	});

	it('persists proposal policy and prevents client auto-apply in discuss or autopilot mode', () => {
		vibeStudioStore.setAutoApply(true);
		expect(localStorage.getItem('magician.vibedev.autoApplyCodeProposals')).toBe('true');
		expect(vibeStudioStore.policyPromptLine()).toContain('auto-apply is enabled');

		vibeStudioStore.setMode('discuss');
		expect(get(vibeStudioStore).autoApplyCodeProposals).toBe(false);
		expect(vibeStudioStore.policyPromptLine()).toContain('read-only request');
		expect(vibeStudioStore.planDirectiveBlock()).toContain('NO code changes');

		vibeStudioStore.setMode('autopilot');
		expect(vibeStudioStore.autopilotDirectiveBlock()).toContain('UNATTENDED');
		expect(get(vibeStudioStore).autoApplyCodeProposals).toBe(false);
	});

	it('normalizes and persists the per-run cost budget', () => {
		vibeStudioStore.setCostBudget(2.5);
		expect(get(vibeStudioStore).costBudgetUsd).toBe(2.5);
		expect(localStorage.getItem('magician.vibedev.costBudgetUsd')).toBe('2.5');
		expect(vibeStudioStore.costBudgetLine()).toContain('$2.50');

		vibeStudioStore.setCostBudget(0);
		expect(get(vibeStudioStore).costBudgetUsd).toBeNull();
		expect(localStorage.getItem('magician.vibedev.costBudgetUsd')).toBeNull();
		expect(vibeStudioStore.costBudgetLine()).toBe('');
	});

	it('keeps project-switch reset behavior deterministic', () => {
		vibeStudioStore.setStageTab('diff');
		vibeStudioStore.setDeviceFrame('phone');
		vibeStudioStore.toggleRail(false);
		vibeStudioStore.toggleTerminal(true);

		expect(get(vibeStudioStore)).toMatchObject({
			stageTab: 'diff',
			deviceFrame: 'phone',
			railExpanded: false,
			terminalOpen: true
		});
		vibeStudioStore.resetView();
		expect(get(vibeStudioStore)).toMatchObject({
			stageTab: 'preview',
			deviceFrame: 'desktop',
			railExpanded: true,
			terminalOpen: false
		});
	});

	it('gates visual correction to visual build work', () => {
		expect(vibeStudioStore.visualSelfCorrectDirectiveBlock(true)).toContain(
			'screenshot_preview'
		);
		expect(vibeStudioStore.visualSelfCorrectDirectiveBlock(false)).toBe('');
		vibeStudioStore.setVisualSelfCorrect(false);
		expect(vibeStudioStore.visualSelfCorrectDirectiveBlock(true)).toBe('');
	});

	it('reads, persists, and overrides the studio feature flag from the URL', () => {
		expect(readStudioFlag(new URLSearchParams('studio=0'))).toBe(false);
		expect(localStorage.getItem('magician.vibedev.studio')).toBe('false');
		expect(readStudioFlag(new URLSearchParams())).toBe(false);
		expect(readStudioFlag(new URLSearchParams('studio=1'))).toBe(true);
		expect(localStorage.getItem('magician.vibedev.studio')).toBe('true');
	});
});
