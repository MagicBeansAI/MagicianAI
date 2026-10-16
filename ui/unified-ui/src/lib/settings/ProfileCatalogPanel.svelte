<script lang="ts">
	/**
	 * Profile catalog — the router's profiles as a browsable list.
	 *
	 * Deliberately profile-centric, where `ModelRoutingPanel` is
	 * operation-centric. That panel answers "what serves this operation?" and
	 * shows profiles only as choices inside a per-operation dropdown, so with
	 * ~230 of them there is no way to see what exists, what a profile actually
	 * is, or which ones nothing uses.
	 *
	 * Reads the same `GET /llm/routing` overview the routing panel does — no new
	 * endpoint, and no second source of truth for what a profile is. Usage
	 * counts are derived here from the operations in that same payload, so a
	 * profile's count can never disagree with the routing table beside it.
	 *
	 * Read-only on purpose. Profiles carry provider credentials, context budgets
	 * and adapter settings; a mistyped edit turns into per-call failures far from
	 * the edit that caused it. Which profile serves an operation is the operator
	 * decision, and that already has a surface.
	 */
	import { onMount } from 'svelte';
	import {
		fetchRoutingOverview,
		type RoutingOverview,
		type RoutingProfile
	} from '$lib/stores/modelRoutingStore';

	let overview = $state<RoutingOverview | null>(null);
	let error = $state<string | null>(null);
	let filter = $state('');
	let classFilter = $state<'all' | 'local' | 'api' | 'harness'>('all');
	let unusedOnly = $state(false);

	const classLabel: Record<string, string> = {
		local: 'Local',
		api: 'API',
		harness: 'Harness'
	};

	/**
	 * Operations per profile, counted on `effective_profile` rather than
	 * `default_profile`: what matters when reading a catalog is what a profile is
	 * serving right now, including operations pointed at it by an override or by
	 * engine affinity.
	 */
	const usage = $derived.by(() => {
		const counts = new Map<string, number>();
		for (const op of overview?.operations ?? []) {
			counts.set(op.effective_profile, (counts.get(op.effective_profile) ?? 0) + 1);
		}
		return counts;
	});

	const visibleProfiles = $derived.by(() => {
		const needle = filter.trim().toLowerCase();
		return (overview?.profiles ?? [])
			.filter((p) => classFilter === 'all' || p.class === classFilter)
			.filter((p) => !unusedOnly || (usage.get(p.name) ?? 0) === 0)
			.filter(
				(p) =>
					!needle ||
					p.name.toLowerCase().includes(needle) ||
					p.model.toLowerCase().includes(needle) ||
					p.provider.toLowerCase().includes(needle)
			)
			.sort(
				(a, b) =>
					(usage.get(b.name) ?? 0) - (usage.get(a.name) ?? 0) || a.name.localeCompare(b.name)
			);
	});

	const unusedCount = $derived(
		(overview?.profiles ?? []).filter((p) => (usage.get(p.name) ?? 0) === 0).length
	);

	async function refresh(): Promise<void> {
		error = null;
		try {
			overview = await fetchRoutingOverview();
		} catch (cause) {
			error = cause instanceof Error ? cause.message : String(cause);
		}
	}

	function usageLabel(profile: RoutingProfile): string {
		const count = usage.get(profile.name) ?? 0;
		if (count === 0) return 'unused';
		return count === 1 ? '1 operation' : `${count} operations`;
	}

	onMount(() => {
		void refresh();
	});
</script>

<section class="catalog">
	<h3>Model profiles</h3>
	<p class="hint">
		Every profile the router can select, with what it runs on and how many operations use it
		right now. Editing a profile is a config change, not a settings toggle — this is the
		catalog; switching an operation happens in Model routing above.
	</p>

	{#if error}
		<p class="error" role="alert">{error}</p>
	{/if}

	{#if overview}
		<div class="controls">
			<input
				class="filter"
				type="search"
				placeholder="Filter by name, model, or provider…"
				bind:value={filter}
			/>
			<select aria-label="Filter by class" bind:value={classFilter}>
				<option value="all">All classes</option>
				<option value="local">Local</option>
				<option value="api">API</option>
				<option value="harness">Harness</option>
			</select>
			<label class="unused-toggle">
				<input type="checkbox" bind:checked={unusedOnly} />
				Unused only ({unusedCount})
			</label>
		</div>

		<p class="count">
			{visibleProfiles.length} of {overview.profiles.length} profiles
		</p>

		<div class="rows">
			{#each visibleProfiles as profile (profile.name)}
				<div class="row" class:unused={(usage.get(profile.name) ?? 0) === 0}>
					<span class="name" title={profile.name}>{profile.name}</span>
					<span class="chip {profile.class}">{classLabel[profile.class] ?? 'API'}</span>
					<span class="model" title={`${profile.provider} · ${profile.model}`}>
						{profile.provider} · {profile.model}
					</span>
					<span class="usage">{usageLabel(profile)}</span>
					{#if !profile.installed}
						<em class="not-installed" title="CLI not on PATH">not installed</em>
					{:else if profile.selectable === false}
						<em class="not-selectable" title="Adaptive composite: listed for display, not settable"
							>composite</em
						>
					{:else}
						<span></span>
					{/if}
				</div>
			{:else}
				<p class="empty">No profile matches that filter.</p>
			{/each}
		</div>
	{:else if !error}
		<p class="hint">Loading profiles…</p>
	{/if}
</section>

<style>
	.catalog {
		color: var(--text-primary);
		display: flex;
		flex-direction: column;
		gap: 0.6rem;
	}
	.hint {
		font-size: 0.85rem;
		opacity: 0.75;
		margin: 0;
	}
	.error {
		color: var(--accent-primary, #c2502a);
		font-size: 0.85rem;
		margin: 0;
	}
	.controls {
		display: flex;
		flex-wrap: wrap;
		gap: 0.5rem;
		align-items: center;
	}
	.filter {
		flex: 1 1 240px;
		min-width: 0;
		max-width: 320px;
		padding: 0.4rem 0.6rem;
		border-radius: 8px;
		border: 1px solid var(--input-border, var(--border-soft));
		background: var(--input-bg, var(--bg-soft));
		color: var(--text-primary);
	}
	select {
		font: inherit;
		font-size: 0.8rem;
		padding: 0.35rem 0.5rem;
		border-radius: 8px;
		border: 1px solid var(--input-border, var(--border-soft));
		background: var(--input-bg, var(--bg-soft));
		color: var(--text-primary);
	}
	.unused-toggle {
		display: flex;
		align-items: center;
		gap: 0.35rem;
		font-size: 0.8rem;
		opacity: 0.85;
	}
	.count {
		font-size: 0.75rem;
		opacity: 0.55;
		margin: 0;
	}
	.rows {
		display: flex;
		flex-direction: column;
		gap: 0.35rem;
		max-height: 420px;
		overflow-y: auto;
	}
	.row {
		display: grid;
		grid-template-columns: minmax(160px, 1.4fr) auto minmax(160px, 1fr) auto auto;
		gap: 0.6rem;
		align-items: center;
		padding: 0.3rem 0.5rem;
		border-radius: 8px;
	}
	/* Unused is information, not an error: a profile can exist for a flow that is
	   off today. Muted rather than coloured so it reads as "nothing points here". */
	.row.unused {
		opacity: 0.6;
	}
	.name {
		font-family: var(--font-mono, monospace);
		font-size: 0.8rem;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.model {
		font-size: 0.78rem;
		opacity: 0.8;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.usage {
		font-size: 0.75rem;
		opacity: 0.65;
		white-space: nowrap;
	}
	.chip {
		font-size: 0.75rem;
		padding: 0.15rem 0.5rem;
		border-radius: 999px;
		white-space: nowrap;
	}
	.chip.local {
		background: var(--color-success-soft, rgba(78, 205, 196, 0.18));
		color: var(--color-success, var(--text-secondary));
	}
	.chip.api {
		background: var(--color-info-soft, rgba(90, 140, 255, 0.15));
		color: var(--color-info, var(--text-secondary));
	}
	.chip.harness {
		background: var(--accent-primary-soft, rgba(170, 110, 255, 0.16));
		color: var(--accent-primary, var(--text-secondary));
	}
	.not-installed {
		font-size: 0.72rem;
		color: var(--accent-primary, #c2502a);
		white-space: nowrap;
	}
	.not-selectable {
		font-size: 0.72rem;
		opacity: 0.6;
		white-space: nowrap;
	}
	.empty {
		font-size: 0.85rem;
		opacity: 0.7;
		margin: 0.4rem 0;
	}
</style>
