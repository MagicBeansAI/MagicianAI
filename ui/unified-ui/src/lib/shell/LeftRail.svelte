<script lang="ts">
	import { goto } from '$app/navigation';
	import { page } from '$app/stores';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import { clickOutside } from '$lib/shared/clickOutside';
	import {
		scopeIdentityStore,
		refreshScopeSession,
		switchScopeWorkspace,
		logoutScopeSession
	} from '$lib/stores/scopeIdentityStore';
	import { themeStore, type Theme } from '$lib/shared/stores/themeStore';
	import ThemeSwitcher from '$lib/shared/components/ThemeSwitcher.svelte';

	$: pathname = $page.url.pathname;
	$: scope = $scopeIdentityStore;
	$: isQualityActive = pathname === '/work' && $page.url.searchParams.get('view') === 'quality';

	const THEME_PAIRS: Record<string, { light: Theme; dark: Theme }> = {
		sorbet: { light: 'sorbet', dark: 'sorbet-dark' },
		sky: { light: 'sky', dark: 'sky-dark' },
		candy: { light: 'candy', dark: 'candy-dark' },
		longhand: { light: 'longhand', dark: 'longhand-dark' },
		arcane: { light: 'arcane-terminal-light', dark: 'arcane-terminal' },
		retro: { light: 'retro-16bit-light', dark: 'retro-16bit' },
		mario: { light: 'mario-8bit', dark: 'mario-8bit-dark' },
		risograph: { light: 'risograph', dark: 'risograph-dark' },
		mixtape: { light: 'mixtape', dark: 'mixtape-dark' },
		mono: { light: 'mono', dark: 'mono-dark' },
		cartoon: { light: 'cartoon', dark: 'cartoon-dark' },
		jarvis: { light: 'jarvis-light', dark: 'jarvis' }
	};

	$: theme = $themeStore;
	$: themePair = Object.values(THEME_PAIRS).find(
		(pair) => pair.light === theme || pair.dark === theme
	);
	$: hasPair = themePair != null;
	$: isDarkInPair = themePair != null && theme === themePair.dark;

	function toggleMode(): void {
		if (!themePair) return;
		themeStore.setTheme(isDarkInPair ? themePair.light : themePair.dark);
	}

	function isActive(prefix: string): boolean {
		return pathname === prefix || pathname.startsWith(`${prefix}/`);
	}

	let avatarMenuOpen = false;
	let avatarMenuEl: HTMLDivElement | null = null;
	let avatarBtnEl: HTMLButtonElement | null = null;

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

	function handleKeydown(event: KeyboardEvent): void {
		if (avatarMenuOpen && event.key === 'Escape') {
			avatarMenuOpen = false;
			avatarBtnEl?.focus();
		}
	}
</script>

<svelte:window on:keydown={handleKeydown} />

<aside class="left-rail" aria-label="Quick actions and navigation">
	<div class="rail-group rail-group--top">
		<a
			class="rail-btn"
			class:active={isActive('/apps')}
			href="/apps"
			title="Apps"
			aria-label="Apps"
		>
			<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
				<rect x="3" y="3" width="7" height="7" rx="1.5" />
				<rect x="14" y="3" width="7" height="7" rx="1.5" />
				<rect x="3" y="14" width="7" height="7" rx="1.5" />
				<rect x="14" y="14" width="7" height="7" rx="1.5" />
			</svg>
			<span class="rail-tooltip">Apps</span>
		</a>

		<a
			class="rail-btn"
			class:active={isActive('/llm')}
			href="/llm"
			title="LLM & Models"
			aria-label="LLM and Models"
		>
			<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
				<path d="M12 2v4M12 18v4M4.93 4.93l2.83 2.83M16.24 16.24l2.83 2.83M2 12h4M18 12h4M4.93 19.07l2.83-2.83M16.24 7.76l2.83-2.83" />
				<circle cx="12" cy="12" r="3.5" />
			</svg>
			<span class="rail-tooltip">LLM</span>
		</a>

		<a
			class="rail-btn"
			class:active={isActive('/memory')}
			href="/memory"
			title="Memory"
			aria-label="Memory"
		>
			<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
				<path d="M9.5 2A2.5 2.5 0 0 1 12 4.5v15a2.5 2.5 0 0 1-4.96.44 2.5 2.5 0 0 1-2.96-3.08 3 3 0 0 1-.34-5.58 2.5 2.5 0 0 1 1.32-4.24 2.5 2.5 0 0 1 4.44-5.04z" />
				<path d="M14.5 2A2.5 2.5 0 0 0 12 4.5v15a2.5 2.5 0 0 0 4.96.44 2.5 2.5 0 0 0 2.96-3.08 3 3 0 0 0 .34-5.58 2.5 2.5 0 0 0-1.32-4.24 2.5 2.5 0 0 0-4.44-5.04z" />
			</svg>
			<span class="rail-tooltip">Memory</span>
		</a>

		<a
			class="rail-btn"
			class:active={isActive('/notes')}
			href="/notes"
			title="Notes"
			aria-label="Notes"
		>
			<Icon name="file-text" size={18} />
			<span class="rail-tooltip">Notes</span>
		</a>

		<a
			class="rail-btn"
			class:active={isActive('/vault')}
			href="/vault"
			title="Vault"
			aria-label="Vault"
		>
			<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
				<rect x="3" y="4" width="18" height="15" rx="2" />
				<circle cx="12" cy="11.5" r="3.5" />
				<path d="M12 8v1.2M12 13.8V15M8.5 11.5h1.2M14.3 11.5h1.2M7 19v2M17 19v2" />
			</svg>
			<span class="rail-tooltip">Vault</span>
		</a>

		<a
			class="rail-btn"
			class:active={isActive('/budget')}
			href="/budget"
			title="Resource Authority"
			aria-label="Resource Authority"
		>
			<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
				<path d="M19 7V5a2 2 0 0 0-2-2H5a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-2" />
				<path d="M21 9h-6a3 3 0 0 0 0 6h6z" />
				<circle cx="15.5" cy="12" r="0.6" fill="currentColor" />
			</svg>
			<span class="rail-tooltip">Resource Authority</span>
		</a>

		<a
			class="rail-btn"
			class:active={isQualityActive}
			href="/work?view=quality"
			title="Quality & Review"
			aria-label="Quality and Review"
		>
			<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
				<path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z" />
				<path d="m9 12 2 2 4-4" />
			</svg>
			<span class="rail-tooltip">Quality &amp; Review</span>
		</a>

		<a
			class="rail-btn"
			class:active={isActive('/settings')}
			href="/settings"
			title="Settings"
			aria-label="Settings"
		>
			<Icon name="settings" size={18} />
			<span class="rail-tooltip">Settings</span>
		</a>
	</div>

	<div class="rail-group rail-group--bottom">
		<div class="avatar-wrap" bind:this={avatarMenuEl}>
			<button
				bind:this={avatarBtnEl}
				class="avatar-btn"
				type="button"
				aria-label="Account menu"
				aria-expanded={avatarMenuOpen}
				title={`Account: ${scope.principal || 'User'}`}
				on:click={toggleAvatarMenu}
			>
				{(scope.principal?.[0] ?? 'u').toUpperCase()}
			</button>

			{#if avatarMenuOpen}
				<div
					class="avatar-menu"
					role="menu"
					use:clickOutside={{
						handler: () => (avatarMenuOpen = false),
						exclude: [avatarBtnEl]
					}}
				>
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
					<div class="menu-divider" role="separator"></div>
					<div class="menu-section">Appearance</div>
					<div class="account-appearance-row">
						<div class="theme-switcher-slot">
							<ThemeSwitcher minimal />
						</div>
						{#if hasPair}
							<button
								class="account-mode-btn"
								type="button"
								title={isDarkInPair ? 'Switch to light mode' : 'Switch to dark mode'}
								aria-label={isDarkInPair ? 'Switch to light mode' : 'Switch to dark mode'}
								on:click={toggleMode}
							>
								{#if isDarkInPair}
									<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
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
									<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
										<path d="M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z"/>
									</svg>
								{/if}
							</button>
						{/if}
					</div>
					<div class="menu-divider" role="separator"></div>
					<a class="menu-item" href="/about" role="menuitem" on:click={() => (avatarMenuOpen = false)}>About</a>
					<div class="menu-divider" role="separator"></div>
					<button class="menu-item menu-item--btn menu-item--signout" role="menuitem" on:click={signOut}>Sign out</button>
				</div>
			{/if}
		</div>
	</div>
</aside>

<style>
	.left-rail {
		width: var(--v5-left-rail-w, 48px);
		flex-shrink: 0;
		display: flex;
		flex-direction: column;
		justify-content: space-between;
		align-items: center;
		padding: 10px 0 12px;
		background: var(--bg-card, #fff);
		border-right: 1px solid var(--border-soft, rgba(0, 0, 0, 0.08));
		position: relative;
		z-index: 130;
		user-select: none;
	}

	.rail-group {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 6px;
		width: 100%;
	}

	.rail-group--bottom {
		margin-top: auto;
		padding-top: 8px;
	}

	.rail-btn {
		position: relative;
		width: 34px;
		height: 34px;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		border-radius: 9px;
		color: var(--text-muted, #777);
		text-decoration: none;
		background: transparent;
		border: 1px solid transparent;
		transition:
			background 180ms ease,
			color 180ms ease,
			transform 180ms cubic-bezier(0.34, 1.56, 0.64, 1),
			box-shadow 180ms ease,
			border-color 180ms ease;
	}

	.rail-btn:hover {
		background: var(--bg-soft, rgba(0, 0, 0, 0.05));
		color: var(--text-primary, #1a1a1a);
		transform: scale(1.06);
	}

	.rail-btn.active {
		background: var(--accent-primary-soft, rgba(194, 80, 42, 0.1));
		color: var(--accent-primary, #c2502a);
		border-color: color-mix(in srgb, var(--accent-primary, #c2502a) 25%, transparent);
		box-shadow: 0 1px 3px color-mix(in srgb, var(--accent-primary, #c2502a) 12%, transparent);
	}

	.rail-btn.active::before {
		content: '';
		position: absolute;
		left: -7px;
		top: 50%;
		transform: translateY(-50%);
		width: 3px;
		height: 16px;
		border-radius: 0 3px 3px 0;
		background: var(--accent-primary, #c2502a);
	}

	/* Simple hover tooltip */
	.rail-tooltip {
		position: absolute;
		left: calc(100% + 8px);
		top: 50%;
		transform: translateY(-50%) translateX(-4px);
		padding: 4px 8px;
		background: var(--bg-elevated, #1c1c1f);
		color: var(--text-primary, #ffffff);
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.1));
		font-family: var(--font-primary, sans-serif);
		font-size: 11px;
		font-weight: 600;
		white-space: nowrap;
		border-radius: 6px;
		pointer-events: none;
		opacity: 0;
		box-shadow: var(--shadow-md, 0 4px 12px rgba(0, 0, 0, 0.15));
		transition: opacity 140ms ease, transform 140ms ease;
		z-index: 150;
	}

	.rail-btn:hover .rail-tooltip {
		opacity: 1;
		transform: translateY(-50%) translateX(0);
	}

	.avatar-wrap {
		position: relative;
	}

	.avatar-btn {
		width: 30px;
		height: 30px;
		border-radius: 50%;
		background: var(--sidebar-logo-bg, linear-gradient(135deg, var(--accent-tertiary, #6a3a5a), var(--accent-primary, #c2502a)));
		color: var(--text-on-bright, var(--button-primary-color, #fff));
		font-family: var(--font-display, sans-serif);
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
		transform: scale(1.08);
		box-shadow: 0 2px 8px color-mix(in srgb, var(--accent-primary, #c2502a) 32%, transparent);
	}

	.avatar-btn:active {
		transform: scale(0.96);
	}

	.avatar-menu {
		position: absolute;
		left: calc(100% + 10px);
		bottom: 0;
		min-width: 220px;
		background: var(--bg-elevated, #fff);
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.1));
		border-radius: var(--radius-md, 10px);
		box-shadow: var(--shadow-lg, 0 8px 24px rgba(0, 0, 0, 0.15));
		padding: 6px;
		z-index: 250;
		animation: avatar-menu-flyout 180ms cubic-bezier(0.34, 1.56, 0.64, 1);
		transform-origin: bottom left;
	}

	@keyframes avatar-menu-flyout {
		from {
			opacity: 0;
			transform: translateX(-4px) scale(0.95);
		}
		to {
			opacity: 1;
			transform: translateX(0) scale(1);
		}
	}

	.menu-section {
		padding: 7px 10px 4px;
		font-family: var(--font-mono, monospace);
		font-size: 10px;
		text-transform: uppercase;
		letter-spacing: 0.12em;
		color: var(--text-faint, #aaa);
	}

	.menu-divider {
		height: 1px;
		background: var(--border-soft, rgba(0, 0, 0, 0.08));
		margin: 4px 6px;
	}

	.menu-item {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 8px;
		width: 100%;
		padding: 7px 10px;
		border-radius: 6px;
		font-family: var(--font-primary, sans-serif);
		font-size: 12.5px;
		font-weight: 500;
		color: var(--text-primary, #1a1a1a);
		text-decoration: none;
		background: transparent;
		border: 0;
		cursor: pointer;
		text-align: left;
		transition: background 140ms ease, color 140ms ease;
	}

	.menu-item:hover {
		background: var(--bg-soft, rgba(0, 0, 0, 0.05));
		color: var(--accent-primary, #c2502a);
	}

	.menu-item--btn {
		font: inherit;
	}

	.menu-item--current {
		background: var(--accent-primary-soft, rgba(194, 80, 42, 0.08));
		color: var(--accent-primary, #c2502a);
		font-weight: 600;
	}

	.ws-label {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.ws-current {
		font-family: var(--font-mono, monospace);
		font-size: 9.5px;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.06em;
		color: var(--accent-primary, #c2502a);
	}

	.menu-item--signout {
		color: var(--color-error, #d9383a);
	}

	.menu-item--signout:hover {
		background: color-mix(in srgb, var(--color-error, #d9383a) 10%, transparent);
		color: var(--color-error, #d9383a);
	}

	.menu-error {
		padding: 4px 10px;
		font-size: 11px;
		color: var(--color-error, #d9383a);
	}

	.account-appearance-row {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 6px;
		padding: 4px 6px;
	}

	.theme-switcher-slot {
		flex: 1;
		min-width: 0;
	}

	.account-mode-btn {
		width: 32px;
		height: 32px;
		flex-shrink: 0;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		border-radius: 8px;
		background: var(--bg-soft, rgba(0, 0, 0, 0.05));
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.08));
		color: var(--text-secondary, #555);
		cursor: pointer;
		transition: background 140ms ease, color 140ms ease, border-color 140ms ease;
	}

	.account-mode-btn:hover {
		background: var(--bg-elevated, #fff);
		color: var(--text-primary, #1a1a1a);
		border-color: var(--border-default, rgba(0, 0, 0, 0.15));
	}

</style>
