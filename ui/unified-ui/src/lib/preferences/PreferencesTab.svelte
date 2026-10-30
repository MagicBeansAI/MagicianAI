<script lang="ts">
	/**
	 * Memory → Preferences: the owner's preferences exactly as preference
	 * checks see them — rules, goals and tastes, how strong the evidence is,
	 * where each came from and when a check last used it — with the owner's
	 * own edits and each preference's history.
	 *
	 * "Review what I've learned" narrows it to the preferences Magician
	 * inferred, a page at a time, to confirm or dismiss.
	 *
	 * Edits name the revision on screen. When a preference changed in the
	 * meantime the screen reloads and says so rather than overwriting it.
	 */
	import { onMount } from 'svelte';

	import { hasHydratedScopeBearer } from '$lib/stores/scopeIdentityStore';

	import PreferenceReview from './PreferenceReview.svelte';
	import PreferenceRow from './PreferenceRow.svelte';
	import { isLearned } from './preferenceReview';
	import {
		addPreference,
		confirmPreference,
		editPreference,
		fetchPreferenceView,
		forgetPreference,
		retirePreference,
		type EditOutcome,
		type IntentionKind,
		type Preference,
		type PreferenceView
	} from './preferencesApi';

	const GROUPS: Array<{ kind: IntentionKind; title: string; empty: string }> = [
		{
			kind: 'standing_instruction',
			title: 'Rules you set',
			empty: 'No rules yet. "Check with me before…" instructions appear here.'
		},
		{ kind: 'goal', title: 'Goals', empty: 'No goals yet.' },
		{ kind: 'taste', title: 'Tastes', empty: 'No tastes yet.' }
	];

	const OMITTED_WORDS: Record<string, string> = {
		unknown_lifecycle: 'its state is not one Magician recognises',
		app_governed: 'an app manages it',
		unreadable_validity: 'its dates cannot be read',
		duplicate_id: 'another preference has the same id',
		unreadable_tier: 'its section of memory cannot be read',
		unreadable: 'it cannot be read'
	};

	let view: PreferenceView | null = null;
	let error = '';
	let notice = '';
	let busy = false;
	let mode: 'all' | 'review' = 'all';

	let newKind: IntentionKind = 'standing_instruction';
	let newWording = '';
	let newLabel = '';
	let newConditions = '';

	async function load(): Promise<void> {
		try {
			view = await fetchPreferenceView();
			error = '';
		} catch (cause) {
			error = cause instanceof Error ? cause.message : String(cause);
		}
	}

	async function apply(change: () => Promise<EditOutcome>, done: string): Promise<void> {
		busy = true;
		notice = '';
		error = '';
		try {
			const outcome = await change();
			notice =
				outcome.status === 'stale'
					? `${outcome.message} The current version is shown.`
					: done;
		} catch (cause) {
			error = cause instanceof Error ? cause.message : String(cause);
		} finally {
			await load();
			busy = false;
		}
	}

	function add(): void {
		const wording = newWording.trim();
		if (!wording) return;
		void apply(
			() =>
				addPreference({
					kind: newKind,
					wording,
					label: newLabel.trim(),
					conditions: newConditions.trim()
				}),
			'Added. The next check uses it.'
		);
		newWording = '';
		newLabel = '';
		newConditions = '';
	}

	const onEdit = (event: CustomEvent<{ preference: Preference; wording: string }>) =>
		apply(
			() => editPreference(event.detail.preference.id, event.detail.preference.revision, event.detail.wording),
			'Reworded. The old wording is kept in its history.'
		);
	const onConfirm = (event: CustomEvent<Preference>) =>
		apply(() => confirmPreference(event.detail.id, event.detail.revision), 'Confirmed.');
	const onRetire = (event: CustomEvent<Preference>) =>
		apply(
			() => retirePreference(event.detail.id, event.detail.revision),
			'Retired. Checks no longer use it.'
		);
	const onDismiss = (event: CustomEvent<Preference>) =>
		apply(
			() => retirePreference(event.detail.id, event.detail.revision),
			'Dismissed. Checks no longer use it; its history is kept.'
		);
	const onForget = (event: CustomEvent<Preference>) =>
		apply(
			() => forgetPreference(event.detail.id, event.detail.revision),
			'Forgotten, and its wording erased from the history.'
		);

	onMount(() => {
		void (async () => {
			if (!(await hasHydratedScopeBearer())) {
				error = 'Sign in as the owner to see your preferences.';
				return;
			}
			await load();
		})();
	});

	$: learnedCount = (view?.preferences ?? []).filter(isLearned).length;
	$: grouped = GROUPS.map((group) => ({
		...group,
		items: (view?.preferences ?? []).filter((preference) => preference.kind === group.kind)
	}));
</script>

<section class="preferences" data-testid="preferences-tab">
	<h3>Your preferences</h3>
	<p class="muted">
		What Magician checks an action against before it sends, books, orders or buys for you.
	</p>

	{#if error}
		<p role="alert">{error}</p>
	{/if}
	{#if notice}
		<p class="notice" data-testid="preferences-notice">{notice}</p>
	{/if}

	{#if view}
		{#if view.coverage !== 'complete'}
			<p class="muted">Some preferences could not be read; checks treat them with caution.</p>
		{/if}

		<div class="modes" role="tablist" aria-label="Which preferences">
			<button
				type="button"
				role="tab"
				aria-selected={mode === 'all'}
				class:active={mode === 'all'}
				on:click={() => (mode = 'all')}>All preferences</button
			>
			<button
				type="button"
				role="tab"
				aria-selected={mode === 'review'}
				class:active={mode === 'review'}
				data-testid="preferences-review-mode"
				on:click={() => (mode = 'review')}>Review what I've learned ({learnedCount})</button
			>
		</div>

		{#if mode === 'review'}
			<PreferenceReview
				preferences={view.preferences}
				{busy}
				on:confirm={onConfirm}
				on:dismiss={onDismiss}
			/>
		{:else}
			{#each grouped as group (group.kind)}
				<h4>{group.title}</h4>
				{#if group.items.length === 0}
					<p class="muted">{group.empty}</p>
				{:else}
					<ul data-testid={`preferences-${group.kind}`}>
						{#each group.items as preference (preference.id)}
							<PreferenceRow
								{preference}
								{busy}
								on:edit={onEdit}
								on:confirm={onConfirm}
								on:retire={onRetire}
								on:forget={onForget}
							/>
						{/each}
					</ul>
				{/if}
			{/each}

			<form class="add" on:submit|preventDefault={add} data-testid="preferences-add">
				<h4>Add a preference</h4>
				<label>
					<span class="muted">Kind</span>
					<select bind:value={newKind}>
						<option value="standing_instruction">A rule ("check with me before…")</option>
						<option value="goal">A goal</option>
						<option value="taste">A taste</option>
					</select>
				</label>
				<label>
					<span class="muted">In your words</span>
					<textarea bind:value={newWording} rows="2" data-testid="preferences-add-wording"></textarea>
				</label>
				<label>
					<span class="muted">Short label (optional)</span>
					<input bind:value={newLabel} />
				</label>
				<label>
					<span class="muted">Only when (optional)</span>
					<input bind:value={newConditions} placeholder="e.g. ordering for guests" />
				</label>
				<button type="submit" class="primary" disabled={busy || !newWording.trim()}>Add</button>
			</form>

			{#if view.retired.length > 0}
				<details>
					<summary>Retired ({view.retired.length})</summary>
					<ul class="plain">
						{#each view.retired as retired (retired.id)}
							<li>
								<span class="struck">{retired.statement}</span>
								<span class="muted">
									· {retired.lifecycle.replace(/_/g, ' ')}{#if retired.supersededBy}, replaced{/if}
								</span>
							</li>
						{/each}
					</ul>
				</details>
			{/if}

			{#if view.omitted.length > 0}
				<details data-testid="preferences-omitted">
					<summary>Not used by checks ({view.omitted.length})</summary>
					<ul class="plain">
						{#each view.omitted as omitted (omitted.id)}
							<li>
								<code>{omitted.id}</code>
								<span class="muted">— {OMITTED_WORDS[omitted.reason] ?? omitted.reason}</span>
							</li>
						{/each}
					</ul>
				</details>
			{/if}
		{/if}
	{:else if !error}
		<p class="muted">Loading your preferences…</p>
	{/if}
</section>

<style>
	.preferences {
		display: grid;
		gap: 0.5rem;
	}
	h3 {
		margin: 0 0 0.1rem;
		font: 700 var(--text-lg, 1.1rem) var(--font-display, inherit);
	}
	h4 {
		margin: 0.8rem 0 0.2rem;
		font: 700 var(--text-md, 0.95rem) var(--font-display, inherit);
	}
	p {
		margin: 0.3rem 0;
		font-size: 0.9rem;
		overflow-wrap: anywhere;
	}
	.muted {
		color: var(--text-secondary);
	}
	.notice {
		border-left: 3px solid var(--accent-primary, #ff6b6b);
		padding-left: 0.6rem;
	}
	ul {
		display: grid;
		gap: 0.5rem;
		margin: 0;
		padding: 0;
	}
	ul.plain {
		list-style: none;
		gap: 0.3rem;
		font-size: 0.85rem;
	}
	.struck {
		text-decoration: line-through;
	}
	.add {
		display: grid;
		gap: 0.4rem;
		border-top: 1px solid var(--border-soft, rgba(0, 0, 0, 0.1));
		padding-top: 0.6rem;
		margin-top: 0.4rem;
	}
	label {
		display: flex;
		flex-direction: column;
		gap: 0.3rem;
	}
	select,
	input,
	textarea {
		width: 100%;
		min-width: 0;
		padding: 0.45rem 0.55rem;
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.12));
		border-radius: 8px;
		background: var(--bg-secondary, transparent);
		color: inherit;
		font: inherit;
	}
	/* The memory page's button language (.btn, .btn--small, variants). */
	button {
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.1));
		border-radius: 8px;
		background: var(--bg-card, #fff);
		color: var(--text-primary, #2d3436);
		padding: 0.3rem 0.65rem;
		font: 600 var(--text-sm, 0.85rem) var(--font-body, inherit);
		cursor: pointer;
		transition: border-color 120ms ease, background 120ms ease;
	}
	button:hover:not(:disabled) {
		border-color: var(--accent-primary, #ff6b6b);
	}
	button:focus-visible {
		outline: 2px solid var(--accent-primary, #ff6b6b);
		outline-offset: 2px;
	}
	button:disabled {
		cursor: not-allowed;
		opacity: 0.55;
	}
	button.primary {
		background: var(--accent-primary, #ff6b6b);
		border-color: var(--accent-primary, #ff6b6b);
		color: var(--button-primary-color, #fff);
	}
	button[type='submit'] {
		justify-self: start;
	}
	.modes {
		display: flex;
		flex-wrap: wrap;
		gap: 0.4rem;
		margin: 0.3rem 0 0.2rem;
	}
	.modes button.active {
		background: color-mix(in srgb, var(--accent-primary, #ff6b6b) 14%, var(--bg-card, #fff));
		border-color: var(--accent-primary, #ff6b6b);
	}
</style>
