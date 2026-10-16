import { get } from 'svelte/store';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { readStudioFlag, vibeStudioStore } from './vibeStudioStore';

function setWindowWidth(width: number): void {
	Object.defineProperty(window, 'innerWidth', { configurable: true, writable: true, value: width });
}

beforeEach(() => {
	localStorage.clear();
	setWindowWidth(1440);
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

	it('remembers a collapsed build stage across project switches', () => {
		expect(get(vibeStudioStore).stageExpanded).toBe(true);
		vibeStudioStore.toggleStage();
		expect(get(vibeStudioStore).stageExpanded).toBe(false);
		expect(localStorage.getItem('magician.vibedev.stageExpanded')).toBe('false');
		vibeStudioStore.resetView();
		expect(get(vibeStudioStore).stageExpanded).toBe(false);
		vibeStudioStore.toggleStage(true);
		expect(localStorage.getItem('magician.vibedev.stageExpanded')).toBe('true');
	});

	it('starts the build stage drawer closed below 1280px without forgetting the wide-screen choice', () => {
		localStorage.setItem('magician.vibedev.stageExpanded', 'true');
		setWindowWidth(1100);
		vibeStudioStore.resetView();
		expect(get(vibeStudioStore).stageExpanded).toBe(false);
		expect(localStorage.getItem('magician.vibedev.stageExpanded')).toBe('true');
		setWindowWidth(1440);
		vibeStudioStore.resetView();
		expect(get(vibeStudioStore).stageExpanded).toBe(true);
	});

	describe('closing the stage', () => {
		afterEach(() => {
			vi.useRealTimers();
		});

		it('slides the drawer out before collapsing it below 1280px', () => {
			vi.useFakeTimers();
			setWindowWidth(1100);
			vibeStudioStore.resetView();
			vibeStudioStore.toggleStage(true);
			vibeStudioStore.closeStage();
			expect(get(vibeStudioStore)).toMatchObject({ stageExpanded: true, stageClosing: true });
			vi.advanceTimersByTime(200);
			expect(get(vibeStudioStore)).toMatchObject({ stageExpanded: false, stageClosing: false });
			expect(localStorage.getItem('magician.vibedev.stageExpanded')).toBe('false');
		});

		it('keeps the drawer open when it is reopened mid-slide', () => {
			vi.useFakeTimers();
			setWindowWidth(1100);
			vibeStudioStore.resetView();
			vibeStudioStore.toggleStage(true);
			vibeStudioStore.closeStage();
			vibeStudioStore.toggleStage(true);
			vi.advanceTimersByTime(200);
			expect(get(vibeStudioStore)).toMatchObject({ stageExpanded: true, stageClosing: false });
		});

		it('collapses straight away on wide screens, where the stage is a column', () => {
			vibeStudioStore.closeStage();
			expect(get(vibeStudioStore)).toMatchObject({ stageExpanded: false, stageClosing: false });
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
