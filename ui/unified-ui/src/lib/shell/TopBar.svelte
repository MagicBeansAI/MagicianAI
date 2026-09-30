<script lang="ts">
	/**
	 * TopBar — 48px sticky shell header for the new (app) layout.
	 *
	 *   [ brand ]    [ Today · VibeDev · Chat · Tasks · Observe ]    [ ⌘K · history · attention · mode · avatar ]
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
	import { attentionStore } from '$lib/stores/attentionStore';
	import { meetingsActiveCount } from '$lib/stores/meetingsStore';
	// The Observe tab's live dot doubles as the rail's "visible while
	// active" privacy posture — whenever the screen is being observed,
	// the nav says so on every page.
	import { observeStatus } from '$lib/stores/observeStore';
	import { scopeIdentityStore, refreshScopeSession, switchScopeWorkspace, logoutScopeSession } from '$lib/stores/scopeIdentityStore';
	import {
		ensurePendingHitlBridge
	} from '$lib/stores/pendingHitlStore';
	import { themeStore, type Theme } from '$lib/shared/stores/themeStore';
	import BackendHealthIndicator from '$lib/shared/components/BackendHealthIndicator.svelte';
	import { BRAND_MARK_PATH, BRAND_MARK_VIEWBOX } from '$lib/shared/brand/mark';
	// Gate S3: first-party navigation an installed app declared. The shared
	// poller lives in the store; the TopBar is simply its longest-lived
	// subscriber, which is why it keeps the cadence alive on every page.
	import { appNavigationTabs } from '$lib/apps/appNavigation';
	import { appNavigationEntries } from '$lib/stores/appNavigationStore';
	import RecordingDot from '$lib/shared/components/RecordingDot.svelte';
	import ThemeSwitcher from '$lib/shared/components/ThemeSwitcher.svelte';

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
	$: scope = $scopeIdentityStore;

	function isTodayPath(value: string): boolean {
		return value === '/'
			|| value === '/today'
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

	$: theme = $themeStore;

	// Themes that ship a paired light/dark variant. Mode toggle flips within
	// the pair; all other themes are single-mode and the toggle is hidden.
	const THEME_PAIRS: Record<string, { light: Theme; dark: Theme }> = {
		longhand: { light: 'longhand', dark: 'longhand-dark' },
		arcane: { light: 'arcane-terminal-light', dark: 'arcane-terminal' },
		retro: { light: 'retro-16bit-light', dark: 'retro-16bit' },
		'soft-machine': { light: 'soft-machine', dark: 'soft-machine-dark' },
		mario: { light: 'mario-8bit', dark: 'mario-8bit-dark' },
		risograph: { light: 'risograph', dark: 'risograph-dark' },
		mixtape: { light: 'mixtape', dark: 'mixtape-dark' },
		mono: { light: 'mono', dark: 'mono-dark' },
		cartoon: { light: 'cartoon', dark: 'cartoon-dark' },
		bubbly: { light: 'bubbly', dark: 'bubbly-dark' },
		jarvis: { light: 'jarvis-light', dark: 'jarvis' }
	};

	$: themePair = Object.values(THEME_PAIRS).find(
		(pair) => pair.light === theme || pair.dark === theme
	);
	$: hasPair = themePair != null;
	$: isDarkInPair = themePair != null && theme === themePair.dark;

	function toggleMode(): void {
		if (!themePair) return;
		themeStore.setTheme(isDarkInPair ? themePair.light : themePair.dark);
	}

	let avatarMenuOpen = false;
	let avatarMenuEl: HTMLDivElement | null = null;

	// Workspace switcher state: the session's owned list, refetched each
	// time the menu opens so a workspace created elsewhere appears without a
	// reload. Switching rotates the bearer — the session's claim, not a
	// request header, is the only scope selector the API honors.
	let sessionWorkspaces: Array<{ id: string; display_name: string; is_default: boolean }> = [];
	let workspaceBusy = false;
	let workspaceError: string | null = null;

	function toggleAvatarMenu(): void {
		avatarMenuOpen = !avatarMenuOpen;
		if (avatarMenuOpen) void loadSessionWorkspaces();
	}

	async function loadSessionWorkspaces(): Promise<void> {
		try {
			const session = await refreshScopeSession();
			sessionWorkspaces = session?.workspaces ?? [];
		} catch {
			sessionWorkspaces = [];
		}
	}

	async function switchWorkspace(id: string): Promise<void> {
		if (workspaceBusy || id === scope.workspace) return;
		workspaceBusy = true;
		workspaceError = null;
		try {
			await switchScopeWorkspace(id);
			avatarMenuOpen = false;
			// Honest reload for this slice: scope-keyed stores (tasks, chat,
			// muij) hold the previous workspace's data, and a full re-boot of
			// the shell re-resolves everything against the rotated token
			// instead of each screen guessing what to clear. Running work is
			// untouched — it keeps executing under the workspace it started
			// in; this only changes what this surface addresses next.
			window.location.reload();
		} catch (error) {
			workspaceError = error instanceof Error ? error.message : 'Workspace switch failed.';
		} finally {
			workspaceBusy = false;
		}
	}

	async function signOut(): Promise<void> {
		await logoutScopeSession().catch(() => undefined);
		avatarMenuOpen = false;
		await goto('/login');
	}

	function handleDocumentClick(event: MouseEvent): void {
		if (!avatarMenuOpen) return;
		const target = event.target as Node | null;
		if (avatarMenuEl && target && avatarMenuEl.contains(target)) return;
		avatarMenuOpen = false;
	}

</script>

<svelte:window on:click={handleDocumentClick} />

<header class="topbar">
	<div class="brand-cluster">
		<a class="brand" href="/" aria-label="magican home">
			<span class="brand-mark" aria-hidden="true">
				<svg width="18" height="18" viewBox={BRAND_MARK_VIEWBOX} fill="currentColor">
					<path d={BRAND_MARK_PATH} />
				</svg>
			</span>
			<span class="brand-name">magican</span>
			{#if version}<span class="brand-ver">v{version}</span>{/if}
		</a>
		<span class="topbar__health-wrap"><BackendHealthIndicator compact /></span>
	</div>

	<nav class="primary-nav" aria-label="Primary">
		<a
			class="tab"
			class:active={isActive('/today')}
			href="/today"
			on:click|preventDefault={() => goto('/today')}
		>Today</a>
		<a
			class="tab"
			class:active={isActive('/vibe')}
			href="/vibe"
			on:click|preventDefault={() => goto('/vibe')}
		>VibeDev</a>
		<a
			class="tab"
			class:active={isActive('/chat')}
			href="/chat"
			on:click|preventDefault={() => goto('/chat')}
		>Chat</a>
		<a
			class="tab"
			class:active={isActive('/tasks')}
			href="/tasks"
			on:click|preventDefault={() => goto('/tasks')}
		>Tasks</a>
		<a
			class="tab"
			class:active={isActive('/observe') || isActive('/meetings')}
			href="/observe"
			on:click|preventDefault={() => goto('/observe')}
		>Observe{#if $meetingsActiveCount > 0 || $observeStatus?.status === 'observing'}<span
				class="meetings-live-dot"
				title="Capture in progress (meeting or screen observation)"
			><RecordingDot size={7} /></span>{/if}</a>
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
			class="icon-btn topbar__history"
			type="button"
			title="History"
			on:click={() => dispatch('open-history')}
		>
			<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
				<circle cx="12" cy="12" r="10"/>
				<polyline points="12 6 12 12 16 14"/>
			</svg>
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

		<ThemeSwitcher iconOnly />

		{#if hasPair}
		<button class="icon-btn" type="button" title="Toggle light/dark" on:click={toggleMode}>
			{#if isDarkInPair}
				<!-- sun -->
				<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
					<circle cx="12" cy="12" r="4"/>
					<line x1="12" y1="2" x2="12" y2="4"/>
					<line x1="12" y1="20" x2="12" y2="22"/>
					<line x1="4.93" y1="4.93" x2="6.34" y2="6.34"/>
					<line x1="17.66" y1="17.66" x2="19.07" y2="19.07"/>
					<line x1="2" y1="12" x2="4" y2="12"/>
					<line x1="20" y1="12" x2="22" y2="12"/>
					<line x1="4.93" y1="19.07" x2="6.34" y2="17.66"/>
					<line x1="17.66" y1="6.34" x2="19.07" y2="4.93"/>
				</svg>
			{:else}
				<!-- moon -->
				<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
					<path d="M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z"/>
				</svg>
			{/if}
		</button>
		{/if}

		<div class="avatar-wrap" bind:this={avatarMenuEl}>
			<button
				class="avatar-btn"
				type="button"
				aria-label="Account menu"
				aria-expanded={avatarMenuOpen}
				on:click={toggleAvatarMenu}
			>{(scope.principal?.[0] ?? 'u').toUpperCase()}</button>
			{#if avatarMenuOpen}
				<div class="avatar-menu" role="menu">
					<div class="menu-section">{scope.principal} · {scope.workspace}</div>
					{#if sessionWorkspaces.length > 0}
						<div class="menu-section">Workspace</div>
						{#each sessionWorkspaces as ws (ws.id)}
							<button
								class="menu-item menu-item--btn"
								class:menu-item--current={ws.id === scope.workspace}
								role="menuitem"
								disabled={workspaceBusy}
								on:click={() => switchWorkspace(ws.id)}
							>
								<span class="ws-label">{ws.display_name || ws.id}</span>
								{#if ws.id === scope.workspace}<span class="ws-current">current</span>{/if}
							</button>
						{/each}
						{#if workspaceError}<div class="menu-error" role="alert">{workspaceError}</div>{/if}
					{/if}
					<a class="menu-item" href="/notes" role="menuitem" on:click={() => (avatarMenuOpen = false)}>Notes</a>
					<a class="menu-item" href="/settings" role="menuitem" on:click={() => (avatarMenuOpen = false)}>Settings</a>
					<a class="menu-item" href="/about" role="menuitem" on:click={() => (avatarMenuOpen = false)}>About</a>
					<button class="menu-item menu-item--btn" role="menuitem" on:click={signOut}>Sign out</button>
				</div>
			{/if}
		</div>
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
		padding: 0 16px;
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
		gap: 10px;
		text-decoration: none;
		color: inherit;
	}

	.brand-mark {
		width: 30px;
		height: 30px;
		border-radius: 9px;
		background: var(--coral, #FF6B6B);
		color: #ffffff;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		flex-shrink: 0;
		padding: 0;
		transition: transform 240ms cubic-bezier(0.34, 1.56, 0.64, 1), box-shadow 240ms ease;
	}

	.brand-mark svg {
		display: block;
		width: 18px;
		height: 18px;
	}

	.brand:hover .brand-mark {
		transform: scale(1.08) rotate(-4deg);
		box-shadow: 0 4px 12px color-mix(in srgb, var(--coral, #FF6B6B) 35%, transparent);
		will-change: transform;
	}

	.brand-name {
		font-family: 'Outfit', var(--font-brand, sans-serif);
		font-weight: 400;
		font-size: 22px;
		line-height: 1;
		color: var(--text-primary, #1a1a1a);
		letter-spacing: -0.02em;
		/* Optical centring, not box centring. The flex row already centres all
		   three boxes exactly — measured, their mid-lines coincide to the pixel.
		   The glyphs still sat low, because Outfit's font box is asymmetric
		   (ascent 22, descent 6 at 22px) while this word's ink is asymmetric the
		   other way (ascent 15.07 for the i-dot, descent 4.6 for the g). Net, the
		   ink centre falls 2.76px below the box centre, against 0.19px for the
		   version and 0.00px for the mark — so only the wordmark reads low.

		   Note this is independent of `line-height`: changing it moves the box
		   and the baseline together and leaves the same 2.76px. A nudge is the
		   fix. -3px rather than -2.76px keeps the shift on whole device pixels;
		   the 0.24px residual is smaller than the version number's own offset. */
		transform: translateY(-3px);
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

	/* Mario brand-mark: pixel-square block (no border-radius) + chunky
	   shadow. Wordmark uses Outfit (via --font-brand from :root) like
	   every other theme, so no per-theme size override is needed. */
	:global([data-theme^="mario-8bit"]) .brand-mark {
		border-radius: 0 !important;
		box-shadow: 2px 2px 0 #000;
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
		/* Use --font-display so the nav (Today · VibeDev · Chat · Tasks · Observe) reads
		   as part of the same chrome as the wordmark and the / page hero —
		   Bricolage Grotesque in longhand, Instrument Serif in dispatches,
		   Fredoka in soft-machine, etc. Was --font-primary which fell to the
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
		background: var(--accent-primary);
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

	.avatar-wrap {
		position: relative;
		margin-left: 2px;
	}

	.avatar-btn {
		width: 28px;
		height: 28px;
		border-radius: 50%;
		background: var(--sidebar-logo-bg, linear-gradient(135deg, var(--accent-tertiary, #6a3a5a), var(--accent-primary, #c2502a)));
		color: var(--button-primary-color, #fff);
		/* Avatar initial uses --font-display to match the brand-mark glyph
		   sitting next to it in the topbar. Was --font-primary which fell
		   to the body font. */
		font-family: var(--font-display);
		font-size: 13px;
		font-weight: 700;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		cursor: pointer;
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.08));
		transition: transform 220ms cubic-bezier(0.34, 1.56, 0.64, 1), box-shadow 220ms ease;
	}

	.avatar-btn:hover {
		transform: translate3d(0, 0, 0) scale(1.06);
		box-shadow: 0 2px 8px color-mix(in srgb, var(--accent-primary, #c2502a) 28%, transparent);
		will-change: transform;
	}

	.avatar-btn:active {
		transform: translate3d(0, 0, 0) scale(0.97);
	}

	.avatar-menu {
		animation: avatar-menu-in 180ms cubic-bezier(0.34, 1.56, 0.64, 1);
		transform-origin: top right;
	}

	@keyframes avatar-menu-in {
		from {
			opacity: 0;
			transform: translateY(-4px) scale(0.96);
		}
		to {
			opacity: 1;
			transform: translateY(0) scale(1);
		}
	}

	.avatar-menu {
		position: absolute;
		top: calc(100% + 8px);
		right: 0;
		min-width: 220px;
		background: var(--bg-elevated, #fff);
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.1));
		border-radius: var(--radius-md, 10px);
		box-shadow: var(--shadow-lg, 0 8px 24px rgba(0, 0, 0, 0.12));
		padding: 6px;
		z-index: 220;
	}

	.menu-section {
		padding: 8px 10px 4px;
		font-family: var(--font-mono);
		font-size: 10px;
		text-transform: uppercase;
		letter-spacing: 0.14em;
		color: var(--text-faint, #aaa);
	}

	/* Button-shaped menu entries (workspace switcher, sign out) need their
	   native chrome stripped; anchors already carry .menu-item's look. */
	.menu-item--btn {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 8px;
		width: 100%;
		text-align: left;
		background: none;
		border: none;
		cursor: pointer;
	}

	.menu-item--btn:disabled {
		cursor: wait;
		opacity: 0.6;
	}

	.menu-item--current {
		font-weight: 700;
	}

	.ws-current {
		font-family: var(--font-mono);
		font-size: 10px;
		text-transform: uppercase;
		letter-spacing: 0.12em;
		color: var(--text-faint, #aaa);
	}

	.menu-error {
		padding: 6px 10px;
		font-family: var(--font-primary);
		font-size: 12px;
		color: var(--accent-primary, #c2502a);
	}

	.menu-item {
		display: block;
		padding: 8px 10px;
		border-radius: 6px;
		font-family: var(--font-primary);
		font-size: 13px;
		color: var(--text-primary, #1a1a1a);
		text-decoration: none;
	}

	.menu-item:hover {
		background: var(--bg-soft, rgba(0, 0, 0, 0.05));
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

	@media (prefers-reduced-motion: reduce) {
		.brand-mark,
		.primary-nav .tab,
		.primary-nav .tab::after,
		.cmdk,
		.icon-btn,
		.avatar-btn {
			transition-duration: 0.01ms !important;
		}
		.avatar-menu {
			animation: none !important;
		}
		.brand:hover .brand-mark,
		.icon-btn:hover,
		.avatar-btn:hover {
			transform: none;
		}
	}

</style>
