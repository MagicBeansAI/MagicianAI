import { cleanup, render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import LiveToolInspector from './LiveToolInspector.svelte';

const { eventHarness, showErrorMock } = vi.hoisted(() => {
	let subscriber: ((events: unknown[]) => void) | null = null;
	return {
		eventHarness: {
			subscribe: vi.fn((next: (events: unknown[]) => void) => {
				subscriber = next;
				next([]);
				return () => {
					subscriber = null;
				};
			}),
			emit(events: unknown[]) {
				subscriber?.(events);
			}
		},
		showErrorMock: vi.fn()
	};
});

vi.mock('$lib/realtime/v2-websocket', () => ({ v2Events: eventHarness }));
vi.mock('$lib/shared/stores/notifications', () => ({ showError: showErrorMock }));

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	vi.unstubAllGlobals();
});

describe('LiveToolInspector cancellation', () => {
	it('keeps the tool visible and reports a non-2xx cancellation response', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn().mockResolvedValue(
				new Response(JSON.stringify({ error: 'Execution is already settling.' }), {
					status: 409,
					headers: { 'Content-Type': 'application/json' }
				})
			)
		);
		const user = userEvent.setup();
		render(LiveToolInspector);
		await waitFor(() => expect(eventHarness.subscribe).toHaveBeenCalled());

		eventHarness.emit([
			{
				event_type: 'AgenticStepStarted',
				data: {
					execution_id: 'exec-live',
					step_id: 'step-1',
					tool_name: 'shell',
					started_at_ms: Date.now()
				}
			}
		]);
		await user.click(await screen.findByTitle('Cancel this execution'));

		await waitFor(() => {
			expect(showErrorMock).toHaveBeenCalledWith('Execution is already settling.');
		});
		expect(screen.getByText('shell')).toBeInTheDocument();
	});
});
