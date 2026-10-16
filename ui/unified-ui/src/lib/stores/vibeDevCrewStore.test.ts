import { get } from 'svelte/store';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { vibeDevCrewStore } from './vibeDevCrewStore';

const crew = (status: string, lanes = [{ lane: 'API', brief: 'Codes', owns: [], status: 'pending' }]) => ({
	crew_id: 'crew_1',
	goal: 'Login with codes',
	status,
	lanes,
	created_at_ms: 1,
	updated_at_ms: 1
});

const json = (body: unknown) =>
	new Response(JSON.stringify(body), { status: 200, headers: { 'Content-Type': 'application/json' } });

let slowRead: ((response: Response) => void) | null = null;
let holdReads = false;

beforeEach(() => {
	holdReads = false;
	slowRead = null;
	vi.stubGlobal(
		'fetch',
		vi.fn(async (_request: string | URL | Request, init?: RequestInit) => {
			if (init?.method === 'POST') return json({ crew: crew('running') });
			if (holdReads) return new Promise<Response>((resolve) => (slowRead = resolve));
			return json({ crews: [crew('proposed')] });
		})
	);
});

afterEach(() => {
	vibeDevCrewStore.load(null);
	vi.unstubAllGlobals();
});

async function loaded(projectId: string) {
	vibeDevCrewStore.load(projectId);
	await vi.waitFor(() => expect(get(vibeDevCrewStore).loaded).toBe(true));
}

describe('vibeDevCrewStore', () => {
	it('keeps a Start that lands after a slower read began', async () => {
		await loaded('proj');
		holdReads = true;
		const read = vibeDevCrewStore.refresh();
		await vi.waitFor(() => expect(slowRead).not.toBeNull());

		await vibeDevCrewStore.start('crew_1');
		expect(get(vibeDevCrewStore).crews[0].status).toBe('running');

		slowRead?.(json({ crews: [crew('proposed')] }));
		await read;
		expect(get(vibeDevCrewStore).crews[0].status).toBe('running');
	});

	it('drops an action result after the owner switched project', async () => {
		await loaded('proj');
		const started = vibeDevCrewStore.start('crew_1');
		vibeDevCrewStore.load('other');
		await started;
		const state = get(vibeDevCrewStore);
		expect(state.projectId).toBe('other');
		expect(state.crews.some((row) => row.status === 'running')).toBe(false);
		expect(state.busy).toEqual([]);
	});

	it('tracks each busy crew on its own', async () => {
		await loaded('proj');
		// Assigned inside the fetch stub, so TypeScript must not narrow it to null.
		let finishCreate = null as ((response: Response) => void) | null;
		vi.stubGlobal(
			'fetch',
			vi.fn(async (request: string | URL | Request, init?: RequestInit) => {
				if (init?.method === 'POST' && String(request).includes('/start')) return json({ crew: crew('running') });
				if (init?.method === 'POST') return new Promise<Response>((resolve) => (finishCreate = resolve));
				return json({ crews: [crew('proposed')] });
			})
		);
		const creating = vibeDevCrewStore.create({ goal: 'Another goal' });
		await vibeDevCrewStore.start('crew_1');
		expect(get(vibeDevCrewStore).busy).toEqual(['new']);
		finishCreate?.(json({ crew: crew('proposed') }));
		await creating;
		expect(get(vibeDevCrewStore).busy).toEqual([]);
	});
});
