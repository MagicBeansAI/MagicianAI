<script lang="ts">
	import { browser } from '$app/environment';
	import { goto } from '$app/navigation';
	import { onMount, onDestroy } from 'svelte';
	import Modal from '$lib/magician/components/generative/Modal.svelte';
	import Button from '$lib/magician/components/generative/Button.svelte';
	import Card from '$lib/magician/components/generative/Card.svelte';
	import Alert from '$lib/magician/components/generative/Alert.svelte';
	import Spinner from '$lib/magician/components/generative/Spinner.svelte';
	import { showError, showSuccess } from '$lib/shared/stores/notifications';
	import {
		resourceAuthorityStore,
		isFrozen,
		activeTokens,
		staleReservations,
		loadDashboard,
		saveCeiling,
		deleteCeiling,
		revokeToken,
		bootstrapAgent,
		withdrawFunds,
		issueRefund,
		addVendorCredit,
		freezeAll,
		unfreeze,
		commitReservation,
		rollbackReservation,
		flagReservation,
		formatAmount,
		formatPeriod,
		formatCarryover,
		usagePercent,
		usageColor,
		getResourceAuthorityApiKey,
		setResourceAuthorityApiKey,
		type CeilingPeriod,
		type CarryoverPolicy,
		type SystemCeiling,
		type SpendToken,
		type FlatTransaction,
		type Reservation
	} from '$lib/stores/resourceAuthorityStore';

	const REFRESH_MS = 15_000;
	let refreshHandle: ReturnType<typeof setInterval> | null = null;

	// Operator-entered resource-authority API key (the server's RESOURCE_AUTHORITY_API_KEY).
	// The key-gated resource-authority endpoints require it whenever the server key is set;
	// it is scoped to THIS admin dashboard only (never a global fetch header).
	let apiKeyInput = browser ? getResourceAuthorityApiKey() : '';
	function saveApiKey() {
		setResourceAuthorityApiKey(apiKeyInput);
		showSuccess('Resource-authority API key saved');
		loadDashboard().catch((e: unknown) => showError(`Reload failed: ${String(e)}`));
	}

	// ---------------------------------------------------------------------------
	// Dashboard data (reactive)
	// ---------------------------------------------------------------------------
	$: state = $resourceAuthorityStore;
	$: frozen = $isFrozen;
	$: tokens = $activeTokens;
	$: staleRes = $staleReservations;
	$: recentTxns = state.transactions.slice(0, 8);

	onMount(() => {
		loadDashboard().catch((e: unknown) => console.warn('[resource-authority] load failed:', e));
		refreshHandle = setInterval(() => {
			loadDashboard().catch(() => {});
		}, REFRESH_MS);
	});

	onDestroy(() => {
		if (refreshHandle) clearInterval(refreshHandle);
	});

	// ---------------------------------------------------------------------------
	// Modal states
	// ---------------------------------------------------------------------------
	let ceilingModalOpen = false;
	let editingCeiling: SystemCeiling | null = null;
	let ceilForm = resetCeilForm();

	let revokeModalOpen = false;
	let revokeTarget: SpendToken | null = null;

	let bootstrapModalOpen = false;
	let bootstrapForm = { agent_id: '', commodity: 'USD', amount: 0 };

	let withdrawModalOpen = false;
	let withdrawForm = { agent_id: '', commodity: 'USD', amount: 0 };

	let refundModalOpen = false;
	let refundForm = { token_id: '', amount: 0, reason: '' };

	let creditModalOpen = false;
	let creditForm = { agent_id: '', commodity: 'USD', amount: 0, reason: '' };

	let freezeModalOpen = false;
	let freezeReason = '';

	let unfreezeModalOpen = false;
	let unfreezeReason = '';

	let resolveModalOpen = false;
	let resolveTarget: Reservation | null = null;

	let modalBusy = false;

	// ---------------------------------------------------------------------------
	// Ceiling form helpers
	// ---------------------------------------------------------------------------
	function resetCeilForm() {
		return {
			commodity: 'USD',
			ceiling: 0,
			period: 'monthly' as CeilingPeriod,
			carryover_type: 'none' as 'none' | 'full' | 'capped',
			carryover_cap: 0,
			relaxation: 2
		};
	}

	function openAddCeiling() {
		editingCeiling = null;
		ceilForm = resetCeilForm();
		ceilingModalOpen = true;
	}

	function openEditCeiling(c: SystemCeiling) {
		editingCeiling = c;
		ceilForm = {
			commodity: c.commodity,
			ceiling: c.ceiling,
			period: c.period,
			carryover_type: c.carryover.type,
			carryover_cap: c.carryover.type === 'capped' ? (c.carryover as { type: 'capped'; cap: number }).cap : 0,
			relaxation: c.relaxation
		};
		ceilingModalOpen = true;
	}

	async function handleSaveCeiling() {
		modalBusy = true;
		try {
			const carryover: CarryoverPolicy =
				ceilForm.carryover_type === 'capped'
					? { type: 'capped', cap: ceilForm.carryover_cap }
					: { type: ceilForm.carryover_type };
			await saveCeiling({
				id: editingCeiling?.id,
				commodity: ceilForm.commodity,
				ceiling: ceilForm.ceiling,
				period: ceilForm.period,
				carryover,
				relaxation: ceilForm.relaxation
			});
			showSuccess('Ceiling saved');
			ceilingModalOpen = false;
		} catch (err) {
			showError('Failed to save ceiling', err instanceof Error ? err.message : String(err));
		} finally {
			modalBusy = false;
		}
	}

	async function handleDeleteCeiling(id: string) {
		try {
			await deleteCeiling(id);
			showSuccess('Ceiling removed');
		} catch (err) {
			showError('Failed to delete ceiling', err instanceof Error ? err.message : String(err));
		}
	}

	// ---------------------------------------------------------------------------
	// Revoke
	// ---------------------------------------------------------------------------
	function openRevoke(token: SpendToken) {
		revokeTarget = token;
		revokeModalOpen = true;
	}

	async function handleRevoke() {
		if (!revokeTarget) return;
		modalBusy = true;
		try {
			await revokeToken(revokeTarget.id);
			showSuccess(`Token ${revokeTarget.id} revoked`);
			revokeModalOpen = false;
			revokeTarget = null;
		} catch (err) {
			showError('Revoke failed', err instanceof Error ? err.message : String(err));
		} finally {
			modalBusy = false;
		}
	}

	// ---------------------------------------------------------------------------
	// Fund management
	// ---------------------------------------------------------------------------
	async function handleBootstrap() {
		modalBusy = true;
		try {
			await bootstrapAgent(bootstrapForm);
			showSuccess('Agent funded');
			bootstrapModalOpen = false;
		} catch (err) {
			showError('Bootstrap failed', err instanceof Error ? err.message : String(err));
		} finally {
			modalBusy = false;
		}
	}

	async function handleWithdraw() {
		modalBusy = true;
		try {
			await withdrawFunds(withdrawForm);
			showSuccess('Funds withdrawn');
			withdrawModalOpen = false;
		} catch (err) {
			showError('Withdraw failed', err instanceof Error ? err.message : String(err));
		} finally {
			modalBusy = false;
		}
	}

	async function handleRefund() {
		modalBusy = true;
		try {
			await issueRefund(refundForm);
			showSuccess('Refund issued');
			refundModalOpen = false;
		} catch (err) {
			showError('Refund failed', err instanceof Error ? err.message : String(err));
		} finally {
			modalBusy = false;
		}
	}

	async function handleCredit() {
		modalBusy = true;
		try {
			await addVendorCredit(creditForm);
			showSuccess('Credit added');
			creditModalOpen = false;
		} catch (err) {
			showError('Credit failed', err instanceof Error ? err.message : String(err));
		} finally {
			modalBusy = false;
		}
	}

	// ---------------------------------------------------------------------------
	// Freeze
	// ---------------------------------------------------------------------------
	async function handleFreeze() {
		modalBusy = true;
		try {
			await freezeAll(freezeReason);
			showSuccess('System frozen');
			freezeModalOpen = false;
			freezeReason = '';
		} catch (err) {
			showError('Freeze failed', err instanceof Error ? err.message : String(err));
		} finally {
			modalBusy = false;
		}
	}

	async function handleUnfreeze() {
		modalBusy = true;
		try {
			await unfreeze(unfreezeReason);
			showSuccess('System unfrozen');
			unfreezeModalOpen = false;
			unfreezeReason = '';
		} catch (err) {
			showError('Unfreeze failed', err instanceof Error ? err.message : String(err));
		} finally {
			modalBusy = false;
		}
	}

	// ---------------------------------------------------------------------------
	// Resolve stale reservation
	// ---------------------------------------------------------------------------
	function openResolve(res: Reservation) {
		resolveTarget = res;
		resolveModalOpen = true;
	}

	async function handleCommitReservation() {
		if (!resolveTarget) return;
		modalBusy = true;
		try {
			await commitReservation(resolveTarget.id);
			showSuccess('Reservation committed');
			resolveModalOpen = false;
			resolveTarget = null;
		} catch (err) {
			showError('Commit failed', err instanceof Error ? err.message : String(err));
		} finally {
			modalBusy = false;
		}
	}

	async function handleRollbackReservation() {
		if (!resolveTarget) return;
		modalBusy = true;
		try {
			await rollbackReservation(resolveTarget.id);
			showSuccess('Reservation rolled back');
			resolveModalOpen = false;
			resolveTarget = null;
		} catch (err) {
			showError('Rollback failed', err instanceof Error ? err.message : String(err));
		} finally {
			modalBusy = false;
		}
	}

	async function handleFlagReservation() {
		if (!resolveTarget) return;
		modalBusy = true;
		try {
			await flagReservation(resolveTarget.id);
			showSuccess('Reservation flagged for review');
			resolveModalOpen = false;
			resolveTarget = null;
		} catch (err) {
			showError('Flag failed', err instanceof Error ? err.message : String(err));
		} finally {
			modalBusy = false;
		}
	}

	// ---------------------------------------------------------------------------
	// Helpers
	// ---------------------------------------------------------------------------
	function formatTimestamp(value: string): string {
		try {
			const d = new Date(value);
			if (isNaN(d.getTime())) return value;
			return d.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit', second: '2-digit' });
		} catch {
			return value;
		}
	}

	function formatDate(value: string | undefined): string {
		if (!value) return 'n/a';
		try {
			const d = new Date(value);
			if (isNaN(d.getTime())) return value;
			return d.toLocaleDateString(undefined, { month: 'short', day: 'numeric' });
		} catch {
			return value;
		}
	}

	const periodOptions: CeilingPeriod[] = ['monthly', 'weekly', 'daily', 'hourly', 'quarterly', 'annual', 'total'];
</script>

<svelte:head>
	<title>Budget · Magican</title>
</svelte:head>

<div class="presto-gaui-page ra-page">
	<!-- Frozen banner (global) -->
	{#if frozen}
		<div class="ra-frozen-banner">
			<div class="ra-frozen-content">
				<span class="ra-frozen-icon">&#x1F534;</span>
				<span class="ra-frozen-text">
					SYSTEM FROZEN &mdash; All spending halted
					{#if state.freezeStatus.frozen_at}
						since {formatDate(state.freezeStatus.frozen_at)}
					{/if}
				</span>
				{#if state.freezeStatus.reason}
					<span class="ra-frozen-reason">Reason: "{state.freezeStatus.reason}"</span>
				{/if}
				{#if state.freezeStatus.frozen_by}
					<span class="ra-frozen-by">Frozen by: {state.freezeStatus.frozen_by}</span>
				{/if}
			</div>
			<Button label="UNFREEZE" variant="secondary" on:click={() => { unfreezeReason = ''; unfreezeModalOpen = true; }} />
		</div>
	{/if}

	<!-- Header -->
	<Card className="ra-hero" elevation={1}>
		<div class="ra-hero-copy">
			<p class="ra-kicker">Budget</p>
			<h1>Resource Authority</h1>
			<p>Ceilings, tokens, and fund management.</p>
		</div>
		<div class="ra-hero-actions">
			<Button label="Audit" variant="secondary" on:click={() => goto('/budget/audit')} />
			<Button
				label={frozen ? 'FROZEN' : 'FREEZE ALL'}
				variant="primary" className="ra-btn-danger"
				disabled={frozen}
				on:click={() => { freezeReason = ''; freezeModalOpen = true; }}
			/>
		</div>
	</Card>

	<Card className="ra-section-card" elevation={0}>
		<div class="ra-section-header">
			<div>
				<h2 class="ra-section-title">API Key</h2>
				<p class="ra-sub">
					Required when the server sets <code>RESOURCE_AUTHORITY_API_KEY</code>. Stored
					locally in this browser and sent only to
					<code>/api/magician/v2/resource-authority</code> requests — keep this dashboard
					operator-only.
				</p>
			</div>
			<div style="display:flex; gap:0.5rem; align-items:center;">
				<input
					type="password"
					placeholder="paste RESOURCE_AUTHORITY_API_KEY"
					bind:value={apiKeyInput}
					autocomplete="off"
					spellcheck="false"
					style="min-width:20rem; padding:0.5rem 0.75rem;"
				/>
				<Button label="Save" variant="secondary" on:click={saveApiKey} />
			</div>
		</div>
	</Card>

	{#if state.loading && state.ceilings.length === 0}
		<div class="ra-loading">
			<Spinner />
			<span>Loading resource authority data...</span>
		</div>
	{:else if state.error && state.ceilings.length === 0}
		<Alert type="error" message={state.error} closable />
	{:else}
		<!-- SYSTEM CEILINGS -->
		<Card className="ra-section-card" elevation={0}>
			<div class="ra-section-header">
				<h2 class="ra-section-title">System Ceilings</h2>
				<Button label="+ Add New" size="sm" on:click={openAddCeiling} />
			</div>
			{#if state.ceilings.length === 0}
				<div class="ra-empty">No ceilings configured</div>
			{:else}
				<div class="ra-table-wrap">
					<table class="ra-table">
						<thead>
							<tr>
								<th>Commodity</th>
								<th>Ceiling</th>
								<th>Period</th>
								<th>Carryover</th>
								<th>Usage</th>
								<th>Actions</th>
							</tr>
						</thead>
						<tbody>
							{#each state.ceilings as c (c.id)}
								{@const pct = usagePercent(c.spent_in_period, c.ceiling)}
								<tr>
									<td class="ra-cell-commodity">{c.commodity}</td>
									<td>
										<div>{formatAmount(c.ceiling, c.commodity)}</div>
										{#if c.relaxation > 0}
											<div class="ra-sub">+{c.relaxation}% relaxation</div>
										{/if}
									</td>
									<td>{formatPeriod(c.period)}</td>
									<td>{formatCarryover(c.carryover)}</td>
									<td>
										<div class="ra-usage-bar-container">
											<div class="ra-usage-bar-track">
												<div
													class="ra-usage-bar-fill"
													style="width: {pct}%; background: {usageColor(pct)};"
												></div>
											</div>
											<span class="ra-usage-pct" style="color: {usageColor(pct)}">{Math.round(pct)}%</span>
										</div>
										<div class="ra-sub">{formatAmount(c.spent_in_period, c.commodity)}/{formatAmount(c.ceiling, c.commodity)}</div>
										<div class="ra-sub">{formatAmount(c.remaining_in_period, c.commodity)} remaining</div>
									</td>
									<td>
										<Button label="Edit" variant="secondary" size="sm" on:click={() => openEditCeiling(c)} />
										<Button label="Del" variant="outline" className="ra-btn-ghost" size="sm" on:click={() => handleDeleteCeiling(c.id)} />
									</td>
								</tr>
							{/each}
						</tbody>
					</table>
				</div>
			{/if}
		</Card>

		<!-- ACTIVE TOKENS -->
		<Card className="ra-section-card" elevation={0}>
			<div class="ra-section-header">
				<h2 class="ra-section-title">Active Tokens</h2>
			</div>
			{#if tokens.length === 0}
				<div class="ra-empty">No active tokens</div>
			{:else}
				<div class="ra-table-wrap">
					<table class="ra-table">
						<thead>
							<tr>
								<th>Token</th>
								<th>Issued To</th>
								<th>Rate</th>
								<th>Usage</th>
								<th>Velocity</th>
								<th>Expires</th>
								<th>Action</th>
							</tr>
						</thead>
						<tbody>
							{#each tokens as token (token.id)}
								{@const pct = usagePercent(token.spent_in_period, token.ceiling)}
								<tr
									class="ra-row-clickable"
									on:click={() => goto(`/budget/tokens/${encodeURIComponent(token.id)}`)}
								>
									<td class="ra-cell-mono">{token.id}</td>
									<td>{token.issued_to}</td>
									<td>
										{formatAmount(token.ceiling, token.commodity)}/{formatPeriod(token.period)}
									</td>
									<td>
										<div class="ra-usage-bar-container">
											<div class="ra-usage-bar-track">
												<div
													class="ra-usage-bar-fill"
													style="width: {pct}%; background: {usageColor(pct)};"
												></div>
											</div>
											<span class="ra-usage-pct" style="color: {usageColor(pct)}">{Math.round(pct)}%</span>
										</div>
										<div class="ra-sub">{formatAmount(token.spent_in_period, token.commodity)}/{formatAmount(token.ceiling, token.commodity)}</div>
									</td>
									<td>
										{#if token.velocity_limit}
											{formatAmount(token.velocity_limit.max_amount, token.commodity)}/{token.velocity_limit.window_seconds}s
										{:else}
											&mdash;
										{/if}
									</td>
									<td>{formatDate(token.expires_at)}</td>
									<td>
										<!-- svelte-ignore a11y_click_events_have_key_events -->
										<Button label="Revoke" variant="primary" className="ra-btn-danger" size="sm" stopPropagation on:click={() => openRevoke(token)} />
									</td>
								</tr>
							{/each}
						</tbody>
					</table>
				</div>
			{/if}
		</Card>

		<!-- ALERTS -->
		{#if state.alerts.length > 0}
			<Card className="ra-section-card" elevation={0}>
				<div class="ra-section-header">
					<h2 class="ra-section-title">Alerts</h2>
				</div>
				<div class="ra-alerts">
					{#each state.alerts as alert (alert.id)}
						<div class="ra-alert-item ra-alert-{alert.severity}">
							<span class="ra-alert-icon">
								{#if alert.severity === 'critical'}&#x1F525;{:else if alert.severity === 'warning'}&#x26A0;{:else}&#x2139;{/if}
							</span>
							<span class="ra-alert-msg">{alert.message}</span>
							{#if alert.reservation_id}
								<Button label="Resolve" variant="secondary" size="sm" on:click={() => {
									const res = state.reservations.find((r) => r.id === alert.reservation_id);
									if (res) openResolve(res);
								}} />
							{/if}
						</div>
					{/each}
				</div>
			</Card>
		{/if}

		<!-- RECENT TRANSACTIONS -->
		<Card className="ra-section-card" elevation={0}>
			<div class="ra-section-header">
				<h2 class="ra-section-title">Recent Transactions</h2>
				<Button label="View All" variant="secondary" size="sm" on:click={() => goto('/budget/transactions')} />
			</div>
			{#if recentTxns.length === 0}
				<div class="ra-empty">No transactions yet</div>
			{:else}
				<div class="ra-table-wrap">
					<table class="ra-table">
						<thead>
							<tr>
								<th>Accrual</th>
								<th>Agent</th>
								<th>Account</th>
								<th>Amount</th>
								<th>Reference</th>
							</tr>
						</thead>
						<tbody>
							{#each recentTxns as txn (txn.id + txn.account)}
								<tr>
									<td class="ra-cell-mono ra-cell-time">{formatTimestamp(txn.accrual_date)}</td>
									<td>{txn.agent_id}</td>
									<td class="ra-cell-mono">{txn.account}</td>
									<td class:ra-amount-neg={txn.amount < 0}>{formatAmount(txn.amount, txn.commodity)}</td>
									<td class="ra-cell-ref">{txn.reference}</td>
								</tr>
							{/each}
						</tbody>
					</table>
				</div>
			{/if}
		</Card>

		<!-- FUND MANAGEMENT -->
		<Card className="ra-section-card ra-fund-bar" elevation={0}>
			<h2 class="ra-section-title">Fund Management</h2>
			<div class="ra-fund-actions">
				<Button label="Bootstrap Agent" on:click={() => { bootstrapForm = { agent_id: '', commodity: 'USD', amount: 0 }; bootstrapModalOpen = true; }} />
				<Button label="Withdraw Funds" variant="secondary" on:click={() => { withdrawForm = { agent_id: '', commodity: 'USD', amount: 0 }; withdrawModalOpen = true; }} />
				<Button label="Issue Refund" variant="secondary" on:click={() => { refundForm = { token_id: '', amount: 0, reason: '' }; refundModalOpen = true; }} />
				<Button label="Add Credit" variant="secondary" on:click={() => { creditForm = { agent_id: '', commodity: 'USD', amount: 0, reason: '' }; creditModalOpen = true; }} />
			</div>
		</Card>
	{/if}
</div>

<!-- ======================================================================= -->
<!-- MODALS                                                                  -->
<!-- ======================================================================= -->

<!-- Add/Edit Ceiling -->
<Modal open={ceilingModalOpen} title={editingCeiling ? 'Edit System Ceiling' : 'Add System Ceiling'} on:close={() => (ceilingModalOpen = false)}>
	<div class="ra-modal-form">
		<label class="ra-field">
			<span class="ra-field-label">Commodity</span>
			<input type="text" class="ra-input" bind:value={ceilForm.commodity} placeholder="USD" />
		</label>
		<label class="ra-field">
			<span class="ra-field-label">Ceiling</span>
			<input type="number" class="ra-input" bind:value={ceilForm.ceiling} min="0" step="0.01" />
		</label>
		<fieldset class="ra-fieldset">
			<legend class="ra-field-label">Period</legend>
			{#each periodOptions as p}
				<label class="ra-radio">
					<input type="radio" bind:group={ceilForm.period} value={p} />
					{formatPeriod(p)}
				</label>
			{/each}
		</fieldset>
		<fieldset class="ra-fieldset">
			<legend class="ra-field-label">Carryover</legend>
			<label class="ra-radio">
				<input type="radio" bind:group={ceilForm.carryover_type} value="none" />
				None (use-it-or-lose)
			</label>
			<label class="ra-radio">
				<input type="radio" bind:group={ceilForm.carryover_type} value="full" />
				Full (roll all)
			</label>
			<label class="ra-radio">
				<input type="radio" bind:group={ceilForm.carryover_type} value="capped" />
				Capped:
			</label>
			{#if ceilForm.carryover_type === 'capped'}
				<input type="number" class="ra-input ra-input-inline" bind:value={ceilForm.carryover_cap} min="0" />
			{/if}
		</fieldset>
		<label class="ra-field">
			<span class="ra-field-label">Relaxation %</span>
			<input type="number" class="ra-input ra-input-inline" bind:value={ceilForm.relaxation} min="0" max="100" />
			<span class="ra-hint">(tolerance for metered variance)</span>
		</label>
		<div class="ra-modal-actions">
			<Button label="Cancel" variant="secondary" on:click={() => (ceilingModalOpen = false)} />
			<Button label={modalBusy ? 'Saving...' : 'Save Ceiling'} disabled={modalBusy} on:click={handleSaveCeiling} />
		</div>
	</div>
</Modal>

<!-- Revoke Token -->
<Modal open={revokeModalOpen} title="Revoke Token" size="md" on:close={() => (revokeModalOpen = false)}>
	{#if revokeTarget}
		<div class="ra-modal-form">
			<div class="ra-info-grid">
				<span class="ra-info-label">Token</span><span class="ra-info-value">{revokeTarget.id}</span>
				<span class="ra-info-label">Issued To</span><span class="ra-info-value">{revokeTarget.issued_to}</span>
				<span class="ra-info-label">Commodity</span><span class="ra-info-value">{revokeTarget.commodity}</span>
				<span class="ra-info-label">Remaining</span><span class="ra-info-value">{formatAmount(revokeTarget.remaining_in_period, revokeTarget.commodity)}</span>
			</div>
			<div class="ra-warning-box">
				<p>This will:</p>
				<ul>
					<li>Block all future spend on this token</li>
					<li>Return remaining balance to issuer's available</li>
					<li>Record a revert journal entry</li>
				</ul>
				<p>Any in-flight reservation will complete but no new reserves allowed.</p>
			</div>
			<div class="ra-modal-actions">
				<Button label="Cancel" variant="secondary" on:click={() => (revokeModalOpen = false)} />
				<Button label={modalBusy ? 'Revoking...' : 'Revoke Token'} variant="primary" className="ra-btn-danger" disabled={modalBusy} on:click={handleRevoke} />
			</div>
		</div>
	{/if}
</Modal>

<!-- Bootstrap Agent -->
<Modal open={bootstrapModalOpen} title="Fund Agent" size="md" on:close={() => (bootstrapModalOpen = false)}>
	<div class="ra-modal-form">
		<label class="ra-field">
			<span class="ra-field-label">Agent</span>
			<input type="text" class="ra-input" bind:value={bootstrapForm.agent_id} placeholder="Agent ID" />
		</label>
		<label class="ra-field">
			<span class="ra-field-label">Commodity</span>
			<input type="text" class="ra-input" bind:value={bootstrapForm.commodity} placeholder="USD" />
		</label>
		<label class="ra-field">
			<span class="ra-field-label">Amount</span>
			<input type="number" class="ra-input" bind:value={bootstrapForm.amount} min="0" step="0.01" />
		</label>
		<div class="ra-journal-preview">
			<p class="ra-preview-label">This creates a journal entry:</p>
			<div class="ra-journal-line">DR agent:{bootstrapForm.agent_id || '?'}:available:{bootstrapForm.commodity} +{formatAmount(bootstrapForm.amount, bootstrapForm.commodity)}</div>
			<div class="ra-journal-line">CR system:allocation:{bootstrapForm.commodity} -{formatAmount(bootstrapForm.amount, bootstrapForm.commodity)}</div>
		</div>
		<div class="ra-modal-actions">
			<Button label="Cancel" variant="secondary" on:click={() => (bootstrapModalOpen = false)} />
			<Button label={modalBusy ? 'Funding...' : 'Fund Agent'} disabled={modalBusy} on:click={handleBootstrap} />
		</div>
	</div>
</Modal>

<!-- Withdraw Funds -->
<Modal open={withdrawModalOpen} title="Withdraw Funds" size="md" on:close={() => (withdrawModalOpen = false)}>
	<div class="ra-modal-form">
		<label class="ra-field">
			<span class="ra-field-label">Agent</span>
			<input type="text" class="ra-input" bind:value={withdrawForm.agent_id} placeholder="Agent ID" />
		</label>
		<label class="ra-field">
			<span class="ra-field-label">Commodity</span>
			<input type="text" class="ra-input" bind:value={withdrawForm.commodity} placeholder="USD" />
		</label>
		<label class="ra-field">
			<span class="ra-field-label">Amount</span>
			<input type="number" class="ra-input" bind:value={withdrawForm.amount} min="0" step="0.01" />
		</label>
		<div class="ra-journal-preview">
			<p class="ra-preview-label">This creates a journal entry:</p>
			<div class="ra-journal-line">DR system:allocation:{withdrawForm.commodity} +{formatAmount(withdrawForm.amount, withdrawForm.commodity)}</div>
			<div class="ra-journal-line">CR agent:{withdrawForm.agent_id || '?'}:available:{withdrawForm.commodity} -{formatAmount(withdrawForm.amount, withdrawForm.commodity)}</div>
		</div>
		<div class="ra-modal-actions">
			<Button label="Cancel" variant="secondary" on:click={() => (withdrawModalOpen = false)} />
			<Button label={modalBusy ? 'Withdrawing...' : 'Withdraw'} disabled={modalBusy} on:click={handleWithdraw} />
		</div>
	</div>
</Modal>

<!-- Issue Refund -->
<Modal open={refundModalOpen} title="Issue Refund" size="md" on:close={() => (refundModalOpen = false)}>
	<div class="ra-modal-form">
		<label class="ra-field">
			<span class="ra-field-label">Token</span>
			<input type="text" class="ra-input" bind:value={refundForm.token_id} placeholder="Token ID" />
		</label>
		<label class="ra-field">
			<span class="ra-field-label">Refund Amount</span>
			<input type="number" class="ra-input" bind:value={refundForm.amount} min="0" step="0.01" />
		</label>
		<label class="ra-field">
			<span class="ra-field-label">Reason</span>
			<input type="text" class="ra-input" bind:value={refundForm.reason} placeholder="e.g. Invalid click refund" />
		</label>
		<div class="ra-journal-preview">
			<p class="ra-preview-label">This reverses expense back to budget:</p>
			<div class="ra-journal-line">DR token:{refundForm.token_id || '?'}:budget +{refundForm.amount}</div>
			<div class="ra-journal-line">CR token:{refundForm.token_id || '?'}:expense -{refundForm.amount}</div>
		</div>
		<div class="ra-modal-actions">
			<Button label="Cancel" variant="secondary" on:click={() => (refundModalOpen = false)} />
			<Button label={modalBusy ? 'Issuing...' : 'Issue Refund'} disabled={modalBusy} on:click={handleRefund} />
		</div>
	</div>
</Modal>

<!-- Vendor Credit -->
<Modal open={creditModalOpen} title="Add Vendor Credit" size="md" on:close={() => (creditModalOpen = false)}>
	<div class="ra-modal-form">
		<label class="ra-field">
			<span class="ra-field-label">Agent</span>
			<input type="text" class="ra-input" bind:value={creditForm.agent_id} placeholder="Agent ID" />
		</label>
		<label class="ra-field">
			<span class="ra-field-label">Commodity</span>
			<input type="text" class="ra-input" bind:value={creditForm.commodity} placeholder="USD" />
		</label>
		<label class="ra-field">
			<span class="ra-field-label">Credit Amount</span>
			<input type="number" class="ra-input" bind:value={creditForm.amount} min="0" step="0.01" />
		</label>
		<label class="ra-field">
			<span class="ra-field-label">Reason</span>
			<input type="text" class="ra-input" bind:value={creditForm.reason} placeholder="e.g. AWS promo credit" />
		</label>
		<div class="ra-journal-preview">
			<p class="ra-preview-label">This creates a vendor credit entry:</p>
			<div class="ra-journal-line">DR agent:{creditForm.agent_id || '?'}:available:{creditForm.commodity} +{formatAmount(creditForm.amount, creditForm.commodity)}</div>
			<div class="ra-journal-line">CR system:vendor_credits:{creditForm.commodity} -{formatAmount(creditForm.amount, creditForm.commodity)}</div>
		</div>
		<div class="ra-modal-actions">
			<Button label="Cancel" variant="secondary" on:click={() => (creditModalOpen = false)} />
			<Button label={modalBusy ? 'Adding...' : 'Add Credit'} disabled={modalBusy} on:click={handleCredit} />
		</div>
	</div>
</Modal>

<!-- Freeze Confirmation -->
<Modal open={freezeModalOpen} title="FREEZE ALL SPENDING" size="md" on:close={() => (freezeModalOpen = false)}>
	<div class="ra-modal-form">
		<div class="ra-warning-box ra-warning-box-critical">
			<p>This will <strong>IMMEDIATELY</strong> block all new spend-bearing actions across the entire system.</p>
			<ul>
				<li>{tokens.length} active token(s)</li>
				<li>{staleRes.length} stale reservation(s)</li>
			</ul>
			<p>In-flight reservations will complete but <strong>NO</strong> new reserves will be accepted.</p>
		</div>
		<label class="ra-field">
			<span class="ra-field-label">Reason</span>
			<input type="text" class="ra-input" bind:value={freezeReason} placeholder="Why are you freezing?" />
		</label>
		<div class="ra-modal-actions">
			<Button label="Cancel" variant="secondary" on:click={() => (freezeModalOpen = false)} />
			<Button label={modalBusy ? 'Freezing...' : 'FREEZE NOW'} variant="primary" className="ra-btn-danger" disabled={modalBusy} on:click={handleFreeze} />
		</div>
	</div>
</Modal>

<!-- Unfreeze Confirmation -->
<Modal open={unfreezeModalOpen} title="Unfreeze System" size="md" on:close={() => (unfreezeModalOpen = false)}>
	<div class="ra-modal-form">
		<label class="ra-field">
			<span class="ra-field-label">Reason for unfreezing</span>
			<input type="text" class="ra-input" bind:value={unfreezeReason} placeholder="e.g. Issue resolved, resuming operations" />
		</label>
		<div class="ra-modal-actions">
			<Button label="Cancel" variant="secondary" on:click={() => (unfreezeModalOpen = false)} />
			<Button label={modalBusy ? 'Unfreezing...' : 'Unfreeze System'} disabled={modalBusy || !unfreezeReason.trim()} on:click={handleUnfreeze} />
		</div>
	</div>
</Modal>

<!-- Resolve Stale Reservation -->
<Modal open={resolveModalOpen} title="Resolve Stale Reservation" size="lg" on:close={() => (resolveModalOpen = false)}>
	{#if resolveTarget}
		<div class="ra-modal-form">
			<div class="ra-info-grid">
				<span class="ra-info-label">Reservation</span><span class="ra-info-value">{resolveTarget.id}</span>
				<span class="ra-info-label">Token</span><span class="ra-info-value">{resolveTarget.token_id}</span>
				<span class="ra-info-label">Amount</span><span class="ra-info-value">{formatAmount(resolveTarget.amount, resolveTarget.commodity)}</span>
				<span class="ra-info-label">Agent</span><span class="ra-info-value">{resolveTarget.agent_id}</span>
				<span class="ra-info-label">Created</span><span class="ra-info-value">{formatTimestamp(resolveTarget.created_at)}</span>
				<span class="ra-info-label">Max Duration</span><span class="ra-info-value">{resolveTarget.max_duration_secs}s (EXCEEDED)</span>
			</div>
			<p class="ra-resolve-prompt">Based on reconciliation check, choose an action:</p>
			<div class="ra-resolve-actions">
				<Button label="Commit — action completed" disabled={modalBusy} on:click={handleCommitReservation} />
				<Button label="Rollback — action did not complete" variant="secondary" disabled={modalBusy} on:click={handleRollbackReservation} />
				<Button label="Flag for manual review" variant="secondary" disabled={modalBusy} on:click={handleFlagReservation} />
			</div>
		</div>
	{/if}
</Modal>

<style>
	/* ===== Layout ===== */
	.ra-page {
		display: grid;
		gap: 1rem;
	}

	.ra-page :global(.muij-card) {
		margin-bottom: 0;
	}

	.ra-page :global(.muij-button) {
		white-space: nowrap;
		overflow-wrap: normal;
	}

	/* ===== Hero card ===== */
	:global(.ra-hero.muij-card) {
		display: flex;
		justify-content: space-between;
		align-items: flex-end;
		gap: 1.5rem;
		padding: 1.4rem 1.5rem;
		border: 1px solid color-mix(in srgb, var(--border-soft, #d8d0c5) 72%, transparent);
		border-radius: var(--radius-lg, 18px);
		background:
			radial-gradient(
				circle at top right,
				color-mix(in srgb, var(--accent-primary, #bf6f45) 14%, transparent),
				transparent 40%
			),
			linear-gradient(
				180deg,
				color-mix(in srgb, var(--bg-card, #fff) 94%, var(--bg-soft, #f6f1e8) 6%),
				var(--bg-card, #fff)
			);
		box-shadow: 0 18px 40px rgba(22, 24, 35, 0.06);
	}

	.ra-kicker {
		margin: 0 0 0.35rem;
		font-size: 0.78rem;
		font-weight: 700;
		letter-spacing: 0.12em;
		text-transform: uppercase;
		color: var(--text-muted, #8a847a);
	}

	.ra-hero-copy h1 {
		margin: 0;
		font-family: var(--font-display, var(--font-primary));
		font-size: clamp(2rem, 4vw, 2.9rem);
		line-height: 0.98;
		letter-spacing: -0.04em;
		color: var(--text-primary, #2d2a26);
	}

	.ra-hero-copy > p:last-child {
		margin: 0.5rem 0 0;
		max-width: 60ch;
		color: var(--text-secondary, #5f5b55);
	}

	.ra-hero-actions {
		display: flex;
		flex-wrap: wrap;
		justify-content: flex-end;
		gap: 0.75rem;
	}

	.ra-loading {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		padding: 2rem;
		justify-content: center;
		color: var(--text-muted, #8a847a);
	}

	.ra-empty {
		padding: 1.5rem;
		text-align: center;
		color: var(--text-muted, #8a847a);
		font-size: 0.875rem;
	}

	/* ===== Frozen banner ===== */
	.ra-frozen-banner {
		display: flex;
		align-items: center;
		justify-content: space-between;
		background: linear-gradient(
			180deg,
			color-mix(in srgb, #fef2f2 76%, var(--bg-card, #fff)),
			color-mix(in srgb, #fef2f2 52%, var(--bg-soft, #f6f1e8))
		);
		border: 1px solid color-mix(in srgb, #b91c1c 24%, var(--border-soft, #d8d0c5));
		border-radius: var(--radius-md, 14px);
		padding: 0.85rem 1.1rem;
		flex-wrap: wrap;
		gap: 0.5rem;
	}

	.ra-frozen-content {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		flex-wrap: wrap;
	}

	.ra-frozen-icon {
		font-size: 1rem;
	}

	.ra-frozen-text {
		font-weight: 700;
		font-size: 0.875rem;
		color: #991b1b;
	}

	.ra-frozen-reason,
	.ra-frozen-by {
		font-size: 0.8rem;
		color: #7f1d1d;
	}


	/* ===== Section cards ===== */
	:global(.ra-section-card.muij-card) {
		padding: 1rem 1.25rem;
		border: 1px solid color-mix(in srgb, var(--border-soft, #d8d0c5) 70%, transparent);
		border-radius: var(--radius-md, 14px);
		background:
			linear-gradient(
				180deg,
				color-mix(in srgb, var(--bg-card, #fff) 92%, transparent),
				color-mix(in srgb, var(--bg-soft, #f6f1e8) 44%, transparent)
			),
			var(--bg-card, #fff);
		box-shadow: 0 14px 32px rgba(22, 24, 35, 0.05);
	}

	.ra-section-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		margin-bottom: 0.75rem;
	}

	.ra-section-title {
		font-size: 0.95rem;
		font-weight: 600;
		color: var(--text-secondary, #4a4540);
		margin: 0;
		text-transform: uppercase;
		letter-spacing: 0.04em;
	}

	/* ===== Table ===== */
	.ra-table-wrap {
		overflow-x: auto;
	}

	.ra-table {
		width: 100%;
		border-collapse: collapse;
		font-size: 0.8125rem;
	}

	.ra-table th {
		text-align: left;
		padding: 0.5rem 0.75rem;
		font-weight: 600;
		color: var(--text-muted, #8a847a);
		border-bottom: 1px solid var(--border-soft, #ebe7e0);
		white-space: nowrap;
	}

	.ra-table td {
		padding: 0.6rem 0.75rem;
		border-bottom: 1px solid var(--border-soft, #ebe7e0);
		vertical-align: top;
	}

	.ra-table tbody tr:last-child td {
		border-bottom: none;
	}

	.ra-row-clickable {
		cursor: pointer;
		transition: background 0.12s;
	}

	.ra-row-clickable:hover {
		background: var(--bg-soft, #f3f0ea);
	}

	.ra-cell-commodity {
		font-weight: 600;
	}

	.ra-cell-mono {
		font-family: var(--font-mono);
		font-size: 0.75rem;
	}

	.ra-cell-time {
		white-space: nowrap;
	}

	.ra-cell-ref {
		max-width: 160px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.ra-sub {
		font-size: 0.7rem;
		color: var(--text-muted, #8a847a);
		margin-top: 2px;
	}

	.ra-amount-neg {
		color: var(--color-success, #5fa67a);
	}

	.ra-txn-type {
		white-space: nowrap;
		font-size: 0.75rem;
	}

	/* ===== Usage bar ===== */
	.ra-usage-bar-container {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		min-width: 120px;
	}

	.ra-usage-bar-track {
		flex: 1;
		height: 8px;
		background: var(--bg-soft, #f3f0ea);
		border-radius: 999px;
		overflow: hidden;
	}

	.ra-usage-bar-fill {
		height: 100%;
		border-radius: 999px;
		transition: width 0.3s ease;
	}

	.ra-usage-pct {
		font-family: var(--font-mono);
		font-size: 0.7rem;
		font-weight: 600;
		min-width: 2.5em;
		text-align: right;
	}

	/* ===== Alerts ===== */
	.ra-alerts {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
	}

	.ra-alert-item {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		padding: 0.5rem 0.75rem;
		border-radius: 8px;
		font-size: 0.8125rem;
	}

	.ra-alert-critical {
		background: #fef2f2;
		color: #991b1b;
		border: 1px solid #fca5a5;
	}

	.ra-alert-warning {
		background: #fffbeb;
		color: #92400e;
		border: 1px solid #fcd34d;
	}

	.ra-alert-info {
		background: color-mix(in srgb, var(--accent-primary, #e85d5d) 10%, white);
		color: #1e3a8a;
		border: 1px solid var(--border-soft, #ebe7e0);
	}

	.ra-alert-icon {
		font-size: 1rem;
		flex-shrink: 0;
	}

	.ra-alert-msg {
		flex: 1;
	}

	/* ===== Fund bar ===== */
	.ra-fund-bar {
		text-align: center;
	}

	.ra-fund-actions {
		display: flex;
		gap: 0.5rem;
		justify-content: center;
		flex-wrap: wrap;
		margin-top: 0.5rem;
	}

	/* ===== Button variant overrides (not in MUIJ) ===== */
	:global(.ra-btn-danger.muij-button) {
		background: var(--color-error, #e85d5d);
		color: #fff;
		border-color: var(--color-error, #e85d5d);
	}

	:global(.ra-btn-danger.muij-button:hover:not(:disabled)) {
		opacity: 0.9;
	}

	:global(.ra-btn-ghost.muij-button) {
		background: transparent;
		border: none;
		color: var(--text-muted, #8a847a);
	}

	:global(.ra-btn-ghost.muij-button:hover:not(:disabled)) {
		color: var(--color-error, #e85d5d);
	}

	/* ===== Modal form ===== */
	.ra-modal-form {
		display: flex;
		flex-direction: column;
		gap: 1rem;
	}

	.ra-field {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
	}

	.ra-field-label {
		font-size: 0.8rem;
		font-weight: 600;
		color: var(--text-secondary, #4a4540);
	}

	.ra-input {
		font-family: var(--font-primary);
		font-size: 0.8125rem;
		padding: 0.4rem 0.6rem;
		border: 1px solid var(--border-soft, #ebe7e0);
		border-radius: var(--radius-md, 8px);
		background: var(--bg-base, #fefdfb);
		color: var(--text-primary, #2d2a26);
		outline: none;
		transition: border-color 0.15s;
	}

	.ra-input:focus {
		border-color: var(--accent-primary, #e85d5d);
	}

	.ra-input-inline {
		width: 8rem;
		display: inline-block;
	}

	.ra-hint {
		font-size: 0.7rem;
		color: var(--text-muted, #8a847a);
	}

	.ra-fieldset {
		border: none;
		padding: 0;
		margin: 0;
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
	}

	.ra-radio {
		display: flex;
		align-items: center;
		gap: 0.4rem;
		font-size: 0.8125rem;
		color: var(--text-primary, #2d2a26);
		cursor: pointer;
	}

	.ra-modal-actions {
		display: flex;
		justify-content: flex-end;
		gap: 0.5rem;
		margin-top: 0.5rem;
	}

	/* ===== Info grid ===== */
	.ra-info-grid {
		display: grid;
		grid-template-columns: max-content 1fr;
		gap: 0.35rem 1rem;
		font-size: 0.8125rem;
		padding: 0.75rem;
		background: var(--bg-soft, #f3f0ea);
		border-radius: 8px;
	}

	.ra-info-label {
		font-weight: 600;
		color: var(--text-muted, #8a847a);
	}

	.ra-info-value {
		font-family: var(--font-mono);
		color: var(--text-primary, #2d2a26);
	}

	/* ===== Warning box ===== */
	.ra-warning-box {
		background: #fffbeb;
		border: 1px solid #fcd34d;
		border-radius: 8px;
		padding: 0.75rem 1rem;
		font-size: 0.8125rem;
		color: #92400e;
	}

	.ra-warning-box-critical {
		background: #fef2f2;
		border-color: #fca5a5;
		color: #991b1b;
	}

	.ra-warning-box ul {
		margin: 0.35rem 0;
		padding-left: 1.25rem;
	}

	.ra-warning-box li {
		margin-bottom: 0.15rem;
	}

	.ra-warning-box p {
		margin: 0.35rem 0;
	}

	/* ===== Journal preview ===== */
	.ra-journal-preview {
		background: var(--bg-soft, #f3f0ea);
		border-radius: 8px;
		padding: 0.75rem 1rem;
		font-size: 0.75rem;
	}

	.ra-preview-label {
		margin: 0 0 0.25rem;
		font-size: 0.75rem;
		color: var(--text-muted, #8a847a);
	}

	.ra-journal-line {
		font-family: var(--font-mono);
		font-size: 0.75rem;
		color: var(--text-primary, #2d2a26);
		padding: 0.1rem 0;
	}

	/* ===== Resolve ===== */
	.ra-resolve-prompt {
		font-size: 0.875rem;
		color: var(--text-secondary, #4a4540);
		margin: 0;
	}

	.ra-resolve-actions {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
	}

	/* ===== Responsive ===== */
	@media (max-width: 768px) {
		:global(.ra-hero.muij-card) {
			flex-direction: column;
			align-items: flex-start;
		}

		.ra-table {
			font-size: 0.75rem;
		}

		.ra-table th,
		.ra-table td {
			padding: 0.4rem 0.5rem;
		}

		.ra-fund-actions {
			flex-direction: column;
		}
	}
</style>
