import { describe, expect, it, vi } from 'vitest';
import {
	LANDING_BACKEND_HEALTH_URL,
	isMagicianHealthPayload,
	probeLandingBackend
} from './backendCapability';

describe('landing backend capability', () => {
	it('recognizes the Magician health identity', () => {
		expect(
			isMagicianHealthPayload({ service: 'magician', status: 'ok', magician: 'healthy' })
		).toBe(true);
		expect(isMagicianHealthPayload({ service: 'magician', magician: 'healthy' })).toBe(true);
	});

	it('rejects unrelated or malformed JSON', () => {
		expect(isMagicianHealthPayload({ status: 'ok' })).toBe(false);
		expect(isMagicianHealthPayload({ service: 'other', status: 'ok' })).toBe(false);
		expect(isMagicianHealthPayload(null)).toBe(false);
	});

	it('uses the purpose-built health route and accepts its JSON response', async () => {
		const fetcher = vi.fn<typeof fetch>().mockResolvedValue(
			new Response(JSON.stringify({ service: 'magician', status: 'ok' }), {
				status: 200,
				headers: { 'Content-Type': 'application/json' }
			})
		);

		await expect(probeLandingBackend(fetcher, new AbortController().signal)).resolves.toBe(true);
		expect(fetcher).toHaveBeenCalledWith(
			LANDING_BACKEND_HEALTH_URL,
			expect.objectContaining({ method: 'GET', signal: expect.any(AbortSignal) })
		);
	});

	it('fails closed for an HTTP-200 marketing HTML fallback', async () => {
		const fetcher = vi.fn<typeof fetch>().mockResolvedValue(
			new Response('<!doctype html><title>Magican</title>', {
				status: 200,
				headers: { 'Content-Type': 'text/html' }
			})
		);

		await expect(probeLandingBackend(fetcher, new AbortController().signal)).resolves.toBe(false);
	});

	it('fails closed when the request rejects or returns a non-success status', async () => {
		const rejected = vi.fn<typeof fetch>().mockRejectedValue(new Error('offline'));
		const unavailable = vi
			.fn<typeof fetch>()
			.mockResolvedValue(new Response('{}', { status: 503, headers: { 'Content-Type': 'application/json' } }));

		await expect(probeLandingBackend(rejected, new AbortController().signal)).resolves.toBe(false);
		await expect(probeLandingBackend(unavailable, new AbortController().signal)).resolves.toBe(false);
	});
});
