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

// Preserve the rail width across Chat, Settings and Files navigation.
export const chatHistoryExpanded = writable(true);
export const historyDrawerOpen = writable<boolean>(false);
export const historyDrawerThreadFilter = writable<string | null>(null);
export const historyDrawerInitialTab = writable<'sessions' | 'threads'>('sessions');
/**
 * Thread the reader tapped in a history panel. Both panels' Sessions lists stay
 * filtered to it (ahead of the route's thread) until × clears it or another tap
 * replaces it, the thread is archived or deleted, or the scope changes.
 * `openHistoryDrawer` leaves it alone.
 */
export const historyPickedThread = writable<string | null>(null);
/**
 * When (ms) a rail thread tap started navigating. A rail the route remounts
 * reads and clears it on mount, and reopens on Sessions if the tap is under
 * 10 s old; it can mount after `goto` resolves, while the thread loads. The
 * tapping rail clears it itself when the route kept it mounted.
 */
export const chatHistoryReopenOnSessions = writable<number | null>(null);

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
