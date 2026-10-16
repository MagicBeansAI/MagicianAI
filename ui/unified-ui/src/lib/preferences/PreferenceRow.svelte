<script lang="ts">
	/**
	 * One preference as checks see it, with the owner's actions on it.
	 *
	 * Every action names the revision on screen; the parent reloads and says
	 * so when the preference changed in the meantime. Forget is two steps,
	 * because it also erases the wording from the history.
	 */
	import { createEventDispatcher } from 'svelte';

	import PreferenceHistory from './PreferenceHistory.svelte';
	import { presentStatement, type Preference } from './preferencesApi';

	export let preference: Preference;
	export let busy = false;

	const dispatch = createEventDispatcher<{
		edit: { preference: Preference; wording: string };
		confirm: Preference;
		retire: Preference;
		forget: Preference;
	}>();

	const STATUS_WORDS: Record<string, string> = {
		direct_owner_statement: 'You said it',
		confirmed: 'You confirmed it',
		inferred: 'Inferred from your activity',
		inferred_untrusted: 'From a page or tool — only ever mentioned',
		unknown_provenance: 'Source unknown — only ever mentioned'
	};

	const LIFECYCLE_WORDS: Record<string, string> = {
		pending_review: 'Awaiting review',
		unresolved: 'Disputed — Magician asks which holds'
	};

	let editing = false;
	let draft = '';
	let confirmingForget = false;
	let showHistory = false;

	function startEdit(): void {
		draft = preference.statement.replace(/^[^:]{1,60}:\s*/, '');
		editing = true;
	}

	function saveEdit(): void {
		const wording = draft.trim();
		if (!wording) return;
		dispatch('edit', { preference, wording });
		editing = false;
	}

	$: canConfirm = preference.status === 'inferred';
	$: presented = presentStatement(preference.statement);
	$: checked = preference.lastCheckedAtMs
		? new Date(preference.lastCheckedAtMs).toLocaleDateString()
		: null;
</script>

<li class="preference" data-testid="preference-row">
	{#if presented}
		<p class="statement">{presented.title}</p>
		<dl class="fields" data-testid="preference-fields">
			{#each presented.details as field (field.label)}
				<dt>{field.label}</dt>
				<dd>{field.value}</dd>
			{/each}
		</dl>
	{:else}
		<p class="statement">{preference.statement}</p>
	{/if}
	{#if preference.conditions}
		<p class="muted conditions">Applies {preference.conditions}</p>
	{/if}
	<p class="facts">
		<span data-testid="preference-status">{STATUS_WORDS[preference.status] ?? preference.status}</span>
		{#if LIFECYCLE_WORDS[preference.lifecycle]}
			<span class="badge">{LIFECYCLE_WORDS[preference.lifecycle]}</span>
		{/if}
		<span class="muted">· {preference.source}</span>
		{#if checked}
			<span class="muted">· last checked {checked}</span>
		{/if}
	</p>

	{#if editing}
		<label>
			<span class="muted">New wording</span>
			<textarea bind:value={draft} rows="2" data-testid="preference-edit-input"></textarea>
		</label>
		<div class="actions">
			<button type="button" class="primary" on:click={saveEdit} disabled={busy || !draft.trim()}>Save</button>
			<button type="button" class="secondary" on:click={() => (editing = false)}>Cancel</button>
		</div>
	{:else if confirmingForget}
		<p role="alert">
			Forget this preference? Magician stops using it, and its wording is erased from the history
			too.
		</p>
		<div class="actions">
			<button
				type="button"
				class="danger"
				data-testid="preference-forget-confirm"
				disabled={busy}
				on:click={() => {
					confirmingForget = false;
					dispatch('forget', preference);
				}}>Forget it</button
			>
			<button type="button" class="secondary" on:click={() => (confirmingForget = false)}
				>Keep it</button
			>
		</div>
	{:else}
		<div class="actions">
			{#if preference.editable}
				<button type="button" on:click={startEdit} disabled={busy}>Edit</button>
				{#if canConfirm}
					<button type="button" on:click={() => dispatch('confirm', preference)} disabled={busy}
						>Confirm</button
					>
				{/if}
				<button type="button" class="secondary" on:click={() => dispatch('retire', preference)} disabled={busy}
					>Retire</button
				>
				<button
					type="button"
					class="danger"
					data-testid="preference-forget"
					on:click={() => (confirmingForget = true)}
					disabled={busy}>Forget</button
				>
			{:else}
				<span class="muted">Change this in your {preference.source}.</span>
			{/if}
			<button type="button" class="link" on:click={() => (showHistory = !showHistory)}>
				{showHistory ? 'Hide history' : 'History'}
			</button>
		</div>
	{/if}

	{#if showHistory}
		<PreferenceHistory id={preference.id} />
	{/if}
</li>

<style>
	.preference {
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.1));
		border-radius: 10px;
		background: var(--bg-card, #fff);
		padding: 0.75rem 0.9rem;
		list-style: none;
	}
	p {
		margin: 0.25rem 0;
		font-size: 0.9rem;
		overflow-wrap: anywhere;
	}
	.statement {
		font-weight: 600;
		color: var(--text-primary, #2d3436);
	}
	.fields {
		display: grid;
		grid-template-columns: minmax(7rem, max-content) 1fr;
		gap: 0.15rem 0.75rem;
		margin: 0.35rem 0 0.2rem;
		font-size: 0.85rem;
	}
	.fields dt {
		color: var(--text-secondary);
	}
	.fields dd {
		margin: 0;
		overflow-wrap: anywhere;
	}
	@media (max-width: 480px) {
		.fields {
			grid-template-columns: 1fr;
		}
		.fields dd {
			margin-bottom: 0.3rem;
		}
	}
	.facts {
		display: flex;
		flex-wrap: wrap;
		gap: 0.35rem;
		font-size: 0.8rem;
	}
	.muted {
		color: var(--text-secondary);
	}
	.badge {
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.1));
		border-radius: 999px;
		padding: 0 0.45rem;
		background: color-mix(in srgb, var(--accent-primary, #ff6b6b) 8%, transparent);
	}
	.actions {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.4rem;
		margin-top: 0.55rem;
	}
	label {
		display: flex;
		flex-direction: column;
		gap: 0.3rem;
		margin-top: 0.4rem;
	}
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
	button.secondary {
		background: color-mix(in srgb, var(--accent-primary, #ff6b6b) 10%, var(--bg-card, #fff));
	}
	button.danger {
		color: var(--color-danger, #d63031);
		border-color: color-mix(in srgb, var(--color-danger, #d63031) 45%, transparent);
	}
	button.danger:hover:not(:disabled) {
		border-color: var(--color-danger, #d63031);
	}
	button.link {
		background: none;
		border-color: transparent;
		color: var(--accent-primary, #ff6b6b);
	}
	button.link:hover:not(:disabled) {
		border-color: transparent;
		text-decoration: underline;
	}
</style>
