<script lang="ts">
	import type { MemoryEntry, MemoryScopeDraft } from './memoryEntry';

	export let entry: MemoryEntry;
	export let confirming = false;
	export let savingScope = false;
	export let onConfirm: ((entry: MemoryEntry) => void | Promise<void>) | null = null;
	export let onSaveScope:
		| ((entry: MemoryEntry, scope: MemoryScopeDraft | null) => void | Promise<void>)
		| null = null;
	export let onKeepConflict: ((entry: MemoryEntry) => void | Promise<void>) | null = null;

	const CONFIRMABLE_TIERS = new Set(['preferences', 'research_findings']);

	let topicsDraft = '';
	let entitiesDraft = '';
	let appliesDraft = '';
	let lastEntryId = '';
	let lastScopeFingerprint = '';
	let topicsInput: HTMLInputElement | null = null;
	let dismissedConflict = false;

	$: entryId = `${entry.tier}:${entry.key}`;
	$: scopeFingerprint = JSON.stringify(entry.scope ?? null);
	$: if (entryId !== lastEntryId || scopeFingerprint !== lastScopeFingerprint) {
		lastEntryId = entryId;
		lastScopeFingerprint = scopeFingerprint;
		topicsDraft = (entry.scope?.topics ?? []).join(', ');
		entitiesDraft = (entry.scope?.entities ?? []).join(', ');
		appliesDraft = (entry.scope?.applies_to ?? []).join(', ');
		dismissedConflict = false;
	}

	$: trustLabel =
		entry.trust === 'stated' ? 'Stated' : entry.trust === 'untrusted' ? 'Untrusted' : 'Inferred';
	$: canConfirm =
		CONFIRMABLE_TIERS.has(entry.tier) &&
		entry.trust === 'inferred' &&
		typeof onConfirm === 'function';
	$: canEditScope = typeof onSaveScope === 'function';
	$: valueText =
		typeof entry.value === 'string'
			? entry.value
			: entry.value == null
				? ''
				: JSON.stringify(entry.value);
	$: hasAttachableScope =
		(entry.scope?.topics?.length ?? 0) > 0 || (entry.scope?.entities?.length ?? 0) > 0;
	$: scopeLabel = hasAttachableScope
		? [
				...(entry.scope?.topics ?? []),
				...(entry.scope?.entities ?? []),
				...(entry.scope?.applies_to ?? [])
			].join(', ')
		: 'No scope — will not attach';
	$: conflictLine = dismissedConflict ? '' : (entry.conflict ?? '').trim();
	$: hasConflict = conflictLine.length > 0;
	$: conflictOpenedAnyway = conflictRatioLabel(entry.conflict_agree, entry.conflict_disagree);

	function parseList(raw: string): string[] {
		const seen = new Set<string>();
		const out: string[] = [];
		for (const part of raw.split(',')) {
			const token = part.trim();
			if (!token || seen.has(token.toLowerCase())) continue;
			seen.add(token.toLowerCase());
			out.push(token);
		}
		return out;
	}

	function saveScope(): void {
		if (!onSaveScope) return;
		const topics = parseList(topicsDraft);
		const entities = parseList(entitiesDraft);
		const applies_to = parseList(appliesDraft);
		if (topics.length === 0 && entities.length === 0) {
			// Empty editor + existing server scope is a no-op so Confirm-then-Save
			// cannot wipe attachability. Clearing requires an explicit empty save
			// only when the server already has no attachable scope.
			if (hasAttachableScope) return;
			void onSaveScope(entry, null);
			return;
		}
		void onSaveScope(entry, { topics, entities, applies_to });
	}

	function conflictRatioLabel(agree: number | undefined, disagree: number | undefined): string {
		if (
			typeof agree !== 'number' ||
			typeof disagree !== 'number' ||
			!Number.isFinite(agree) ||
			!Number.isFinite(disagree) ||
			agree < 0 ||
			disagree < 0
		) {
			return '';
		}
		const total = agree + disagree;
		if (total <= 0) return '';
		return `${disagree}/${total} opened anyway`;
	}

	function editConflict(): void {
		topicsInput?.focus();
	}

	function keepConflict(): void {
		dismissedConflict = true;
		if (entry.trust === 'inferred' && typeof onConfirm === 'function') {
			void onConfirm(entry);
			return;
		}
		if (typeof onKeepConflict === 'function') {
			void onKeepConflict(entry);
		}
	}
</script>

<article class="memory-entry-card">
	<header>
		<strong>{entry.tier}: {entry.key}</strong>
		<span>{trustLabel}</span>
	</header>
	{#if valueText}
		<p>{valueText}</p>
	{/if}
	<p class="memory-entry-card__scope">{scopeLabel}</p>
	{#if hasConflict}
		<p class="memory-entry-card__conflict" role="status">{conflictLine}</p>
		{#if conflictOpenedAnyway}
			<p class="memory-entry-card__conflict-count">{conflictOpenedAnyway}</p>
		{/if}
		<div class="memory-entry-card__conflict-actions">
			<button type="button" on:click={editConflict}>Edit</button>
			<button type="button" disabled={confirming} on:click={keepConflict}>Keep</button>
		</div>
	{/if}
	{#if canEditScope}
		<div class="memory-entry-card__editor">
			<label>
				Topics
				<input bind:this={topicsInput} bind:value={topicsDraft} disabled={savingScope} />
			</label>
			<label>
				Entities
				<input bind:value={entitiesDraft} disabled={savingScope} />
			</label>
			<label>
				Applies to
				<input bind:value={appliesDraft} disabled={savingScope} placeholder="comm, web, task" />
			</label>
			<p class="memory-entry-card__hint">
				Comma-separated. Topics or entities are required before this memory can attach.
			</p>
			<button type="button" disabled={savingScope} on:click={saveScope}>Save scope</button>
		</div>
	{/if}
	{#if canConfirm}
		<button type="button" disabled={confirming} on:click={() => onConfirm?.(entry)}>
			Confirm
		</button>
	{/if}
</article>

<style>
	.memory-entry-card {
		display: grid;
		gap: 0.4rem;
		padding: 0.75rem 0;
		border-bottom: 1px solid color-mix(in srgb, currentColor 12%, transparent);
	}
	.memory-entry-card header {
		display: flex;
		justify-content: space-between;
		gap: 0.75rem;
	}
	.memory-entry-card p,
	.memory-entry-card__scope,
	.memory-entry-card__hint,
	.memory-entry-card__conflict,
	.memory-entry-card__conflict-count {
		margin: 0;
		opacity: 0.8;
	}
	.memory-entry-card__conflict,
	.memory-entry-card__conflict-count {
		opacity: 1;
	}
	.memory-entry-card__conflict-actions {
		display: flex;
		gap: 0.5rem;
	}
	.memory-entry-card__editor {
		display: grid;
		gap: 0.45rem;
	}
	.memory-entry-card__editor label {
		display: grid;
		gap: 0.2rem;
		font-size: 0.85rem;
	}
	.memory-entry-card__editor input {
		width: 100%;
	}
	.memory-entry-card__hint {
		font-size: 0.8rem;
	}
</style>
