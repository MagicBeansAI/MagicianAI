<script lang="ts">
	/**
	 * ContextPill — floating glass pill anchored top-centre of the chat
	 * canvas. Shows the active thread + session title and exposes the
	 * history drawer + a more-actions menu (inspect / clear / archive).
	 *
	 * Replaces the role of the legacy <ChatSessionSidebar /> column header
	 * in /chat and /t/[name].
	 */
	import { createEventDispatcher } from 'svelte';

	export let threadId: string | null = null;
	export let sessionTitle: string | null = null;
	export let updatedAt: number | null = null;
	export let isReadOnly = false;
	export let canClear = false;
	/**
	 * Docked variant — the pill renders as a normal flow child (of the composer
	 * shell) instead of floating over the chat canvas. Drops `position: fixed`,
	 * the top/left anchoring, the z-index, and the `pointer-events: none`
	 * container guard.
	 *
	 * That guard exists only for the FLOATING pill: it shares a y-band with the
	 * `/t/[name]` thread-bar, so a long session title could otherwise swallow
	 * clicks on the tabs beneath it. A docked pill has no such overlap, so it
	 * takes pointer events normally.
	 */
	export let inline = false;
	/** Whether this conversation has text that can seed a VibeDev build. */
	export let canBuild = false;

	const dispatch = createEventDispatcher<{
		'open-history': void;
		'open-more': void;
		build: void;
		clear: void;
		archive: void;
		delete: void;
		'return-to-active': void;
		'new-session': void;
	}>();

	let moreMenuOpen = false;
	let moreMenuEl: HTMLDivElement | null = null;

	function toggleMore(): void {
		moreMenuOpen = !moreMenuOpen;
		if (moreMenuOpen) dispatch('open-more');
	}

	function handleDocumentClick(event: MouseEvent): void {
		if (!moreMenuOpen) return;
		const target = event.target as Node | null;
		if (moreMenuEl && target && moreMenuEl.contains(target)) return;
		moreMenuOpen = false;
	}

	function formatRelative(ts: number | null): string {
		if (ts == null) return '';
		const diffMs = Date.now() - ts;
		if (diffMs < 0) return 'just now';
		const minutes = Math.round(diffMs / 60000);
		if (minutes < 1) return 'just now';
		if (minutes < 60) return `${minutes}m ago`;
		const hours = Math.round(minutes / 60);
		if (hours < 24) return `${hours}h ago`;
		const days = Math.round(hours / 24);
		return `${days}d ago`;
	}

	$: relativeTime = formatRelative(updatedAt);
</script>

<svelte:window on:click={handleDocumentClick} />

<div class="ctx-pill" class:ctx-pill--inline={inline} role="status">
	<!--
	  The thread + session name is a shortcut into the history drawer, but ONLY
	  in the docked variant. The floating pill deliberately takes no pointer
	  events (see `.ctx-pill`): it shares a y-band with the `/t/[name]`
	  thread-bar, and a wide clickable name label is exactly what would swallow
	  clicks on the tabs beneath it. Docked, there is nothing underneath.
	-->
	{#if inline}
		<button
			type="button"
			class="ctx-identity"
			on:click={() => dispatch('open-history')}
			aria-label="Open sessions and threads"
			title={`${sessionTitle ?? (threadId ? 'New session' : 'No session')} — open sessions and threads`}
		>
			{#if threadId}<span class="thread">#{threadId}</span><span class="sep">/</span>{/if}
			<span class="title">
				{sessionTitle ?? (threadId ? 'New session' : 'No session')}
				{#if relativeTime}<em class="ts">· {relativeTime}</em>{/if}
			</span>
		</button>
	{:else}
		{#if threadId}<span class="thread">#{threadId}</span><span class="sep">/</span>{/if}
		<span class="title" title={sessionTitle ?? ''}>
			{sessionTitle ?? (threadId ? 'New session' : 'No session')}
			{#if relativeTime}<em class="ts">· {relativeTime}</em>{/if}
		</span>
	{/if}

	{#if isReadOnly}
		<button
			type="button"
			class="readonly-pill"
			on:click={() => dispatch('return-to-active')}
			title="Viewing archived session — return to active"
		>archived</button>
	{/if}

	{#if threadId}
		<button
			type="button"
			class="ctx-btn"
			title="New session in this thread"
			aria-label="New session"
			on:click={() => dispatch('new-session')}
			disabled={isReadOnly}
		>
			<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
				<line x1="12" y1="5" x2="12" y2="19"/>
				<line x1="5" y1="12" x2="19" y2="12"/>
			</svg>
		</button>
	{/if}

	<button
		type="button"
		class="ctx-btn"
		title="Sessions and threads"
		aria-label="Open history"
		on:click={() => dispatch('open-history')}
	>
		<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
			<circle cx="12" cy="12" r="10"/>
			<polyline points="12 6 12 12 16 14"/>
		</svg>
	</button>

	<div class="more-wrap" bind:this={moreMenuEl}>
		<button
			type="button"
			class="ctx-btn"
			title="More actions"
			aria-label="More actions"
			aria-haspopup="menu"
			aria-expanded={moreMenuOpen}
			on:click={toggleMore}
		>
			<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
				<circle cx="5" cy="12" r="1.4"/>
				<circle cx="12" cy="12" r="1.4"/>
				<circle cx="19" cy="12" r="1.4"/>
			</svg>
		</button>
		{#if moreMenuOpen}
			<div class="more-menu" role="menu">
				{#if canBuild}
					<button
						type="button"
						class="menu-item"
						role="menuitem"
						on:click={() => { moreMenuOpen = false; dispatch('build'); }}
					>✦ Build in VibeDev</button>
				{/if}
				<button
					class="menu-item"
					role="menuitem"
					disabled={!canClear || isReadOnly}
					on:click={() => { moreMenuOpen = false; dispatch('clear'); }}
				>Clear chat</button>
				<button
					class="menu-item"
					role="menuitem"
					disabled={isReadOnly}
					on:click={() => { moreMenuOpen = false; dispatch('archive'); }}
				>Archive session</button>
				<button
					class="menu-item menu-item--danger"
					role="menuitem"
					disabled={isReadOnly}
					on:click={() => { moreMenuOpen = false; dispatch('delete'); }}
				>Delete session</button>
				{#if isReadOnly}
					<button
						class="menu-item"
						role="menuitem"
						on:click={() => { moreMenuOpen = false; dispatch('return-to-active'); }}
					>Return to active</button>
				{/if}
			</div>
		{/if}
	</div>
</div>

<style>
	.ctx-pill {
		/* Fixed-to-viewport so the pill stays put when the message stream
		   scrolls. `top: 54px` clears the 48px top bar with a hair of
		   breathing room. The pill shares the y-band with the /t/[name]
		   thread-bar, so the pill ITSELF is click-transparent — only its
		   interactive children take pointer events — to guarantee a wide
		   pill (long titles) can never swallow clicks on the tabs beneath. */
		pointer-events: none;
		position: fixed;
		top: 54px;
		left: 50%;
		transform: translateX(-50%);
		z-index: 50;
		display: inline-flex;
		align-items: center;
		gap: 8px;
		padding: 3px 6px 3px 14px;
		background: var(--bg-elevated, rgba(255, 255, 255, 0.85));
		color: var(--text-primary, #1a1a1a);
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.08));
		border-radius: 999px;
		backdrop-filter: blur(16px) saturate(140%);
		-webkit-backdrop-filter: blur(16px) saturate(140%);
		box-shadow: var(--shadow-md, 0 4px 14px rgba(0, 0, 0, 0.1));
		max-width: calc(100% - 48px);
		font-family: var(--font-primary);
		font-size: 11.5px;
	}

	/* ─── Docked variant ───
	   A normal flow child of the composer shell. Spans the full row width so
	   the title has room; a host that fills the composer's `dock-actions`
	   slot (the Tauri HUD: theme + expand) takes the space it needs on the
	   right and the pill flexes down around it.

	   Quiet at rest, exposed on hover: the title and thread chip stay
	   readable — you always know which session you're typing into — while
	   the action buttons fade in only when the composer is hovered or
	   focused. They keep their layout box at rest (opacity, not display), so
	   revealing them never shifts the row. */
	.ctx-pill--inline {
		pointer-events: auto;
		position: static;
		top: auto;
		left: auto;
		transform: none;
		z-index: auto;
		display: flex;
		/* `flex: 1 1 auto`, NOT `width: 100%`. The pill is a flex item in the
		   dock row; at 100% it claims the entire line, which is harmless while
		   the row is nowrap but pushes the host's dock-actions onto a second
		   row the moment the row is allowed to wrap. Flexing lets it take the
		   leftover space instead. */
		flex: 1 1 auto;
		width: auto;
		max-width: 100%;
		min-width: 0;
		gap: 6px;
		padding: 0;
		background: transparent;
		border: 0;
		border-radius: 0;
		box-shadow: none;
		backdrop-filter: none;
		-webkit-backdrop-filter: none;
		font-size: 11px;
	}

	/* The clickable identity shortcut. Carries no button chrome at rest — it
	   has to read as the pill's label, not as a control — and picks up a
	   soft background on hover to show it is live. */
	.ctx-identity {
		display: inline-flex;
		align-items: center;
		gap: 8px;
		flex: 1 1 auto;
		min-width: 0;
		margin: 0;
		/* Horizontal padding gives the hover background some body, but it
		   would also push the thread name 5px right of the composer's input
		   text — both rows pad 14px, and this would add to that. The negative
		   margin pulls the text back into alignment while keeping the wider
		   hit area. */
		padding: 2px 5px;
		margin-left: -5px;
		font: inherit;
		color: inherit;
		text-align: left;
		background: transparent;
		border: 0;
		border-radius: 5px;
		cursor: pointer;
		transition: background 0.12s ease, color 0.12s ease;
	}

	.ctx-identity:hover {
		background: var(--bg-soft, rgba(0, 0, 0, 0.05));
	}

	/* The dock owns the full-width highlight, including the shell corners. */
	.ctx-pill--inline .ctx-identity:hover,
	.ctx-pill--inline .ctx-btn:hover {
		background: transparent;
	}
	.ctx-pill--inline .ctx-identity:focus-visible,
	.ctx-pill--inline .ctx-btn:focus-visible {
		outline: none;
	}

	.ctx-identity:hover .title {
		color: var(--text-primary, #1a1a1a);
	}

	/* Let the title give ground first — the floating pill's 380px cap would
	   otherwise push the dock-actions off a narrow composer. */
	.ctx-pill--inline .title {
		max-width: none;
		flex: 1 1 auto;
		min-width: 0;
		font-weight: 400;
		color: var(--text-muted, #888);
	}

	.ctx-pill--inline .thread {
		flex-shrink: 0;
	}

	.ctx-pill--inline .ctx-btn,
	.ctx-pill--inline .more-wrap {
		opacity: 0;
		transition: opacity 140ms ease;
	}

	/* Hovering the pill itself is enough on its own; the composer also
	   drives this from its shell so hovering anywhere on the composer
	   reveals the row (see FloatingComposer's `.composer-dock` rules). */
	.ctx-pill--inline:hover .ctx-btn,
	.ctx-pill--inline:hover .more-wrap,
	.ctx-pill--inline:focus-within .ctx-btn,
	.ctx-pill--inline:focus-within .more-wrap {
		opacity: 1;
	}

	/* An open more-menu must not vanish when the pointer leaves the pill. */
	.ctx-pill--inline .more-wrap:has(.more-menu) {
		opacity: 1;
	}

	@media (prefers-reduced-motion: reduce) {
		.ctx-pill--inline .ctx-btn,
		.ctx-pill--inline .more-wrap {
			transition: none;
		}
	}

	/* Sit tight under the topbar on phone width — every vertical pixel
	   counts on a 700px-tall device, and the 16px breathing gap that
	   makes the pill feel "floating" on desktop just steals scroll
	   room on mobile. 50px = 48px topbar + 2px hairline. */
	@media (max-width: 767px) {
		.ctx-pill {
			top: 50px;
			max-width: calc(100% - 16px);
			padding: 4px 6px 4px 10px;
			font-size: 11px;
		}
	}

	.thread {
		font-family: var(--font-mono);
		font-size: 10.5px;
		color: var(--text-muted, #888);
		/* Meeting threads (`meeting-<label>-<date>`) get long — cap the chip so
		   the pill can't balloon across the thread-bar band. */
		max-width: 180px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.sep {
		color: var(--text-faint, #ccc);
		margin: 0 2px;
	}

	.title {
		font-weight: 500;
		color: var(--text-primary, #1a1a1a);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
		max-width: 380px;
	}

	.title em {
		font-style: normal;
		color: var(--text-muted, #888);
		font-weight: 400;
		margin-left: 4px;
	}

	.readonly-pill {
		font-family: var(--font-mono);
		font-size: 9.5px;
		text-transform: uppercase;
		letter-spacing: 0.06em;
		color: var(--accent-primary, #c2502a);
		background: var(--accent-primary-soft, rgba(194, 80, 42, 0.12));
		border: 1px solid var(--accent-primary, #c2502a);
		border-radius: 999px;
		padding: 2px 8px;
		cursor: pointer;
	}

	/* Re-enable pointer events on everything interactive (the pill container
	   is pointer-events: none — see .ctx-pill). */
	.ctx-btn,
	.readonly-pill,
	.more-wrap {
		pointer-events: auto;
	}

	.ctx-btn {
		width: 22px;
		height: 22px;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		color: var(--text-muted, #888);
		background: transparent;
		border: 0;
		border-radius: 999px;
		cursor: pointer;
		transition: background 0.12s ease, color 0.12s ease;
	}

	.ctx-btn:hover {
		background: var(--bg-soft, rgba(0, 0, 0, 0.05));
		color: var(--text-primary, #1a1a1a);
	}

	.more-wrap {
		position: relative;
	}

	.more-menu {
		position: absolute;
		top: calc(100% + 6px);
		right: 0;
		min-width: 180px;
		background: var(--bg-elevated, #fff);
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.1));
		border-radius: var(--radius-md, 10px);
		box-shadow: var(--shadow-lg, 0 12px 28px rgba(0, 0, 0, 0.18));
		padding: 4px;
		z-index: 60;
	}

	.menu-item {
		display: block;
		width: 100%;
		padding: 7px 10px;
		text-align: left;
		font-family: var(--font-primary);
		font-size: 12.5px;
		color: var(--text-primary, #1a1a1a);
		background: transparent;
		border: 0;
		border-radius: 6px;
		cursor: pointer;
	}

	.menu-item:hover:not(:disabled) {
		background: var(--bg-soft, rgba(0, 0, 0, 0.05));
	}

	.menu-item:disabled {
		opacity: 0.4;
		cursor: not-allowed;
	}

	.menu-item--danger {
		color: var(--color-error, #c0392b);
	}

	.menu-item--danger:hover:not(:disabled) {
		background: var(--color-error-soft, rgba(192, 57, 43, 0.1));
	}
</style>
