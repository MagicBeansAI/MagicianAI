<!--
  /notify-overlay — transparent notification overlay surface.

  Loaded by a separate transparent, always-on-top Tauri window (label
  `notify-overlay`) created in desktop/src-tauri/src/overlay.rs. The window is
  sized + pinned BOTTOM-right by Rust (`resize_notify_overlay`) and grows UPWARD
  with a fixed bottom edge; this route just fills its width and reports its
  content height back on every change so the window hugs the card stack (which
  re-pins the fixed bottom). Chrome-less like /draw-overlay: it sits at the
  route root (a sibling of the (app) group), so the root +layout.svelte — which
  has no nav/sidebar shell — is the only layout it inherits.

  This route owns the VIEW LAYER for the data layer in `$lib/notify`:
    - the `cards` store (V3 HITL stream, deduped/dismissed in `notifyStream.ts`,
      arrival-ordered: index 0 = earliest) splits into a persistent APPROVAL
      queue (kind `actionable`) and an auto-dismissing TRANSIENT family (info/
      success/error). Total visible cards are capped at MAX_VISIBLE (4),
      approvals first; since the window grows upward, the stack renders
      newest-transient at the top, then a muted "+N more pending" chip for any
      capped-out approvals, then the visible approvals with the EARLIEST at the
      very bottom — so the approval being acted on is never buried;
    - actionable cards are compact launchers: whole-card click opens the exact
      canonical global prompt; response controls stay in that prompt;
    - it auto show/hides + (de)focuses the Tauri window as the stack fills and
      empties, schedules transient auto-dismiss timers ONLY once a card is
      visible (so a capped-out transient isn't lost), and keeps the
      ResizeObserver height reporting that drives the bottom-anchored re-pin.

  All Tauri `invoke` calls are guarded behind the dynamic-import try/catch so
  the route still renders in a plain browser (svelte-check / vitest / preview).
-->
<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { fly, fade } from 'svelte/transition';
	import { flip } from 'svelte/animate';
	import { cubicOut } from 'svelte/easing';
	import NotifyCard from '$lib/notify/NotifyCard.svelte';
	import type { NotifyCard as NotifyCardModel } from '$lib/notify/cardModel';
	import {
		cards,
		startNotifyStream,
		stopNotifyStream,
		reconcileStaleCards
	} from '$lib/notify/notifyStream';
	import { suppressActionableNotification } from '$lib/notify/suppression';
	import { showError } from '$lib/shared/stores/notifications';

	let stackEl: HTMLDivElement;

	// ── App-theme mirror ──────────────────────────────────────────────────────
	// This route runs in a SEPARATE transparent Tauri webview (label
	// `notify-overlay`), a different origin from the main app surface — so it does
	// NOT share `localStorage['magican-theme']`, and the root layout's `themeStore.init()`
	// would only ever resolve the DEFAULT theme here, not the user's active one.
	// The card CSS reads the app's theme tokens (`--bg-elevated` / `--text-…` /
	// `--border-…` / `--accent-…`), which only resolve to the right palette once
	// `<html data-theme="…">` is set. So we mirror the live app theme exactly the
	// way the desktop Settings/setup webviews do (desktop/src/App.svelte): read the
	// brokered snapshot on mount (`get_app_theme`), then track every switch via the
	// `app-theme-changed` Tauri event. Both apply the theme NAME (so name-keyed CSS
	// in the bundled app.css matches) AND the resolved tokens inline as a belt-and-
	// braces fallback. Env-guarded: in a plain browser (tests/preview) the dynamic
	// imports throw and we simply keep the default theme.
	interface AppThemeSnapshot {
		name: string;
		tokens: Record<string, string>;
	}

	function applyAppTheme(snapshot: AppThemeSnapshot | null): void {
		if (!snapshot) return;
		const root = document.documentElement;
		// Equality guard: the broker rebroadcasts `app-theme-changed` to EVERY
		// window (including this one), so skip the `data-theme` write when it's
		// already applied. An echoed event is then a true no-op that can't re-arm a
		// MutationObserver or re-trigger a publish — defends against theme flicker.
		if (snapshot.name && root.getAttribute('data-theme') !== snapshot.name) {
			root.setAttribute('data-theme', snapshot.name);
		}
		for (const [key, value] of Object.entries(snapshot.tokens ?? {})) {
			if (key.startsWith('--') && value) root.style.setProperty(key, value);
		}
	}

	let unlistenTheme: (() => void) | null = null;

	async function startThemeMirror(): Promise<void> {
		try {
			const { invoke } = await import('@tauri-apps/api/core');
			const { listen } = await import('@tauri-apps/api/event');
			applyAppTheme(await invoke<AppThemeSnapshot>('get_app_theme'));
			unlistenTheme = await listen<AppThemeSnapshot>('app-theme-changed', (event) =>
				applyAppTheme(event.payload)
			);
		} catch {
			/* plain browser (tests/preview) — no Tauri host; keep the default theme */
		}
	}

	// ── Notifications-enabled mirror (tray Show/Hide toggle) ──────────────────
	// The tray owns the master Show/Hide-notifications switch (persisted in the
	// desktop config). This route mirrors it: seed from `get_notifications_enabled`
	// on mount (default ON), then track every flip via the
	// `notifications-enabled-changed` Tauri event. The flag gates only the
	// overlay WINDOW visibility — the card stack, the stream, and the tray
	// pending-count keep flowing regardless of it (so the tray counter stays
	// accurate while muted, and un-muting re-shows the current pending stack).
	let notificationsEnabled = true;
	let unlistenNotificationsEnabled: (() => void) | null = null;

	async function startNotificationsEnabledMirror(): Promise<void> {
		try {
			const { invoke } = await import('@tauri-apps/api/core');
			const { listen } = await import('@tauri-apps/api/event');
			notificationsEnabled = await invoke<boolean>('get_notifications_enabled');
			// Re-evaluate window visibility against the seeded flag + current cards.
			reconcileWindowVisibility(notificationsEnabled, lastCardCount > 0);
			unlistenNotificationsEnabled = await listen<boolean>(
				'notifications-enabled-changed',
				(event) => {
					notificationsEnabled = event.payload;
					// Toggling Hide immediately hides the window even with cards present;
					// toggling Show re-shows it when there are cards.
					reconcileWindowVisibility(notificationsEnabled, lastCardCount > 0);
				}
			);
		} catch {
			/* plain browser (tests/preview) — no Tauri host; keep notifications ON */
		}
	}

	// ── Tauri invoke, env-guarded ────────────────────────────────────────────
	// Each helper resolves the host `invoke` lazily and swallows the throw in a
	// plain browser (no Tauri host). Keeping them tiny + isolated means a single
	// failed call never tears down the rest of the overlay's reactivity.
	async function invokeOverlay(command: string, args?: Record<string, unknown>): Promise<void> {
		try {
			const { invoke } = await import('@tauri-apps/api/core');
			await invoke(command, args);
		} catch {
			/* plain browser (tests/preview) — no Tauri host, no-op */
		}
	}

	// Like `invokeOverlay` but REPORTS the outcome: true iff the host `invoke`
	// resolved, false on a plain browser (no host) or any invoke/IPC error. The
	// tray pending-count sync needs this so a dropped/failed push is NOT latched
	// as "delivered" — leaving the count free to re-assert and converge.
	async function invokeOverlayResult(
		command: string,
		args?: Record<string, unknown>
	): Promise<boolean> {
		try {
			const { invoke } = await import('@tauri-apps/api/core');
			await invoke(command, args);
			return true;
		} catch {
			return false;
		}
	}

	// Report the stack's measured content height to Rust so the top-right
	// window resizes to hug the card stack (and re-pins itself).
	async function reportHeight() {
		if (!stackEl) return;
		const h = Math.ceil(stackEl.getBoundingClientRect().height);
		await invokeOverlay('resize_notify_overlay', { height: Math.max(h, 1) });
	}

	// Idempotent removal: the card may already be gone (the stream's
	// `HitlResolved` arrived first). `filter` on a missing id is a no-op.
	function removeCard(id: string): void {
		cards.update((current) => current.filter((c) => c.id !== id));
	}

	function handleOpenInApp(event: CustomEvent<NotifyCardModel>): void {
		const card = event.detail;
		if (card.kind !== 'actionable') return;
		// The notification carries the canonical HITL id. Deep-link the dedicated
		// native window to the same /attention route the browser uses; the page's
		// existing item resolver opens the matching prompt from that id.
		const query = new URLSearchParams({ attention_item: card.correlationId });
		void (async () => {
			const opened = await invokeOverlayResult('open_app_at', {
				path: `/attention?${query.toString()}`
			});
			if (opened) removeCard(card.id);
			else showError('Could not open notification', 'The Attention request is still pending.');
		})();
	}

	// ── Deep-link (informational `open`) ──────────────────────────────────────
	// An informational card carrying a `deepLink` opens the main app at that path
	// (Rust `open_app_at({ path })`), then removes itself from the stack. A
	// missing/empty deepLink is a no-op so the card simply stays.
	function handleOpen(event: CustomEvent<NotifyCardModel>): void {
		const card = event.detail;
		const path = card.kind === 'actionable' ? undefined : card.deepLink;
		if (!path) return;
		void invokeOverlay('open_app_at', { path });
		removeCard(card.id);
	}

	// ── Dismiss ───────────────────────────────────────────────────────────────
	function handleDismiss(event: CustomEvent<NotifyCardModel>): void {
		const card = event.detail;
		if (card.kind === 'actionable') {
			// Persist by correlation id. Replays/reconnects stay hidden, while a
			// genuinely resurfaced request with a new id remains eligible.
			suppressActionableNotification(card.correlationId);
		}
		removeCard(card.id);
	}

	// ── Auto-dismiss timers (VISIBLE transient only) ─────────────────────────
	// Informational cards may carry a `dismissAfterMs`; the model stays pure (it
	// never starts a timer) so the VIEW owns scheduling here. Crucially, the TTL
	// timer starts only once a transient card is actually VISIBLE — never at
	// arrival — so a transient that is capped out (queued behind the visible 4)
	// isn't silently lost before the user ever sees it. We keep one live timer
	// per visible-transient id and reconcile against the set of currently
	// VISIBLE transient ids on every change:
	//   - a visible transient with `dismissAfterMs` and no live timer → start one;
	//   - an id that has LEFT the store (resolved / dismissed / opened) → clear +
	//     drop its timer so a stale timeout never removes a re-used id.
	// A still-queued (not-yet-visible) transient keeps NO timer, so promoting it
	// to visible later starts its full TTL fresh. Actionable cards have no
	// `dismissAfterMs` → no timer → they never auto-dismiss.
	const dismissTimers = new Map<string, ReturnType<typeof setTimeout>>();

	function reconcileDismissTimers(
		visibleTransientList: Extract<NotifyCardModel, { kind: 'info' | 'success' | 'error' }>[],
		storeIds: Set<string>
	): void {
		// Start a timer for any visible transient that carries a TTL and doesn't
		// already have one running.
		for (const card of visibleTransientList) {
			const ms = card.dismissAfterMs;
			if (typeof ms === 'number' && ms > 0 && !dismissTimers.has(card.id)) {
				const id = card.id;
				dismissTimers.set(
					id,
					setTimeout(() => {
						dismissTimers.delete(id);
						removeCard(id);
					}, ms)
				);
			}
		}
		// Drop timers for ids that have left the STORE entirely (a card that is
		// merely no longer visible but still in the store keeps its running timer
		// so it continues counting down toward dismissal).
		for (const [id, handle] of dismissTimers) {
			if (!storeIds.has(id)) {
				clearTimeout(handle);
				dismissTimers.delete(id);
			}
		}
	}

	// ── Tray pending-approval count ───────────────────────────────────────────
	// Drive the tray's pending-approval count (painted on the menu-bar icon via
	// `TrayIcon::set_title`, and appended to the Show/Hide-notifications toggle
	// label) from the WHOLE approval queue length (every actionable card,
	// including any that overflow the visible cap below), so a hidden/muted
	// overlay still signals the full backlog. Informational cards don't count.
	// The count flows REGARDLESS of the Show/Hide toggle.
	//
	// SELF-HEALING delivery. The count is the overlay's source of truth, pushed to
	// Rust over IPC. A naive "latch the value, then fire-and-forget, swallow
	// errors" loses the badge forever if a single push is dropped (a hidden webview
	// throttled mid-reconnect, a cross-surface HitlResolved the overlay never
	// received, an IPC blip): the de-dupe guard then refuses to re-emit the
	// unchanged value, stranding a stale badge. So we (1) latch `lastPendingCount`
	// only AFTER a CONFIRMED push, (2) coalesce last-write-wins through a single
	// flush loop, and (3) periodically re-assert (reconcile, see onMount) so the
	// badge converges to truth within one interval even if every event-driven push
	// failed.
	let currentApprovalCount = 0; // latest truth derived from the card store
	let lastPendingCount = -1; // last value CONFIRMED pushed to Rust (-1 = unknown)
	let desiredPendingCount = -1; // latest value we want Rust to show
	let flushingPendingCount = false;
	let pendingCountReconcileTimer: ReturnType<typeof setInterval> | null = null;
	const PENDING_COUNT_RECONCILE_MS = 15_000;
	// Stale actionable-card reconcile (drops orphaned HITL cards whose backing
	// request is no longer pending — see reconcileStaleCards in notifyStream.ts).
	// Slightly slower cadence than the count reconcile since it makes 1-2 GETs.
	let staleCardReconcileTimer: ReturnType<typeof setInterval> | null = null;
	const STALE_CARD_RECONCILE_MS = 20_000;
	function syncPendingCount(approvalsLen: number): void {
		desiredPendingCount = approvalsLen;
		void flushPendingCount();
	}
	async function flushPendingCount(): Promise<void> {
		if (flushingPendingCount) return; // a flush loop is already draining
		flushingPendingCount = true;
		try {
			// Re-read `desiredPendingCount` each pass so the LAST value wins even if
			// several store mutations landed while an invoke was in flight.
			while (desiredPendingCount !== lastPendingCount) {
				const target = desiredPendingCount;
				const ok = await invokeOverlayResult('set_pending_approval_count', { count: target });
				if (!ok) {
					// Transient host/IPC failure: stop WITHOUT latching, so the next
					// store mutation or the reconcile tick retries. Leaving
					// `lastPendingCount` stale is exactly what lets the badge recover.
					break;
				}
				lastPendingCount = target;
			}
		} finally {
			flushingPendingCount = false;
		}
	}
	// Force a re-push of the current truth even when the value looks unchanged, so
	// a badge stranded by a dropped push (or a HitlResolved the overlay never
	// received) self-heals instead of lingering. Driven by the reconcile interval.
	function reassertPendingCount(): void {
		lastPendingCount = -1; // invalidate the de-dupe guard
		syncPendingCount(currentApprovalCount);
	}

	// ── Window visibility (gated on the Show/Hide toggle) ─────────────────────
	// The overlay window is shown only when notifications are ENABLED AND there
	// is at least one card. Edge-triggered on the desired-visible boolean so we
	// don't spam show/hide invokes on every store mutation. Recomputed from BOTH
	// inputs: the card-stack subscriber passes the live card count, and the
	// notifications-enabled mirror passes the flag — either changing re-evaluates
	// the same predicate. `lastCardCount` lets the flag listener recompute against
	// the latest stack without resubscribing.
	let lastCardCount = 0;
	let lastWindowVisible: boolean | null = null;
	function reconcileWindowVisibility(enabled: boolean, hasCards: boolean): void {
		const shouldShow = enabled && hasCards;
		if (shouldShow === lastWindowVisible) return;
		lastWindowVisible = shouldShow;
		void invokeOverlay(shouldShow ? 'show_notify_overlay' : 'hide_notify_overlay');
	}

	// ── Grouped, capped, bottom-anchored derivation ───────────────────────────
	// The Rust window is BOTTOM-anchored and grows UPWARD, and a flex-column's
	// LAST child renders at the bottom. `$cards` preserves arrival order
	// (applyEvent appends), so index 0 is the EARLIEST.
	//
	// We split into the persistent APPROVAL queue and the auto-dismissing
	// TRANSIENT family, then cap the total visible cards at MAX_VISIBLE:
	// approvals take priority (earliest first), transient fills any remaining
	// slots. `approvalsOverflow` counts approvals hidden behind the cap (still
	// counted in the tray pending count); resolving the bottom (earliest)
	// approval frees a slot so a hidden newer approval becomes visible.
	const MAX_VISIBLE = 4;
	$: approvals = $cards.filter(
		(c): c is Extract<NotifyCardModel, { kind: 'actionable' }> => c.kind === 'actionable'
	); // arrival order; [0] = earliest
	$: transient = $cards.filter(
		(c): c is Extract<NotifyCardModel, { kind: 'info' | 'success' | 'error' }> =>
			c.kind !== 'actionable'
	); // arrival order; [0] = earliest
	$: visibleApprovals = approvals.slice(0, MAX_VISIBLE); // the EARLIEST up to 4
	$: approvalsOverflow = approvals.length - visibleApprovals.length;
	$: transientSlots = Math.max(0, MAX_VISIBLE - visibleApprovals.length);
	$: visibleTransient = transient.slice(0, transientSlots); // earliest pending, FIFO

	// ── Auto show / hide + focusability ───────────────────────────────────────
	// Drive the window from (notificationsEnabled && stack length > 0): muted or
	// empty → hidden; enabled with cards → shown, focusable iff at least one
	// approval is VISIBLE (so the user can click or keyboard-open the card).
	// Visibility is edge-triggered in `reconcileWindowVisibility`; focusability +
	// the tray pending count + the visible-only dismiss timers are recomputed on
	// every change (they keep flowing even while muted).
	//
	// `cards.subscribe` gives the raw list; we re-derive the same groups the
	// markup uses (the `$:` reactive copies above can lag a manual subscriber, so
	// recompute here to keep the imperative side-effects in lock-step with what's
	// rendered).
	const unsubVisibility = cards.subscribe((list) => {
		const hasCards = list.length > 0;
		const approvalsList = list.filter(
			(c): c is Extract<NotifyCardModel, { kind: 'actionable' }> => c.kind === 'actionable'
		);
		const transientList = list.filter(
			(c): c is Extract<NotifyCardModel, { kind: 'info' | 'success' | 'error' }> =>
				c.kind !== 'actionable'
		);
		const visibleApprovalsList = approvalsList.slice(0, MAX_VISIBLE);
		const slots = Math.max(0, MAX_VISIBLE - visibleApprovalsList.length);
		const visibleTransientList = transientList.slice(0, slots);

		// Window visibility is gated on (notificationsEnabled && hasCards); the
		// flag listener recomputes the same predicate via `lastCardCount`.
		lastCardCount = list.length;
		reconcileWindowVisibility(notificationsEnabled, hasCards);
		void invokeOverlay('set_notify_overlay_focusable', {
			focusable: visibleApprovalsList.length > 0
		});
		// Start TTL timers only for transients that are CURRENTLY VISIBLE; clear
		// timers only for ids that left the store entirely. The stack keeps
		// flowing even while muted (window hidden) so un-muting shows the current
		// pending and the tray counter stays accurate.
		reconcileDismissTimers(visibleTransientList, new Set(list.map((c) => c.id)));
		// Tray pending count counts the WHOLE approval queue (incl. overflow) and
		// keeps flowing regardless of the mute state — only the WINDOW is gated.
		currentApprovalCount = approvalsList.length;
		syncPendingCount(currentApprovalCount);
	});

	onMount(() => {
		startNotifyStream();
		// Mirror the active app theme into this separate webview's document so the
		// card tokens resolve to the real palette (light + dark). Fire-and-forget.
		void startThemeMirror();
		// Mirror the tray's Show/Hide-notifications toggle so the window visibility
		// is gated on it. Fire-and-forget (no-op in a plain browser).
		void startNotificationsEnabledMirror();
		const ro = new ResizeObserver(() => {
			void reportHeight();
		});
		if (stackEl) ro.observe(stackEl);
		// Report once on mount so the window hugs an (initially empty) stack
		// even before the first ResizeObserver callback fires.
		void reportHeight();
		// Periodically re-assert the tray pending-approval count so a badge
		// stranded by a dropped IPC push or a missed cross-surface HitlResolved
		// converges to the real backlog without needing a fresh store mutation.
		pendingCountReconcileTimer = setInterval(reassertPendingCount, PENDING_COUNT_RECONCILE_MS);
		// Periodically drop actionable cards whose backing request is no longer
		// pending on the backend (self-heals a HITL card stranded by a missed
		// HitlResolved) — fail-open + scope-matched, see reconcileStaleCards.
		staleCardReconcileTimer = setInterval(() => void reconcileStaleCards(), STALE_CARD_RECONCILE_MS);
		return () => ro.disconnect();
	});

	onDestroy(() => {
		unsubVisibility();
		if (pendingCountReconcileTimer) clearInterval(pendingCountReconcileTimer);
		if (staleCardReconcileTimer) clearInterval(staleCardReconcileTimer);
		// Clear every live auto-dismiss timer so none fire after teardown.
		for (const handle of dismissTimers.values()) {
			clearTimeout(handle);
		}
		dismissTimers.clear();
		// Stop tracking app-theme-changed (no-op in a plain browser).
		unlistenTheme?.();
		// Stop tracking notifications-enabled-changed (no-op in a plain browser).
		unlistenNotificationsEnabled?.();
		stopNotifyStream();
	});
</script>

<!--
  The window is BOTTOM-anchored and grows UPWARD, so a flex-column's LAST child
  renders at the very bottom. Render order, TOP → BOTTOM:

    1. visibleTransient — info/success/error, NEWEST at the top (iterate
       reversed) so a fresh toast appears at the very top while the oldest
       transient sits just above the approvals. They auto-dismiss (timers start
       only once visible) and never bury an approval.
    2. The muted "+N more pending" chip (only when approvals overflow the cap),
       sitting just above the approval block.
    3. visibleApprovals — EARLIEST at the very bottom (iterate reversed): the
       newest approval renders just below the chip, the earliest at the bottom
       edge. Approvals never auto-dismiss; clicking opens the exact global prompt,
       while × permanently suppresses only that native correlation id. Resolving
       the bottom (earliest) one removes it so the next earliest sinks down.

  Total visible cards are capped at MAX_VISIBLE (4): approvals take priority,
  transient fills any leftover slots.
-->
<div class="notify-stack" bind:this={stackEl}>
	{#each [...visibleTransient].reverse() as card (card.id)}
		<div
			class="notify-slot"
			in:fly={{ y: 16, duration: 220, easing: cubicOut }}
			out:fly={{ y: 16, duration: 160, easing: cubicOut }}
			animate:flip={{ duration: 200, easing: cubicOut }}
			on:introstart={reportHeight}
			on:outroend={reportHeight}
		>
			<NotifyCard {card} on:open={handleOpen} on:dismiss={handleDismiss} />
		</div>
	{/each}

	{#if approvalsOverflow > 0}
		<div class="notify-pending-more" aria-live="polite" transition:fade={{ duration: 160 }}>
			+{approvalsOverflow} more pending
		</div>
	{/if}

	{#each [...visibleApprovals].reverse() as card (card.id)}
		<div
			class="notify-slot"
			in:fly={{ y: 16, duration: 220, easing: cubicOut }}
			out:fly={{ y: 16, duration: 160, easing: cubicOut }}
			animate:flip={{ duration: 200, easing: cubicOut }}
			on:introstart={reportHeight}
			on:outroend={reportHeight}
		>
			<NotifyCard
				{card}
				on:openInApp={handleOpenInApp}
				on:dismiss={handleDismiss}
			/>
		</div>
	{/each}
</div>

<style>
	/* The window is transparent + chrome-less. Defeat the opaque themed
	   background that the global app.html inline style paints on <html>/<body>
	   (`body, html { background: var(--bg-base) }`) — that is the "light dark
	   background box" that would otherwise sit behind the notifications. Only the
	   cards (and the ~5px gaps between them) must be visible. The transition there
	   is also killed so the first paint can't briefly flash --bg-base. */
	:global(html),
	:global(body) {
		width: 100%;
		height: 100%;
		margin: 0;
		padding: 0;
		overflow: hidden;
		background: transparent !important;
		transition: none !important;
	}

	:global(body::before),
	:global(body::after) {
		content: none !important;
		display: none !important;
	}

	:global(body > div) {
		background: transparent !important;
	}

	/* The window is sized + pinned bottom-right by Rust (resize_notify_overlay),
	   growing upward as the stack fills; this just fills its width. The ~5px gaps
	   between cards are the only non-card pixels captured in the reported height. */
	.notify-stack {
		display: flex;
		flex-direction: column;
		gap: 5px;
	}

	/* Transition/flip wrapper — purely a transition host (the keyed element the
	   fly + flip directives attach to). It must not introduce its own box model
	   beyond the card it wraps, so it stays display:contents-adjacent: a plain
	   block whose only child is the card. No padding/margin so the reported
	   stack height stays card + gaps only. */
	.notify-slot {
		display: block;
	}

	/* Visually subordinate "+N more pending" chip just above the approval block —
	   a quiet grouped indicator for the approvals hidden behind the visible cap,
	   not a full card. */
	.notify-pending-more {
		align-self: flex-start;
		font-size: 0.6875rem;
		line-height: 1;
		padding: 3px 8px;
		border-radius: 999px;
		color: var(--text-muted);
		background: color-mix(in srgb, var(--text-muted) 12%, transparent);
	}
</style>
