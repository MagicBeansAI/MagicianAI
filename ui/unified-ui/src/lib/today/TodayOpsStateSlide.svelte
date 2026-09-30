<script lang="ts">
	/**
	 * TodayOpsStateSlide.svelte
	 *
	 * Slide 2 of the Today Operations carousel ("State of Operations"):
	 * Active / Succeeded / Failed task buckets as a solid pie + legend, agent
	 * Enabled · Active · Total, Tasks / Agent Crew links, and the 20 most
	 * recently updated tasks as one-line rows that open the task.
	 * The task list scrolls inside the carousel's fixed slide height.
	 */
	import Icon from '$lib/shared/icons/Icon.svelte';
	import type { Task } from '$lib/stores/taskStore';
	import type { AgentSummary } from '$lib/stores/agentStore';
	import { bucketTasks, recentTasks, shortRelativeTime, taskDotTone } from '$lib/today/opsCarousel';

	interface Props {
		tasks: Task[];
		agents: AgentSummary[];
		/** Live running-agent count from the agent store (0 when unknown). */
		runningAgentCount?: number;
		/** Pulse completed-today count; Succeeded never reads below it. */
		pulseCompletedToday?: number;
		nowMs: number;
	}

	let { tasks, agents, runningAgentCount = 0, pulseCompletedToday = 0, nowMs }: Props = $props();

	const buckets = $derived(bucketTasks(tasks, pulseCompletedToday));
	const total = $derived(buckets.total);
	const succeededPct = $derived(total > 0 ? Math.round((buckets.succeeded / total) * 100) : 0);
	const failedPct = $derived(total > 0 ? Math.round((buckets.failed / total) * 100) : 0);
	const activePct = $derived(total > 0 ? Math.max(0, 100 - succeededPct - failedPct) : 0);

	const totalAgents = $derived(agents.length);
	const enabledAgents = $derived(agents.filter((a) => !a.disabled && a.status !== 'disabled').length);
	const runningAgents = $derived(
		runningAgentCount || agents.filter((a) => a.status === 'running' || a.status === 'triggered').length
	);

	const rows = $derived(recentTasks(tasks));

	// --- Solid SVG pie (centre 50,50, radius 40) ---
	const PIE_R = 40;
	const PIE_CX = 50;
	const PIE_CY = 50;

	interface PieSlice {
		key: string;
		label: string;
		count: number;
		pct: number;
		color: string;
		path: string;
		isFullCircle: boolean;
	}

	const pieSlices = $derived.by((): PieSlice[] => {
		if (total <= 0) return [];
		const categories = [
			{ key: 'succeeded', label: 'Succeeded', count: buckets.succeeded, pct: succeededPct, color: 'var(--status-success, #16a34a)' },
			{ key: 'failed', label: 'Failed', count: buckets.failed, pct: failedPct, color: 'var(--status-error, #dc2626)' },
			{ key: 'active', label: 'Active', count: buckets.active, pct: activePct, color: 'var(--status-warning, #d97706)' }
		].filter((c) => c.count > 0);
		if (categories.length === 0) return [];
		if (categories.length === 1) return [{ ...categories[0], path: '', isFullCircle: true }];

		let angle = -Math.PI / 2; // 12 o'clock
		return categories.map((cat) => {
			const delta = (cat.count / total) * 2 * Math.PI;
			const next = angle + delta;
			const x1 = PIE_CX + PIE_R * Math.cos(angle);
			const y1 = PIE_CY + PIE_R * Math.sin(angle);
			const x2 = PIE_CX + PIE_R * Math.cos(next);
			const y2 = PIE_CY + PIE_R * Math.sin(next);
			const largeArc = delta > Math.PI ? 1 : 0;
			angle = next;
			return {
				...cat,
				path: `M ${PIE_CX} ${PIE_CY} L ${x1.toFixed(2)} ${y1.toFixed(2)} A ${PIE_R} ${PIE_R} 0 ${largeArc} 1 ${x2.toFixed(2)} ${y2.toFixed(2)} Z`,
				isFullCircle: false
			};
		});
	});
</script>

<div class="np-ops-state">
	<div class="np-ops-state__summary">
		<div class="np-panel__kicker">
			<a href="/tasks" class="np-panel__link" title="Open task center">
				<span>Tasks</span>
				<Icon name="arrow-right" size={11} />
			</a>
			<a href="/crew" class="np-panel__link" title="Open agent crew">
				<span>Agent Crew</span>
				<Icon name="arrow-right" size={11} />
			</a>
		</div>

		<div class="np-ops-state__yield">
			<div class="np-pie-container">
				<svg class="np-pie-svg" viewBox="0 0 100 100" role="img" aria-label="Solid pie chart of task outcomes">
					{#if total === 0}
						<circle
							cx={PIE_CX}
							cy={PIE_CY}
							r={PIE_R}
							fill="var(--bg-subtle, rgba(128, 128, 128, 0.08))"
							stroke="var(--border-color, rgba(128, 128, 128, 0.25))"
							stroke-width="1.5"
							stroke-dasharray="3 3"
						/>
						<text x="50" y="53" text-anchor="middle" class="pie-idle-text">IDLE</text>
					{:else}
						{#each pieSlices as slice (slice.key)}
							{#if slice.isFullCircle}
								<circle cx={PIE_CX} cy={PIE_CY} r={PIE_R} fill={slice.color} stroke="var(--bg-surface, #ffffff)" stroke-width="1.5">
									<title>{slice.label}: {slice.count} ({slice.pct}%)</title>
								</circle>
							{:else}
								<path d={slice.path} fill={slice.color} stroke="var(--bg-surface, #ffffff)" stroke-width="1.5" stroke-linejoin="round">
									<title>{slice.label}: {slice.count} ({slice.pct}%)</title>
								</path>
							{/if}
						{/each}
					{/if}
				</svg>
			</div>

			<ul class="np-yield-list" aria-label="Task outcomes">
				<li class="np-yield-item is-inflight">
					<span class="np-yield-bullet" aria-hidden="true">●</span>
					<span class="np-yield-name">Active</span>
					<span class="np-yield-count"><strong>{buckets.active}</strong><small>({activePct}%)</small></span>
				</li>
				<li class="np-yield-item is-succeeded">
					<span class="np-yield-bullet" aria-hidden="true">●</span>
					<span class="np-yield-name">Succeeded</span>
					<span class="np-yield-count"><strong>{buckets.succeeded}</strong><small>({succeededPct}%)</small></span>
				</li>
				<li class="np-yield-item is-failed">
					<span class="np-yield-bullet" aria-hidden="true">●</span>
					<span class="np-yield-name">Failed</span>
					<span class="np-yield-count"><strong>{buckets.failed}</strong><small>({failedPct}%)</small></span>
				</li>
			</ul>
		</div>

		<p
			class="np-ops-state__agents fleet-metrics-list"
			title="{enabledAgents} enabled · {runningAgents} active · {totalAgents} total agents"
		>
			<span class="np-ops-state__agents-label">Agents</span>
			<span>Enabled: <strong>{enabledAgents}</strong></span>
			<span class="np-ops-state__sep" aria-hidden="true">·</span>
			<span class:has-active={runningAgents > 0}>Active: <strong>{runningAgents}</strong></span>
			<span class="np-ops-state__sep" aria-hidden="true">·</span>
			<span>Total: <strong>{totalAgents}</strong></span>
		</p>
	</div>

	<div class="np-ops-state__recent">
		<div class="np-ops-state__recent-kicker">Recently updated</div>
		{#if rows.length === 0}
			<p class="np-ops-state__empty">No tasks yet today.</p>
		{:else}
			<ul class="np-ops-tasks" aria-label="Recently updated tasks">
				{#each rows as task (task.id)}
					{@const age = shortRelativeTime(task.updatedAt || task.createdAt, nowMs)}
					<li>
						<a
							class="np-ops-task"
							href={`/tasks?selected=${encodeURIComponent(task.id)}`}
							title={`${task.title || 'Untitled task'} · ${task.status}`}
						>
							<span class="np-ops-task__dot is-{taskDotTone(task.status)}" aria-hidden="true"></span>
							<span class="np-ops-task__title">{task.title || 'Untitled task'}</span>
							<span class="np-ops-task__age">{age}</span>
						</a>
					</li>
				{/each}
			</ul>
		{/if}
	</div>
</div>

<style>
	.np-ops-state {
		display: grid;
		grid-template-columns: minmax(0, 1fr) minmax(0, 1.25fr);
		gap: 1.75rem;
		height: 100%;
		min-width: 0;
	}

	@media (max-width: 860px) {
		.np-ops-state {
			grid-template-columns: minmax(0, 1fr);
			grid-template-rows: auto minmax(0, 1fr);
			gap: 0.75rem;
		}
	}

	.np-ops-state__summary {
		display: flex;
		flex-direction: column;
		justify-content: center;
		gap: 0.6rem;
		min-width: 0;
	}

	.np-panel__kicker {
		display: flex;
		justify-content: space-between;
		align-items: center;
		gap: 0.75rem;
	}

	.np-panel__link {
		display: inline-flex;
		align-items: center;
		gap: 0.25rem;
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

	.np-ops-state__yield {
		display: flex;
		align-items: center;
		gap: 1rem;
	}

	.np-pie-container {
		width: 64px;
		height: 64px;
		flex-shrink: 0;
	}

	.np-pie-svg {
		width: 100%;
		height: 100%;
		overflow: visible;
		filter: drop-shadow(0 1px 2px rgba(0, 0, 0, 0.08));
	}

	.pie-idle-text {
		font-family: 'Cinzel', Georgia, serif;
		font-size: 0.52rem;
		letter-spacing: 0.1em;
		fill: var(--text-muted, #94a3b8);
	}

	.np-yield-list {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 0.2rem;
	}

	.np-yield-item {
		display: flex;
		align-items: center;
		font-size: 0.74rem;
		gap: 0.35rem;
		line-height: 1.2;
	}

	.np-yield-bullet {
		font-size: 0.68rem;
	}

	.np-yield-item.is-succeeded .np-yield-bullet {
		color: var(--status-success, #16a34a);
	}

	.np-yield-item.is-failed .np-yield-bullet {
		color: var(--status-error, #dc2626);
	}

	.np-yield-item.is-inflight .np-yield-bullet {
		color: var(--status-warning, #d97706);
	}

	.np-yield-name {
		color: var(--text-muted, #64748b);
		min-width: 5.5rem;
	}

	.np-yield-count {
		font-family: ui-monospace, SFMono-Regular, monospace;
		font-size: 0.72rem;
		color: var(--text-primary, #1e293b);
	}

	.np-yield-count small {
		color: var(--text-muted, #64748b);
		margin-left: 0.25rem;
	}

	.np-ops-state__agents {
		display: flex;
		flex-wrap: wrap;
		align-items: baseline;
		gap: 0.35rem;
		margin: 0;
		font-size: 0.72rem;
		color: var(--text-muted, #64748b);
	}

	.np-ops-state__agents strong {
		font-family: ui-monospace, SFMono-Regular, monospace;
		color: var(--text-primary, #1e293b);
	}

	.np-ops-state__agents .has-active strong {
		color: var(--status-success, #16a34a);
	}

	.np-ops-state__agents-label {
		font-family: ui-monospace, SFMono-Regular, monospace;
		font-size: 0.62rem;
		letter-spacing: 0.1em;
		text-transform: uppercase;
		margin-right: 0.15rem;
	}

	.np-ops-state__sep {
		opacity: 0.6;
	}

	.np-ops-state__recent {
		display: flex;
		flex-direction: column;
		min-width: 0;
		min-height: 0;
		border-left: 1px solid var(--border-color, rgba(128, 128, 128, 0.2));
		padding-left: 1.75rem;
	}

	@media (max-width: 860px) {
		.np-ops-state__recent {
			border-left: none;
			padding-left: 0;
			border-top: 1px dashed var(--border-color, rgba(128, 128, 128, 0.2));
			padding-top: 0.6rem;
		}
	}

	.np-ops-state__recent-kicker {
		font-family: ui-monospace, SFMono-Regular, monospace;
		font-size: 0.62rem;
		letter-spacing: 0.1em;
		text-transform: uppercase;
		color: var(--text-muted, #64748b);
		margin-bottom: 0.3rem;
	}

	.np-ops-state__empty {
		margin: 0;
		font-family: 'Newsreader', Georgia, serif;
		font-style: italic;
		font-size: 0.82rem;
		color: var(--text-muted, #64748b);
	}

	.np-ops-tasks {
		/* About three and a half rows show; the half row signals the list scrolls. */
		--ops-task-row: 1.5rem;
		list-style: none;
		margin: 0;
		padding: 0;
		flex: 0 1 auto;
		min-height: 0;
		max-height: calc(3.5 * var(--ops-task-row));
		overflow-y: auto;
		overscroll-behavior: contain;
	}

	.np-ops-task {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		box-sizing: border-box;
		height: var(--ops-task-row, 1.5rem);
		padding: 0 0.25rem;
		border-bottom: 1px solid color-mix(in srgb, var(--border-color, rgba(128, 128, 128, 0.2)) 60%, transparent);
		color: var(--text-primary, #1e293b);
		text-decoration: none;
		font-size: 0.8rem;
		line-height: 1.3;
		border-radius: 2px;
	}

	.np-ops-task:hover {
		background: color-mix(in srgb, var(--text-primary) 4%, transparent);
	}

	.np-ops-task:focus-visible {
		outline: 2px solid var(--accent-primary, #6366f1);
		outline-offset: -2px;
	}

	.np-ops-task__dot {
		width: 6px;
		height: 6px;
		border-radius: 50%;
		flex-shrink: 0;
		background: var(--border-color, rgba(128, 128, 128, 0.45));
	}

	.np-ops-task__dot.is-active {
		background: var(--status-warning, #d97706);
	}

	.np-ops-task__dot.is-success {
		background: var(--status-success, #16a34a);
	}

	.np-ops-task__dot.is-danger {
		background: var(--status-error, #dc2626);
	}

	.np-ops-task__title {
		flex: 1 1 auto;
		min-width: 0;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
		font-family: 'Newsreader', Georgia, serif;
	}

	.np-ops-task__age {
		flex-shrink: 0;
		font-family: ui-monospace, SFMono-Regular, monospace;
		font-size: 0.66rem;
		color: var(--text-muted, #64748b);
		min-width: 2.2rem;
		text-align: right;
	}
</style>
