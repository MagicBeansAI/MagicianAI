/**
 * Dashboard theme store.
 *
 * Mirrors the YAML-defined registry served by
 * `GET /api/magician/v2/dashboard_themes`. The store is lazy — the registry
 * is fetched the first time anything reads `themes` or asks for a theme
 * by id, cached for the session, and refreshed only on explicit
 * `refreshRegistry()` calls.
 *
 * Per-surface selection lives in `selectedThemeId` (a writable). The
 * `currentTheme` derived store returns the resolved theme object once the
 * registry is loaded; `null` while loading or when the id doesn't match.
 *
 * The store does NOT apply CSS variables to the document — that's the
 * `applyThemeToCssVariables` helper's job. Consumers call the helper
 * whenever `currentTheme` changes (typically in the published-surface
 * page's `$effect` / `$:` block).
 */

import { derived, writable, get, type Readable, type Writable } from 'svelte/store';
import { timedFetch } from '$lib/shared/fetch';

export interface ThemeFontFace {
	family: string;
	url?: string;
	weights: number[];
}

export interface ThemeFonts {
	display: ThemeFontFace;
	body: ThemeFontFace;
	mono: ThemeFontFace;
}

export interface ThemePalette {
	background: string;
	surface: string;
	foreground: string;
	foreground_muted: string;
	accent: string;
	accent_alt?: string;
	chart_colors: string[];
	border: string;
	shadow: string;
}

export interface ThemeSpacing {
	container_max_width_px: number;
	container_padding_x_px: number;
	container_padding_y_px: number;
	block_gap_px: number;
	prose_line_height: number;
	heading_scale: number;
}

export type AtmosphereKind =
	| 'flat'
	| 'paper_texture'
	| 'gradient_mesh'
	| 'noise'
	| 'scanlines'
	| 'watercolor';

export interface ThemeAtmosphere {
	kind: AtmosphereKind;
	intensity: number;
}

export interface ThemeMotion {
	load_stagger_ms: number;
	load_duration_ms: number;
	load_easing: string;
	hover_lift_px?: number;
	scroll_reveal?: boolean;
}

export interface DashboardTheme {
	id: string;
	name: string;
	description: string;
	best_for: string;
	fonts: ThemeFonts;
	palette: ThemePalette;
	spacing: ThemeSpacing;
	atmosphere: ThemeAtmosphere;
	motion: ThemeMotion;
}

type LoadState = 'idle' | 'loading' | 'ready' | 'error';

interface RegistryState {
	state: LoadState;
	themes: DashboardTheme[];
	error: string | null;
}

const INITIAL_STATE: RegistryState = { state: 'idle', themes: [], error: null };

const registryStore: Writable<RegistryState> = writable(INITIAL_STATE);
let inflightFetch: Promise<RegistryState> | null = null;

async function fetchRegistry(): Promise<RegistryState> {
	try {
		const response = await timedFetch('/api/magician/v2/dashboard_themes', {
			headers: { Accept: 'application/json' }
		});
		if (!response.ok) {
			const message = `Theme registry fetch failed: HTTP ${response.status}`;
			const next: RegistryState = { state: 'error', themes: [], error: message };
			registryStore.set(next);
			return next;
		}
		const body = (await response.json()) as { themes: DashboardTheme[] };
		const next: RegistryState = { state: 'ready', themes: body.themes ?? [], error: null };
		registryStore.set(next);
		return next;
	} catch (err) {
		const message = err instanceof Error ? err.message : String(err);
		const next: RegistryState = { state: 'error', themes: [], error: message };
		registryStore.set(next);
		return next;
	}
}

/**
 * Ensure the registry is fetched. Safe to call multiple times — concurrent
 * callers share the same in-flight fetch.
 */
export async function ensureRegistryLoaded(): Promise<RegistryState> {
	const current = get(registryStore);
	if (current.state === 'ready' || current.state === 'error') {
		return current;
	}
	if (!inflightFetch) {
		registryStore.set({ state: 'loading', themes: [], error: null });
		inflightFetch = fetchRegistry().finally(() => {
			inflightFetch = null;
		});
	}
	return inflightFetch;
}

/**
 * Force a re-fetch — useful when a scope-specific overlay theme has been
 * authored at runtime and the user wants to pick it up without restarting.
 */
export async function refreshRegistry(): Promise<RegistryState> {
	registryStore.set({ state: 'loading', themes: [], error: null });
	inflightFetch = fetchRegistry().finally(() => {
		inflightFetch = null;
	});
	return inflightFetch;
}

export const themeRegistry: Readable<RegistryState> = {
	subscribe: registryStore.subscribe
};

/**
 * Per-surface theme selection. Set when a published surface declares its
 * theme id in the response metadata. Default `null` means "fall back to
 * the first theme in the registry" so a fresh app load still has CSS
 * variables to render against.
 */
export const selectedThemeId: Writable<string | null> = writable(null);

/**
 * Resolved theme object — `null` until the registry is loaded or when the
 * selected id doesn't match any registry entry.
 */
export const currentTheme: Readable<DashboardTheme | null> = derived(
	[registryStore, selectedThemeId],
	([$registry, $id]) => {
		if ($registry.state !== 'ready') return null;
		if (!$id) return $registry.themes[0] ?? null;
		return $registry.themes.find((t) => t.id === $id) ?? $registry.themes[0] ?? null;
	}
);

/**
 * Synchronous lookup helper for non-reactive callers (e.g. test fixtures).
 * Returns `null` if registry isn't loaded yet.
 */
export function getThemeById(id: string): DashboardTheme | null {
	const reg = get(registryStore);
	if (reg.state !== 'ready') return null;
	return reg.themes.find((t) => t.id === id) ?? null;
}
