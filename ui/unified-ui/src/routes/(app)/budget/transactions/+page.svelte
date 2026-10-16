<script lang="ts">
	import { page } from '$app/stores';
	import { onMount, onDestroy } from 'svelte';
	import Spinner from '$lib/magician/components/generative/Spinner.svelte';
	import Alert from '$lib/magician/components/generative/Alert.svelte';
	import Button from '$lib/magician/components/generative/Button.svelte';
	import Card from '$lib/magician/components/generative/Card.svelte';
	import { showError } from '$lib/shared/stores/notifications';
	import {
		loadTransactions,
		isFrozen,
		resourceAuthorityStore,
		loadFreezeStatus,
		formatAmount,
		type FlatTransaction,
		type TransactionFilter
	} from '$lib/stores/resourceAuthorityStore';

	const REFRESH_MS = 15_000;
	let refreshHandle: ReturnType<typeof setInterval> | null = null;

	$: frozen = $isFrozen;

	let transactions: FlatTransaction[] = [];
	let loading = true;
	let error: string | null = null;

	// Filter state
	let filterCommodity = '';
	let filterAgentId = '';
	let filterTokenId = $page.url.searchParams.get('token_id') ?? '';
	let filterSince = '';

	function buildFilter(): TransactionFilter {
		const f: TransactionFilter = {};
		if (filterCommodity.trim()) f.commodity = filterCommodity.trim();
		if (filterAgentId.trim()) f.agent_id = filterAgentId.trim();
		if (filterTokenId.trim()) f.token_id = filterTokenId.trim();
		if (filterSince) f.since = new Date(filterSince).toISOString();
		return f;
	}

	async function load() {
		loading = true;
		try {
			transactions = await loadTransactions(buildFilter());
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

	function handleFilter() {
		load();
	}

	function clearFilters() {
		filterCommodity = '';
		filterAgentId = '';
		filterTokenId = '';
		filterSince = '';
		load();
	}

	function formatTimestamp(value: string): string {
		try {
			const d = new Date(value);
			if (isNaN(d.getTime())) return value;
			return d.toLocaleString(undefined, {
				month: 'short', day: 'numeric',
				hour: '2-digit', minute: '2-digit', second: '2-digit'
			});
		} catch {
			return value;
		}
	}
</script>

<svelte:head>
	<title>Transactions · Magican</title>
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
				<span>Transactions</span>
			</div>
			<h1>Transactions</h1>
		</div>
	</Card>

	<!-- Filters -->
	<Card className="ra-section-card ra-filter-section" elevation={0}>
		<div class="ra-filter-row">
			<label class="ra-filter-field">
				<span class="ra-filter-label">Commodity</span>
				<input type="text" class="ra-input" bind:value={filterCommodity} placeholder="e.g. USD" />
			</label>
			<label class="ra-filter-field">
				<span class="ra-filter-label">Agent</span>
				<input type="text" class="ra-input" bind:value={filterAgentId} placeholder="Agent ID" />
			</label>
			<label class="ra-filter-field">
				<span class="ra-filter-label">Token</span>
				<input type="text" class="ra-input" bind:value={filterTokenId} placeholder="Token ID" />
			</label>
			<label class="ra-filter-field">
				<span class="ra-filter-label">Since</span>
				<input type="date" class="ra-input" bind:value={filterSince} />
			</label>
		</div>
		<div class="ra-filter-actions">
			<Button label="Apply" size="sm" on:click={handleFilter} />
			<Button label="Clear" variant="secondary" size="sm" on:click={clearFilters} />
		</div>
	</Card>

	<!-- Results -->
	{#if loading && transactions.length === 0}
		<div class="ra-loading">
			<Spinner />
			<span>Loading transactions...</span>
		</div>
	{:else if error}
		<Alert type="error" message={error} closable />
	{:else if transactions.length === 0}
		<div class="ra-empty">No transactions found</div>
	{:else}
		<Card className="ra-section-card" elevation={0}>
			<div class="ra-table-wrap">
				<table class="ra-table">
					<thead>
						<tr>
							<th>Accrual</th>
							<th>Agent</th>
							<th>Account</th>
							<th>Amount</th>
							<th>Commodity</th>
							<th>Reference</th>
						</tr>
					</thead>
					<tbody>
						{#each transactions as txn (txn.id + txn.account)}
							<tr>
								<td class="ra-cell-mono ra-cell-time">{formatTimestamp(txn.accrual_date)}</td>
								<td>{txn.agent_id}</td>
								<td class="ra-cell-mono">{txn.account}</td>
								<td class:ra-amount-neg={txn.amount < 0}>{formatAmount(txn.amount, txn.commodity)}</td>
								<td>{txn.commodity}</td>
								<td class="ra-cell-ref">{txn.reference}</td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
			<div class="ra-count">{transactions.length} transaction(s)</div>
		</Card>
	{/if}
</div>

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

	.ra-empty {
		padding: 2rem;
		text-align: center;
		color: var(--text-muted, #8a847a);
		font-size: 0.875rem;
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

	/* Filters */
	:global(.ra-filter-section.muij-card) {
		margin-bottom: 1rem;
	}

	.ra-filter-row {
		display: flex;
		gap: 0.75rem;
		flex-wrap: wrap;
		margin-bottom: 0.5rem;
	}

	.ra-filter-field {
		display: flex;
		flex-direction: column;
		gap: 0.2rem;
		flex: 1;
		min-width: 120px;
	}

	.ra-filter-label {
		font-size: 0.7rem;
		font-weight: 600;
		color: var(--text-muted, #8a847a);
		text-transform: uppercase;
		letter-spacing: 0.03em;
	}

	.ra-filter-actions {
		display: flex;
		gap: 0.5rem;
	}

	.ra-input {
		font-family: var(--font-primary);
		font-size: 0.8125rem;
		padding: 0.35rem 0.5rem;
		border: 1px solid var(--border-soft, #ebe7e0);
		border-radius: var(--radius-md, 8px);
		background: var(--bg-base, #fefdfb);
		color: var(--text-primary, #2d2a26);
		outline: none;
	}

	.ra-input:focus { border-color: var(--accent-primary, #e85d5d); }

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
		vertical-align: top;
	}

	.ra-table tbody tr:last-child td { border-bottom: none; }

	.ra-cell-mono { font-family: var(--font-mono); font-size: 0.75rem; }
	.ra-cell-time { white-space: nowrap; }
	.ra-cell-ref { max-width: 160px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
	.ra-amount-neg { color: var(--color-success, #5fa67a); }

	.ra-count {
		text-align: right;
		font-size: 0.75rem;
		color: var(--text-muted, #8a847a);
		margin-top: 0.5rem;
	}

	@media (max-width: 768px) {
		.ra-filter-row { flex-direction: column; }
	}
</style>
