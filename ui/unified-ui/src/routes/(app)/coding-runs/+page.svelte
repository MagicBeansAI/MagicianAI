<!--
  Work in progress: everything the owner has going (`workLiveStore` over
  `GET /work/live`). Coding runs keep their Steer, Stop and Open rows
  (`LiveRunsStrip`); crews (one row each) and Work tasks get a row with their
  state, step, Open (the item's `web_path`) and Follow, which pins the item to
  the phones. The desktop notch ear opens this page in its side panel when a
  dot is tapped (`?run=` names a tapped coding run, `?item=` a crew or task; a
  later tap arrives as `notch://runs-select`). It also works as an ordinary
  page.
-->
<script lang="ts">
	import { onDestroy, onMount, tick } from 'svelte';
	import { get } from 'svelte/store';
	import { browser } from '$app/environment';
	import { goto, replaceState } from '$app/navigation';
	import { page } from '$app/stores';
	import LiveRunsStrip from '$lib/shell/vibe/live/LiveRunsStrip.svelte';
	import { hideNativeAttentionWindow } from '$lib/attention/nativeWindow';
	import { vibeDevLiveRunsStore } from '$lib/stores/vibeDevLiveRunsStore';
	import { workLiveStore, type WorkItem, type WorkItemState } from '$lib/stores/workLiveStore';

	/** The desktop host emits this when another dot is tapped while the panel is open. */
	const RUN_SELECT_EVENT = 'notch://runs-select';
	const STATE_LABEL: Record<WorkItemState, string> = {
		needs_you: 'Needs you',
		working: 'Working',
		paused: 'Paused',
		failed: 'Failed',
		stopped: 'Stopped',
		done: 'Done'
	};

	$: nativeWindow = $page.url.searchParams.get('native_attention') === '1';
	$: runs = $vibeDevLiveRunsStore.runs;
	/** Coding runs show in `LiveRunsStrip`; these are the crews and Work tasks. */
	$: others = $workLiveStore.items.filter((item) => item.kind !== 'coding_run');
	$: loaded = $vibeDevLiveRunsStore.loaded && $workLiveStore.loaded;
	$: activeCount =
		runs.filter((run) => run.state === 'working' || run.state === 'needs_you').length +
		others.filter((item) => item.state === 'working' || item.state === 'needs_you').length;
	$: followError = $workLiveStore.followError;

	let selectedRunId: string | null = null;
	let selectedItemId: string | null = null;
	pick($page.url.searchParams.get('run'), $page.url.searchParams.get('item'));
	let followBusy: string[] = [];
	let stopListening: (() => void) | null = null;
	let destroyed = false;

	onMount(() => {
		vibeDevLiveRunsStore.start();
		workLiveStore.start();
		window.addEventListener('keydown', closeOnEscape);
		if (nativeWindow && '__TAURI_INTERNALS__' in window) {
			void import('@tauri-apps/api/event')
				.then(({ listen }) =>
					listen<{ runId?: string | null; itemId?: string | null }>(RUN_SELECT_EVENT, (event) =>
						selectItem(event.payload?.runId ?? null, event.payload?.itemId ?? null)
					)
				)
				.then((unlisten) => {
					if (destroyed) unlisten();
					else stopListening = unlisten;
				})
				.catch(() => undefined);
		}
	});

	onDestroy(() => {
		destroyed = true;
		stopListening?.();
		if (browser) {
			window.removeEventListener('keydown', closeOnEscape);
			vibeDevLiveRunsStore.stop();
			workLiveStore.stop();
		}
	});

	/** A coding run picks its `LiveRunsStrip` row; a crew or task picks its own. */
	function pick(runId: string | null, itemId: string | null): void {
		if (itemId?.startsWith('run:')) {
			runId = itemId.slice('run:'.length);
			itemId = null;
		}
		selectedRunId = runId || null;
		selectedItemId = runId ? null : itemId || null;
	}

	/** Pick an item and keep `?run=` / `?item=` in step, so the host sees the panel already shows it. */
	function selectItem(runId: string | null, itemId: string | null): void {
		pick(runId, itemId);
		const current = get(page);
		const next = new URL(current.url);
		next.searchParams.delete('run');
		next.searchParams.delete('item');
		if (selectedRunId) next.searchParams.set('run', selectedRunId);
		else if (selectedItemId) next.searchParams.set('item', selectedItemId);
		replaceState(next, current.state);
	}

	/** Scrolls the picked row into view once it renders. */
	function reveal(node: HTMLElement, selected: boolean) {
		const show = async (on: boolean) => {
			if (!on) return;
			await tick();
			node.scrollIntoView?.({ block: 'nearest' });
		};
		void show(selected);
		return { update: (next: boolean) => void show(next) };
	}

	function detail(item: WorkItem): string {
		const parts: string[] = [];
		if (item.context) parts.push(item.context);
		parts.push(item.kind === 'crew' ? 'Crew' : 'Task');
		if (item.step) parts.push(item.step);
		if (item.spentUsd !== null) {
			parts.push(
				item.budgetUsd !== null
					? `$${item.spentUsd.toFixed(2)} of $${item.budgetUsd.toFixed(2)}`
					: `$${item.spentUsd.toFixed(2)}`
			);
		}
		return parts.join(' · ');
	}

	async function toggleFollow(item: WorkItem): Promise<void> {
		if (followBusy.includes(item.id)) return;
		followBusy = [...followBusy, item.id];
		try {
			if (item.followed) await workLiveStore.unfollow(item.id);
			else await workLiveStore.follow(item.id);
		} finally {
			followBusy = followBusy.filter((id) => id !== item.id);
		}
	}

	async function openPath(path: string): Promise<void> {
		if (browser && nativeWindow && '__TAURI_INTERNALS__' in window) {
			const { invoke } = await import('@tauri-apps/api/core');
			await invoke('plugin:magician-attention|open_application_path', { path });
			return;
		}
		await goto(path);
	}

	function closeOnEscape(event: KeyboardEvent): void {
		if (!nativeWindow || event.key !== 'Escape' || event.defaultPrevented) return;
		event.preventDefault();
		void hideNativeAttentionWindow();
	}
</script>

<svelte:head><title>Work in progress</title></svelte:head>

<div class="coding-runs" class:coding-runs--panel={nativeWindow}>
	<header class="coding-runs__head">
		<h1 class="coding-runs__title">
			Work in progress
			{#if activeCount > 0}<span class="coding-runs__count">{activeCount} active</span>{/if}
		</h1>
		{#if nativeWindow}
			<button
				type="button"
				class="coding-runs__close"
				aria-label="Close"
				on:click={() => void hideNativeAttentionWindow()}
			>✕</button>
		{/if}
	</header>

	{#if loaded && runs.length === 0 && others.length === 0}
		<p class="coding-runs__empty">
			Nothing in progress right now. Coding runs, crews and Work tasks you start show here.
		</p>
	{/if}
	<LiveRunsStrip compact={nativeWindow} showTitle={false} {selectedRunId} {openPath} />

	{#if others.length > 0}
		<section class="work-items" class:work-items--compact={nativeWindow} aria-label="Crews and tasks">
			{#if followError}
				<p class="work-items__notice" role="status">
					{followError}
					<button type="button" class="work-items__link" on:click={() => workLiveStore.clearFollowError()}
						>Dismiss</button
					>
				</p>
			{/if}
			<ul class="work-items__list">
				{#each others as item (item.id)}
					<li
						class="work-items__row"
						class:work-items__row--selected={item.id === selectedItemId}
						data-state={item.state}
						aria-current={item.id === selectedItemId ? 'true' : undefined}
						use:reveal={item.id === selectedItemId}
					>
						<span class="work-items__dot" aria-hidden="true"></span>
						<div class="work-items__info">
							<button
								type="button"
								class="work-items__name"
								disabled={!item.webPath}
								on:click={() => void openPath(item.webPath)}
							>{item.title}</button>
							<span class="work-items__meta">
								<span class="work-items__state">{STATE_LABEL[item.state]}</span>
								· {detail(item)}
							</span>
						</div>
						<div class="work-items__actions">
							{#if item.webPath}
								<button
									type="button"
									class="work-items__btn"
									class:work-items__btn--primary={item.state === 'needs_you'}
									on:click={() => void openPath(item.webPath)}
								>{item.state === 'needs_you' ? 'Review' : 'Open'}</button>
							{/if}
							<button
								type="button"
								class="work-items__btn"
								aria-pressed={item.followed}
								title={item.followed ? 'Stop showing this on your phone' : 'Show this on your phone until it ends'}
								disabled={followBusy.includes(item.id)}
								on:click={() => void toggleFollow(item)}
							>{item.followed ? 'Unfollow' : 'Follow'}</button>
						</div>
					</li>
				{/each}
			</ul>
		</section>
	{/if}
</div>

<style>
	.coding-runs {
		max-width: 52rem;
		margin: 0 auto;
		padding: 1.5rem 1rem;
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
	}

	.coding-runs--panel {
		max-width: none;
		margin: 0;
		padding: 0.75rem 0.75rem 1rem;
		height: 100vh;
		overflow-y: auto;
	}

	.coding-runs__head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.5rem;
	}

	.coding-runs__title {
		margin: 0;
		font-size: var(--text-base, 1rem);
		font-weight: 600;
		display: flex;
		align-items: baseline;
		gap: 0.5rem;
	}

	.coding-runs__count {
		font-size: var(--text-2xs, 0.72rem);
		font-weight: 500;
		color: var(--text-muted);
	}

	.coding-runs__close {
		width: 1.75rem;
		height: 1.75rem;
		border: 0;
		border-radius: 50%;
		background: transparent;
		color: var(--text-muted);
		font: inherit;
		cursor: pointer;
	}

	.coding-runs__close:hover {
		background: color-mix(in srgb, var(--text-primary, #000) 8%, transparent);
		color: var(--text-primary);
	}

	.coding-runs__empty {
		margin: 0;
		font-size: var(--text-sm, 0.85rem);
		color: var(--text-muted);
	}

	/* Crew and task rows, drawn like `LiveRunsStrip`'s. */
	.work-items {
		--run-working: var(--accent-primary, #4f7cff);
		--run-needs-you: var(--color-warning, #e0a02a);
		--run-done: var(--color-success, #2f9e5b);
		--run-failed: var(--color-error, #d23a3a);
		--run-idle: var(--text-muted, #8a8a8a);
		border: 1px solid var(--border-subtle, rgba(0, 0, 0, 0.12));
		border-radius: var(--radius-lg, 12px);
		padding: 0.75rem 0.9rem;
		background: var(--bg-elevated, transparent);
	}

	.work-items--compact {
		padding: 0.55rem 0.7rem;
	}

	.work-items__list {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
	}

	.work-items__row {
		display: grid;
		grid-template-columns: auto minmax(0, 1fr) auto;
		align-items: start;
		gap: 0.6rem;
	}

	.work-items__row--selected {
		margin: -0.3rem -0.4rem;
		padding: 0.3rem 0.4rem;
		border-radius: var(--radius-md, 8px);
		background: color-mix(in srgb, var(--accent-primary, #4f7cff) 10%, transparent);
		box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--accent-primary, #4f7cff) 35%, transparent);
	}

	.work-items__dot {
		width: 0.55rem;
		height: 0.55rem;
		margin-top: 0.35rem;
		border-radius: 50%;
		background: var(--run-idle);
	}

	[data-state='working'] .work-items__dot {
		background: var(--run-working);
		animation: work-items-pulse 1.6s ease-in-out infinite;
	}
	[data-state='needs_you'] .work-items__dot,
	[data-state='paused'] .work-items__dot { background: var(--run-needs-you); }
	[data-state='done'] .work-items__dot { background: var(--run-done); }
	[data-state='failed'] .work-items__dot { background: var(--run-failed); }

	@keyframes work-items-pulse {
		50% { opacity: 0.35; }
	}

	@media (prefers-reduced-motion: reduce) {
		[data-state='working'] .work-items__dot { animation: none; }
	}

	.work-items__info {
		display: flex;
		flex-direction: column;
		gap: 0.15rem;
		min-width: 0;
	}

	.work-items__name {
		all: unset;
		cursor: pointer;
		font-size: var(--text-sm, 0.85rem);
		font-weight: 500;
		color: var(--text-primary);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.work-items__name:disabled { cursor: default; }
	.work-items__name:not(:disabled):hover { text-decoration: underline; }
	.work-items__name:focus-visible { outline: 2px solid var(--run-working); outline-offset: 2px; }

	.work-items__meta,
	.work-items__notice {
		font-size: var(--text-2xs, 0.72rem);
		color: var(--text-muted);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.work-items__notice {
		margin: 0 0 0.5rem;
		color: var(--run-failed);
		white-space: normal;
	}

	.work-items__link {
		all: unset;
		cursor: pointer;
		margin-left: 0.35rem;
		color: var(--text-muted);
		text-decoration: underline;
	}

	.work-items__state { font-weight: 600; }
	[data-state='needs_you'] .work-items__state { color: var(--run-needs-you); }
	[data-state='failed'] .work-items__state { color: var(--run-failed); }

	.work-items__actions {
		display: flex;
		gap: 0.35rem;
		flex-shrink: 0;
	}

	.work-items__btn {
		padding: 0.3rem 0.65rem;
		border-radius: var(--radius-md, 8px);
		border: 1px solid var(--border-subtle, rgba(0, 0, 0, 0.12));
		background: var(--bg-base, #fff);
		color: var(--text-primary);
		font-size: var(--text-xs, 0.78rem);
		cursor: pointer;
	}

	.work-items__btn:disabled { opacity: 0.55; cursor: default; }
	.work-items__btn--primary { border-color: var(--run-needs-you); }
	.work-items__btn[aria-pressed='true'] {
		border-color: var(--run-working);
		color: var(--run-working);
	}
</style>
