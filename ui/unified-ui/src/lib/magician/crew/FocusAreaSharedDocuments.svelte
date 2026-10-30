<script lang="ts">
	import { createEventDispatcher, onDestroy, onMount } from 'svelte';
	import { listPrograms, type ProgramSummary } from '$lib/work/programs';
	import {
		setSharedDocsMode,
		sharedDocumentOptions,
		toggleSharedDocument,
		type FocusAreaSharedDocs,
		type SharedDocsMode
	} from './focusAreaSharedPrograms';

	interface SharedDocsArea {
		name: string;
		docs: FocusAreaSharedDocs;
		/** Names stored before this edit; they stay listed after being unchecked. */
		retained: string[];
	}

	export let areas: SharedDocsArea[] = [];
	export let disabled = false;
	/** Reloads the program list when the principal/workspace changes. */
	export let scopeKey = '';

	const dispatch = createEventDispatcher<{ change: { index: number; docs: FocusAreaSharedDocs } }>();
	const MODES: Array<{ value: SharedDocsMode; label: string; hint: string }> = [
		{ value: 'global', label: 'Use global list', hint: 'Follows the shared documents in autopilot settings.' },
		{ value: 'none', label: 'None', hint: 'This area gets no shared documents.' },
		{ value: 'choose', label: 'Choose documents', hint: 'These documents replace the global list for this area.' }
	];

	let programs: ProgramSummary[] | null = null;
	let loading = false;
	let loadError: string | null = null;
	let mounted = false;
	let loadedScope: string | null = null;
	let controller: AbortController | null = null;

	$: if (mounted && scopeKey !== loadedScope) void load();

	async function load(): Promise<void> {
		loadedScope = scopeKey;
		controller?.abort();
		const request = new AbortController();
		controller = request;
		loading = true;
		loadError = null;
		programs = null;
		try {
			const result = await listPrograms(request.signal);
			if (controller !== request) return;
			programs = result;
		} catch (error) {
			if (controller !== request || request.signal.aborted) return;
			loadError = error instanceof Error ? error.message : 'Could not load playbook documents.';
		} finally {
			if (controller === request) loading = false;
		}
	}

	function selectMode(index: number, mode: SharedDocsMode): void {
		const area = areas[index];
		if (!area) return;
		dispatch('change', { index, docs: setSharedDocsMode(area.docs, mode) });
	}

	function toggle(index: number, name: string, event: Event): void {
		const area = areas[index];
		if (!area) return;
		const checked = (event.currentTarget as HTMLInputElement).checked;
		dispatch('change', { index, docs: toggleSharedDocument(area.docs, name, checked) });
	}

	function areaLabel(area: SharedDocsArea, index: number): string {
		const name = area.name.trim();
		return name ? `Focus area ${index + 1}: ${name}` : `Focus area ${index + 1}`;
	}

	onMount(() => {
		mounted = true;
	});

	onDestroy(() => {
		mounted = false;
		controller?.abort();
		controller = null;
	});
</script>

<section class="shared-docs" aria-labelledby="shared-docs-title" data-component-id="presto-definition-shared-documents">
	<header class="shared-docs__header">
		<h3 id="shared-docs-title">Shared documents</h3>
		<p>
			Shared documents are appended to every cycle for a focus area, after its own program. The global
			list lives in harness settings (<code>harness.shared_program_docs</code>).
		</p>
	</header>

	{#if loading}
		<p class="shared-docs__status" role="status">Loading playbook documents…</p>
	{:else if loadError}
		<div class="shared-docs__status shared-docs__status--error" role="alert">
			<span>Couldn't load program documents: {loadError}. Other fields can still be edited and saved.</span>
			<button type="button" class="shared-docs__retry" on:click={() => void load()}>Retry</button>
		</div>
	{/if}

	{#each areas as area, index (index)}
		{@const options = sharedDocumentOptions(programs, [...area.retained, ...area.docs.selected])}
		<fieldset class="shared-docs__area" {disabled}>
			<legend>{areaLabel(area, index)}</legend>
			<div class="shared-docs__modes">
				{#each MODES as mode (mode.value)}
					<label class="shared-docs__option" class:selected={area.docs.mode === mode.value}>
						<input
							type="radio"
							name={`shared-docs-mode-${index}`}
							value={mode.value}
							checked={area.docs.mode === mode.value}
							on:change={() => selectMode(index, mode.value)}
						/>
						<span class="shared-docs__option-text">
							<strong>{mode.label}</strong>
							<small>{mode.hint}</small>
						</span>
					</label>
				{/each}
			</div>

			{#if area.docs.mode === 'choose'}
				<fieldset class="shared-docs__docs">
					<legend class="shared-docs__sr-only">Documents for {areaLabel(area, index)}</legend>
					{#if options.length === 0}
						<p class="shared-docs__note">
							{#if loading}Loading program documents…{:else if loadError}No program documents could be listed.{:else}No program documents yet. Create one in Work → Programs.{/if}
						</p>
					{:else}
						<div class="shared-docs__doc-list">
							{#each options as option (option.name)}
								<label class="shared-docs__doc" class:missing={option.status === 'missing'}>
									<input
										type="checkbox"
										checked={area.docs.selected.includes(option.name)}
										on:change={(event) => toggle(index, option.name, event)}
									/>
									<span class="shared-docs__doc-text">
										<strong>{option.title}</strong>
										<code>{option.name}</code>
									</span>
									{#if option.status === 'missing'}
										<span class="shared-docs__badge">not found</span>
									{/if}
								</label>
							{/each}
						</div>
					{/if}
					{#if area.docs.selected.length === 0}
						<p class="shared-docs__note shared-docs__note--warning">
							No documents chosen — this area gets no shared documents.
						</p>
					{/if}
				</fieldset>
			{/if}
		</fieldset>
	{/each}
</section>

<style>
	.shared-docs {
		display: grid;
		gap: var(--space-md, 1rem);
		width: 100%;
		min-width: 0;
		box-sizing: border-box;
		margin-top: var(--space-md, 1rem);
		padding: var(--space-md, 1rem);
		border: 1px solid var(--border-soft, #ebe7e0);
		border-radius: var(--radius-md, 8px);
		background: var(--bg-card, #fff);
		box-shadow: var(--shadow-sm, 0 1px 3px rgba(0, 0, 0, 0.08));
		color: var(--text-primary, #2d2a26);
	}

	.shared-docs__header {
		display: grid;
		gap: var(--space-xs, 0.25rem);
	}

	.shared-docs__header h3 {
		margin: 0;
		font: 700 var(--text-sm, 0.85rem) var(--font-display, inherit);
		color: var(--text-primary, #2d2a26);
	}

	.shared-docs__header p,
	.shared-docs__note,
	.shared-docs__status {
		margin: 0;
		font-size: var(--text-xs, 0.78rem);
		line-height: var(--leading-normal, 1.5);
		color: var(--text-secondary, #6b665e);
	}

	.shared-docs code {
		font: var(--text-xs, 0.78rem) var(--font-mono, monospace);
		overflow-wrap: anywhere;
	}

	.shared-docs__status--error {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		justify-content: space-between;
		gap: var(--space-sm, 0.5rem);
		padding: var(--space-sm, 0.5rem) var(--space-md, 1rem);
		border: 1px solid color-mix(in srgb, var(--color-error, #ff6b6b) 32%, transparent);
		border-radius: var(--radius-md, 8px);
		background: var(--color-error-soft, rgba(255, 107, 107, 0.14));
		color: var(--text-primary, #2d2a26);
	}

	.shared-docs__retry {
		min-height: 44px;
		padding: 0 var(--space-md, 1rem);
		border: 1px solid var(--border-soft, #d8d0c5);
		border-radius: var(--radius-md, 8px);
		background: var(--bg-card, #fff);
		color: var(--text-primary, #2d2a26);
		font: 700 var(--text-xs, 0.78rem) var(--font-primary, inherit);
		cursor: pointer;
	}

	.shared-docs__area {
		display: grid;
		gap: var(--space-sm, 0.5rem);
		min-width: 0;
		margin: 0;
		padding: var(--space-sm, 0.5rem) var(--space-md, 1rem) var(--space-md, 1rem);
		border: 1px solid var(--border-soft, #ebe7e0);
		border-radius: var(--radius-md, 8px);
	}

	.shared-docs__area > legend {
		padding: 0 var(--space-xs, 0.25rem);
		font-size: var(--text-xs, 0.78rem);
		font-weight: 700;
		color: var(--text-primary, #2d2a26);
		overflow-wrap: anywhere;
	}

	.shared-docs__area:disabled {
		opacity: 0.6;
	}

	.shared-docs__modes {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(min(100%, 12rem), 1fr));
		gap: var(--space-sm, 0.5rem);
	}

	.shared-docs__option,
	.shared-docs__doc {
		display: flex;
		align-items: center;
		gap: var(--space-sm, 0.5rem);
		min-height: 44px;
		min-width: 0;
		box-sizing: border-box;
		padding: var(--space-xs, 0.25rem) var(--space-sm, 0.5rem);
		border: 1px solid var(--border-soft, #d8d0c5);
		border-radius: var(--radius-sm, 6px);
		background: color-mix(in srgb, var(--bg-card, #fff) 88%, var(--bg-soft, #f6f1e8));
		cursor: pointer;
	}

	.shared-docs__option.selected {
		border-color: var(--accent-primary, #ff6b6b);
		background: color-mix(in srgb, var(--accent-primary, #ff6b6b) 10%, var(--bg-card, #fff));
	}

	.shared-docs__area:disabled .shared-docs__option,
	.shared-docs__area:disabled .shared-docs__doc {
		cursor: default;
	}

	.shared-docs input[type='radio'],
	.shared-docs input[type='checkbox'] {
		flex: 0 0 auto;
		width: 1.05rem;
		height: 1.05rem;
		margin: 0;
		accent-color: var(--accent-primary, #ff6b6b);
		cursor: inherit;
	}

	.shared-docs input:focus-visible,
	.shared-docs__retry:focus-visible {
		outline: 2px solid var(--accent-primary, #ff6b6b);
		outline-offset: 2px;
	}

	.shared-docs__option-text,
	.shared-docs__doc-text {
		display: grid;
		gap: 0.1rem;
		min-width: 0;
	}

	.shared-docs__option-text strong,
	.shared-docs__doc-text strong {
		font-size: var(--text-sm, 0.85rem);
		font-weight: 700;
		color: var(--text-primary, #2d2a26);
		overflow-wrap: anywhere;
	}

	.shared-docs__option-text small,
	.shared-docs__doc-text code {
		font-size: var(--text-xs, 0.78rem);
		color: var(--text-secondary, #6b665e);
	}

	.shared-docs__docs {
		display: grid;
		gap: var(--space-sm, 0.5rem);
		min-width: 0;
		margin: 0;
		padding: 0;
		border: 0;
	}

	.shared-docs__doc-list {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(min(100%, 16rem), 1fr));
		gap: var(--space-sm, 0.5rem);
	}

	.shared-docs__doc.missing {
		border-style: dashed;
	}

	.shared-docs__badge {
		margin-left: auto;
		flex: 0 0 auto;
		padding: 0.1rem var(--space-sm, 0.5rem);
		border-radius: var(--radius-full, 999px);
		background: var(--color-warning-soft, rgba(255, 230, 109, 0.22));
		color: color-mix(in srgb, var(--color-warning, #ffe66d) 55%, var(--text-primary, #2d2a26));
		font-size: var(--text-2xs, 0.72rem);
		font-weight: 700;
	}

	.shared-docs__note--warning {
		color: color-mix(in srgb, var(--color-warning, #ffe66d) 45%, var(--text-primary, #2d2a26));
	}

	.shared-docs__sr-only {
		position: absolute;
		width: 1px;
		height: 1px;
		padding: 0;
		margin: -1px;
		overflow: hidden;
		clip: rect(0, 0, 0, 0);
		white-space: nowrap;
		border: 0;
	}
</style>
