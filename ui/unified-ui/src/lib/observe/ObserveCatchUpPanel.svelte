<script lang="ts">
	import { createEventDispatcher, onMount } from 'svelte';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import {
		fetchObserveCatchUp,
		saveObserveCatchUp,
		type CatchUpReplayMode,
		type ObserveCatchUpEnvelope,
		type ObserveCatchUpPolicy
	} from './observeCatchUp';

	const dispatch = createEventDispatcher<{ policychange: ObserveCatchUpPolicy }>();
	let envelope: ObserveCatchUpEnvelope | null = null;
	let draft: ObserveCatchUpPolicy | null = null;
	let busy = false;
	let error = '';
	let notice = '';
	let expanded = false;
	let policyOpen = false;

	const fallbackOptions = {
		lookback_days: [1, 5, 7, 14, 30],
		max_items_per_source: [10, 25, 50, 100, 200],
		max_total_items: [50, 100, 200, 500, 1000],
		max_duration_minutes: [5, 15, 30, 60]
	};

	async function load(): Promise<boolean> {
		try {
			envelope = await fetchObserveCatchUp();
			draft = { ...envelope.status.policy };
			dispatch('policychange', draft);
			error = '';
			notice = envelope.warnings?.join(' ') ?? '';
			return true;
		} catch (reason) {
			error = reason instanceof Error ? reason.message : 'Startup catch-up is unavailable.';
			return false;
		}
	}

	async function refreshStatus() {
		if (busy) return;
		try {
			envelope = await fetchObserveCatchUp();
			if (envelope.warnings?.length) notice = envelope.warnings.join(' ');
		} catch (reason) {
			if (!envelope) {
				error = reason instanceof Error ? reason.message : 'Startup catch-up is unavailable.';
			}
		}
	}

	async function save() {
		if (!draft || busy) return;
		busy = true;
		try {
			envelope = await saveObserveCatchUp(draft);
			draft = { ...envelope.status.policy };
			dispatch('policychange', draft);
			error = '';
			notice = envelope.warnings?.join(' ') ?? '';
		} catch (reason) {
			const saveError =
				reason instanceof Error ? reason.message : 'Could not save startup catch-up.';
			if (await load()) error = saveError;
		} finally {
			busy = false;
		}
	}

	function alignTotalCap() {
		if (!draft || draft.max_total_items >= draft.max_items_per_source) return;
		const totals = envelope?.options.max_total_items ?? fallbackOptions.max_total_items;
		draft.max_total_items =
			totals.find((candidate) => candidate >= draft!.max_items_per_source) ??
			draft.max_items_per_source;
	}

	function replayLabel(mode: CatchUpReplayMode): string {
		switch (mode) {
			case 'checkpointed_replay':
				return 'Replayable';
			case 'current_snapshot_only':
				return 'Current feed only';
			case 'scheduled_window':
				return 'Bounded window';
		}
	}

	function phaseLabel(phase: string): string {
		return phase.replaceAll('_', ' ').replace(/^./, (value) => value.toUpperCase());
	}

	onMount(() => {
		void load();
		const timer = window.setInterval(() => {
			if (!envelope) {
				void load();
			} else if (envelope.status.phase === 'waiting' || envelope.status.phase === 'active') {
				void refreshStatus();
			}
		}, 5_000);
		return () => window.clearInterval(timer);
	});
</script>

<section class="catch-up surface-card" aria-labelledby="catch-up-title">
	<header class="catch-up__head">
		<div class="catch-up__title">
			<span class="catch-up__icon"><Icon name="rotate-ccw" size={16} /></span>
			<div>
				<h2 id="catch-up-title">Startup catch-up</h2>
				<p>Bounded recovery after Magician was offline. Future polling stays on either way.</p>
			</div>
		</div>
		{#if envelope}
			<span
				class:off={!envelope.status.policy.enabled}
				class:expired={envelope.status.phase === 'expired'}
				class="catch-up__phase"
				title={envelope.status.phase === 'expired'
					? 'The startup recovery window for this boot has closed. Normal polling continues.'
					: undefined}
			>
				{phaseLabel(envelope.status.phase)}
			</span>
			<div class="catch-up__progress" aria-label="Feed and message catch-up progress">
				<span><strong>{envelope.status.processed_items}</strong> processed</span>
				<span><strong>{envelope.status.reserved_items}</strong> running</span>
				<span><strong>{envelope.status.remaining_items}</strong> budget left</span>
			</div>
		{/if}
		{#if draft}
			<label class="catch-up__switch">
				<input type="checkbox" bind:checked={draft.enabled} />
				<span>{draft.enabled ? 'On' : 'Off'}</span>
			</label>
			<button type="button" class="action-button action-button--outline action-button--sm" on:click={() => (policyOpen = !policyOpen)}>
				<span>{policyOpen ? 'Hide policy' : 'Edit policy'}</span>
			</button>
			<button
				type="button"
				class="action-button action-button--primary action-button--sm"
				disabled={busy}
				on:click={() => void save()}
			>
				<Icon name="check" size={14} />
				<span>{busy ? 'Saving…' : 'Save catch-up policy'}</span>
			</button>
		{/if}
	</header>

	{#if !draft && !error}
		<p class="catch-up__muted">Loading catch-up policy…</p>
	{:else if policyOpen && draft}
		<div class="catch-up__fields" class:disabled={!draft.enabled}>
			<label>
				<span>History window</span>
				<select bind:value={draft.lookback_days} disabled={!draft.enabled}>
					{#each envelope?.options.lookback_days ?? fallbackOptions.lookback_days as days}
						<option value={days}>{days === 1 ? '1 day' : `${days} days`}</option>
					{/each}
				</select>
			</label>
			<label>
				<span>Each source may admit</span>
				<select
					bind:value={draft.max_items_per_source}
					disabled={!draft.enabled}
					on:change={alignTotalCap}
				>
					{#each envelope?.options.max_items_per_source ?? fallbackOptions.max_items_per_source as cap}
						<option value={cap}>Up to {cap} items</option>
					{/each}
				</select>
			</label>
			<label>
				<span>Feeds and messages together</span>
				<select bind:value={draft.max_total_items} disabled={!draft.enabled}>
					{#each envelope?.options.max_total_items ?? fallbackOptions.max_total_items as cap}
						<option value={cap}>Up to {cap} items</option>
					{/each}
				</select>
			</label>
			<label>
				<span>Stop starting catch-up after</span>
				<select bind:value={draft.max_duration_minutes} disabled={!draft.enabled}>
					{#each envelope?.options.max_duration_minutes ?? fallbackOptions.max_duration_minutes as minutes}
						<option value={minutes}>{minutes} minutes</option>
					{/each}
				</select>
			</label>
		</div>

		<div class="catch-up__actions">
			<button type="button" class="action-button action-button--outline" on:click={() => (expanded = !expanded)}>
				<span>{expanded ? 'Hide source behavior' : 'How each source catches up'}</span>
				<Icon name={expanded ? 'chevron-up' : 'chevron-down'} size={14} />
			</button>
		</div>

		{#if expanded && envelope}
			<div class="catch-up__sources">
				{#each envelope.status.sources as source}
					<article>
						<div class="catch-up__source-head">
							<strong>{source.display_name}</strong>
							<span>{replayLabel(source.replay_mode)}</span>
						</div>
						<p>{source.limitation}</p>
						<small>
							{#if source.source_id === 'calendar'}
								Scheduled separately; not counted in the feed/message ledger
							{:else if source.runs === 0}
								Waiting for its first automatic check this boot
							{:else}
								{source.processed} {source.item_unit} processed this boot
								{#if source.failures > 0} · {source.failures} failed{/if}
							{/if}
						</small>
						{#if source.last_error}
							<small class="catch-up__source-error">{source.last_error}</small>
						{/if}
					</article>
				{/each}
			</div>
		{/if}
	{/if}

	{#if error}<p class="catch-up__error">{error}</p>{/if}
	{#if notice}<p class="catch-up__notice">{notice}</p>{/if}
</section>

<style>
	.catch-up { padding: 0.85rem 1rem; margin-bottom: 0; }
	.catch-up__head, .catch-up__title, .catch-up__actions,
	.catch-up__source-head, .catch-up__progress { display: flex; align-items: center; }
	.catch-up__head { flex-wrap: wrap; justify-content: flex-start; gap: 0.55rem 0.75rem; }
	.catch-up__source-head { justify-content: space-between; gap: 1rem; }
	.catch-up__head .catch-up__progress { margin-top: 0; }
	.catch-up__title { gap: .75rem; flex: 1 1 16rem; min-width: 0; }
	.catch-up__title h2 { margin: 0; font-size: 1rem; }
	.catch-up__title p, .catch-up__sources p { margin: .2rem 0 0; color: var(--text-secondary); font-size: .82rem; line-height: 1.4; }
	.catch-up__icon { display: grid; place-items: center; width: 2rem; height: 2rem; border-radius: .65rem; color: var(--accent); background: color-mix(in srgb, var(--accent) 12%, transparent); }
	.catch-up__phase, .catch-up__source-head span { flex: none; border: 1px solid color-mix(in srgb, var(--success) 35%, transparent); border-radius: 999px; padding: .2rem .55rem; color: var(--success); font-size: .7rem; font-weight: 650; }
	.catch-up__phase.off { color: var(--text-tertiary); border-color: var(--border-subtle); }
	/* "Expired" is the closed-window resting state after a boot's bounded
	   recovery finished — quiet, not an alarm and not a success. */
	.catch-up__phase.expired { color: var(--text-tertiary); border-color: var(--border-subtle); }
	.catch-up__switch { display: flex; align-items: center; gap: .45rem; font-size: .8rem; font-weight: 650; }
	.catch-up__fields { display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: .75rem; margin-top: .8rem; }
	.catch-up__fields.disabled { opacity: .58; }
	.catch-up__fields label { display: grid; gap: .35rem; color: var(--text-secondary); font-size: .75rem; }
	.catch-up__fields select { width: 100%; min-width: 0; }
	.catch-up__progress { flex-wrap: wrap; gap: .55rem; margin-top: .8rem; }
	.catch-up__progress span { padding: .35rem .55rem; border-radius: .55rem; background: var(--bg-subtle, var(--bg-card)); color: var(--text-secondary); font-size: .75rem; }
	.catch-up__actions { flex-wrap: wrap; gap: .55rem; margin-top: .85rem; }
	.catch-up__sources { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: .65rem; margin-top: .8rem; }
	.catch-up__sources article { border: 1px solid var(--border-subtle); border-radius: .7rem; padding: .7rem; }
	.catch-up__sources small, .catch-up__muted { color: var(--text-tertiary); font-size: .72rem; }
	.catch-up__source-head span { color: var(--text-secondary); border-color: var(--border-subtle); }
	.catch-up__source-error { display: block; margin-top: .35rem; color: var(--danger); }
	.catch-up__error { color: var(--danger); font-size: .8rem; margin: .7rem 0 0; }
	.catch-up__notice { color: var(--warning, var(--text-secondary)); font-size: .8rem; margin: .7rem 0 0; }
	@media (max-width: 860px) { .catch-up__fields { grid-template-columns: repeat(2, minmax(0, 1fr)); } }
	@media (max-width: 560px) { .catch-up__head { align-items: flex-start; } .catch-up__fields, .catch-up__sources { grid-template-columns: 1fr; } }
</style>
