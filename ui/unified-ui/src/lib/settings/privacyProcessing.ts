/**
 * Processing locality (`privacy.processing.mode`): whether local-eligible
 * operations run on the on-device model (`local`) or on their cloud
 * counterparts (`cloud`). `GET /settings/privacy` reports the effective mode
 * and the routing it produces; `PUT` writes it and reloads it live.
 */
import { scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';

export type ProcessingMode = 'local' | 'cloud';

export interface ProcessingRouting {
	operation: string;
	profile: string;
	provider: string;
	/** True when the cloud counterpart serves the operation right now. */
	switched: boolean;
}

export interface ProcessingSettings {
	mode: ProcessingMode;
	routing: ProcessingRouting[];
	warnings: string[];
}

const ENDPOINT = '/api/magician/v2/settings/privacy';

export function parseProcessingSettings(body: unknown): ProcessingSettings {
	const value = (body ?? {}) as Record<string, unknown>;
	const routing = Array.isArray(value.routing)
		? value.routing.flatMap((entry): ProcessingRouting[] => {
				const row = (entry ?? {}) as Record<string, unknown>;
				if (typeof row.operation !== 'string') return [];
				return [
					{
						operation: row.operation,
						profile: typeof row.profile === 'string' ? row.profile : '',
						provider: typeof row.provider === 'string' ? row.provider : '',
						switched: row.switched === true
					}
				];
			})
		: [];
	return {
		mode: value.mode === 'cloud' ? 'cloud' : 'local',
		routing,
		warnings: Array.isArray(value.warnings)
			? value.warnings.filter((warning): warning is string => typeof warning === 'string')
			: []
	};
}

function errorMessage(body: unknown, status: number): string {
	const value = (body ?? {}) as Record<string, unknown>;
	if (typeof value.message === 'string' && value.message.trim()) return value.message;
	if (typeof value.error === 'string' && value.error.trim()) return value.error;
	return `Processing settings failed (${status})`;
}

export async function fetchProcessingSettings(): Promise<ProcessingSettings> {
	const response = await fetch(ENDPOINT, {
		headers: scopedRequestHeaders({ Accept: 'application/json' }),
		cache: 'no-store'
	});
	const body = await response.json().catch(() => null);
	if (!response.ok) throw new Error(errorMessage(body, response.status));
	return parseProcessingSettings(body);
}

export async function setProcessingMode(mode: ProcessingMode): Promise<ProcessingSettings> {
	const response = await fetch(ENDPOINT, {
		method: 'PUT',
		headers: scopedRequestHeaders({ 'Content-Type': 'application/json', Accept: 'application/json' }),
		body: JSON.stringify({ processing: { mode } })
	});
	const body = await response.json().catch(() => null);
	if (!response.ok) throw new Error(errorMessage(body, response.status));
	return parseProcessingSettings(body);
}
