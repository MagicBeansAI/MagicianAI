<!--
  Coding runs at a glance: every project's VibeDev runs that are going,
  waiting on the owner, or just ended, one row each (`vibeDevLiveRunsStore`).
  A live run can be steered or stopped in place; Stop waits STOP_UNDO_MS so a
  slip can be taken back. Renders nothing while there are no runs.
-->
<script lang="ts">
	import { onDestroy, onMount, tick } from 'svelte';
	import {
		vibeDevLiveRunsStore,
		liveRunPath,
		type LiveRun,
		type LiveRunState
	} from '$lib/stores/vibeDevLiveRunsStore';
	import { controlRun } from '$lib/shell/vibe/conversation/control';

	/** Opens an app path; the notch drawer routes this to the main window. */
	export let openPath: (path: string) => void | Promise<void>;
	export let compact = false;
	/** The run a notch ear dot opened: highlighted and scrolled into view. */
	export let selectedRunId: string | null = null;
	/** Off where the host already titles the list (the notch runs panel). */
	export let showTitle = true;

	const STOP_UNDO_MS = 2_000;
	const STATE_LABEL: Record<LiveRunState, string> = {
		needs_you: 'Needs you',
		working: 'Working',
		paused: 'Paused',
		failed: 'Failed',
		stopped: 'Stopped',
		done: 'Done'
	};

	let steeringId: string | null = null;
	let steerText = '';
	let sendingId: string | null = null;
	let notice: { runId: string; text: string } | null = null;
	const pendingStops = new Map<string, ReturnType<typeof setTimeout>>();
	/** The turn each pending Stop was pressed on, should the run leave the list. */
	const pendingStopTurns = new Map<string, string>();
	let pendingStopIds: string[] = [];

	$: runs = $vibeDevLiveRunsStore.runs;
	$: void revealSelected(selectedRunId, runs.length);

	const rowNodes = new Map<string, HTMLElement>();
	function trackRow(node: HTMLElement, runId: string) {
		let current = runId;
		rowNodes.set(current, node);
		return {
			update(next: string) {
				rowNodes.delete(current);
				current = next;
				rowNodes.set(current, node);
			},
			destroy() {
				rowNodes.delete(current);
			}
		};
	}

	async function revealSelected(runId: string | null, _count: number): Promise<void> {
		if (!runId) return;
		await tick();
		rowNodes.get(runId)?.scrollIntoView({ block: 'nearest' });
	}
	$: activeCount = runs.filter((run) => run.state === 'working' || run.state === 'needs_you').length;

	onMount(() => vibeDevLiveRunsStore.start());
	onDestroy(() => {
		// A Stop the owner pressed still goes out when the strip unmounts
		// inside its undo window; leaving the view is not an undo.
		for (const [runId, handle] of pendingStops) {
			clearTimeout(handle);
			void controlRun(currentLiveTaskId(runId), 'stop');
		}
		pendingStops.clear();
		pendingStopTurns.clear();
		vibeDevLiveRunsStore.stop();
	});

	/** The run's newest live turn now: a follow-up may have started since the click. */
	function currentLiveTaskId(runId: string): string {
		const current = $vibeDevLiveRunsStore.runs.find((row) => row.runId === runId);
		return current?.liveTaskId ?? pendingStopTurns.get(runId) ?? runId;
	}

	function canControl(run: LiveRun): boolean {
		return run.state === 'working' || run.state === 'paused' || run.state === 'needs_you';
	}

	function detail(run: LiveRun): string {
		const parts = [run.projectName || 'VibeDev'];
		if (run.crewId) parts.push(run.lane ? `Lane ${run.lane}` : 'Crew');
		const line = run.summary ?? run.step ?? run.latestTurnTitle;
		if (line) parts.push(line);
		else parts.push(STATE_LABEL[run.state]);
		return parts.join(' · ');
	}

	function syncPendingStops(): void {
		pendingStopIds = [...pendingStops.keys()];
	}

	function stop(run: LiveRun): void {
		if (pendingStops.has(run.runId)) return;
		pendingStopTurns.set(run.runId, run.liveTaskId);
		pendingStops.set(
			run.runId,
			setTimeout(() => {
				const liveTaskId = currentLiveTaskId(run.runId);
				pendingStops.delete(run.runId);
				pendingStopTurns.delete(run.runId);
				syncPendingStops();
				void send({ ...run, liveTaskId }, 'stop');
			}, STOP_UNDO_MS)
		);
		syncPendingStops();
	}

	function undoStop(run: LiveRun): void {
		const handle = pendingStops.get(run.runId);
		if (handle) clearTimeout(handle);
		pendingStops.delete(run.runId);
		pendingStopTurns.delete(run.runId);
		syncPendingStops();
	}

	function openSteer(run: LiveRun): void {
		steeringId = steeringId === run.runId ? null : run.runId;
		steerText = '';
	}

	async function send(run: LiveRun, action: 'steer' | 'stop', message?: string): Promise<void> {
		sendingId = run.runId;
		notice = null;
		const result = await controlRun(run.liveTaskId, action, message);
		sendingId = null;
		if (result.ok && result.delivered) {
			if (action === 'steer') {
				steeringId = null;
				steerText = '';
			}
			notice = { runId: run.runId, text: action === 'stop' ? 'Stopping.' : 'Sent to the run.' };
			void vibeDevLiveRunsStore.refresh();
			return;
		}
		notice = {
			runId: run.runId,
			text:
				result.status === 409
					? 'The run is between turns. Open it to send a follow-up.'
					: (result.error ?? 'That did not reach the run.')
		};
	}

	function focusOnMount(node: HTMLInputElement): void {
		node.focus();
	}

	function onSteerKey(event: KeyboardEvent, run: LiveRun): void {
		if (event.key === 'Escape') {
			event.preventDefault();
			event.stopPropagation();
			steeringId = null;
			return;
		}
		if (event.key === 'Enter' && !event.shiftKey && steerText.trim()) {
			event.preventDefault();
			void send(run, 'steer', steerText.trim());
		}
	}
</script>

{#if runs.length > 0}
	<section class="live-runs" class:live-runs--compact={compact} aria-label="Coding runs">
		{#if showTitle}
			<h2 class="live-runs__title">
				Coding runs
				{#if activeCount > 0}<span class="live-runs__count">{activeCount} active</span>{/if}
			</h2>
		{/if}
		<ul class="live-runs__list">
			{#each runs as run (run.runId)}
				<li
					class="live-runs__row"
					class:live-runs__row--selected={run.runId === selectedRunId}
					data-state={run.state}
					aria-current={run.runId === selectedRunId ? 'true' : undefined}
					use:trackRow={run.runId}
				>
					<span class="live-runs__dot" aria-hidden="true"></span>
					<div class="live-runs__info">
						<button
							type="button"
							class="live-runs__name"
							title="Open in VibeDev"
							on:click={() => void openPath(liveRunPath(run))}
						>{run.title}</button>
						<span class="live-runs__meta">
							<span class="live-runs__state">{STATE_LABEL[run.state]}</span>
							· {detail(run)}
						</span>
						{#if notice?.runId === run.runId}
							<span class="live-runs__notice" role="status">{notice.text}</span>
						{/if}
						{#if steeringId === run.runId}
							<input
								class="live-runs__steer"
								type="text"
								aria-label={`Steer ${run.title}`}
								placeholder="Tell this run what to change…"
								bind:value={steerText}
								disabled={sendingId === run.runId}
								use:focusOnMount
								on:keydown={(event) => onSteerKey(event, run)}
							/>
						{/if}
					</div>
					<div class="live-runs__actions">
						{#if run.state === 'needs_you'}
							<button
								type="button"
								class="live-runs__btn live-runs__btn--primary"
								on:click={() => void openPath(liveRunPath(run))}
							>Review</button>
						{/if}
						{#if canControl(run)}
							{#if pendingStopIds.includes(run.runId)}
								<button type="button" class="live-runs__btn" on:click={() => undoStop(run)}>Undo stop</button>
							{:else}
								<button
									type="button"
									class="live-runs__btn"
									aria-expanded={steeringId === run.runId}
									disabled={sendingId === run.runId}
									on:click={() => openSteer(run)}
								>Steer</button>
								<button
									type="button"
									class="live-runs__btn live-runs__btn--danger"
									disabled={sendingId === run.runId}
									on:click={() => stop(run)}
								>Stop</button>
							{/if}
						{:else if run.state !== 'needs_you'}
							<button
								type="button"
								class="live-runs__btn"
								on:click={() => void openPath(liveRunPath(run))}
							>Open</button>
						{/if}
					</div>
				</li>
			{/each}
		</ul>
	</section>
{/if}

<style>
	.live-runs {
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

	.live-runs--compact {
		padding: 0.55rem 0.7rem;
	}

	.live-runs__title {
		margin: 0 0 0.5rem;
		font-size: var(--text-sm, 0.85rem);
		font-weight: 600;
		display: flex;
		align-items: baseline;
		gap: 0.5rem;
	}

	.live-runs__count {
		font-size: var(--text-2xs, 0.72rem);
		font-weight: 500;
		color: var(--text-muted);
	}

	.live-runs__list {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
	}

	.live-runs__row {
		display: grid;
		grid-template-columns: auto minmax(0, 1fr) auto;
		align-items: start;
		gap: 0.6rem;
	}

	.live-runs__row--selected {
		margin: -0.3rem -0.4rem;
		padding: 0.3rem 0.4rem;
		border-radius: var(--radius-md, 8px);
		background: color-mix(in srgb, var(--accent-primary, #4f7cff) 10%, transparent);
		box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--accent-primary, #4f7cff) 35%, transparent);
	}

	.live-runs__dot {
		width: 0.55rem;
		height: 0.55rem;
		margin-top: 0.35rem;
		border-radius: 50%;
		background: var(--run-idle);
	}

	[data-state='working'] .live-runs__dot {
		background: var(--run-working);
		animation: live-runs-pulse 1.6s ease-in-out infinite;
	}
	[data-state='needs_you'] .live-runs__dot { background: var(--run-needs-you); }
	[data-state='done'] .live-runs__dot { background: var(--run-done); }
	[data-state='failed'] .live-runs__dot { background: var(--run-failed); }

	@keyframes live-runs-pulse {
		50% { opacity: 0.35; }
	}

	@media (prefers-reduced-motion: reduce) {
		[data-state='working'] .live-runs__dot { animation: none; }
	}

	.live-runs__info {
		display: flex;
		flex-direction: column;
		gap: 0.15rem;
		min-width: 0;
	}

	.live-runs__name {
		all: unset;
		cursor: pointer;
		font-size: var(--text-sm, 0.85rem);
		font-weight: 500;
		color: var(--text-primary);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.live-runs__name:hover { text-decoration: underline; }
	.live-runs__name:focus-visible { outline: 2px solid var(--run-working); outline-offset: 2px; }

	.live-runs__meta,
	.live-runs__notice {
		font-size: var(--text-2xs, 0.72rem);
		color: var(--text-muted);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.live-runs__state { font-weight: 600; }
	[data-state='needs_you'] .live-runs__state { color: var(--run-needs-you); }
	[data-state='failed'] .live-runs__state { color: var(--run-failed); }

	.live-runs__steer {
		margin-top: 0.3rem;
		padding: 0.35rem 0.5rem;
		border-radius: var(--radius-md, 8px);
		border: 1px solid var(--border-subtle, rgba(0, 0, 0, 0.12));
		background: var(--bg-base, #fff);
		color: var(--text-primary);
		font-size: var(--text-xs, 0.78rem);
	}

	.live-runs__actions {
		display: flex;
		gap: 0.35rem;
		flex-shrink: 0;
	}

	.live-runs__btn {
		padding: 0.3rem 0.65rem;
		border-radius: var(--radius-md, 8px);
		border: 1px solid var(--border-subtle, rgba(0, 0, 0, 0.12));
		background: var(--bg-base, #fff);
		color: var(--text-primary);
		font-size: var(--text-xs, 0.78rem);
		cursor: pointer;
	}

	.live-runs__btn:disabled { opacity: 0.55; cursor: default; }
	.live-runs__btn--primary { border-color: var(--run-needs-you); }
	.live-runs__btn--danger { color: var(--run-failed); }
</style>
