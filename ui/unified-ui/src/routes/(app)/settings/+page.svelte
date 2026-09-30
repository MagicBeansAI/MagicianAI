<script lang="ts">
	import { PRODUCT_NAME } from '$lib/presentationIdentity';
	import { browser } from '$app/environment';
	import { onMount } from 'svelte';
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
		type NotesProviderId,
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
	import AudioSurfaceSettingsPanel from '$lib/media/AudioSurfaceSettingsPanel.svelte';
	import SharedVoicePreferences from '$lib/media/SharedVoicePreferences.svelte';
	import LiveCallVoicePanel from '$lib/media/LiveCallVoicePanel.svelte';
	import WorkspaceStoragePanel from '$lib/settings/WorkspaceStoragePanel.svelte';
	import DevicePairingPanel from '$lib/devices/DevicePairingPanel.svelte';
	import TerminalGrantsPanel from '$lib/plane/TerminalGrantsPanel.svelte';
	import EnginesPanel from '$lib/plane/EnginesPanel.svelte';
	import LocalGenerationPanel from '$lib/settings/LocalGenerationPanel.svelte';
	import CriticalDeliveryPanel from '$lib/settings/CriticalDeliveryPanel.svelte';
	import WorkspacesPanel from '$lib/settings/WorkspacesPanel.svelte';
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
	let notesDefaultProvider: NotesProviderId = 'local_markdown';
	let notesLocalRoot = '';
	let notesSilverbulletSpacePath = '';
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
			silverbullet: {
				local_url: 'http://127.0.0.1:3021',
				server_url: 'http://127.0.0.1:3021'
			},
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
			settings.silverbullet.space_path ?? '',
			settings.silverbullet.local_url ?? '',
			settings.silverbullet.public_origin ?? '',
			settings.silverbullet.server_url ?? '',
			settings.task_publishing.auto_publish_completed,
			settings.task_publishing.default_mode,
			settings.task_publishing.include_assets
		]);
	}

	function syncNotesFormFromSettings(settings: NotesProviderSettings): void {
		const signature = notesSettingsSignature(settings);
		if (notesFormDirty || signature === notesFormSignature) return;
		notesEnabled = settings.enabled;
		notesDefaultProvider = settings.default_provider;
		notesLocalRoot = settings.local_markdown.root ?? '';
		notesSilverbulletSpacePath = settings.silverbullet.space_path ?? '';
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

	function providerLabel(provider: NotesProviderId | string | undefined): string {
		if (provider === 'silverbullet') return 'Notes folder';
		return 'Local Markdown';
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
			default_provider: notesDefaultProvider,
			fallback_provider: 'local_markdown',
			local_markdown: {
				root: notesLocalRoot.trim() || null
			},
			silverbullet: {
				space_path: notesSilverbulletSpacePath.trim() || null,
				local_url: current.silverbullet.local_url,
				server_url: current.silverbullet.server_url,
				public_origin: current.silverbullet.public_origin
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
			showSuccess('Notes provider settings saved');
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
</script>

<svelte:head>
	<title>Settings - Magican</title>
</svelte:head>

<div class="settings-page">
	<section class="settings-hero">
		<div>
			<p class="settings-overline">Settings</p>
			<h1>Rules and providers</h1>
			<p>Manage voice and audio, trust policy, on-device generation, model configuration, notes, and workspace storage.</p>
			<div class="settings-chip-row">
				<span class:badge-success={$trustPolicyDocument?.is_valid} class:badge-warning={$trustPolicyDocument && !$trustPolicyDocument.is_valid} class="settings-badge">
					Trust {$trustPolicyDocument ? ($trustPolicyDocument.is_valid ? 'valid' : 'invalid') : 'not loaded'}
				</span>
				<span class="settings-badge badge-info">Notes {providerLabel(notesSettings.default_provider)}</span>
				<span class="settings-badge">Updated {formatRelativeTime(lastRefreshAt)}</span>
			</div>
		</div>
	</section>

	{#if combinedError}
		<div class="settings-alert settings-alert-error" role="alert">{combinedError}</div>
	{/if}

	{#if validationSummary}
		<div class="settings-alert settings-alert-warning" role="alert">{validationSummary}</div>
	{/if}

	<section id="workspace" class="settings-card settings-card-wide" aria-labelledby="account-scope-title">
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

	{#if authSession}
		<WorkspacesPanel currentWorkspace={authSession.workspace} onChange={refreshAuthSession} />
	{/if}

	<EnginesPanel />

	<AudioSurfaceSettingsPanel />

	<SharedVoicePreferences />

	<LiveCallVoicePanel />

	<DevicePairingPanel />

	<TerminalGrantsPanel />

	<section class="settings-card settings-card-wide" aria-labelledby="api-mining-settings-title">
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

	<section class="settings-card settings-card-wide" aria-labelledby="trust-policy-title">
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

	<LocalGenerationPanel />

	<CriticalDeliveryPanel />

	<WorkspaceStoragePanel />

	<div class="settings-grid">
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
					<dt>Profiles</dt>
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

		<section class="settings-card settings-card-wide" aria-labelledby="notes-provider-title">
			<div class="settings-section-header">
				<div>
					<p class="settings-overline">Notes</p>
					<h2 id="notes-provider-title">Notes provider</h2>
					<p>Choose where {PRODUCT_NAME} saves user-visible notes, captures, and published task pages.</p>
				</div>
				<button class="settings-button settings-button-secondary" type="button" disabled={$notesSettingsStore.isLoading || $notesSettingsStore.isSaving} on:click={() => refreshNotesSettingsFromSettings(true)}>
					{$notesSettingsStore.isLoading ? 'Refreshing...' : 'Refresh notes'}
				</button>
			</div>

			<div class="settings-alert settings-alert-warning" role="note">
				Changing provider or location affects where new notes are saved. Existing notes are not deleted, but can appear lost until you switch back or migrate them.
			</div>

			<dl class="settings-meta-grid">
				<div>
					<dt>Settings file</dt>
					<dd>{$notesSettingsStore.envelope?.settings_path || 'not loaded'}</dd>
				</div>
				<div>
					<dt>Active provider</dt>
					<dd>{providerLabel(notesSettings.default_provider)}</dd>
				</div>
				<div>
					<dt>Local Markdown root</dt>
					<dd>{notesResolved?.local_markdown_root || 'default scope notes path'}</dd>
				</div>
				<div>
					<dt>Notes folder</dt>
					<dd>{notesResolved?.silverbullet_space_path || 'not configured'}</dd>
				</div>
			</dl>

			{#if notesStatus}
				<div class="settings-provider-list" aria-label="Notes provider status">
					{#each notesStatus.providers as provider (provider.id)}
						<div class="settings-provider-row">
							<div>
								<strong>{provider.label}</strong>
								<span>{provider.root || 'n/a'}</span>
							</div>
							<span class={`settings-badge badge-${statusTone(provider)}`}>
								{provider.writable ? 'ready' : provider.message || 'unavailable'}
							</span>
						</div>
					{/each}
				</div>
			{:else}
				<div class="settings-empty compact">
					<h3>Notes provider status unavailable</h3>
					<p>Refresh notes settings to inspect provider readiness.</p>
				</div>
			{/if}

			<form class="settings-form" on:submit|preventDefault={saveNotesSettingsFromCurrentForm}>
				<label class="settings-checkbox-row">
					<input type="checkbox" bind:checked={notesEnabled} disabled={$notesSettingsStore.isLoading || $notesSettingsStore.isSaving} on:change={markNotesFormDirty} />
					<span>Enable notes capture</span>
				</label>
				<label class="settings-field" for="notes-provider">
					<span>Provider</span>
					<select id="notes-provider" bind:value={notesDefaultProvider} disabled={$notesSettingsStore.isLoading || $notesSettingsStore.isSaving} on:change={markNotesFormDirty}>
						<option value="local_markdown">Local Markdown</option>
						<option value="silverbullet">Notes folder</option>
					</select>
				</label>
				<label class="settings-field" for="notes-local-root">
					<span>Local Markdown root</span>
					<input id="notes-local-root" bind:value={notesLocalRoot} disabled={$notesSettingsStore.isLoading || $notesSettingsStore.isSaving} placeholder="Blank uses the current workspace notes folder" on:input={markNotesFormDirty} />
				</label>
				<label class="settings-field" for="notes-silverbullet-path">
					<span>Notes folder path</span>
					<input id="notes-silverbullet-path" bind:value={notesSilverbulletSpacePath} disabled={$notesSettingsStore.isLoading || $notesSettingsStore.isSaving} placeholder="$MAGICIAN_ROOT_DIR/MagicanNotes/spaces/<principal>/<workspace>" on:input={markNotesFormDirty} />
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
					<button class="settings-button settings-button-primary" type="submit" disabled={$notesSettingsStore.isLoading || $notesSettingsStore.isSaving || !notesFormDirty}>
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
</div>

<style>
	.settings-page {
		box-sizing: border-box;
		color: var(--text-primary);
		display: flex;
		flex-direction: column;
		gap: 1.25rem;
		margin: 0 auto;
		max-width: var(--app-content-max, 1320px);
		padding: 1.35rem 1.45rem 5rem;
		width: 100%;
	}

	.settings-hero,
	.settings-card,
	.settings-empty {
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
	}

	.settings-hero,
	.settings-card {
		padding: 1rem;
	}

	.settings-hero {
		background:
			radial-gradient(circle at top right, color-mix(in srgb, var(--accent-primary) 13%, transparent), transparent 42%),
			linear-gradient(180deg, color-mix(in srgb, var(--bg-card) 94%, var(--bg-soft) 6%), var(--bg-card));
	}

	.settings-overline,
	.settings-hero h1,
	.settings-card h2,
	.settings-empty h3,
	.settings-hero p,
	.settings-card p,
	.settings-empty p {
		letter-spacing: 0;
		margin: 0;
	}

	.settings-hero h1 {
		font-family: var(--font-display, var(--font-primary));
		font-size: 2.35rem;
		font-weight: 700;
		line-height: 1.1;
		margin: 0.25rem 0 0.55rem;
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

	.settings-hero p,
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

	.settings-chip-row,
	.settings-actions,
	.settings-form-actions {
		align-items: center;
		display: flex;
		flex-wrap: wrap;
		gap: 0.65rem;
	}

	.settings-chip-row {
		margin-top: 0.9rem;
	}

	.settings-grid {
		display: grid;
		gap: 1.25rem;
		grid-template-columns: minmax(18rem, 0.85fr) minmax(0, 1.15fr);
	}

	.settings-card-wide {
		grid-column: 1 / -1;
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
		max-height: min(64vh, 52rem);
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

	.settings-provider-row + .settings-provider-row {
		border-top: 1px solid var(--border-soft);
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
		background: var(--accent-primary);
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
		.settings-grid,
		.settings-form,
		.settings-auth-row {
			grid-template-columns: minmax(0, 1fr);
		}

		.settings-section-header {
			flex-direction: column;
		}
	}

	@media (max-width: 640px) {
		.settings-page {
			padding: 0.85rem 0.75rem 4.5rem;
		}

		.settings-hero h1 {
			font-size: 1.9rem;
		}

		.settings-provider-row {
			flex-direction: column;
		}
	}
</style>
