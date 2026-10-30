import { browser } from '$app/environment';
import { get, writable } from 'svelte/store';
import type { ModelChoice, ModelSelection } from '$lib/llm/modelCatalog';
import { migrateLegacyCodingSelection } from '$lib/llm/legacySelections';
import { fetchCodingModelRows, typedCodingModelChoice } from '$lib/llm/codingModelSelection';

const SELECTED_CODING_PROFILE_STORAGE_KEY = 'magician.coding.selectedProfile';
const SELECTED_CODING_MODEL_STORAGE_KEY = 'magician.coding.modelSelection';

export type SavedCodingSelection = { kind: 'auto' } | ({ kind: 'model'; engine: string } & ModelSelection);

export function decodeSavedCodingSelection(raw: string | null): SavedCodingSelection | null {
	if (!raw) return null;
	try {
		const value = JSON.parse(raw);
		if (value?.kind === 'auto') return { kind: 'auto' };
		if (value?.kind === 'model' && typeof value.engine === 'string' && value.engine.trim()
			&& typeof value.model_id === 'string' && value.model_id.trim()
			&& typeof value.thinking === 'string' && value.thinking.trim()) {
			return { kind: 'model', engine: value.engine, model_id: value.model_id, thinking: value.thinking };
		}
	} catch { /* Retain malformed preferences until an explicit new choice. */ }
	return null;
}

export function savedCodingSelection(profiles: readonly CodingProfile[], id: string | null): SavedCodingSelection | null {
	if (isAutoCodingChoiceId(id)) return { kind: 'auto' };
	const row = profiles.find(profile => profile.id === id);
	return row?.engine && row.model_selection ? { kind: 'model', engine: row.engine, ...row.model_selection } : null;
}

export function resolveSelectedCodingProfile(
	profiles: readonly CodingProfile[], current: string | null, saved: SavedCodingSelection | null
): string | null {
	const choice = saved ?? savedCodingSelection(profiles, current) ?? migrateLegacyCodingSelection(current);
	if (choice?.kind === 'auto') return AUTO_CODING_CHOICE_ID;
	if (choice?.kind === 'model') {
		const route = profiles.find(profile => profile.engine === choice.engine && profile.model_choice?.id === choice.model_id)
			?.model_choice?.choices[choice.thinking];
		return route && profiles.some(profile => profile.id === route && profile.engine === choice.engine)
			? route : '__unavailable_coding_selection__';
	}
	// Never replace a missing saved choice with a configured default.
	return current ?? profiles.find(profile => profile.is_default)?.id ?? null;
}

/** Client-only picker sentinel. Never sent as `profile_id`. */
export const AUTO_CODING_CHOICE_ID = 'auto';

export interface CodingProfile {
	id: string;
	label: string;
	llm_profile?: string;
	provider?: string;
	model?: string;
	model_selection?: ModelSelection | null;
	model_choice?: ModelChoice | null;
	supports_user_image_inputs: boolean;
	is_default: boolean;
	description?: string | null;
	engine?: string;
	selectable?: boolean;
	readiness?: string;
	reason?: string | null;
	/** Grok only: what its MCP gateway can reach outside Magician. */
	gateway?: CodingEngineGateway | null;
	/** Whose credentials a launch uses: `login`, `magician_key` or `none`. */
	credentials?: string;
}

export interface CodingEngineGateway {
	mcp_servers: string[];
	plugins: string[];
	/** False when the CLI listings failed: the lists are unknown, not empty. */
	listed: boolean;
}

export type VibeDevCodingChoiceWire = SavedCodingSelection;

/** Composer only shows rows that are selectable. Missing `selectable` is Pi. */
export function selectableCodingProfiles(profiles: CodingProfile[]): CodingProfile[] {
	return profiles.filter((profile) => profile.selectable !== false);
}

/**
 * Engine rows the server reports as not selectable (sign-in required, failed
 * isolation, not installed, disabled). Pickers show them disabled with their
 * reason so a blocked engine is visible instead of silently missing; they are
 * never part of `profiles`, so selection, defaults and other consumers never
 * see them.
 */
export function blockedCodingProfiles(profiles: CodingProfile[]): CodingProfile[] {
	return profiles.filter((profile) => profile.selectable === false);
}

/** Option text: a blocked row names why it cannot be chosen. */
export function codingProfileOptionLabel(profile: CodingProfile): string {
	if (profile.selectable !== false) return profile.label;
	const reason = profile.reason?.trim();
	return reason ? `${profile.label} (unavailable: ${reason})` : `${profile.label} (unavailable)`;
}

export function isAutoCodingChoiceId(id: string | null | undefined): boolean {
	return id === AUTO_CODING_CHOICE_ID;
}

/** Auto is opt-in and never the configured default. */
export function autoCodingProfile(named: CodingProfile[]): CodingProfile {
	return {
		id: AUTO_CODING_CHOICE_ID,
		label: 'Auto',
		description: 'Coordinator may choose any eligible engine or model',
		supports_user_image_inputs: named.some((profile) => profile.supports_user_image_inputs),
		is_default: false,
		selectable: true,
		readiness: 'ready'
	};
}

/** Named selectable rows plus an Auto row when the picker is usable. */
export function pickerCodingProfiles(profiles: CodingProfile[]): CodingProfile[] {
	const named = selectableCodingProfiles(profiles);
	if (named.length === 0) return named;
	return [...named, autoCodingProfile(named)];
}

const CODING_ENGINE_LABELS: Record<string, string> = {
	pi: 'Pi',
	codex_app_server: 'Codex',
	claude_code: 'Claude Code',
	grok_acp: 'Grok',
	agy_cli: 'Antigravity'
};

const CODING_ENGINE_NOTES: Record<string, string> = {
	pi: "Magician's own coding agent. You pick the model.",
	codex_app_server: 'Codex CLI with your ChatGPT / OpenAI login.',
	claude_code: 'Claude Code with your Claude login.',
	grok_acp: 'Grok Build CLI with your xAI login.',
	agy_cli: 'Antigravity CLI with your Google login.'
};

/** One line on what an engine is and whose credentials it runs on. */
export function codingEngineNote(
	engine: string | null | undefined,
	credentials?: string | null
): string | null {
	if (!engine) return null;
	if (engine !== 'pi' && credentials === 'magician_key') {
		return `${codingEngineLabel(engine)} with Magician’s provider key (Settings → Harness credentials).`;
	}
	if (credentials === 'none') {
		return `${codingEngineLabel(engine)} is not signed in, and Magician’s keys are off for it.`;
	}
	return CODING_ENGINE_NOTES[engine] ?? null;
}

/**
 * What a gateway reaches, for the picker. Names what the CLI listed; says
 * "unknown" when it could not list; always names account connectors, which
 * live server-side and cannot be listed.
 */
export function codingGatewaySummary(gateway: CodingEngineGateway): string {
	const local = !gateway.listed
		? 'MCP servers and plugins: unknown (listing failed)'
		: [
				gateway.mcp_servers.length
					? `MCP servers: ${gateway.mcp_servers.join(', ')}`
					: 'MCP servers: none',
				gateway.plugins.length ? `plugins: ${gateway.plugins.join(', ')}` : 'plugins: none'
			].join(' · ');
	return `Its MCP gateway can reach ${local}, plus any connectors on your xAI account (not listable). Magician does not control these.`;
}

/** Display name for a coding engine id; unknown ids show as sent. */
export function codingEngineLabel(engine: string | null | undefined): string {
	if (!engine) return 'Other';
	return CODING_ENGINE_LABELS[engine] ?? engine;
}

export interface CodingProfileGroup {
	/** Engine display name, or null for rows with no engine (Auto). */
	label: string | null;
	profiles: CodingProfile[];
}

/**
 * Picker rows grouped by engine, in first-seen order, with engine-less rows
 * (Auto) last and ungrouped. Config profiles are all Pi and their labels name
 * only the model ("Grok 4.7 coding" is Pi running a Grok model), so without
 * the group the picker never says which engine runs a row.
 */
export function groupCodingProfilesByEngine(profiles: CodingProfile[]): CodingProfileGroup[] {
	const groups = new Map<string, CodingProfile[]>();
	const loose: CodingProfile[] = [];
	for (const profile of profiles) {
		if (!profile.engine) {
			loose.push(profile);
			continue;
		}
		const label = codingEngineLabel(profile.engine);
		const rows = groups.get(label) ?? [];
		rows.push(profile);
		groups.set(label, rows);
	}
	const grouped: CodingProfileGroup[] = [...groups].map(([label, rows]) => ({
		label,
		profiles: rows
	}));
	return loose.length > 0 ? [...grouped, { label: null, profiles: loose }] : grouped;
}

export function codingChoiceFromSelection(
	selected: string | null | undefined,
	profiles?: readonly CodingProfile[]
): VibeDevCodingChoiceWire | null {
	const id = selected?.trim();
	if (!id) return null;
	if (profiles) return typedCodingModelChoice(profiles, id);
	const state = get(codingProfileStore);
	if (state.error || state.isLoading) throw new Error(state.error ?? 'Coding model choices are still loading.');
	if (!state.profiles.some(profile => profile.id === id)) throw new Error('Choose an available coding model to continue.');
	return typedCodingModelChoice(state.profiles, id);
}

/** Chat/voice show the coding picker only for an explicit `@vibedev` draft. */
export function draftInvokesVibedev(text: string): boolean {
	return /@vibedev\b/i.test(text);
}

function loadPersistedSelection(): string | null {
	if (!browser) return null;
	try {
		const value = localStorage.getItem(SELECTED_CODING_PROFILE_STORAGE_KEY);
		return value && value.trim() ? value : null;
	} catch {
		return null;
	}
}

function persistedModelSelection(): { saved: SavedCodingSelection | null; invalid: boolean } {
	if (!browser) return { saved: null, invalid: false };
	try {
		const raw = localStorage.getItem(SELECTED_CODING_MODEL_STORAGE_KEY);
		const saved = decodeSavedCodingSelection(raw);
		return { saved, invalid: raw !== null && saved === null };
	} catch { return { saved: null, invalid: false }; }
}

function persistSelection(id: string | null, profiles: readonly CodingProfile[]): void {
	if (!browser) return;
	try {
		if (id) {
			const selection = savedCodingSelection(profiles, id);
			if (!selection) return;
			localStorage.setItem(SELECTED_CODING_MODEL_STORAGE_KEY, JSON.stringify(selection));
			localStorage.removeItem(SELECTED_CODING_PROFILE_STORAGE_KEY);
		} else {
			localStorage.removeItem(SELECTED_CODING_PROFILE_STORAGE_KEY);
			localStorage.removeItem(SELECTED_CODING_MODEL_STORAGE_KEY);
		}
	} catch {
		// Storage can be unavailable in private windows. The backend
		// configured default still applies when persistence fails.
	}
}

function createCodingProfileStore() {
	let loadGeneration = 0;
	const { subscribe, set, update } = writable<{
		profiles: CodingProfile[];
		blocked: CodingProfile[];
		selected: string | null;
		isLoading: boolean;
		error: string | null;
	}>({
		profiles: [],
		blocked: [],
		selected: loadPersistedSelection(),
		isLoading: true,
		error: null
	});

	return {
		subscribe,
		load: async () => {
			const generation = ++loadGeneration;
			update((state) => ({ ...state, isLoading: true, error: null }));
			try {
				const currentSelected = get({ subscribe }).selected ?? loadPersistedSelection();
				const { rows } = await fetchCodingModelRows();
				if (generation !== loadGeneration) return;
				const profiles = pickerCodingProfiles(rows);
				const blocked = blockedCodingProfiles(rows);
				const { saved, invalid } = persistedModelSelection();
				const selected = invalid ? '__unavailable_coding_selection__'
					: resolveSelectedCodingProfile(profiles, currentSelected, saved);
				const error = selected && !profiles.some(profile => profile.id === selected)
					? 'Your saved coding model choice is unavailable. Choose an engine and model to continue.'
					: !selected ? 'Choose an engine and model to continue.' : null;
				if (!error) persistSelection(selected, profiles);
				set({ profiles, blocked, selected, isLoading: false, error });
			} catch (error) {
				if (generation !== loadGeneration) return;
				update((state) => ({
					...state,
					isLoading: false,
					error: error instanceof Error ? error.message : String(error)
				}));
			}
		},
		select: (id: string) => {
			const profiles = get({ subscribe }).profiles;
			if (!profiles.some(profile => profile.id === id)) return;
			const selected = resolveSelectedCodingProfile(profiles, id, null);
			persistSelection(selected, profiles);
			update((state) => ({ ...state, selected, error: null }));
		},
		getSelected: (): string | null => get({ subscribe }).selected
	};
}

export const codingProfileStore = createCodingProfileStore();
