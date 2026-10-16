<script lang="ts">
	/**
	 * TopBar — 48px sticky shell header for the new (app) layout.
	 *
	 *   [ brand ]    [ Today · VibeDev · Chat · Work · Observe ]    [ ⌘K · history · attention · mode · avatar ]
	 *
	 * Glass blur, hairline bottom border, theme-token-driven. Replaces the
	 * legacy left rail + ThemeSwitcher + bottom-rail brand stack.
	 *
	 * Emits open-palette / open-history events upward; the layout owns the
	 * actual <CommandPalette /> and <HistoryDrawer /> mounts.
	 */
	import { goto } from '$app/navigation';
	import { page } from '$app/stores';
	import { createEventDispatcher, onMount } from 'svelte';

	import {
		attentionCenterState,
		openAttentionCenter
	} from '$lib/attention';
	import { attentionBadgeCount } from '$lib/attention/attentionBadgeCount';
	import AttentionRain from '$lib/attention/AttentionRain.svelte';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import BrandStar from '$lib/shared/brand/BrandStar.svelte';
	import { attentionStore } from '$lib/stores/attentionStore';
	import { meetingsActiveCount } from '$lib/stores/meetingsStore';
	// The Observe tab's live dot doubles as the rail's "visible while
	// active" privacy posture — whenever the screen is being observed,
	// the nav says so on every page.
	import { observeStatus } from '$lib/stores/observeStore';
	import { ensurePendingHitlBridge } from '$lib/stores/pendingHitlStore';
	import BackendHealthIndicator from '$lib/shared/components/BackendHealthIndicator.svelte';
	// Gate S3: first-party navigation an installed app declared. The shared
	// poller lives in the store; the TopBar is simply its longest-lived
	// subscriber, which is why it keeps the cadence alive on every page.
	import { appNavigationTabs } from '$lib/apps/appNavigation';
	import { appNavigationEntries } from '$lib/stores/appNavigationStore';
	import RecordingDot from '$lib/shared/components/RecordingDot.svelte';

	const dispatch = createEventDispatcher<{
		'open-palette': void;
		'open-history': void;
	}>();

	onMount(() => {
		ensurePendingHitlBridge();
		attentionStore.start();
		return () => {
			attentionStore.stop();
		};
	});

	// __APP_VERSION__ is injected at build time via vite.config.ts; declared
	// globally in src/app.d.ts so we can use it directly here.
	const version = __APP_VERSION__;

	$: pathname = $page.url.pathname;

	function isTodayPath(value: string): boolean {
		return value === '/today'
			|| value.startsWith('/today/')
			|| value === '/home'
			|| value.startsWith('/home/')
			|| value === '/desk'
			|| value.startsWith('/desk/');
	}

	function isActive(prefix: string): boolean {
		if (prefix === '/today') {
			return isTodayPath(pathname);
		}
		return pathname === prefix || pathname.startsWith(`${prefix}/`);
	}

	$: pendingAttention = $attentionBadgeCount;

	// Declared tabs render after every built-in one: an app may add a
	// destination, never reorder or displace the shell's own.
	$: navigationTabs = appNavigationTabs($appNavigationEntries);
	// `pathname` is read here rather than through `isActive`, because Svelte
	// cannot see a dependency that only exists inside a called helper.
	$: activeNavigationTab = navigationTabs.find(
		(item) => pathname === item.href || pathname.startsWith(`${item.href}/`)
	)?.href ?? '';
</script>

<header class="topbar">
	<div class="brand-cluster">
		<a class="brand" href="/chat" aria-label="magican chat">
			<span class="brand-mark-col"><span class="brand-mark"><BrandStar size={30} /></span></span>
			<span class="brand-name">magican</span>
			{#if version}<span class="brand-ver">v{version}</span>{/if}
		</a>
		<span class="topbar__health-wrap"><BackendHealthIndicator compact /></span>
	</div>

	<nav class="primary-nav" aria-label="Primary">
		<a
			class="tab tab--chat"
			class:active={isActive('/chat')}
			href="/chat"
			on:click|preventDefault={() => goto('/chat')}
		>Chat</a>
		<!-- Work preserves direct task creation and full inspectors in one destination. -->
		<a
			class="tab"
			class:active={isActive('/work')}
			href="/work"
			on:click|preventDefault={() => goto('/work')}
		>Work</a>
		<a
			class="tab"
			class:active={isActive('/observe') || isActive('/meetings')}
			href="/observe"
			on:click|preventDefault={() => goto('/observe')}
		>Observe{#if $meetingsActiveCount > 0 || $observeStatus?.status === 'observing'}<span
				class="meetings-live-dot"
				title="Capture in progress (meeting or screen observation)"
			><RecordingDot size={7} /></span>{/if}</a>
		<a
			class="tab"
			class:active={isActive('/today')}
			href="/today"
			on:click|preventDefault={() => goto('/today')}
		>Today</a>
		{#each navigationTabs as declared (declared.entry.route)}
			<a
				class="tab"
				class:active={activeNavigationTab === declared.href}
				href={declared.href}
				on:click|preventDefault={() => goto(declared.href)}
			>{declared.entry.title}</a>
		{/each}
	</nav>

	<div class="top-right">
		<button
			class="cmdk"
			type="button"
			on:click={() => dispatch('open-palette')}
			aria-label="Open command palette"
		>
			<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
				<circle cx="11" cy="11" r="7"/>
				<path d="M21 21l-4.3-4.3"/>
			</svg>
			<span class="cmdk-text">Search, jump…</span>
			<kbd>⌘K</kbd>
		</button>

		<button
			type="button"
			class="icon-btn icon-btn--attention"
			title={pendingAttention > 0
				? `${pendingAttention} item${pendingAttention === 1 ? '' : 's'} need attention`
				: 'Attention'}
			aria-label={pendingAttention > 0
				? `Open Attention, ${pendingAttention} pending`
				: 'Open Attention'}
			aria-haspopup="dialog"
			aria-expanded={$attentionCenterState.open}
			data-attention-trigger
			on:click={openAttentionCenter}
		>
			<AttentionRain count={pendingAttention} />
			<Icon name="alert" size={17} />
			{#if pendingAttention > 0}
				<span class="hitl-badge" aria-hidden="true">
					{pendingAttention}
				</span>
			{/if}
		</button>
	</div>
</header>

<style>
	.topbar {
		position: sticky;
		top: 0;
		z-index: 200;
		display: grid;
		/* Brand and right controls take what they need; the nav centres in
		   the remaining space and scrolls within its track on narrow screens. */
		grid-template-columns: auto minmax(0, 1fr) auto;
		align-items: center;
		gap: 24px;
		padding: 0 16px 0 0;
		/* Owned by the v5 shell (see .layout-v5) so a page can subtract the bar
		   in pure CSS. The literal is the fallback for any other mount. */
		height: var(--v5-topbar-h, 48px);
		background: var(--bg-base, #fff);
		background:
			linear-gradient(
				180deg,
				color-mix(in srgb, var(--bg-elevated, var(--bg-base, #fff)) 90%, transparent) 0%,
				color-mix(in srgb, var(--bg-base, #fff) 82%, transparent) 100%
			);
		backdrop-filter: blur(16px) saturate(150%);
		-webkit-backdrop-filter: blur(16px) saturate(150%);
		border-bottom: 1px solid transparent;
		border-image: linear-gradient(
			90deg,
			transparent 0%,
			color-mix(in srgb, var(--border-soft, rgba(0, 0, 0, 0.08)) 100%, transparent) 18%,
			color-mix(in srgb, var(--border-soft, rgba(0, 0, 0, 0.08)) 100%, transparent) 82%,
			transparent 100%
		) 1;
		/* Layout isolation: keeps sticky scroll cheap.
		   `paint` is intentionally omitted — it would clip absolutely-positioned
		   children that overflow downward (theme dropdown, avatar menu). */
		contain: layout style;
	}

	.brand-cluster {
		display: inline-flex;
		align-items: center;
		gap: 12px;
		justify-self: start;
		min-width: 0;
	}

	.brand {
		display: inline-flex;
		align-items: center;
		gap: 0;
		text-decoration: none;
		color: inherit;
	}

	.brand-mark-col {
		width: 48px;
		height: var(--v5-topbar-h, 48px);
		display: inline-flex;
		align-items: center;
		justify-content: center;
		flex-shrink: 0;
	}

	.brand-mark {
		width: 30px;
		height: 30px;
		border-radius: 9px;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		flex-shrink: 0;
		padding: 0;
		transition: transform 240ms cubic-bezier(0.34, 1.56, 0.64, 1), box-shadow 240ms ease;
	}

	.brand:hover .brand-mark {
		transform: scale(1.08) rotate(-4deg);
		filter: drop-shadow(0 3px 8px rgba(245, 178, 30, 0.45));
		will-change: transform;
	}

	.brand-name {
		font-family: 'Outfit', var(--font-brand, sans-serif);
		font-weight: 400;
		font-size: 22px;
		line-height: 1;
		color: var(--text-primary, #1a1a1a);
		letter-spacing: -0.02em;
		transform: translateY(-3px);
		margin-left: 2px;
	}

	/* Per-theme nav tuning. The base `.primary-nav .tab` rule sets
	   font-weight: 600, which works for variable display fonts (Bricolage,
	   Fredoka, Satoshi). Single-weight display fonts would need their own
	   override here. */

	/* Longhand (light + dark): Bricolage variation for the small UI scale. */
	:global([data-theme="longhand"]) .primary-nav .tab,
	:global([data-theme="longhand-dark"]) .primary-nav .tab {
		font-variation-settings: 'wdth' 96, 'opsz' 16;
		letter-spacing: -0.005em;
	}

	/* Mario 8-bit (sky + underground): Press Start 2P is single-weight (400)
	   and dense — disable synthesis, drop size to fit nav strip, tighten
	   tracking. */
	:global([data-theme^="mario-8bit"]) .primary-nav .tab {
		font-weight: 400;
		font-size: 9px;
		letter-spacing: 0.04em;
		text-transform: uppercase;
		font-synthesis: none;
		padding: 8px 12px;
		border-radius: 0;
	}

	/* Mario brand-mark: a chunky pixel shadow that follows the star's own
	   outline (no tile behind it any more). Wordmark uses Outfit (via
	   --font-brand from :root) like every other theme, so no per-theme size
	   override is needed. */
	:global([data-theme^="mario-8bit"]) .brand-mark {
		filter: drop-shadow(2px 2px 0 #000);
	}

	.brand-ver {
		margin-left: 4px;
		font-family: var(--font-mono);
		font-size: 10.5px;
		color: var(--text-faint, #aaa);
	}

	.primary-nav {
		display: inline-flex;
		gap: 2px;
		justify-self: center;
		min-width: 0;
		max-width: 100%;
		/* On tight viewports the tab row scrolls instead of painting over the
		   right-hand controls. The scrollbar stays hidden; drag/trackpad still
		   scrolls, and focus follows the links. */
		overflow-x: auto;
		scrollbar-width: none;
	}
	.primary-nav::-webkit-scrollbar {
		display: none;
	}

	.primary-nav .tab {
		position: relative;
		padding: 6px 14px;
		border-radius: 8px;
		/* Use --font-display so the nav (Today · VibeDev · Chat · Work · Observe) reads
		   as part of the same chrome as the wordmark and the / page hero —
		   Bricolage Grotesque in longhand, Instrument Serif in dispatches, etc.
		   Was --font-primary which fell to the
		   body serif (Newsreader in longhand) and looked unrelated to the
		   wordmark next to it. */
		font-family: var(--font-display);
		font-size: 14px;
		font-weight: 600;
		color: var(--text-muted, #888);
		text-decoration: none;
		transition:
			background 180ms ease,
			color 180ms ease,
			transform 180ms cubic-bezier(0.34, 1.56, 0.64, 1);
	}

	.primary-nav .tab::after {
		content: '';
		position: absolute;
		left: 14px;
		right: 14px;
		bottom: 2px;
		height: 1.5px;
		border-radius: 1px;
		background: linear-gradient(90deg, var(--accent-primary, #c2502a), var(--accent-secondary, var(--accent-primary, #c2502a)));
		transform: scaleX(0);
		transform-origin: center;
		opacity: 0;
		transition:
			transform 220ms cubic-bezier(0.34, 1.56, 0.64, 1),
			opacity 180ms ease;
	}

	.primary-nav .tab:hover {
		background: var(--bg-soft, rgba(0, 0, 0, 0.04));
		color: var(--text-primary, #1a1a1a);
	}

	.primary-nav .tab:hover::after {
		transform: scaleX(0.4);
		opacity: 0.5;
	}

	.primary-nav .tab.active {
		background: var(--accent-primary-soft, rgba(0, 0, 0, 0.06));
		color: var(--text-primary, #1a1a1a);
	}

	.primary-nav .tab.active::after {
		transform: scaleX(1);
		opacity: 1;
	}

	.primary-nav .tab.tab--chat {
		font-size: 15.5px;
		font-weight: 700;
		padding: 6px 18px;
		letter-spacing: -0.01em;
	}

	/* Positions the shared RecordingDot on the Meetings tab while ANY meeting
	   capture (passive listener or agent attendee) is active — capture must
	   stay visible from every page. */
	.meetings-live-dot {
		display: inline-flex;
		margin-left: 0.35rem;
		vertical-align: middle;
	}

	.hitl-badge {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		min-width: 1.1rem;
		height: 1.1rem;
		padding: 0 0.35rem;
		background: var(--accent-fill);
		color: var(--text-on-accent, #fff);
		border-radius: 999px;
		font-family: var(--font-mono);
		font-size: 0.65rem;
		font-weight: 700;
		line-height: 1;
		animation: hitl-badge-pulse 2.4s ease-in-out infinite;
	}

	@keyframes hitl-badge-pulse {
		0%,
		100% {
			box-shadow: 0 0 0 0 var(--accent-primary-soft);
		}
		50% {
			box-shadow: 0 0 0 4px transparent;
		}
	}

	.top-right {
		display: inline-flex;
		align-items: center;
		gap: 6px;
		justify-self: end;
		min-width: 0;
	}

	.cmdk {
		position: relative;
		display: inline-flex;
		align-items: center;
		gap: 10px;
		padding: 5px 9px 5px 12px;
		background: var(--bg-soft, rgba(0, 0, 0, 0.04));
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.08));
		border-radius: 8px;
		font-family: var(--font-primary);
		font-size: 12.5px;
		color: var(--text-muted, #888);
		cursor: text;
		transition:
			background 180ms ease,
			border-color 180ms ease,
			box-shadow 220ms ease,
			transform 180ms cubic-bezier(0.34, 1.56, 0.64, 1);
		min-width: 160px;
		overflow: hidden;
		isolation: isolate;
	}

	/* Subtle shimmer sweep on hover. GPU-only: transform translateX. */
	.cmdk::before {
		content: '';
		position: absolute;
		inset: 0;
		background: linear-gradient(
			110deg,
			transparent 35%,
			color-mix(in srgb, var(--accent-primary, #c2502a) 8%, transparent) 50%,
			transparent 65%
		);
		transform: translate3d(-110%, 0, 0);
		opacity: 0;
		pointer-events: none;
		z-index: -1;
	}

	.cmdk:hover {
		background: var(--bg-warm, rgba(0, 0, 0, 0.06));
		border-color: var(--border-default, rgba(0, 0, 0, 0.14));
		box-shadow: 0 1px 0 color-mix(in srgb, var(--accent-primary, #c2502a) 8%, transparent);
		will-change: transform;
	}

	.cmdk:hover::before {
		opacity: 1;
		transform: translate3d(110%, 0, 0);
		transition:
			transform 720ms cubic-bezier(0.22, 1, 0.36, 1),
			opacity 200ms ease;
	}

	.cmdk:focus-visible {
		outline: none;
		border-color: color-mix(in srgb, var(--accent-primary, #c2502a) 50%, var(--border-default, rgba(0, 0, 0, 0.14)));
		box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent-primary, #c2502a) 18%, transparent);
	}

	.cmdk-text {
		flex: 1;
		text-align: left;
	}

	.cmdk kbd {
		font-family: var(--font-mono);
		font-size: 10.5px;
		color: var(--text-muted, #888);
		background: var(--bg-base, #fff);
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.08));
		border-radius: 4px;
		padding: 1px 5px;
		transition:
			transform 180ms cubic-bezier(0.34, 1.56, 0.64, 1),
			box-shadow 180ms ease,
			color 180ms ease;
	}

	.cmdk:hover kbd {
		transform: translate3d(0, -1px, 0);
		box-shadow: var(--shadow-sm);
		color: var(--text-primary, #1a1a1a);
	}

	.icon-btn {
		width: 30px;
		height: 30px;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		color: var(--text-muted, #888);
		border-radius: 7px;
		background: transparent;
		border: 0;
		text-decoration: none;
		cursor: pointer;
		transition:
			background 180ms ease,
			color 180ms ease,
			transform 180ms cubic-bezier(0.34, 1.56, 0.64, 1);
		position: relative;
	}

	.icon-btn:hover {
		background: var(--bg-soft, rgba(0, 0, 0, 0.05));
		color: var(--text-primary, #1a1a1a);
		transform: translate3d(0, -1px, 0);
		will-change: transform;
	}

	.icon-btn:active {
		transform: translate3d(0, 0, 0) scale(0.96);
	}

	/* Pinned counter on the right-side Attention icon. Positions
	 * the shared `.hitl-badge` at the top-right corner of the
	 * 30x30 button. Smaller min-width than the nav-tab variant
	 * because there's less room on a square icon. */
	.icon-btn--attention .hitl-badge {
		position: absolute;
		top: -2px;
		right: -2px;
		min-width: 0.95rem;
		height: 0.95rem;
		padding: 0 0.3rem;
		font-size: 0.62rem;
		border: 1.5px solid var(--bg-base, #fff);
	}



	@media (max-width: 1000px) {
		.cmdk-text {
			display: none;
		}
		.cmdk {
			min-width: 0;
		}
	}

	@media (max-width: 720px) {
		.cmdk-text {
			display: none;
		}
		.cmdk {
			min-width: 0;
		}
		.brand-ver {
			display: none;
		}
	}

	/* Phones: the primary nav had no width left beside the brand and six
	   controls, so it collapsed to nothing. It takes a second row, the full
	   width, scrolling if it must; the shell publishes the taller bar as
	   --v5-topbar-h (88px) so pages size against it exactly. Below 900px the
	   tabs no longer fit in one row beside the controls. */
	@media (max-width: 900px) {
		.topbar {
			grid-template-columns: auto minmax(0, 1fr);
			grid-template-rows: 48px 40px;
			grid-template-areas:
				'brand right'
				'nav nav';
			row-gap: 0;
		}
		.brand-cluster {
			grid-area: brand;
		}
		.top-right {
			grid-area: right;
		}
		.primary-nav {
			grid-area: nav;
			justify-self: stretch;
			justify-content: safe center;
			align-self: center;
		}
		/* The attention rain falls from the bell; start it below the nav row
		   so its markers never cross the tab labels. */
		.icon-btn--attention :global(.attention-rain) {
			top: calc(100% + 40px);
		}
	}

	/* Phones: six controls sit on the right, so the desktop gaps squeezed the
	   brand until the search pill overlapped the wordmark. Tighten the
	   spacing, and below the width where the wordmark fits keep only the
	   mark (the link keeps its "magican home" label). */
	@media (max-width: 520px) {
		.topbar {
			gap: 8px;
			padding: 0 10px 0 0;
		}
		.top-right {
			gap: 2px;
		}
		.brand-cluster {
			gap: 8px;
		}
		/* The health pill shrinks to its icon so it never covers the search
		   pill; its words stay for screen readers. */
		.topbar__health-wrap :global(.health-banner--compact) {
			padding: 4px 7px;
		}
		/* While the pill shows, the wordmark gives way to it (the mark keeps
		   the home link and its label). */
		.brand-cluster:has(:global(.health-banner)) .brand-name {
			display: none;
		}
		.topbar__health-wrap :global(.health-message) {
			position: absolute;
			width: 1px;
			height: 1px;
			overflow: hidden;
			clip: rect(0 0 0 0);
			white-space: nowrap;
		}
	}

	@media (max-width: 420px) {
		.cmdk kbd {
			display: none;
		}
	}

	@media (max-width: 385px) {
		.brand-name {
			display: none;
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.brand-mark,
		.primary-nav .tab,
		.primary-nav .tab::after,
		.cmdk,
		.icon-btn {
			transition-duration: 0.01ms !important;
		}
		.brand:hover .brand-mark,
		.icon-btn:hover {
			transform: none;
		}
	}

</style>
