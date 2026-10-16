import { cleanup, render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { VibeDevProject } from '$lib/stores/vibeDevProjectStore';
import { vibeDevCrewStore } from '$lib/stores/vibeDevCrewStore';
import CrewSection from './CrewSection.svelte';

const trusted = {
	project_id: 'proj',
	name: 'Shop',
	chat_thread_id: 'vibedev',
	chat_session_id: 'sess',
	trust: 'trusted',
	created_at_ms: 1
} as unknown as VibeDevProject;

const proposed = {
	crew_id: 'crew_1',
	goal: 'Add login with email codes',
	status: 'proposed',
	lanes: [
		{ lane: 'API', brief: 'Codes endpoint', owns: ['src/api'], status: 'pending' },
		{ lane: 'UI', brief: 'Login form', owns: ['web'], status: 'pending' }
	],
	budget_usd: 5,
	start_request_id: 'vibedev-crew-crew_1-start',
	note: 'Waiting for you to start it',
	created_at_ms: 2,
	updated_at_ms: 2
};

let posts: { url: string; body: unknown }[] = [];

beforeEach(() => {
	posts = [];
	vi.stubGlobal(
		'fetch',
		vi.fn(async (request: string | URL | Request, init?: RequestInit) => {
			const url = String(request);
			if (init?.method === 'POST') {
				posts.push({ url, body: init.body ? JSON.parse(String(init.body)) : undefined });
				const running = {
					...proposed,
					status: 'running',
					lanes: proposed.lanes.map((lane, index) => ({ ...lane, status: 'running', run_task_id: `task_${index}` }))
				};
				return new Response(JSON.stringify({ crew: url.endsWith('/crews') || url.includes('/crews?') ? proposed : running }), {
					status: url.includes('/start') ? 200 : 201,
					headers: { 'Content-Type': 'application/json' }
				});
			}
			return new Response(JSON.stringify({ crews: [proposed] }), {
				status: 200,
				headers: { 'Content-Type': 'application/json' }
			});
		})
	);
});

afterEach(() => {
	cleanup();
	vibeDevCrewStore.load(null);
	vi.unstubAllGlobals();
});

describe('CrewSection', () => {
	it('renders a crew whose lanes repeat a name', async () => {
		const twice = {
			...proposed,
			status: 'conflict',
			lanes: [
				...proposed.lanes,
				{ lane: 'Integrate', brief: 'Merge', owns: [], status: 'failed', role: 'integrate' },
				{ lane: 'Integrate', brief: 'Merge again', owns: [], status: 'running', role: 'integrate' }
			]
		};
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => new Response(JSON.stringify({ crews: [twice] }), { status: 200 }))
		);
		render(CrewSection, { props: { project: trusted } });
		await waitFor(() => expect(screen.getAllByText('Integrate')).toHaveLength(2));
	});

	it('shows a proposed crew and starts it', async () => {
		const user = userEvent.setup();
		render(CrewSection, { props: { project: trusted } });

		expect(await screen.findByText('Add login with email codes')).toBeInTheDocument();
		expect(screen.getByText('Waiting to start')).toBeInTheDocument();
		expect(screen.getByText('API')).toBeInTheDocument();
		expect(screen.getByText('$5.00')).toBeInTheDocument();

		await user.click(screen.getByRole('button', { name: 'Start' }));
		await waitFor(() => expect(posts.at(-1)?.url).toContain('/vibedev/projects/proj/crews/crew_1/start'));
		// The crew and both lanes now read Running.
		await waitFor(() => expect(screen.getAllByText('Running')).toHaveLength(3));
		expect(screen.getByRole('button', { name: 'API' })).toBeInTheDocument();
	});

	it('plans a new crew from a goal and a budget', async () => {
		const user = userEvent.setup();
		render(CrewSection, { props: { project: trusted } });
		await screen.findByText('Add login with email codes');

		await user.click(screen.getByRole('button', { name: 'New crew' }));
		await user.click(screen.getByRole('button', { name: 'Plan lanes' }));
		expect(screen.getByRole('alert')).toHaveTextContent('Say what the crew should build.');

		await user.type(screen.getByLabelText('Crew goal'), 'Ship search');
		await user.type(screen.getByLabelText(/Budget in dollars/), '12.5');
		await user.click(screen.getByRole('button', { name: 'Plan lanes' }));
		await waitFor(() => expect(posts).toHaveLength(1));
		expect(posts[0].url).toContain('/vibedev/projects/proj/crews');
		expect(posts[0].body).toMatchObject({ goal: 'Ship search', budget_usd: 12.5, start: false });
	});

	it('explains that crews need a trusted project', async () => {
		render(CrewSection, { props: { project: { ...trusted, trust: 'sandboxed' } as VibeDevProject } });
		expect(await screen.findByText(/Crews need a trusted project/)).toBeInTheDocument();
		expect(screen.queryByRole('button', { name: 'New crew' })).not.toBeInTheDocument();
	});

	it('offers a pull request for merged lanes and a retry for a conflict', async () => {
		const merged = {
			...proposed,
			crew_id: 'crew_2',
			goal: 'Merged goal',
			status: 'integrated',
			spent_usd: 1.25,
			branch: 'vibedev/shop-crew-abcd1234',
			lanes: proposed.lanes.map((lane) => ({ ...lane, status: 'done' }))
		};
		const conflicted = {
			...merged,
			crew_id: 'crew_3',
			goal: 'Conflicted goal',
			status: 'conflict',
			conflict: { lane: 'UI', files: ['README.md'] }
		};
		vi.stubGlobal(
			'fetch',
			vi.fn(async (request: string | URL | Request, init?: RequestInit) => {
				const url = String(request);
				if (init?.method === 'POST') {
					posts.push({ url, body: undefined });
					const crew = url.includes('/integrate')
						? conflicted
						: { ...merged, pull_request_url: 'https://example.test/pr/1' };
					return new Response(JSON.stringify({ crew }), {
						status: 200,
						headers: { 'Content-Type': 'application/json' }
					});
				}
				return new Response(JSON.stringify({ crews: [merged, conflicted] }), {
					status: 200,
					headers: { 'Content-Type': 'application/json' }
				});
			})
		);
		const user = userEvent.setup();
		render(CrewSection, { props: { project: trusted } });

		expect(await screen.findByText('Merged goal')).toBeInTheDocument();
		expect(screen.getAllByText('$1.25 of $5.00')).toHaveLength(2);
		expect(screen.getAllByText('vibedev/shop-crew-abcd1234')).toHaveLength(2);
		expect(screen.getByText(/Lane UI conflicts in README\.md/)).toBeInTheDocument();

		await user.click(screen.getByRole('button', { name: 'Merge again' }));
		await waitFor(() => expect(posts.at(-1)?.url).toContain('/crews/crew_3/integrate'));
		await user.click(screen.getByRole('button', { name: 'Open pull request' }));
		await waitFor(() => expect(posts.at(-1)?.url).toContain('/crews/crew_2/pull-request'));
		expect(await screen.findByRole('link', { name: 'Pull request' })).toHaveAttribute('href', 'https://example.test/pr/1');
	});

	it('lets the owner keep a race attempt and send a conflict to an integrator', async () => {
		const race = {
			...proposed,
			crew_id: 'crew_race',
			goal: 'Race goal',
			status: 'choosing',
			mode: 'race',
			lanes: [
				{ lane: 'Attempt 1', brief: 'b', status: 'done', run_task_id: 't1', role: 'work' },
				{ lane: 'Attempt 2', brief: 'b', status: 'failed', run_task_id: 't2', role: 'work' }
			]
		};
		const conflicted = {
			...proposed,
			crew_id: 'crew_split',
			goal: 'Split goal',
			status: 'conflict',
			conflict: { lane: 'UI', files: ['web/app.ts'] },
			lanes: [
				...proposed.lanes.map((lane) => ({ ...lane, status: 'done' })),
				{ lane: 'Integrate', brief: 'b', status: 'failed', run_task_id: 't9', role: 'integrate' }
			]
		};
		vi.stubGlobal(
			'fetch',
			vi.fn(async (request: string | URL | Request, init?: RequestInit) => {
				const url = String(request);
				if (init?.method === 'POST') {
					posts.push({ url, body: init.body ? JSON.parse(String(init.body)) : undefined });
					return new Response(JSON.stringify({ crew: url.includes('/choose') ? race : conflicted }), {
						status: 200,
						headers: { 'Content-Type': 'application/json' }
					});
				}
				return new Response(JSON.stringify({ crews: [race, conflicted] }), {
					status: 200,
					headers: { 'Content-Type': 'application/json' }
				});
			})
		);
		const user = userEvent.setup();
		render(CrewSection, { props: { project: trusted } });

		expect(await screen.findByText('Pick an attempt')).toBeInTheDocument();
		expect(screen.queryByRole('button', { name: 'Keep Attempt 2' })).not.toBeInTheDocument();
		await user.click(screen.getByRole('button', { name: 'Keep Attempt 1' }));
		await waitFor(() => expect(posts.at(-1)?.url).toContain('/crews/crew_race/choose'));
		expect(posts.at(-1)?.body).toEqual({ lane_index: 0 });

		expect(screen.getByText('Integrator')).toBeInTheDocument();
		await user.click(screen.getByRole('button', { name: 'Resolve with an agent' }));
		await waitFor(() => expect(posts.at(-1)?.url).toContain('/crews/crew_split/resolve'));
	});

	it('sets up a race with a review', async () => {
		const user = userEvent.setup();
		render(CrewSection, { props: { project: trusted } });
		await screen.findByText('Add login with email codes');
		await user.click(screen.getByRole('button', { name: 'New crew' }));
		await user.type(screen.getByLabelText('Crew goal'), 'Ship search');
		await user.click(screen.getByLabelText(/Race engines/));
		await user.clear(screen.getByLabelText('Number of attempts'));
		await user.type(screen.getByLabelText('Number of attempts'), '2');
		await user.click(screen.getByLabelText(/Review the merged branch/));
		await user.click(screen.getByRole('button', { name: 'Set up race' }));
		await waitFor(() => expect(posts).toHaveLength(1));
		expect(posts[0].body).toMatchObject({ goal: 'Ship search', mode: 'race', racers: 2, review: true });
	});
});
