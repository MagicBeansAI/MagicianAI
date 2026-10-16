<script lang="ts">
	import { onDestroy, onMount } from 'svelte';
	import { createLiveRewirer, setupLiveDataSource } from '$lib/magician/dashboard/useLiveDataSource';
	import { decisionModelSql, modelCost } from '$lib/llm/decisionModels';
	import { parseNumericDuckDbValue } from '$lib/magician/components/generative/chartUtil';
	export let where = 'TRUE';
	export let rangeLabel = '7d';
	let mounted = false;
	let loading = true;
	let error: string | null = null;
	let rows: Record<string, any>[] = [];
	$: source = { kind: 'llm_calls_sql' as const, sql: decisionModelSql(where) };
	const rewirer = createLiveRewirer((dataSource) => setupLiveDataSource({
		dataSource,
		onStart: () => { loading = true; },
		onRows: ({ records }) => {
			rows = records.map(row => Object.fromEntries(Object.entries(row).map(([key, value]) =>
				[key, key === 'provider' || key === 'model' ? value : parseNumericDuckDbValue(value) ?? null])));
			error = null;
		},
		onError: (e) => { error = e.message; },
		onSettled: () => { loading = false; }
	}));
	onMount(() => { mounted = true; });
	$: if (mounted) rewirer.sync(source);
	onDestroy(() => rewirer.destroy());
	function number(value: unknown): string { return value == null ? 'Not reported' : Number(value).toLocaleString(undefined, { maximumFractionDigits: 1 }); }
	function bucket(value: unknown, reported: unknown, calls: unknown): string {
		if (!Number(reported)) return 'Not reported';
		return number(value) + (Number(reported) < Number(calls) ? ` (${reported}/${calls} calls)` : '');
	}
</script>

<section id="decision-models" aria-label="Decision Models" aria-busy={loading}>
	<h2>Decision Models <small>({rangeLabel})</small></h2>
	<p>Jev, Laya, Kev and other decision models. Included in the totals above. Each retry and review is counted once; local inference has no API charge.</p>
	{#if error}<p role="alert">Decision Model usage unavailable: {error}</p>
	{:else if loading && !rows.length}<p>Loading Decision Model usage…</p>
	{:else if !rows.length}<p>No Decision Model calls in this window.</p>
	{:else}
	<div class="table-scroll">
		<table>
			<thead><tr><th>Model / provider</th><th>Calls / succeeded</th><th>Cost</th><th>Input / output</th><th>Cache read / write</th><th>Avg / p95</th></tr></thead>
			<tbody>{#each rows as row (`${row.provider}:${row.model}`)}
				<tr>
					<td>{row.model}<small>{String(row.provider).replace(/^decision:/, '')}</small></td>
					<td>{number(row.calls)} / {number(row.succeeded)}</td>
					<td>{modelCost(row.cost_usd)}{#if Number(row.priced_calls) < Number(row.calls)}<small>{Number(row.calls) - Number(row.priced_calls)} unpriced</small>{/if}</td>
					<td>{bucket(row.input_tokens, row.usage_calls, row.calls)} / {bucket(row.output_tokens, row.usage_calls, row.calls)}</td>
					<td>{bucket(row.cache_read_tokens, row.cache_read_calls, row.calls)} / {bucket(row.cache_creation_tokens, row.cache_write_calls, row.calls)}</td>
					<td>{number(row.latency_ms)} / {number(row.p95_ms)} ms</td>
				</tr>
			{/each}</tbody>
		</table>
	</div>
	{/if}
	<p class="note">Costs use the rate effective at call time. Jev currently reports no cache counters; unavailable measurements are never shown as zero hits.</p>
</section>

<style>
	section { margin: 1.5rem 0; padding: 1rem; border: 1px solid var(--border-color, #8884); border-radius: .75rem; }
	h2 { margin: 0 0 .5rem; font-size: 1.1rem; }
	p { opacity: .75; font-size: .85rem; }
	.table-scroll { overflow-x: auto; }
	table { width: 100%; border-collapse: collapse; font-size: .85rem; }
	th, td { text-align: left; padding: .65rem .5rem; border-bottom: 1px solid var(--border-color, #8883); vertical-align: top; }
	td { font-variant-numeric: tabular-nums; }
	small { display: block; opacity: .65; }
	h2 small { display: inline; }
	.note { margin-bottom: 0; font-size: .75rem; }
</style>
