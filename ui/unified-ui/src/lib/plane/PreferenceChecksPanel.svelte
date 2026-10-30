<script lang="ts">
	/**
	 * Settings → Agents & safety → Preference checks (P6.4, P7.2).
	 *
	 * Two facts the owner needs before trusting the feature: the mode actually
	 * in force, and whether the saved configuration would be accepted — with
	 * the stable rejection code and the exact property to fix when it is not.
	 * A rejected save never reaches the running service, so both are shown and
	 * never collapsed into one.
	 *
	 * Below that, the owner's own check policy: an explicit choice, saved
	 * against the revision this screen read.
	 */
	import { onMount } from 'svelte';

	import { hasHydratedScopeBearer } from '$lib/stores/scopeIdentityStore';

	import {
		POLICY_CHOICES,
		answerPreferenceNotice,
		fetchOwnerPolicy,
		fetchPreferenceCheckStatus,
		fetchPreferenceNotices,
		saveOwnerPolicy,
		type InterventionPolicy,
		type OwnerPolicy,
		type PolicyRead,
		type PreferenceCheckStatus,
		type PreferenceNotice
	} from './preferenceChecks';

	let status: PreferenceCheckStatus | null = null;
	let statusError = '';
	let policy: PolicyRead | null = null;
	let policyError = '';
	let selected: InterventionPolicy | '' = '';
	let saving = false;
	let notice = '';
	let notes: PreferenceNotice[] = [];
	let notesError = '';
	let answering = '';

	async function answerNote(note: PreferenceNotice, answer: 'remember' | 'dismiss'): Promise<void> {
		answering = note.id;
		notesError = '';
		try {
			const state = await answerPreferenceNotice(note.id, answer, note.remember?.token);
			notes = notes.map((existing) => (existing.id === note.id ? { ...existing, state } : existing));
			if (state === 'stale') {
				notesError = 'That preference changed since this note, so nothing was saved.';
			}
		} catch (error) {
			notesError = error instanceof Error ? error.message : String(error);
		} finally {
			answering = '';
		}
	}

	const MODE_WORDS: Record<string, string> = {
		off: 'Off — nothing is checked',
		shadow: 'Shadow — checks are recorded, nothing is held',
		enforce: 'On — conflicting actions are held for you'
	};

	async function load(): Promise<void> {
		try {
			status = await fetchPreferenceCheckStatus();
			statusError = '';
		} catch (error) {
			statusError = error instanceof Error ? error.message : String(error);
		}
		try {
			policy = await fetchOwnerPolicy();
			policyError = '';
			if (policy.status === 'ok') selected = policy.global ?? '';
		} catch (error) {
			policyError = error instanceof Error ? error.message : String(error);
		}
		try {
			notes = await fetchPreferenceNotices();
		} catch (error) {
			notesError = error instanceof Error ? error.message : String(error);
		}
	}

	async function save(): Promise<void> {
		if (!policy) return;
		saving = true;
		notice = '';
		policyError = '';
		const unreadable = policy.status === 'unreadable';
		const current: OwnerPolicy | null = policy.status === 'ok' ? policy : null;
		try {
			const outcome = await saveOwnerPolicy(
				current?.revision ?? 0,
				selected === '' ? null : selected,
				current?.intentions ?? {},
				unreadable
			);
			if (outcome.status === 'stale') {
				policy = outcome.current;
				selected = outcome.current.global ?? '';
				policyError = `${outcome.message} The current setting is shown; save again to change it.`;
			} else {
				policy = outcome.policy;
				notice = 'Saved. The next check uses it.';
			}
		} catch (error) {
			policyError = error instanceof Error ? error.message : String(error);
		} finally {
			saving = false;
		}
	}

	onMount(() => {
		void (async () => {
			if (!(await hasHydratedScopeBearer())) {
				statusError = 'Sign in as the owner to manage preference checks.';
				return;
			}
			await load();
		})();
	});

	$: openNotes = notes.filter((note) => note.state === 'open');
	$: savedGlobal = policy?.status === 'ok' ? (policy.global ?? '') : null;
	$: selectedDetail = POLICY_CHOICES.find((choice) => choice.value === selected)?.detail ?? '';
</script>

<section class="preference-checks" data-testid="preference-checks">
	<header class="header">
		<p class="overline">Preferences</p>
		<h2>Preference checks</h2>
		<p class="lede">
			Before Magician sends, books, orders or buys for you, it can check the action against what
			you have told it you want.
		</p>
	</header>

	{#if statusError}
		<p role="alert">{statusError}</p>
	{:else if status}
		<dl class="state box">
			<dt>In force now</dt>
			<dd data-testid="preference-checks-effective">
				{MODE_WORDS[status.effective.modeForYou] ?? status.effective.modeForYou}
			</dd>
			<dt>Saved configuration</dt>
			{#if status.saved.status === 'rejected'}
				<dd data-testid="preference-checks-rejected">
					<strong>Not applied.</strong>
					{status.saved.error.reason}
					<span class="code">
						{status.saved.error.code}{#if status.saved.error.property}
							· {status.saved.error.property}{/if}
					</span>
					<span class="muted">The setting in force above stays until a valid one is saved.</span>
				</dd>
			{:else}
				<dd>
					{status.saved.mode
						? (MODE_WORDS[status.saved.mode.modeForYou] ?? status.saved.mode.modeForYou)
						: 'Accepted'}
					{#if !status.inSync}
						<span class="muted">— not loaded yet; it applies on the next reload.</span>
					{/if}
				</dd>
			{/if}
		</dl>
	{:else}
		<p class="muted">Loading…</p>
	{/if}

	<form
		class="policy box"
		onsubmit={(event) => {
			event.preventDefault();
			void save();
		}}
	>
		<h3>How firmly to check</h3>
		{#if policy?.status === 'unreadable'}
			<p role="alert">
				Your saved setting cannot be read, so checks are paused for your review until you save a
				new one.
			</p>
		{/if}
		<label>
			<span>Check policy</span>
			<select bind:value={selected} disabled={saving || !policy} data-testid="preference-policy-select">
				{#each POLICY_CHOICES as choice (choice.value)}
					<option value={choice.value}>{choice.label}</option>
				{/each}
			</select>
		</label>
		{#if selectedDetail}
			<p class="muted">{selectedDetail}</p>
		{/if}
		{#if policyError}
			<p role="alert">{policyError}</p>
		{/if}
		{#if notice}
			<p role="status">{notice}</p>
		{/if}
		<button
			class="btn btn-primary"
			type="submit"
			disabled={saving || !policy || (savedGlobal !== null && selected === savedGlobal)}
		>
			{saving ? 'Saving…' : 'Save check policy'}
		</button>
	</form>

	<section class="notes box" aria-label="Notes from recent checks" data-testid="preference-notes">
		<h3>Notes from recent checks</h3>
		<p class="muted">
			Actions Magician went ahead with, and what it noticed. Saving one makes Magician treat it
			as something you said.
		</p>
		{#if notesError}
			<p role="alert">{notesError}</p>
		{/if}
		{#if openNotes.length === 0}
			<p class="muted">Nothing new.</p>
		{/if}
		{#each openNotes as note (note.id)}
			<article class="note" data-testid="preference-note">
				<p>{note.explanation}</p>
				<p class="muted">Went ahead: {note.actSummary}</p>
				{#if note.remember}
					<p>Remember exactly this as your preference?</p>
					<blockquote>{note.remember.wording}</blockquote>
				{/if}
				<div class="note-actions">
					{#if note.remember}
						<button
							class="btn"
							type="button"
							disabled={answering === note.id}
							onclick={() => void answerNote(note, 'remember')}
						>
							Save this wording
						</button>
					{/if}
					<button
						class="btn"
						type="button"
						disabled={answering === note.id}
						onclick={() => void answerNote(note, 'dismiss')}
					>
						Dismiss
					</button>
				</div>
			</article>
		{/each}
	</section>
</section>

<style>
	/* A Settings card: theme tokens and the Settings type scale (label 12px,
	   title ~17px, lede ~15px, sub-heading 14px, body 13px). */
	.preference-checks {
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		box-sizing: border-box;
		color: var(--text-primary);
		display: grid;
		gap: 0.85rem;
		min-width: 0;
		padding: 1rem;
	}
	.header {
		display: grid;
		gap: 0.2rem;
	}
	.overline {
		color: var(--text-secondary);
		font-size: 0.75rem;
		font-weight: 700;
		letter-spacing: 0;
		margin: 0;
		text-transform: uppercase;
	}
	h2 {
		color: var(--text-primary);
		font-size: 1.05rem;
		font-weight: 600;
		letter-spacing: 0;
		line-height: 1.25;
		margin: 0;
	}
	.lede {
		color: var(--text-secondary);
		font-size: 0.92rem;
		line-height: 1.5;
		margin: 0.15rem 0 0;
	}
	/* Each part sits in the soft inner box Settings uses for sub-panels. */
	.box {
		background: color-mix(in srgb, var(--bg-card) 88%, var(--bg-soft) 12%);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		margin: 0;
		min-width: 0;
		padding: 0.75rem 0.85rem;
	}
	h3 {
		color: var(--text-primary);
		font-size: 0.88rem;
		font-weight: 600;
		letter-spacing: 0;
		margin: 0 0 0.35rem;
	}
	p {
		font-size: 0.8125rem;
		line-height: 1.45;
		margin: 0.4rem 0;
		overflow-wrap: anywhere;
	}
	.muted {
		color: var(--text-secondary);
	}
	p[role='alert'] {
		color: var(--danger, #c2410c);
	}
	p[role='status'] {
		color: var(--accent-primary);
		font-weight: 600;
	}
	.state {
		display: grid;
		font-size: 0.8125rem;
		gap: 0.35rem 1rem;
		grid-template-columns: minmax(0, max-content) minmax(0, 1fr);
	}
	.state dt {
		color: var(--text-secondary);
	}
	.state dd {
		color: var(--text-primary);
		font-weight: 600;
		margin: 0;
		overflow-wrap: anywhere;
	}
	.state dd .muted {
		font-weight: 400;
	}
	.code {
		color: var(--text-secondary);
		display: block;
		font-family: var(--font-mono, monospace);
		font-size: 0.75rem;
		font-weight: 400;
	}
	label {
		display: flex;
		flex-direction: column;
		gap: 0.3rem;
		margin-block: 0.5rem;
	}
	label span {
		color: var(--text-secondary);
		font-size: 0.8rem;
		font-weight: 600;
	}
	select {
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		color: var(--text-primary);
		max-width: 28rem;
		min-width: 0;
		padding: 0 0.6rem;
		width: 100%;
	}
	.policy button {
		margin-top: 0.25rem;
	}
	.note {
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 6px;
		margin-block: 0.5rem;
		padding: 0.6rem 0.7rem;
	}
	.note blockquote {
		border-left: 2px solid var(--accent-primary);
		color: var(--text-primary);
		font-size: 0.8125rem;
		margin: 0.4rem 0;
		overflow-wrap: anywhere;
		padding-left: 0.6rem;
	}
	.note-actions {
		display: flex;
		flex-wrap: wrap;
		gap: 0.4rem;
	}
	@media (max-width: 640px) {
		.state {
			grid-template-columns: minmax(0, 1fr);
		}
		.state dd {
			margin-bottom: 0.35rem;
		}
	}
</style>
