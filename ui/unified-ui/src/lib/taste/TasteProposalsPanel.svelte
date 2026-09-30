<script lang="ts">
	import { onDestroy, onMount } from 'svelte';
	import {
		approveTasteProposal,
		fetchTasteProposals,
		rejectTasteProposal,
		tasteProposalsPoll,
		type CaptureStats,
		type TasteProposal
	} from './tasteProposals';

	let proposals: TasteProposal[] = [];
	let stats: CaptureStats | null = null;
	let enabled = false;
	let pendingRetries = 0;
	let healthUnavailable = false;
	let loaded = false;
	let error = '';
	let notice = '';
	/** Ids currently being decided, so a double-click cannot double-submit. */
	let busy = new Set<string>();

	// Proposals arrive on the worker's sweep, not on user action, so the queue
	// polls rather than loading once — otherwise the owner sees "nothing
	// waiting" while proposals sit unread.
	const unsubscribe = tasteProposalsPoll.value.subscribe((envelope) => {
		if (!envelope) return;
		enabled = envelope.enabled;
		proposals = envelope.proposals;
		stats = envelope.stats ?? null;
		pendingRetries = envelope.capture_health?.pending_retries ?? 0;
		healthUnavailable = envelope.capture_health_unavailable ?? false;
		error = '';
		loaded = true;
	});
	onDestroy(unsubscribe);

	// The poll swallows failures into its backoff — subscribers only ever see
	// successes — so a backend that is down would leave this on "Loading…"
	// forever with nothing said. The first read is therefore direct, so it can
	// fail visibly; the poll takes over for updates after that.
	onMount(async () => {
		try {
			const envelope = await fetchTasteProposals();
			enabled = envelope.enabled;
			proposals = envelope.proposals;
			stats = envelope.stats ?? null;
			pendingRetries = envelope.capture_health?.pending_retries ?? 0;
			healthUnavailable = envelope.capture_health_unavailable ?? false;
		} catch (cause) {
			error = cause instanceof Error ? cause.message : String(cause);
		} finally {
			loaded = true;
		}
	});

	function load() {
		// After a decision: refresh immediately rather than waiting out the
		// idle cadence. Coalesces with any in-flight poll.
		tasteProposalsPoll.pollNow();
	}

	async function decide(proposal: TasteProposal, approve: boolean) {
		if (busy.has(proposal.id)) return;
		busy = new Set(busy).add(proposal.id);
		notice = '';
		try {
			const result = approve
				? await approveTasteProposal(proposal.id)
				: await rejectTasteProposal(proposal.id);
			if (approve) {
				if (proposal.kind === 'retract') {
					notice = 'Removed from your profile.';
				} else {
					notice = result.placed
						? `Added to “${proposal.destination}”.`
						: `Already in “${proposal.destination}” — nothing changed.`;
				}
			} else {
				notice =
					proposal.kind === 'retract'
						? 'Kept. This directive stays in your profile.'
						: 'Rejected. This will not be suggested again.';
			}
			proposals = proposals.filter((candidate) => candidate.id !== proposal.id);
		} catch (cause) {
			error = cause instanceof Error ? cause.message : String(cause);
			// The queue moved under us; re-read rather than leaving a stale row.
			load();
		} finally {
			const next = new Set(busy);
			next.delete(proposal.id);
			busy = next;
			// Reconcile against the server rather than trusting the local
			// filter above — another surface may have decided something too.
			load();
		}
	}
</script>

<section class="taste-proposals">
	<header>
		<h2>Taste proposals</h2>
		<p class="sub">
			Directives distilled from finished sessions. Approving one adds it to your profile
			note, where it joins every future prompt.
		</p>
	</header>

	{#if stats && (stats.approved > 0 || stats.rejected > 0)}
		<p class="stats" class:warn={stats.needs_attention}>
			{stats.approved} accepted · {stats.rejected} rejected{#if stats.accept_rate !== null}
				· {Math.round(stats.accept_rate * 100)}% accept rate{/if}
			{#if stats.needs_attention}
				— most suggestions are being rejected, so the distiller is worth revisiting.
			{/if}
		</p>
	{/if}

	{#if error}
		<p class="error" role="alert">{error}</p>
	{/if}
	{#if enabled && healthUnavailable}
		<p class="stats warn" role="status">Capture progress is unavailable. Pending proposals are still shown below.</p>
	{:else if enabled && pendingRetries > 0}
		<p class="stats warn" role="status">{pendingRetries} session(s) could not be processed yet. Capture will retry them automatically.</p>
	{/if}
	{#if notice}
		<p class="notice" role="status">{notice}</p>
	{/if}

	{#if !loaded}
		<p class="empty">Loading…</p>
	{:else if !enabled}
		<p class="empty">
			Capture is off. Nothing reads your transcripts until you enable
			<code>memory.taste_profile.capture_enabled</code>.
		</p>
	{:else if proposals.length === 0}
		<p class="empty">{pendingRetries > 0 || healthUnavailable ? 'No proposals are ready yet.' : 'Nothing waiting. Most sessions reveal nothing durable — that is normal.'}</p>
	{:else}
		<ul>
			{#each proposals as proposal (proposal.id)}
				<li class:retract={proposal.kind === 'retract'}>
					{#if proposal.kind === 'retract'}
						<p class="kind-label">Still true? Recent work contradicts this.</p>
					{/if}
					<p class="directive">{proposal.directive}</p>
					<p class="meta">
						<span class="dest">{proposal.destination}</span>
						<span class="conf">confidence {proposal.confidence.toFixed(2)}</span>
					</p>

					<!-- The evidence is the point of the review: approving without
					     seeing what the claim rests on is rubber-stamping. -->
					{#if proposal.evidence.length}
						<details open>
							<summary>Why this was suggested</summary>
							{#each proposal.evidence as quote}
								<blockquote>{quote}</blockquote>
							{/each}
						</details>
					{/if}

					<div class="actions">
						<button
							type="button"
							class="approve"
							disabled={busy.has(proposal.id)}
							on:click={() => decide(proposal, true)}
						>
							{proposal.kind === 'retract' ? 'Remove it' : 'Add to profile'}
						</button>
						<button
							type="button"
							class="reject"
							disabled={busy.has(proposal.id)}
							on:click={() => decide(proposal, false)}
						>
							{proposal.kind === 'retract' ? 'Keep it' : 'Never suggest this'}
						</button>
					</div>
				</li>
			{/each}
		</ul>
	{/if}
</section>

<style>
	.taste-proposals {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
	}
	h2 {
		margin: 0;
		font-size: 1rem;
	}
	.sub,
	.empty {
		margin: 0;
		color: var(--text-muted, #7a7a85);
		font-size: 0.85rem;
	}
	ul {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
	}
	li {
		border: 1px solid var(--border-subtle, #2a2a33);
		border-radius: 8px;
		padding: 0.75rem;
		/* Paired with overflow-wrap so a long unbroken directive cannot
		   collapse the column to one character wide. */
		min-width: 0;
		overflow-wrap: anywhere;
	}
	.directive {
		margin: 0 0 0.35rem;
		font-weight: 600;
	}
	.meta {
		margin: 0 0 0.5rem;
		display: flex;
		gap: 0.75rem;
		font-size: 0.75rem;
		color: var(--text-muted, #7a7a85);
	}
	blockquote {
		margin: 0.4rem 0 0;
		padding-left: 0.6rem;
		border-left: 2px solid var(--border-subtle, #2a2a33);
		color: var(--text-muted, #7a7a85);
		font-size: 0.85rem;
	}
	summary {
		cursor: pointer;
		font-size: 0.8rem;
		color: var(--text-muted, #7a7a85);
	}
	.actions {
		display: flex;
		gap: 0.5rem;
		margin-top: 0.6rem;
		flex-wrap: wrap;
	}
	button {
		border-radius: 6px;
		padding: 0.35rem 0.7rem;
		font-size: 0.8rem;
		cursor: pointer;
		border: 1px solid var(--border-subtle, #2a2a33);
		background: transparent;
		color: inherit;
	}
	button:disabled {
		opacity: 0.5;
		cursor: default;
	}
	.approve {
		border-color: var(--accent, #6c8cff);
	}
	.stats {
		margin: 0;
		font-size: 0.75rem;
		color: var(--text-muted, #7a7a85);
	}
	.stats.warn {
		color: var(--warning, #d9a441);
	}
	li.retract {
		border-color: var(--warning, #d9a441);
	}
	.kind-label {
		margin: 0 0 0.3rem;
		font-size: 0.7rem;
		text-transform: uppercase;
		letter-spacing: 0.04em;
		color: var(--warning, #d9a441);
	}
	.error {
		margin: 0;
		color: var(--danger, #ff6b6b);
		font-size: 0.85rem;
	}
	.notice {
		margin: 0;
		color: var(--text-muted, #7a7a85);
		font-size: 0.85rem;
	}
</style>
