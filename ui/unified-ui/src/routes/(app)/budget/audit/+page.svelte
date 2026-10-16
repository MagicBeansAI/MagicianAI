<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import Modal from '$lib/magician/components/generative/Modal.svelte';
	import Button from '$lib/magician/components/generative/Button.svelte';
	import Card from '$lib/magician/components/generative/Card.svelte';
	import Spinner from '$lib/magician/components/generative/Spinner.svelte';
	import Alert from '$lib/magician/components/generative/Alert.svelte';
	import { showError, showSuccess } from '$lib/shared/stores/notifications';
	import {
		loadAudit,
		loadBalances,
		loadPeriodCloses,
		loadReservations,
		closePeriod,
		commitReservation,
		rollbackReservation,
		flagReservation,
		isFrozen,
		resourceAuthorityStore,
		loadFreezeStatus,
		formatAmount,
		formatPeriod,
		type AuditResult,
		type AccountBalance,
		type PeriodCloseEntry,
		type CeilingPeriod,
		type Reservation
	} from '$lib/stores/resourceAuthorityStore';

	$: frozen = $isFrozen;

	let audit: AuditResult | null = null;
	let balances: AccountBalance[] = [];
	let periodCloses: PeriodCloseEntry[] = [];
	let reservations: Reservation[] = [];
	let loading = true;
	let error: string | null = null;

	let closeModalOpen = false;
	let closeCommodity = '';
	let closePeriodValue: CeilingPeriod = 'monthly';
	let closePeriodEnd = '';
	let closeBusy = false;

	const periodOptions: CeilingPeriod[] = ['monthly', 'weekly', 'daily', 'hourly', 'quarterly', 'annual', 'total'];

	let resolveModalOpen = false;
	let resolveTarget: Reservation | null = null;
	let resolveBusy = false;

	async function load() {
		loading = true;
		try {
			const [a, b, p, r] = await Promise.all([
				loadAudit(),
				loadBalances(),
				loadPeriodCloses(),
				loadReservationsLocal()
			]);
			audit = a;
			balances = b;
			periodCloses = p;
			error = null;
		} catch (err) {
			error = err instanceof Error ? err.message : String(err);
		} finally {
			loading = false;
		}
	}

	async function loadReservationsLocal(): Promise<void> {
		await loadReservations();
		reservations = $resourceAuthorityStore.reservations;
	}

	onMount(() => {
		load();
		loadFreezeStatus().catch(() => {});
	});

	$: staleRes = reservations.filter((r) => r.is_stale);

	// Group balances by commodity
	$: balancesByCommodity = balances.length > 0
		? Object.entries(
				balances.reduce<Record<string, AccountBalance[]>>((acc, b) => {
					(acc[b.commodity] = acc[b.commodity] || []).push(b);
					return acc;
				}, {})
			)
		: [];

	function formatDateFull(value: string): string {
		try {
			const d = new Date(value);
			if (isNaN(d.getTime())) return value;
			return d.toLocaleString(undefined, { year: 'numeric', month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' });
		} catch {
			return value;
		}
	}

	function formatTimestamp(value: string): string {
		try {
			const d = new Date(value);
			if (isNaN(d.getTime())) return value;
			return d.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit', second: '2-digit' });
		} catch {
			return value;
		}
	}

	async function handleClosePeriod() {
		closeBusy = true;
		try {
			const periodEnd = closePeriodEnd ? new Date(closePeriodEnd).toISOString() : new Date().toISOString();
			await closePeriod({ commodity: closeCommodity, period: closePeriodValue, period_end: periodEnd });
			showSuccess(`Period closed for ${closeCommodity}`);
			closeModalOpen = false;
			await load();
		} catch (err) {
			showError('Close period failed', err instanceof Error ? err.message : String(err));
		} finally {
			closeBusy = false;
		}
	}

	function openResolve(res: Reservation) {
		resolveTarget = res;
		resolveModalOpen = true;
	}

	async function handleCommit() {
		if (!resolveTarget) return;
		resolveBusy = true;
		try {
			await commitReservation(resolveTarget.id);
			showSuccess('Reservation committed');
			resolveModalOpen = false;
			resolveTarget = null;
			await load();
		} catch (err) {
			showError('Commit failed', err instanceof Error ? err.message : String(err));
		} finally {
			resolveBusy = false;
		}
	}

	async function handleRollback() {
		if (!resolveTarget) return;
		resolveBusy = true;
		try {
			await rollbackReservation(resolveTarget.id);
			showSuccess('Reservation rolled back');
			resolveModalOpen = false;
			resolveTarget = null;
			await load();
		} catch (err) {
			showError('Rollback failed', err instanceof Error ? err.message : String(err));
		} finally {
			resolveBusy = false;
		}
	}

	async function handleFlag() {
		if (!resolveTarget) return;
		resolveBusy = true;
		try {
			await flagReservation(resolveTarget.id);
			showSuccess('Reservation flagged for review');
			resolveModalOpen = false;
			resolveTarget = null;
			await load();
		} catch (err) {
			showError('Flag failed', err instanceof Error ? err.message : String(err));
		} finally {
			resolveBusy = false;
		}
	}
</script>

<svelte:head>
	<title>Audit · Magican</title>
</svelte:head>

<div class="presto-gaui-page ra-page">
	<!-- Frozen banner -->
	{#if frozen}
		<div class="ra-frozen-banner">
			<span class="ra-frozen-text">&#x1F534; SYSTEM FROZEN &mdash; All spending halted</span>
		</div>
	{/if}

	<!-- Header -->
	<Card className="ra-hero" elevation={1}>
		<div class="ra-hero-copy">
			<div class="ra-breadcrumb">
				<a href="/budget" class="ra-breadcrumb-link">Resource Authority</a>
				<span class="ra-breadcrumb-sep">/</span>
				<span>Audit</span>
			</div>
			<h1>Ledger Audit</h1>
		</div>
	</Card>

	{#if loading}
		<div class="ra-loading">
			<Spinner />
			<span>Running audit...</span>
		</div>
	{:else if error}
		<Alert type="error" message={error} closable />
	{:else if audit}
		<!-- Conservation Check -->
		<Card className="ra-section-card" elevation={0}>
			<div class="ra-conservation">
				<div class="ra-conservation-status" class:ra-pass={audit.ok} class:ra-fail={!audit.ok}>
					Conservation Check: {audit.ok ? 'PASSED' : 'FAILED'}
				</div>
				<div class="ra-conservation-meta">
					<span>{audit.message}</span>
				</div>
				{#if audit.imbalances && audit.imbalances.length > 0}
					<div class="ra-imbalances">
						<h3 class="ra-subsection-title">Imbalances Detected</h3>
						<div class="ra-table-wrap">
							<table class="ra-table">
								<thead>
									<tr>
										<th>Commodity</th>
										<th class="ra-col-right">Expected</th>
										<th class="ra-col-right">Actual</th>
									</tr>
								</thead>
								<tbody>
									{#each audit.imbalances as imb}
										<tr>
											<td>{imb.commodity}</td>
											<td class="ra-col-right">{formatAmount(imb.expected, imb.commodity)}</td>
											<td class="ra-col-right">{formatAmount(imb.actual, imb.commodity)}</td>
										</tr>
									{/each}
								</tbody>
							</table>
						</div>
					</div>
				{/if}
			</div>
		</Card>

		<!-- Account Balances -->
		{#if balancesByCommodity.length > 0}
		<Card className="ra-section-card" elevation={0}>
			<h2 class="ra-section-title">Account Balances</h2>
			<div class="ra-table-wrap">
				<table class="ra-table">
					<thead>
						<tr>
							<th>Account</th>
							<th class="ra-col-right">Balance</th>
						</tr>
					</thead>
					<tbody>
						{#each balancesByCommodity as [commodity, bals]}
							{#each bals as b}
								<tr>
									<td class="ra-cell-mono">{b.account}</td>
									<td class="ra-col-right" class:ra-amount-neg={b.balance < 0}>
										{formatAmount(b.balance, b.commodity)}
									</td>
								</tr>
							{/each}
							<!-- Sum row -->
							{@const sum = bals.reduce((s, b) => s + b.balance, 0)}
							<tr class="ra-sum-row">
								<td class="ra-cell-mono">SUM ({commodity})</td>
								<td class="ra-col-right">
									{formatAmount(sum, commodity)}
									{#if Math.abs(sum) < 0.005}
										<span class="ra-check-mark">&#x2713;</span>
									{:else}
										<span class="ra-check-fail">&#x2717;</span>
									{/if}
								</td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
		</Card>
		{/if}

		<!-- Pending Reconciliation -->
		{#if staleRes.length > 0}
			<Card className="ra-section-card" elevation={0}>
				<h2 class="ra-section-title">Pending Reconciliation</h2>
				<div class="ra-reconciliation-list">
					{#each staleRes as res (res.id)}
						<div class="ra-reconciliation-item">
							<div class="ra-reconciliation-info">
								<span class="ra-reconciliation-icon">&#x26A0;</span>
								Reservation {res.id} ({res.token_id}, {formatAmount(res.amount, res.commodity)}, {formatTimestamp(res.created_at)}) &mdash; stale
							</div>
							<div class="ra-reconciliation-actions">
								<Button label="Resolve" size="sm" on:click={() => openResolve(res)} />
							</div>
						</div>
					{/each}
				</div>
			</Card>
		{/if}

		<!-- Period Management -->
		<Card className="ra-section-card" elevation={0}>
			<h2 class="ra-section-title">Period Management</h2>

			{#if periodCloses.length > 0}
				<h3 class="ra-subsection-title">Closed Periods</h3>
				<div class="ra-table-wrap">
					<table class="ra-table">
						<thead>
							<tr>
								<th>Commodity</th>
								<th>Period End</th>
								<th>Closed At</th>
								<th>Closed By</th>
							</tr>
						</thead>
						<tbody>
							{#each periodCloses as pc (pc.commodity + pc.period_end)}
								<tr>
									<td>{pc.commodity}</td>
									<td class="ra-cell-time">{formatDateFull(pc.period_end)}</td>
									<td class="ra-cell-time">{formatDateFull(pc.closed_at)}</td>
									<td>{pc.closed_by}</td>
								</tr>
							{/each}
						</tbody>
					</table>
				</div>
			{/if}

			<div class="ra-period-action">
				<Button
					label="Close Period"
					on:click={() => { closeCommodity = ''; closePeriodValue = 'monthly'; closePeriodEnd = ''; closeModalOpen = true; }}
				/>
			</div>
		</Card>
	{/if}
</div>

<!-- Close Period Modal -->
<Modal open={closeModalOpen} title="Close Period" size="md" on:close={() => (closeModalOpen = false)}>
	<div class="ra-modal-form">
		<label class="ra-field">
			<span class="ra-field-label">Commodity</span>
			<input type="text" class="ra-input" bind:value={closeCommodity} placeholder="e.g. USD" />
		</label>
		<label class="ra-field">
			<span class="ra-field-label">Period</span>
			<select class="ra-input" bind:value={closePeriodValue}>
				{#each periodOptions as p}
					<option value={p}>{formatPeriod(p)}</option>
				{/each}
			</select>
		</label>
		<label class="ra-field">
			<span class="ra-field-label">Period End</span>
			<input type="datetime-local" class="ra-input" bind:value={closePeriodEnd} />
		</label>
		<div class="ra-warning-box">
			<p>After closing:</p>
			<ul>
				<li>No new entries with this period's accrual dates</li>
				<li>Post-close adjustments require justification</li>
				<li>Carryover computed from closing balances</li>
			</ul>
		</div>
		<div class="ra-modal-actions">
			<Button label="Cancel" variant="secondary" on:click={() => (closeModalOpen = false)} />
			<Button label={closeBusy ? 'Closing...' : 'Close Period'} disabled={closeBusy} on:click={handleClosePeriod} />
		</div>
	</div>
</Modal>

<!-- Resolve Stale Reservation Modal -->
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
				<Button label="Commit — action completed" disabled={resolveBusy} on:click={handleCommit} />
				<Button label="Rollback — action did not complete" variant="secondary" disabled={resolveBusy} on:click={handleRollback} />
				<Button label="Flag for manual review" variant="secondary" disabled={resolveBusy} on:click={handleFlag} />
			</div>
		</div>
	{/if}
</Modal>

<style>
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

	.ra-frozen-banner {
		background: linear-gradient(
			180deg,
			color-mix(in srgb, #fef2f2 76%, var(--bg-card, #fff)),
			color-mix(in srgb, #fef2f2 52%, var(--bg-soft, #f6f1e8))
		);
		border: 1px solid color-mix(in srgb, #b91c1c 24%, var(--border-soft, #d8d0c5));
		border-radius: var(--radius-md, 14px);
		padding: 0.85rem 1.1rem;
	}

	.ra-frozen-text {
		font-weight: 700;
		font-size: 0.875rem;
		color: #991b1b;
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

	.ra-breadcrumb {
		font-size: 0.78rem;
		font-weight: 700;
		letter-spacing: 0.12em;
		text-transform: uppercase;
		color: var(--text-muted, #8a847a);
		margin: 0 0 0.35rem;
		display: flex;
		align-items: center;
		gap: 0.35rem;
	}

	.ra-breadcrumb-link {
		color: var(--accent-primary, #e85d5d);
		text-decoration: none;
	}

	.ra-breadcrumb-link:hover { text-decoration: underline; }
	.ra-breadcrumb-sep { color: var(--text-faint, #c4beb4); }

	.ra-hero-copy h1 {
		margin: 0;
		font-family: var(--font-display, var(--font-primary));
		font-size: clamp(2rem, 4vw, 2.9rem);
		line-height: 0.98;
		letter-spacing: -0.04em;
		color: var(--text-primary, #2d2a26);
	}

	.ra-loading {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		padding: 2rem;
		justify-content: center;
		color: var(--text-muted, #8a847a);
	}

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

	.ra-section-title {
		font-size: 0.95rem;
		font-weight: 600;
		color: var(--text-secondary, #4a4540);
		margin: 0 0 0.75rem;
		text-transform: uppercase;
		letter-spacing: 0.04em;
	}

	.ra-subsection-title {
		font-size: 0.85rem;
		font-weight: 600;
		color: var(--text-muted, #8a847a);
		margin: 0.75rem 0 0.5rem;
	}

	/* Conservation */
	.ra-conservation {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
		align-items: center;
		padding: 0.75rem 0;
	}

	.ra-conservation-status {
		font-size: 1.1rem;
		font-weight: 700;
		padding: 0.5rem 1.5rem;
		border-radius: 8px;
	}

	.ra-pass {
		background: #ecfdf5;
		color: #065f46;
		border: 1px solid #6ee7b7;
	}

	.ra-fail {
		background: #fef2f2;
		color: #991b1b;
		border: 1px solid #fca5a5;
	}

	.ra-conservation-meta {
		display: flex;
		gap: 1.5rem;
		font-size: 0.8rem;
		color: var(--text-muted, #8a847a);
	}

	.ra-imbalances {
		width: 100%;
		margin-top: 0.75rem;
	}

	/* Table */
	.ra-table-wrap { overflow-x: auto; }

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
	}

	.ra-table tbody tr:last-child td { border-bottom: none; }

	.ra-col-right { text-align: right; }

	.ra-cell-mono { font-family: var(--font-mono); font-size: 0.75rem; }
	.ra-cell-time { white-space: nowrap; }
	.ra-amount-neg { color: var(--color-success, #5fa67a); }

	.ra-sum-row {
		background: var(--bg-soft, #f3f0ea);
		font-weight: 600;
	}

	.ra-check-mark {
		color: var(--color-success, #5fa67a);
		margin-left: 0.35rem;
		font-weight: 700;
	}

	.ra-check-fail {
		color: var(--color-error, #e85d5d);
		margin-left: 0.35rem;
		font-weight: 700;
	}

	/* Reconciliation */
	.ra-reconciliation-list {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
	}

	.ra-reconciliation-item {
		display: flex;
		align-items: center;
		justify-content: space-between;
		background: #fffbeb;
		border: 1px solid #fcd34d;
		border-radius: 8px;
		padding: 0.5rem 0.75rem;
		font-size: 0.8125rem;
		color: #92400e;
		flex-wrap: wrap;
		gap: 0.5rem;
	}

	.ra-reconciliation-info {
		display: flex;
		align-items: center;
		gap: 0.35rem;
	}

	.ra-reconciliation-icon {
		font-size: 1rem;
	}

	.ra-reconciliation-actions {
		display: flex;
		gap: 0.35rem;
	}

	/* Period */
	.ra-period-action {
		margin-top: 0.75rem;
		text-align: right;
	}

	/* Modal form */
	.ra-modal-form { display: flex; flex-direction: column; gap: 1rem; }
	.ra-field { display: flex; flex-direction: column; gap: 0.25rem; }
	.ra-field-label { font-size: 0.8rem; font-weight: 600; color: var(--text-secondary, #4a4540); }

	.ra-input {
		font-family: var(--font-primary);
		font-size: 0.8125rem;
		padding: 0.4rem 0.6rem;
		border: 1px solid var(--border-soft, #ebe7e0);
		border-radius: var(--radius-md, 8px);
		background: var(--bg-base, #fefdfb);
		color: var(--text-primary, #2d2a26);
		outline: none;
	}

	.ra-input:focus { border-color: var(--accent-primary, #e85d5d); }

	.ra-modal-actions { display: flex; justify-content: flex-end; gap: 0.5rem; margin-top: 0.5rem; }

	.ra-warning-box {
		background: #fffbeb;
		border: 1px solid #fcd34d;
		border-radius: 8px;
		padding: 0.75rem 1rem;
		font-size: 0.8125rem;
		color: #92400e;
	}
	.ra-warning-box ul { margin: 0.35rem 0; padding-left: 1.25rem; }
	.ra-warning-box li { margin-bottom: 0.15rem; }
	.ra-warning-box p { margin: 0.35rem 0; }

	.ra-info-grid {
		display: grid;
		grid-template-columns: max-content 1fr;
		gap: 0.35rem 1rem;
		font-size: 0.8125rem;
		padding: 0.75rem;
		background: var(--bg-soft, #f3f0ea);
		border-radius: 8px;
	}

	.ra-info-label { font-weight: 600; color: var(--text-muted, #8a847a); }
	.ra-info-value { font-family: var(--font-mono); color: var(--text-primary, #2d2a26); }

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
</style>
