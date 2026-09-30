<script lang="ts">
	/**
	 * WorkbenchColumn — developer workbench pane rendered only in
	 * Developer Mode (`thread.display_mode === 'dev'`).
	 *
	 * Hosts the dev-flavored chrome that supplements the chat column:
	 *   - InteractiveTerminalPane (xterm.js) — Phase 1 (this file)
	 *   - InteractiveTerminalTabs (multi-session) — Phase 3
	 *   - PlanModePane (escalation gate) — Phase 5
	 *   - LiveToolInspector — Phase 5
	 *
	 * Phase 1: single-session view. The WorkbenchColumn listens to the
	 * realtime event stream for `InteractivePtyChunk` events and auto-
	 * switches to the most recent `session_id`. Multi-session tabs land
	 * in Phase 3.
	 *
	 * See docs/plans/2026-05-13-developer-mode-workbench.md.
	 */
	import { createEventDispatcher, onDestroy, onMount } from 'svelte';
	import { v2Events } from '$lib/realtime/v2-websocket';
	import InteractiveTerminalTabs from './InteractiveTerminalTabs.svelte';
	import DevWorkbenchLauncher from './DevWorkbenchLauncher.svelte';
	import DiffStrip from './DiffStrip.svelte';
	import PlanModePane from './PlanModePane.svelte';
	import LiveToolInspector from './LiveToolInspector.svelte';
	import { timedFetch } from '$lib/shared/fetch';
	import {
		normalizeInteractiveSessionSummary,
		scopedInteractiveSessionParams,
		type InteractiveSessionList,
		type LiveInteractiveSession
	} from '$lib/shell/interactiveSessionApi';

	/// Optional thread scope. A string = "this thread's sessions" (the embedded
	/// per-thread workbench in chat); null = UNSCOPED — all interactive sessions
	/// in the workspace, new sessions untagged (the standalone /dev page). The
	/// thread tag is just a grouping label on sessions, never a requirement.
	export let threadId: string | null = null;
	export let active = true;
	/** Plan-mode flag from the thread; surfaces the approval gate UI. */
	export let planMode = false;
	export let placement: 'side' | 'main' = 'side';
	export let startedSession: { id: string; program: string | null; nonce: number } | null = null;
	export let selectedSessionId: string | null = null;
	export let focusNonce = 0;

	const dispatch = createEventDispatcher<{
		sessionschange: { count: number; activeSessionId: string | null };
		started: { sessionId: string; program: string | null };
		focus: void;
	}>();

	let sessions: LiveInteractiveSession[] = [];
	let activeSessionId: string | null = null;
	let unsubscribeEvents: (() => void) | null = null;
	let unsubscribeSessionClosed: (() => void) | null = null;
	let unsubscribeSessionTouched: (() => void) | null = null;
	let handledStartedSessionNonce: number | null = null;
	let appliedSelectedSessionId: string | null = null;
	let emittedSessionsSignature = '';
	let closingAll = false;
	let closeAllError: string | null = null;
	let sessionError: string | null = null;
	let loadedThreadId: string | null = null;
	let refreshGeneration = 0;
	let mounted = false;
	let reconcileTimer: ReturnType<typeof setInterval> | null = null;
	let auxDrawerOpen = false;
	// Overflow menu (mobile only) — collapses Inspect + Close-all into a
	// kebab dropdown because they don't fit alongside the launcher on a
	// phone-width header. Desktop renders both buttons inline as before.
	let overflowMenuOpen = false;
	let overflowMenuEl: HTMLDivElement | null = null;

	function toggleOverflowMenu(): void {
		overflowMenuOpen = !overflowMenuOpen;
	}

	function closeOverflowMenu(): void {
		overflowMenuOpen = false;
	}

	function handleOverflowDocClick(event: MouseEvent): void {
		if (!overflowMenuOpen) return;
		const target = event.target as Node | null;
		if (overflowMenuEl && target && overflowMenuEl.contains(target)) return;
		overflowMenuOpen = false;
	}

	function handleOverflowKey(event: KeyboardEvent): void {
		if (event.key === 'Escape' && overflowMenuOpen) {
			event.preventDefault();
			overflowMenuOpen = false;
		}
	}

	let bodyEl: HTMLElement | null = null;
	let drawerWidthPx: number | null = null;
	let dragging = false;
	let dragStartX = 0;
	let dragStartWidthPx = 0;
	let dragBodyWidth = 0;

	const DRAWER_MIN_PX = 320;
	const DRAWER_MAX_RATIO = 0.75; // up to 3/4 of the workbench body

	function onResizeStart(event: PointerEvent): void {
		if (!bodyEl || !auxDrawerOpen) return;
		dragging = true;
		dragBodyWidth = bodyEl.clientWidth;
		dragStartX = event.clientX;
		// If width hasn't been customised yet, seed from the rendered drawer
		// so the drag feels continuous instead of snapping to the CSS default.
		const drawerEl = (event.currentTarget as HTMLElement).parentElement;
		dragStartWidthPx = drawerWidthPx ?? (drawerEl?.getBoundingClientRect().width ?? 720);
		(event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
		event.preventDefault();
	}

	function onResizeMove(event: PointerEvent): void {
		if (!dragging) return;
		// Drawer is anchored to the right edge; dragging the handle LEFT
		// (negative deltaX from start) GROWS the drawer.
		const delta = dragStartX - event.clientX;
		const maxPx = Math.max(DRAWER_MIN_PX, Math.floor(dragBodyWidth * DRAWER_MAX_RATIO));
		drawerWidthPx = Math.max(DRAWER_MIN_PX, Math.min(maxPx, dragStartWidthPx + delta));
	}

	function onResizeEnd(event: PointerEvent): void {
		if (!dragging) return;
		dragging = false;
		try {
			(event.currentTarget as HTMLElement).releasePointerCapture(event.pointerId);
		} catch {
			/* Pointer may have already been released by the browser. */
		}
	}

	$: {
		const signature = `${activeSessionId ?? 'none'}:${sessions.map((session) => session.id).join(',')}`;
		if (signature !== emittedSessionsSignature) {
			emittedSessionsSignature = signature;
			dispatch('sessionschange', { count: sessions.length, activeSessionId });
		}
	}

	function upsertSession(
		id: string,
		program: string | null | undefined,
		ts: number,
		patch: Partial<LiveInteractiveSession> = {}
	): void {
		const existing = sessions.find((s) => s.id === id);
		if (existing) {
			sessionError = null;
			existing.lastSeenMs = ts;
			if (program && !existing.program) existing.program = program;
			Object.assign(existing, patch);
			sessions = sessions; // trigger reactivity
		} else {
			sessionError = null;
			sessions = [
				...sessions,
				{
					id,
					program: program ?? null,
					uiThreadId: threadId,
					workingDir: null,
					createdAtMs: ts,
					lastOutputAtMs: null,
					lastInputAtMs: null,
					replayStartOffset: 0,
					replayEndOffset: 0,
					alive: true,
					exitCode: null,
					lastSeenMs: ts,
					...patch
				}
			];
			// Auto-focus the freshly-seen session so the user lands on the one
			// the agent just spawned — but only in the thread-scoped embed (or
			// when nothing is focused yet). The unscoped /dev workbench sees
			// EVERY thread's PTY chunks; stealing focus there would yank the
			// user's keystrokes into whatever a background agent just opened.
			if (threadId !== null || activeSessionId === null) {
				activeSessionId = id;
			}
		}
	}

	$: if (startedSession && startedSession.nonce !== handledStartedSessionNonce) {
		handledStartedSessionNonce = startedSession.nonce;
		upsertSession(startedSession.id, startedSession.program, Date.now());
		activeSessionId = startedSession.id;
	}

	$: if (
		selectedSessionId &&
		selectedSessionId !== appliedSelectedSessionId &&
		sessions.some((session) => session.id === selectedSessionId)
	) {
		appliedSelectedSessionId = selectedSessionId;
		activeSessionId = selectedSessionId;
	}

	async function refreshLiveSessions(
		targetThreadId = threadId,
		generation = refreshGeneration,
		reconcile = false
	): Promise<void> {
		try {
			const response = await timedFetch(
				`/api/magician/v2/interactive-sessions?${scopedInteractiveSessionParams(targetThreadId).toString()}`
			);
			if (!response.ok) return;
			const payload = (await response.json().catch(() => null)) as InteractiveSessionList | null;
			if (generation !== refreshGeneration || targetThreadId !== threadId) return;
			const normalizedSessions = (payload?.sessions ?? [])
				.filter((session) => Boolean(session.session_id))
				.map((session) => normalizeInteractiveSessionSummary(session));
			if (reconcile) {
				const previousActiveSessionId = activeSessionId;
				sessions = normalizedSessions;
				if (
					previousActiveSessionId &&
					normalizedSessions.some((session) => session.id === previousActiveSessionId)
				) {
					activeSessionId = previousActiveSessionId;
				} else {
					activeSessionId = normalizedSessions[normalizedSessions.length - 1]?.id ?? null;
				}
				return;
			}
			for (const normalized of normalizedSessions) {
				upsertSession(normalized.id, normalized.program, normalized.lastSeenMs, normalized);
			}
		} catch {
			/* Best-effort hydration; live chunks still create tabs. */
		}
	}

	async function closeAllSessions(): Promise<void> {
		if (closingAll || sessions.length === 0) return;
		closingAll = true;
		closeAllError = null;
		sessionError = null;
		const targets = [...sessions];
		const closedIds = new Set<string>();
		try {
			await Promise.all(
				targets.map(async (session) => {
					const params = scopedInteractiveSessionParams();
					const response = await timedFetch(
						`/api/magician/v2/interactive-sessions/${encodeURIComponent(session.id)}?${params.toString()}`,
						{ method: 'DELETE' }
					).catch(() => null);
					if (response?.ok) {
						closedIds.add(session.id);
						window.dispatchEvent(
							new CustomEvent('magician:interactive-session-closed', {
								detail: { sessionId: session.id }
							})
						);
					}
				})
			);
			sessions = sessions.filter((session) => !closedIds.has(session.id));
			if (activeSessionId && closedIds.has(activeSessionId)) {
				activeSessionId = sessions[sessions.length - 1]?.id ?? null;
			}
			const failedCount = targets.length - closedIds.size;
			if (failedCount > 0) {
				closeAllError = `${failedCount} session${failedCount === 1 ? '' : 's'} could not be closed.`;
			}
		} finally {
			closingAll = false;
		}
	}

	function removeSession(sessionId: string): void {
		sessionError = null;
		sessions = sessions.filter((s) => s.id !== sessionId);
		if (activeSessionId === sessionId) {
			activeSessionId = sessions[sessions.length - 1]?.id ?? null;
		}
	}

	function loadThreadSessions(nextThreadId: string | null): void {
		loadedThreadId = nextThreadId;
		refreshGeneration += 1;
		sessions = [];
		activeSessionId = null;
		appliedSelectedSessionId = null;
		closeAllError = null;
		sessionError = null;
		void refreshLiveSessions(nextThreadId, refreshGeneration, true);
	}

	$: if (mounted && threadId !== loadedThreadId) {
		loadThreadSessions(threadId);
	}

	onMount(() => {
		mounted = true;
		loadThreadSessions(threadId);
		reconcileTimer = setInterval(() => {
			if (mounted) void refreshLiveSessions(threadId, refreshGeneration, true);
		}, 20_000);
		const handleClosed = (event: Event) => {
			const detail = (event as CustomEvent<{ sessionId?: string }>).detail;
			if (detail?.sessionId) removeSession(detail.sessionId);
		};
		window.addEventListener('magician:interactive-session-closed', handleClosed);
		unsubscribeSessionClosed = () => {
			window.removeEventListener('magician:interactive-session-closed', handleClosed);
		};
		const handleTouched = (event: Event) => {
			const detail = (event as CustomEvent<{
				sessionId?: string;
				kind?: string;
				timestampMs?: number;
			}>).detail;
			if (!detail?.sessionId) return;
			const timestamp = detail.timestampMs ?? Date.now();
			if (detail.kind === 'input') {
				if (sessions.some((session) => session.id === detail.sessionId)) {
					upsertSession(detail.sessionId, null, timestamp, { lastInputAtMs: timestamp });
				}
			}
		};
		window.addEventListener('magician:interactive-session-touched', handleTouched);
		unsubscribeSessionTouched = () => {
			window.removeEventListener('magician:interactive-session-touched', handleTouched);
		};
		unsubscribeEvents = v2Events.subscribe((events) => {
			for (const event of events) {
				if (event.event_type !== 'InteractivePtyChunk') continue;
				const data = event.data;
				if (!data || !data.session_id) continue;
				// Thread-scoped: only this thread's chunks. Unscoped: take all.
				if (threadId !== null && data.ui_thread_id !== threadId) continue;
				const timestamp = data.timestamp_ms ?? Date.now();
				upsertSession(data.session_id, data.program ?? null, timestamp, {
					lastOutputAtMs: timestamp,
					uiThreadId: data.ui_thread_id ?? threadId,
					replayEndOffset: typeof data.offset_end === 'number' ? data.offset_end : undefined
				});
			}
		});
	});

	onDestroy(() => {
		mounted = false;
		if (reconcileTimer) clearInterval(reconcileTimer);
		unsubscribeEvents?.();
		unsubscribeSessionClosed?.();
		unsubscribeSessionTouched?.();
	});

	// `activeSessionId` is bound to the InteractiveTerminalTabs component
	// (it surfaces user-driven tab switches via the `select` event).
	// We don't need a derived `activeSession` reference here because
	// the tabs component is the source of truth for which pane renders.
</script>

<svelte:window on:click={handleOverflowDocClick} on:keydown={handleOverflowKey} />

<aside
	class="workbench"
	class:workbench--collapsed={!active}
	class:workbench--main={placement === 'main'}
	aria-label="Developer Mode workbench"
>
	<header class="workbench__head">
		<div class="workbench__head-left">
			<span class="workbench__title">Workbench</span>
			<span class="workbench__thread">{threadId ? `#${threadId}` : 'all sessions'}</span>
			{#if sessions.length > 0}
				<span class="workbench__count">{sessions.length} live</span>
			{/if}
		</div>
		<div class="workbench__head-launcher">
			<DevWorkbenchLauncher
				{threadId}
				on:started={(e) => dispatch('started', e.detail)}
				on:focus={() => dispatch('focus')}
			/>
		</div>
		{#if sessions.length > 0}
			<!-- Desktop layout: both buttons inline. Hidden on mobile via @media. -->
			<button
				type="button"
				class="workbench__inspect workbench__head-action"
				class:workbench__inspect--on={auxDrawerOpen}
				aria-pressed={auxDrawerOpen}
				aria-expanded={auxDrawerOpen}
				title="Toggle inspector (changed files, in-flight tools, plan-mode gate)"
				on:click={() => auxDrawerOpen = !auxDrawerOpen}
			>
				Inspect {auxDrawerOpen ? '◂' : '▸'}
			</button>
			<button
				type="button"
				class="workbench__close-all workbench__head-action"
				disabled={closingAll}
				on:click={() => void closeAllSessions()}
			>
				Close all
			</button>

			<!-- Mobile layout: kebab dropdown. Hidden on desktop via @media. -->
			<div class="workbench__overflow" bind:this={overflowMenuEl}>
				<button
					type="button"
					class="workbench__overflow-btn"
					aria-label="More workbench actions"
					aria-expanded={overflowMenuOpen}
					aria-haspopup="menu"
					on:click|stopPropagation={toggleOverflowMenu}
				>
					<svg viewBox="0 0 24 24" width="18" height="18" fill="currentColor" aria-hidden="true">
						<circle cx="12" cy="5" r="1.6" />
						<circle cx="12" cy="12" r="1.6" />
						<circle cx="12" cy="19" r="1.6" />
					</svg>
				</button>
				{#if overflowMenuOpen}
					<div class="workbench__overflow-menu" role="menu">
						<button
							type="button"
							role="menuitem"
							class="workbench__overflow-item"
							on:click={() => { auxDrawerOpen = !auxDrawerOpen; closeOverflowMenu(); }}
						>
							{auxDrawerOpen ? 'Hide inspector' : 'Show inspector'}
						</button>
						<button
							type="button"
							role="menuitem"
							class="workbench__overflow-item workbench__overflow-item--danger"
							disabled={closingAll}
							on:click={() => { void closeAllSessions(); closeOverflowMenu(); }}
						>
							{closingAll ? 'Closing…' : 'Close all sessions'}
						</button>
					</div>
				{/if}
			</div>
		{/if}
	</header>

	<section class="workbench__body" bind:this={bodyEl}>
		{#if closeAllError}
			<p class="workbench__error">{closeAllError}</p>
		{/if}
		{#if sessionError}
			<p class="workbench__error">{sessionError}</p>
		{/if}
		{#if sessions.length > 0}
			<!-- Full-bleed the CLI so it runs edge-to-edge inside the
			     workbench body. Cancels the parent's `padding: 16px`
			     so the terminal sits flush with the header above and
			     the workbench edges. -->
			<div class="workbench__terminal-bleed">
				<InteractiveTerminalTabs
					sessions={sessions}
					activeSessionId={activeSessionId}
					{focusNonce}
					on:select={(e) => { activeSessionId = e.detail.sessionId; }}
					on:closed={(e) => {
						removeSession(e.detail.sessionId);
					}}
					on:closeerror={(e) => {
						sessionError = e.detail.message;
					}}
				/>
			</div>
			<!-- Aux panes (changed files, plan-mode gate, in-flight tools)
			     live in a right-side drawer that slides over the terminal
			     when the "Inspect" header button is on. The terminal keeps
			     the full workbench area when closed; the drawer never
			     steals vertical space. -->
			<aside
				class="workbench__drawer"
				class:workbench__drawer--open={auxDrawerOpen}
				class:workbench__drawer--dragging={dragging}
				style={drawerWidthPx !== null ? `width: ${drawerWidthPx}px;` : ''}
				aria-hidden={!auxDrawerOpen}
				aria-label="Workbench inspector"
			>
				<!-- svelte-ignore a11y_no_static_element_interactions -->
				<div
					class="workbench__drawer-resizer"
					role="separator"
					aria-orientation="vertical"
					aria-label="Resize inspector"
					tabindex="-1"
					on:pointerdown={onResizeStart}
					on:pointermove={onResizeMove}
					on:pointerup={onResizeEnd}
					on:pointercancel={onResizeEnd}
				></div>
				<header class="workbench__drawer-head">
					<span class="workbench__drawer-title">Inspector</span>
					<button
						type="button"
						class="workbench__drawer-close"
						title="Close inspector"
						aria-label="Close inspector"
						on:click={() => auxDrawerOpen = false}
					>×</button>
				</header>
				<div class="workbench__drawer-body">
					<LiveToolInspector threadId={threadId} />
					{#if activeSessionId}
						<!-- `view="split"` opts into side-by-side rendering;
						     DiffStrip auto-falls back to unified when the
						     drawer is narrower than `splitStackBelowPx`
						     (default 600px), so resizing below ~600px
						     transparently collapses to a single column. -->
						<DiffStrip sessionId={activeSessionId} view="split" />
					{/if}
					<PlanModePane threadId={threadId} planModeEnabled={planMode} />
				</div>
			</aside>
		{:else}
			<div class="workbench__placeholder">
				<p>No live session.</p>
				<p class="workbench__hint">
					Pick a CLI and press <strong>Start</strong> in the header to spin one up.
				</p>
			</div>
		{/if}
	</section>
</aside>

<style>
	.workbench {
		display: flex;
		flex-direction: column;
		border-left: 1px solid var(--border-soft, rgba(0, 0, 0, 0.1));
		background: var(--bg-base, #fafafa);
		min-width: 360px;
		max-width: 720px;
		flex: 1 1 480px;
		overflow: hidden;
	}

	.workbench--main {
		width: 100%;
		min-width: 0;
		max-width: none;
		flex: 1 1 auto;
		border-left: 0;
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.1));
		border-radius: var(--radius-lg, 14px);
		/* Smaller shadow: prior `var(--shadow-lg, 0 24px 48px -20px)`
		   blooms ~28px horizontally on each side and reads as the
		   workbench being wider than its 1320px wrapper. A tighter
		   shadow (12px blur, 4px y) keeps the elevation cue inside
		   the wrapper. */
		box-shadow: 0 4px 12px rgba(0, 0, 0, 0.12);
	}

	.workbench--collapsed {
		display: none;
	}

	.workbench__head {
		display: flex;
		align-items: center;
		gap: 12px;
		padding: 8px 12px;
		border-bottom: 1px solid var(--border-soft, rgba(0, 0, 0, 0.08));
		font-family: var(--font-primary);
		font-size: 12px;
		color: var(--text-muted, #888);
	}

	.workbench__overflow {
		position: relative;
		display: none;
	}

	.workbench__overflow-btn {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 30px;
		height: 28px;
		padding: 0;
		background: transparent;
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.14));
		border-radius: 6px;
		color: var(--text-secondary, #444);
		cursor: pointer;
	}

	.workbench__overflow-btn:active {
		background: var(--bg-soft, rgba(0, 0, 0, 0.05));
	}

	.workbench__overflow-menu {
		position: absolute;
		top: calc(100% + 6px);
		right: 0;
		min-width: 180px;
		display: flex;
		flex-direction: column;
		padding: 4px;
		background: var(--bg-elevated, var(--bg-card, #fff));
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.1));
		border-radius: 8px;
		box-shadow: 0 8px 24px rgba(0, 0, 0, 0.16);
		z-index: 60;
	}

	.workbench__overflow-item {
		padding: 8px 10px;
		text-align: left;
		background: transparent;
		border: none;
		border-radius: 6px;
		color: var(--text-primary, #1a1a1a);
		font: 500 13px var(--font-primary);
		cursor: pointer;
	}

	.workbench__overflow-item:hover:not(:disabled) {
		background: var(--bg-soft, rgba(0, 0, 0, 0.05));
	}

	.workbench__overflow-item:disabled {
		opacity: 0.55;
		cursor: not-allowed;
	}

	.workbench__overflow-item--danger:hover:not(:disabled) {
		color: var(--color-error, #c0392b);
	}

	.workbench__head-left {
		display: inline-flex;
		align-items: baseline;
		gap: 8px;
		flex: 0 0 auto;
	}

	.workbench__head-launcher {
		flex: 1 1 auto;
		min-width: 0;
	}

	/* Launcher lives in the header, so its picker / sessions popups
	   must drop DOWN (default placement is `bottom: 100%`, which would
	   render them above the page and clip them). */
	.workbench__head-launcher :global(.dev-launcher__picker),
	.workbench__head-launcher :global(.dev-launcher__sessions) {
		bottom: auto;
		top: calc(100% + 8px);
	}

	.workbench__title {
		font-weight: 600;
		color: var(--text-primary, #1a1a1a);
		text-transform: uppercase;
		letter-spacing: 0.06em;
		font-size: 10.5px;
	}

	.workbench__thread {
		font-family: var(--font-mono);
		font-size: 11px;
	}

	.workbench__count {
		font-family: var(--font-mono);
		font-size: 10.5px;
		color: var(--accent-primary, #c2502a);
	}

	.workbench__close-all {
		height: 22px;
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.14));
		border-radius: 6px;
		background: var(--bg-elevated, #fff);
		color: var(--text-secondary, #444);
		font: 600 10.5px var(--font-primary);
		padding: 0 8px;
		cursor: pointer;
	}

	.workbench__close-all:hover:not(:disabled) {
		border-color: var(--danger, #b42318);
		color: var(--danger, #b42318);
	}

	.workbench__close-all:disabled {
		cursor: not-allowed;
		opacity: 0.55;
	}

	.workbench__body {
		flex: 1 1 auto;
		display: flex;
		flex-direction: column;
		gap: 12px;
		/* `overflow: hidden` (not auto) so flex distribution is the
		   only sizing path. With `auto`, a child's natural height
		   exceeding the container would trigger a scroll instead of
		   forcing the terminal to shrink — visually that read as
		   "workbench has fixed height that doesn't adjust". With
		   `hidden` + the `min-height: 0` chain through terminal-pane,
		   the terminal-bleed wrapper grows/shrinks correctly. */
		overflow: hidden;
		min-height: 0;
		padding: 16px;
		/* Anchor for the absolute-positioned inspector drawer that
		   slides in from the right over the terminal. */
		position: relative;
	}

	/* Full-bleed wrapper for the CLI terminal: cancels the workbench
	   body's padding on all four sides so the terminal sits flush
	   with the header above, the workbench edges left/right, and the
	   bottom of the workbench card. (Aux panes used to live below
	   the terminal and needed the bottom padding for spacing; they
	   now live in the inspector drawer overlay, so the bottom strip
	   was dead space.) Holds the InteractiveTerminalTabs flex chain
	   together via flex inheritance. */
	.workbench__terminal-bleed {
		display: flex;
		flex-direction: column;
		flex: 1 1 auto;
		min-height: 0;
		margin: -16px;
	}

	/* Inspector drawer: absolutely-positioned panel that slides in
	   from the right over the terminal. Off-screen by default
	   (translated 100% right); slides to 0 when `--open`. Keeps the
	   terminal at full size regardless of how much aux content
	   exists; aux content scrolls inside the drawer body. */
	.workbench__drawer {
		position: absolute;
		top: 0;
		right: 0;
		bottom: 0;
		/* Default starting width; the user can drag the left handle to
		   resize up to 75% of the workbench body (DRAWER_MAX_RATIO).
		   Inline `style="width: ...px"` from the drag state takes
		   precedence over this default when the user has resized. */
		width: min(720px, 78%);
		display: flex;
		flex-direction: column;
		background: var(--bg-elevated, #fff);
		border-left: 1px solid var(--border-soft, rgba(0, 0, 0, 0.1));
		box-shadow: -12px 0 24px -16px rgba(0, 0, 0, 0.18);
		transform: translateX(100%);
		transition: transform 180ms ease-out, width 0s; /* width changes are user-driven; no transition */
		z-index: 5;
		pointer-events: none;
		visibility: hidden;
	}

	/* Suspend the open/close transform-transition while the user is
	   actively dragging the resizer so each pointermove repaints
	   immediately without queueing animation frames. */
	.workbench__drawer--dragging {
		transition: none;
		user-select: none;
	}

	.workbench__drawer-resizer {
		position: absolute;
		top: 0;
		left: 0;
		bottom: 0;
		width: 6px;
		margin-left: -3px;
		cursor: col-resize;
		background: transparent;
		touch-action: none;
		z-index: 1;
	}

	.workbench__drawer-resizer:hover,
	.workbench__drawer--dragging .workbench__drawer-resizer {
		background: color-mix(in srgb, var(--accent-primary, #c2502a) 42%, transparent);
	}

	.workbench__drawer--open {
		transform: translateX(0);
		pointer-events: auto;
		visibility: visible;
	}

	.workbench__drawer-head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: 10px 12px;
		border-bottom: 1px solid var(--border-soft, rgba(0, 0, 0, 0.08));
		font-family: var(--font-primary);
	}

	.workbench__drawer-title {
		font-size: 10.5px;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.06em;
		color: var(--text-primary, #1a1a1a);
	}

	.workbench__drawer-close {
		width: 24px;
		height: 24px;
		border: 0;
		border-radius: 6px;
		background: transparent;
		color: var(--text-muted, #777);
		cursor: pointer;
		font-size: 18px;
		line-height: 1;
	}

	.workbench__drawer-close:hover {
		background: var(--bg-soft, rgba(0, 0, 0, 0.06));
		color: var(--text-primary, #1a1a1a);
	}

	.workbench__drawer-body {
		flex: 1 1 auto;
		min-height: 0;
		overflow-y: auto;
		padding: 12px 14px;
		display: flex;
		flex-direction: column;
		gap: 10px;
	}

	.workbench__inspect {
		height: 22px;
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.14));
		border-radius: 6px;
		background: var(--bg-elevated, #fff);
		color: var(--text-secondary, #444);
		font: 600 10.5px var(--font-primary);
		padding: 0 8px;
		cursor: pointer;
	}

	.workbench__inspect:hover {
		border-color: var(--accent-primary, #c2502a);
		color: var(--accent-primary, #c2502a);
	}

	.workbench__inspect--on {
		border-color: var(--accent-primary, #c2502a);
		background: var(--accent-primary, #c2502a);
		color: var(--accent-contrast, #fff);
	}

	.workbench__placeholder {
		margin: auto;
		text-align: center;
		color: var(--text-muted, #888);
		font-family: var(--font-primary);
		font-size: 13px;
		max-width: 320px;
	}

	.workbench__placeholder p {
		margin: 0 0 8px;
	}

	.workbench__hint {
		font-size: 12px;
		opacity: 0.85;
	}

	.workbench__error {
		margin: 0;
		border: 1px solid color-mix(in srgb, var(--danger, #b42318) 28%, transparent);
		border-radius: 7px;
		background: color-mix(in srgb, var(--danger, #b42318) 7%, var(--bg-elevated, #fff));
		color: var(--danger, #b42318);
		font: 12px var(--font-primary);
		padding: 8px 10px;
	}

	/* ── Mobile overrides ──────────────────────────────────────────────
	   Must live at the end of the <style> block so they win the source-
	   order tie against the desktop base rules above. Earlier versions
	   placed this @media block higher up, which silently let the
	   later-in-source desktop `.workbench__head-left { display:
	   inline-flex }` and `.workbench__head-launcher { flex: 1 1 auto }`
	   rules clobber the mobile hides — the title + thread chip stayed
	   visible on phone width. */
	@media (max-width: 767px) {
		/* Smaller corner radius on phone — the desktop 14px reads as
		   over-rounded against the workbench filling most of the
		   viewport. 6px keeps a hint of softness without the "floating
		   card" exaggeration. Tighter shadow for the same reason. */
		.workbench--main {
			border-radius: 6px;
			box-shadow: 0 2px 6px rgba(0, 0, 0, 0.08);
		}

		/* Phone width: hide decorative chips, let the launcher own the
		   horizontal space, and pin the kebab to the top-right of the
		   header on the SAME row as the cwd input. The launcher wraps
		   its own controls internally (cwd full-width, then CLI +
		   buttons on a second line) — that wrapping must happen INSIDE
		   the launcher, not on the outer header, otherwise the kebab
		   gets bumped to a new row below the launcher's tall block.
		   Hence: outer .workbench__head stays `flex-wrap: nowrap` with
		   `align-items: flex-start` so the kebab pins at the top while
		   the launcher's two-row stack hangs below it. */
		.workbench__head {
			flex-wrap: nowrap;
			align-items: flex-start;
			gap: 8px;
			padding: 6px 8px;
		}

		.workbench__head-left {
			display: none;
		}

		.workbench__head-launcher {
			flex: 1 1 auto;
			min-width: 0;
		}

		.workbench__head-action {
			display: none;
		}

		.workbench__overflow {
			display: block;
			flex: 0 0 auto;
			/* Top-aligned by the parent's `align-items: flex-start` so
			   the kebab sits next to the cwd row, not centred against
			   the full two-row launcher block. */
		}
	}
</style>
