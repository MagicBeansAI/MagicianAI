import { get } from 'svelte/store';
import { afterEach, describe, expect, it, vi } from 'vitest';
import {
	ExecutionControlApiError,
	applyExecutionControl,
	beginExecutionControl,
	coordinateExecutionControl,
	endExecutionControl,
	executionControlBusy,
	executionControlInvalidations,
	executionControlTimeoutMs,
	getExecutionControlState,
	requireExecutionControlResponse,
	steerMessageByteLength
} from './controlClient';

afterEach(() => {
	endExecutionControl('exec/1');
	vi.unstubAllGlobals();
});

describe('execution control client', () => {
	it('loads authoritative control state and encodes execution ids', async () => {
		const fetchMock = vi.fn().mockResolvedValue(
			new Response(
				JSON.stringify({
					execution_id: 'exec/1',
					waiting_state: 'executing',
					active: true,
					can_pause: true,
					can_resume: false,
					can_steer: true,
					can_cancel: true
				}),
				{ status: 200, headers: { 'Content-Type': 'application/json' } }
			)
		);
		vi.stubGlobal('fetch', fetchMock);

		const state = await getExecutionControlState('exec/1');

		expect(state.can_steer).toBe(true);
		expect(String(fetchMock.mock.calls[0]?.[0])).toContain('/executions/exec%2F1/control-state');
	});

	it('posts a bounded steer message through the generic control plane', async () => {
		const fetchMock = vi.fn().mockResolvedValue(new Response('{}', { status: 200 }));
		vi.stubGlobal('fetch', fetchMock);

		await applyExecutionControl('exec/1', 'steer', 'Prioritize the failing check.');

		const init = fetchMock.mock.calls[0]?.[1] as RequestInit;
		expect(init.method).toBe('POST');
		expect(JSON.parse(String(init.body))).toEqual({ message: 'Prioritize the failing check.' });
	});

	it('gives cooperative pause longer than the backend settlement bound', () => {
		expect(executionControlTimeoutMs('pause')).toBe(45_000);
		expect(executionControlTimeoutMs('resume')).toBe(15_000);
		expect(executionControlTimeoutMs('steer')).toBe(15_000);
		expect(executionControlTimeoutMs('cancel')).toBe(15_000);
	});

	it.each(['pause', 'resume', 'cancel'] as const)(
		'posts %s without fabricating a request body',
		async (action) => {
			const fetchMock = vi.fn().mockResolvedValue(new Response('{}', { status: 200 }));
			vi.stubGlobal('fetch', fetchMock);

			await applyExecutionControl('exec/1', action);

			expect(String(fetchMock.mock.calls[0]?.[0])).toContain(`/executions/exec%2F1/${action}`);
			const init = fetchMock.mock.calls[0]?.[1] as RequestInit;
			expect(init.method).toBe('POST');
			expect(init.body).toBeUndefined();
		}
	);

	it('preserves structured backend errors', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn().mockResolvedValue(
				new Response(
					JSON.stringify({ code: 'resource_conflict', error: 'Steer queue is full.' }),
					{ status: 409, headers: { 'Content-Type': 'application/json' } }
				)
			)
		);

		await expect(applyExecutionControl('exec/1', 'steer', 'Try again.')).rejects.toEqual(
			expect.objectContaining<Partial<ExecutionControlApiError>>({
				message: 'Steer queue is full.',
				status: 409,
				code: 'resource_conflict'
			})
		);
	});

	it('locks duplicate controls by execution and counts UTF-8 bytes', () => {
		expect(beginExecutionControl('exec/1')).toBe(true);
		expect(beginExecutionControl('exec/1')).toBe(false);
		expect(get(executionControlBusy).has('exec/1')).toBe(true);
		expect(steerMessageByteLength('a')).toBe(1);
		expect(steerMessageByteLength('₹')).toBe(3);
		expect(steerMessageByteLength('₹'.repeat(1_366))).toBe(4_098);
		endExecutionControl('exec/1');
		expect(beginExecutionControl('exec/1')).toBe(true);
	});

	it('coordinates every mutation path and invalidates controls after success or failure', async () => {
		let releaseFirst!: () => void;
		const first = coordinateExecutionControl(
			'exec/1',
			() => new Promise<void>((resolve) => { releaseFirst = resolve; })
		);
		expect(get(executionControlBusy).has('exec/1')).toBe(true);

		await expect(coordinateExecutionControl('exec/1', async () => undefined)).rejects.toEqual(
			expect.objectContaining({ status: 409, code: 'execution_control_busy' })
		);
		releaseFirst();
		await first;
		expect(get(executionControlBusy).has('exec/1')).toBe(false);
		const afterSuccess = get(executionControlInvalidations).get('exec/1') ?? 0;
		expect(afterSuccess).toBeGreaterThan(0);

		await expect(
			coordinateExecutionControl('exec/1', async () => { throw new Error('failed mutation'); })
		).rejects.toThrow('failed mutation');
		expect(get(executionControlInvalidations).get('exec/1')).toBe(afterSuccess + 1);
		expect(get(executionControlBusy).has('exec/1')).toBe(false);
	});

	it('uses an actionable fallback for a non-JSON backend failure', async () => {
		vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('gateway failed', { status: 502 })));

		await expect(getExecutionControlState('exec/1')).rejects.toEqual(
			expect.objectContaining<Partial<ExecutionControlApiError>>({
				message: 'Could not load execution controls.',
				status: 502
			})
		);
	});

	it('rejects raw control responses that return a non-success status', async () => {
		await expect(
			requireExecutionControlResponse(
				new Response(JSON.stringify({ error: 'Cancellation was rejected.' }), {
					status: 409,
					headers: { 'Content-Type': 'application/json' }
				}),
				'Could not cancel the run.'
			)
		).rejects.toEqual(
			expect.objectContaining({ message: 'Cancellation was rejected.', status: 409 })
		);
	});
});
