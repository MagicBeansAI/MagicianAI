/**
 * Floating Events Console state — visibility, position, size.
 *
 * The console is a draggable + resizable overlay that mounts the same
 * `<EventStreamCard />` as the `/events` route, but stays attached to
 * whichever route the operator is on. Toggled by ⌘E / ⌃E (was: jump to
 * `/events`); the full-page nav moved to ⌘⇧E / ⌃⇧E.
 *
 * Persists position + size to localStorage so the operator's preferred
 * layout survives reloads. The visibility flag is *not* persisted —
 * each tab boots with the console hidden, the operator pops it open
 * with the shortcut as needed.
 */
import { writable, type Writable } from 'svelte/store';
import { browser } from '$app/environment';

export interface EventsConsoleLayout {
	x: number;
	y: number;
	width: number;
	height: number;
}

const STORAGE_KEY = 'magician:events-console:layout';

const DEFAULT_LAYOUT: EventsConsoleLayout = {
	// Land in the upper-right area of the viewport on first open.
	// Resolved at-open-time against the current viewport so it always
	// fits — see `clampLayoutToViewport`.
	x: -1,
	y: -1,
	width: 720,
	height: 520
};

const MIN_WIDTH = 360;
const MIN_HEIGHT = 320;

function loadInitialLayout(): EventsConsoleLayout {
	if (!browser) return DEFAULT_LAYOUT;
	try {
		const raw = localStorage.getItem(STORAGE_KEY);
		if (!raw) return DEFAULT_LAYOUT;
		const parsed = JSON.parse(raw) as Partial<EventsConsoleLayout>;
		return {
			x: typeof parsed.x === 'number' ? parsed.x : DEFAULT_LAYOUT.x,
			y: typeof parsed.y === 'number' ? parsed.y : DEFAULT_LAYOUT.y,
			width: clamp(parsed.width ?? DEFAULT_LAYOUT.width, MIN_WIDTH, 4000),
			height: clamp(parsed.height ?? DEFAULT_LAYOUT.height, MIN_HEIGHT, 4000)
		};
	} catch {
		return DEFAULT_LAYOUT;
	}
}

function clamp(value: number, min: number, max: number): number {
	return Math.min(Math.max(value, min), max);
}

/**
 * Snap the layout into the current viewport. Called at open-time and
 * after window resize so the console can never end up off-screen (e.g.
 * a saved position from a wider monitor).
 */
export function clampLayoutToViewport(layout: EventsConsoleLayout): EventsConsoleLayout {
	if (!browser) return layout;
	const vw = window.innerWidth;
	const vh = window.innerHeight;
	const width = clamp(layout.width, MIN_WIDTH, vw - 16);
	const height = clamp(layout.height, MIN_HEIGHT, vh - 16);
	let { x, y } = layout;
	// First-open sentinel — pin to the upper-right.
	if (x < 0 || y < 0) {
		x = vw - width - 24;
		y = 64;
	}
	x = clamp(x, 8, vw - width - 8);
	y = clamp(y, 8, vh - height - 8);
	return { x, y, width, height };
}

export const eventsConsoleVisible: Writable<boolean> = writable(false);

export const eventsConsoleLayout: Writable<EventsConsoleLayout> = writable(loadInitialLayout());

// Debounced persistence — the layout store ticks once per
// `pointermove` while dragging or resizing the console (60 Hz on
// modern monitors). Persisting on every tick ran `JSON.stringify` +
// `localStorage.setItem` in the same frame, which on slower hardware
// stalled the main thread enough to drop drag frames. Coalescing to
// a single write 250ms after the last change keeps the persistence
// guarantee (the geometry is durable across reloads) without
// blocking the drag at all.
let persistTimer: ReturnType<typeof setTimeout> | null = null;
let pendingLayout: EventsConsoleLayout | null = null;
function flushLayout(): void {
	persistTimer = null;
	if (!browser || !pendingLayout) return;
	try {
		localStorage.setItem(STORAGE_KEY, JSON.stringify(pendingLayout));
	} catch {
		// Quota exceeded / disabled — non-fatal, console still works.
	}
	pendingLayout = null;
}
eventsConsoleLayout.subscribe((layout) => {
	if (!browser) return;
	pendingLayout = layout;
	if (persistTimer === null) {
		persistTimer = setTimeout(flushLayout, 250);
	}
});

export function toggleEventsConsole(): void {
	eventsConsoleVisible.update((open) => !open);
}

export function openEventsConsole(): void {
	eventsConsoleVisible.set(true);
}

export function closeEventsConsole(): void {
	eventsConsoleVisible.set(false);
}

export const EVENTS_CONSOLE_MIN_WIDTH = MIN_WIDTH;
export const EVENTS_CONSOLE_MIN_HEIGHT = MIN_HEIGHT;
