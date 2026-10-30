import { describe, expect, it } from 'vitest';
import { createPanelScheduler } from './panelLoading';

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

describe('createPanelScheduler', () => {
	it('loads the opened category first and the rest once it has arrived', async () => {
		const started: string[] = [];
		let finishPriority: () => void = () => {};
		const scheduler = createPanelScheduler('voice', () => undefined);
		scheduler.load('voice', () => {
			started.push('voice');
			return new Promise((resolve) => (finishPriority = () => resolve({ default: 'Voice' })));
		});
		scheduler.load('agents', async () => {
			started.push('agents');
			return { default: 'Agents' };
		});
		scheduler.start();
		await flush();
		expect(started).toEqual(['voice']);
		finishPriority();
		await flush();
		expect(started).toEqual(['voice', 'agents']);
		await scheduler.allSettled;
	});

	it('releases the rest after the fallback even if the opened category stalls', async () => {
		const started: string[] = [];
		let fire: () => void = () => {};
		const scheduler = createPanelScheduler('voice', (fn) => (fire = fn));
		scheduler.load('voice', () => new Promise(() => {}));
		scheduler.load('data', async () => {
			started.push('data');
			return { default: 'Data' };
		});
		scheduler.start();
		await flush();
		expect(started).toEqual([]);
		fire();
		await flush();
		expect(started).toEqual(['data']);
	});

	it('counts a failed panel as settled', async () => {
		const scheduler = createPanelScheduler('account', () => undefined);
		const failing = scheduler.load('account', () => Promise.reject(new Error('offline')));
		scheduler.start();
		await expect(failing).rejects.toThrow('offline');
		await scheduler.allSettled;
	});
});
