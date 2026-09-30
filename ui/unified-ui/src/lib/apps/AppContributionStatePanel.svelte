<script lang="ts">
	import { onDestroy, onMount } from 'svelte';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import { fetchAppContributionState, type AppContributionStateSnapshot } from './appContributionState';

	let snapshot: AppContributionStateSnapshot | null = null;
	let loading = false;
	let error = '';
	let request: AbortController | null = null;
	let mounted = false;
	let appliedScope = '';
	$: scopeKey = JSON.stringify([$scopeIdentityStore.principal, $scopeIdentityStore.workspace]);
	$: if (mounted && scopeKey !== appliedScope) { appliedScope = scopeKey; void refresh(scopeKey); }

	async function refresh(expectedScope = scopeKey): Promise<void> {
		request?.abort();
		const controller = new AbortController();
		request = controller;
		loading = true;
		error = '';
		try {
			const next = await fetchAppContributionState(controller.signal);
			if (!controller.signal.aborted && expectedScope === scopeKey) snapshot = next;
		} catch (cause) {
			if (!controller.signal.aborted && expectedScope === scopeKey) {
				snapshot = null;
				error = cause instanceof Error ? cause.message : 'Contribution state is unavailable.';
			}
		} finally {
			if (request === controller) { request = null; loading = false; }
		}
	}

	onMount(() => { mounted = true; appliedScope = scopeKey; void refresh(scopeKey); });
	onDestroy(() => { mounted = false; request?.abort(); });
</script>

<section class="contribution-state" aria-labelledby="app-contribution-state-title" aria-busy={loading}>
	<header><div><p class="eyebrow">Source-linked ingress</p><h2 id="app-contribution-state-title">Memory and retrieval</h2></div><button type="button" disabled={loading} on:click={() => void refresh()}>{loading ? 'Loading…' : 'Refresh'}</button></header>
	<p class="description">Inspect destination-owned state and retention. Only the code-verified desktop Settings owner can sign memory accept, reject, or revoke receipts.</p>
	{#if error}<p class="error" role="alert">{error}</p>{/if}
	{#if snapshot}
		<p class="heads">Memory head {snapshot.memoryGeneration} · retrieval {snapshot.retrievalAvailable ? `head ${snapshot.retrievalGeneration ?? 0}` : 'unavailable'}</p>
		{#if snapshot.items.length === 0}<p>No contribution state is retained for this scope.</p>{/if}
		<div class="rows">
			{#each snapshot.items as item (`${item.destination}:${item.proposalDigest}`)}
				<article>
					<div class="row-title"><strong>{item.installationId}</strong><span>{item.destination} · {item.state}</span></div>
					{#if item.claimOrSummary}<p>{item.claimOrSummary}</p>{/if}
					<dl><dt>Reason</dt><dd>{item.reason.replaceAll('_', ' ')}</dd><dt>Source</dt><dd><code>{item.sourceEventRef}@{item.sourceEventRevision}</code></dd><dt>Retention</dt><dd>{item.retainedUntilMs ? new Date(item.retainedUntilMs).toLocaleString() : item.expiresAtMs ? new Date(item.expiresAtMs).toLocaleString() : 'compacted'}</dd>{#if item.targetAgentId}<dt>Target</dt><dd>{item.targetAgentId}{item.targetGoalId ? ` / ${item.targetGoalId}` : ''}</dd>{/if}</dl>
				</article>
			{/each}
		</div>
	{/if}
</section>

<style>
	.contribution-state { margin: 18px 0; padding: 18px; border: 1px solid var(--border-soft); border-radius: 18px; background: var(--bg-elevated); }
	header, .row-title { display: flex; align-items: center; justify-content: space-between; gap: 12px; }
	h2, p { margin: 0; } h2 { font-family: var(--font-display, var(--font-primary)); font-size: 1.02rem; }
	.eyebrow { color: var(--accent-primary); font-size: .72rem; font-weight: 800; letter-spacing: .1em; text-transform: uppercase; }
	.description, .heads { margin-top: 8px; color: var(--text-secondary); font-size: .8rem; line-height: 1.45; } .heads { color: var(--text-muted); }
	.rows { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(100%, 310px), 1fr)); gap: 9px; margin-top: 12px; }
	article { min-width: 0; padding: 10px; border: 1px solid var(--border-soft); border-radius: 12px; background: var(--bg-soft); }
	.row-title span { color: var(--text-muted); font-size: .72rem; text-transform: capitalize; } article > p { margin-top: 7px; font-size: .78rem; }
	dl { display: grid; grid-template-columns: auto minmax(0, 1fr); gap: 4px 8px; margin: 8px 0 0; font-size: .72rem; } dt { font-weight: 700; color: var(--text-secondary); } dd { margin: 0; overflow-wrap: anywhere; color: var(--text-secondary); }
	.error { margin-top: 8px; color: var(--color-error, #c43f3f); }
	button { border: 1px solid var(--border-soft); border-radius: 999px; background: var(--bg-elevated); color: var(--text-primary); padding: 7px 10px; font: inherit; cursor: pointer; } button:disabled { opacity: .55; }
</style>
