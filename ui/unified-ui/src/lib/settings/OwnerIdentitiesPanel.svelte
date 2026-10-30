<script lang="ts">
	import { onMount } from 'svelte';
	import { requestConfirmation } from '$lib/stores/confirmationStore';
	import { showError, showSuccess } from '$lib/shared/stores/notifications';
	import {
		channelDisplayName,
		fetchOwnerIdentities,
		mintClaimCode,
		removeOwnerIdentity,
		type ClaimCode,
		type OwnerIdentitiesState
	} from '$lib/settings/ownerIdentities';

	let ownerState = $state<OwnerIdentitiesState | null>(null);
	let claim = $state<ClaimCode | null>(null);
	let now = $state(Date.now());
	let busy = $state(false);
	let error = $state<string | null>(null);

	const claimLive = $derived(claim !== null && claim.expires_at_ms > now);
	const claimPlaces = $derived(
		Array.from(new Set((ownerState?.claimable_channels ?? []).map((channel) => channelDisplayName(channel).replace(/ \(.*\)$/, '')))).join(' or ')
	);

	onMount(() => {
		void refresh();
		const timer = setInterval(() => {
			now = Date.now();
		}, 1000);
		return () => clearInterval(timer);
	});

	async function refresh(): Promise<void> {
		try {
			ownerState = await fetchOwnerIdentities();
			error = null;
		} catch (caught) {
			error = caught instanceof Error ? caught.message : 'Owner identities are unavailable.';
		}
	}

	async function mint(): Promise<void> {
		busy = true;
		try {
			claim = await mintClaimCode();
			now = Date.now();
		} catch (caught) {
			showError(caught instanceof Error ? caught.message : 'Could not create a claim code.');
		} finally {
			busy = false;
		}
	}

	async function remove(channel: string, index: number, masked: string): Promise<void> {
		const confirmed = await requestConfirmation({
			title: `Remove ${masked} from ${channelDisplayName(channel)}?`,
			message: 'Messages from that address are treated as a guest’s again: they go to the envoy agent, not to Magician directly.',
			confirmLabel: 'Remove',
			destructive: true
		});
		if (!confirmed) return;
		busy = true;
		try {
			ownerState = await removeOwnerIdentity(channel, index);
			showSuccess('Owner identity removed');
		} catch (caught) {
			showError(caught instanceof Error ? caught.message : 'Could not remove the owner identity.');
		} finally {
			busy = false;
		}
	}

	function remaining(expiresAt: number): string {
		const secs = Math.max(0, Math.round((expiresAt - now) / 1000));
		return `${Math.floor(secs / 60)}:${String(secs % 60).padStart(2, '0')}`;
	}
</script>

<section id="owner-identities" class="card" aria-labelledby="owner-identities-title">
	<div class="header">
		<div>
			<p class="overline">Owner identities</p>
			<h2 id="owner-identities-title">Which chats are you</h2>
			<p>
				Messages from these addresses reach Magician directly; everyone else talks to the envoy agent.
				To add the chat you are using, create a code here and send <code>/claim</code> with it to the Magician bot from that chat.
				You can also set them by hand: <code>envoy.owner_identities</code> in <code>magician-config.yaml</code>, or the channel’s
				<code>MAGICIAN_OWNER_*_IDENTITIES</code> variable.
			</p>
		</div>
		<div class="actions">
			<button type="button" disabled={busy} onclick={() => void refresh()}>Refresh</button>
			<button class="primary" type="button" disabled={busy || !ownerState || ownerState.claimable_channels.length === 0} onclick={() => void mint()}>
				{claimPlaces ? `Claim from ${claimPlaces}` : 'Claim from a chat'}
			</button>
		</div>
	</div>

	{#if error}
		<p class="alert" role="alert">{error}</p>
	{/if}

	{#if claim && claimLive}
		<p class="claim" role="status">
			Send <code class="code">/claim {claim.code}</code> to the Magician bot{claimPlaces ? ` on ${claimPlaces}` : ''} within {remaining(claim.expires_at_ms)}.
			The code works once.
		</p>
	{:else if claim}
		<p class="meta">That code expired. Create a new one when you are ready to send it.</p>
	{/if}

	{#if ownerState}
		{#if ownerState.channels.length === 0}
			<p class="empty">No channel has owner identities configured yet.</p>
		{/if}
		<ul class="channels">
			{#each ownerState.channels as entry (entry.channel)}
				<li>
					<strong>{channelDisplayName(entry.channel)}</strong>
					{#if entry.identities.length === 0}
						<span class="chip warn">none yet</span>
					{/if}
					{#each entry.identities as identity (identity.index)}
						<span class="identity">
							<span class="addresses">{identity.masked}</span>
							{#if identity.source === 'config'}<span class="chip">config file</span>{/if}
							{#if identity.removable}
								<button class="link" type="button" disabled={busy} aria-label={`Remove ${identity.masked}`} onclick={() => void remove(entry.channel, identity.index, identity.masked)}>remove</button>
							{/if}
						</span>
					{/each}
					{#if !entry.claimable}<span class="meta">set by hand</span>{/if}
				</li>
			{/each}
		</ul>
	{:else if !error}
		<p class="empty">Loading owner identities…</p>
	{/if}
</section>

<style>
	.card {
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		display: flex;
		flex-direction: column;
		font-size: 0.88rem;
		gap: 1rem;
		padding: 1rem;
	}
	.header {
		display: flex;
		flex-direction: column;
		gap: 1rem;
	}
	.overline {
		color: var(--text-secondary);
		font-size: 0.75rem;
		font-weight: 700;
		margin: 0;
		text-transform: uppercase;
	}
	h2,
	p {
		margin: 0;
	}
	h2 {
		color: var(--text-primary);
		font-size: 1.05rem;
	}
	.header p:not(.overline) {
		color: var(--text-secondary);
		font-size: 0.92rem;
		line-height: 1.5;
	}
	.empty,
	.meta {
		color: var(--text-secondary);
		font-size: 0.85rem;
	}
	code {
		font-size: 0.85em;
	}
	.actions {
		display: flex;
		flex-wrap: wrap;
		gap: 0.5rem;
	}
	button {
		background: var(--bg-secondary);
		border: 1px solid var(--border-soft);
		border-radius: 6px;
		color: var(--text-primary);
		cursor: pointer;
		padding: 0.45rem 0.85rem;
	}
	button.primary {
		background: var(--accent-fill);
		border-color: var(--accent);
		color: var(--text-on-accent, #fff);
	}
	button.link {
		background: none;
		border: none;
		color: var(--accent);
		font-size: 0.8rem;
		padding: 0 0.2rem;
	}
	button:disabled {
		cursor: not-allowed;
		opacity: 0.6;
	}
	.alert {
		background: var(--bg-secondary);
		border-left: 3px solid var(--danger, #c0392b);
		padding: 0.5rem 0.75rem;
	}
	.claim {
		background: var(--bg-secondary);
		border-left: 3px solid var(--accent);
		line-height: 1.6;
		padding: 0.6rem 0.75rem;
	}
	.claim .code {
		font-family: var(--font-mono, monospace);
		font-size: 1rem;
		font-weight: 700;
		letter-spacing: 0.06em;
		user-select: all;
	}
	.channels {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
		list-style: none;
		margin: 0;
		padding: 0;
	}
	.channels li {
		align-items: center;
		display: flex;
		flex-wrap: wrap;
		gap: 0.5rem;
	}
	.identity {
		align-items: center;
		display: inline-flex;
		gap: 0.25rem;
	}
	.addresses {
		color: var(--text-secondary);
		font-family: var(--font-mono, monospace);
		font-size: 0.8rem;
	}
	.chip {
		background: var(--bg-secondary);
		border-radius: 999px;
		font-size: 0.72rem;
		padding: 0.1rem 0.5rem;
	}
	.chip.warn {
		background: var(--warning-soft, #fdf1dc);
		color: var(--warning, #8a5a00);
	}
</style>
