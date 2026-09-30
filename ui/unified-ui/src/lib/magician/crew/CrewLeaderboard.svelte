<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import { healthBand } from './health';
	import type { CrewLeaderboardRow } from './leaderboard';
	import { relativeTime, VIBE_LABEL } from '$lib/magician/square/derive';

	export let rows: CrewLeaderboardRow[] = [];
	export let presentation: 'page' | 'overlay' = 'page';
	export let showHeader = true;
	export let emptyMessage = 'No crew members are available.';

	const dispatch = createEventDispatcher<{ select: string }>();

	function formatDelta(value: number): string {
		return `${value > 0 ? '+' : ''}${value.toFixed(1)}`;
	}
</script>

<section
	class="crew-leaderboard"
	class:crew-leaderboard--overlay={presentation === 'overlay'}
	aria-label="Crew leaderboard"
>
	{#if showHeader}
		<header class="crew-leaderboard__head">
			<div>
				<h2>Crew leaderboard</h2>
				<p>Ranked by activity over the last seven days</p>
			</div>
			<span>{rows.length} crew member{rows.length === 1 ? '' : 's'}</span>
		</header>
	{/if}

	{#if rows.length === 0}
		<div class="crew-leaderboard__empty">{emptyMessage}</div>
	{:else}
		<div class="crew-leaderboard__scroller">
			<table>
				<thead>
					<tr>
						<th class="rank">Rank</th>
						<th>Crew member</th>
						<th>Program</th>
						<th>Status</th>
						<th class="number">Overall</th>
						<th class="number">Health 7d</th>
						<th class="number">Spend 7d</th>
						<th class="number">Calls 7d</th>
						<th class="number">Success</th>
						<th class="last-active">Last active</th>
						<th class="action"><span class="sr-only">Open</span></th>
					</tr>
				</thead>
				<tbody>
					{#each rows as row (row.id)}
						<tr class:needs-attention={row.vibe === 'needs'}>
							<td class="rank"><strong>{row.rank}</strong></td>
							<td>
								<div class="identity">
									<strong>{row.name}</strong>
									{#if row.isPrimary}<span>Primary</span>{:else if row.isEnvoy}<span>Envoy</span>{/if}
								</div>
							</td>
							<td class="program">{row.program}</td>
							<td>
								<span class="status"><i data-vibe={row.vibe}></i>{VIBE_LABEL[row.vibe]}</span>
							</td>
							<td class="number">
								{#if row.health != null}<strong data-band={healthBand(row.health)}>{row.health}</strong>{:else}-{/if}
							</td>
							<td class="number">
								{#if row.health7d != null}
									{row.health7d.toFixed(1)}
									{#if row.healthDelta7d != null}
										<small data-delta={row.healthDelta7d > 0 ? 'up' : row.healthDelta7d < 0 ? 'down' : 'flat'}>{formatDelta(row.healthDelta7d)}</small>
									{/if}
								{:else}-{/if}
							</td>
							<td class="number">{row.spendUsd7d != null ? `$${row.spendUsd7d.toFixed(2)}` : '-'}</td>
							<td class="number">{row.calls7d != null ? row.calls7d : '-'}</td>
							<td class="number">{row.successRate7d != null ? `${Math.round(row.successRate7d * 100)}%` : '-'}</td>
							<td class="last-active">{relativeTime(row.lastActiveAt)}</td>
							<td class="action">
								<button type="button" title={`Open ${row.name}`} aria-label={`Open ${row.name}`} on:click={() => dispatch('select', row.id)}>
									<Icon name="chevron-right" size={16} />
								</button>
							</td>
						</tr>
					{/each}
				</tbody>
			</table>
		</div>
	{/if}
</section>

<style>
	.crew-leaderboard {
		min-width: 0;
		padding: 1rem;
		border: 1px solid var(--border-subtle);
		border-radius: 0.5rem;
		background: var(--bg-card);
		color: var(--text-primary);
	}
	.crew-leaderboard--overlay {
		padding: 0;
		border: 0;
		background: transparent;
		color: var(--game-text);
	}
	.crew-leaderboard__head {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 1rem;
		margin-bottom: 0.8rem;
	}
	.crew-leaderboard__head h2 { margin: 0; font-size: 1.05rem; letter-spacing: 0; }
	.crew-leaderboard__head p { margin: 0.2rem 0 0; color: var(--text-secondary); font-size: 0.78rem; }
	.crew-leaderboard__head > span { color: var(--text-secondary); font-size: 0.75rem; white-space: nowrap; }
	.crew-leaderboard__scroller { max-width: 100%; overflow-x: auto; }
	table { width: 100%; min-width: 68rem; border-collapse: collapse; table-layout: fixed; }
	th, td { padding: 0.58rem 0.5rem; border-bottom: 1px solid var(--border-subtle); text-align: left; vertical-align: middle; }
	th { color: var(--text-secondary); font-size: 0.68rem; font-weight: 700; text-transform: uppercase; }
	td { min-width: 0; font-size: 0.82rem; }
	tbody tr:last-child td { border-bottom: 0; }
	tbody tr:hover { background: var(--accent-primary-soft); }
	tbody tr.needs-attention { background: var(--color-warning-soft); }
	.rank { width: 3.2rem; text-align: center; }
	.rank strong { font-variant-numeric: tabular-nums; }
	.program { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
	.identity { display: flex; min-width: 0; align-items: center; gap: 0.45rem; }
	.identity > strong { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
	.identity > span { flex: none; padding: 0.12rem 0.32rem; border: 1px solid var(--border-subtle); border-radius: 0.25rem; color: var(--text-secondary); font-size: 0.62rem; }
	.status { display: inline-flex; align-items: center; gap: 0.4rem; white-space: nowrap; }
	.status i { width: 0.48rem; height: 0.48rem; border-radius: 50%; background: var(--fleet-idle, #0078a4); }
	.status i[data-vibe='working'] { background: var(--fleet-working, #20773d); }
	.status i[data-vibe='needs'] { background: var(--fleet-needs, #ae6500); }
	.status i[data-vibe='paused'] { background: var(--fleet-paused, #8b5cf6); }
	.status i[data-vibe='offline'] { background: var(--fleet-offline, #4f5860); }
	.number { width: 5rem; text-align: right; font-variant-numeric: tabular-nums; }
	.number strong[data-band='good'], .number small[data-delta='up'] { color: var(--color-success); }
	.number strong[data-band='fair'] { color: var(--color-warning); }
	.number strong[data-band='poor'], .number small[data-delta='down'] { color: var(--color-error); }
	.number small { display: block; color: var(--text-secondary); font-size: 0.62rem; }
	.last-active { width: 6rem; text-align: right; white-space: nowrap; }
	.action { width: 2.6rem; text-align: right; }
	.action button { display: inline-grid; width: 2rem; height: 2rem; place-items: center; border: 0; border-radius: 0.25rem; background: transparent; color: currentColor; cursor: pointer; }
	.action button:hover, .action button:focus-visible { background: var(--accent-primary-soft); outline: 2px solid var(--accent-primary); outline-offset: 1px; }
	.crew-leaderboard__empty { display: grid; min-height: 10rem; place-items: center; color: var(--text-secondary); }
	.crew-leaderboard--overlay th,
	.crew-leaderboard--overlay td { border-color: var(--game-border); }
	.crew-leaderboard--overlay table { min-width: 54rem; }
	.crew-leaderboard--overlay th,
	.crew-leaderboard--overlay td { padding-right: 0.35rem; padding-left: 0.35rem; }
	.crew-leaderboard--overlay .rank { width: 2.8rem; }
	.crew-leaderboard--overlay .number { width: 4.4rem; }
	.crew-leaderboard--overlay .last-active { width: 5.4rem; }
	.crew-leaderboard--overlay .action { width: 2.4rem; }
	.crew-leaderboard--overlay th,
	.crew-leaderboard--overlay .number small { color: var(--game-text-muted); }
	.crew-leaderboard--overlay tbody tr:hover { background: var(--game-material-selected); }
	.crew-leaderboard--overlay tbody tr.needs-attention { background: color-mix(in srgb, var(--game-state-attention) 12%, transparent); }
	.crew-leaderboard--overlay .identity > span { border-color: var(--game-border); color: var(--game-text-muted); }
	.crew-leaderboard--overlay .action button:hover,
	.crew-leaderboard--overlay .action button:focus-visible { background: var(--game-material-selected); outline-color: var(--game-focus-color); }
	.crew-leaderboard--overlay .crew-leaderboard__empty { color: var(--game-text-muted); }
	.sr-only { position: absolute; width: 1px; height: 1px; padding: 0; overflow: hidden; clip: rect(0, 0, 0, 0); white-space: nowrap; border: 0; }
</style>
