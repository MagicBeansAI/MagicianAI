import { derived, writable } from 'svelte/store';
import { timedFetch } from '$lib/shared/fetch';

export interface MagicianConfigReloadResult {
	path: string;
	profile_count: number;
	operation_mapping_count: number;
	live_reloaded: string[];
	restart_required: string[];
	warnings: string[];
}

export interface MagicianConfigStoreState {
	isReloading: boolean;
	error: string | null;
	lastReloadedAt: number | null;
}

interface MagicianConfigMetaState extends MagicianConfigStoreState {}

interface ParsedApiError {
	message: string;
}

const defaultMetaState: MagicianConfigMetaState = {
	isReloading: false,
	error: null,
	lastReloadedAt: null
};

const magicianConfigReloadResultStore = writable<MagicianConfigReloadResult | null>(null);
const magicianConfigMeta = writable<MagicianConfigMetaState>(defaultMetaState);

function asRecord(value: unknown): Record<string, unknown> | null {
	return typeof value === 'object' && value !== null && !Array.isArray(value)
		? (value as Record<string, unknown>)
		: null;
}

function readString(payload: Record<string, unknown>, field: string): string | undefined {
	const value = payload[field];
	return typeof value === 'string' ? value : undefined;
}

function readNumber(payload: Record<string, unknown>, field: string): number | undefined {
	const value = payload[field];
	return typeof value === 'number' && Number.isFinite(value) ? value : undefined;
}

function readStringArray(payload: Record<string, unknown>, field: string): string[] {
	const value = payload[field];
	if (!Array.isArray(value)) return [];
	return value.filter((item): item is string => typeof item === 'string');
}

function parseReloadResult(raw: unknown): MagicianConfigReloadResult | null {
	const root = asRecord(raw);
	if (!root) return null;

	const path = readString(root, 'path');
	const profileCount = readNumber(root, 'profile_count');
	const operationMappingCount = readNumber(root, 'operation_mapping_count');
	if (!path || profileCount === undefined || operationMappingCount === undefined) {
		return null;
	}

	return {
		path,
		profile_count: profileCount,
		operation_mapping_count: operationMappingCount,
		live_reloaded: readStringArray(root, 'live_reloaded'),
		restart_required: readStringArray(root, 'restart_required'),
		warnings: readStringArray(root, 'warnings')
	};
}

async function readApiError(response: Response): Promise<ParsedApiError> {
	let message = `Request failed (${response.status})`;

	try {
		const text = await response.text();
		if (!text) {
			return { message };
		}

		try {
			const parsed = JSON.parse(text) as unknown;
			const root = asRecord(parsed);
			const errorText = root ? readString(root, 'error') : undefined;
			const messageText = root ? readString(root, 'message') : undefined;
			message = `Request failed (${response.status}): ${errorText || messageText || text}`;
		} catch {
			message = `Request failed (${response.status}): ${text}`;
		}
	} catch {
		// Best effort only.
	}

	return { message };
}

function beginReload(): void {
	magicianConfigMeta.update((state) => ({
		...state,
		isReloading: true,
		error: null
	}));
}

function endReload(updates?: Partial<MagicianConfigMetaState>): void {
	magicianConfigMeta.update((state) => ({
		...state,
		isReloading: false,
		...(updates || {})
	}));
}

export const magicianConfigReloadResult = derived(
	magicianConfigReloadResultStore,
	($result) => $result
);

export const magicianConfigStoreState = derived(
	magicianConfigMeta,
	($meta): MagicianConfigStoreState => ({
		isReloading: $meta.isReloading,
		error: $meta.error,
		lastReloadedAt: $meta.lastReloadedAt
	})
);

export function clearMagicianConfigError(): void {
	magicianConfigMeta.update((state) => ({
		...state,
		error: null
	}));
}

export async function reloadMagicianConfig(): Promise<MagicianConfigReloadResult> {
	beginReload();
	try {
		const response = await timedFetch('/api/magician/v2/settings/magician-config/reload', {
			method: 'POST'
		});
		if (!response.ok) {
			const apiError = await readApiError(response);
			endReload({ error: apiError.message });
			throw new Error(apiError.message);
		}

		const payload = (await response.json()) as unknown;
		const result = parseReloadResult(payload);
		if (!result) {
			throw new Error('Malformed magician config reload response');
		}

		magicianConfigReloadResultStore.set(result);
		endReload({
			error: null,
			lastReloadedAt: Date.now()
		});
		return result;
	} catch (error) {
		const message =
			error instanceof Error ? error.message : 'Failed to reload magician config';
		endReload({ error: message });
		throw error;
	}
}
