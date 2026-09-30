import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { requestConfirmation } from '$lib/stores/confirmationStore';
import ExecutionControls from './ExecutionControls.svelte';
import ExecutionControlsTestHost from './ExecutionControls.test-host.svelte';

const { showErrorMock, showSuccessMock } = vi.hoisted(() => ({
	showErrorMock: vi.fn(),
	showSuccessMock: vi.fn()
}));

vi.mock('$lib/stores/confirmationStore', () => ({ requestConfirmation: vi.fn() }));
vi.mock('$lib/shared/stores/notifications', () => ({
	showError: showErrorMock,
	showSuccess: showSuccessMock
}));

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	vi.unstubAllGlobals();
});

function controlState(overrides: Record<string, unknown> = {}): Record<string, unknown> {
	return {
		execution_id: 'exec-1',
		waiting_state: 'executing',
		active: true,
		can_pause: true,
		can_resume: false,
		can_steer: true,
		can_cancel: true,
		...overrides
	};
}

describe('ExecutionControls', () => {
	it('opens the shared steer composer and posts guidance', async () => {
		const fetchMock = vi.fn().mockImplementation(async (input: RequestInfo | URL) => {
			const url = String(input);
			if (url.endsWith('/steer')) return new Response('{}', { status: 200 });
			return new Response(JSON.stringify(controlState()), {
				status: 200,
				headers: { 'Content-Type': 'application/json' }
			});
		});
		vi.stubGlobal('fetch', fetchMock);
		const user = userEvent.setup();

		render(ExecutionControls, { executionId: 'exec-1', label: 'Research task' });
		await user.click(await screen.findByRole('button', { name: 'Steer run' }));
		await user.type(
			screen.getByRole('textbox', { name: 'Guidance for the next decision turn' }),
			'Use the existing implementation and only fix the failing branch.'
		);
		await user.click(screen.getByRole('button', { name: 'Send steer' }));

		await waitFor(() => {
			expect(fetchMock.mock.calls.some(([input]) => String(input).endsWith('/steer'))).toBe(true);
		});
		const steerCall = fetchMock.mock.calls.find(([input]) => String(input).endsWith('/steer'));
		expect(JSON.parse(String((steerCall?.[1] as RequestInit).body))).toEqual({
			message: 'Use the existing implementation and only fix the failing branch.'
		});
	});

	it('shows Resume without dead Pause or Steer actions for a manual pause', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn().mockResolvedValue(
				new Response(
					JSON.stringify(
						controlState({
							waiting_state: 'paused',
							active: false,
							can_pause: false,
							can_resume: true,
							can_steer: false
						})
					),
					{ status: 200, headers: { 'Content-Type': 'application/json' } }
				)
			)
		);

		render(ExecutionControls, { executionId: 'exec-1' });

		expect(await screen.findByRole('button', { name: 'Resume run' })).toBeEnabled();
		expect(screen.queryByRole('button', { name: 'Pause run' })).not.toBeInTheDocument();
		expect(screen.queryByRole('button', { name: 'Steer run' })).not.toBeInTheDocument();
	});

	it.each([
		{ label: 'Pause run', action: 'pause', overrides: {} },
		{
			label: 'Resume run',
			action: 'resume',
			overrides: { can_pause: false, can_resume: true, can_steer: false }
		}
	])('submits $action and emits the canonical changed event', async ({ label, action, overrides }) => {
		const fetchMock = vi.fn().mockImplementation(async (input: RequestInfo | URL) => {
			if (String(input).endsWith(`/${action}`)) return new Response('{}', { status: 200 });
			return new Response(JSON.stringify(controlState(overrides)), {
				status: 200,
				headers: { 'Content-Type': 'application/json' }
			});
		});
		vi.stubGlobal('fetch', fetchMock);
		const user = userEvent.setup();
		render(ExecutionControlsTestHost, { executionId: 'exec-1' });

		await user.click(await screen.findByRole('button', { name: label }));

		await waitFor(() => {
			expect(fetchMock.mock.calls.some(([input]) => String(input).endsWith(`/${action}`))).toBe(true);
			expect(screen.getByTestId('changed-action')).toHaveTextContent(action);
		});
	});

	it('requires destructive confirmation before cancelling', async () => {
		vi.mocked(requestConfirmation).mockResolvedValue(true);
		const fetchMock = vi.fn().mockImplementation(async (input: RequestInfo | URL) => {
			if (String(input).endsWith('/cancel')) return new Response('{}', { status: 200 });
			return new Response(JSON.stringify(controlState()), {
				status: 200,
				headers: { 'Content-Type': 'application/json' }
			});
		});
		vi.stubGlobal('fetch', fetchMock);
		const user = userEvent.setup();

		render(ExecutionControls, { executionId: 'exec-1', label: 'Research task' });
		await user.click(await screen.findByRole('button', { name: 'Stop run' }));

		expect(requestConfirmation).toHaveBeenCalledWith(
			expect.objectContaining({ title: 'Stop this run?', destructive: true })
		);
		await waitFor(() => {
			expect(fetchMock.mock.calls.some(([input]) => String(input).endsWith('/cancel'))).toBe(true);
		});
	});

	it('does not cancel a replacement execution confirmed from a stale Stop dialog', async () => {
		let resolveConfirmation!: (confirmed: boolean) => void;
		vi.mocked(requestConfirmation).mockReturnValue(
			new Promise<boolean>((resolve) => {
				resolveConfirmation = resolve;
			})
		);
		const fetchMock = vi.fn().mockImplementation(async (input: RequestInfo | URL) => {
			const url = String(input);
			const executionId = url.includes('exec-2') ? 'exec-2' : 'exec-1';
			if (url.endsWith('/cancel')) return new Response('{}', { status: 200 });
			return new Response(JSON.stringify(controlState({ execution_id: executionId })), {
				status: 200,
				headers: { 'Content-Type': 'application/json' }
			});
		});
		vi.stubGlobal('fetch', fetchMock);
		const user = userEvent.setup();
		const view = render(ExecutionControls, { executionId: 'exec-1', label: 'Original run' });

		await user.click(await screen.findByRole('button', { name: 'Stop run' }));
		await view.rerender({ executionId: 'exec-2', label: 'Replacement run' });
		resolveConfirmation(true);

		await waitFor(() => {
			expect(showErrorMock).toHaveBeenCalledWith(
				expect.stringMatching(/active run changed while confirmation was open/i)
			);
		});
		expect(fetchMock.mock.calls.some(([input]) => String(input).endsWith('/cancel'))).toBe(false);
	});

	it('retries a failed capability load and recovers the controls', async () => {
		const fetchMock = vi
			.fn()
			.mockResolvedValueOnce(new Response(JSON.stringify({ error: 'temporary failure' }), { status: 503 }))
			.mockResolvedValue(
				new Response(JSON.stringify(controlState()), {
					status: 200,
					headers: { 'Content-Type': 'application/json' }
				})
			);
		vi.stubGlobal('fetch', fetchMock);
		const user = userEvent.setup();

		render(ExecutionControls, { executionId: 'exec-1' });
		await user.click(await screen.findByRole('button', { name: 'Retry controls' }));

		expect(await screen.findByRole('button', { name: 'Pause run' })).toBeEnabled();
		expect(fetchMock).toHaveBeenCalledTimes(2);
	});

	it('enforces the UTF-8 byte limit in the steer composer', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn().mockResolvedValue(
				new Response(JSON.stringify(controlState()), {
					status: 200,
					headers: { 'Content-Type': 'application/json' }
				})
			)
		);
		const user = userEvent.setup();
		render(ExecutionControls, { executionId: 'exec-1' });
		await user.click(await screen.findByRole('button', { name: 'Steer run' }));

		await fireEvent.input(screen.getByRole('textbox'), { target: { value: '₹'.repeat(1_366) } });

		expect(screen.getByRole('button', { name: 'Send steer' })).toBeDisabled();
		expect(screen.getByText(/4,098 \/ 4,096 bytes/)).toBeInTheDocument();
	});

	it('refreshes every mounted control surface after one surface mutates the run', async () => {
		let paused = false;
		const fetchMock = vi.fn().mockImplementation(async (input: RequestInfo | URL) => {
			const url = String(input);
			if (url.endsWith('/pause')) {
				paused = true;
				return new Response('{}', { status: 200 });
			}
			return new Response(JSON.stringify(controlState({
				waiting_state: paused ? 'paused' : 'executing',
				active: !paused,
				can_pause: !paused,
				can_resume: paused,
				can_steer: !paused
			})), {
				status: 200,
				headers: { 'Content-Type': 'application/json' }
			});
		});
		vi.stubGlobal('fetch', fetchMock);
		const user = userEvent.setup();

		render(ExecutionControls, { executionId: 'exec-1', label: 'First' });
		render(ExecutionControls, { executionId: 'exec-1', label: 'Second' });
		const pauseButtons = await screen.findAllByRole('button', { name: 'Pause run' });
		expect(pauseButtons).toHaveLength(2);
		await user.click(pauseButtons[0]);

		await waitFor(() => {
			expect(screen.getAllByRole('button', { name: 'Resume run' })).toHaveLength(2);
		});
		expect(screen.queryByRole('button', { name: 'Pause run' })).not.toBeInTheDocument();
	});
});
