/**
 * Local-generation kitty pin — Settings switcher for
 * `runtime.ollama.local_generation.selected`.
 *
 * Switching rewrites the YAML anchor and reloads Ollama. RAM-tier
 * mismatches are warnings, not refusals.
 */
import { timedFetch, LONG_FETCH_TIMEOUT_MS } from '$lib/shared/fetch';
import { scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';

export interface LocalGenerationHost {
	memory_gb: number;
	free_memory_gb: number | null;
	arch: string;
	os: string;
}

export interface LocalGenerationModel {
	id: string;
	label: string;
	ollama: string;
	install: string | null;
	min_memory_gb: number;
	resident_gb: number | null;
	disk_gb: number | null;
	classify_agree_pct: number | null;
	distill_recall_pct: number | null;
	browser_effect: string | null;
	browser_protocol: string | null;
	tok_s_channel: number | null;
	notes: string | null;
	selected: boolean;
	recommended: boolean;
	rule_ok: boolean;
	installed: boolean | null;
	warnings: string[];
}

export interface LocalGenerationEnvelope {
	settings_path: string;
	selected: string | null;
	recommended: string | null;
	processing_mode: string;
	host: LocalGenerationHost;
	min_memory_gb: number;
	requires_arch: string | null;
	requires_os: string | null;
	models: LocalGenerationModel[];
	warnings: string[];
	catalog_path: string | null;
	previous?: string | null;
	config_reloaded?: boolean;
	ollama_reloaded?: boolean;
	rule_violated?: boolean;
	reload_error?: string;
	ollama_error?: string;
}

function asRecord(value: unknown): Record<string, unknown> | null {
	return typeof value === 'object' && value !== null && !Array.isArray(value)
		? (value as Record<string, unknown>)
		: null;
}

function readString(payload: Record<string, unknown>, field: string): string | null {
	const value = payload[field];
	return typeof value === 'string' ? value : null;
}

function readNumber(payload: Record<string, unknown>, field: string): number | null {
	const value = payload[field];
	return typeof value === 'number' && Number.isFinite(value) ? value : null;
}

function readBoolean(payload: Record<string, unknown>, field: string): boolean | null {
	const value = payload[field];
	return typeof value === 'boolean' ? value : null;
}

function readStringArray(payload: Record<string, unknown>, field: string): string[] {
	const value = payload[field];
	if (!Array.isArray(value)) return [];
	return value.filter((item): item is string => typeof item === 'string');
}

function parseHost(raw: unknown): LocalGenerationHost {
	const record = asRecord(raw);
	return {
		memory_gb: record ? (readNumber(record, 'memory_gb') ?? 0) : 0,
		free_memory_gb: record ? readNumber(record, 'free_memory_gb') : null,
		arch: record ? (readString(record, 'arch') ?? 'unknown') : 'unknown',
		os: record ? (readString(record, 'os') ?? 'unknown') : 'unknown'
	};
}

function parseModel(raw: unknown): LocalGenerationModel | null {
	const record = asRecord(raw);
	if (!record) return null;
	const id = readString(record, 'id');
	const label = readString(record, 'label');
	if (!id || !label) return null;
	return {
		id,
		label,
		ollama: readString(record, 'ollama') ?? id,
		install: readString(record, 'install'),
		min_memory_gb: readNumber(record, 'min_memory_gb') ?? 0,
		resident_gb: readNumber(record, 'resident_gb'),
		disk_gb: readNumber(record, 'disk_gb'),
		classify_agree_pct: readNumber(record, 'classify_agree_pct'),
		distill_recall_pct: readNumber(record, 'distill_recall_pct'),
		browser_effect: readString(record, 'browser_effect'),
		browser_protocol: readString(record, 'browser_protocol'),
		tok_s_channel: readNumber(record, 'tok_s_channel'),
		notes: readString(record, 'notes'),
		selected: readBoolean(record, 'selected') ?? false,
		recommended: readBoolean(record, 'recommended') ?? false,
		rule_ok: readBoolean(record, 'rule_ok') ?? true,
		installed: readBoolean(record, 'installed'),
		warnings: readStringArray(record, 'warnings')
	};
}

export function parseLocalGenerationEnvelope(raw: unknown): LocalGenerationEnvelope | null {
	const root = asRecord(raw);
	if (!root) return null;
	const settingsPath = readString(root, 'settings_path');
	const host = parseHost(root.host);
	const models = Array.isArray(root.models)
		? root.models.map(parseModel).filter((model): model is LocalGenerationModel => model !== null)
		: [];
	if (!settingsPath) return null;
	return {
		settings_path: settingsPath,
		selected: readString(root, 'selected'),
		recommended: readString(root, 'recommended'),
		processing_mode: readString(root, 'processing_mode') ?? 'local',
		host,
		min_memory_gb: readNumber(root, 'min_memory_gb') ?? 0,
		requires_arch: readString(root, 'requires_arch'),
		requires_os: readString(root, 'requires_os'),
		models,
		warnings: readStringArray(root, 'warnings'),
		catalog_path: readString(root, 'catalog_path'),
		previous: readString(root, 'previous'),
		config_reloaded: readBoolean(root, 'config_reloaded') ?? undefined,
		ollama_reloaded: readBoolean(root, 'ollama_reloaded') ?? undefined,
		rule_violated: readBoolean(root, 'rule_violated') ?? undefined,
		reload_error: readString(root, 'reload_error') ?? undefined,
		ollama_error: readString(root, 'ollama_error') ?? undefined
	};
}

async function readApiError(response: Response): Promise<string> {
	let message = `Request failed (${response.status})`;
	try {
		const text = await response.text();
		if (!text) return message;
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
	return message;
}

export async function fetchLocalGeneration(): Promise<LocalGenerationEnvelope> {
	const response = await timedFetch('/api/magician/v2/settings/local-generation', {
		headers: scopedRequestHeaders({ Accept: 'application/json' }),
		cache: 'no-store'
	});
	if (!response.ok) throw new Error(await readApiError(response));
	const envelope = parseLocalGenerationEnvelope(await response.json());
	if (!envelope) throw new Error('Malformed local generation settings response');
	return envelope;
}

export async function switchLocalGeneration(
	selected: string,
	reloadOllama = true
): Promise<LocalGenerationEnvelope> {
	const response = await timedFetch('/api/magician/v2/settings/local-generation', {
		method: 'PUT',
		headers: scopedRequestHeaders({
			Accept: 'application/json',
			'Content-Type': 'application/json'
		}),
		body: JSON.stringify({ selected, reload_ollama: reloadOllama }),
		timeoutMs: LONG_FETCH_TIMEOUT_MS
	});
	if (!response.ok) throw new Error(await readApiError(response));
	const envelope = parseLocalGenerationEnvelope(await response.json());
	if (!envelope) throw new Error('Malformed local generation switch response');
	return envelope;
}
