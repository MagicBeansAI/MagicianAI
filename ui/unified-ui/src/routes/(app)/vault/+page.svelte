<script lang="ts">
	import { browser } from '$app/environment';
	import { onMount } from 'svelte';
	import { showError, showInfo, showSuccess } from '$lib/shared/stores/notifications';
	import { requestConfirmation } from '$lib/stores/confirmationStore';
	import {
		acknowledgeSetupToken,
		approvalDomainLabel,
		approveSecretChallenge,
		buildSecretRequestFromDraft,
		createEmptySecretDraft,
		createSecret,
		deleteSecret,
		draftFromSecret,
		getStoredSetupToken,
		loadSecretDetail,
		loadSecretVaultOverview,
		rotateSetupToken,
		storeSetupToken,
		type PendingSecretApproval,
		type SecretCookieRow,
		type SecretDetail,
		type SecretDraft,
		type SecretFieldRow,
		type SecretMappingRow,
		type SecretSummary,
		type SecretVaultApiError,
		type SetupTokenStatus,
		updateSecret
	} from '$lib/stores/secretVaultStore';

	type EditorMode = 'create' | 'view' | 'edit';

	type BadgeColor = 'default' | 'success' | 'warning' | 'error' | 'info';
	type DataListItem = { id: string; key: string; value: string };

	const injectionOptions = [
		{ value: 'header', label: 'HTTP header' },
		{ value: 'form_fields', label: 'JSON form fields' },
		{ value: 'cookies', label: 'Browser cookies' }
	];

	const sameSiteOptions = [
		{ value: '', label: 'Default SameSite' },
		{ value: 'lax', label: 'Lax' },
		{ value: 'strict', label: 'Strict' },
		{ value: 'none', label: 'None' }
	];

	let loading = true;
	let refreshing = false;
	let detailLoading = false;
	let saving = false;
	let deleting = false;
	let acknowledging = false;
	let rotating = false;
	let approvalBusy = new Set<string>();

	let routeError: string | null = null;
	let lastLoadedAt: number | null = null;

	let setup: SetupTokenStatus | null = null;
	let tokenInput = getStoredSetupToken();
	let secrets: SecretSummary[] = [];
	let approvals: PendingSecretApproval[] = [];

	let selectedId: string | null = null;
	let selectedDetail: SecretDetail | null = null;
	let editorMode: EditorMode = 'create';
	let draft: SecretDraft = createEmptySecretDraft();

	$: selectedSummary = selectedId
		? secrets.find((secret) => secret.id === selectedId) ?? null
		: null;
	$: vaultAvailable = setup?.available !== false;
	$: vaultUnavailableReason =
		!vaultAvailable
			? setup?.unavailable_reason || 'The OS keychain is unavailable, so provisioned vault storage is disabled on this machine.'
			: null;
	$: hasSessionToken = tokenInput.trim().length > 0;
	$: canEditSelected = editorMode === 'edit' && !!selectedDetail?.fields;
	$: selectedFieldCount = selectedDetail?.fields ? Object.keys(selectedDetail.fields).length : 0;
	$: pendingApprovalCount = approvals.length;
	$: tokenStatusLabel = buildTokenStatusLabel(setup);
	$: tokenStatusColor = buildTokenStatusColor(setup);
	$: summaryItems = [
		{ id: 'vault-summary-secret-count', key: 'Secrets', value: String(secrets.length) },
		{ id: 'vault-summary-approval-count', key: 'Pending approvals', value: String(approvals.length) },
		{ id: 'vault-summary-token-status', key: 'Setup token', value: tokenStatusLabel },
		{ id: 'vault-summary-selected', key: 'Selected', value: selectedSummary?.label || 'None' },
		{ id: 'vault-summary-fields', key: 'Loaded fields', value: String(selectedFieldCount) },
		{ id: 'vault-summary-refresh', key: 'Last refresh', value: formatRelativeTime(lastLoadedAt) }
	];
	$: setupDetailItems = [
		{ id: 'setup-created', key: 'Created', value: formatDateTime(setup?.created_at) },
		{ id: 'setup-acknowledged', key: 'Acknowledged', value: formatDateTime(setup?.acknowledged_at) },
		{ id: 'setup-header', key: 'Header', value: setup?.header_name || 'X-Magician-Setup-Token' },
		{ id: 'setup-surface', key: 'Mutation surface', value: 'Localhost API only' }
	];
	$: selectedInspectorItems = selectedSummary ? buildSelectedInspectorItems(selectedSummary) : [];
	$: selectedPolicyItems = selectedSummary ? buildSelectedPolicyItems(selectedSummary) : [];
	$: allowedToolsPlaceholder = buildAllowedToolsPlaceholder(setup, draft.injectionKind);

	$: if (browser) {
		storeSetupToken(tokenInput);
	}

	onMount(() => {
		void refresh(false);
	});

	function formatRelativeTime(timestamp: number | null | undefined): string {
		if (!timestamp) return 'n/a';
		const diffMs = timestamp - Date.now();
		const diffMinutes = Math.round(diffMs / 60000);
		if (Math.abs(diffMinutes) < 1) return 'just now';
		if (Math.abs(diffMinutes) < 60) return `${Math.abs(diffMinutes)}m ${diffMinutes < 0 ? 'ago' : 'from now'}`;
		const diffHours = Math.round(diffMinutes / 60);
		if (Math.abs(diffHours) < 48) return `${Math.abs(diffHours)}h ${diffHours < 0 ? 'ago' : 'from now'}`;
		const diffDays = Math.round(diffHours / 24);
		return `${Math.abs(diffDays)}d ${diffDays < 0 ? 'ago' : 'from now'}`;
	}

	function formatDateTime(timestamp: number | null | undefined): string {
		if (!timestamp) return 'n/a';
		return new Date(timestamp).toLocaleString();
	}

	function formatList(values: string[], emptyLabel: string): string {
		return values.length > 0 ? values.join(', ') : emptyLabel;
	}

	function buildAllowedToolsPlaceholder(
		status: SetupTokenStatus | null,
		injectionKind: SecretDraft['injectionKind']
	): string {
		const catalog = status?.supported_policy_routes;
		if (!catalog) {
			return 'Canonical runtime routes, one per line';
		}
		if (injectionKind === 'header' || injectionKind === 'form_fields') {
			return catalog.http.slice(0, 3).join('\n') || 'Canonical HTTP routes, one per line';
		}
		return catalog.browser.slice(0, 3).join('\n') || 'Canonical browser routes, one per line';
	}

	function buildTokenStatusLabel(status: SetupTokenStatus | null): string {
		if (!status) return 'Loading setup token state';
		if (!status.available) return 'Unavailable';
		if (status.pending_acknowledgement) return 'Pending acknowledgement';
		if (status.acknowledged_at) return 'Acknowledged';
		return 'Configured';
	}

	function buildTokenStatusColor(status: SetupTokenStatus | null): BadgeColor {
		if (!status) return 'default';
		if (!status.available) return 'error';
		if (status.pending_acknowledgement) return 'warning';
		if (status.acknowledged_at) return 'success';
		return 'info';
	}

	function toErrorMessage(error: unknown, fallback: string): string {
		if (error instanceof Error) return error.message;
		return fallback;
	}

	function buildSelectedInspectorItems(secret: SecretSummary): DataListItem[] {
		return [
			{ id: 'selected-id', key: 'Identifier', value: secret.id },
			{ id: 'selected-label', key: 'Label', value: secret.label },
			{ id: 'selected-created', key: 'Created', value: formatDateTime(secret.created_at) },
			{ id: 'selected-injection', key: 'Injection', value: injectionLabel(secret) },
			{
				id: 'selected-fields',
				key: 'Field names',
				value: secret.field_names.length > 0 ? secret.field_names.join(', ') : 'n/a'
			}
		];
	}

	function buildSelectedPolicyItems(secret: SecretSummary): DataListItem[] {
		return [
			{
				id: 'policy-tools',
				key: 'Allowed tools',
				value: formatList(secret.policy.allowed_tools, 'Any tool')
			},
			{
				id: 'policy-domains',
				key: 'Allowed domains',
				value: formatList(secret.policy.allowed_domains, 'Any domain')
			},
			{
				id: 'policy-max-uses',
				key: 'Max uses/day',
				value: secret.policy.max_uses_per_day ? String(secret.policy.max_uses_per_day) : 'Unbounded'
			},
			{
				id: 'policy-approval',
				key: 'Approval',
				value: secret.policy.requires_approval ? 'Required' : 'Not required'
			}
		];
	}

	function buildApprovalItems(approval: PendingSecretApproval): DataListItem[] {
		return [
			{ id: `${approval.challenge_id}-tool`, key: 'Tool', value: `${approval.tool}:${approval.action}` },
			{ id: `${approval.challenge_id}-id`, key: 'Secret id', value: approval.secret_id },
			{ id: `${approval.challenge_id}-domain`, key: approval.domains?.length ? 'Domains' : 'Domain', value: approvalDomainLabel(approval) },
			{
				id: `${approval.challenge_id}-expires`,
				key: 'Expires',
				value: `${formatRelativeTime(approval.expires_at)} (${formatDateTime(approval.expires_at)})`
			}
		];
	}

	function resetDraft(): void {
		if (editorMode === 'edit' && selectedDetail?.fields) {
			draft = draftFromSecret(selectedDetail);
			return;
		}
		draft = createEmptySecretDraft();
	}

	function startCreate(): void {
		selectedId = null;
		selectedDetail = null;
		editorMode = 'create';
		draft = createEmptySecretDraft();
		routeError = null;
	}

	function updateDraft<K extends keyof SecretDraft>(key: K, value: SecretDraft[K]): void {
		draft = {
			...draft,
			[key]: value
		};
	}

	function updateFieldRow(index: number, patch: Partial<SecretFieldRow>): void {
		draft = {
			...draft,
			fields: draft.fields.map((field, rowIndex) => (rowIndex === index ? { ...field, ...patch } : field))
		};
	}

	function updateMappingRow(index: number, patch: Partial<SecretMappingRow>): void {
		draft = {
			...draft,
			formMappings: draft.formMappings.map((mapping, rowIndex) =>
				rowIndex === index ? { ...mapping, ...patch } : mapping
			)
		};
	}

	function updateCookieRow(index: number, patch: Partial<SecretCookieRow>): void {
		draft = {
			...draft,
			cookies: draft.cookies.map((cookie, rowIndex) => (rowIndex === index ? { ...cookie, ...patch } : cookie))
		};
	}

	async function refresh(showToast: boolean): Promise<void> {
		const firstLoad = loading;
		if (!firstLoad) refreshing = true;
		routeError = null;
		try {
			const overview = await loadSecretVaultOverview(tokenInput);
			setup = overview.setup;
			secrets = overview.secrets;
			approvals = overview.approvals;
			lastLoadedAt = Date.now();

			if (!overview.setup.available) {
				selectedId = null;
				selectedDetail = null;
				editorMode = 'create';
				draft = createEmptySecretDraft();
			} else if (overview.setup.pending_token && tokenInput.trim().length === 0) {
				tokenInput = overview.setup.pending_token;
			}

			if (selectedId && overview.setup.available) {
				const stillExists = overview.secrets.some((secret) => secret.id === selectedId);
				if (stillExists) {
					await reloadSelectedSecret(false);
				} else {
					startCreate();
				}
			}

			if (showToast) {
				showSuccess('Vault refreshed');
			}
		} catch (error) {
			const message = toErrorMessage(error, 'Failed to load secret vault');
			routeError = message;
			showError(message);
		} finally {
			loading = false;
			refreshing = false;
		}
	}

	async function reloadSelectedSecret(showToastOnAuthFailure: boolean): Promise<void> {
		if (!selectedId) return;
		detailLoading = true;
		try {
			if (tokenInput.trim().length > 0) {
				selectedDetail = await loadSecretDetail(selectedId, tokenInput, true);
				editorMode = selectedDetail.fields ? 'edit' : 'view';
				if (selectedDetail.fields) {
					draft = draftFromSecret(selectedDetail);
				}
				return;
			}

			selectedDetail = await loadSecretDetail(selectedId, undefined, false);
			editorMode = 'view';
		} catch (error) {
			const apiError = error as SecretVaultApiError;
			if (apiError?.status === 401) {
				try {
					selectedDetail = await loadSecretDetail(selectedId, undefined, false);
					editorMode = 'view';
					if (showToastOnAuthFailure) {
						showInfo('Setup token required to load secret field values');
					}
					return;
				} catch (fallbackError) {
					const message = toErrorMessage(fallbackError, 'Failed to load secret');
					routeError = message;
					showError(message);
					return;
				}
			}

			const message = toErrorMessage(error, 'Failed to load secret');
			routeError = message;
			showError(message);
		} finally {
			detailLoading = false;
		}
	}

	async function selectSecret(secretId: string): Promise<void> {
		if (!vaultAvailable) return;
		selectedId = secretId;
		await reloadSelectedSecret(true);
	}

	async function unlockSelected(): Promise<void> {
		if (!vaultAvailable) {
			showInfo(vaultUnavailableReason || 'Vault is unavailable on this machine');
			return;
		}
		if (!selectedId) return;
		if (!hasSessionToken) {
			showInfo('Enter the setup token to unlock secret field values');
			return;
		}
		await reloadSelectedSecret(true);
	}

	async function saveSecret(): Promise<void> {
		if (!vaultAvailable) {
			showError(vaultUnavailableReason || 'Vault is unavailable on this machine');
			return;
		}
		if (!hasSessionToken) {
			showError('Setup token is required to save vault changes');
			return;
		}

		try {
			buildSecretRequestFromDraft(draft, editorMode === 'create' ? 'create' : 'update');
		} catch (error) {
			showError(toErrorMessage(error, 'Secret form is invalid'));
			return;
		}

		saving = true;
		routeError = null;
		try {
			if (editorMode === 'create') {
				const created = await createSecret(tokenInput, draft);
				showSuccess(`Created ${created.label}`);
				await refresh(false);
				await selectSecret(created.id);
			} else if (selectedId) {
				const updated = await updateSecret(tokenInput, selectedId, draft);
				showSuccess(`Updated ${updated.label}`);
				await refresh(false);
				await selectSecret(updated.id);
			}
		} catch (error) {
			const message = toErrorMessage(error, 'Failed to save secret');
			routeError = message;
			showError(message);
		} finally {
			saving = false;
		}
	}

	async function removeSelectedSecret(): Promise<void> {
		if (!vaultAvailable) {
			showError(vaultUnavailableReason || 'Vault is unavailable on this machine');
			return;
		}
		if (!selectedId || !selectedSummary) return;
		if (!hasSessionToken) {
			showError('Setup token is required to delete a secret');
			return;
		}
		if (browser) {
			const confirmed = await requestConfirmation({
				title: `Delete "${selectedSummary.label}"?`,
				message: 'This cannot be undone.',
				confirmLabel: 'Delete',
				destructive: true
			});
			if (!confirmed) return;
		}

		deleting = true;
		routeError = null;
		try {
			const deleted = await deleteSecret(tokenInput, selectedId);
			if (deleted) {
				showSuccess(`Deleted ${selectedSummary.label}`);
				startCreate();
				await refresh(false);
			} else {
				showInfo('Secret was already removed');
				startCreate();
				await refresh(false);
			}
		} catch (error) {
			const message = toErrorMessage(error, 'Failed to delete secret');
			routeError = message;
			showError(message);
		} finally {
			deleting = false;
		}
	}

	async function approvePendingChallenge(challengeId: string): Promise<void> {
		if (!vaultAvailable) {
			showError(vaultUnavailableReason || 'Vault is unavailable on this machine');
			return;
		}
		if (!hasSessionToken) {
			showError('Setup token is required to approve pending secret use');
			return;
		}

		approvalBusy = new Set([...approvalBusy, challengeId]);
		try {
			const approved = await approveSecretChallenge(tokenInput, challengeId);
			if (approved) {
				showSuccess('Secret use approved');
			} else {
				showInfo('Approval challenge was already missing or expired');
			}
			await refresh(false);
		} catch (error) {
			const message = toErrorMessage(error, 'Failed to approve secret use');
			routeError = message;
			showError(message);
		} finally {
			const next = new Set(approvalBusy);
			next.delete(challengeId);
			approvalBusy = next;
		}
	}

	async function acknowledgeVisibleToken(): Promise<void> {
		if (!vaultAvailable) {
			showError(vaultUnavailableReason || 'Vault is unavailable on this machine');
			return;
		}
		const token = tokenInput.trim() || setup?.pending_token || '';
		if (!token) {
			showError('Setup token is required to acknowledge the displayed token');
			return;
		}

		acknowledging = true;
		try {
			setup = await acknowledgeSetupToken(token);
			showSuccess('Setup token acknowledged');
			await refresh(false);
		} catch (error) {
			const message = toErrorMessage(error, 'Failed to acknowledge setup token');
			routeError = message;
			showError(message);
		} finally {
			acknowledging = false;
		}
	}

	async function rotateCurrentToken(): Promise<void> {
		if (!vaultAvailable) {
			showError(vaultUnavailableReason || 'Vault is unavailable on this machine');
			return;
		}
		if (!hasSessionToken) {
			showError('Current setup token is required before it can be rotated');
			return;
		}

		if (browser) {
			const confirmed = await requestConfirmation({
				title: 'Rotate the setup token now?',
				message: 'The current token will stop working immediately.',
				confirmLabel: 'Rotate',
				destructive: true
			});
			if (!confirmed) return;
		}

		rotating = true;
		try {
			const nextSetup = await rotateSetupToken(tokenInput);
			setup = nextSetup;
			if (nextSetup.pending_token) {
				tokenInput = nextSetup.pending_token;
			}
			showSuccess('Setup token rotated');
			await refresh(false);
		} catch (error) {
			const message = toErrorMessage(error, 'Failed to rotate setup token');
			routeError = message;
			showError(message);
		} finally {
			rotating = false;
		}
	}

	function addFieldRow(): void {
		draft = {
			...draft,
			fields: [...draft.fields, { key: '', value: '' }]
		};
	}

	function removeFieldRow(index: number): void {
		if (draft.fields.length === 1) return;
		draft = {
			...draft,
			fields: draft.fields.filter((_, rowIndex) => rowIndex !== index)
		};
	}

	function addMappingRow(): void {
		draft = {
			...draft,
			formMappings: [...draft.formMappings, { source: '', target: '' }]
		};
	}

	function removeMappingRow(index: number): void {
		if (draft.formMappings.length === 1) return;
		draft = {
			...draft,
			formMappings: draft.formMappings.filter((_, rowIndex) => rowIndex !== index)
		};
	}

	function addCookieRow(): void {
		draft = {
			...draft,
			cookies: [
				...draft.cookies,
				{ name: '', domain: '', path: '/', secure: true, http_only: true, same_site: '', expires: '' }
			]
		};
	}

	function removeCookieRow(index: number): void {
		if (draft.cookies.length === 1) return;
		draft = {
			...draft,
			cookies: draft.cookies.filter((_, rowIndex) => rowIndex !== index)
		};
	}

	function injectionLabel(secret: SecretSummary | SecretDetail | null): string {
		if (!secret) return 'n/a';
		if (secret.injection.kind === 'header') {
			return `Header · ${secret.injection.name}`;
		}
		if (secret.injection.kind === 'form_fields') {
			return `Form fields · ${Object.keys(secret.injection.mapping).length} mapping(s)`;
		}
		return `Cookies · ${secret.injection.cookies.length} cookie(s)`;
	}

	function badgeClass(color: BadgeColor): string {
		return `vault-badge vault-badge-${color}`;
	}

	function fieldValue(event: Event): string {
		return (event.currentTarget as HTMLInputElement | HTMLSelectElement | HTMLTextAreaElement).value;
	}

	function fieldChecked(event: Event): boolean {
		return (event.currentTarget as HTMLInputElement).checked;
	}
</script>

<svelte:head>
	<title>Vault | Magican</title>
</svelte:head>

<div class="vault-route">
	<div class="vault-shell">
		<div class="vault-stack">
			<section class="vault-card vault-hero-card">
				<div class="vault-hero">
					<div class="vault-hero-copy">
						<p class="vault-overline">Vault</p>
						<h1 class="vault-title">Provisioned secret management</h1>
						<p class="vault-text">
							Create and update provisioned credentials, review pending secret approvals, and manage the localhost setup token used for mutating vault operations.
						</p>
					</div>
					<div class="vault-hero-actions">
						<span class={badgeClass('info')}>Last refresh {formatRelativeTime(lastLoadedAt)}</span>
						<span class={badgeClass(tokenStatusColor)}>{tokenStatusLabel}</span>
						<button
							type="button"
							class="vault-button vault-button-primary"
							disabled={loading || refreshing || detailLoading || saving || deleting}
							on:click={() => refresh(true)}
						>
							{refreshing ? 'Refreshing...' : 'Refresh vault'}
						</button>
					</div>
				</div>
				{#if routeError}
					<div class="vault-alert-row">
						<div class="vault-alert vault-alert-error" role="alert">{routeError}</div>
					</div>
				{:else if !vaultAvailable && vaultUnavailableReason}
					<div class="vault-alert-row">
						<div class="vault-alert vault-alert-warning" role="status">{vaultUnavailableReason}</div>
					</div>
				{:else if loading}
					<div class="vault-alert-row">
						<div class="vault-alert vault-alert-info" role="status">Loading vault state from the localhost API.</div>
					</div>
				{/if}
			</section>

			<section class="vault-summary-strip" aria-labelledby="vault-summary-title">
				<div>
					<p class="vault-overline">Status</p>
					<h2 id="vault-summary-title" class="vault-section-title">Vault summary</h2>
					<p class="vault-text compact">Live counters for provisioned secrets, approvals, and setup-token state.</p>
				</div>
				<dl class="vault-metric-grid">
					{#each summaryItems as item}
						<div class="vault-metric">
							<dt>{item.key}</dt>
							<dd>{item.value}</dd>
						</div>
					{/each}
				</dl>
			</section>

			<div class="vault-grid vault-grid-two">
				<section class="vault-card vault-section-card">
					<div class="vault-stack-tight">
						<div class="vault-section-header">
							<div>
								<h2 class="vault-section-title">Setup token</h2>
								<p class="vault-text">Mutating vault operations require the one-time setup token over the localhost API.</p>
							</div>
							<div class="vault-button-row">
								<button
									type="button"
									class="vault-button vault-button-outline"
									disabled={!vaultAvailable || !setup?.pending_acknowledgement || acknowledging}
									on:click={acknowledgeVisibleToken}
								>
									{acknowledging ? 'Acknowledging...' : 'Acknowledge'}
								</button>
								<button
									type="button"
									class="vault-button vault-button-secondary"
									disabled={!vaultAvailable || !hasSessionToken || rotating}
									on:click={rotateCurrentToken}
								>
									{rotating ? 'Rotating...' : 'Rotate token'}
								</button>
							</div>
						</div>

						{#if !vaultAvailable}
							<div class="vault-empty">
								<h3>Vault storage unavailable</h3>
								<p>{vaultUnavailableReason || 'Provisioned vault storage is disabled because no durable OS keychain backend is available.'}</p>
							</div>
						{:else}
							<div class="vault-grid vault-grid-auto">
								<section class="vault-panel" aria-labelledby="vault-session-token-panel-title">
									<h3 id="vault-session-token-panel-title" class="vault-panel-title">Session token</h3>
									<div class="vault-panel-body">
										<label class="vault-field" for="vault-setup-token">
											<span>Session token</span>
											<input
												id="vault-setup-token"
												class="vault-input"
												value={tokenInput}
												placeholder="Paste the setup token for this browser session"
												on:input={(event) => (tokenInput = fieldValue(event))}
											/>
										</label>
										<div class="vault-badge-row">
											<span class={badgeClass(tokenStatusColor)}>Status: {tokenStatusLabel}</span>
											<span class={badgeClass('default')}>Header: {setup?.header_name || 'X-Magician-Setup-Token'}</span>
										</div>
										{#if setup?.pending_token}
											<div class="vault-alert vault-alert-warning" role="status">
												This token is shown until it is acknowledged. After acknowledgement it is not returned again.
											</div>
											<pre class="vault-code"><code>{setup.pending_token}</code></pre>
										{/if}
									</div>
								</section>

								<section class="vault-panel" aria-labelledby="vault-token-state-panel-title">
									<h3 id="vault-token-state-panel-title" class="vault-panel-title">Token state</h3>
									<div class="vault-panel-body">
										<dl class="vault-data-list">
											{#each setupDetailItems as item}
												<div class="vault-data-row">
													<dt>{item.key}</dt>
													<dd>{item.value}</dd>
												</div>
											{/each}
										</dl>
										<p class="vault-caption">
											Token-backed operations stay on the existing localhost API surface. No planner-visible tool path is involved here.
										</p>
									</div>
								</section>
							</div>
						{/if}
					</div>
				</section>

				<section class="vault-card vault-section-card">
					<div class="vault-stack-tight">
						<div class="vault-section-header">
							<div>
								<h2 class="vault-section-title">Pending approvals</h2>
								<p class="vault-text">Approve provisioned secret usage that paused for local confirmation.</p>
							</div>
							<span class={badgeClass(pendingApprovalCount > 0 ? 'warning' : 'default')}>{pendingApprovalCount} pending</span>
						</div>

						{#if !vaultAvailable}
							<div class="vault-empty">
								<h3>Approvals unavailable</h3>
								<p>Pending approval workflows are disabled while provisioned vault storage is unavailable.</p>
							</div>
						{:else if approvals.length === 0}
							<div class="vault-empty">
								<h3>No pending approvals</h3>
								<p>Secret-use challenges will appear here when execution pauses for local approval.</p>
							</div>
						{:else}
							<div class="vault-list">
								{#each approvals as approval}
									<article class="vault-subcard vault-approval-card">
										<div class="vault-inline-header">
											<div>
												<h3 class="vault-inline-title">{approval.secret_label}</h3>
												<p class="vault-caption">{approval.tool}:{approval.action}</p>
											</div>
											<span class={badgeClass('warning')}>Expires {formatRelativeTime(approval.expires_at)}</span>
										</div>
										<dl class="vault-data-list">
											{#each buildApprovalItems(approval) as item}
												<div class="vault-data-row">
													<dt>{item.key}</dt>
													<dd>{item.value}</dd>
												</div>
											{/each}
										</dl>
										<button
											type="button"
											class="vault-button vault-button-primary"
											disabled={!vaultAvailable || approvalBusy.has(approval.challenge_id)}
											on:click={() => approvePendingChallenge(approval.challenge_id)}
										>
											{approvalBusy.has(approval.challenge_id) ? 'Approving...' : 'Approve use'}
										</button>
									</article>
								{/each}
							</div>
						{/if}
					</div>
				</section>
			</div>

			<div class="vault-workbench">
				<section class="vault-card vault-section-card">
					<div class="vault-stack-tight">
						<div class="vault-section-header">
							<div>
								<h2 class="vault-section-title">Provisioned secrets</h2>
								<p class="vault-text">Browse and inspect stored vault entries.</p>
							</div>
							<button type="button" class="vault-button vault-button-outline" disabled={!vaultAvailable} on:click={startCreate}>
								New secret
							</button>
						</div>

						{#if !vaultAvailable}
							<div class="vault-empty">
								<h3>Vault disabled</h3>
								<p>{vaultUnavailableReason || 'Provisioned secrets are unavailable on this machine because durable keychain storage is not available.'}</p>
							</div>
						{:else if secrets.length === 0}
							<div class="vault-empty">
								<h3>No provisioned secrets</h3>
								<p>Create the first vault entry to attach real credentials to executor-side secret injection.</p>
								<button type="button" class="vault-button vault-button-outline" on:click={startCreate}>Create secret</button>
							</div>
						{:else}
							<div class="vault-list">
								{#each secrets as secret}
									<button
										type="button"
										class={`vault-secret-item ${selectedId === secret.id ? 'is-selected' : ''}`}
										title={secret.id}
										on:click={() => selectSecret(secret.id)}
									>
										<span class="vault-inline-header">
											<span>
												<span class="vault-inline-title">{secret.label}</span>
												<span class="vault-caption vault-mono-caption">{secret.id}</span>
											</span>
											<span class={badgeClass(selectedId === secret.id ? 'info' : 'default')}>
												{secret.field_names.length} field(s)
											</span>
										</span>
										<span class="vault-text">{injectionLabel(secret)}</span>
									</button>
								{/each}
							</div>
						{/if}
					</div>
				</section>

				<section class="vault-card vault-section-card">
					<div class="vault-stack-tight">
						<div class="vault-section-header">
							<div>
								<p class="vault-overline">
									{editorMode === 'create' ? 'Create' : editorMode === 'edit' ? 'Edit' : 'Inspect'}
								</p>
								<h2 class="vault-section-title">
									{editorMode === 'create'
										? 'New provisioned secret'
										: selectedSummary?.label || 'Provisioned secret'}
								</h2>
								<p class="vault-text">
									{editorMode === 'create'
										? 'Define the secret fields, placement target, and policy guardrails.'
										: 'Inspect metadata or unlock field values with the setup token before editing.'}
								</p>
							</div>
							<div class="vault-button-row">
								{#if selectedId}
									<button
										type="button"
										class="vault-button vault-button-outline"
										disabled={!vaultAvailable || detailLoading || !selectedId}
										on:click={unlockSelected}
									>
										{detailLoading ? 'Loading...' : canEditSelected ? 'Reload fields' : 'Unlock fields'}
									</button>
								{/if}
								{#if editorMode === 'edit'}
									<button
										type="button"
										class="vault-button vault-button-outline"
										disabled={!vaultAvailable || deleting}
										on:click={removeSelectedSecret}
									>
										{deleting ? 'Deleting...' : 'Delete secret'}
									</button>
								{/if}
							</div>
						</div>

						{#if !vaultAvailable}
							<div class="vault-empty">
								<h3>Editing disabled</h3>
								<p>This page stays available for status and diagnostics, but setup-token and provisioned-secret operations remain disabled until the OS keychain backend is available.</p>
							</div>
						{:else if editorMode === 'view'}
							<div class="vault-stack-tight">
								<div class={`vault-alert ${hasSessionToken ? 'vault-alert-info' : 'vault-alert-warning'}`} role="status">
									{hasSessionToken
										? 'The selected secret is loaded in metadata-only mode. Unlock fields to fetch field values for editing.'
										: 'Enter the setup token above to unlock secret field values before editing this entry.'}
								</div>
								{#if selectedSummary}
									<dl class="vault-data-list">
										{#each selectedInspectorItems as item}
											<div class="vault-data-row">
												<dt>{item.key}</dt>
												<dd>{item.value}</dd>
											</div>
										{/each}
									</dl>
								{/if}
							</div>
						{:else}
							<form class="vault-form" on:submit|preventDefault={saveSecret}>
								<div class="vault-stack-tight">
									<section class="vault-panel" aria-labelledby="vault-identity-panel-title">
										<h3 id="vault-identity-panel-title" class="vault-panel-title">Identity</h3>
										<div class="vault-panel-body">
											<div class="vault-grid vault-grid-auto">
												<label class="vault-field" for="vault-secret-id">
													<span>Secret id</span>
													<input
														id="vault-secret-id"
														class="vault-input"
														value={draft.id}
														placeholder="stripe.checkout.primary"
														disabled={editorMode === 'edit'}
														on:input={(event) => updateDraft('id', fieldValue(event))}
													/>
												</label>
												<label class="vault-field" for="vault-secret-label">
													<span>Label</span>
													<input
														id="vault-secret-label"
														class="vault-input"
														value={draft.label}
														placeholder="Primary checkout token"
														on:input={(event) => updateDraft('label', fieldValue(event))}
													/>
												</label>
											</div>
										</div>
									</section>

									<section class="vault-panel" aria-labelledby="vault-fields-panel-title">
										<h3 id="vault-fields-panel-title" class="vault-panel-title">Secret fields</h3>
										<div class="vault-panel-body">
											<div class="vault-section-header compact">
												<p class="vault-caption">Store the actual secret values here. Header injection uses a single field or a field named value.</p>
												<button type="button" class="vault-button vault-button-sm vault-button-outline" on:click={addFieldRow}>
													Add field
												</button>
											</div>
											<div class="vault-list">
												{#each draft.fields as field, index}
													<div class="vault-row vault-row-field">
														<label class="vault-field" for={`vault-field-key-${index}`}>
															<span>Field {index + 1} key</span>
															<input
																id={`vault-field-key-${index}`}
																class="vault-input"
																value={field.key}
																placeholder={draft.injectionKind === 'cookies' ? 'cookie:sessionid' : 'value'}
																on:input={(event) => updateFieldRow(index, { key: fieldValue(event) })}
															/>
														</label>
														<label class="vault-field" for={`vault-field-value-${index}`}>
															<span>Field {index + 1} value</span>
															<input
																id={`vault-field-value-${index}`}
																class="vault-input"
																value={field.value}
																placeholder="Secret value"
																on:input={(event) => updateFieldRow(index, { value: fieldValue(event) })}
															/>
														</label>
														<div class="vault-row-action">
															<button
																type="button"
																class="vault-button vault-button-sm vault-button-outline"
																disabled={draft.fields.length === 1}
																on:click={() => removeFieldRow(index)}
															>
																Remove
															</button>
														</div>
													</div>
												{/each}
											</div>
										</div>
									</section>

									<section class="vault-panel" aria-labelledby="vault-injection-panel-title">
										<h3 id="vault-injection-panel-title" class="vault-panel-title">Injection target</h3>
										<div class="vault-panel-body">
											<label class="vault-field" for="vault-injection-kind">
												<span>Injection target</span>
												<select
													id="vault-injection-kind"
													class="vault-select"
													value={draft.injectionKind}
													on:change={(event) => updateDraft('injectionKind', fieldValue(event) as SecretDraft['injectionKind'])}
												>
													{#each injectionOptions as option}
														<option value={option.value}>{option.label}</option>
													{/each}
												</select>
											</label>

											{#if draft.injectionKind === 'header'}
												<div class="vault-grid vault-grid-auto">
													<label class="vault-field" for="vault-header-name">
														<span>Header name</span>
														<input
															id="vault-header-name"
															class="vault-input"
															value={draft.headerName}
															placeholder="Authorization"
															on:input={(event) => updateDraft('headerName', fieldValue(event))}
														/>
													</label>
													<label class="vault-field" for="vault-header-prefix">
														<span>Header prefix</span>
														<input
															id="vault-header-prefix"
															class="vault-input"
															value={draft.headerPrefix}
															placeholder="Bearer "
															on:input={(event) => updateDraft('headerPrefix', fieldValue(event))}
														/>
													</label>
												</div>
											{:else if draft.injectionKind === 'form_fields'}
												<div class="vault-stack-tight">
													<div class="vault-section-header compact">
														<p class="vault-caption">Map stored field names to outbound JSON field names.</p>
														<button type="button" class="vault-button vault-button-sm vault-button-outline" on:click={addMappingRow}>
															Add mapping
														</button>
													</div>
													<div class="vault-list">
														{#each draft.formMappings as mapping, index}
															<div class="vault-row vault-row-field">
																<label class="vault-field" for={`vault-mapping-source-${index}`}>
																	<span>Mapping {index + 1} source</span>
																	<input
																		id={`vault-mapping-source-${index}`}
																		class="vault-input"
																		value={mapping.source}
																		placeholder="api_key"
																		on:input={(event) => updateMappingRow(index, { source: fieldValue(event) })}
																	/>
																</label>
																<label class="vault-field" for={`vault-mapping-target-${index}`}>
																	<span>Mapping {index + 1} target</span>
																	<input
																		id={`vault-mapping-target-${index}`}
																		class="vault-input"
																		value={mapping.target}
																		placeholder="payment.apiKey"
																		on:input={(event) => updateMappingRow(index, { target: fieldValue(event) })}
																	/>
																</label>
																<div class="vault-row-action">
																	<button
																		type="button"
																		class="vault-button vault-button-sm vault-button-outline"
																		disabled={draft.formMappings.length === 1}
																		on:click={() => removeMappingRow(index)}
																	>
																		Remove
																	</button>
																</div>
															</div>
														{/each}
													</div>
												</div>
											{:else}
												<div class="vault-stack-tight">
													<div class="vault-section-header compact">
														<p class="vault-caption">Cookie rows reference values from the fields table by cookie name or cookie:name.</p>
														<button type="button" class="vault-button vault-button-sm vault-button-outline" on:click={addCookieRow}>
															Add cookie
														</button>
													</div>
													<div class="vault-list">
														{#each draft.cookies as cookie, index}
															<section class="vault-panel vault-nested-panel" aria-labelledby={`vault-cookie-panel-${index}-title`}>
																<h4 id={`vault-cookie-panel-${index}-title`} class="vault-panel-title">{cookie.name || `Cookie ${index + 1}`}</h4>
																<div class="vault-panel-body">
																	<div class="vault-grid vault-grid-compact">
																		<label class="vault-field" for={`vault-cookie-name-${index}`}>
																			<span>Cookie name</span>
																			<input
																				id={`vault-cookie-name-${index}`}
																				class="vault-input"
																				value={cookie.name}
																				placeholder="sessionid"
																				on:input={(event) => updateCookieRow(index, { name: fieldValue(event) })}
																			/>
																		</label>
																		<label class="vault-field" for={`vault-cookie-domain-${index}`}>
																			<span>Cookie domain</span>
																			<input
																				id={`vault-cookie-domain-${index}`}
																				class="vault-input"
																				value={cookie.domain}
																				placeholder=".example.com"
																				on:input={(event) => updateCookieRow(index, { domain: fieldValue(event) })}
																			/>
																		</label>
																		<label class="vault-field" for={`vault-cookie-path-${index}`}>
																			<span>Cookie path</span>
																			<input
																				id={`vault-cookie-path-${index}`}
																				class="vault-input"
																				value={cookie.path}
																				placeholder="/"
																				on:input={(event) => updateCookieRow(index, { path: fieldValue(event) })}
																			/>
																		</label>
																	</div>
																	<div class="vault-grid vault-grid-compact">
																		<label class="vault-field" for={`vault-cookie-same-site-${index}`}>
																			<span>SameSite</span>
																			<select
																				id={`vault-cookie-same-site-${index}`}
																				class="vault-select"
																				value={cookie.same_site}
																				on:change={(event) =>
																					updateCookieRow(index, {
																						same_site: fieldValue(event) as SecretCookieRow['same_site']
																					})}
																			>
																				{#each sameSiteOptions as option}
																					<option value={option.value}>{option.label}</option>
																				{/each}
																			</select>
																		</label>
																		<label class="vault-field" for={`vault-cookie-expires-${index}`}>
																			<span>Expires (unix seconds)</span>
																			<input
																				id={`vault-cookie-expires-${index}`}
																				class="vault-input"
																				value={cookie.expires}
																				placeholder="1718100000"
																				on:input={(event) => updateCookieRow(index, { expires: fieldValue(event) })}
																			/>
																		</label>
																		<div class="vault-row-action vault-row-action-inline">
																			<button
																				type="button"
																				class="vault-button vault-button-sm vault-button-outline"
																				disabled={draft.cookies.length === 1}
																				on:click={() => removeCookieRow(index)}
																			>
																				Remove cookie
																			</button>
																		</div>
																	</div>
																	<div class="vault-checkbox-row">
																		<label class="vault-checkbox" for={`vault-cookie-secure-${index}`}>
																			<input
																				id={`vault-cookie-secure-${index}`}
																				type="checkbox"
																				checked={cookie.secure}
																				on:change={(event) => updateCookieRow(index, { secure: fieldChecked(event) })}
																			/>
																			<span>Secure</span>
																		</label>
																		<label class="vault-checkbox" for={`vault-cookie-http-only-${index}`}>
																			<input
																				id={`vault-cookie-http-only-${index}`}
																				type="checkbox"
																				checked={cookie.http_only}
																				on:change={(event) => updateCookieRow(index, { http_only: fieldChecked(event) })}
																			/>
																			<span>HttpOnly</span>
																		</label>
																	</div>
																</div>
															</section>
														{/each}
													</div>
												</div>
											{/if}
										</div>
									</section>

									<section class="vault-panel" aria-labelledby="vault-policy-panel-title">
										<h3 id="vault-policy-panel-title" class="vault-panel-title">Policy guardrails</h3>
										<div class="vault-panel-body">
											<div class="vault-grid vault-grid-auto">
												<label class="vault-field" for="vault-allowed-tools">
													<span>Allowed tools</span>
													<textarea
														id="vault-allowed-tools"
														class="vault-textarea"
														rows="4"
														value={draft.allowedToolsText}
														placeholder={allowedToolsPlaceholder}
														on:input={(event) => updateDraft('allowedToolsText', fieldValue(event))}
													></textarea>
												</label>
												<label class="vault-field" for="vault-allowed-domains">
													<span>Allowed domains</span>
													<textarea
														id="vault-allowed-domains"
														class="vault-textarea"
														rows="4"
														value={draft.allowedDomainsText}
														placeholder={'api.example.com\ncheckout.example.com'}
														on:input={(event) => updateDraft('allowedDomainsText', fieldValue(event))}
													></textarea>
												</label>
											</div>
											<div class="vault-grid vault-grid-auto">
												<label class="vault-field" for="vault-max-uses">
													<span>Max uses per day</span>
													<input
														id="vault-max-uses"
														class="vault-input"
														value={draft.maxUsesPerDay}
														placeholder="25"
														on:input={(event) => updateDraft('maxUsesPerDay', fieldValue(event))}
													/>
												</label>
												<div class="vault-checkbox-wrap">
													<label class="vault-checkbox" for="vault-requires-approval">
														<input
															id="vault-requires-approval"
															type="checkbox"
															checked={draft.requiresApproval}
															on:change={(event) => updateDraft('requiresApproval', fieldChecked(event))}
														/>
														<span>Require approval before use</span>
													</label>
												</div>
											</div>
										</div>
									</section>

									<div class="vault-form-actions">
										<button type="button" class="vault-button vault-button-outline" on:click={resetDraft}>Reset form</button>
										<button
											type="submit"
											class="vault-button vault-button-primary"
											disabled={!vaultAvailable || !hasSessionToken || saving || deleting}
										>
											{saving
												? editorMode === 'create'
													? 'Creating...'
													: 'Saving...'
												: editorMode === 'create'
													? 'Create secret'
													: 'Save changes'}
										</button>
									</div>
								</div>
							</form>
						{/if}
					</div>
				</section>

				<section class="vault-card vault-section-card">
					<div class="vault-stack-tight">
						<div>
							<h2 class="vault-section-title">Selected secret</h2>
							<p class="vault-text">Runtime-safe metadata and policy state for the current selection.</p>
						</div>

						{#if !vaultAvailable}
							<div class="vault-empty">
								<h3>No active selection</h3>
								<p>Provisioned secret inspection is disabled while vault storage is unavailable.</p>
							</div>
						{:else if !selectedSummary}
							<div class="vault-empty">
								<h3>No secret selected</h3>
								<p>Select a secret to inspect its metadata and policy configuration.</p>
							</div>
						{:else}
							<div class="vault-stack-tight">
								<dl class="vault-data-list">
									{#each selectedInspectorItems as item}
										<div class="vault-data-row">
											<dt>{item.key}</dt>
											<dd>{item.value}</dd>
										</div>
									{/each}
								</dl>
								<section class="vault-panel" aria-labelledby="vault-selected-policy-panel-title">
									<h3 id="vault-selected-policy-panel-title" class="vault-panel-title">Policy</h3>
									<div class="vault-panel-body">
										<dl class="vault-data-list">
											{#each selectedPolicyItems as item}
												<div class="vault-data-row">
													<dt>{item.key}</dt>
													<dd>{item.value}</dd>
												</div>
											{/each}
										</dl>
									</div>
								</section>
								<section class="vault-panel" aria-labelledby="vault-selected-fields-panel-title">
									<h3 id="vault-selected-fields-panel-title" class="vault-panel-title">Field access</h3>
									<div class="vault-panel-body">
										<div class="vault-stack-compact">
											<span class={badgeClass(selectedFieldCount > 0 ? 'success' : 'default')}>
												{selectedFieldCount > 0 ? `${selectedFieldCount} field value(s) loaded` : 'Field values not loaded'}
											</span>
											<p class="vault-caption">
												{selectedFieldCount > 0
													? 'Field values are loaded in this browser session and can be edited in the form.'
													: 'Field values are hidden until the setup token is supplied and the secret is unlocked.'}
											</p>
										</div>
									</div>
								</section>
							</div>
						{/if}
					</div>
				</section>
			</div>
		</div>
	</div>
</div>

<style>
	.vault-route {
		box-sizing: border-box;
		color: var(--text-primary);
		display: flex;
		flex-direction: column;
		margin: 0 auto;
		max-width: var(--app-content-max, 1320px);
		padding: 1.35rem 1.45rem 5rem;
		width: 100%;
	}

	.vault-shell {
		min-width: 0;
		width: 100%;
	}

	.vault-stack,
	.vault-stack-tight,
	.vault-stack-compact,
	.vault-list,
	.vault-panel-body {
		display: flex;
		flex-direction: column;
	}

	.vault-stack {
		gap: 1.25rem;
	}

	.vault-stack-tight,
	.vault-panel-body {
		gap: 1rem;
	}

	.vault-stack-compact {
		gap: 0.5rem;
		align-items: flex-start;
	}

	.vault-list {
		gap: 0.75rem;
	}

	.vault-card,
	.vault-summary-strip,
	.vault-panel,
	.vault-subcard,
	.vault-empty,
	.vault-secret-item {
		border: 1px solid var(--border-soft);
		background: var(--bg-card);
		border-radius: 8px;
	}

	.vault-card,
	.vault-summary-strip {
		padding: 1rem;
	}

	.vault-hero-card {
		background:
			radial-gradient(circle at top right, color-mix(in srgb, var(--accent-primary) 14%, transparent), transparent 42%),
			linear-gradient(180deg, color-mix(in srgb, var(--bg-card) 94%, var(--bg-soft) 6%), var(--bg-card));
	}

	.vault-hero,
	.vault-section-header,
	.vault-inline-header,
	.vault-hero-actions,
	.vault-button-row,
	.vault-badge-row,
	.vault-checkbox-row {
		display: flex;
		gap: 0.75rem;
	}

	.vault-hero,
	.vault-section-header,
	.vault-inline-header {
		align-items: flex-start;
		justify-content: space-between;
	}

	.vault-hero-actions,
	.vault-button-row,
	.vault-badge-row,
	.vault-checkbox-row {
		align-items: center;
		flex-wrap: wrap;
	}

	.vault-hero-copy {
		max-width: 64rem;
	}

	.vault-title,
	.vault-section-title,
	.vault-panel-title,
	.vault-inline-title,
	.vault-empty h3 {
		margin: 0;
		color: var(--text-primary);
		letter-spacing: 0;
	}

	.vault-title {
		font-family: var(--font-display, var(--font-primary));
		font-size: 2.35rem;
		font-weight: 700;
		line-height: 1.1;
		margin-top: 0.25rem;
		margin-bottom: 0.55rem;
	}

	.vault-section-title {
		font-size: 1.05rem;
		font-weight: 600;
		line-height: 1.25;
	}

	.vault-panel-title {
		border-bottom: 1px solid var(--border-soft);
		background: color-mix(in srgb, var(--bg-card) 86%, var(--bg-soft) 14%);
		border-radius: 8px 8px 0 0;
		font-size: 0.92rem;
		font-weight: 600;
		line-height: 1.2;
		padding: 0.75rem 0.9rem;
	}

	.vault-inline-title {
		display: block;
		font-size: 0.92rem;
		font-weight: 600;
		line-height: 1.25;
		overflow-wrap: anywhere;
	}

	.vault-overline,
	.vault-text,
	.vault-caption,
	.vault-empty p {
		margin: 0;
		color: var(--text-secondary);
		letter-spacing: 0;
	}

	.vault-overline {
		font-size: 0.75rem;
		font-weight: 700;
		text-transform: uppercase;
	}

	.vault-text {
		font-size: 0.92rem;
		line-height: 1.5;
	}

	.vault-text.compact {
		margin-top: 0.25rem;
	}

	.vault-caption {
		display: block;
		font-size: 0.8rem;
		line-height: 1.45;
	}

	.vault-mono-caption,
	.vault-code {
		font-family: var(--font-mono);
	}

	.vault-alert-row {
		margin-top: 1rem;
	}

	.vault-workbench {
		display: grid;
		grid-template-columns: minmax(0, 0.9fr) minmax(0, 1.4fr) minmax(0, 1fr);
		gap: 1rem;
		align-items: start;
	}

	.vault-section-card {
		height: 100%;
	}

	.vault-grid {
		display: grid;
		gap: 1rem;
	}

	.vault-grid-two {
		grid-template-columns: repeat(2, minmax(0, 1fr));
	}

	.vault-grid-auto {
		grid-template-columns: repeat(auto-fit, minmax(260px, 1fr));
	}

	.vault-grid-compact {
		grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
	}

	.vault-summary-strip {
		display: grid;
		grid-template-columns: minmax(12rem, 0.75fr) minmax(0, 2.25fr);
		gap: 1rem;
		align-items: start;
		background: color-mix(in srgb, var(--bg-card) 92%, var(--bg-soft) 8%);
	}

	.vault-metric-grid {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(9rem, 1fr));
		gap: 0.75rem;
		margin: 0;
	}

	.vault-metric {
		border-left: 2px solid color-mix(in srgb, var(--accent-primary) 42%, var(--border-soft));
		padding-left: 0.75rem;
		min-width: 0;
	}

	.vault-metric dt,
	.vault-data-list dt {
		color: var(--text-secondary);
		font-size: 0.74rem;
		font-weight: 600;
		line-height: 1.3;
		margin: 0;
	}

	.vault-metric dd,
	.vault-data-list dd {
		color: var(--text-primary);
		font-size: 0.9rem;
		font-weight: 600;
		line-height: 1.35;
		margin: 0.15rem 0 0;
		overflow-wrap: anywhere;
	}

	.vault-panel {
		overflow: hidden;
		background: color-mix(in srgb, var(--bg-card) 92%, var(--bg-soft) 8%);
	}

	.vault-nested-panel {
		background: color-mix(in srgb, var(--bg-card) 96%, var(--bg-soft) 4%);
	}

	.vault-panel-body {
		padding: 0.9rem;
	}

	.vault-subcard,
	.vault-empty,
	.vault-secret-item {
		padding: 0.85rem;
	}

	.vault-empty {
		align-items: flex-start;
		display: flex;
		flex-direction: column;
		gap: 0.55rem;
		background: color-mix(in srgb, var(--bg-card) 90%, var(--bg-soft) 10%);
	}

	.vault-secret-item {
		appearance: none;
		color: inherit;
		cursor: pointer;
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
		text-align: left;
		width: 100%;
		border-color: var(--border-soft);
		background: linear-gradient(
			180deg,
			color-mix(in srgb, var(--bg-card) 88%, var(--bg-soft) 12%),
			var(--bg-card)
		);
	}

	.vault-secret-item.is-selected {
		border-color: color-mix(in srgb, var(--accent-primary) 48%, var(--border-soft));
		background:
			linear-gradient(
				180deg,
				color-mix(in srgb, var(--accent-primary) 12%, var(--bg-card)),
				color-mix(in srgb, var(--accent-primary) 5%, var(--bg-card))
			);
		box-shadow: 0 0 0 1px color-mix(in srgb, var(--accent-primary) 28%, transparent);
	}

	.vault-secret-item:hover:not(:disabled),
	.vault-secret-item:focus-visible {
		border-color: color-mix(in srgb, var(--accent-primary) 42%, var(--border-soft));
		outline: none;
	}

	.vault-button {
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
		text-align: center;
		transition:
			background 120ms ease,
			border-color 120ms ease,
			color 120ms ease;
		white-space: nowrap;
	}

	.vault-button-primary {
		background: var(--accent-primary);
		border-color: var(--accent-primary);
		color: var(--accent-on-primary, #fff);
	}

	.vault-button-secondary {
		background: color-mix(in srgb, var(--accent-primary) 14%, var(--bg-soft));
		border-color: color-mix(in srgb, var(--accent-primary) 30%, var(--border-soft));
		color: var(--text-primary);
	}

	.vault-button-outline {
		background: transparent;
		border-color: var(--border-soft);
		color: var(--text-primary);
	}

	.vault-button-sm {
		font-size: 0.78rem;
		min-height: 2rem;
		padding: 0.42rem 0.65rem;
	}

	.vault-button:hover:not(:disabled) {
		filter: brightness(1.04);
	}

	.vault-button:focus-visible {
		outline: 2px solid color-mix(in srgb, var(--accent-primary) 54%, transparent);
		outline-offset: 2px;
	}

	.vault-button:disabled,
	.vault-input:disabled,
	.vault-select:disabled,
	.vault-textarea:disabled {
		cursor: not-allowed;
		opacity: 0.58;
	}

	.vault-badge {
		align-items: center;
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
		word-break: normal;
		overflow-wrap: anywhere;
	}

	.vault-badge-info {
		background: color-mix(in srgb, var(--accent-primary) 12%, transparent);
		border-color: color-mix(in srgb, var(--accent-primary) 28%, var(--border-soft));
		color: color-mix(in srgb, var(--accent-primary) 75%, var(--text-primary));
	}

	.vault-badge-success {
		background: color-mix(in srgb, var(--success, #12805c) 14%, transparent);
		border-color: color-mix(in srgb, var(--success, #12805c) 34%, var(--border-soft));
		color: color-mix(in srgb, var(--success, #12805c) 75%, var(--text-primary));
	}

	.vault-badge-warning {
		background: color-mix(in srgb, var(--warning, #b7791f) 14%, transparent);
		border-color: color-mix(in srgb, var(--warning, #b7791f) 34%, var(--border-soft));
		color: color-mix(in srgb, var(--warning, #b7791f) 82%, var(--text-primary));
	}

	.vault-badge-error {
		background: color-mix(in srgb, var(--danger, #c2410c) 14%, transparent);
		border-color: color-mix(in srgb, var(--danger, #c2410c) 34%, var(--border-soft));
		color: color-mix(in srgb, var(--danger, #c2410c) 82%, var(--text-primary));
	}

	.vault-alert {
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		color: var(--text-primary);
		font-size: 0.88rem;
		line-height: 1.45;
		padding: 0.75rem 0.85rem;
	}

	.vault-alert-info {
		background: color-mix(in srgb, var(--accent-primary) 10%, var(--bg-card));
		border-color: color-mix(in srgb, var(--accent-primary) 26%, var(--border-soft));
	}

	.vault-alert-warning {
		background: color-mix(in srgb, var(--warning, #b7791f) 10%, var(--bg-card));
		border-color: color-mix(in srgb, var(--warning, #b7791f) 30%, var(--border-soft));
	}

	.vault-alert-error {
		background: color-mix(in srgb, var(--danger, #c2410c) 10%, var(--bg-card));
		border-color: color-mix(in srgb, var(--danger, #c2410c) 30%, var(--border-soft));
	}

	.vault-code {
		background: color-mix(in srgb, var(--bg-soft) 72%, var(--bg-card));
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		color: var(--text-primary);
		font-size: 0.82rem;
		line-height: 1.45;
		margin: 0;
		max-height: 140px;
		overflow: auto;
		padding: 0.75rem;
		white-space: pre-wrap;
		word-break: break-word;
	}

	.vault-data-list {
		display: grid;
		gap: 0.55rem;
		margin: 0;
	}

	.vault-data-row {
		display: grid;
		gap: 0.25rem;
		grid-template-columns: minmax(8rem, 0.55fr) minmax(0, 1fr);
	}

	.vault-field {
		color: var(--text-primary);
		display: flex;
		flex-direction: column;
		font-size: 0.82rem;
		font-weight: 600;
		gap: 0.4rem;
		min-width: 0;
	}

	.vault-input,
	.vault-select,
	.vault-textarea {
		background: color-mix(in srgb, var(--bg-card) 92%, var(--bg-soft) 8%);
		border: 1px solid var(--border-soft);
		border-radius: 6px;
		color: var(--text-primary);
		font: inherit;
		font-size: 0.9rem;
		line-height: 1.3;
		min-height: 2.4rem;
		padding: 0.55rem 0.65rem;
		width: 100%;
	}

	.vault-textarea {
		min-height: 6.8rem;
		resize: vertical;
	}

	.vault-input:focus,
	.vault-select:focus,
	.vault-textarea:focus {
		border-color: color-mix(in srgb, var(--accent-primary) 50%, var(--border-soft));
		outline: none;
		box-shadow: 0 0 0 2px color-mix(in srgb, var(--accent-primary) 18%, transparent);
	}

	.vault-checkbox-wrap {
		display: flex;
		align-items: center;
		padding: 0.35rem 0;
	}

	.vault-checkbox {
		align-items: center;
		color: var(--text-primary);
		display: inline-flex;
		font-size: 0.86rem;
		font-weight: 600;
		gap: 0.45rem;
		line-height: 1.3;
	}

	.vault-checkbox input {
		accent-color: var(--accent-primary);
		height: 1rem;
		width: 1rem;
	}

	.vault-form {
		display: block;
	}

	.vault-row {
		display: grid;
		gap: 0.75rem;
	}

	.vault-row-field {
		grid-template-columns: minmax(0, 1fr) minmax(0, 1.2fr) auto;
		align-items: end;
	}

	.vault-row-action {
		display: flex;
		align-items: end;
		justify-content: flex-end;
	}

	.vault-row-action-inline {
		min-height: 100%;
	}

	.vault-form-actions {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
		flex-wrap: wrap;
	}

	@media (max-width: 1180px) {
		.vault-grid-two,
		.vault-workbench {
			grid-template-columns: minmax(0, 1fr) minmax(0, 1.35fr);
		}

		.vault-workbench > :last-child {
			grid-column: 1 / -1;
		}
	}

	@media (max-width: 900px) {
		.vault-title {
			font-size: 1.9rem;
		}

		.vault-hero,
		.vault-section-header,
		.vault-inline-header {
			flex-direction: column;
			align-items: stretch;
		}

		.vault-hero-actions,
		.vault-button-row {
			justify-content: flex-start;
		}

		.vault-grid-two,
		.vault-summary-strip,
		.vault-workbench {
			grid-template-columns: minmax(0, 1fr);
		}

		.vault-data-row,
		.vault-row-field {
			grid-template-columns: minmax(0, 1fr);
		}

		.vault-row-action,
		.vault-row-action-inline {
			justify-content: flex-start;
		}
	}

	@media (max-width: 640px) {
		.vault-route {
			padding: 0.85rem 0.75rem 4.5rem;
		}
	}
</style>
