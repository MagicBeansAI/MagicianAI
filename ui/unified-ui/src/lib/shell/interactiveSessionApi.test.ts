import { beforeEach, describe, expect, it, vi } from 'vitest';

const timedFetch = vi.hoisted(() => vi.fn());

vi.mock('$lib/shared/fetch', () => ({ timedFetch }));

import {
	composerSubmissionBytes,
	resizeInteractiveSession,
	writeInteractiveSessionStdin
} from './interactiveSessionApi';

function sentBytes(callIndex: number): string {
	const init = timedFetch.mock.calls[callIndex][1] as RequestInit;
	const { bytes_b64 } = JSON.parse(String(init.body)) as { bytes_b64: string };
	return new TextDecoder().decode(Uint8Array.from(atob(bytes_b64), (c) => c.charCodeAt(0)));
}

function deferred(): { promise: Promise<Response>; resolve: (response: Response) => void } {
	let resolve!: (response: Response) => void;
	const promise = new Promise<Response>((res) => {
		resolve = res;
	});
	return { promise, resolve };
}

describe('interactive session stdin', () => {
	beforeEach(() => timedFetch.mockReset());

	it('keeps one request in flight and sends what was typed meanwhile as the next, in order', async () => {
		const first = deferred();
		const second = deferred();
		timedFetch.mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);

		const a = writeInteractiveSessionStdin('s1', 'a');
		const b = writeInteractiveSessionStdin('s1', 'b');
		const c = writeInteractiveSessionStdin('s1', 'c');
		expect(timedFetch).toHaveBeenCalledTimes(1);
		expect(sentBytes(0)).toBe('a');

		const firstResponse = new Response(null, { status: 200 });
		first.resolve(firstResponse);
		await expect(a).resolves.toBe(firstResponse);
		await vi.waitFor(() => expect(timedFetch).toHaveBeenCalledTimes(2));
		// Coalesced, and never reordered ahead of the earlier request.
		expect(sentBytes(1)).toBe('bc');

		const secondResponse = new Response(null, { status: 200 });
		second.resolve(secondResponse);
		await expect(b).resolves.toBe(secondResponse);
		await expect(c).resolves.toBe(secondResponse);
		expect(timedFetch).toHaveBeenCalledTimes(2);
	});

	it('queues each session independently', async () => {
		const left = deferred();
		const right = deferred();
		timedFetch.mockReturnValueOnce(left.promise).mockReturnValueOnce(right.promise);
		const x = writeInteractiveSessionStdin('left', 'x');
		const y = writeInteractiveSessionStdin('right', 'y');
		// Neither waits on the other's in-flight request.
		expect(timedFetch).toHaveBeenCalledTimes(2);
		expect(String(timedFetch.mock.calls[0][0])).toContain('/interactive-sessions/left/stdin');
		expect(String(timedFetch.mock.calls[1][0])).toContain('/interactive-sessions/right/stdin');
		left.resolve(new Response(null, { status: 200 }));
		right.resolve(new Response(null, { status: 200 }));
		await Promise.all([x, y]);
	});

	it('rejects the batch whose request failed and keeps serving later input', async () => {
		timedFetch
			.mockRejectedValueOnce(new Error('offline'))
			.mockResolvedValueOnce(new Response(null, { status: 200 }));
		await expect(writeInteractiveSessionStdin('s2', 'lost')).rejects.toThrow('offline');
		await expect(writeInteractiveSessionStdin('s2', 'next')).resolves.toHaveProperty('ok', true);
		expect(sentBytes(1)).toBe('next');
	});
});

describe('composer submission', () => {
	it('submits a single line with a carriage return', () => {
		expect(composerSubmissionBytes('run the tests')).toBe('run the tests\r');
	});

	it('wraps multi-line text as a bracketed paste so inner newlines are not Enter', () => {
		expect(composerSubmissionBytes('one\ntwo')).toBe('\x1b[200~one\ntwo\x1b[201~\r');
	});
});

describe('resize', () => {
	beforeEach(() => timedFetch.mockReset());

	it('posts the grid to the session resize endpoint', async () => {
		timedFetch.mockResolvedValue(new Response(null, { status: 200 }));
		await resizeInteractiveSession('s3', 33, 101);
		const [url, init] = timedFetch.mock.calls[0] as [string, RequestInit];
		expect(url).toBe('/api/magician/v2/interactive-sessions/s3/resize');
		expect(init.method).toBe('POST');
		expect(JSON.parse(String(init.body))).toEqual({ rows: 33, cols: 101 });
	});
});
