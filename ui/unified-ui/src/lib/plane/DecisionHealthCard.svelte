<script lang="ts">
	import { onDestroy, onMount } from 'svelte';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import { fetchDecisionHealth, formatMs, formatWindow, HEALTH_WINDOWS, overallStatus, sortByHealth, type DecisionHealthReport, type DecisionHealthStatus } from './decisionHealth';

	export let hours = 24;
	/** When set (the decision-models page), operation names filter in place. */
	export let onSelect: ((operation: string) => void) | null = null;
	let report: DecisionHealthReport | null = null;
	let error = '';
	let loading = false;
	let alive = true;
	let generation = 0;
	let controller: AbortController | undefined;

	$: rows = sortByHealth(report?.operations ?? []);
	$: overall = overallStatus(report?.operations ?? []);
	const label: Record<DecisionHealthStatus, string> = { failing: 'Failing', degraded: 'Degraded', healthy: 'Healthy', idle: 'Idle' };
	const name = (operation: string) => operation.replaceAll('_', ' ');
	const when = (ms: number | null) => (ms ? new Date(ms).toLocaleString() : '—');

	/** Fetch once on open and on explicit refresh; no background polling. */
	export async function refresh(): Promise<void> {
		const id = ++generation;
		controller?.abort();
		controller = new AbortController();
		loading = true;
		error = '';
		try {
			const next = await fetchDecisionHealth(hours, controller.signal);
			if (alive && id === generation) report = next;
		} catch (e) {
			if (alive && id === generation && !(e instanceof DOMException && e.name === 'AbortError')) {
				error = e instanceof Error ? e.message : String(e);
				report = null;
			}
		} finally {
			if (alive && id === generation) loading = false;
		}
	}

	onMount(() => {
		let key = '';
		return scopeIdentityStore.subscribe((scope) => {
			const next = `${scope.principal}:${scope.workspace}`;
			if (next !== key) {
				key = next;
				report = null;
				refresh();
			}
		});
	});
	onDestroy(() => {
		alive = false;
		generation++;
		controller?.abort();
	});
</script>

<section class="decision-health" aria-labelledby="decision-health-title" aria-busy={loading}>
	<header>
		<div>
			<h2 id="decision-health-title">
				Decision model health
				{#if report}<span class="pill status-{overall}">{label[overall]}</span>{/if}
			</h2>
			<p class="muted">Per operation: how many decisions the engine answered, left below threshold, timed out or errored. Degraded above 10% or when p95 exceeds the caller's budget; failing above 50%.</p>
		</div>
		<div class="controls">
			<label>Window
				<select bind:value={hours} onchange={() => refresh()}>
					{#each HEALTH_WINDOWS as h}<option value={h}>Last {formatWindow(h)}</option>{/each}
				</select>
			</label>
			<button type="button" onclick={() => refresh()} disabled={loading}>{loading ? 'Refreshing…' : 'Refresh'}</button>
		</div>
	</header>

	{#if error}<p role="alert">Decision model health unavailable: {error}</p>{/if}
	{#if loading && !report}<p role="status">Loading decision model health…</p>{/if}
	{#if report}
		{#if report.truncated || report.capture_dropped || report.write_errors || report.unreadable_cases}
			<p role="alert">Evidence is incomplete: {report.capture_dropped} capture drops, {report.write_errors} write errors{report.unreadable_cases ? `, ${report.unreadable_cases} unreadable cases` : ''}{report.truncated ? ', scan limit reached (older cases in the window are missing)' : ''}.</p>
		{/if}
		{#if !rows.length}
			<p>No decisions recorded in the last {formatWindow(report.window_hours)}.</p>
		{:else}
			<div class="table-wrap">
				<table>
					<thead>
						<tr><th>Operation</th><th>Status</th><th>Cases</th><th>Decided</th><th>Unresolved</th><th>Timeout</th><th>Error</th><th>p50 / p95</th><th>Last problem</th></tr>
					</thead>
					<tbody>
						{#each rows as { op, verdict } (op.operation)}
							<tr class="row-{verdict.status}">
								<th scope="row">
									{#if onSelect}<button type="button" class="link" onclick={() => onSelect?.(op.operation)}>{name(op.operation)}</button>{:else}<a href={`/decision-models?operation=${encodeURIComponent(op.operation)}`}>{name(op.operation)}</a>{/if}
									<small>{op.mode ?? '—'}{op.models.length ? ` · ${op.models.map((m) => m.model).join(', ')}` : ''}</small>
								</th>
								<td>
									<span class="status status-{verdict.status}"><span class="dot" aria-hidden="true"></span>{label[verdict.status]}</span>
									{#if verdict.reasons.length}<small>{verdict.reasons.join(' · ')}</small>{/if}
								</td>
								<td>{op.cases}{#if op.skipped}<small>{op.skipped} skipped</small>{/if}</td>
								<td>{op.decided}</td>
								<td>{op.unresolved}</td>
								<td>{op.timeout}</td>
								<td>{op.error}</td>
								<td>{formatMs(op.p50_ms)} / {formatMs(op.p95_ms)}{#if op.budget_ms}<small>budget {formatMs(op.budget_ms)}</small>{/if}</td>
								<td>
									{#if op.last_error_ms}{when(op.last_error_ms)}<small class="cause">{op.last_error}</small>{:else}—{/if}
									{#if Object.keys(op.causes).length}
										<details>
											<summary>Causes</summary>
											<ul>{#each Object.entries(op.causes).sort((a, b) => b[1] - a[1]) as [cause, n]}<li><code>{cause}</code> × {n}</li>{/each}</ul>
											{#if op.models.length}<p>Answered by model:</p><ul>{#each op.models as m}<li>{m.model}: {m.answered} · {formatMs(m.p50_ms)} / {formatMs(m.p95_ms)}</li>{/each}</ul>{/if}
											{#if op.route_health.length}<p>Engine route health (last seen {when(op.route_health[0].observed_ms)}):</p><ul>{#each op.route_health as r}<li>{r.model}: {r.issue ?? 'ok'}</li>{/each}</ul>{/if}
										</details>
									{/if}
								</td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
			<p class="muted">Last {formatWindow(report.window_hours)} · {report.cases_scanned} cases · generated {when(report.generated_at_ms)}. Skipped decisions never reached the engine (disabled, policy unavailable, admission) and are not counted as cases. Older records cannot tell a timeout from other engine failures and show <code>no_engine_reply</code>.</p>
		{/if}
	{/if}
</section>

<style>
	.decision-health { margin: 1rem 0 1.5rem; padding: 1rem; border: 1px solid var(--border-color, #8884); border-radius: .75rem; }
	header { display: flex; flex-wrap: wrap; gap: 1rem; justify-content: space-between; align-items: flex-start; }
	h2 { margin: 0; font-size: 1.15rem; display: flex; align-items: center; gap: .5rem; flex-wrap: wrap; }
	p { margin: .5rem 0; font-size: .85rem; overflow-wrap: anywhere; }
	.muted, small { opacity: .75; }
	small { display: block; font-size: .75rem; overflow-wrap: anywhere; }
	.controls { display: flex; gap: .5rem; align-items: flex-end; flex-wrap: wrap; }
	label { display: flex; flex-direction: column; gap: .3rem; font-size: .8rem; }
	select, button { padding: .45rem .6rem; border: 1px solid #8886; border-radius: .4rem; background: var(--bg-secondary, transparent); color: inherit; max-width: 100%; }
	button:disabled { opacity: .5; }
	.table-wrap { overflow-x: auto; margin-top: .75rem; }
	table { width: 100%; border-collapse: collapse; font-size: .85rem; }
	th, td { padding: .55rem .5rem; text-align: left; border-bottom: 1px solid var(--border-color, #8883); vertical-align: top; }
	td { font-variant-numeric: tabular-nums; }
	thead th { font-weight: 600; white-space: nowrap; }
	tbody th a, .link { text-decoration: underline; text-transform: capitalize; }
	.link { padding: 0; border: 0; background: none; font: inherit; font-weight: 600; cursor: pointer; text-align: left; }
	.status { display: inline-flex; align-items: center; gap: .35rem; font-weight: 600; white-space: nowrap; }
	.dot { width: .6rem; height: .6rem; border-radius: 50%; background: currentColor; }
	.pill { font-size: .75rem; padding: .1rem .5rem; border-radius: 999px; border: 1px solid currentColor; font-weight: 600; }
	.status-healthy { color: var(--color-success, #1a7f37); }
	.status-degraded { color: var(--color-warning, #b26a00); }
	.status-failing { color: var(--color-danger, #c62828); }
	.status-idle { color: var(--text-secondary, #888); }
	.row-failing { background: color-mix(in srgb, var(--color-danger, #c62828) 7%, transparent); }
	.row-degraded { background: color-mix(in srgb, var(--color-warning, #b26a00) 6%, transparent); }
	.cause { font-family: ui-monospace, monospace; }
	details { margin-top: .3rem; font-size: .8rem; }
	details ul { margin: .25rem 0; padding-left: 1.1rem; }
	code { font-size: .78rem; overflow-wrap: anywhere; }
</style>
