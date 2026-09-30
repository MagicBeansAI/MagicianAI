<script lang="ts">
	/**
	 * Quiet `↗` link that deep-jumps to `/events` scoped to a specific
	 * execution and anchored at a specific event timestamp.
	 *
	 * Drop this on any surface that renders an event-derived item
	 * (attention bar items, step rows, agent updates, feed entries).
	 * The receiving `/events` page reads `?t=<ms>` and scrolls to the
	 * row whose `timestamp_ms` is closest to that anchor, then briefly
	 * highlights it.
	 *
	 * See `docs/archive/plans/2026-05-10-execution-panel-canvas-redesign.md` Phase 4.
	 */
	import { goto } from '$app/navigation';

	export let executionId: string | null | undefined = null;
	export let taskId: string | null | undefined = null;
	export let agentId: string | null | undefined = null;
	export let timestampMs: number | null | undefined = null;
	export let title: string = 'Open in stream';
	export let label: string = '↗';

	$: visible = !!executionId || !!taskId || !!agentId;

	function open(event: MouseEvent): void {
		event.preventDefault();
		event.stopPropagation();
		const params = new URLSearchParams();
		if (executionId) params.set('execution_id', executionId);
		if (taskId) params.set('task_id', taskId);
		if (agentId) params.set('agent_id', agentId);
		if (timestampMs) params.set('t', String(timestampMs));
		void goto(`/events?${params.toString()}`);
	}
</script>

{#if visible}
	<button
		type="button"
		class="open-in-stream"
		title={title}
		aria-label={title}
		on:click={open}
	>
		{label}
	</button>
{/if}

<style>
	.open-in-stream {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		padding: 0.1rem 0.35rem;
		font-size: 0.75rem;
		line-height: 1;
		color: var(--text-muted, #7c746a);
		background: transparent;
		border: 1px solid transparent;
		border-radius: 6px;
		cursor: pointer;
		transition: color 140ms ease, background 140ms ease, border-color 140ms ease;
	}

	.open-in-stream:hover {
		color: var(--accent-primary);
		background: var(--accent-primary-soft);
		border-color: var(--accent-primary);
	}
</style>
