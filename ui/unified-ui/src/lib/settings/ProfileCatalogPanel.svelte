<script lang="ts">
	/** One row per server-owned model, with usage aggregated across resolved assignments. */
	import { onMount } from 'svelte';
	import { contextLabel, thinkingLabel, selectionForRoute } from '$lib/llm/modelCatalog';
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

	interface CatalogEntry extends RoutingProfile { label: string; controls: string; }
	const entries = $derived<CatalogEntry[]>((overview?.models ?? []).map(model => ({ name: model.id, label: model.label, model: model.model, provider: model.provider,
			class: model.provider === 'ollama' ? 'local' : model.provider.startsWith('harness-') ? 'harness' : 'api',
			installed: model.installed !== false, selectable: true,
			controls: [model.limits ? `${contextLabel(model.limits.context_window_tokens)} context` : '',
				model.thinking.levels.length ? `Thinking: ${model.thinking.levels.map(thinkingLabel).join(' / ')}` : 'No thinking adjustment'].filter(Boolean).join(' · ')
		})));

	const usage = $derived.by(() => {
		const counts = new Map<string, number>();
		for (const op of overview?.operations ?? []) {
			const key = selectionForRoute(overview?.models ?? [], overview?.profiles ?? [], op.effective_profile)?.model_id ?? op.effective_profile;
			counts.set(key, (counts.get(key) ?? 0) + 1);
		}
		return counts;
	});

	const visibleProfiles = $derived.by(() => {
		const needle = filter.trim().toLowerCase();
		return entries
			.filter((p) => classFilter === 'all' || p.class === classFilter)
			.filter((p) => !unusedOnly || (usage.get(p.name) ?? 0) === 0)
			.filter(
				(p) =>
					!needle ||
					p.label.toLowerCase().includes(needle) ||
					p.model.toLowerCase().includes(needle) ||
					p.provider.toLowerCase().includes(needle)
			)
			.sort(
				(a, b) =>
					(usage.get(b.name) ?? 0) - (usage.get(a.name) ?? 0) || a.label.localeCompare(b.label)
			);
	});

	const unusedCount = $derived(
		entries.filter((p) => (usage.get(p.name) ?? 0) === 0).length
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
	<h3>Models</h3>
	<p class="hint">
		Available providers and models, their supported controls, and the operations assigned to them.
		Use Model routing above to change an assignment.
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
			{visibleProfiles.length} of {entries.length} models
		</p>

		<div class="rows">
			{#each visibleProfiles as profile (profile.name)}
				<div class="row" class:unused={(usage.get(profile.name) ?? 0) === 0}>
					<span class="name" title={profile.label}>{profile.label}</span>
					<span class="chip {profile.class}">{classLabel[profile.class] ?? 'API'}</span>
					<span class="model" title={`${profile.provider} · ${profile.model}`}>
						{profile.provider} · {profile.model}
						{#if profile.controls}<small>{profile.controls}</small>{/if}
					</span>
					<span class="usage">{usageLabel(profile)}</span>
					{#if !profile.installed}
						<em class="not-installed" title="CLI not on PATH">not installed</em>
					{:else}
						<span></span>
					{/if}
				</div>
			{:else}
				<p class="empty">{entries.length ? 'No model matches that filter.' : 'Model choices are unavailable.'}</p>
			{/each}
		</div>
	{:else if !error}
		<p class="hint">Loading models…</p>
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
		grid-template-columns: minmax(0, 1fr) auto;
		gap: 0.6rem;
		align-items: center;
		padding: 0.65rem 0.75rem;
		border: 1px solid var(--border-soft);
		border-radius: 8px;
	}
	/* Unused is information, not an error: a model can be available for a flow that is
	   off today. Muted rather than coloured so it reads as "nothing points here". */
	.row.unused {
		opacity: 0.6;
	}
	.name {
		font-weight: 600;
		font-size: 0.8rem;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.model {
		grid-column: 1 / -1;
		display: grid;
		gap: .25rem;
		font-size: 0.78rem;
		opacity: 0.8;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: normal;
		overflow-wrap: anywhere;
	}
	.model small { font-size: var(--text-2xs, .72rem); }
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
	.empty {
		font-size: 0.85rem;
		opacity: 0.7;
		margin: 0.4rem 0;
	}
</style>
