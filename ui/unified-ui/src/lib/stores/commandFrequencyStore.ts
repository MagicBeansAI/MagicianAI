/**
 * commandFrequencyStore — tracks how often each command-palette item is
 * executed and persists to localStorage. Exposes a frecency score for
 * ranking so the palette can surface frequently-used commands first.
 *
 * Frecency = log-frequency × exponential recency decay. A command used
 * 50 times yesterday still ranks above one used 200 times last month.
 * Half-life is ~7 days.
 *
 * The store is bounded: at most MAX_ENTRIES are kept; least-recently-used
 * entries get dropped when full so localStorage stays under a few KB.
 */
import { browser } from '$app/environment';
import { writable, get } from 'svelte/store';

const STORAGE_KEY = 'command-palette-frecency-v1';
const MAX_ENTRIES = 200;
const HALF_LIFE_DAYS = 7;

interface FrecencyEntry {
	count: number;
	lastUsed: number;
}

export type FrecencyMap = Record<string, FrecencyEntry>;

function loadFromStorage(): FrecencyMap {
	if (!browser) return {};
	try {
		const raw = localStorage.getItem(STORAGE_KEY);
		return raw ? (JSON.parse(raw) as FrecencyMap) : {};
	} catch {
		return {};
	}
}

function persistToStorage(map: FrecencyMap): void {
	if (!browser) return;
	try {
		const entries = Object.entries(map);
		let payload: FrecencyMap = map;
		if (entries.length > MAX_ENTRIES) {
			entries.sort((a, b) => b[1].lastUsed - a[1].lastUsed);
			payload = Object.fromEntries(entries.slice(0, MAX_ENTRIES));
		}
		localStorage.setItem(STORAGE_KEY, JSON.stringify(payload));
	} catch {
		// localStorage full / disabled / sandboxed — silent fail; ranking
		// just degrades to "no boost" rather than crashing the palette.
	}
}

const internal = writable<FrecencyMap>(loadFromStorage());

export const commandFrequencyStore = {
	subscribe: internal.subscribe,

	record(commandId: string): void {
		internal.update((map) => {
			const next: FrecencyMap = {
				...map,
				[commandId]: {
					count: (map[commandId]?.count ?? 0) + 1,
					lastUsed: Date.now()
				}
			};
			persistToStorage(next);
			return next;
		});
	},

	clear(): void {
		internal.set({});
		if (browser) {
			try {
				localStorage.removeItem(STORAGE_KEY);
			} catch {
				/* ignore */
			}
		}
	},

	snapshot(): FrecencyMap {
		return get(internal);
	}
};

/**
 * Score a command by its frecency entry. Higher = more relevant.
 * Returns 0 for unknown commands (never executed).
 */
export function frecencyScore(map: FrecencyMap, commandId: string): number {
	const entry = map[commandId];
	if (!entry) return 0;
	const ageDays = (Date.now() - entry.lastUsed) / (1000 * 60 * 60 * 24);
	const recency = Math.exp(-ageDays / HALF_LIFE_DAYS);
	const frequency = Math.log(entry.count + 1);
	return frequency * recency;
}
