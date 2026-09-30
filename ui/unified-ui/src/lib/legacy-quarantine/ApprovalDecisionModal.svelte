<script lang="ts">
	import { browser } from '$app/environment';
	import { onDestroy } from 'svelte';
	import { createEventDispatcher } from 'svelte';
	import type { ApprovalSummary } from '$lib/stores/approvalStore';

	const dispatch = createEventDispatcher<{
		close: void;
		approve: void;
		reject: void;
	}>();

	export let isOpen = false;
	export let approval: ApprovalSummary | null = null;
	export let isResolving = false;
	export let resolvingDecision: 'approve' | 'reject' | null = null;
	export let error: string | null = null;

	let now = Date.now();
	let clockTimer: ReturnType<typeof setInterval> | null = null;

	function stopClock(): void {
		if (!clockTimer) return;
		clearInterval(clockTimer);
		clockTimer = null;
	}

	function startClock(): void {
		if (!browser || clockTimer) return;
		clockTimer = setInterval(() => {
			now = Date.now();
		}, 1000);
	}

	$: shouldTick =
		isOpen && (approval?.status === 'pending' || approval?.status === 'validating');
	$: {
		if (shouldTick) {
			startClock();
		} else {
			stopClock();
			now = Date.now();
		}
	}

	onDestroy(() => {
		stopClock();
	});

	$: isPending = approval ? approval.status === 'pending' || approval.status === 'validating' : false;
	$: expiredByClock = approval ? approval.expires_at <= now : false;
	$: canResolve = Boolean(approval && isPending && !expiredByClock && !isResolving);

	function close(): void {
		dispatch('close');
	}

	function handleBackdropClick(event: MouseEvent): void {
		if (event.target === event.currentTarget && !isResolving) {
			close();
		}
	}

	function handleKeydown(event: KeyboardEvent): void {
		if (event.key === 'Escape' && isOpen && !isResolving) {
			close();
		}
	}

	function formatDateTime(timestamp?: number): string {
		if (!timestamp || !Number.isFinite(timestamp)) return 'n/a';
		return new Date(timestamp).toLocaleString();
	}

	function relativeTime(timestamp?: number): string {
		if (!timestamp || !Number.isFinite(timestamp)) return 'n/a';
		const diffMs = timestamp - Date.now();
		const diffMinutes = Math.round(diffMs / 60000);
		if (Math.abs(diffMinutes) < 1) return 'just now';
		if (Math.abs(diffMinutes) < 60) return `${Math.abs(diffMinutes)}m ${diffMinutes < 0 ? 'ago' : 'from now'}`;
		const diffHours = Math.round(diffMinutes / 60);
		if (Math.abs(diffHours) < 48) return `${Math.abs(diffHours)}h ${diffHours < 0 ? 'ago' : 'from now'}`;
		const diffDays = Math.round(diffHours / 24);
		return `${Math.abs(diffDays)}d ${diffDays < 0 ? 'ago' : 'from now'}`;
	}

	function formatAction(action: unknown): string {
		if (typeof action === 'string') return action;
		try {
			return JSON.stringify(action, null, 2);
		} catch {
			return String(action);
		}
	}
</script>

<svelte:window on:keydown={handleKeydown} />

{#if isOpen && approval}
	<!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
	<div class="backdrop" role="presentation" on:click={handleBackdropClick}>
		<div class="modal" role="dialog" aria-modal="true" aria-label="Approval decision">
			<header class="modal-header">
				<div>
					<p class="eyebrow">Approval review</p>
					<h2>{approval.approval_id}</h2>
				</div>
				<button type="button" class="close-btn" on:click={close} disabled={isResolving}>Close</button>
			</header>

			<div class="status-row">
				<span class="status-chip" data-status={approval.status}>{approval.status}</span>
				<span>Created {relativeTime(approval.created_at)}</span>
				<span>Expires {relativeTime(approval.expires_at)}</span>
			</div>

			{#if error}
				<div class="error-banner">{error}</div>
			{/if}

			{#if expiredByClock && isPending}
				<div class="notice warning">
					This request is past its expiry time and is treated as non-actionable.
				</div>
			{:else if !isPending}
				<div class="notice info">
					This request is terminal and cannot be changed.
				</div>
			{/if}

			<section class="meta-grid">
				<div>
					<h3>Agent</h3>
					<p>{approval.agent_id}</p>
				</div>
				<div>
					<h3>Goal</h3>
					<p>{approval.goal_id}</p>
				</div>
				<div>
					<h3>Cycle</h3>
					<p>{approval.cycle_id}</p>
				</div>
				<div>
					<h3>Trigger seq</h3>
					<p>{approval.trigger_seq}</p>
				</div>
				<div>
					<h3>Plan hash</h3>
					<p class="mono">{approval.plan_hash || 'n/a'}</p>
				</div>
				<div>
					<h3>Resolved by</h3>
					<p>{approval.resolved_by || 'n/a'}</p>
				</div>
				<div>
					<h3>Resolved at</h3>
					<p>{formatDateTime(approval.resolved_at)}</p>
				</div>
				<div>
					<h3>Updated at</h3>
					<p>{formatDateTime(approval.updated_at)}</p>
				</div>
			</section>

			<section class="actions-list">
				<h3>Pending actions ({approval.pending_action_count})</h3>
				{#if approval.pending_actions && approval.pending_actions.length > 0}
					<ul>
						{#each approval.pending_actions as action, index (index)}
							<li>
								<pre>{formatAction(action)}</pre>
							</li>
						{/each}
					</ul>
				{:else}
					<p class="muted">No pending action payload was attached.</p>
				{/if}
			</section>

			<section class="deliveries">
				<h3>Deliveries</h3>
				{#if approval.deliveries && approval.deliveries.length > 0}
					<ul>
						{#each approval.deliveries as delivery (delivery.delivery_id)}
							<li>
								<div>
									<strong>{delivery.channel}</strong>
									<span class="muted">{delivery.delivery_id}</span>
								</div>
								<div>
									<span class="status-chip" data-status={delivery.status}>{delivery.status}</span>
									<span class="muted">{formatDateTime(delivery.delivered_at)}</span>
								</div>
							</li>
						{/each}
					</ul>
				{:else}
					<p class="muted">No delivery records available.</p>
				{/if}
			</section>

			<footer class="modal-footer">
				<button type="button" class="ghost-btn" on:click={close} disabled={isResolving}>Close</button>
				<button
					type="button"
					class="reject-btn"
					on:click={() => dispatch('reject')}
					disabled={!canResolve}
				>
					{isResolving && resolvingDecision === 'reject' ? 'Rejecting...' : 'Reject'}
				</button>
				<button
					type="button"
					class="approve-btn"
					on:click={() => dispatch('approve')}
					disabled={!canResolve}
				>
					{isResolving && resolvingDecision === 'approve' ? 'Approving...' : 'Approve'}
				</button>
			</footer>
		</div>
	</div>
{/if}

<style>
	.backdrop {
		position: fixed;
		inset: 0;
		background: rgba(24, 20, 16, 0.48);
		display: flex;
		justify-content: center;
		align-items: center;
		padding: 1rem;
		z-index: 160;
	}

	.modal {
		width: min(940px, 100%);
		max-height: calc(100vh - 2rem);
		overflow: auto;
		background: #fff;
		border-radius: 14px;
		border: 1px solid #e6dfd5;
		padding: 1rem;
		display: grid;
		gap: 0.85rem;
	}

	.modal-header {
		display: flex;
		justify-content: space-between;
		gap: 0.9rem;
		align-items: center;
	}

	.eyebrow {
		margin: 0;
		font-size: 0.72rem;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		color: #857d72;
	}

	h2 {
		margin: 0.35rem 0 0;
		font-size: 1.02rem;
		word-break: break-all;
	}

	h3 {
		margin: 0;
		font-size: 0.88rem;
	}

	p {
		margin: 0.3rem 0 0;
		font-size: 0.9rem;
		color: #3e392f;
	}

	.close-btn,
	.ghost-btn,
	.reject-btn,
	.approve-btn {
		border-radius: 9px;
		padding: 0.48rem 0.75rem;
		font-size: 0.82rem;
		font-weight: 600;
		cursor: pointer;
		border: 1px solid #dad2c8;
		background: #fff;
	}

	.approve-btn {
		border-color: #2f8a65;
		background: #2f8a65;
		color: #fff;
	}

	.reject-btn {
		border-color: #d35c5c;
		background: #fff2f2;
		color: #9d3333;
	}

	button:disabled {
		opacity: 0.6;
		cursor: not-allowed;
	}

	.status-row {
		display: flex;
		flex-wrap: wrap;
		gap: 0.45rem;
		font-size: 0.8rem;
		color: #6e665a;
	}

	.status-chip {
		display: inline-flex;
		align-items: center;
		border-radius: 999px;
		padding: 0.18rem 0.48rem;
		font-size: 0.75rem;
		font-weight: 600;
		text-transform: lowercase;
		background: #f2ece4;
		color: #625c54;
	}

	.status-chip[data-status='pending'],
	.status-chip[data-status='validating'] {
		background: #fff4da;
		color: #8f6400;
	}

	.status-chip[data-status='approved'],
	.status-chip[data-status='resolved'] {
		background: #e7f8ee;
		color: #226743;
	}

	.status-chip[data-status='rejected'],
	.status-chip[data-status='failed'] {
		background: #ffe8e8;
		color: #a13f3f;
	}

	.status-chip[data-status='expired'],
	.status-chip[data-status='dismissed'] {
		background: #f1f1f1;
		color: #626262;
	}

	.error-banner {
		border: 1px solid #f3c5c5;
		background: #fff4f4;
		color: #8e3131;
		padding: 0.7rem 0.8rem;
		border-radius: 10px;
		font-size: 0.86rem;
	}

	.notice {
		padding: 0.6rem 0.75rem;
		border-radius: 9px;
		font-size: 0.84rem;
	}

	.notice.info {
		background: #f4f8ff;
		border: 1px solid #d4e4ff;
		color: #345b98;
	}

	.notice.warning {
		background: #fff7e6;
		border: 1px solid #f5dba9;
		color: #8e5d00;
	}

	.meta-grid {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
		gap: 0.55rem;
	}

	.meta-grid > div {
		padding: 0.6rem 0.65rem;
		border: 1px solid #efe9df;
		border-radius: 10px;
		background: #fffefc;
	}

	.mono {
		font-family: var(--font-mono);
		word-break: break-all;
	}

	.actions-list ul,
	.deliveries ul {
		margin: 0.45rem 0 0;
		padding-left: 1rem;
		display: grid;
		gap: 0.45rem;
	}

	.actions-list li,
	.deliveries li {
		border: 1px solid #efe9df;
		border-radius: 10px;
		padding: 0.55rem 0.6rem;
		background: #fffeff;
	}

	.actions-list pre {
		margin: 0;
		font-size: 0.78rem;
		white-space: pre-wrap;
		word-break: break-word;
	}

	.deliveries li {
		display: flex;
		justify-content: space-between;
		gap: 0.6rem;
		align-items: center;
	}

	.muted {
		color: #847d72;
		font-size: 0.8rem;
	}

	.modal-footer {
		display: flex;
		justify-content: flex-end;
		gap: 0.5rem;
	}

	@media (max-width: 760px) {
		.modal {
			padding: 0.75rem;
		}

		.deliveries li {
			flex-direction: column;
			align-items: flex-start;
		}
	}
</style>
