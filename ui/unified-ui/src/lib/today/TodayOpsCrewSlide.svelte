<script lang="ts">
	/**
	 * TodayOpsCrewSlide.svelte
	 *
	 * Slide 3 of the Today Operations carousel ("State of the Crew"): crew
	 * totals over the last 24 hours, then one compact row per agent (cost,
	 * calls, tasks, success, reliability) that scrolls inside the fixed slide
	 * height. Data is joined by `buildCrewRows` (crewQueries.ts); this
	 * component only presents it. Fail-soft: an error line with Retry, never
	 * substitute numbers.
	 */
	import Icon from '$lib/shared/icons/Icon.svelte';
	import { crewTotals, formatPct, pctTone, type CrewRow } from '$lib/today/crewQueries';
	import { formatPerCall } from '$lib/today/opsCarousel';

	interface Props {
		/** null until the first load settles. */
		rows: CrewRow[] | null;
		totalAgents: number;
		error?: string | null;
		onRetry?: () => void;
	}

	let { rows, totalAgents, error = null, onRetry }: Props = $props();

	const totals = $derived(rows ? crewTotals(rows, totalAgents) : null);

	function money(value: number): string {
		return value > 0 ? formatPerCall(value) : '$0.00';
	}

	function plural(n: number, one: string, many: string): string {
		return `${n.toLocaleString()} ${n === 1 ? one : many}`;
	}

	function rowLabel(row: CrewRow): string {
		return [
			row.name,
			row.state === 'active' ? 'active' : row.state === 'off' ? 'disabled' : 'idle',
			`${money(row.costUsd)} in 24 hours`,
			plural(row.calls, 'model call', 'model calls'),
			plural(row.done, 'task done', 'tasks done'),
			`success ${formatPct(row.successPct)}`,
			`reliability ${formatPct(row.reliabilityPct)}`
		].join(', ');
	}
</script>

<div class="np-ops-crew">
	<div class="np-ops-crew__top">
		{#if totals}
			<dl class="np-ops-crew__totals" aria-label="Crew totals, last 24 hours">
				<div class="np-ops-crew__total">
					<dt>Active</dt>
					<dd>{totals.activeNow}/{totals.totalAgents}</dd>
				</div>
				<div class="np-ops-crew__total">
					<dt>Cost 24h</dt>
					<dd>{money(totals.costUsd)}</dd>
				</div>
				<div class="np-ops-crew__total">
					<dt>Tasks 24h</dt>
					<dd>{totals.tasksDone}</dd>
				</div>
				<div class="np-ops-crew__total">
					<dt>Reliability</dt>
					<dd class="is-{pctTone(totals.reliabilityPct)}">{formatPct(totals.reliabilityPct)}</dd>
				</div>
			</dl>
		{:else}
			<span></span>
		{/if}
		<a href="/crew" class="np-panel__link" title="Open agent crew">
			<span>Agent Crew</span>
			<Icon name="arrow-right" size={11} />
		</a>
	</div>

	{#if error}
		<p class="np-ops-crew__error" role="status">
			<span>{rows ? 'Crew activity may be out of date.' : "Couldn't load crew activity."}</span>
			{#if onRetry}
				<button type="button" class="np-ops-crew__retry" onclick={() => onRetry?.()}>Retry</button>
			{/if}
		</p>
	{/if}

	{#if rows === null}
		{#if !error}
			<p class="np-ops-crew__note">Loading crew activity…</p>
		{/if}
	{:else if rows.length === 0}
		<p class="np-ops-crew__note">The crew is resting — no activity in the last 24 hours.</p>
	{:else}
		<div class="np-ops-crew__head" aria-hidden="true">
			<span>Name</span>
			<span>Cost</span>
			<span class="np-ops-crew__calls">Calls</span>
			<span>Tasks</span>
			<span>Succ.</span>
			<span>Rel.</span>
		</div>
		<ul class="np-ops-crew__rows" aria-label="Crew activity, last 24 hours">
			{#each rows as row (row.agentId)}
				<li>
					<a
						class="np-ops-crew__row"
						href={`/crew/${encodeURIComponent(row.agentId)}`}
						aria-label={rowLabel(row)}
						title={rowLabel(row)}
					>
						<span class="np-ops-crew__name">
							<span class="np-ops-crew__dot is-{row.state}" aria-hidden="true"></span>
							<span class="np-ops-crew__name-text">{row.name}</span>
							{#if row.state === 'active'}<span class="np-ops-crew__active">Active</span>{/if}
						</span>
						<span class="np-ops-crew__num">{money(row.costUsd)}</span>
						<span class="np-ops-crew__num np-ops-crew__calls">{plural(row.calls, 'call', 'calls')}</span>
						<span class="np-ops-crew__num">{plural(row.done, 'task', 'tasks')}</span>
						<span class="np-ops-crew__num is-{pctTone(row.successPct)}">✓ {formatPct(row.successPct)}</span>
						<span class="np-ops-crew__num is-{pctTone(row.reliabilityPct)}">⚡ {formatPct(row.reliabilityPct)}</span>
					</a>
				</li>
			{/each}
		</ul>
	{/if}
</div>

<style>
	.np-ops-crew {
		--ops-task-row: 1.5rem;
		--crew-cols: minmax(0, 1fr) 4.2rem 4.6rem 3.8rem 3.6rem 3.6rem;
		display: flex;
		flex-direction: column;
		gap: 0.3rem;
		height: 100%;
		min-width: 0;
	}

	@media (max-width: 560px) {
		.np-ops-crew {
			--crew-cols: minmax(0, 1fr) 3.8rem 3.4rem 3.4rem 3.4rem;
		}

		.np-ops-crew__calls {
			display: none;
		}
	}

	.np-ops-crew__top {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
		min-width: 0;
	}

	.np-ops-crew__totals {
		display: flex;
		flex-wrap: wrap;
		align-items: baseline;
		gap: 0.2rem 1.1rem;
		margin: 0;
		min-width: 0;
	}

	.np-ops-crew__total {
		display: flex;
		align-items: baseline;
		gap: 0.35rem;
	}

	.np-ops-crew__total dt {
		order: 2;
		font-family: ui-monospace, SFMono-Regular, monospace;
		font-size: 0.58rem;
		letter-spacing: 0.1em;
		text-transform: uppercase;
		color: var(--text-muted, #64748b);
	}

	.np-ops-crew__total dd {
		margin: 0;
		font-family: 'Newsreader', Georgia, serif;
		font-size: 1.05rem;
		font-weight: 700;
		line-height: 1.1;
		color: var(--text-primary, #1e293b);
	}

	.np-panel__link {
		display: inline-flex;
		align-items: center;
		gap: 0.25rem;
		flex-shrink: 0;
		color: var(--text-muted, #64748b);
		text-decoration: none;
		font-family: 'Cinzel', Georgia, serif;
		font-size: 0.65rem;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		transition: color 0.15s ease;
	}

	.np-panel__link:hover {
		color: var(--accent-primary, #6366f1);
		text-decoration: underline;
	}

	.np-ops-crew__note,
	.np-ops-crew__error {
		margin: 0;
		font-family: 'Newsreader', Georgia, serif;
		font-style: italic;
		font-size: 0.82rem;
		color: var(--text-muted, #64748b);
	}

	.np-ops-crew__error {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		color: var(--status-error, #dc2626);
	}

	.np-ops-crew__retry {
		font: inherit;
		font-style: normal;
		font-size: 0.72rem;
		padding: 0.05rem 0.45rem;
		border: 1px solid var(--border-color, rgba(128, 128, 128, 0.35));
		border-radius: 3px;
		background: transparent;
		color: var(--text-primary, #1e293b);
		cursor: pointer;
	}

	.np-ops-crew__head,
	.np-ops-crew__row {
		display: grid;
		grid-template-columns: var(--crew-cols);
		align-items: center;
		column-gap: 0.5rem;
	}

	.np-ops-crew__head {
		padding: 0 0.25rem;
		font-family: ui-monospace, SFMono-Regular, monospace;
		font-size: 0.56rem;
		letter-spacing: 0.1em;
		text-transform: uppercase;
		color: var(--text-muted, #64748b);
	}

	.np-ops-crew__head span:not(:first-child) {
		text-align: right;
	}

	.np-ops-crew__rows {
		list-style: none;
		margin: 0;
		padding: 0;
		flex: 0 1 auto;
		min-height: 0;
		max-height: calc(3.5 * var(--ops-task-row));
		overflow-y: auto;
		overscroll-behavior: contain;
	}

	.np-ops-crew__row {
		box-sizing: border-box;
		height: var(--ops-task-row);
		padding: 0 0.25rem;
		border-bottom: 1px solid color-mix(in srgb, var(--border-color, rgba(128, 128, 128, 0.2)) 60%, transparent);
		color: var(--text-primary, #1e293b);
		text-decoration: none;
		font-size: 0.8rem;
		border-radius: 2px;
	}

	.np-ops-crew__row:hover {
		background: color-mix(in srgb, var(--text-primary) 4%, transparent);
	}

	.np-ops-crew__row:focus-visible {
		outline: 2px solid var(--accent-primary, #6366f1);
		outline-offset: -2px;
	}

	.np-ops-crew__name {
		display: flex;
		align-items: center;
		gap: 0.45rem;
		min-width: 0;
	}

	.np-ops-crew__name-text {
		min-width: 0;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
		font-family: 'Newsreader', Georgia, serif;
	}

	.np-ops-crew__active {
		flex-shrink: 0;
		font-family: ui-monospace, SFMono-Regular, monospace;
		font-size: 0.56rem;
		letter-spacing: 0.1em;
		text-transform: uppercase;
		color: var(--status-success, #16a34a);
	}

	.np-ops-crew__dot {
		width: 6px;
		height: 6px;
		border-radius: 50%;
		flex-shrink: 0;
		background: var(--text-muted, #94a3b8);
	}

	.np-ops-crew__dot.is-active {
		background: var(--status-success, #16a34a);
	}

	.np-ops-crew__dot.is-off {
		background: transparent;
		box-shadow: inset 0 0 0 1px var(--border-color, rgba(128, 128, 128, 0.45));
	}

	.np-ops-crew__num {
		text-align: right;
		white-space: nowrap;
		font-family: ui-monospace, SFMono-Regular, monospace;
		font-size: 0.68rem;
		color: var(--text-primary, #1e293b);
	}

	.is-success {
		color: var(--status-success, #16a34a);
	}

	.is-warning {
		color: var(--status-warning, #d97706);
	}

	.is-danger {
		color: var(--status-error, #dc2626);
	}

	.np-ops-crew__num.is-muted {
		color: var(--text-muted, #64748b);
	}
</style>
