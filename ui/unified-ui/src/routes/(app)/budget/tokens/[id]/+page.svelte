<script lang="ts">
	import { page } from '$app/stores';
	import { goto } from '$app/navigation';
	import { onMount, onDestroy } from 'svelte';
	import Modal from '$lib/magician/components/generative/Modal.svelte';
	import Button from '$lib/magician/components/generative/Button.svelte';
	import Card from '$lib/magician/components/generative/Card.svelte';
	import Gauge from '$lib/magician/components/generative/Gauge.svelte';
	import Spinner from '$lib/magician/components/generative/Spinner.svelte';
	import Alert from '$lib/magician/components/generative/Alert.svelte';
	import { showError, showSuccess } from '$lib/shared/stores/notifications';
	import {
		loadTokenDetail,
		revokeToken,
		isFrozen,
		resourceAuthorityStore,
		loadFreezeStatus,
		formatAmount,
		formatPeriod,
		usagePercent,
		usageColor,
		type TokenDetailResponse,
		type FlatTransaction
	} from '$lib/stores/resourceAuthorityStore';

	const REFRESH_MS = 15_000;
	let refreshHandle: ReturnType<typeof setInterval> | null = null;

	$: tokenId = decodeURIComponent($page.params.id || '');
	$: frozen = $isFrozen;

	let detail: TokenDetailResponse | null = null;
	let loading = true;
	let error: string | null = null;

	let revokeModalOpen = false;
	let revokeBusy = false;

	async function load() {
		if (!tokenId) return;
		try {
			detail = await loadTokenDetail(tokenId);
			error = null;
		} catch (err) {
			error = err instanceof Error ? err.message : String(err);
		} finally {
			loading = false;
		}
	}

	onMount(() => {
		load();
		loadFreezeStatus().catch(() => {});
		refreshHandle = setInterval(() => { load(); }, REFRESH_MS);
	});

	onDestroy(() => {
		if (refreshHandle) clearInterval(refreshHandle);
	});

	async function handleRevoke() {
		if (!detail) return;
		revokeBusy = true;
		try {
			await revokeToken(detail.token.id);
			showSuccess(`Token ${detail.token.id} revoked`);
			revokeModalOpen = false;
			goto('/budget');
		} catch (err) {
			showError('Revoke failed', err instanceof Error ? err.message : String(err));
		} finally {
			revokeBusy = false;
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

	function formatDate(value: string | undefined): string {
		if (!value) return 'n/a';
		try {
			const d = new Date(value);
			if (isNaN(d.getTime())) return value;
			return d.toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' });
		} catch {
			return value;
		}
	}

	/** Flatten spend_history journal entries into flat transaction rows. */
	function flattenSpendHistory(detail: TokenDetailResponse): FlatTransaction[] {
		return detail.spend_history.flatMap((je) =>
			je.entries.map((le) => ({
				id: je.id,
				timestamp: je.timestamp,
				accrual_date: je.accrual_date,
				reference: je.reference,
				agent_id: je.agent_id,
				account: le.account,
				amount: le.amount,
				commodity: le.commodity,
				metadata: je.metadata
			}))
		);
	}

	$: token = detail?.token ?? null;
	$: pct = token ? usagePercent(token.spent_in_period, token.ceiling) : 0;
	$: recentTxns = detail ? flattenSpendHistory(detail).slice(0, 10) : [];
</script>

<svelte:head>
	<title>Token · Magican</title>
</svelte:head>

<div class="presto-gaui-page ra-page">
	<!-- Frozen banner -->
	{#if frozen}
		<div class="ra-frozen-banner">
			<span class="ra-frozen-text">&#x1F534; SYSTEM FROZEN &mdash; All spending halted</span>
		</div>
	{/if}

	{#if loading}
		<div class="ra-loading">
			<Spinner />
			<span>Loading token detail...</span>
		</div>
	{:else if error}
		<Alert type="error" message={error} closable />
	{:else if detail && token}
		<!-- Header -->
		<Card className="ra-hero" elevation={1}>
			<div class="ra-hero-copy">
				<div class="ra-breadcrumb">
					<a href="/budget" class="ra-breadcrumb-link">Resource Authority</a>
					<span class="ra-breadcrumb-sep">/</span>
					<span>Token</span>
				</div>
				<h1>{token.id}</h1>
			</div>
			<div class="ra-hero-actions">
				<Button
					label="Revoke"
					variant="primary"
					className="ra-btn-danger"
					disabled={token.status !== 'active'}
					on:click={() => (revokeModalOpen = true)}
				/>
			</div>
		</Card>

		<!-- Info table -->
		<Card className="ra-section-card" elevation={0}>
			<div class="ra-info-grid">
				<span class="ra-info-label">Issued By</span><span class="ra-info-value">{token.issued_by}</span>
				<span class="ra-info-label">Issued To</span><span class="ra-info-value">{token.issued_to}</span>
				<span class="ra-info-label">Commodity</span><span class="ra-info-value">{token.commodity}</span>
				<span class="ra-info-label">Ceiling</span><span class="ra-info-value">{formatAmount(token.ceiling, token.commodity)} / {formatPeriod(token.period)}</span>
				{#if token.velocity_limit}
					<span class="ra-info-label">Velocity Limit</span><span class="ra-info-value">{formatAmount(token.velocity_limit.max_amount, token.commodity)} / {token.velocity_limit.window_seconds}s</span>
				{/if}
				<span class="ra-info-label">Status</span>
				<span class="ra-info-value">
					{#if token.status === 'active'}
						<span class="ra-status-dot ra-dot-active"></span> Active
					{:else if token.status === 'revoked'}
						<span class="ra-status-dot ra-dot-revoked"></span> Revoked
					{:else}
						<span class="ra-status-dot ra-dot-expired"></span> Expired
					{/if}
				</span>
				<span class="ra-info-label">Expires</span><span class="ra-info-value">{formatDate(token.expires_at)}</span>
				{#if token.conditions.length > 0}
					<span class="ra-info-label">Conditions</span><span class="ra-info-value">{token.conditions.join(', ')}</span>
				{/if}
			</div>
		</Card>

		<!-- Budget gauge -->
		<div class="ra-gauge-row">
			<Card className="ra-section-card ra-gauge-section" elevation={0}>
				<h2 class="ra-section-title">Period Budget</h2>
				<div class="ra-gauge-content">
					<div class="ra-usage-bar-wide">
						<div class="ra-usage-bar-track">
							<div
								class="ra-usage-bar-fill"
								style="width: {pct}%; background: {usageColor(pct)};"
							></div>
						</div>
						<div class="ra-usage-label">
							{formatAmount(token.spent_in_period, token.commodity)} / {formatAmount(token.ceiling, token.commodity)} ({Math.round(pct)}%)
						</div>
					</div>
					{#if token.days_of_runway != null}
						<div class="ra-sub">Projected exhaustion: ~{token.days_of_runway.toFixed(1)} days at current rate</div>
					{/if}
					<div class="ra-sub">
						Burn rate: {formatAmount(token.burn_rate_per_day, token.commodity)}/day
					</div>
					{#if token.projected_exhaustion}
						<div class="ra-sub">Projected exhaustion date: {formatDate(token.projected_exhaustion)}</div>
					{/if}
				</div>
			</Card>
		</div>

		<!-- Transactions (spend_history) -->
		<Card className="ra-section-card" elevation={0}>
			<div class="ra-section-header">
				<h2 class="ra-section-title">Spend History</h2>
				<Button label="View All" variant="secondary" size="sm" on:click={() => goto(`/budget/transactions?token_id=${encodeURIComponent(token?.id ?? '')}`)} />
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
	{/if}
</div>

<!-- Revoke Modal -->
<Modal open={revokeModalOpen} title="Revoke Token" size="md" on:close={() => (revokeModalOpen = false)}>
	{#if token}
		<div class="ra-modal-form">
			<div class="ra-info-grid">
				<span class="ra-info-label">Token</span><span class="ra-info-value">{token.id}</span>
				<span class="ra-info-label">Issued To</span><span class="ra-info-value">{token.issued_to}</span>
				<span class="ra-info-label">Commodity</span><span class="ra-info-value">{token.commodity}</span>
				<span class="ra-info-label">Remaining</span><span class="ra-info-value">{formatAmount(token.remaining_in_period, token.commodity)}</span>
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
				<Button label={revokeBusy ? 'Revoking...' : 'Revoke Token'} variant="primary" className="ra-btn-danger" disabled={revokeBusy} on:click={handleRevoke} />
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

	/* Frozen banner */
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

	/* Hero card */
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
		font-size: clamp(1.4rem, 3vw, 2.2rem);
		line-height: 0.98;
		letter-spacing: -0.04em;
		color: var(--text-primary, #2d2a26);
		word-break: break-all;
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

	/* Section cards */
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

	/* Info grid */
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

	/* Status dots */
	.ra-status-dot {
		display: inline-block;
		width: 8px;
		height: 8px;
		border-radius: 50%;
		margin-right: 0.25rem;
	}

	.ra-dot-active { background: var(--color-success, #5fa67a); }
	.ra-dot-revoked { background: var(--color-error, #e85d5d); }
	.ra-dot-expired { background: var(--text-muted, #8a847a); }

	/* Gauge row */
	.ra-gauge-row {
		display: grid;
		grid-template-columns: 1fr 1fr;
		gap: 1.25rem;
	}

	@media (max-width: 768px) {
		.ra-gauge-row { grid-template-columns: 1fr; }
	}


	.ra-gauge-content {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 0.5rem;
		padding: 0.5rem 0;
	}

	/* Usage bar wide */
	.ra-usage-bar-wide {
		width: 100%;
	}

	.ra-usage-bar-track {
		height: 12px;
		background: var(--bg-soft, #f3f0ea);
		border-radius: 999px;
		overflow: hidden;
		margin-bottom: 0.35rem;
	}

	.ra-usage-bar-fill {
		height: 100%;
		border-radius: 999px;
		transition: width 0.3s ease;
	}

	.ra-usage-label {
		font-size: 0.8rem;
		color: var(--text-secondary, #4a4540);
		text-align: center;
	}

	.ra-sub {
		font-size: 0.75rem;
		color: var(--text-muted, #8a847a);
		text-align: center;
	}
	/* Table */
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

	.ra-table tbody tr:last-child td { border-bottom: none; }

	.ra-cell-mono {
		font-family: var(--font-mono);
		font-size: 0.75rem;
	}

	.ra-cell-time { white-space: nowrap; }
	.ra-cell-ref { max-width: 160px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }

	.ra-amount-neg { color: var(--color-success, #5fa67a); }

	/* Button variant overrides */
	:global(.ra-btn-danger.muij-button) {
		background: var(--color-error, #e85d5d);
		color: #fff;
		border-color: var(--color-error, #e85d5d);
	}

	:global(.ra-btn-danger.muij-button:hover:not(:disabled)) {
		opacity: 0.9;
	}

	/* Gauge section */
	:global(.ra-gauge-section.muij-card) {
		display: flex;
		flex-direction: column;
	}

	/* Modal form */
	.ra-modal-form { display: flex; flex-direction: column; gap: 1rem; }
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
</style>
