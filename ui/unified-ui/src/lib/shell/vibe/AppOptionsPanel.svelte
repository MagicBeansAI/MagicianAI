<script lang="ts">
	/**
	 * VibeDev App options. Same catalog as `magician app tools|agents|
	 * personalities|procedure list`. Selection inserts YAML names into
	 * the composer. There is no free-text "add any tool" box.
	 */
	import { createEventDispatcher, onDestroy, onMount } from 'svelte';
	import {
		buildAuthoringYamlSnippet,
		emptyAuthoringSelection,
		fetchAuthoringAgents,
		fetchAuthoringPersonalities,
		fetchAuthoringProcedures,
		fetchAuthoringTools,
		partitionAuthoringToolsForApps,
		type AuthoringAgentEntry,
		type AuthoringPersonalityEntry,
		type AuthoringProcedureEntry,
		type AuthoringSelection,
		type AuthoringToolEntry
	} from '$lib/apps/authoringCatalog';

	export let open = false;

	const dispatch = createEventDispatcher<{
		close: void;
		insert: { text: string };
	}>();

	type Tab = 'tools' | 'agents' | 'personalities' | 'procedures';

	let tab: Tab = 'tools';
	let appEligibleOnly = true;
	let loading = false;
	let catalogErrors: Partial<Record<Tab, string>> = {};
	let query = '';
	let tools: AuthoringToolEntry[] = [];
	let agents: AuthoringAgentEntry[] = [];
	let personalities: AuthoringPersonalityEntry[] = [];
	let procedures: AuthoringProcedureEntry[] = [];
	let selection: AuthoringSelection = emptyAuthoringSelection();
	let controller: AbortController | null = null;
	$: error = catalogErrors[tab] ?? null;

	$: toolPartition = partitionAuthoringToolsForApps(filteredTools);
	$: blockedToolCount = toolPartition.blocked.reduce((total, group) => total + group.tools.length, 0);
	$: filteredTools = filterByQuery(
		tools.filter((entry) => !appEligibleOnly || entry.app_eligible),
		query,
		(entry) => `${entry.name} ${entry.description}`
	);
	$: filteredAgents = filterByQuery(agents, query, (entry) => `${entry.name} ${entry.description}`);
	$: filteredPersonalities = filterByQuery(
		personalities,
		query,
		(entry) => `${entry.name} ${entry.description}`
	);
	$: filteredProcedures = filterByQuery(
		procedures,
		query,
		(entry) => `${entry.name} ${entry.description}`
	);
	$: snippet = buildAuthoringYamlSnippet(selection, { tools, agents, personalities, procedures });
	$: selectedCount =
		selection.tools.length +
		selection.agents.length +
		selection.personalities.length +
		selection.procedures.length;

	onMount(() => {
		void loadCatalog();
	});
	onDestroy(() => {
		controller?.abort();
	});

	async function loadCatalog(): Promise<void> {
		controller?.abort();
		controller = new AbortController();
		const signal = controller.signal;
		loading = true;
		catalogErrors = {};
		try {
			const [toolPage, agentPage, personalityPage, procedurePage] = await Promise.allSettled([
				fetchAuthoringTools({ signal }),
				fetchAuthoringAgents(signal),
				fetchAuthoringPersonalities(signal),
				fetchAuthoringProcedures(signal)
			]);
			if (signal.aborted) return;

			const nextErrors: Partial<Record<Tab, string>> = {};
			if (toolPage.status === 'fulfilled') {
				tools = toolPage.value.items;
				if (toolPage.value.status !== 'ok') {
					nextErrors.tools = catalogStatusMessage('tool', toolPage.value.status);
				}
			} else {
				tools = [];
				nextErrors.tools = catalogFailureMessage(toolPage.reason);
			}
			if (agentPage.status === 'fulfilled') {
				agents = agentPage.value.items;
				if (agentPage.value.status !== 'ok') {
					nextErrors.agents = catalogStatusMessage('agent', agentPage.value.status);
				}
			} else {
				agents = [];
				nextErrors.agents = catalogFailureMessage(agentPage.reason);
			}
			if (personalityPage.status === 'fulfilled') {
				personalities = personalityPage.value.items;
				if (personalityPage.value.status !== 'ok') {
					nextErrors.personalities = catalogStatusMessage(
						'personality',
						personalityPage.value.status
					);
				}
			} else {
				personalities = [];
				nextErrors.personalities = catalogFailureMessage(personalityPage.reason);
			}
			if (procedurePage.status === 'fulfilled') {
				procedures = procedurePage.value.items;
				if (procedurePage.value.status !== 'ok') {
					nextErrors.procedures = catalogStatusMessage('procedure', procedurePage.value.status);
				}
			} else {
				procedures = [];
				nextErrors.procedures = catalogFailureMessage(procedurePage.reason);
			}
			catalogErrors = nextErrors;
		} finally {
			if (!signal.aborted) loading = false;
		}
	}

	function catalogStatusMessage(kind: string, status: 'degraded' | 'unavailable'): string {
		return status === 'degraded'
			? `Some ${kind} sources could not be loaded. Available entries are shown.`
			: `The ${kind} catalog is unavailable.`;
	}

	function catalogFailureMessage(reason: unknown): string {
		return reason instanceof Error ? reason.message : 'The authoring catalog could not be loaded.';
	}

	function filterByQuery<T>(items: T[], raw: string, haystack: (item: T) => string): T[] {
		const needle = raw.trim().toLowerCase();
		if (!needle) return items;
		return items.filter((item) => haystack(item).toLowerCase().includes(needle));
	}

	function toggle(list: string[], name: string): string[] {
		return list.includes(name) ? list.filter((item) => item !== name) : [...list, name];
	}

	function insertSelection(): void {
		if (!snippet) return;
		dispatch('insert', { text: snippet });
	}

	function close(): void {
		dispatch('close');
	}
</script>

{#snippet toolRow(entry: AuthoringToolEntry, showReason: boolean)}
	<li>
		<label>
			<input
				type="checkbox"
				checked={selection.tools.includes(entry.name)}
				disabled={!entry.app_eligible}
				on:change={() => {
					if (!entry.app_eligible) return;
					selection = { ...selection, tools: toggle(selection.tools, entry.name) };
				}}
			/>
			<span class="app-options__name">{entry.name}</span>
			<span class="app-options__kind">{entry.kind}</span>
			{#if !entry.app_eligible}
				<span class="app-options__badge">not eligible</span>
			{/if}
			{#if entry.app_eligible && entry.dispatchable === false}
				<span class="app-options__badge">not dispatchable yet</span>
			{/if}
			{#if entry.app_eligible && entry.lock_review_required}
				<span class="app-options__badge">review verified at pack</span>
			{/if}
		</label>
		<p>{entry.description}</p>
		{#if showReason && entry.ineligible_reason}
			<p class="app-options__reason">{entry.ineligible_reason}</p>
		{/if}
	</li>
{/snippet}

{#if open}
	<!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
	<div
		class="app-options-overlay"
		role="dialog"
		aria-modal="true"
		aria-label="App options"
		tabindex="-1"
		on:click|self={close}
	>
		<div class="app-options">
			<header class="app-options__head">
				<div>
					<h2>App options</h2>
					<p>Same catalog as <code>magician app tools list</code>. Pick names; the YAML is written from them.</p>
				</div>
				<button type="button" class="app-options__close" on:click={close} aria-label="Close">✕</button>
			</header>

			<div class="app-options__tabs" role="tablist">
				<button type="button" class:active={tab === 'tools'} on:click={() => (tab = 'tools')}>Tools</button>
				<button type="button" class:active={tab === 'agents'} on:click={() => (tab = 'agents')}>Agents</button>
				<button type="button" class:active={tab === 'personalities'} on:click={() => (tab = 'personalities')}
					>Personalities</button
				>
				<button type="button" class:active={tab === 'procedures'} on:click={() => (tab = 'procedures')}
					>Procedures</button
				>
			</div>

			<div class="app-options__filters">
				<input type="search" bind:value={query} placeholder="Filter by name…" aria-label="Filter catalog" />
				{#if tab === 'tools'}
					<label>
						<input type="checkbox" bind:checked={appEligibleOnly} />
						App-compatible shape only
					</label>
				{/if}
			</div>

			{#if loading}
				<p class="app-options__status">Loading catalog…</p>
			{:else}
				{#if error}
					<p class="app-options__status app-options__status--error">{error}</p>
					<button type="button" class="app-options__retry" on:click={() => void loadCatalog()}>Retry</button>
				{/if}
				<ul class="app-options__list">
					{#if tab === 'tools'}
						{#if toolPartition.ready.length > 0}
							<li class="app-options__section">Ready in apps ({toolPartition.ready.length})</li>
						{/if}
						{#each toolPartition.ready as entry (entry.name)}
							{@render toolRow(entry, true)}
						{/each}
						{#if toolPartition.blocked.length > 0}
							<li class="app-options__blocked">
								<details>
									<summary>
										Not yet runnable in apps ({blockedToolCount}) — grouped by why
									</summary>
									{#each toolPartition.blocked as group (group.reason)}
										<details class="app-options__group">
											<summary>
												<span class="app-options__reason">{group.reason}</span>
												<span class="app-options__count">{group.tools.length}</span>
											</summary>
											<ul class="app-options__list">
												{#each group.tools as entry (entry.name)}
													{@render toolRow(entry, false)}
												{/each}
											</ul>
										</details>
									{/each}
								</details>
							</li>
						{/if}
					{:else if tab === 'agents'}
						{#each filteredAgents as entry (entry.name)}
							<li>
								<label>
									<input
										type="checkbox"
										checked={selection.agents.includes(entry.name)}
										on:change={() =>
											(selection = { ...selection, agents: toggle(selection.agents, entry.name) })}
									/>
									<span class="app-options__name">{entry.display_name || entry.name}</span>
									{#if entry.default_runner}
										<span class="app-options__badge app-options__badge--ok">default</span>
									{/if}
								</label>
								<p>{entry.description}</p>
							</li>
						{/each}
					{:else if tab === 'personalities'}
						{#each filteredPersonalities as entry (entry.name)}
							<li>
								<label>
									<input
										type="checkbox"
										checked={selection.personalities.includes(entry.name)}
										on:change={() =>
											(selection = {
												...selection,
												personalities: toggle(selection.personalities, entry.name)
											})}
									/>
									<span class="app-options__name">{entry.name}</span>
								</label>
								<p>{entry.description}</p>
							</li>
						{/each}
					{:else}
						{#each filteredProcedures as entry (entry.name)}
							<li>
								<label>
									<input
										type="checkbox"
										checked={selection.procedures.includes(entry.name)}
										on:change={() =>
											(selection = {
												...selection,
												procedures: toggle(selection.procedures, entry.name)
											})}
									/>
									<span class="app-options__name">{entry.name}</span>
								</label>
								<p>{entry.description}</p>
							</li>
						{/each}
					{/if}
				</ul>
			{/if}

			<footer class="app-options__foot">
				<span>{selectedCount} selected</span>
				<button type="button" class="vbtn" disabled={!snippet} on:click={insertSelection}>
					Insert YAML into prompt
				</button>
			</footer>
		</div>
	</div>
{/if}

<style>
	.app-options-overlay {
		position: fixed;
		inset: 0;
		z-index: 60;
		display: grid;
		place-items: center;
		padding: 1.5rem;
		background: color-mix(in srgb, var(--bg-surface, #000) 68%, transparent);
	}
	.app-options {
		width: min(720px, 100%);
		max-height: min(88vh, 52rem);
		overflow: hidden;
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
		padding: 1.15rem;
		border-radius: 14px;
		border: 1px solid var(--vibe-border-strong, var(--vibe-border));
		background: var(--vibe-surface);
		color: var(--vibe-text);
		box-shadow: 0 24px 60px rgba(0, 0, 0, 0.35);
	}
	.app-options__head {
		display: flex;
		justify-content: space-between;
		gap: 1rem;
	}
	.app-options__head h2 {
		margin: 0;
		font-size: 1.05rem;
	}
	.app-options__head p {
		margin: 0.25rem 0 0;
		font-size: 0.8rem;
		opacity: 0.78;
	}
	.app-options__close {
		border: 0;
		background: transparent;
		color: inherit;
		cursor: pointer;
	}
	.app-options__tabs {
		display: flex;
		gap: 0.35rem;
		flex-wrap: wrap;
	}
	.app-options__tabs button {
		border: 1px solid var(--vibe-border, #333);
		background: transparent;
		color: inherit;
		border-radius: 999px;
		padding: 0.25rem 0.7rem;
		font-size: 0.78rem;
		cursor: pointer;
	}
	.app-options__tabs button.active {
		background: var(--vibe-text);
		color: var(--vibe-surface);
	}
	.app-options__filters {
		display: flex;
		gap: 0.75rem;
		align-items: center;
	}
	.app-options__filters input[type='search'] {
		flex: 1;
		border: 1px solid var(--vibe-border, #333);
		background: transparent;
		color: inherit;
		border-radius: 8px;
		padding: 0.4rem 0.6rem;
	}
	.app-options__list {
		margin: 0;
		padding: 0;
		list-style: none;
		overflow: auto;
		min-height: 12rem;
		display: flex;
		flex-direction: column;
		gap: 0.65rem;
	}
	.app-options__list label {
		display: flex;
		align-items: center;
		gap: 0.45rem;
	}
	.app-options__name {
		font-weight: 650;
		font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
		font-size: 0.82rem;
	}
	.app-options__kind,
	.app-options__badge {
		font-size: 0.72rem;
		opacity: 0.78;
	}
	.app-options__badge--ok {
		opacity: 1;
	}
	.app-options__list p {
		margin: 0.15rem 0 0 1.4rem;
		font-size: 0.78rem;
		opacity: 0.8;
	}
	.app-options__section {
		padding: 0.35rem 0.1rem 0.15rem;
		font-size: 0.72rem;
		font-weight: 800;
		letter-spacing: 0.04em;
		text-transform: uppercase;
		color: var(--text-muted, currentColor);
	}
	.app-options__blocked > details > summary,
	.app-options__group > summary {
		cursor: pointer;
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		gap: 0.75rem;
		padding: 0.45rem 0.1rem;
		font-weight: 700;
	}
	.app-options__group {
		margin-left: 0.75rem;
		border-top: 1px solid var(--border-soft, rgba(127, 127, 127, 0.25));
	}
	.app-options__group > summary {
		font-weight: 500;
	}
	.app-options__count {
		flex: none;
		font-variant-numeric: tabular-nums;
		color: var(--text-muted, currentColor);
	}
	.app-options__reason {
		color: color-mix(in srgb, var(--vibe-danger, #c45b5b) 92%, transparent);
	}
	.app-options__status {
		margin: 0;
		font-size: 0.85rem;
	}
	.app-options__status--error {
		color: #c45b5b;
	}
	.app-options__foot {
		display: flex;
		justify-content: space-between;
		align-items: center;
		gap: 0.75rem;
	}
	.app-options__retry,
	.vbtn {
		border: 1px solid var(--vibe-border-strong, var(--vibe-border));
		background: var(--vibe-text);
		color: var(--vibe-surface);
		border-radius: 8px;
		padding: 0.4rem 0.75rem;
		cursor: pointer;
	}
	.vbtn:disabled {
		opacity: 0.45;
		cursor: not-allowed;
	}
</style>
