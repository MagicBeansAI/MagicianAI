<script lang="ts">
	import { rise } from './motion';
	import { latestRows } from './stageRows';
	import { tintVars } from './tint';

	type StageStatus = 'running' | 'done' | 'failed' | 'waiting';

	interface StageRow {
		key: string;
		label: string;
		status: StageStatus;
		tool?: string;
		target?: string;
	}

	type Glyph = 'web' | 'search' | 'file' | 'code' | 'generic';

	export let rows: StageRow[] = [];

	let expanded = false;
	// The expanded list shows the latest rows until the reader asks for all.
	let showAll = false;
	$: if (!expanded) showAll = false;

	const STATUS_TEXT: Record<StageStatus, string> = {
		running: 'running',
		done: 'done',
		failed: 'failed',
		waiting: 'waiting'
	};

	function glyphFor(tool: string | undefined): Glyph {
		const name = (tool ?? '').toLowerCase();
		if (!name) return 'generic';
		if (/search|query|lookup/.test(name)) return 'search';
		if (/web|browser|fetch|url|http|page/.test(name)) return 'web';
		if (/file|read|write|edit|path|fs|dir/.test(name)) return 'file';
		if (/code|bash|shell|exec|python|script|run/.test(name)) return 'code';
		return 'generic';
	}

	// The current step is the LAST running row: rows arrive latest-last.
	$: current = [...rows].reverse().find((row) => row.status === 'running') ?? null;
	$: settled = rows.filter((row) => row.status !== 'running');
	// Only the latest settled steps get a dot; the rest fold into `+N`.
	$: dotWindow = latestRows(settled);
	$: listWindow = showAll ? { shown: rows, hidden: 0 } : latestRows(rows);
	$: glyph = glyphFor(current?.tool);
	// Nothing running is not "finishing" when a step is waiting on the reader:
	// saying so would tell them the turn is wrapping up while it waits on them.
	$: waiting = current === null && rows.some((row) => row.status === 'waiting');
	$: idleLabel = waiting ? 'Waiting on your input' : 'Finishing…';
	$: announcement = current ? current.label : rows.length > 0 ? idleLabel : '';
</script>

<div class="cn-stage" data-testid="turn-stage">
	<p class="cn-stage__sr" aria-live="polite">{announcement}</p>

	{#if current}
		{#key current.key}
			<div
				class="cn-stage__chip cn-stage__chip--working"
				data-testid="turn-stage-chip"
				style={tintVars(current.tool ?? current.label)}
				in:rise={{ y: 6 }}
			>
				<span class="cn-stage__glyph" data-glyph={glyph} aria-hidden="true">
					<svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">
						{#if glyph === 'web'}
							<circle cx="8" cy="8" r="6" /><path d="M2 8h12M8 2c2 2 2 10 0 12M8 2c-2 2-2 10 0 12" />
						{:else if glyph === 'search'}
							<circle cx="7" cy="7" r="4.5" /><path d="M10.5 10.5 14 14" />
						{:else if glyph === 'file'}
							<path d="M4 1.5h5l3 3v10H4z" /><path d="M9 1.5v3h3" />
						{:else if glyph === 'code'}
							<path d="M5.5 4 2 8l3.5 4M10.5 4 14 8l-3.5 4" />
						{:else}
							<circle cx="8" cy="8" r="2.5" /><path d="M8 1.5v2M8 12.5v2M1.5 8h2M12.5 8h2" />
						{/if}
					</svg>
				</span>
				<span class="cn-stage__label">{current.label}</span>
				{#if current.target}
					<span class="cn-stage__target">{current.target}</span>
				{/if}
			</div>
		{/key}
	{:else if rows.length > 0}
		<div
			class="cn-stage__chip cn-stage__chip--finishing"
			class:cn-stage__chip--waiting={waiting}
			data-testid="turn-stage-chip"
			in:rise={{ y: 6 }}
		>
			<span class="cn-stage__label">{idleLabel}</span>
		</div>
	{/if}

	{#if rows.length > 0}
		<div class="cn-stage__trail">
			{#if settled.length > 0}
				<span class="cn-stage__dots">
					{#if dotWindow.hidden > 0}
						<span
							class="cn-stage__more"
							data-testid="turn-stage-more"
							role="img"
							title="{dotWindow.hidden} earlier step{dotWindow.hidden === 1 ? '' : 's'}"
							aria-label="{dotWindow.hidden} earlier step{dotWindow.hidden === 1 ? '' : 's'}"
							>+{dotWindow.hidden}</span
						>
					{/if}
					{#each dotWindow.shown as row (row.key)}
						<span
							class="cn-stage__dot"
							data-status={row.status}
							style={tintVars(row.tool ?? row.label)}
							role="img"
							title="{row.label} ({STATUS_TEXT[row.status]})"
							aria-label="{row.label} ({STATUS_TEXT[row.status]})"
						></span>
					{/each}
				</span>
			{/if}
			<button
				type="button"
				class="cn-stage__toggle"
				aria-expanded={expanded}
				on:click={() => (expanded = !expanded)}
			>
				{expanded ? 'Hide' : 'Show'} {rows.length} step{rows.length === 1 ? '' : 's'}
			</button>
		</div>

		{#if expanded}
			<ul class="cn-stage__list">
				{#each listWindow.shown as row (row.key)}
					<li class="cn-stage__item" data-status={row.status}>
						<span class="cn-stage__dot" data-status={row.status} style={tintVars(row.tool ?? row.label)} aria-hidden="true"></span>
						<span class="cn-stage__item-label">{row.label}</span>
						{#if row.target}
							<span class="cn-stage__target">{row.target}</span>
						{/if}
						<span class="cn-stage__status">{STATUS_TEXT[row.status]}</span>
					</li>
				{/each}
			</ul>
			{#if listWindow.hidden > 0 || showAll}
				<button
					type="button"
					class="cn-stage__toggle cn-stage__toggle--all"
					aria-expanded={showAll}
					on:click={() => (showAll = !showAll)}
				>
					{showAll ? 'Show latest only' : `Show all ${rows.length} steps`}
				</button>
			{/if}
		{/if}
	{/if}
</div>

<style>
	.cn-stage {
		display: grid;
		gap: 0.45rem;
		min-width: 0;
	}

	.cn-stage__sr {
		position: absolute;
		width: 1px;
		height: 1px;
		margin: -1px;
		padding: 0;
		overflow: hidden;
		clip: rect(0 0 0 0);
		white-space: nowrap;
		border: 0;
	}

	.cn-stage__chip {
		display: inline-flex;
		align-items: center;
		gap: 0.4rem;
		justify-self: start;
		max-width: 100%;
		min-width: 0;
		padding: 0.3rem 0.75rem 0.3rem 0.4rem;
		border-radius: var(--radius-chip);
		font-size: var(--text-sm);
	}

	.cn-stage__chip--working {
		background: color-mix(in srgb, var(--cn-tint) 55%, var(--bg-card));
		color: var(--cn-tint-ink);
		animation: cn-stage-pulse calc(var(--dur-slow) * 3) ease-in-out infinite;
	}

	/* A status label, not a pill: a rounded square tag at the small type size. */
	.cn-stage__chip--finishing {
		padding: 0.15rem 0.5rem;
		border: 1px solid var(--border-default);
		border-radius: var(--radius-chip);
		background: var(--surface-glass);
		color: var(--text-secondary);
		font-size: var(--text-xs);
		font-weight: 600;
		letter-spacing: 0.02em;
	}

	.cn-stage__chip--waiting {
		border-color: color-mix(in srgb, var(--color-warning) 55%, var(--border-default));
		color: var(--text-primary);
	}

	@keyframes cn-stage-pulse {
		0%,
		100% {
			box-shadow: 0 0 0 0 color-mix(in srgb, var(--cn-tint) 70%, transparent);
		}
		50% {
			box-shadow: 0 0 0 5px color-mix(in srgb, var(--cn-tint) 0%, transparent);
		}
	}

	.cn-stage__glyph {
		display: inline-grid;
		place-items: center;
		flex: none;
		width: 1.35rem;
		height: 1.35rem;
		border-radius: 50%;
		background: var(--cn-tint);
		color: var(--cn-tint-ink);
	}

	.cn-stage__label,
	.cn-stage__item-label {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		min-width: 0;
	}

	.cn-stage__target {
		flex: none;
		max-width: 12rem;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		color: var(--text-muted);
		font-size: var(--text-xs);
	}

	.cn-stage__trail {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.5rem;
	}

	.cn-stage__dots {
		display: inline-flex;
		align-items: center;
		flex-wrap: wrap;
		gap: 0.25rem;
	}

	.cn-stage__dot {
		display: inline-block;
		flex: none;
		width: 0.5rem;
		height: 0.5rem;
		border-radius: 50%;
		background: var(--cn-tint);
		transition: transform var(--dur-quick) var(--spring-snappy);
	}

	.cn-stage__dot[data-status='failed'] {
		background: var(--color-error);
	}

	.cn-stage__dot[data-status='waiting'] {
		background: var(--color-warning);
	}

	.cn-stage__more {
		color: var(--text-muted);
		font-size: var(--text-xs);
		font-variant-numeric: tabular-nums;
		line-height: 1;
	}

	.cn-stage__toggle--all {
		justify-self: start;
	}

	.cn-stage__dots .cn-stage__dot:hover {
		transform: scale(1.4);
	}

	.cn-stage__toggle {
		padding: 0.05rem 0.4rem;
		border: 0;
		border-radius: var(--radius-chip);
		background: transparent;
		color: var(--text-muted);
		font-size: var(--text-xs);
		cursor: pointer;
	}

	.cn-stage__toggle:hover {
		color: var(--text-primary);
	}

	.cn-stage__toggle:focus-visible {
		outline: none;
		box-shadow: var(--ring-focus);
	}

	.cn-stage__list {
		display: grid;
		gap: 0.25rem;
		margin: 0;
		padding: 0;
		list-style: none;
		font-size: var(--text-xs);
	}

	.cn-stage__item {
		display: flex;
		align-items: center;
		gap: 0.4rem;
		min-width: 0;
		color: var(--text-primary);
	}

	.cn-stage__status {
		flex: none;
		margin-inline-start: auto;
		color: var(--text-muted);
	}

	.cn-stage__item[data-status='failed'] .cn-stage__status {
		color: var(--color-error);
	}

	.cn-stage__item[data-status='waiting'] .cn-stage__status {
		color: var(--color-warning);
	}
</style>
