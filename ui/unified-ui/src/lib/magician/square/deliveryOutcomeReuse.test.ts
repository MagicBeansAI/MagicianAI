import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';

function source(path: string): string {
	return readFileSync(resolve(process.cwd(), path), 'utf8');
}

describe('canonical delivery and outcome review reuse', () => {
	it('keeps the Task router limited to concise game-native result signals', () => {
		const inspector = source('src/lib/magician/square/hud/LandmarkInspector.svelte');

		expect(inspector).toContain("import { relativeTime } from '../derive'");
		expect(inspector).toContain('deliveryStatusLabel(delivery.status)');
		expect(inspector).toContain('{#if delivery.summary}<p>{delivery.summary}</p>{/if}');
		for (const duplicatedDetail of [
			'GameEncounter',
			'selectedDeliveryId',
			'selectedDeliveryDetails',
			'delivery.metadata',
			'acceptance_criteria',
			'Close review'
		]) {
			expect(inspector).not.toContain(duplicatedDetail);
		}
	});

	it('opens task-backed results in the existing canonical Tasks overlay', () => {
		const inspector = source('src/lib/magician/square/hud/LandmarkInspector.svelte');
		const hud = source('src/lib/magician/square/hud/FleetHud.svelte');
		const journal = source('src/lib/magician/square/hud/QuestJournal.svelte');

		expect(inspector).toContain("dispatch('reviewtask', delivery.task_id)");
		expect(inspector).not.toContain('/tasks?selected=');
		expect(hud).toContain('function openTaskReview(taskId: string)');
		expect(hud).toContain('on:reviewtask={(event) => openTaskReview(event.detail)}');
		expect(hud).toContain('void goto(`/tasks?selected=${encodeURIComponent(normalizedTaskId)}`)');
		expect(journal).toContain("import TasksWorkspace from '$lib/magician/tasks/TasksWorkspace.svelte'");
		expect(journal).toContain('<TasksWorkspace navigationMode="local" {initialSelectedTaskId} />');
		expect(journal).toContain('Open full Tasks page');
	});

	it('routes feed-only results to Delivered on Today without inventing tasks', () => {
		const inspector = source('src/lib/magician/square/hud/LandmarkInspector.svelte');

		expect(inspector).toContain("new URLSearchParams({ tab: 'delivered', selected_item: delivery.id })");
		expect(inspector).toContain('Open in Today');
		expect(inspector).toContain('deliveryCanUseFollowUp(delivery)');
		expect(inspector).toContain("dispatch('followup', delivery)");
		expect(inspector).not.toContain('taskStore');
		expect(inspector).not.toContain('timedFetch');
	});
});
