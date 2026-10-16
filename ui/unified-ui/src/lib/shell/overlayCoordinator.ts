/**
 * OverlayCoordinator — single-overlay-at-a-time arbitration.
 *
 * The new shell has many surfaces that compete for focus: Attention input
 * modal, ApprovalDecisionModal, ExecutionPanel, page-local modals,
 * CommandPalette, HistoryDrawer, ChatBubble panel, etc. Without
 * coordination they stack on top of each other and create "layered modal"
 * confusion.
 *
 * This store holds the registry of currently-open overlays and exposes a
 * `requestFocus(id, priority)` API. Opening an overlay auto-closes lower-priority
 * overlays and a different overlay at the same priority. Esc closes the
 * highest-priority open overlay first.
 *
 * Priority matrix (1 = highest, 9 = lowest):
 *
 *   1  Attention input (masked password / external action / etc.)
 *   2  ApprovalDecisionModal
 *   3  ExecutionPanel
 *   4  Page-local modals (Budget, Vault, Bot editor, SwaggerModal, Edit&Replay,
 *      Delete-task, ExecutionPlanInspector, PublishedScrollCanvas)
 *   5  Global Attention center
 *   6  CommandPalette
 *   7  HistoryDrawer
 *   8  ChatBubble panel
 *
 *   — Always-visible non-blocking surfaces (top-bar Attention cascade,
 *     ChatBubble FAB, toasts, freeze banner) do not register.
 */

import { derived, writable, get } from 'svelte/store';
import { browser } from '$app/environment';

export const OVERLAY_PRIORITIES = {
	attentionInput: 1,
	approvalDecision: 2,
	executionPanel: 3,
	pageModal: 4,
	attentionCenter: 5,
	commandPalette: 6,
	historyDrawer: 7,
	chatBubble: 8
} as const;

export const OVERLAY_IDS = {
	attentionCenter: 'attention-center',
	attentionPrompt: 'attention-prompt',
	attentionChannelChild: 'attention-channel-follow-up'
} as const;

export type OverlayPriority = (typeof OVERLAY_PRIORITIES)[keyof typeof OVERLAY_PRIORITIES];

export interface OverlayEntry {
	id: string;
	priority: OverlayPriority;
	/** called when this overlay is forced closed by a higher-priority surface */
	onClose?: () => void;
	/** Exact higher-priority child overlays that may remain mounted above this parent. */
	allowedChildOverlayIds?: readonly string[];
}

const stack = writable<OverlayEntry[]>([]);

/** Read-only view: list of currently-open overlay entries. */
export const openOverlays = derived(stack, ($stack) => $stack);

/** Read-only view: the entry with the lowest priority number (highest priority), or null. */
export const focusedOverlay = derived(stack, ($stack) => {
	if ($stack.length === 0) return null;
	// The most recently registered overlay wins ties so Escape follows visual order.
	return $stack.reduce((min, cur) => (cur.priority <= min.priority ? cur : min));
});

/** Returns true when any overlay is currently open. */
export const hasOpenOverlay = derived(openOverlays, ($overlays) => $overlays.length > 0);

function callOnClose(entry: OverlayEntry): void {
	try {
		entry.onClose?.();
	} catch (err) {
		// eslint-disable-next-line no-console
		console.warn('[OverlayCoordinator] onClose threw for', entry.id, err);
	}
}

function closeCompeting(priority: OverlayPriority, childId: string): void {
	const current = get(stack);
	const survivors: OverlayEntry[] = [];
	const closing: OverlayEntry[] = [];
	for (const entry of current) {
		const retainsChild = entry.allowedChildOverlayIds?.includes(childId) === true;
		const samePriorityCompetitor = entry.priority === priority && entry.id !== childId;
		const unretainedLowerPriority = entry.priority > priority && !retainsChild;
		if (samePriorityCompetitor || unretainedLowerPriority) {
			closing.push(entry);
		} else {
			survivors.push(entry);
		}
	}
	if (closing.length === 0) return;

	// Remove entries before invoking callbacks so a callback's release() cannot
	// race this update or leave two same-priority focus traps registered.
	stack.set(survivors);
	for (const entry of closing) callOnClose(entry);
}

/**
 * Request focus for an overlay. Closes any currently-open overlays with
 * lower priority (higher number) or a different id at the same priority.
 * Idempotent — re-registering with the same id is a no-op. Returns false
 * when an already-focused higher-priority overlay rejects the request; the
 * rejected overlay's onClose is still run
 * so its backing UI state cannot remain invisibly open.
 */
export function requestFocus(entry: OverlayEntry): boolean {
	const current = get(stack);
	const existing = current.find((e) => e.id === entry.id);
	if (existing) return true;
	const blocker = get(focusedOverlay);
	if (blocker && blocker.priority < entry.priority) {
		callOnClose(entry);
		return false;
	}
	closeCompeting(entry.priority, entry.id);
	stack.update((s) => [...s, entry]);
	return true;
}

/** Release an overlay registration without firing its onClose handler. */
export function release(id: string): void {
	stack.update((s) => s.filter((e) => e.id !== id));
}

/** Force-close all overlays. Calls each onClose handler. */
export function closeAll(): void {
	const current = get(stack);
	for (const entry of current) {
		callOnClose(entry);
	}
	stack.set([]);
}

/** Force-close just the highest-priority open overlay (Esc handler). */
export function closeFocused(): void {
	const focused = get(focusedOverlay);
	if (!focused) return;
	callOnClose(focused);
	stack.update((s) => s.filter((e) => e.id !== focused.id));
}

/**
 * Mount a global Escape handler that closes the focused overlay. Idempotent —
 * call from the (app) layout root once. Returns a cleanup function.
 */
export function mountEscapeHandler(): () => void {
	if (!browser) return () => {};
	const handler = (event: KeyboardEvent): void => {
		if (event.key !== 'Escape') return;
		const focused = get(focusedOverlay);
		if (!focused) return;
		event.stopPropagation();
		closeFocused();
	};
	// Capture phase so the coordinator gets first crack before component-local
	// handlers (e.g. textareas) consume the keystroke.
	document.addEventListener('keydown', handler, { capture: true });
	return () => document.removeEventListener('keydown', handler, { capture: true });
}
