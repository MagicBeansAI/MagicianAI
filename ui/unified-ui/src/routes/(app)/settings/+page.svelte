<script lang="ts">
	import { PRODUCT_NAME } from '$lib/presentationIdentity';
	import { browser } from '$app/environment';
	import { onMount, tick } from 'svelte';
	import { page } from '$app/stores';
	import { replaceState } from '$app/navigation';
	import { requestConfirmation } from '$lib/stores/confirmationStore';
	import {
		clearMagicianConfigError,
		magicianConfigReloadResult,
		magicianConfigStoreState,
		reloadMagicianConfig
	} from '$lib/stores/magicianConfigStore';
	import {
		notesSettingsStore,
		refreshNotesSettings,
		saveNotesSettings,
		type NotesProviderSettings,
		type NotesProviderStatus,
		type TaskNotePublishMode
	} from '$lib/stores/notesSettingsStore';
	import { showError, showSuccess } from '$lib/shared/stores/notifications';
	import {
		loginScopeSession,
		logoutScopeSession,
		refreshScopeSession,
		switchScopeWorkspace,
		type AuthenticatedScopeSession
	} from '$lib/stores/scopeIdentityStore';
	import SettingsNav from '$lib/settings/SettingsNav.svelte';
	import LazyPanel from '$lib/settings/LazyPanel.svelte';
	import { createPanelScheduler } from '$lib/settings/panelLoading';
	import {
		SETTINGS_CATEGORIES,
		cardMatches,
		categoryForHash,
		normalizeSettingsQuery,
		type SettingsNavCategory
	} from '$lib/settings/settingsIndex';
	import {
		clearTrustPolicyError,
		loadTrustPolicy,
		resetTrustPolicyDraftToPersisted,
		restoreTrustPolicyFromTemplate,
		saveTrustPolicy,
		setTrustPolicyDraftContent,
		trustPolicyDocument,
		trustPolicyStoreState
	} from '$lib/stores/trustPolicyStore';

	let routeError: string | null = null;
	let isEditing = false;
	let editorDraft = '';
	let lastRefreshAt: number | null = null;
	let notesEnabled = true;
	let notesFolderPath = '';
	let notesAutoPublishCompleted = false;
	let notesTaskPublishMode: TaskNotePublishMode = 'standard';
	let notesTaskIncludeAssets = true;
	let notesFormDirty = false;
	let notesFormSignature = '';
	let authSession: AuthenticatedScopeSession | null = null;
	let authUsername = '';
	let authPassword = '';
	let authWorkspace = 'default';
	let authBusy = false;
	let authMessage: string | null = null;
	let apiMiningSettings: {
		effective: boolean;
		process_enabled: boolean;
		set_by: 'config' | 'scope' | 'default';
	} | null = null;
	let apiMiningBusy = false;

	// Panels load in the order the reader needs them: the category the URL
	// opens (else the first) before the rest. See `panelLoading.ts`.
	const panels = createPanelScheduler(
		(browser && categoryForHash(window.location.hash)) || 'account'
	);

	$: loading = $trustPolicyStoreState.isLoading && !$trustPolicyDocument;
	$: combinedError = routeError || $trustPolicyStoreState.error;
	$: validationSummary = buildValidationSummary(
		$trustPolicyStoreState.validationError,
		$trustPolicyStoreState.validationLine,
		$trustPolicyStoreState.validationColumn
	);
	$: persistedContent = $trustPolicyDocument?.content || '';
	$: hasDraftChanges = isEditing && editorDraft !== persistedContent;
	$: if (!isEditing && editorDraft !== persistedContent) {
		editorDraft = persistedContent;
	}
	$: notesSettings = $notesSettingsStore.envelope?.settings ?? defaultNotesSettings();
	$: notesResolved = $notesSettingsStore.envelope?.resolved;
	$: notesStatus = $notesSettingsStore.status;
	// One notes location: the notes folder (persisted provider id
	// `silverbullet`, kept so stored settings and note records stay valid).
	// Local Markdown is only where writes fall back when that folder is unset
	// or unsafe, so it is shown only while in use.
	$: notesFolderStatus = notesStatus?.providers.find((provider) => provider.id === 'silverbullet');
	$: notesUsingFallback =
		notesSettings.default_provider !== 'silverbullet' ||
		(notesResolved ? !notesResolved.notes_folder_write_safe : false) ||
		(notesFolderStatus ? !notesFolderStatus.writable : false);
	$: notesWarnings = uniqueStrings([
		...($notesSettingsStore.envelope?.warnings ?? []),
		...(notesStatus?.warnings ?? [])
	]);
	$: syncNotesFormFromSettings(notesSettings);

	function buildValidationSummary(
		error: string | null,
		line: number | null,
		column: number | null
	): string | null {
		if (!error) return null;
		if (!line || !column) return error;
		return `${error} (line ${line}, column ${column})`;
	}

	function formatRelativeTime(timestamp: number | null | undefined): string {
		if (!timestamp) return 'never';
		const diffMs = timestamp - Date.now();
		const diffMinutes = Math.round(diffMs / 60000);
		if (Math.abs(diffMinutes) < 1) return 'just now';
		if (Math.abs(diffMinutes) < 60) {
			return `${Math.abs(diffMinutes)}m ${diffMinutes < 0 ? 'ago' : 'from now'}`;
		}
		const diffHours = Math.round(diffMinutes / 60);
		if (Math.abs(diffHours) < 48) {
			return `${Math.abs(diffHours)}h ${diffHours < 0 ? 'ago' : 'from now'}`;
		}
		const diffDays = Math.round(diffHours / 24);
		return `${Math.abs(diffDays)}d ${diffDays < 0 ? 'ago' : 'from now'}`;
	}

	function defaultNotesSettings(): NotesProviderSettings {
		return {
			enabled: true,
			default_provider: 'local_markdown',
			fallback_provider: 'local_markdown',
			local_markdown: {},
			notes_folder: {},
			task_publishing: {
				auto_publish_completed: false,
				default_mode: 'standard',
				include_assets: true
			}
		};
	}

	function notesSettingsSignature(settings: NotesProviderSettings): string {
		return JSON.stringify([
			settings.enabled,
			settings.default_provider,
			settings.local_markdown.root ?? '',
			settings.notes_folder.space_path ?? '',
			settings.task_publishing.auto_publish_completed,
			settings.task_publishing.default_mode,
			settings.task_publishing.include_assets
		]);
	}

	function syncNotesFormFromSettings(settings: NotesProviderSettings): void {
		const signature = notesSettingsSignature(settings);
		if (notesFormDirty || signature === notesFormSignature) return;
		notesEnabled = settings.enabled;
		notesFolderPath = settings.notes_folder.space_path ?? '';
		notesAutoPublishCompleted = settings.task_publishing.auto_publish_completed;
		notesTaskPublishMode = settings.task_publishing.default_mode;
		notesTaskIncludeAssets = settings.task_publishing.include_assets;
		notesFormSignature = signature;
	}

	function markNotesFormDirty(): void {
		notesFormDirty = true;
	}

	function uniqueStrings(values: string[]): string[] {
		return values.filter((value, index, all) => value.trim() && all.indexOf(value) === index);
	}

	function joinOrFallback(values: string[] | undefined, fallback = 'n/a'): string {
		return values && values.length > 0 ? values.join(', ') : fallback;
	}

	function statusTone(provider: NotesProviderStatus): 'success' | 'warning' | 'error' {
		if (provider.writable) return 'success';
		if (provider.available || provider.configured) return 'warning';
		return 'error';
	}

	async function refresh(showToast: boolean): Promise<void> {
		if (hasDraftChanges) {
			showError('Save or cancel edits before refreshing trust policy');
			return;
		}
		routeError = null;
		clearTrustPolicyError();
		try {
			await loadTrustPolicy();
			lastRefreshAt = Date.now();
			if (showToast) showSuccess('Trust policy loaded');
		} catch (error) {
			const message = error instanceof Error ? error.message : 'Failed to load trust policy';
			routeError = message;
			showError(message);
		}
	}

	function startEditing(): void {
		if (!$trustPolicyDocument) return;
		isEditing = true;
		editorDraft = $trustPolicyDocument.content;
		routeError = null;
		clearTrustPolicyError();
	}

	function cancelEditing(): void {
		resetTrustPolicyDraftToPersisted();
		editorDraft = persistedContent;
		isEditing = false;
		routeError = null;
		clearTrustPolicyError();
	}

	async function saveChanges(): Promise<void> {
		routeError = null;
		clearTrustPolicyError();
		try {
			setTrustPolicyDraftContent(editorDraft);
			await saveTrustPolicy();
			lastRefreshAt = Date.now();
			isEditing = false;
			showSuccess('Trust policy saved');
		} catch (error) {
			const message = error instanceof Error ? error.message : 'Failed to save trust policy';
			routeError = message;
			showError(message);
		}
	}

	async function restoreTemplate(): Promise<void> {
		if (browser) {
			const confirmed = await requestConfirmation({
				title: 'Restore trust_policies.yaml?',
				message: 'Replaces current content with trust_policies.template.yaml.',
				confirmLabel: 'Restore',
				destructive: true
			});
			if (!confirmed) return;
		}

		routeError = null;
		clearTrustPolicyError();
		try {
			await restoreTrustPolicyFromTemplate();
			lastRefreshAt = Date.now();
			isEditing = false;
			editorDraft = $trustPolicyDocument?.content || '';
			showSuccess('Trust policy restored from template');
		} catch (error) {
			const message = error instanceof Error ? error.message : 'Failed to restore trust policy';
			routeError = message;
			showError(message);
		}
	}

	async function reloadMagicianConfigFromSettings(): Promise<void> {
		clearMagicianConfigError();
		try {
			const result = await reloadMagicianConfig();
			const liveReloaded = result.live_reloaded.length > 0 ? result.live_reloaded.join(', ') : 'nothing';
			showSuccess(`Magician backend config reloaded: ${liveReloaded}`);
		} catch (error) {
			const message = error instanceof Error ? error.message : 'Failed to reload backend config';
			showError(message);
		}
	}

	async function refreshNotesSettingsFromSettings(showToast: boolean): Promise<void> {
		try {
			await refreshNotesSettings();
			notesFormDirty = false;
			notesFormSignature = '';
			if (showToast) showSuccess('Notes settings loaded');
		} catch (error) {
			const message = error instanceof Error ? error.message : 'Failed to load notes settings';
			showError(message);
		}
	}

	async function saveNotesSettingsFromCurrentForm(): Promise<void> {
		const current = $notesSettingsStore.envelope?.settings ?? defaultNotesSettings();
		const next: NotesProviderSettings = {
			enabled: notesEnabled,
			default_provider: 'silverbullet',
			fallback_provider: 'local_markdown',
			local_markdown: current.local_markdown,
			notes_folder: {
				space_path: notesFolderPath.trim() || null
			},
			task_publishing: {
				auto_publish_completed: notesAutoPublishCompleted,
				default_mode: notesTaskPublishMode,
				include_assets: notesTaskIncludeAssets
			}
		};
		try {
			await saveNotesSettings(next);
			notesFormDirty = false;
			notesFormSignature = '';
			showSuccess('Notes settings saved');
		} catch (error) {
			const message = error instanceof Error ? error.message : 'Failed to save notes settings';
			showError(message);
		}
	}

	async function login(): Promise<void> {
		authBusy = true;
		authMessage = null;
		try {
			authSession = await loginScopeSession({
				username: authUsername.trim(),
				password: authPassword,
				workspace: authWorkspace
			});
			authPassword = '';
			authWorkspace = authSession.workspace;
			showSuccess(`Signed in to ${authSession.workspace}`);
			await Promise.all([refresh(false), refreshNotesSettingsFromSettings(false)]);
		} catch (error) {
			authMessage = error instanceof Error ? error.message : 'Sign-in failed';
		} finally {
			authBusy = false;
		}
	}

	async function switchWorkspace(): Promise<void> {
		authBusy = true;
		authMessage = null;
		try {
			authSession = await switchScopeWorkspace(authWorkspace);
			showSuccess(`Switched to ${authSession.workspace}`);
			await Promise.all([refresh(false), refreshNotesSettingsFromSettings(false)]);
		} catch (error) {
			authMessage = error instanceof Error ? error.message : 'Workspace switch failed';
		} finally {
			authBusy = false;
		}
	}

	async function logout(): Promise<void> {
		authBusy = true;
		await logoutScopeSession();
		authSession = null;
		authBusy = false;
		showSuccess('Signed out');
	}

	// The workspaces panel creates, renames and deletes; the switcher above
	// lists the same set from the session, so re-read it after any change.
	async function refreshAuthSession(): Promise<void> {
		authSession = await refreshScopeSession().catch(() => authSession);
	}

	async function initializeSettings(): Promise<void> {
		authSession = await refreshScopeSession().catch(() => null);
		if (authSession) authWorkspace = authSession.workspace;
		await Promise.all([refresh(false), refreshNotesSettingsFromSettings(false), loadApiMiningSettings()]);
	}

	async function loadApiMiningSettings(): Promise<void> {
		try {
			const response = await fetch('/api/magician/v2/api-mining/settings');
			if (response.ok) apiMiningSettings = await response.json();
		} catch {
			apiMiningSettings = null;
		}
	}

	async function toggleApiMining(): Promise<void> {
		if (!apiMiningSettings) return;
		apiMiningBusy = true;
		try {
			const enabled = !apiMiningSettings.effective;
			const response = await fetch('/api/magician/v2/api-mining/settings', {
				method: 'PUT',
				headers: { 'Content-Type': 'application/json' },
				body: JSON.stringify({ enabled, layer: 'scope' })
			});
			if (!response.ok) throw new Error(`HTTP ${response.status}`);
			const updated = await response.json();
			apiMiningSettings = updated;
			if (enabled && !updated.effective) {
				showError('API mining remains off', 'The process-level configuration is the active ceiling.');
			} else {
				showSuccess(`API mining ${enabled ? 'enabled' : 'disabled'} for this workspace`);
			}
		} catch (error) {
			showError('API mining update failed', error instanceof Error ? error.message : String(error));
		} finally {
			apiMiningBusy = false;
		}
	}

	onMount(() => {
		if (!browser) return;
		void initializeSettings();
	});

	// Navigation and search. Every card stays rendered; search hides the cards
	// that do not match with CSS, so panels keep their state and never refetch.
	let settingsContent: HTMLDivElement | undefined;
	let settingsQuery = '';
	let presentCards = new Set<string>();
	let matchingCards = new Set<string>();
	let activeCategory = SETTINGS_CATEGORIES[0].id;
	let spyPausedUntil = 0;

	$: searching = normalizeSettingsQuery(settingsQuery) !== '';

	// Shown inside the desktop app's Settings window (`embed=1&host=desktop`):
	// the window's This Mac / Magican tabs, and other app pages open outside
	// the window instead of replacing Settings.
	// Inside the iOS or Android app, whose own top bar already says "Magican
	// settings", the page's bold "Settings" heading is redundant.
	$: phoneHost =
		$page.url.searchParams.get('embed') === '1' &&
		['ios', 'android'].includes($page.url.searchParams.get('host') ?? '');
	$: desktopHost =
		browser &&
		$page.url.searchParams.get('embed') === '1' &&
		$page.url.searchParams.get('host') === 'desktop' &&
		'__TAURI_INTERNALS__' in window;
	// Same names as the desktop page's own tab.
	$: desktopDeviceLabel =
		({ Mac: 'Mac', Windows: 'Windows PC', Linux: 'Linux device' } as Record<string, string>)[
			$page.url.searchParams.get('device') ?? ''
		] ?? 'computer';
	let desktopSwitchError: string | null = null;

	async function showDesktopSettings(): Promise<void> {
		desktopSwitchError = null;
		try {
			const { invoke } = await import('@tauri-apps/api/core');
			await invoke('plugin:magician-settings|open_desktop_settings');
		} catch (error) {
			desktopSwitchError = error instanceof Error ? error.message : String(error);
		}
	}

	function routeDesktopHostLinks(): () => void {
		const handleClick = (event: MouseEvent) => {
			if (event.defaultPrevented || event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey) return;
			const anchor = (event.target as Element | null)?.closest?.('a[href]');
			if (!(anchor instanceof HTMLAnchorElement) || anchor.target === '_blank') return;
			const url = new URL(anchor.href, window.location.href);
			if (url.origin !== window.location.origin || url.pathname === '/settings') return;
			event.preventDefault();
			void import('@tauri-apps/api/core')
				.then(({ invoke }) =>
					invoke('plugin:magician-attention|open_application_path', {
						path: `${url.pathname}${url.search}${url.hash}`
					})
				)
				.catch((error) => showError(error instanceof Error ? error.message : String(error)));
		};
		document.addEventListener('click', handleClick, true);
		return () => document.removeEventListener('click', handleClick, true);
	}
	// The Workspaces card only renders with a session, so presence follows it.
	$: void refreshSettingsMatches(settingsQuery, authSession);
	$: navCategories = SETTINGS_CATEGORIES.map(
		(category): SettingsNavCategory => ({
			id: category.id,
			label: category.label,
			count: category.cards.filter(
				(card) => presentCards.has(card.id) && (!searching || matchingCards.has(card.id))
			).length
		})
	);
	$: matchCount = searching ? navCategories.reduce((sum, category) => sum + category.count, 0) : null;

	function categoryLabel(id: string): string {
		return SETTINGS_CATEGORIES.find((category) => category.id === id)?.label ?? id;
	}

	function cardHidden(id: string, isSearching: boolean, matching: Set<string>): boolean {
		return isSearching && !matching.has(id);
	}

	function categoryHidden(id: string, isSearching: boolean, categories: SettingsNavCategory[]): boolean {
		return isSearching && (categories.find((category) => category.id === id)?.count ?? 0) === 0;
	}

	async function refreshSettingsMatches(query: string, _session: unknown): Promise<void> {
		if (!browser) return;
		await tick();
		if (!settingsContent) return;
		const present = new Set<string>();
		const matching = new Set<string>();
		for (const category of SETTINGS_CATEGORIES) {
			for (const card of category.cards) {
				const element = settingsContent.querySelector<HTMLElement>(`[data-settings-card="${card.id}"]`);
				if (!element) continue;
				present.add(card.id);
				if (cardMatches(card, query, element.textContent ?? '')) matching.add(card.id);
			}
		}
		presentCards = present;
		matchingCards = matching;
		// A search that empties the highlighted category moves the highlight to
		// the first category that still has matches.
		const visible = (id: string) =>
			SETTINGS_CATEGORIES.find((category) => category.id === id)?.cards.some(
				(card) => present.has(card.id) && matching.has(card.id)
			) ?? false;
		if (normalizeSettingsQuery(query) && !visible(activeCategory)) {
			activeCategory = SETTINGS_CATEGORIES.find((category) => visible(category.id))?.id ?? activeCategory;
		}
	}

	function scrollBehavior(): ScrollBehavior {
		return window.matchMedia('(prefers-reduced-motion: reduce)').matches ? 'auto' : 'smooth';
	}

	function setSettingsHash(id: string): void {
		try {
			replaceState(`${$page.url.pathname}${$page.url.search}#${id}`, $page.state);
		} catch {
			// Before the router is ready the hash simply stays as it was.
		}
	}

	function selectCategory(id: string): void {
		const section = document.getElementById(id);
		if (!section) return;
		activeCategory = id;
		spyPausedUntil = Date.now() + 900;
		section.scrollIntoView({ behavior: scrollBehavior(), block: 'start' });
		setSettingsHash(id);
	}

	function jumpToFirstMatch(): void {
		const first = SETTINGS_CATEGORIES.flatMap((category) => category.cards).find(
			(card) => presentCards.has(card.id) && (!searching || matchingCards.has(card.id))
		);
		const element = first
			? settingsContent?.querySelector<HTMLElement>(`[data-settings-card="${first.id}"]`)
			: null;
		if (!first || !element) return;
		activeCategory = categoryForHash(first.id) ?? activeCategory;
		spyPausedUntil = Date.now() + 900;
		element.scrollIntoView({ behavior: scrollBehavior(), block: 'start' });
		element.classList.remove('settings-item-flash');
		void element.offsetWidth;
		element.classList.add('settings-item-flash');
	}

	// Scroll-spy: the active category is the last one whose top has passed a
	// line a third of the way down the viewport (below the mobile sticky bar),
	// or the last visible one once the page cannot scroll further.
	function trackActiveCategory(): () => void {
		let frame = 0;
		const update = () => {
			frame = 0;
			if (!settingsContent || Date.now() < spyPausedUntil) return;
			const sections = [...settingsContent.querySelectorAll<HTMLElement>('.settings-category')].filter(
				(section) => section.offsetParent !== null
			);
			if (sections.length === 0) return;
			const line = window.innerHeight / 3;
			let current = sections[0];
			for (const section of sections) {
				if (section.getBoundingClientRect().top <= line) current = section;
			}
			const last = sections[sections.length - 1];
			const lastRect = last.getBoundingClientRect();
			if (lastRect.bottom <= window.innerHeight + 2 && lastRect.top < window.innerHeight) current = last;
			activeCategory = current.id;
		};
		const schedule = () => {
			if (!frame) frame = requestAnimationFrame(update);
		};
		// Scroll events do not bubble; capturing on the document sees the
		// shell's scroll container as well as the window (embedded mode).
		document.addEventListener('scroll', schedule, { capture: true, passive: true });
		window.addEventListener('resize', schedule);
		schedule();
		return () => {
			document.removeEventListener('scroll', schedule, { capture: true });
			window.removeEventListener('resize', schedule);
			if (frame) cancelAnimationFrame(frame);
		};
	}

	// A deep link (`/settings#budgets`) lands before the cards above it have
	// loaded; keep the target aligned while they grow, until the user scrolls
	// or the panels have all loaded.
	function followHashTarget(hash: string): () => void {
		const id = decodeURIComponent(hash.replace(/^#/, ''));
		if (!id || !settingsContent || typeof ResizeObserver === 'undefined') return () => {};
		const align = () => {
			const target =
				document.getElementById(id) ??
				settingsContent?.querySelector<HTMLElement>(`[data-settings-card="${CSS.escape(id)}"]`);
			if (!target) return;
			spyPausedUntil = Date.now() + 400;
			target.scrollIntoView({ block: 'start' });
		};
		const observer = new ResizeObserver(align);
		observer.observe(settingsContent);
		const userEvents = ['wheel', 'touchstart', 'keydown', 'pointerdown'] as const;
		const stop = () => {
			observer.disconnect();
			clearTimeout(timer);
			for (const name of userEvents) window.removeEventListener(name, stop, true);
		};
		// Panels load on demand, so keep following until they all have (and
		// layout has settled once more), at most ten seconds.
		const timer = setTimeout(stop, 10_000);
		void panels.allSettled.then(() => setTimeout(stop, 600));
		for (const name of userEvents) window.addEventListener(name, stop, { capture: true, passive: true });
		align();
		return stop;
	}

	onMount(() => {
		panels.start();
		const hash = window.location.hash;
		const category = categoryForHash(hash);
		let stopFollowing = () => {};
		if (category) {
			activeCategory = category;
			void tick().then(() => {
				stopFollowing = followHashTarget(hash);
			});
		}
		const stopTracking = trackActiveCategory();
		const stopRoutingLinks = desktopHost ? routeDesktopHostLinks() : () => {};
		return () => {
			stopFollowing();
			stopTracking();
			stopRoutingLinks();
		};
	});
</script>

<svelte:head>
	<title>Settings - Magican</title>
</svelte:head>

<div class="settings-page" class:desktop-host={desktopHost}>
	{#if desktopHost}
		<div class="settings-host-tabs" role="tablist" aria-label="Settings">
			<button type="button" role="tab" aria-selected="false" on:click={showDesktopSettings}>This {desktopDeviceLabel}</button>
			<button type="button" role="tab" aria-selected="true" class="active">Magican</button>
			{#if desktopSwitchError}<span class="settings-host-error" role="alert">{desktopSwitchError}</span>{/if}
		</div>
	{/if}

	<SettingsNav
		categories={navCategories}
		activeId={activeCategory}
		bind:query={settingsQuery}
		{matchCount}
		onselect={selectCategory}
		onsubmit={jumpToFirstMatch}
		hideTitle={phoneHost}
	/>

	<div class="settings-content" bind:this={settingsContent}>
		{#if combinedError}
			<div class="settings-alert settings-alert-error" role="alert">{combinedError}</div>
		{/if}

		{#if validationSummary}
			<div class="settings-alert settings-alert-warning" role="alert">{validationSummary}</div>
		{/if}

		{#if searching && matchCount === 0}
			<div class="settings-empty settings-search-empty" role="status">
				<h3>No settings match “{settingsQuery.trim()}”</h3>
				<p>Try another word, or <button class="settings-link-button" type="button" on:click={() => (settingsQuery = '')}>clear the search</button>.</p>
			</div>
		{/if}

		<section id="account" class="settings-category" class:filtered-out={categoryHidden('account', searching, navCategories)} aria-labelledby="settings-category-account">
			<h2 class="settings-category-title" id="settings-category-account">{categoryLabel('account')}</h2>
			<div class="settings-item" data-settings-card="sign-in" class:filtered-out={cardHidden('sign-in', searching, matchingCards)}>
				<section id="workspace" class="settings-card" aria-labelledby="account-scope-title">
					<div class="settings-section-header">
						<div>
							<p class="settings-overline">Account scope</p>
							<h2 id="account-scope-title">Bearer-bound workspace</h2>
							<p>The active principal and workspace come from the opaque session token. Requests cannot override them with headers or query parameters.</p>
						</div>
						{#if authSession}
							<button class="settings-button settings-button-outline" type="button" disabled={authBusy} on:click={logout}>Sign out</button>
						{/if}
					</div>
					{#if authSession}
						<div class="settings-auth-row">
							<div><strong>{authSession.identity.display_name}</strong><br /><span>{authSession.identity.name} · {authSession.workspace}</span></div>
							<label for="auth-workspace">Workspace
								<select id="auth-workspace" bind:value={authWorkspace} disabled={authBusy}>
									{#each authSession.workspaces as workspace}
										<option value={workspace.id}>{workspace.display_name}</option>
									{/each}
								</select>
							</label>
							<button class="settings-button settings-button-primary" type="button" disabled={authBusy || authWorkspace === authSession.workspace} on:click={switchWorkspace}>Switch</button>
						</div>
					{:else}
						<form class="settings-auth-row" on:submit|preventDefault={login}>
							<label for="auth-username">Username<input id="auth-username" autocomplete="username" bind:value={authUsername} required /></label>
							<label for="auth-password">Password<input id="auth-password" type="password" autocomplete="current-password" bind:value={authPassword} required /></label>
							<label for="auth-login-workspace">Workspace<input id="auth-login-workspace" autocomplete="off" bind:value={authWorkspace} required /></label>
							<button class="settings-button settings-button-primary" type="submit" disabled={authBusy}>{authBusy ? 'Signing in…' : 'Sign in'}</button>
						</form>
					{/if}
					{#if authMessage}<div class="settings-alert settings-alert-error" role="alert">{authMessage}</div>{/if}
				</section>
			</div>
			{#if authSession}
				<div class="settings-item" data-settings-card="workspaces" class:filtered-out={cardHidden('workspaces', searching, matchingCards)}>
					<LazyPanel scheduler={panels} category="account" load={() => import('$lib/settings/WorkspacesPanel.svelte')} props={{ currentWorkspace: authSession.workspace, onChange: refreshAuthSession }} />
				</div>
			{/if}
			<div class="settings-item" data-settings-card="service-accounts" class:filtered-out={cardHidden('service-accounts', searching, matchingCards)}>
				<LazyPanel scheduler={panels} category="account" load={() => import('$lib/settings/ServiceAccountsPanel.svelte')} props={{ expandedBySearch: searching }} />
			</div>

		</section>

		<section id="agents" class="settings-category" class:filtered-out={categoryHidden('agents', searching, navCategories)} aria-labelledby="settings-category-agents">
			<h2 class="settings-category-title" id="settings-category-agents">{categoryLabel('agents')}</h2>
			<div class="settings-item" data-settings-card="god-mode" class:filtered-out={cardHidden('god-mode', searching, matchingCards)}>
				<LazyPanel scheduler={panels} category="agents" load={() => import('$lib/settings/GodModePanel.svelte')} />
			</div>
			<div class="settings-item" data-settings-card="trust-policy" class:filtered-out={cardHidden('trust-policy', searching, matchingCards)}>
				<section class="settings-card" aria-labelledby="trust-policy-title">
					<div class="settings-section-header">
						<div>
							<p class="settings-overline">Trust policy</p>
							<h2 id="trust-policy-title">Rules</h2>
							<p>View, edit, validate, and restore system trust policy YAML.</p>
						</div>
						<div class="settings-actions">
							<button class="settings-button settings-button-secondary" type="button" disabled={loading || $trustPolicyStoreState.isSaving || $trustPolicyStoreState.isRestoring || hasDraftChanges} on:click={() => refresh(true)}>
								{loading ? 'Refreshing...' : 'Refresh'}
							</button>
							{#if isEditing}
								<button class="settings-button settings-button-outline" type="button" disabled={$trustPolicyStoreState.isSaving || $trustPolicyStoreState.isRestoring} on:click={cancelEditing}>Cancel</button>
							{:else}
								<button class="settings-button settings-button-outline" type="button" disabled={!$trustPolicyDocument || $trustPolicyStoreState.isSaving || $trustPolicyStoreState.isRestoring} on:click={startEditing}>Edit</button>
							{/if}
							<button class="settings-button settings-button-primary" type="button" disabled={!isEditing || !hasDraftChanges || $trustPolicyStoreState.isSaving || $trustPolicyStoreState.isRestoring} on:click={saveChanges}>
								{$trustPolicyStoreState.isSaving ? 'Saving...' : 'Save'}
							</button>
							<button class="settings-button settings-button-outline" type="button" disabled={loading || $trustPolicyStoreState.isSaving || $trustPolicyStoreState.isRestoring} on:click={restoreTemplate}>
								{$trustPolicyStoreState.isRestoring ? 'Restoring...' : 'Restore template'}
							</button>
						</div>
					</div>

					<dl class="settings-meta-grid">
						<div>
							<dt>Policy path</dt>
							<dd>{$trustPolicyDocument?.path || 'n/a'}</dd>
						</div>
						<div>
							<dt>Template path</dt>
							<dd>{$trustPolicyDocument?.template_path || 'n/a'}</dd>
						</div>
						<div>
							<dt>Validation</dt>
							<dd>{$trustPolicyDocument ? ($trustPolicyDocument.is_valid ? 'valid' : 'invalid') : 'n/a'}</dd>
						</div>
						<div>
							<dt>Last refresh</dt>
							<dd>{formatRelativeTime(lastRefreshAt)}</dd>
						</div>
					</dl>

					{#if loading}
						<div class="settings-empty">
							<h3>Loading trust policy</h3>
							<p>Fetching trust policy configuration from system scope.</p>
						</div>
					{:else if !$trustPolicyDocument}
						<div class="settings-empty">
							<h3>Trust policy unavailable</h3>
							<p>Refresh to retry loading trust policy files.</p>
						</div>
					{:else if isEditing}
						<label class="settings-editor" for="trust-policy-yaml">
							<span>Trust policy YAML</span>
							<textarea id="trust-policy-yaml" bind:value={editorDraft} rows="28" spellcheck="false" disabled={$trustPolicyStoreState.isSaving || $trustPolicyStoreState.isRestoring}></textarea>
						</label>
					{:else}
						<pre class="settings-code" aria-label="Read-only trust policy YAML"><code>{persistedContent}</code></pre>
					{/if}
				</section>
			</div>
			<div class="settings-item" data-settings-card="preferences" class:filtered-out={cardHidden('preferences', searching, matchingCards)}>
				<LazyPanel scheduler={panels} category="agents" load={() => import('$lib/settings/PreferencesLinkCard.svelte')} />
			</div>
			<div class="settings-item" data-settings-card="preference-checks" class:filtered-out={cardHidden('preference-checks', searching, matchingCards)}>
				<LazyPanel scheduler={panels} category="agents" load={() => import('$lib/plane/PreferenceChecksPanel.svelte')} />
			</div>
			<div class="settings-item" data-settings-card="outward-actions" class:filtered-out={cardHidden('outward-actions', searching, matchingCards)}>
				<LazyPanel scheduler={panels} category="agents" load={() => import('$lib/settings/OutwardSettingsPanel.svelte')} />
			</div>
			<div class="settings-item" data-settings-card="budgets" class:filtered-out={cardHidden('budgets', searching, matchingCards)}>
				<LazyPanel scheduler={panels} category="agents" load={() => import('$lib/settings/BudgetsPanel.svelte')} />
			</div>
			<div class="settings-item" data-settings-card="harness-auth" class:filtered-out={cardHidden('harness-auth', searching, matchingCards)}>
				<LazyPanel scheduler={panels} category="agents" load={() => import('$lib/settings/HarnessAuthPanel.svelte')} />
			</div>
			<div class="settings-item" data-settings-card="terminal-grants" class:filtered-out={cardHidden('terminal-grants', searching, matchingCards)}>
				<LazyPanel scheduler={panels} category="agents" load={() => import('$lib/plane/TerminalGrantsPanel.svelte')} />
			</div>
		</section>

		<section id="models" class="settings-category" class:filtered-out={categoryHidden('models', searching, navCategories)} aria-labelledby="settings-category-models">
			<h2 class="settings-category-title" id="settings-category-models">{categoryLabel('models')}</h2>
			<div class="settings-item" data-settings-card="engines" class:filtered-out={cardHidden('engines', searching, matchingCards)}>
				<LazyPanel scheduler={panels} category="models" load={() => import('$lib/plane/EnginesPanel.svelte')} />
			</div>
			<div class="settings-item" data-settings-card="local-generation" class:filtered-out={cardHidden('local-generation', searching, matchingCards)}>
				<LazyPanel scheduler={panels} category="models" load={() => import('$lib/settings/LocalGenerationPanel.svelte')} />
			</div>
			<div class="settings-item" data-settings-card="backend-config" class:filtered-out={cardHidden('backend-config', searching, matchingCards)}>
				<section class="settings-card" aria-labelledby="model-config-title">
					<div class="settings-section-header compact">
						<div>
							<p class="settings-overline">Model config</p>
							<h2 id="model-config-title">Magician backend config</h2>
							<p>Reload routed LLM and execution-policy settings into the running backend.</p>
						</div>
						<div class="settings-actions">
							<a class="settings-button settings-button-secondary" href="/settings/model-routing">Open model routing</a>
							<button class="settings-button settings-button-primary" type="button" disabled={$magicianConfigStoreState.isReloading} on:click={reloadMagicianConfigFromSettings}>
								{$magicianConfigStoreState.isReloading ? 'Reloading...' : 'Reload'}
							</button>
						</div>
					</div>

					<dl class="settings-meta-grid single">
						<div>
							<dt>Config path</dt>
							<dd>{$magicianConfigReloadResult?.path || 'not reloaded yet'}</dd>
						</div>
						<div>
							<dt>Resolved routes</dt>
							<dd>{$magicianConfigReloadResult?.profile_count ?? 'n/a'}</dd>
						</div>
						<div>
							<dt>Operation mappings</dt>
							<dd>{$magicianConfigReloadResult?.operation_mapping_count ?? 'n/a'}</dd>
						</div>
						<div>
							<dt>Last reload</dt>
							<dd>{formatRelativeTime($magicianConfigStoreState.lastReloadedAt)}</dd>
						</div>
						<div>
							<dt>Live reloaded</dt>
							<dd>{joinOrFallback($magicianConfigReloadResult?.live_reloaded)}</dd>
						</div>
						<div>
							<dt>Restart required</dt>
							<dd>{joinOrFallback($magicianConfigReloadResult?.restart_required)}</dd>
						</div>
					</dl>

					{#if $magicianConfigStoreState.error}
						<div class="settings-alert settings-alert-error" role="alert">{$magicianConfigStoreState.error}</div>
					{/if}
					{#if $magicianConfigReloadResult?.warnings?.length}
						<div class="settings-alert settings-alert-warning" role="alert">{joinOrFallback($magicianConfigReloadResult.warnings, '')}</div>
					{/if}
				</section>
			</div>
		</section>

		<section id="voice" class="settings-category" class:filtered-out={categoryHidden('voice', searching, navCategories)} aria-labelledby="settings-category-voice">
			<h2 class="settings-category-title" id="settings-category-voice">{categoryLabel('voice')}</h2>
			<div class="settings-item" data-settings-card="audio-surfaces" class:filtered-out={cardHidden('audio-surfaces', searching, matchingCards)}>
				<LazyPanel scheduler={panels} category="voice" load={() => import('$lib/media/AudioSurfaceSettingsPanel.svelte')} />
			</div>
			<div class="settings-item" data-settings-card="voice-preferences" class:filtered-out={cardHidden('voice-preferences', searching, matchingCards)}>
				<LazyPanel scheduler={panels} category="voice" load={() => import('$lib/media/SharedVoicePreferences.svelte')} />
			</div>
			<div class="settings-item" data-settings-card="tts-voices" class:filtered-out={cardHidden('tts-voices', searching, matchingCards)}>
				<LazyPanel scheduler={panels} category="voice" load={() => import('$lib/media/TtsVoiceSettingsPanel.svelte')} />
			</div>
			<div class="settings-item" data-settings-card="live-call-voices" class:filtered-out={cardHidden('live-call-voices', searching, matchingCards)}>
				<LazyPanel scheduler={panels} category="voice" load={() => import('$lib/media/LiveCallVoicePanel.svelte')} />
			</div>
			<div class="settings-item" data-settings-card="whatsapp-calls" class:filtered-out={cardHidden('whatsapp-calls', searching, matchingCards)}>
				<LazyPanel scheduler={panels} category="voice" load={() => import('$lib/media/WhatsAppCallSettings.svelte')} />
			</div>
			<div class="settings-item" data-settings-card="meet-calls" class:filtered-out={cardHidden('meet-calls', searching, matchingCards)}>
				<LazyPanel scheduler={panels} category="voice" load={() => import('$lib/media/MeetCallSettings.svelte')} />
			</div>
		</section>

		<section id="notifications" class="settings-category" class:filtered-out={categoryHidden('notifications', searching, navCategories)} aria-labelledby="settings-category-notifications">
			<h2 class="settings-category-title" id="settings-category-notifications">{categoryLabel('notifications')}</h2>
			<div class="settings-item" data-settings-card="browser-notifications" class:filtered-out={cardHidden('browser-notifications', searching, matchingCards)}>
				<LazyPanel scheduler={panels} category="notifications" load={() => import('$lib/settings/BrowserNotificationsPanel.svelte')} />
			</div>
			<div class="settings-item" data-settings-card="owner-identities" class:filtered-out={cardHidden('owner-identities', searching, matchingCards)}>
				<LazyPanel scheduler={panels} category="notifications" load={() => import('$lib/settings/OwnerIdentitiesPanel.svelte')} />
			</div>
			<div class="settings-item" data-settings-card="critical-delivery" class:filtered-out={cardHidden('critical-delivery', searching, matchingCards)}>
				<LazyPanel scheduler={panels} category="notifications" load={() => import('$lib/settings/CriticalDeliveryPanel.svelte')} />
			</div>
		</section>

		<section id="devices" class="settings-category" class:filtered-out={categoryHidden('devices', searching, navCategories)} aria-labelledby="settings-category-devices">
			<h2 class="settings-category-title" id="settings-category-devices">{categoryLabel('devices')}</h2>
			<div class="settings-item" data-settings-card="machines" class:filtered-out={cardHidden('machines', searching, matchingCards)}>
				<LazyPanel scheduler={panels} category="devices" load={() => import('$lib/devices/MachinesPanel.svelte')} />
			</div>
			<div class="settings-item" data-settings-card="device-pairing" class:filtered-out={cardHidden('device-pairing', searching, matchingCards)}>
				<LazyPanel scheduler={panels} category="devices" load={() => import('$lib/devices/DevicePairingPanel.svelte')} />
			</div>
		</section>

		<section id="data" class="settings-category" class:filtered-out={categoryHidden('data', searching, navCategories)} aria-labelledby="settings-category-data">
			<h2 class="settings-category-title" id="settings-category-data">{categoryLabel('data')}</h2>
			<div class="settings-item" data-settings-card="workspace-storage" class:filtered-out={cardHidden('workspace-storage', searching, matchingCards)}>
				<LazyPanel scheduler={panels} category="data" load={() => import('$lib/settings/WorkspaceStoragePanel.svelte')} />
			</div>
			<div class="settings-item" data-settings-card="storage-governance" class:filtered-out={cardHidden('storage-governance', searching, matchingCards)}>
				<section class="settings-card" aria-labelledby="storage-settings-title">
					<div class="settings-section-header compact">
						<div>
							<p class="settings-overline">Local data</p>
							<h2 id="storage-settings-title">Storage governance</h2>
							<p>Inspect every owned store, understand its retention policy, and run verified maintenance.</p>
						</div>
						<a class="settings-button settings-button-primary" href="/storage">Open storage</a>
					</div>
					<dl class="settings-meta-grid single">
						<div><dt>Mail and threads</dt><dd>Lossless physical compaction only</dd></div>
						<div><dt>Analytics telemetry</dt><dd>Verified daily compaction · 90-day retention</dd></div>
						<div><dt>Recovery state</dt><dd>Protected from generic cleanup</dd></div>
					</dl>
				</section>
			</div>
			<div class="settings-item" data-settings-card="notes-folder" class:filtered-out={cardHidden('notes-folder', searching, matchingCards)}>
				<section class="settings-card" aria-labelledby="notes-provider-title">
					<div class="settings-section-header">
						<div>
							<p class="settings-overline">Notes</p>
							<h2 id="notes-provider-title">Notes folder</h2>
							<p>Where {PRODUCT_NAME} saves user-visible notes, captures, and published task pages.</p>
						</div>
						<button class="settings-button settings-button-secondary" type="button" disabled={$notesSettingsStore.isLoading || $notesSettingsStore.isSaving} on:click={() => refreshNotesSettingsFromSettings(true)}>
							{$notesSettingsStore.isLoading ? 'Refreshing...' : 'Refresh notes'}
						</button>
					</div>

					<div class="settings-alert settings-alert-warning" role="note">
						Changing the folder affects where new notes are saved. Existing notes are not deleted, but can appear lost until you switch back or migrate them.
					</div>

					<dl class="settings-meta-grid">
						<div>
							<dt>Settings file</dt>
							<dd>{$notesSettingsStore.envelope?.settings_path || 'not loaded'}</dd>
						</div>
						<div>
							<dt>Notes folder</dt>
							<dd>{notesResolved?.notes_folder_path || 'not configured'}</dd>
						</div>
					</dl>

					{#if notesFolderStatus}
						<div class="settings-provider-list" aria-label="Notes folder status">
							<div class="settings-provider-row">
								<div>
									<strong>{notesFolderStatus.label}</strong>
									<span>{notesFolderStatus.root || 'n/a'}</span>
								</div>
								<span class={`settings-badge badge-${statusTone(notesFolderStatus)}`}>
									{notesFolderStatus.writable ? 'ready' : notesFolderStatus.message || 'unavailable'}
								</span>
							</div>
						</div>
					{:else if !notesStatus}
						<div class="settings-empty compact">
							<h3>Notes folder status unavailable</h3>
							<p>Refresh notes settings to check the folder.</p>
						</div>
					{/if}

					{#if notesStatus && notesUsingFallback}
						<div class="settings-alert settings-alert-warning" role="note">
							New notes are going to the fallback folder {notesResolved?.local_markdown_root || ''} because the notes folder is not in use or not writable. Save a notes folder below to switch back.
						</div>
					{/if}

					<form class="settings-form" on:submit|preventDefault={saveNotesSettingsFromCurrentForm}>
						<label class="settings-checkbox-row">
							<input type="checkbox" bind:checked={notesEnabled} disabled={$notesSettingsStore.isLoading || $notesSettingsStore.isSaving} on:change={markNotesFormDirty} />
							<span>Enable notes capture</span>
						</label>
						<label class="settings-field" for="notes-folder-path">
							<span>Notes folder path</span>
							<input id="notes-folder-path" bind:value={notesFolderPath} disabled={$notesSettingsStore.isLoading || $notesSettingsStore.isSaving} placeholder="$MAGICIAN_ROOT_DIR/MagicanNotes/spaces/<principal>/<workspace>" on:input={markNotesFormDirty} />
						</label>
						<label class="settings-field" for="notes-task-publish-mode">
							<span>Completed task page detail</span>
							<select id="notes-task-publish-mode" bind:value={notesTaskPublishMode} disabled={$notesSettingsStore.isLoading || $notesSettingsStore.isSaving} on:change={markNotesFormDirty}>
								<option value="compact">Compact</option>
								<option value="standard">Standard</option>
								<option value="diagnostic">Diagnostic (explicit raw bundle)</option>
							</select>
							<small>Standard includes the final answer, a bounded run timeline, important outputs, and selected assets. Diagnostic may include raw runtime metadata.</small>
						</label>
						<label class="settings-checkbox-row">
							<input type="checkbox" bind:checked={notesTaskIncludeAssets} disabled={$notesSettingsStore.isLoading || $notesSettingsStore.isSaving} on:change={markNotesFormDirty} />
							<span>Copy selected task files and screenshots into the note’s asset folder</span>
						</label>
						<label class="settings-checkbox-row">
							<input type="checkbox" bind:checked={notesAutoPublishCompleted} disabled={$notesSettingsStore.isLoading || $notesSettingsStore.isSaving} on:change={markNotesFormDirty} />
							<span>Automatically publish completed tasks after their final output settles</span>
						</label>
						<div class="settings-form-actions">
							<button class="settings-button settings-button-primary" type="submit" disabled={$notesSettingsStore.isLoading || $notesSettingsStore.isSaving || (!notesFormDirty && notesSettings.default_provider === 'silverbullet')}>
								{$notesSettingsStore.isSaving ? 'Saving...' : 'Save notes settings'}
							</button>
						</div>
					</form>

					{#if $notesSettingsStore.error}
						<div class="settings-alert settings-alert-error" role="alert">{$notesSettingsStore.error}</div>
					{/if}
					{#if notesWarnings.length > 0}
						<div class="settings-alert settings-alert-warning" role="alert">{notesWarnings.join(', ')}</div>
					{/if}
				</section>
			</div>
			<div class="settings-item" data-settings-card="api-mining" class:filtered-out={cardHidden('api-mining', searching, matchingCards)}>
				<section class="settings-card" aria-labelledby="api-mining-settings-title">
					<div class="settings-section-header">
						<div>
							<p class="settings-overline">Privacy and automation</p>
							<h2 id="api-mining-settings-title">{apiMiningSettings ? `API mining is ${apiMiningSettings.effective ? 'on' : 'off'}` : 'Loading API mining state'}</h2>
							<p>When on, Magican learns APIs from its own browser runs so repeated tasks can finish without opening a browser. Off stops capture, auth learning, compilation, and replay; existing data remains available to review or delete on the API Mining page.</p>
							{#if apiMiningSettings}<p>Set by: {apiMiningSettings.set_by}.</p>{/if}
						</div>
						<div class="settings-actions">
							<button class="settings-button settings-button-primary" type="button" disabled={!apiMiningSettings || apiMiningBusy || (!apiMiningSettings.process_enabled && !apiMiningSettings.effective)} on:click={toggleApiMining}>
								{apiMiningBusy ? 'Updating…' : !apiMiningSettings?.process_enabled && apiMiningSettings?.effective === false ? 'Disabled by process config' : apiMiningSettings?.effective === false ? 'Turn on' : 'Turn off'}
							</button>
							<a class="settings-button settings-button-secondary" href="/api-mining">Review learned data</a>
						</div>
					</div>
				</section>
			</div>
		</section>

		<section id="about" class="settings-category" class:filtered-out={categoryHidden('about', searching, navCategories)} aria-labelledby="settings-category-about">
			<h2 class="settings-category-title" id="settings-category-about">{categoryLabel('about')}</h2>
			<div class="settings-item" data-settings-card="versions" class:filtered-out={cardHidden('versions', searching, matchingCards)}>
				<LazyPanel scheduler={panels} category="about" load={() => import('$lib/settings/VersionsPanel.svelte')} />
			</div>
		</section>
	</div>
</div>

<style>
	.settings-page {
		align-items: start;
		box-sizing: border-box;
		color: var(--text-primary);
		display: grid;
		gap: 1.5rem;
		grid-template-columns: 15.5rem minmax(0, 1fr);
		margin: 0 auto;
		max-width: var(--app-content-max, 1320px);
		padding: 1.35rem 1.45rem 5rem;
		width: 100%;
	}

	.settings-host-tabs {
		align-items: center;
		border-bottom: 1px solid var(--border-soft);
		display: flex;
		flex-wrap: wrap;
		gap: 0.35rem;
		grid-column: 1 / -1;
		padding-bottom: 0.75rem;
	}

	.settings-host-tabs button {
		background: transparent;
		border: 1px solid transparent;
		border-radius: 6px;
		color: var(--text-secondary);
		cursor: pointer;
		font: inherit;
		font-size: 0.9rem;
		font-weight: 600;
		padding: 0.45rem 0.9rem;
	}

	.settings-host-tabs button:hover {
		color: var(--text-primary);
	}

	.settings-host-tabs button.active {
		background: color-mix(in srgb, var(--accent-primary) 14%, var(--bg-soft));
		border-color: color-mix(in srgb, var(--accent-primary) 30%, var(--border-soft));
		color: var(--text-primary);
		cursor: default;
	}

	.settings-host-error {
		color: var(--danger, #c2410c);
		font-size: 0.82rem;
	}

	.settings-content {
		display: flex;
		flex-direction: column;
		gap: 2.25rem;
		min-width: 0;
	}

	.settings-category {
		display: flex;
		flex-direction: column;
		gap: 1rem;
		min-width: 0;
	}

	.settings-category-title {
		border-bottom: 1px solid var(--border-soft);
		color: var(--text-primary);
		font-family: var(--font-display, var(--font-primary));
		font-size: 1.3rem;
		font-weight: 700;
		letter-spacing: 0;
		line-height: 1.2;
		margin: 0;
		padding-bottom: 0.5rem;
	}

	.settings-item {
		border-radius: 8px;
		min-width: 0;
	}

	/* One control scale for every card: compact rounded-square action
	   buttons and fields. The input selector repeats app.css's nine-type
	   exclusion chain so it outranks that global rule (specificity 0,9,1). Compact families keep their own sizes — choice
	   triggers, thinking steps, surface tabs, voice pickers, tab strips,
	   chips, toggles and icon buttons. Fields only lose vertical padding, so
	   custom select arrows keep their horizontal room. */
	.settings-content
		:global(
			[data-settings-card]
				:is(button, a.settings-button, a.open-link, a.btn):not(.choice-trigger):not([role='tab']):not(
					[role='switch']
				):not([role='combobox']):not([class*='icon']):not([class*='chip']):not([class*='toggle']):not(
					[class*='pill']
				):not([class*='tab']):not(
					:is(
							.thinking-steps,
							.thinking-heading,
							.surface-tabs,
							.voice-persona-picker,
							.order,
							[role='tablist'],
							[role='radiogroup']
						)
						*
				)
		) {
		border-radius: 6px;
		font-size: 0.8125rem;
		height: auto;
		line-height: 1.2;
		min-height: 2rem;
		padding: 0.375rem 0.75rem;
	}

	/* Card headers made of a text block and a button group: the buttons sit
	   beside the title, and the description runs the full width under both
	   instead of wrapping in the space left of the buttons. The text block is
	   flattened into the grid (display: contents) so its overline and title
	   take the first column and its description spans both. Only headers
	   with exactly those two parts qualify, and only when the second part is
	   not a description paragraph. (`:has()` cannot nest, so this cannot
	   test for buttons inside it.) */
	.settings-content
		:global(
			[data-settings-card]
				:is(.settings-section-header, .header, .panel-header, .god-header, .super-god):not(.compact):has(
					> :nth-child(2):last-child:not(p)
				)
		) {
		align-items: start;
		column-gap: 1rem;
		display: grid;
		grid-template-columns: minmax(0, 1fr) auto;
		row-gap: 0;
	}

	.settings-content
		:global(
			[data-settings-card]
				:is(.settings-section-header, .header, .panel-header, .god-header, .super-god):not(.compact):has(
					> :nth-child(2):last-child:not(p)
				)
				> :first-child
		) {
		display: contents;
	}

	.settings-content
		:global(
			[data-settings-card]
				:is(.settings-section-header, .header, .panel-header, .god-header, .super-god):not(.compact):has(
					> :nth-child(2):last-child:not(p)
				)
				> :first-child
				> *
		) {
		grid-column: 1;
		min-width: 0;
	}

	.settings-content
		:global(
			[data-settings-card]
				:is(.settings-section-header, .header, .panel-header, .god-header, .super-god):not(.compact):has(
					> :nth-child(2):last-child:not(p)
				)
				> :first-child
				> :is(p, small, ul, dl, div):not([class*='overline']):not(:first-child)
		) {
		grid-column: 1 / -1;
	}

	.settings-content
		:global(
			[data-settings-card]
				:is(.settings-section-header, .header, .panel-header, .god-header, .super-god):not(.compact):has(
					> :nth-child(2):last-child:not(p)
				)
				> :last-child
		) {
		align-self: start;
		flex: none;
		grid-column: 2;
		grid-row: 1 / span 2;
		justify-content: flex-end;
		max-width: none;
	}

	@media (max-width: 640px) {
		.settings-content
			:global(
				[data-settings-card]
					:is(.settings-section-header, .header, .panel-header, .god-header, .super-god):not(.compact):has(
						> :nth-child(2):last-child:not(p)
					)
					> :last-child
			) {
			grid-column: 1 / -1;
			grid-row: auto;
			justify-content: flex-start;
			margin-top: 0.5rem;
		}
	}

	/* The label above every card title draws a line over its text. Panels
	   name that label differently (`overline`, `settings-overline`,
	   `eyebrow`, ...) and only the plain `overline` class got the line from
	   the global utility, so the rule keys on its place before the title. */
	.settings-content
		:global([data-settings-card] :is([class*='overline'], .eyebrow):has(+ :is(h2, h3))) {
		text-decoration-line: overline;
	}

	/* A dropdown never needs the whole card width. */
	.settings-content :global([data-settings-card] select) {
		max-width: 28rem;
	}

	/* Reorder arrows and similar inline steppers: the small variant. */
	.settings-content :global([data-settings-card] .order button) {
		border-radius: 6px;
		font-size: 0.8125rem;
		line-height: 1;
		min-height: 1.75rem;
		padding: 0.25rem 0.5rem;
	}

	.settings-content
		:global(
			[data-settings-card]
				:is(
					select,
					input:not([type='checkbox']):not([type='radio']):not([type='file']):not([type='range']):not(
							[type='color']
						):not([type='button']):not([type='submit']):not([type='reset']):not([type='image']):not(
							[type='hidden']
						)
				)
		) {
		border-radius: 6px;
		box-sizing: border-box;
		font-size: 0.8125rem;
		height: 2rem;
		line-height: 1.2;
		min-height: 2rem;
		padding-bottom: 0;
		padding-top: 0;
	}

	/* Touch screens (the phone apps' embedded Settings) zoom into fields
	   under 16px, so fields keep 16px there. */
	@media (hover: none) and (pointer: coarse) {
		.settings-content
			:global(
				[data-settings-card]
					:is(
						select,
						input:not([type='checkbox']):not([type='radio']):not([type='file']):not([type='range']):not(
								[type='color']
							):not([type='button']):not([type='submit']):not([type='reset']):not([type='image']):not(
								[type='hidden']
							)
					)
			) {
			font-size: 1rem;
			height: 2.25rem;
		}
	}

	/* Search hides cards (and empty categories) without unmounting them. */
	.filtered-out {
		display: none !important;
	}

	.settings-category,
	.settings-item,
	.settings-content :global(section[id]) {
		scroll-margin-top: 1rem;
	}

	.settings-item:global(.settings-item-flash) {
		animation: settings-item-flash 1.6s ease-out;
	}

	@keyframes settings-item-flash {
		0%,
		35% {
			box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent-primary) 45%, transparent);
		}
		100% {
			box-shadow: 0 0 0 3px transparent;
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.settings-item:global(.settings-item-flash) {
			animation: none;
			box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent-primary) 45%, transparent);
		}
	}

	.settings-search-empty h3 {
		margin-bottom: 0.3rem;
	}

	.settings-link-button {
		background: none;
		border: 0;
		color: var(--accent-primary);
		cursor: pointer;
		font: inherit;
		padding: 0;
		text-decoration: underline;
	}

	.settings-card,
	.settings-empty {
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
	}

	.settings-card {
		padding: 1rem;
	}

	.settings-overline,
	.settings-card h2,
	.settings-empty h3,
	.settings-card p,
	.settings-empty p {
		letter-spacing: 0;
		margin: 0;
	}

	.settings-overline {
		color: var(--text-secondary);
		font-size: 0.75rem;
		font-weight: 700;
		text-transform: uppercase;
	}

	/* <small> otherwise renders at the browser's relative default, which
	   lands under the type floor in 13px contexts. */
	.settings-card small {
		color: var(--text-secondary);
		font-size: var(--text-2xs, 0.72rem);
		line-height: 1.45;
	}

	.settings-card p,
	.settings-empty p {
		color: var(--text-secondary);
		font-size: 0.92rem;
		line-height: 1.5;
	}

	.settings-card h2,
	.settings-empty h3 {
		color: var(--text-primary);
		font-size: 1.05rem;
		font-weight: 600;
		line-height: 1.25;
	}

	.settings-actions,
	.settings-form-actions {
		align-items: center;
		display: flex;
		flex-wrap: wrap;
		gap: 0.65rem;
	}

	.settings-card {
		display: flex;
		flex-direction: column;
		gap: 1rem;
	}

	.settings-section-header {
		align-items: flex-start;
		display: flex;
		gap: 1rem;
		justify-content: space-between;
	}

	.settings-section-header.compact {
		flex-direction: column;
	}

	.settings-meta-grid {
		display: grid;
		gap: 0.75rem;
		grid-template-columns: repeat(auto-fit, minmax(15rem, 1fr));
		margin: 0;
	}

	/* "single" used to force one full-width column per fact, which read as a
	   very airy stack for the long backend-config lists. Auto-fit keeps long
	   values wrapping inside their own cell instead. */
	.settings-meta-grid.single {
		grid-template-columns: repeat(auto-fit, minmax(16rem, 1fr));
	}

	.settings-meta-grid > div {
		border-left: 2px solid color-mix(in srgb, var(--accent-primary) 42%, var(--border-soft));
		min-width: 0;
		padding-left: 0.75rem;
	}

	.settings-meta-grid dt {
		color: var(--text-secondary);
		font-size: 0.74rem;
		font-weight: 700;
		margin: 0;
	}

	.settings-meta-grid dd {
		color: var(--text-primary);
		font-size: 0.88rem;
		line-height: 1.4;
		margin: 0.15rem 0 0;
		overflow-wrap: anywhere;
	}

	.settings-alert {
		border-radius: 8px;
		font-size: 0.88rem;
		line-height: 1.45;
		padding: 0.75rem 0.85rem;
	}

	.settings-alert-error {
		background: color-mix(in srgb, var(--danger, #c2410c) 10%, var(--bg-card));
		border: 1px solid color-mix(in srgb, var(--danger, #c2410c) 30%, var(--border-soft));
		color: var(--text-primary);
	}

	.settings-alert-warning {
		background: color-mix(in srgb, var(--warning, #b7791f) 12%, var(--bg-card));
		border: 1px solid color-mix(in srgb, var(--warning, #b7791f) 34%, var(--border-soft));
		color: var(--text-primary);
	}

	.settings-empty {
		background: color-mix(in srgb, var(--bg-card) 90%, var(--bg-soft) 10%);
		padding: 0.85rem;
	}

	.settings-empty.compact {
		padding: 0.7rem;
	}

	.settings-code {
		background: color-mix(in srgb, var(--bg-card) 90%, var(--bg-soft) 10%);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		color: var(--text-primary);
		font-family: var(--font-mono);
		font-size: 0.82rem;
		line-height: 1.5;
		margin: 0;
		max-height: min(40vh, 24rem);
		overflow: auto;
		padding: 0.85rem;
		white-space: pre;
	}

	.settings-editor,
	.settings-field {
		color: var(--text-primary);
		display: flex;
		flex-direction: column;
		font-size: 0.82rem;
		font-weight: 700;
		gap: 0.4rem;
	}

	.settings-editor textarea,
	.settings-field input,
	.settings-field select {
		background: color-mix(in srgb, var(--bg-card) 92%, var(--bg-soft) 8%);
		border: 1px solid var(--border-soft);
		border-radius: 6px;
		color: var(--text-primary);
		font: inherit;
		font-size: 0.9rem;
		line-height: 1.35;
		padding: 0.55rem 0.65rem;
		width: 100%;
	}

	.settings-editor textarea {
		font-family: var(--font-mono);
		min-height: 28rem;
		resize: vertical;
	}

	.settings-form {
		display: grid;
		gap: 0.85rem;
		grid-template-columns: repeat(2, minmax(0, 1fr));
	}

	.settings-auth-row {
		align-items: end;
		display: grid;
		gap: 0.85rem;
		grid-template-columns: repeat(3, minmax(0, 1fr)) auto;
	}

	.settings-auth-row label {
		color: var(--text-primary);
		display: flex;
		flex-direction: column;
		font-size: 0.82rem;
		font-weight: 700;
		gap: 0.4rem;
	}

	/* The Switch button keeps its own width instead of filling a column. */
	.settings-auth-row > .settings-button {
		justify-self: start;
	}

	.settings-auth-row input,
	.settings-auth-row select {
		background: color-mix(in srgb, var(--bg-card) 92%, var(--bg-soft) 8%);
		border: 1px solid var(--border-soft);
		border-radius: 6px;
		color: var(--text-primary);
		font: inherit;
		padding: 0.55rem 0.65rem;
	}

	.settings-checkbox-row {
		align-items: center;
		color: var(--text-secondary);
		display: flex;
		font-size: 0.84rem;
		gap: 0.65rem;
	}

	.settings-checkbox-row input {
		accent-color: var(--accent-primary);
		height: 1rem;
		width: 1rem;
	}

	.settings-form-actions {
		grid-column: 1 / -1;
	}

	.settings-provider-list {
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		overflow: hidden;
	}

	.settings-provider-row {
		align-items: flex-start;
		display: flex;
		gap: 1rem;
		justify-content: space-between;
		padding: 0.7rem 0.75rem;
	}

	.settings-provider-row strong,
	.settings-provider-row span {
		display: block;
	}

	.settings-provider-row div > span {
		color: var(--text-secondary);
		font-size: 0.78rem;
		line-height: 1.4;
		margin-top: 0.15rem;
		overflow-wrap: anywhere;
	}

	.settings-button {
		align-items: center;
		border: 1px solid transparent;
		border-radius: 6px;
		cursor: pointer;
		display: inline-flex;
		font: inherit;
		font-size: 0.86rem;
		font-weight: 600;
		justify-content: center;
		line-height: 1.2;
		min-height: 2.25rem;
		padding: 0.55rem 0.8rem;
		white-space: nowrap;
	}

	.settings-button-primary {
		background: var(--accent-fill);
		border-color: var(--accent-primary);
		color: var(--accent-on-primary, #fff);
	}

	.settings-button-secondary {
		background: color-mix(in srgb, var(--accent-primary) 14%, var(--bg-soft));
		border-color: color-mix(in srgb, var(--accent-primary) 30%, var(--border-soft));
		color: var(--text-primary);
	}

	.settings-button-outline {
		background: transparent;
		border-color: var(--border-soft);
		color: var(--text-primary);
	}

	.settings-button:disabled,
	.settings-field input:disabled,
	.settings-field select:disabled,
	.settings-editor textarea:disabled {
		cursor: not-allowed;
		opacity: 0.58;
	}

	.settings-badge {
		align-items: center;
		background: color-mix(in srgb, var(--bg-card) 88%, var(--bg-soft) 12%);
		border: 1px solid var(--border-soft);
		border-radius: 999px;
		color: var(--text-secondary);
		display: inline-flex;
		font-size: 0.76rem;
		font-weight: 700;
		line-height: 1.2;
		max-width: 100%;
		min-height: 1.65rem;
		padding: 0.28rem 0.55rem;
		white-space: normal;
	}

	.badge-info {
		background: color-mix(in srgb, var(--accent-primary) 12%, transparent);
		border-color: color-mix(in srgb, var(--accent-primary) 28%, var(--border-soft));
		color: color-mix(in srgb, var(--accent-primary) 75%, var(--text-primary));
	}

	.badge-success {
		background: color-mix(in srgb, var(--success, #12805c) 14%, transparent);
		border-color: color-mix(in srgb, var(--success, #12805c) 34%, var(--border-soft));
		color: color-mix(in srgb, var(--success, #12805c) 75%, var(--text-primary));
	}

	.badge-warning {
		background: color-mix(in srgb, var(--warning, #b7791f) 14%, transparent);
		border-color: color-mix(in srgb, var(--warning, #b7791f) 34%, var(--border-soft));
		color: color-mix(in srgb, var(--warning, #b7791f) 82%, var(--text-primary));
	}

	.badge-error {
		background: color-mix(in srgb, var(--danger, #c2410c) 14%, transparent);
		border-color: color-mix(in srgb, var(--danger, #c2410c) 34%, var(--border-soft));
		color: color-mix(in srgb, var(--danger, #c2410c) 82%, var(--text-primary));
	}

	@media (max-width: 980px) {
		.settings-form,
		.settings-auth-row {
			grid-template-columns: minmax(0, 1fr);
		}

		.settings-section-header {
			flex-direction: column;
		}
	}

	/* Narrow screens: one column under the nav's sticky top bar, which bleeds
	   to the edges through the page's 1rem gutter. */
	@media (max-width: 899px) {
		.settings-page {
			gap: 1rem;
			grid-template-columns: minmax(0, 1fr);
			padding: 0 1rem 4.5rem;
		}

		.settings-page.desktop-host {
			padding-top: 0.75rem;
		}

		.settings-host-tabs {
			padding-bottom: 0;
		}

		.settings-content {
			gap: 1.75rem;
		}

		.settings-category,
		.settings-item,
		.settings-content :global(section[id]) {
			scroll-margin-top: 7rem;
		}
	}

	@media (max-width: 640px) {
		.settings-category-title {
			font-size: 1.15rem;
		}

		.settings-provider-row {
			flex-direction: column;
		}

		.settings-button {
			white-space: normal;
		}

		.settings-actions > .settings-button {
			flex: 1 1 auto;
		}

		.settings-editor textarea {
			min-height: 18rem;
		}
	}
</style>
