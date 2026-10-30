import { act, cleanup, render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { vibeDevLiveRunsStore } from '$lib/stores/vibeDevLiveRunsStore';
import LiveRunsStrip from './LiveRunsStrip.svelte';

const rows = [
	{
		run_id: 'run_a',
		live_task_id: 'turn_a2',
		project_id: 'proj',
		project_name: 'Shop',
		title: 'Add checkout',
		state: 'needs_you',
		status: 'running',
		turns: 2,
		updated_at: '2026-10-12T11:59:00Z'
	},
	{
		run_id: 'run_b',
		live_task_id: 'run_b',
		project_id: 'proj',
		project_name: 'Shop',
		title: 'Fix login',
		state: 'working',
		status: 'running',
		step: 'Editing auth.ts',
		crew_id: 'crew-1',
		lane: 'Auth',
		turns: 1,
		updated_at: '2026-10-12T11:58:00Z'
	},
	{
		run_id: 'run_c',
		live_task_id: 'run_c',
		project_id: 'other',
		project_name: 'Docs',
		title: 'Rewrite intro',
		state: 'done',
		status: 'completed',
		summary: 'Intro rewritten',
		turns: 1,
		updated_at: '2026-10-12T11:50:00Z',
		// An Autopilot run's owner briefing: a notice, not a review.
		waiting_kind: 'notice',
		notice: {
			request_id: 'vibedev-run-ended:run_c:exec_1',
			option_id: 'acknowledge',
			title: 'Autopilot run finished: Rewrite intro',
			message: 'Autopilot run finished: Rewrite intro. Open it to review the result.',
			severity: 'info'
		}
	}
];

let controls: { url: string; body: Record<string, unknown> }[] = [];
let responds: { url: string; body: Record<string, unknown> }[] = [];

beforeEach(() => {
	controls = [];
	responds = [];
	vi.stubGlobal(
		'fetch',
		vi.fn(async (request: string | URL | Request, init?: RequestInit) => {
			const url = String(request);
			if (url.includes('/vibedev/runs/live')) {
				return new Response(JSON.stringify({ runs: rows, counts: {} }), {
					status: 200,
					headers: { 'Content-Type': 'application/json' }
				});
			}
			if (url.includes('/hitl/') && url.endsWith('/respond')) {
				responds.push({ url, body: JSON.parse(String(init?.body ?? '{}')) });
				return new Response(JSON.stringify({ accepted: true }), {
					status: 200,
					headers: { 'Content-Type': 'application/json' }
				});
			}
			if (url.includes('/control')) {
				controls.push({ url, body: JSON.parse(String(init?.body ?? '{}')) });
				return new Response(JSON.stringify({ delivered: true }), {
					status: 200,
					headers: { 'Content-Type': 'application/json' }
				});
			}
			return new Response('{}', { status: 404 });
		})
	);
});

afterEach(() => {
	cleanup();
	vi.useRealTimers();
	vi.unstubAllGlobals();
});

describe('LiveRunsStrip', () => {
	it('lists runs by state and opens one in VibeDev', async () => {
		const openPath = vi.fn();
		const user = userEvent.setup();
		render(LiveRunsStrip, { openPath });

		expect(await screen.findByText('Add checkout')).toBeInTheDocument();
		expect(screen.getByText('2 active')).toBeInTheDocument();
		expect(screen.getByText(/Lane Auth · Editing auth\.ts/)).toBeInTheDocument();
		expect(screen.getByText(/Intro rewritten/)).toBeInTheDocument();

		await user.click(screen.getByRole('button', { name: 'Review' }));
		expect(openPath).toHaveBeenCalledWith('/vibe?project=proj&task=run_a');
	});

	it('shows a finished run\'s briefing as a notice to acknowledge, not a review', async () => {
		const user = userEvent.setup();
		render(LiveRunsStrip, { openPath: vi.fn() });

		expect(
			await screen.findByText('Autopilot run finished: Rewrite intro. Open it to review the result.')
		).toBeInTheDocument();
		// Only the needs-you run is reviewed.
		expect(screen.getAllByRole('button', { name: 'Review' })).toHaveLength(1);

		await user.click(screen.getByRole('button', { name: 'Acknowledge' }));
		await waitFor(() => expect(responds).toHaveLength(1));
		expect(responds[0].url).toContain('/api/magician/v2/hitl/vibedev-run-ended%3Arun_c%3Aexec_1/respond');
		expect(responds[0].body).toEqual({
			source: 'user_request',
			input_type: 'choice',
			value: { type: 'choice', selected_id: 'acknowledge' },
			channel: 'web'
		});
	});

	it('steers the live turn, not the root', async () => {
		const user = userEvent.setup();
		render(LiveRunsStrip, { openPath: vi.fn() });
		await screen.findByText('Add checkout');

		await user.click(screen.getAllByRole('button', { name: 'Steer' })[0]);
		await user.type(screen.getByLabelText('Steer Add checkout'), 'use the new API{Enter}');

		await waitFor(() => expect(controls).toHaveLength(1));
		expect(controls[0].url).toContain('/vibedev/runs/turn_a2/control');
		expect(controls[0].body).toEqual({ action: 'steer', message: 'use the new API' });
		expect(await screen.findByText('Sent to the run.')).toBeInTheDocument();
	});

	it('waits before stopping so the stop can be taken back', async () => {
		vi.useFakeTimers({ shouldAdvanceTime: true });
		const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
		render(LiveRunsStrip, { openPath: vi.fn() });
		await screen.findByText('Fix login');

		const stops = screen.getAllByRole('button', { name: 'Stop' });
		await user.click(stops[1]);
		await user.click(screen.getByRole('button', { name: 'Undo stop' }));
		await act(() => vi.advanceTimersByTime(2_500));
		expect(controls).toHaveLength(0);

		await user.click(screen.getAllByRole('button', { name: 'Stop' })[1]);
		await act(() => vi.advanceTimersByTime(2_500));
		await waitFor(() => expect(controls).toHaveLength(1));
		expect(controls[0].url).toContain('/vibedev/runs/run_b/control');
		expect(controls[0].body.action).toBe('stop');
	});

	it('still stops a run when the strip closes inside the undo window', async () => {
		const user = userEvent.setup();
		const { unmount } = render(LiveRunsStrip, { openPath: vi.fn() });
		await screen.findByText('Add checkout');

		await user.click(screen.getAllByRole('button', { name: 'Stop' })[0]);
		expect(controls).toHaveLength(0);
		unmount();

		await waitFor(() => expect(controls).toHaveLength(1));
		expect(controls[0].url).toContain('/vibedev/runs/turn_a2/control');
		expect(controls[0].body.action).toBe('stop');
	});

	it('renders nothing while no run is listed', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => new Response(JSON.stringify({ runs: [] }), { status: 200 }))
		);
		await vibeDevLiveRunsStore.refresh();
		render(LiveRunsStrip, { openPath: vi.fn() });
		await waitFor(() => expect(screen.queryByLabelText('Coding runs')).not.toBeInTheDocument());
	});
});
