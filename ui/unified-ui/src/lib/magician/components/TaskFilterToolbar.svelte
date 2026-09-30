<script context="module" lang="ts">
	/** One entry of the toolbar's display-only status ledger. */
	export interface TaskStatusChip {
		/** Canonical status key — feeds statusTone() for house coloring. */
		key: string;
		label: string;
		count: number;
	}

	/**
	 * Raw per-status counts for the toolbar's status ledger (the pill counts
	 * are FILTER counts, which fold statuses together — e.g. the Running pill
	 * counts running+paused). Statuses whose count is exactly a preset pill's
	 * count (completed) are omitted; zero counts are dropped so the ledger
	 * stays quiet. Display-only: the filter system (URL `?filter=` +
	 * taskStore.TaskFilter) only supports the six presets + tag filters, so
	 * these chips don't dispatch.
	 */
	const STATUS_CHIP_SPECS: ReadonlyArray<{ key: string; label: string; statuses: readonly string[] }> = [
		{ key: 'paused', label: 'Paused', statuses: ['paused'] },
		{ key: 'pending', label: 'Pending', statuses: ['pending'] },
		{ key: 'planning', label: 'Planning', statuses: ['planning'] },
		{ key: 'ready', label: 'Ready', statuses: ['ready'] },
		{ key: 'running', label: 'Running', statuses: ['running'] },
		{ key: 'failed', label: 'Failed', statuses: ['failed', 'cancelled'] }
	];

	export function computeStatusChips(tasks: ReadonlyArray<{ status: string }>): TaskStatusChip[] {
		return STATUS_CHIP_SPECS.map(({ key, label, statuses }) => ({
			key,
			label,
			count: tasks.reduce((total, task) => total + (statuses.includes(task.status) ? 1 : 0), 0)
		})).filter((chip) => chip.count > 0);
	}

	/** Keep the most compact filters immediately reachable before horizontal scroll. */
	export function orderTaskTags(tags: readonly string[]): string[] {
		return [...tags].sort((left, right) => {
			const lengthDelta = left.trim().length - right.trim().length;
			return lengthDelta !== 0
				? lengthDelta
				: left.localeCompare(right, undefined, { sensitivity: 'base' });
		});
	}
</script>

<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import { statusTone } from '$lib/shared/statusTone';

	/**
	 * v5 horizontal task-filter toolbar — preset filter pills (with count
	 * badges) + tag chips + a trailing display-only status ledger (per-status
	 * counts, replacing the old SpellsSurface summary card). Presentation
	 * only: it holds no store/URL state. It exposes a self-contained
	 * `(activeFilter, counts, tags, activeTag, statusChips, totalCount)` props
	 * + `filter` / `tag` events contract (the same shape the now-removed legacy
	 * vertical TaskFilterBar used), so each host page keeps its own
	 * filter→store/URL policy.
	 *
	 * Used by `/tasks` (global task list, `?filter=`/`?tag=` URL sync) and by
	 * the thread Tasks route (`/t/<id>/tasks`, in-memory thread-scoped filter).
	 *
	 * Layout note: the toolbar is `position: sticky; top: 0`, so place it as the
	 * first child of a scroll container whose top padding is 0.
	 */

	export let activeFilter = 'all';
	export let activeTag: string | null = null;
	export let tags: string[] = [];
	export let counts: Record<string, number | null | undefined> = {};
	/** Per-status counts for the trailing ledger — build via computeStatusChips(). */
	export let statusChips: TaskStatusChip[] = [];
	/** Total task count shown ahead of the status chips; null hides it. */
	export let totalCount: number | null = null;
	export let filters: string[] = ['all', 'inbox', 'today', 'overdue', 'running', 'completed'];
	export let labels: Record<string, string> = {
		all: 'All',
		inbox: 'Inbox',
		today: 'Today',
		overdue: 'Overdue',
		running: 'Running',
		completed: 'Completed'
	};

	const dispatch = createEventDispatcher<{
		filter: { value: string };
		tag: { value: string | null };
	}>();

	$: orderedTags = orderTaskTags(tags);
</script>

<div class="tasks-toolbar-v5" role="toolbar" aria-label="Task filters">
	<div class="tasks-toolbar-v5__primary">
		<div class="tasks-toolbar-v5__group">
			{#each filters as filt (filt)}
				<button
					type="button"
					class="tasks-toolbar-v5__filt"
					class:active={activeFilter === filt && !activeTag}
					on:click={() => dispatch('filter', { value: filt })}
				>
					{labels[filt] ?? filt}
					{#if counts[filt] != null}
						<span class="tasks-toolbar-v5__ct">{counts[filt]}</span>
					{/if}
				</button>
			{/each}
		</div>
		{#if totalCount != null || statusChips.length > 0}
			<!-- Display-only status ledger: raw per-status counts (dot = statusTone
			     color var). The filter presets don't cover arbitrary statuses, so
			     these are informational, not clickable. -->
			<div class="tasks-toolbar-v5__group tasks-toolbar-v5__group--status" role="note" aria-label="Task counts by status">
				{#if totalCount != null}
					<span class="tasks-toolbar-v5__total">{totalCount} total</span>
				{/if}
				{#each statusChips as chip (chip.key)}
					<span class="tasks-toolbar-v5__status" title={`${chip.count} ${chip.label.toLowerCase()} (status count)`}>
						<i class="tasks-toolbar-v5__dot" style={`background:${statusTone(chip.key).colorVar};`} aria-hidden="true"></i>
						<span class="tasks-toolbar-v5__status-ct">{chip.count}</span>
						{chip.label}
					</span>
				{/each}
			</div>
		{/if}
	</div>
	{#if tags.length > 0}
		<div class="tasks-toolbar-v5__group tasks-toolbar-v5__group--tags" aria-label="Task tags">
			{#each orderedTags as tag (tag)}
				<button
					type="button"
					class="tasks-toolbar-v5__tag"
					class:active={activeTag === tag}
					on:click={() => dispatch('tag', { value: activeTag === tag ? null : tag })}
				><span class="tasks-toolbar-v5__hash">#</span>{tag}</button>
			{/each}
		</div>
	{/if}
</div>

<style>
	/* Translucent + backdrop-blur so the AtmosphereLayer (paper grid + stipple)
	   reads through the toolbar without scrolled content bleeding through. */
	.tasks-toolbar-v5 {
		display: flex;
		flex-direction: column;
		align-items: stretch;
		gap: 7px;
		padding: 14px 24px 12px;
		border-bottom: 1px solid var(--border-soft, rgba(0, 0, 0, 0.08));
		position: sticky;
		top: 0;
		z-index: 10;
		background: color-mix(in srgb, var(--bg-base, #fff) 84%, transparent);
		backdrop-filter: blur(14px) saturate(140%);
		-webkit-backdrop-filter: blur(14px) saturate(140%);
	}

	.tasks-toolbar-v5__primary {
		display: flex;
		min-width: 0;
		align-items: center;
		gap: 8px;
	}

	.tasks-toolbar-v5__group {
		display: inline-flex;
		gap: 2px;
	}

	.tasks-toolbar-v5__group--tags {
		width: 100%;
		min-width: 0;
		flex-wrap: nowrap;
		gap: 4px;
		overflow-x: auto;
		overflow-y: hidden;
		overscroll-behavior-inline: contain;
		scrollbar-width: thin;
		scrollbar-color: color-mix(in srgb, var(--text-muted, #888) 28%, transparent) transparent;
		padding-bottom: 3px;
	}

	.tasks-toolbar-v5__group--tags::-webkit-scrollbar {
		height: 4px;
	}

	.tasks-toolbar-v5__group--tags::-webkit-scrollbar-track {
		background: transparent;
	}

	.tasks-toolbar-v5__group--tags::-webkit-scrollbar-thumb {
		border-radius: 999px;
		background: color-mix(in srgb, var(--text-muted, #888) 24%, transparent);
	}

	.tasks-toolbar-v5__group--tags::-webkit-scrollbar-thumb:hover {
		background: color-mix(in srgb, var(--text-muted, #888) 42%, transparent);
	}

	.tasks-toolbar-v5__filt {
		padding: 5px 11px 4px;
		border-radius: 7px;
		font-family: var(--font-primary);
		font-size: 12.5px;
		font-weight: 500;
		color: var(--text-muted, #888);
		background: transparent;
		border: 0;
		cursor: pointer;
		transition: background 0.12s ease, color 0.12s ease;
		display: inline-flex;
		align-items: center;
		gap: 6px;
	}

	.tasks-toolbar-v5__filt:hover {
		background: var(--bg-soft, rgba(0, 0, 0, 0.04));
		color: var(--text-primary, #1a1a1a);
	}

	.tasks-toolbar-v5__filt.active {
		background: var(--accent-primary-soft, rgba(0, 0, 0, 0.06));
		color: var(--text-primary, #1a1a1a);
	}

	.tasks-toolbar-v5__ct {
		font-family: var(--font-mono);
		font-size: 10.5px;
		color: var(--text-faint, #aaa);
	}

	.tasks-toolbar-v5__filt.active .tasks-toolbar-v5__ct {
		color: var(--text-muted, #777);
	}

	.tasks-toolbar-v5__tag {
		flex: none;
		padding: 4px 10px 3px;
		border-radius: 7px;
		font-family: var(--font-primary);
		font-size: 12px;
		color: var(--text-muted, #888);
		background: transparent;
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.08));
		white-space: nowrap;
		cursor: pointer;
		transition: background 0.12s ease, color 0.12s ease, border-color 0.12s ease;
	}

	.tasks-toolbar-v5__tag:hover {
		background: var(--bg-soft, rgba(0, 0, 0, 0.05));
		color: var(--text-primary, #1a1a1a);
		border-color: var(--border-default, rgba(0, 0, 0, 0.18));
	}

	.tasks-toolbar-v5__tag.active {
		background: var(--accent-primary-soft, rgba(0, 0, 0, 0.06));
		color: var(--text-primary, #1a1a1a);
		border-color: var(--accent-primary, var(--text-primary, #1a1a1a));
	}

	.tasks-toolbar-v5__hash {
		color: var(--accent-primary, #c2502a);
		margin-right: 3px;
	}

	/* Trailing display-only status ledger (absorbs the old summary card). */
	.tasks-toolbar-v5__group--status {
		margin-left: auto;
		align-items: center;
		gap: 10px;
		flex-wrap: wrap;
	}

	.tasks-toolbar-v5__total {
		font-family: var(--font-mono);
		font-size: var(--text-2xs);
		color: var(--text-faint, #aaa);
		margin-right: 2px;
		white-space: nowrap;
	}

	.tasks-toolbar-v5__status {
		display: inline-flex;
		align-items: center;
		gap: 5px;
		font-family: var(--font-primary);
		font-size: var(--text-2xs);
		font-weight: 500;
		color: var(--text-muted, #888);
		white-space: nowrap;
		cursor: default;
	}

	.tasks-toolbar-v5__dot {
		width: 7px;
		height: 7px;
		border-radius: 999px;
		flex: none;
	}

	.tasks-toolbar-v5__status-ct {
		font-family: var(--font-mono);
		color: var(--text-secondary, #555);
	}
</style>
