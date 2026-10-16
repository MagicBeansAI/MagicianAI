/**
 * Shared shell-state stores.
 *
 * The (app) layout owns the actual `<CommandPalette />` and
 * `<HistoryDrawer />` mounts. Components elsewhere (TopBar, ContextPill,
 * future page chrome) need to *trigger* those overlays without owning
 * their state. These stores let them write through.
 *
 * Usage from a child component:
 *   import { historyDrawerOpen, historyDrawerThreadFilter } from '$lib/shell/shellState';
 *   historyDrawerThreadFilter.set('marketing');
 *   historyDrawerOpen.set(true);
 *
 * The layout binds its `<HistoryDrawer bind:open={$historyDrawerOpen}
 * threadFilter={$historyDrawerThreadFilter} />` so any writer flips it.
 */

import { writable } from 'svelte/store';

export const commandPaletteOpen = writable<boolean>(false);

export const historyDrawerOpen = writable<boolean>(false);
export const historyDrawerThreadFilter = writable<string | null>(null);
export const historyDrawerInitialTab = writable<'sessions' | 'threads'>('sessions');

export function openHistoryDrawer(opts: {
	threadFilter?: string | null;
	initialTab?: 'sessions' | 'threads';
} = {}): void {
	// An opener must declare its scope every time. Treat an omitted filter as
	// the global drawer so a prior thread-scoped open cannot leak into the next
	// top-bar/command invocation.
	historyDrawerThreadFilter.set(opts.threadFilter ?? null);
	historyDrawerInitialTab.set(opts.initialTab ?? 'sessions');
	historyDrawerOpen.set(true);
}

export function openCommandPalette(): void {
	commandPaletteOpen.set(true);
}
