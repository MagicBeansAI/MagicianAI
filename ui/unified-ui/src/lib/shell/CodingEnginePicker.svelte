<script lang="ts">
	import { createEventDispatcher, onDestroy, tick } from 'svelte';
	import {
		codingEngineLabel,
		codingEngineNote,
		codingGatewaySummary,
		groupCodingProfilesByEngine,
		type CodingProfile
	} from '$lib/stores/codingProfileStore';

	/** Selectable rows (Auto included by the caller when it applies). */
	export let profiles: CodingProfile[] = [];
	/** Rows the server reports as not selectable; shown with their reason. */
	export let blockedProfiles: CodingProfile[] = [];
	export let selectedProfileId: string | null = null;
	export let selectedProfile: CodingProfile | null = null;
	export let disabled = false;
	export let ariaLabel = 'Coding profile';
	/** A small chip for dense composers (the floating chat composer). */
	export let compact = false;

	const dispatch = createEventDispatcher<{ select: { id: string } }>();
	const listId = `coding-engine-list-${Math.random().toString(36).slice(2, 8)}`;

	let open = false;
	let root: HTMLDivElement;
	let listEl: HTMLDivElement;
	let activeId: string | null = null;

	$: groups = groupCodingProfilesByEngine([...profiles, ...blockedProfiles]);
	$: selectableIds = groups.flatMap((group) =>
		group.profiles.filter((profile) => profile.selectable !== false).map((profile) => profile.id)
	);
	$: triggerEngine = selectedProfile?.engine ? codingEngineLabel(selectedProfile.engine) : null;
	$: triggerText = selectedProfile
		? triggerEngine && triggerEngine !== selectedProfile.label
			? `${triggerEngine} · ${selectedProfile.label}`
			: selectedProfile.label
		: 'Choose engine';

	async function toggle(): Promise<void> {
		if (disabled) return;
		open = !open;
		if (open) {
			activeId = selectedProfileId && selectableIds.includes(selectedProfileId)
				? selectedProfileId
				: (selectableIds[0] ?? null);
			await tick();
			listEl?.focus();
		}
	}

	function close(): void {
		open = false;
	}

	function choose(profile: CodingProfile): void {
		if (profile.selectable === false) return;
		dispatch('select', { id: profile.id });
		close();
	}

	function move(step: number): void {
		if (selectableIds.length === 0) return;
		const index = activeId ? selectableIds.indexOf(activeId) : -1;
		activeId = selectableIds[(index + step + selectableIds.length) % selectableIds.length];
	}

	function onListKeydown(event: KeyboardEvent): void {
		if (event.key === 'ArrowDown') {
			event.preventDefault();
			move(1);
		} else if (event.key === 'ArrowUp') {
			event.preventDefault();
			move(-1);
		} else if (event.key === 'Enter' || event.key === ' ') {
			event.preventDefault();
			const row = [...profiles, ...blockedProfiles].find((profile) => profile.id === activeId);
			if (row) choose(row);
		} else if (event.key === 'Escape') {
			event.preventDefault();
			close();
		}
	}

	function onDocumentPointer(event: PointerEvent): void {
		if (open && root && !root.contains(event.target as Node)) close();
	}

	$: if (typeof document !== 'undefined') {
		if (open) document.addEventListener('pointerdown', onDocumentPointer);
		else document.removeEventListener('pointerdown', onDocumentPointer);
	}
	onDestroy(() => {
		if (typeof document !== 'undefined')
			document.removeEventListener('pointerdown', onDocumentPointer);
	});

	function rowDetail(profile: CodingProfile): string | null {
		if (profile.selectable === false) return profile.reason?.trim() || 'Unavailable';
		if (profile.gateway) return null;
		return profile.description ?? (profile.engine === 'pi' ? (profile.model ?? null) : null);
	}
</script>

<div class="engine-picker" class:is-compact={compact} bind:this={root}>
	<button
		type="button"
		class="engine-picker__trigger"
		aria-haspopup="listbox"
		aria-expanded={open}
		aria-controls={listId}
		aria-label={ariaLabel}
		title={selectedProfile?.description ?? selectedProfile?.model ?? 'Coding engine and profile'}
		{disabled}
		on:click={toggle}
	>
		{#if compact}
			<svg class="pp-icon" width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
				<path d="M12 3l1.9 4.9L19 10l-4.9 1.9L12 17l-1.9-5.1L5 10l5.1-2L12 3z" />
				<path d="M5 19l.7-1.7L7 16.5l-1.3-.8L5 14l-.7 1.7L3 16.5l1.3.8L5 19z" />
			</svg>
			<span class="pp-engine">{triggerText}</span>
			{#if selectedProfile?.gateway}
				<span class="engine-picker__flag" aria-label="Reaches services outside Magician">!</span>
			{/if}
			<svg class="pp-chevron" width="8" height="8" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true">
				<polyline points="6 9 12 15 18 9" />
			</svg>
		{:else}
			<span class="engine-picker__trigger-text">{triggerText}</span>
			{#if selectedProfile?.gateway}
				<span class="engine-picker__flag" aria-label="Reaches services outside Magician">!</span>
			{/if}
			<svg viewBox="0 0 24 24" width="12" height="12" aria-hidden="true"
				><path d="M6 9l6 6 6-6" fill="none" stroke="currentColor" stroke-width="2.4" /></svg
			>
		{/if}
	</button>

	{#if open}
		<!-- svelte-ignore a11y-no-noninteractive-tabindex -->
		<div
			class="engine-picker__panel"
			id={listId}
			role="listbox"
			aria-label="Coding engines and profiles"
			aria-activedescendant={activeId ? `${listId}-${activeId}` : undefined}
			tabindex="0"
			bind:this={listEl}
			on:keydown={onListKeydown}
		>
			{#each groups as group (group.label ?? '')}
				<div class="engine-picker__group" role="group" aria-label={group.label ?? 'Other'}>
					{#if group.label}
						<div class="engine-picker__group-head">
							<span class="engine-picker__group-name">{group.label}</span>
							{#if codingEngineNote(group.profiles[0]?.engine)}
								<span class="engine-picker__group-note">{codingEngineNote(group.profiles[0]?.engine)}</span>
							{/if}
						</div>
					{/if}
					{#each group.profiles as profile (profile.id)}
						{@const blocked = profile.selectable === false}
						{@const detail = rowDetail(profile)}
						<!-- svelte-ignore a11y-click-events-have-key-events -->
						<div
							class="engine-picker__row"
							class:is-active={activeId === profile.id}
							class:is-selected={selectedProfileId === profile.id}
							class:is-blocked={blocked}
							id={`${listId}-${profile.id}`}
							role="option"
							tabindex="-1"
							aria-selected={selectedProfileId === profile.id}
							aria-disabled={blocked}
							on:click={() => choose(profile)}
							on:pointerenter={() => {
								if (!blocked) activeId = profile.id;
							}}
						>
							<span class="engine-picker__row-label">
								{profile.label}
								{#if blocked}<span class="engine-picker__badge">unavailable</span>{/if}
							</span>
							{#if detail}
								<span class="engine-picker__row-detail">{detail}</span>
							{/if}
							{#if profile.gateway}
								<span class="engine-picker__row-warn">{codingGatewaySummary(profile.gateway)}</span>
							{/if}
						</div>
					{/each}
				</div>
			{/each}
		</div>
	{/if}
</div>

<style>
	.engine-picker {
		position: relative;
		display: inline-flex;
		min-width: min(12rem, 34vw);
		max-width: min(18rem, 42vw);
	}

	.engine-picker__trigger {
		display: inline-flex;
		align-items: center;
		gap: 0.35rem;
		width: 100%;
		min-width: 0;
		height: 1.74rem;
		padding: 0 0.5rem;
		border: 0;
		border-radius: 7px;
		background: var(--bg-soft, color-mix(in srgb, var(--vibe-text) 5%, transparent));
		color: var(--vibe-text);
		font: inherit;
		font-size: 0.76rem;
		font-weight: 800;
		cursor: pointer;
	}

	.engine-picker__trigger:disabled {
		cursor: not-allowed;
		opacity: 0.62;
	}

	.engine-picker__trigger:focus-visible,
	.engine-picker__panel:focus-visible {
		outline: 2px solid var(--vibe-accent, var(--accent, #6b8afd));
		outline-offset: 1px;
	}

	.engine-picker__trigger-text {
		flex: 1;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		text-align: left;
	}

	.engine-picker__flag {
		display: inline-grid;
		place-items: center;
		width: 0.95rem;
		height: 0.95rem;
		border-radius: 50%;
		background: var(--warning-soft, color-mix(in srgb, #d9a000 22%, transparent));
		color: var(--warning, #a07400);
		font-size: 0.66rem;
		font-weight: 900;
	}

	.engine-picker__panel {
		position: absolute;
		bottom: calc(100% + 6px);
		/* The picker sits at the right end of the composer row. */
		right: 0;
		z-index: 40;
		width: min(24rem, 86vw);
		max-height: min(26rem, 60vh);
		overflow-y: auto;
		padding: 0.3rem;
		border: 1px solid var(--vibe-border, var(--border, rgba(127, 127, 127, 0.3)));
		border-radius: 10px;
		background: var(--vibe-surface, var(--bg, #fff));
		box-shadow: 0 10px 30px rgba(0, 0, 0, 0.18);
		outline: none;
	}

	.engine-picker__group + .engine-picker__group {
		margin-top: 0.25rem;
		padding-top: 0.25rem;
		border-top: 1px solid var(--vibe-border, rgba(127, 127, 127, 0.2));
	}

	.engine-picker__group-head {
		display: flex;
		flex-direction: column;
		gap: 0.05rem;
		padding: 0.3rem 0.45rem 0.2rem;
	}

	.engine-picker__group-name {
		color: var(--vibe-text);
		font-size: 0.7rem;
		font-weight: 900;
		letter-spacing: 0.04em;
		text-transform: uppercase;
	}

	.engine-picker__group-note,
	.engine-picker__row-detail {
		color: var(--vibe-text-muted);
		font-size: 0.7rem;
		font-weight: 600;
		line-height: 1.3;
	}

	.engine-picker__row {
		display: flex;
		flex-direction: column;
		gap: 0.12rem;
		padding: 0.38rem 0.45rem;
		border-radius: 7px;
		color: var(--vibe-text);
		font-size: 0.78rem;
		font-weight: 750;
		cursor: pointer;
	}

	.engine-picker__row.is-active {
		background: var(--bg-soft, color-mix(in srgb, var(--vibe-text) 7%, transparent));
	}

	.engine-picker__row.is-selected .engine-picker__row-label::after {
		content: ' ✓';
		color: var(--vibe-accent, var(--accent, #6b8afd));
	}

	.engine-picker__row.is-blocked {
		cursor: not-allowed;
		opacity: 0.72;
	}

	.engine-picker__row-label {
		display: inline-flex;
		align-items: center;
		gap: 0.35rem;
	}

	.engine-picker__badge {
		padding: 0.02rem 0.32rem;
		border-radius: 999px;
		background: color-mix(in srgb, var(--vibe-text) 10%, transparent);
		color: var(--vibe-text-muted);
		font-size: 0.62rem;
		font-weight: 800;
	}

	.engine-picker__row-warn {
		color: var(--warning, #a07400);
		font-size: 0.7rem;
		font-weight: 650;
		line-height: 1.3;
	}

	.engine-picker.is-compact {
		min-width: 0;
		max-width: 18rem;
	}

	.engine-picker.is-compact .engine-picker__trigger {
		position: relative;
		display: inline-flex;
		align-items: center;
		gap: 4px;
		height: auto;
		padding: 2px 7px;
		border-radius: 5px;
		font-family: var(--font-primary);
		font-size: 9.5px;
		color: var(--text-secondary, #444);
		background: transparent;
		border: 0;
		cursor: pointer;
		transition: background 0.15s ease;
		min-width: 0;
		overflow: visible;
	}

	.engine-picker.is-compact .engine-picker__trigger:hover:not(:disabled) {
		background: var(--bg-soft, rgba(0, 0, 0, 0.05));
	}

	.engine-picker.is-compact .pp-icon {
		flex-shrink: 0;
		color: var(--accent-primary, #c2502a);
		width: 10px;
		height: 10px;
	}

	.engine-picker.is-compact .pp-engine {
		font-weight: 650;
		white-space: nowrap;
	}

	.engine-picker.is-compact .pp-chevron {
		flex-shrink: 0;
		width: 8px;
		height: 8px;
	}

	@media (max-width: 640px) {
		.engine-picker {
			max-width: 100%;
		}
	}
</style>
