<!--
  The VibeDev rail's Crews section: goals split into lanes that run at once,
  each lane its own trusted run and branch (`vibeDevCrewStore`). A new crew is
  planned by the server and waits for the owner's start, here or in Attention
  (the notch answers it too). Lane rows open their run.
-->
<script lang="ts">
	import { createEventDispatcher, onDestroy } from 'svelte';
	import type { VibeDevProject } from '$lib/stores/vibeDevProjectStore';
	import {
		vibeDevCrewStore,
		isCrewOpen,
		SETTLED_CREW_PAGE,
		type VibeDevCrew,
		type CrewLaneStatus,
		type CrewStatus
	} from '$lib/stores/vibeDevCrewStore';

	export let project: VibeDevProject | null = null;

	const dispatch = createEventDispatcher<{ selectRun: { taskId: string } }>();

	const CREW_LABEL: Record<CrewStatus, string> = {
		proposed: 'Waiting to start',
		running: 'Running',
		choosing: 'Pick an attempt',
		ready: 'Merging lanes',
		integrated: 'Merged',
		conflict: 'Lanes conflict',
		failed: 'Failed',
		cancelled: 'Stopped'
	};
	const LANE_LABEL: Record<CrewLaneStatus, string> = {
		pending: 'Waiting',
		running: 'Running',
		done: 'Done',
		failed: 'Failed',
		stopped: 'Stopped'
	};

	let formOpen = false;
	let goal = '';
	let budget = '';
	let mode: 'split' | 'race' = 'split';
	let racers = '3';
	let review = false;
	let formError = '';
	const ROLE_LABEL = { work: '', integrate: 'Integrator', review: 'Reviewer' } as const;
	let showSettled = false;

	$: trusted = project?.trust === 'trusted';
	$: vibeDevCrewStore.load(project?.project_id ?? null);
	$: state = $vibeDevCrewStore;
	// Crews still in play show, and the ones acted on here (the store keeps
	// those). A crew that ends moves to the earlier crews, which the server
	// pages (`vibeDevCrewStore.loadSettled`).
	$: openCrews = state.crews.filter(isCrewOpen);
	$: listedIds = new Set(state.crews.map((crew) => crew.crewId));
	$: shownCrews = showSettled
		? [...state.crews, ...state.settled.filter((crew) => !listedIds.has(crew.crewId))]
		: state.crews;
	$: moreSettled = showSettled && state.settled.length < state.settledTotal;

	function toggleSettled(): void {
		showSettled = !showSettled;
		if (showSettled) void vibeDevCrewStore.loadSettled(true);
	}

	// A crew that ends while the earlier crews are open joins them at once.
	let seenSettledTotal = 0;
	$: if (state.settledTotal !== seenSettledTotal) {
		const grew = state.settledTotal > seenSettledTotal;
		seenSettledTotal = state.settledTotal;
		if (showSettled && grew) void vibeDevCrewStore.loadSettled(true);
	}

	function act(crewId: string, action: (id: string) => Promise<unknown>): void {
		void action(crewId);
	}

	onDestroy(() => vibeDevCrewStore.load(null));

	async function submit(): Promise<void> {
		formError = '';
		const trimmed = goal.trim();
		if (!trimmed) {
			formError = 'Say what the crew should build.';
			return;
		}
		let budgetUsd: number | undefined;
		if (budget.trim()) {
			budgetUsd = Number(budget);
			if (!Number.isFinite(budgetUsd) || budgetUsd <= 0) {
				formError = 'The budget must be a positive number of dollars.';
				return;
			}
		}
		let racerCount: number | undefined;
		if (mode === 'race') {
			racerCount = Number(racers);
			if (!Number.isInteger(racerCount) || racerCount < 2 || racerCount > 8) {
				formError = 'A race needs 2 to 8 attempts.';
				return;
			}
		}
		const crew = await vibeDevCrewStore.create({
			goal: trimmed,
			budgetUsd,
			mode,
			racers: racerCount,
			review
		});
		if (crew) {
			goal = '';
			budget = '';
			formOpen = false;
		}
	}
</script>

{#if project}
	<div class="rail__section crews">
		<div class="rail__section-head crews__head">
			Crews
			{#if openCrews.length > 0}<span class="rail__section-count">{openCrews.length} open</span>{/if}
			{#if trusted}
				<button
					type="button"
					class="crews__new"
					aria-expanded={formOpen}
					on:click={() => (formOpen = !formOpen)}
				>{formOpen ? 'Cancel' : 'New crew'}</button>
			{/if}
		</div>

		{#if !trusted}
			<p class="rail__hint">Crews need a trusted project: their lanes run at once, each on its own branch.</p>
		{:else if formOpen}
			<form class="crews__form" on:submit|preventDefault={() => void submit()}>
				<textarea
					class="crews__goal"
					rows="3"
					aria-label="Crew goal"
					placeholder="What should the crew build? It is split into lanes that run at once."
					bind:value={goal}
				></textarea>
				<div class="crews__modes" role="radiogroup" aria-label="How the crew works">
					<label><input type="radio" bind:group={mode} value="split" /> Split the goal</label>
					<label><input type="radio" bind:group={mode} value="race" /> Race engines</label>
				</div>
				{#if mode === 'race'}
					<input
						class="crews__budget"
						type="text"
						inputmode="numeric"
						aria-label="Number of attempts"
						placeholder="Attempts (2–8)"
						bind:value={racers}
					/>
				{/if}
				<label class="crews__check">
					<input type="checkbox" bind:checked={review} /> Review the merged branch first
				</label>
				<input
					class="crews__budget"
					type="text"
					inputmode="decimal"
					aria-label="Budget in dollars for all lanes (optional)"
					placeholder="Budget, $ (optional)"
					bind:value={budget}
				/>
				{#if formError}<p class="crews__error" role="alert">{formError}</p>{/if}
				<button type="submit" class="crews__btn crews__btn--primary" disabled={state.busy.includes('new')}>
					{state.busy.includes('new')
						? mode === 'race'
							? 'Setting up…'
							: 'Planning lanes…'
						: mode === 'race'
							? 'Set up race'
							: 'Plan lanes'}
				</button>
			</form>
		{/if}

		{#if state.error}<p class="crews__error" role="alert">{state.error}</p>{/if}

		{#each shownCrews as crew (crew.crewId)}
			<article class="crew" data-status={crew.status} aria-label={`Crew: ${crew.goal}`}>
				<header class="crew__head">
					<span class="crew__status">{CREW_LABEL[crew.status]}</span>
					{#if crew.budgetUsd !== null || crew.spentUsd !== null}
						<span class="crew__budget">
							{crew.spentUsd !== null ? `$${crew.spentUsd.toFixed(2)}` : ''}{crew.spentUsd !== null && crew.budgetUsd !== null ? ' of ' : ''}{crew.budgetUsd !== null ? `$${crew.budgetUsd.toFixed(2)}` : ''}
						</span>
					{/if}
				</header>
				<p class="crew__goal" title={crew.goal}>{crew.goal}</p>
				<ul class="crew__lanes">
					{#each crew.lanes as lane, index (`${index}:${lane.lane}`)}
						<li class="crew__lane" data-status={lane.status}>
							<span class="crew__dot" aria-hidden="true"></span>
							{#if lane.runTaskId}
								<button
									type="button"
									class="crew__lane-name"
									title={lane.brief}
									on:click={() => lane.runTaskId && dispatch('selectRun', { taskId: lane.runTaskId })}
								>{lane.lane}</button>
							{:else}
								<span class="crew__lane-name" title={lane.brief}>{lane.lane}</span>
							{/if}
							<span class="crew__lane-status">
								{#if ROLE_LABEL[lane.role]}<span class="crew__role">{ROLE_LABEL[lane.role]}</span>{/if}
								{LANE_LABEL[lane.status]}
							</span>
						</li>
					{/each}
				</ul>
				{#if crew.note}<p class="crew__note">{crew.note}</p>{/if}
				{#if crew.branch}<p class="crew__note">Branch <code>{crew.branch}</code></p>{/if}
				{#if crew.conflict}
					<p class="crew__note crew__note--warn">
						Lane {crew.conflict.lane} conflicts in {crew.conflict.files.join(', ')}. Resolve it on the
						lane's branch, then merge again.
					</p>
				{/if}
				{#if crew.pullRequestUrl}
					<a class="crew__link" href={crew.pullRequestUrl} target="_blank" rel="noreferrer">Pull request</a>
				{/if}
				{#if crew.status === 'integrated' && !crew.pullRequestUrl}
					<div class="crew__actions">
						<button
							type="button"
							class="crews__btn crews__btn--primary"
							disabled={state.busy.includes(crew.crewId)}
							on:click={() => act(crew.crewId, vibeDevCrewStore.openPullRequest)}
						>Open pull request</button>
					</div>
				{:else if crew.status === 'choosing'}
					<div class="crew__actions crew__actions--wrap">
						{#each crew.lanes as lane, index (`${index}:${lane.lane}`)}
							{#if lane.role === 'work' && lane.status === 'done'}
								<button
									type="button"
									class="crews__btn crews__btn--primary"
									disabled={state.busy.includes(crew.crewId)}
									on:click={() => act(crew.crewId, (id) => vibeDevCrewStore.choose(id, index))}
								>Keep {lane.lane}</button>
							{/if}
						{/each}
						<button
							type="button"
							class="crews__btn crews__btn--danger"
							disabled={state.busy.includes(crew.crewId)}
							on:click={() => act(crew.crewId, (id) => vibeDevCrewStore.choose(id, null))}
						>Keep none</button>
					</div>
				{:else if crew.status === 'conflict'}
					<div class="crew__actions">
						{#if crew.mode === 'split'}
							<button
								type="button"
								class="crews__btn crews__btn--primary"
								disabled={state.busy.includes(crew.crewId)}
								on:click={() => act(crew.crewId, vibeDevCrewStore.resolve)}
							>Resolve with an agent</button>
						{/if}
						<button
							type="button"
							class="crews__btn crews__btn--primary"
							disabled={state.busy.includes(crew.crewId)}
							on:click={() => act(crew.crewId, vibeDevCrewStore.integrate)}
						>Merge again</button>
					</div>
				{:else if crew.status === 'proposed' || crew.status === 'running'}
					<div class="crew__actions">
						{#if crew.status === 'proposed'}
							<button
								type="button"
								class="crews__btn crews__btn--primary"
								disabled={state.busy.includes(crew.crewId)}
								on:click={() => act(crew.crewId, vibeDevCrewStore.start)}
							>Start</button>
						{/if}
						<button
							type="button"
							class="crews__btn crews__btn--danger"
							disabled={state.busy.includes(crew.crewId)}
							on:click={() => act(crew.crewId, vibeDevCrewStore.stop)}
						>{crew.status === 'proposed' ? 'Discard' : 'Stop'}</button>
					</div>
				{/if}
			</article>
		{/each}

		{#if moreSettled}
			<button
				type="button"
				class="rail__more"
				disabled={state.settledLoading}
				on:click={() => void vibeDevCrewStore.loadSettled()}
			>{state.settledLoading ? 'Loading…' : `Load ${Math.min(SETTLED_CREW_PAGE, state.settledTotal - state.settled.length)} more`}</button>
		{/if}
		{#if state.settledTotal > 0}
			<button type="button" class="rail__more" on:click={toggleSettled}>
				{showSettled
					? 'Hide earlier crews'
					: `Show ${state.settledTotal} earlier crew${state.settledTotal === 1 ? '' : 's'}`}
			</button>
		{/if}
	</div>
{/if}

<style>
	/* The rail's section look; StudioRail's own rules are scoped to it. */
	.rail__section {
		display: flex;
		flex-direction: column;
		gap: 0.3rem;
	}
	.rail__section-head {
		font-size: var(--text-2xs);
		font-weight: 700;
		letter-spacing: 0.06em;
		text-transform: uppercase;
		color: var(--vibe-text-muted);
		padding: 0 0.2rem;
	}
	.rail__section-count {
		margin-left: 0.35rem;
		font-weight: 500;
		text-transform: none;
		letter-spacing: 0;
		color: var(--vibe-text-muted);
	}
	.rail__hint {
		margin: 0;
		font-size: 0.76rem;
		color: var(--vibe-text-muted);
		padding: 0 0.2rem;
	}
	.rail__more {
		width: 100%;
		margin-top: 0.35rem;
		border: 1px dashed var(--vibe-border);
		border-radius: var(--radius-sm, 10px);
		background: transparent;
		color: var(--vibe-text-muted);
		font: inherit;
		font-size: 0.76rem;
		font-weight: 600;
		padding: 0.4rem 0.6rem;
		cursor: pointer;
	}
	.rail__more:hover:not(:disabled) {
		color: var(--vibe-accent);
		border-color: var(--vibe-accent);
	}

	.crews {
		--crew-working: var(--accent-primary, #4f7cff);
		--crew-wait: var(--color-warning, #e0a02a);
		--crew-done: var(--color-success, #2f9e5b);
		--crew-failed: var(--color-error, #d23a3a);
	}

	.crews__head {
		display: flex;
		align-items: center;
		gap: 0.4rem;
	}

	.crews__new {
		margin-left: auto;
		border: 0;
		background: transparent;
		color: var(--crew-working);
		font: inherit;
		font-size: var(--text-2xs);
		font-weight: 700;
		letter-spacing: 0.04em;
		text-transform: none;
		cursor: pointer;
	}

	.crews__form {
		display: flex;
		flex-direction: column;
		gap: 0.35rem;
	}

	.crews__goal,
	.crews__budget {
		width: 100%;
		box-sizing: border-box;
		border: 1px solid var(--vibe-border);
		border-radius: var(--radius-sm, 10px);
		background: transparent;
		color: inherit;
		font: inherit;
		font-size: 0.8rem;
		padding: 0.4rem 0.5rem;
		resize: vertical;
	}

	.crews__error {
		margin: 0;
		font-size: 0.74rem;
		color: var(--crew-failed);
	}

	.crews__btn {
		border: 1px solid var(--vibe-border);
		border-radius: var(--radius-sm, 10px);
		background: transparent;
		color: inherit;
		font: inherit;
		font-size: 0.74rem;
		padding: 0.25rem 0.6rem;
		cursor: pointer;
	}

	.crews__btn:disabled {
		opacity: 0.55;
		cursor: default;
	}

	.crews__btn--primary {
		border-color: var(--crew-working);
		color: var(--crew-working);
	}

	.crews__btn--danger {
		color: var(--crew-failed);
	}

	.crew {
		display: flex;
		flex-direction: column;
		gap: 0.3rem;
		border: 1px solid var(--vibe-border);
		border-radius: var(--radius-sm, 10px);
		padding: 0.5rem 0.55rem;
	}

	.crew__head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		font-size: var(--text-2xs);
		font-weight: 700;
		color: var(--vibe-text-muted);
	}

	.crew[data-status='proposed'] .crew__status {
		color: var(--crew-wait);
	}
	.crew[data-status='running'] .crew__status {
		color: var(--crew-working);
	}
	.crew[data-status='ready'] .crew__status,
	.crew[data-status='integrated'] .crew__status {
		color: var(--crew-done);
	}
	.crew[data-status='conflict'] .crew__status {
		color: var(--crew-wait);
	}
	.crew[data-status='failed'] .crew__status {
		color: var(--crew-failed);
	}

	.crew__goal {
		margin: 0;
		font-size: 0.8rem;
		display: -webkit-box;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		-webkit-box-orient: vertical;
		overflow: hidden;
	}

	.crew__lanes {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 0.15rem;
	}

	.crew__lane {
		display: grid;
		grid-template-columns: auto minmax(0, 1fr) auto;
		align-items: center;
		gap: 0.4rem;
		font-size: 0.76rem;
	}

	.crew__dot {
		width: 0.45rem;
		height: 0.45rem;
		border-radius: 50%;
		background: var(--vibe-text-muted);
	}

	.crew__lane[data-status='running'] .crew__dot {
		background: var(--crew-working);
	}
	.crew__lane[data-status='done'] .crew__dot {
		background: var(--crew-done);
	}
	.crew__lane[data-status='failed'] .crew__dot {
		background: var(--crew-failed);
	}

	.crew__lane-name {
		all: unset;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	button.crew__lane-name {
		cursor: pointer;
	}

	button.crew__lane-name:hover {
		text-decoration: underline;
	}

	button.crew__lane-name:focus-visible {
		outline: 2px solid var(--crew-working);
		outline-offset: 2px;
	}

	.crew__lane-status,
	.crew__note,
	.crew__budget {
		font-size: var(--text-2xs);
		color: var(--vibe-text-muted);
	}

	.crew__note {
		margin: 0;
	}

	.crew__note--warn {
		color: var(--crew-wait);
	}

	.crew__note code {
		font-size: 0.7rem;
	}

	.crew__link {
		font-size: 0.76rem;
		color: var(--crew-working);
	}

	.crew__actions {
		display: flex;
		gap: 0.35rem;
	}

	.crew__actions--wrap {
		flex-wrap: wrap;
	}

	.crews__modes {
		display: flex;
		gap: 0.6rem;
		font-size: 0.76rem;
	}

	.crews__check {
		font-size: 0.76rem;
	}

	.crew__role {
		margin-right: 0.3rem;
		font-weight: 700;
		color: var(--crew-working);
	}
</style>
