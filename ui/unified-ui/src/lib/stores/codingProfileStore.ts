import { browser } from '$app/environment';
import { get, writable } from 'svelte/store';
import { timedFetch } from '$lib/shared/fetch';

const SELECTED_CODING_PROFILE_STORAGE_KEY = 'magician.coding.selectedProfile';

/** Client-only picker sentinel. Never sent as `profile_id`. */
export const AUTO_CODING_CHOICE_ID = 'auto';

export interface CodingProfile {
	id: string;
	label: string;
	llm_profile?: string;
	provider?: string;
	model?: string;
	supports_user_image_inputs: boolean;
	is_default: boolean;
	description?: string | null;
	engine?: string;
	selectable?: boolean;
	readiness?: string;
	reason?: string | null;
}

export type VibeDevCodingChoiceWire =
	| { kind: 'auto' }
	| { kind: 'profile'; profile_id: string };

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
		description: 'Coordinator may choose any eligible engine or profile',
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
	selected: string | null | undefined
): VibeDevCodingChoiceWire | null {
	const id = selected?.trim();
	if (!id) return null;
	if (isAutoCodingChoiceId(id)) return { kind: 'auto' };
	return { kind: 'profile', profile_id: id };
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

function persistSelection(id: string | null): void {
	if (!browser) return;
	try {
		if (id) {
			localStorage.setItem(SELECTED_CODING_PROFILE_STORAGE_KEY, id);
		} else {
			localStorage.removeItem(SELECTED_CODING_PROFILE_STORAGE_KEY);
		}
	} catch {
		// Storage can be unavailable in private windows. The backend
		// configured default still applies when persistence fails.
	}
}

function createCodingProfileStore() {
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
		isLoading: false,
		error: null
	});

	return {
		subscribe,
		load: async () => {
			update((state) => ({ ...state, isLoading: true, error: null }));
			try {
				const currentSelected = get({ subscribe }).selected ?? loadPersistedSelection();
				const response = await timedFetch('/api/magician/v2/coding/profiles');
				if (!response.ok) throw new Error(`HTTP ${response.status}`);
				const body = await response.json();
				const profiles = pickerCodingProfiles(body.profiles ?? []);
				const blocked = blockedCodingProfiles(body.profiles ?? []);
				const namedDefault =
					profiles.find((profile) => profile.is_default && !isAutoCodingChoiceId(profile.id))
						?.id ??
					profiles.find((profile) => !isAutoCodingChoiceId(profile.id))?.id ??
					null;
				const selected =
					isAutoCodingChoiceId(currentSelected) &&
					profiles.some((profile) => isAutoCodingChoiceId(profile.id))
						? AUTO_CODING_CHOICE_ID
						: currentSelected &&
							  profiles.some(
									(profile) =>
										profile.id === currentSelected && !isAutoCodingChoiceId(profile.id)
							  )
							? currentSelected
							: namedDefault;
				persistSelection(selected);
				set({ profiles, blocked, selected, isLoading: false, error: null });
			} catch (error) {
				update((state) => ({
					...state,
					isLoading: false,
					error: error instanceof Error ? error.message : String(error)
				}));
			}
		},
		select: (id: string) => {
			persistSelection(id);
			update((state) => ({ ...state, selected: id }));
		},
		getSelected: (): string | null => get({ subscribe }).selected
	};
}

export const codingProfileStore = createCodingProfileStore();
