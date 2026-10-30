<script lang="ts">
	import { onMount } from 'svelte';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import {
		captures,
		contextsOf,
		loadOutwardGrants,
		loadOutwardSettings,
		revokeOutwardGrant,
		saveOutwardSettings,
		type OutwardEnvelope,
		type OutwardGrant,
		type OutwardSettings
	} from './outwardSettings';

	let envelope = $state<OutwardEnvelope | null>(null);
	let draft = $state<OutwardSettings | null>(null);
	let loading = $state(true);
	let saving = $state(false);
	let error = $state('');
	let saved = $state(false);
	const dirty = $derived(!!envelope && JSON.stringify(draft) !== JSON.stringify(envelope.settings));
	/** `null` hides the Always allowed list: still loading, or a server without grants (404). */
	let grants = $state<OutwardGrant[] | null>(null);
	let grantsError = $state('');
	let revoking = $state('');
	let scopeEpoch = 0;

	function adopt(next: OutwardEnvelope) {
		envelope = next;
		draft = structuredClone(next.settings);
	}

	async function load() {
		const epoch = ++scopeEpoch;
		envelope = null;
		draft = null;
		grants = null;
		revoking = '';
		saving = false;
		loading = true;
		error = '';
		saved = false;
		void loadGrants(epoch);
		try {
			const next = await loadOutwardSettings();
			if (epoch === scopeEpoch) adopt(next);
		} catch (caught) {
			if (epoch === scopeEpoch) error = caught instanceof Error ? caught.message : 'Could not load outward settings.';
		} finally { if (epoch === scopeEpoch) loading = false; }
	}

	async function loadGrants(epoch: number) {
		grantsError = '';
		try {
			const next = await loadOutwardGrants();
			if (epoch === scopeEpoch) grants = next;
		}
		catch (caught) {
			if (epoch !== scopeEpoch) return;
			grants = grants ?? [];
			grantsError = caught instanceof Error ? caught.message : 'Could not load always-allowed sends.';
		}
	}

	async function revoke(grant: OutwardGrant) {
		if (loading || !grants?.some((entry) => entry.id === grant.id)) return;
		const epoch = scopeEpoch;
		revoking = grant.id;
		grantsError = '';
		try {
			await revokeOutwardGrant(grant.id);
			if (epoch !== scopeEpoch) return;
			grants = (grants ?? []).filter((entry) => entry.id !== grant.id);
		} catch (caught) {
			if (epoch === scopeEpoch) grantsError = caught instanceof Error ? caught.message : 'Could not revoke this permission.';
		} finally { if (epoch === scopeEpoch) revoking = ''; }
	}

	function grantedOn(ms: number): string {
		if (!Number.isFinite(ms) || ms <= 0) return '';
		return new Date(ms).toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' });
	}

	async function save() {
		if (!draft || !envelope) return;
		const epoch = scopeEpoch;
		saving = true;
		error = '';
		saved = false;
		try {
			const next = await saveOutwardSettings($state.snapshot(draft), envelope.revision);
			if (epoch !== scopeEpoch) return;
			adopt(next);
			saved = !!envelope?.applied;
			if (!saved) error = 'Saved, but the running policy changed elsewhere. Reload to review it.';
		} catch (caught) {
			if (epoch === scopeEpoch) error = caught instanceof Error ? caught.message : 'Could not save outward settings.';
		} finally { if (epoch === scopeEpoch) saving = false; }
	}

	onMount(() => {
		let key: string | null = null;
		const unsubscribe = scopeIdentityStore.subscribe(({ principal, workspace, isResolved }) => {
			const next = JSON.stringify([principal, workspace, isResolved]);
			if (next === key) return;
			key = next;
			void load();
		});
		return () => { scopeEpoch++; unsubscribe(); };
	});
</script>

<section class="outward-panel" id="outward-actions" aria-labelledby="outward-title">
	<header>
		<p class="overline">Safety</p>
		<h2 id="outward-title">Outward actions</h2>
		<p>Choose which work may send messages, email or invitations. Changes apply immediately across this server’s workspaces.</p>
	</header>
	{#if error}<p class="error" role="alert">{error}</p>{/if}
	{#if loading}<p role="status">Loading outward settings…</p>{/if}
	{#if draft}
		<fieldset disabled={loading || saving}>
			<label class="check master" class:on={draft.force_capture_only}>
				<input type="checkbox" bind:checked={draft.force_capture_only} />
				<span>
					<strong>Capture all sending categories</strong>
					<small>Overrides every sending choice below. Actions are recorded without sending. Browser form clicks use the separate review switch.</small>
				</span>
				{#if draft.force_capture_only}<b class="master-state" aria-hidden="true">On · nothing is sent</b>{/if}
			</label>
			{#if draft.review_captured_sends !== undefined}
				<!-- Only on servers that can pause a captured send for review. -->
				<label class="row">
					<span>
						<strong>When a send is captured</strong>
						<small>{draft.review_captured_sends
							? 'Pauses the agent and asks you on Attention and your phones: Send, Discard or Always allow.'
							: 'Captured sends are recorded and never sent.'}</small>
						{#if draft.force_capture_only}<em>Capture-all records sends without asking.</em>{/if}
					</span>
					<select aria-label="When a send is captured" bind:value={draft.review_captured_sends}>
						<option value={true}>Ask me to review</option>
						<option value={false}>Just record it</option>
					</select>
				</label>
			{/if}
			<label class="row">
				<span><strong>Default sending policy</strong><small>Used by categories set to Follow default. Capture only is the initial default.</small></span>
				<select aria-label="Default sending policy" disabled={draft.force_capture_only} bind:value={draft.capture_only}>
					<option value={true}>Capture only</option>
					<option value={false}>Allow real sends</option>
				</select>
			</label>
			{#each contextsOf(draft) as context (context.key)}
				<label class="row">
					<span><strong>{context.label}</strong><small>{context.description}</small><em>After saving: {captures(draft, context.key) ? 'Capture only' : 'Real sends permitted'}</em></span>
					<select aria-label={context.label} disabled={draft.force_capture_only} bind:value={draft.contexts[context.key]}>
						<option value="inherit">Follow default</option>
						<option value="capture">Capture only</option>
						<option value="live">Allow real sends</option>
					</select>
				</label>
			{/each}
			{#if draft.force_capture_only}<p class="note">Sending choices are paused while capture-all is on. Turn it off to edit them; their saved values are kept.</p>{/if}
			<label class="check">
				<input type="checkbox" bind:checked={draft.browser_submission_guard} />
				<span><strong>Review browser form submissions</strong><small>Require approval for recognised Post or Submit actions. Existing site approvals still apply.</small></span>
			</label>
		</fieldset>
		<p class="note">Tool permissions, required approvals, preferences and recipient restrictions still apply. Captured actions are not automatically resent when enabled. Sends already admitted cannot be recalled.</p>
		<p class="note">Older tasks without a known origin stay in capture mode when a category override is used.</p>
		{#if envelope && !envelope.applied}<p class="note">The saved configuration differs from the running policy. Save to apply it now.</p>{/if}
	{/if}
	{#if grants}
		<section class="grants" aria-labelledby="outward-grants-title" data-testid="outward-grants">
			<h3 id="outward-grants-title">Always allowed</h3>
			<p class="note">Agents you let send to a recipient without asking. Suppression, recipient rules and preference checks still apply.</p>
			{#if grantsError}<p class="error" role="alert">{grantsError}</p>{/if}
			{#if grants.length === 0}
				<p class="note">No standing permissions. Choosing Always allow on a captured send adds one here.</p>
			{:else}
				<ul>
					{#each grants as grant (grant.id)}
						<li data-testid="outward-grant">
							<span>
								<strong>{grant.agent_id} → {grant.recipient}</strong>
								<small>{grant.capability}{grantedOn(grant.granted_at_ms) ? ` · since ${grantedOn(grant.granted_at_ms)}` : ''}</small>
							</span>
							<button
								type="button"
								aria-label={`Revoke ${grant.agent_id} sending to ${grant.recipient}`}
								disabled={loading || revoking !== ''}
								onclick={() => void revoke(grant)}
							>{revoking === grant.id ? 'Revoking…' : 'Revoke'}</button>
						</li>
					{/each}
				</ul>
			{/if}
		</section>
	{/if}
	<footer>
		<button type="button" onclick={load} disabled={loading || saving}>Reload</button>
		<button type="button" class="primary" onclick={save} disabled={!draft || loading || saving || (!dirty && !!envelope?.applied)}>{saving ? 'Saving…' : 'Save and apply'}</button>
		{#if saved && !dirty}<span role="status">Saved and applied. No restart needed.</span>{/if}
	</footer>
</section>

<style>
	/* The Settings card anatomy and theme tokens (as BudgetsPanel and the
	   page's own cards use), so every theme and dark mode apply. */
	.outward-panel { background: var(--bg-card); border: 1px solid var(--border-soft); border-radius: 8px; color: var(--text-primary); padding: 1rem; }
	.overline { color: var(--text-secondary); font-size: 0.75rem; font-weight: 700; letter-spacing: 0; margin: 0; text-transform: uppercase; }
	h2 { color: var(--text-primary); font-size: 1.05rem; font-weight: 600; line-height: 1.25; margin: 0.2rem 0 0.35rem; }
	header p:not(.overline), .note { color: var(--text-secondary); font-size: 0.92rem; line-height: 1.5; margin: 0; }
	.note { font-size: 0.82rem; margin-top: 0.5rem; }
	fieldset { border: 0; padding: 0; margin: 0.75rem 0 0.5rem; min-width: 0; }
	.row { display: grid; grid-template-columns: minmax(0, 1fr) minmax(10rem, 13rem); gap: 0.75rem 1rem; align-items: start; padding: 0.7rem 0; border-bottom: 1px solid var(--border-soft); }
	strong { display: block; color: var(--text-primary); font-size: 0.88rem; font-weight: 600; }
	small { display: block; color: var(--text-secondary); font-size: 0.8rem; line-height: 1.45; margin-top: 0.2rem; }
	em { display: block; color: var(--text-secondary); font-style: normal; font-size: 0.78rem; margin-top: 0.3rem; }
	select { width: 100%; min-width: 0; border-radius: 6px; border: 1px solid var(--border-soft); background-color: color-mix(in srgb, var(--bg-card) 92%, var(--bg-soft) 8%); color: var(--text-primary); font: inherit; font-size: 0.8125rem; }
	select:disabled { opacity: 0.58; cursor: not-allowed; }
	.check { display: flex; gap: 0.65rem; align-items: start; margin: 0.75rem 0; }
	.check input { accent-color: var(--accent-primary); margin-top: 0.2rem; flex-shrink: 0; }
	/* Capture-all overrides every sending choice below it, so it sits in an
	   accented box of its own and says plainly when it is on. */
	.master {
		background: color-mix(in srgb, var(--accent-primary) 7%, var(--bg-card));
		border: 1px solid color-mix(in srgb, var(--accent-primary) 32%, var(--border-soft));
		border-left-width: 4px;
		border-left-color: var(--accent-primary);
		border-radius: 8px;
		margin: 0.25rem 0 0.75rem;
		padding: 0.75rem 0.85rem;
	}
	.master.on { background: color-mix(in srgb, var(--accent-primary) 14%, var(--bg-card)); }
	.master strong { font-size: 0.95rem; }
	.master span { flex: 1; min-width: 0; }
	.master-state {
		align-self: center;
		background: var(--accent-fill, var(--accent-primary));
		border-radius: 999px;
		color: var(--accent-on-primary, #fff);
		flex-shrink: 0;
		font-size: 0.72rem;
		font-weight: 700;
		padding: 0.2rem 0.55rem;
		white-space: nowrap;
	}
	@media (max-width: 600px) {
		.master { flex-wrap: wrap; }
		.master-state { margin-left: 1.65rem; }
	}
	.grants { border-top: 1px solid var(--border-soft); margin-top: 0.75rem; padding-top: 0.75rem; }
	h3 { color: var(--text-primary); font-size: 0.88rem; font-weight: 600; letter-spacing: 0; margin: 0 0 0.25rem; }
	.grants .note { margin-top: 0.25rem; }
	.grants ul { list-style: none; margin: 0.5rem 0 0; padding: 0; }
	.grants li { display: flex; gap: 0.75rem; align-items: center; justify-content: space-between; padding: 0.55rem 0; border-bottom: 1px solid var(--border-soft); }
	.grants li span { min-width: 0; overflow-wrap: anywhere; }
	.grants li button { flex-shrink: 0; padding: 0.25rem 0.65rem; }
	footer { display: flex; flex-wrap: wrap; gap: 0.5rem; align-items: center; margin-top: 0.75rem; }
	button { border: 1px solid var(--border-soft); background: transparent; color: var(--text-primary); border-radius: 6px; font: inherit; font-size: 0.8125rem; font-weight: 600; cursor: pointer; }
	button.primary { background: var(--accent-fill); border-color: var(--accent-primary); color: var(--accent-on-primary, #fff); }
	button:disabled { opacity: 0.58; cursor: not-allowed; }
	footer span { color: var(--text-secondary); font-size: 0.8rem; }
	.error { color: var(--danger, #c2410c); font-size: 0.85rem; margin: 0.5rem 0 0; }
	@media (max-width: 600px) { .row { grid-template-columns: 1fr; gap: 0.5rem; } }
</style>
