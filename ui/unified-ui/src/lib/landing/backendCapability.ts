export const LANDING_BACKEND_HEALTH_URL = '/health';
export const LANDING_BACKEND_PROBE_TIMEOUT_MS = 5_000;

type MagicianHealthPayload = {
	service?: unknown;
	status?: unknown;
	magician?: unknown;
};

export function isMagicianHealthPayload(value: unknown): boolean {
	if (!value || typeof value !== 'object' || Array.isArray(value)) return false;
	const payload = value as MagicianHealthPayload;
	return (
		payload.service === 'magician' &&
		(payload.status === 'ok' || payload.magician === 'healthy')
	);
}

/**
 * Probe the cheap, purpose-built service health contract. A successful HTML
 * fallback or unrelated JSON document is deliberately rejected so a static
 * marketing deployment cannot accidentally expose product-only surfaces.
 */
export async function probeLandingBackend(
	fetcher: typeof fetch,
	signal: AbortSignal
): Promise<boolean> {
	try {
		const response = await fetcher(LANDING_BACKEND_HEALTH_URL, {
			method: 'GET',
			headers: { Accept: 'application/json' },
			signal
		});
		if (!response.ok) return false;
		const contentType = response.headers.get('content-type') || '';
		if (!contentType.toLowerCase().includes('application/json')) return false;
		return isMagicianHealthPayload(await response.json());
	} catch {
		return false;
	}
}
