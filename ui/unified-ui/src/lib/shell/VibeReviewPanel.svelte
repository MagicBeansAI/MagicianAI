<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import { cubicOut } from 'svelte/easing';
	import { fly } from 'svelte/transition';
	import Checkbox from '$lib/magician/components/generative/Checkbox.svelte';
	import DiffStrip from '$lib/shell/DiffStrip.svelte';
	import type { HitlRequest, HitlSource } from '$lib/hitl/types';
	import type { HitlPendingEntry } from '$lib/stores/pendingHitlStore';

	type VibeReviewRow = {
		key: string;
		entry: HitlPendingEntry;
		request: HitlRequest;
	};

	export let open = true;
	export let diffRows: VibeReviewRow[] = [];
	export let otherRows: VibeReviewRow[] = [];
	export let orderedDiffRows: VibeReviewRow[] = [];
	export let orderedOtherRows: VibeReviewRow[] = [];
	export let autoApplyCodeProposals = true;
	export let bulkApplying = false;
	export let actingKeys: Set<string> = new Set();

	const SOURCE_LABELS: Record<HitlSource, string> = {
		approval: 'Approval',
		clarification: 'Clarification',
		plan_approval: 'Plan approval',
		user_request: 'User request',
		agentic: 'Agentic pause',
		escalation: 'Escalation',
		diff_approval: 'Code changes',
		service_health: 'Service health',
		bot_auth: 'Bot auth'
	};

	const dispatch = createEventDispatcher<{
		hide: void;
		show: void;
		approveAll: void;
		toggleAutoApply: { value: boolean };
		applyDiff: { request: HitlRequest };
		rejectDiff: { request: HitlRequest };
		applyFile: { request: HitlRequest; path: string };
		rejectFile: { request: HitlRequest; path: string };
		respond: { row: VibeReviewRow };
	}>();

	$: reviewPendingCount = diffRows.length + otherRows.length;

	function rowKey(request: HitlRequest): string {
		return (
			request.schema.proposal_id ??
			request.schema.transaction_id ??
			request.identifiers.correlation_id ??
			request.id
		);
	}

	function isProposalDiff(request: HitlRequest): boolean {
		return (
			request.input_type === 'diff_approval' &&
			(request.schema.approval_source === 'proposal' || Boolean(request.schema.proposal_id))
		);
	}

	function diffStat(row: VibeReviewRow, kind: 'additions' | 'deletions'): number {
		return (row.request.schema.files ?? []).reduce((sum, file) => sum + file[kind], 0);
	}

	function relativeTime(ms: number): string {
		const diff = Date.now() - ms;
		const minutes = Math.round(diff / 60_000);
		if (minutes < 1) return 'just now';
		if (minutes < 60) return `${minutes}m ago`;
		const hours = Math.round(minutes / 60);
		if (hours < 48) return `${hours}h ago`;
		const days = Math.round(hours / 24);
		return `${days}d ago`;
	}
</script>

{#if open}
	<aside
		id="vibe-review-panel"
		class="vibe-review-panel"
		aria-label="Review code changes and decisions"
		transition:fly={{ x: 36, duration: 190, easing: cubicOut, opacity: 0.2 }}
	>
		<div class="vibe-review-head">
			<div>
				<h2>Review</h2>
				<p>
					{diffRows.length} change set{diffRows.length === 1 ? '' : 's'} · {otherRows.length}
					decision{otherRows.length === 1 ? '' : 's'}
				</p>
			</div>
			<div class="vibe-review-head__actions">
				<button
					type="button"
					class="vibe-approve-all"
					disabled={diffRows.length === 0 || bulkApplying}
					on:click={() => dispatch('approveAll')}
				>
					{bulkApplying ? 'Applying...' : 'Apply all now'}
				</button>
				<button
					type="button"
					class="vibe-review-toggle vibe-review-toggle--compact"
					aria-label="Hide review panel"
					aria-expanded="true"
					aria-controls="vibe-review-panel"
					on:click={() => dispatch('hide')}
				>Hide</button>
			</div>
		</div>

		<div class="vibe-policy-note" class:vibe-policy-note--on={autoApplyCodeProposals}>
			<div class="vibe-policy-toggle">
				<Checkbox
					label={autoApplyCodeProposals ? 'Auto-apply on' : 'Manual review'}
					checked={autoApplyCodeProposals}
					on:change={(event) =>
						dispatch('toggleAutoApply', {
							value: event.detail.checked
						})}
				/>
			</div>
			<p>
				{autoApplyCodeProposals
					? 'Proposal-backed code diffs apply when they arrive.'
					: 'Code diffs wait here until you apply or reject them.'}
			</p>
		</div>

		<section class="vibe-section" aria-labelledby="vibe-code-heading">
			<div class="vibe-section__head">
				<h2 id="vibe-code-heading">Code changes</h2>
				<span>{diffRows.length}</span>
			</div>

			{#if diffRows.length === 0}
				<div class="vibe-empty">No code changes pending.</div>
			{:else}
				<div class="vibe-diff-list">
					{#each orderedDiffRows as row (row.key)}
						{@const request = row.request}
						{@const files = request.schema.files ?? []}
						{@const key = rowKey(request)}
						<article class="vibe-diff-card">
							<div class="vibe-card-head">
								<div>
									<div class="vibe-card-title">{request.schema.rationale ?? request.prompt}</div>
									<div class="vibe-card-meta">
										<span>{files.length} file{files.length === 1 ? '' : 's'}</span>
										<span>+{diffStat(row, 'additions')} / -{diffStat(row, 'deletions')}</span>
										<span>{relativeTime(row.entry.at)}</span>
										{#if isProposalDiff(request)}
											<span>proposal</span>
										{/if}
									</div>
								</div>
								<div class="vibe-actions">
									<button
										type="button"
										class="vibe-btn vibe-btn--ghost"
										disabled={actingKeys.has(key) || bulkApplying}
										on:click={() => dispatch('rejectDiff', { request })}
									>Reject</button>
									<button
										type="button"
										class="vibe-btn vibe-btn--primary"
										disabled={actingKeys.has(key) || bulkApplying}
										on:click={() => dispatch('applyDiff', { request })}
									>{actingKeys.has(key) ? 'Applying...' : 'Apply'}</button>
								</div>
							</div>
							<DiffStrip
								diff={{ files, note: request.schema.proposal_id ?? request.schema.transaction_id ?? key }}
								lifecycle="pending"
								title="Staged changes"
								showStats={true}
								allowCopy={true}
								autoExpandFirst={true}
								showLineNumbers={true}
								wrapLongLines={true}
								truncateLinesThreshold={500}
								allowPerFileApproval={(request.schema.files?.length ?? 0) > 1}
								on:applyFile={(event) =>
									dispatch('applyFile', { request, path: event.detail.file.path })}
								on:rejectFile={(event) =>
									dispatch('rejectFile', { request, path: event.detail.file.path })}
							/>
						</article>
					{/each}
				</div>
			{/if}
		</section>

		<section class="vibe-section" aria-labelledby="vibe-decisions-heading">
			<div class="vibe-section__head">
				<h2 id="vibe-decisions-heading">Other decisions</h2>
				<span>{otherRows.length}</span>
			</div>

			{#if otherRows.length === 0}
				<div class="vibe-empty">No other decisions pending.</div>
			{:else}
				<div class="vibe-decision-list">
					{#each orderedOtherRows as row (row.key)}
						<article class="vibe-decision-row">
							<div>
								<div class="vibe-source">{SOURCE_LABELS[row.request.source] ?? row.request.source}</div>
								<div class="vibe-decision-title">{row.request.prompt}</div>
								{#if row.request.hint}
									<div class="vibe-decision-hint">{row.request.hint}</div>
								{/if}
							</div>
							<button
								type="button"
								class="vibe-btn vibe-btn--primary"
								on:click={() => dispatch('respond', { row })}
							>
								Respond
							</button>
						</article>
					{/each}
				</div>
			{/if}
		</section>
	</aside>
{:else}
	<aside
		class="vibe-review-rail"
		aria-label="Review panel collapsed"
		transition:fly={{ x: 18, duration: 140, easing: cubicOut, opacity: 0.25 }}
	>
		<button
			type="button"
			class="vibe-review-rail__button"
			aria-expanded="false"
			aria-controls="vibe-review-panel"
			on:click={() => dispatch('show')}
		>
			<span class="vibe-review-rail__label">Review</span>
			{#if reviewPendingCount > 0}
				<span class="vibe-review-rail__count">{reviewPendingCount}</span>
			{/if}
		</button>
	</aside>
{/if}

<style>
	.vibe-approve-all,
	.vibe-review-toggle,
	.vibe-review-rail__button,
	.vibe-btn {
		border: 1px solid var(--vibe-border-strong);
		border-radius: 8px;
		background: var(--vibe-surface);
		color: var(--vibe-text);
		font: inherit;
		font-weight: 700;
		cursor: pointer;
		transition:
			background 0.15s ease,
			border-color 0.15s ease,
			transform 0.15s ease;
	}

	.vibe-approve-all {
		padding: 0.55rem 0.8rem;
	}

	.vibe-approve-all:not(:disabled):hover,
	.vibe-review-toggle:not(:disabled):hover,
	.vibe-review-rail__button:hover,
	.vibe-btn:not(:disabled):hover {
		transform: translateY(-1px);
		border-color: var(--vibe-accent);
	}

	.vibe-approve-all:focus-visible,
	.vibe-review-toggle:focus-visible,
	.vibe-review-rail__button:focus-visible,
	.vibe-btn:focus-visible {
		outline: 2px solid color-mix(in srgb, var(--vibe-accent) 68%, transparent);
		outline-offset: 2px;
	}

	.vibe-approve-all:disabled,
	.vibe-review-toggle:disabled,
	.vibe-btn:disabled {
		cursor: not-allowed;
		opacity: 0.55;
	}

	.vibe-review-toggle {
		display: inline-flex;
		align-items: center;
		gap: 0.35rem;
		padding: 0.55rem 0.75rem;
	}

	.vibe-review-toggle--compact {
		padding: 0.45rem 0.65rem;
	}

	.vibe-review-head {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 1rem;
	}

	.vibe-review-head__actions {
		display: flex;
		align-items: center;
		justify-content: flex-end;
		gap: 0.5rem;
		flex-wrap: wrap;
	}

	.vibe-review-head__actions .vibe-approve-all,
	.vibe-review-head__actions .vibe-review-toggle--compact {
		min-height: 1.75rem;
		border-radius: 6px;
		padding: 0.3rem 0.55rem;
		font-size: 0.7rem;
		font-weight: 850;
		line-height: 1;
	}

	.vibe-review-head__actions .vibe-approve-all {
		border-color: color-mix(in srgb, var(--vibe-accent) 36%, var(--vibe-border));
		background: color-mix(in srgb, var(--vibe-accent) 10%, var(--vibe-surface));
		color: var(--vibe-accent);
	}

	.vibe-review-head__actions .vibe-approve-all:not(:disabled):hover {
		background: color-mix(in srgb, var(--vibe-accent) 15%, var(--vibe-surface));
	}

	.vibe-review-head__actions .vibe-approve-all:disabled {
		border-color: var(--vibe-border);
		background: color-mix(in srgb, var(--vibe-page-surface) 82%, var(--vibe-surface));
		color: var(--vibe-text-muted);
		opacity: 0.55;
	}

	.vibe-review-head__actions .vibe-review-toggle--compact {
		border-color: var(--vibe-border);
		background: color-mix(in srgb, var(--vibe-page-surface) 70%, var(--vibe-surface));
		color: var(--vibe-text-muted);
	}

	.vibe-review-head__actions .vibe-review-toggle--compact:not(:disabled):hover {
		background: color-mix(in srgb, var(--vibe-surface) 92%, var(--vibe-page-surface));
		color: var(--vibe-text);
	}

	.vibe-review-head h2 {
		margin: 0;
		font-size: 1rem;
		line-height: 1.2;
		letter-spacing: 0;
	}

	.vibe-review-head p {
		margin: 0.25rem 0 0;
		color: var(--vibe-text-muted);
		font-size: 0.8rem;
	}

	.vibe-review-panel {
		position: fixed;
		top: 48px;
		right: 1rem;
		bottom: 0;
		z-index: 80;
		width: min(38rem, calc(100vw - 2rem));
		display: flex;
		flex-direction: column;
		gap: 0.85rem;
		overflow: auto;
		border: 1px solid var(--vibe-border);
		border-radius: 8px;
		background: color-mix(in srgb, var(--vibe-surface) 98%, transparent);
		padding: 0.9rem;
		box-shadow: var(--shadow-lg, 0 16px 38px color-mix(in srgb, var(--vibe-text) 16%, transparent));
	}

	.vibe-review-rail {
		position: fixed;
		top: 50%;
		right: 1rem;
		z-index: 79;
		transform: translateY(-50%);
		min-width: 2.9rem;
	}

	.vibe-review-rail__button {
		width: 2.9rem;
		min-height: 8.5rem;
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: 0.5rem;
		padding: 0.65rem 0.35rem;
	}

	.vibe-review-rail__label {
		writing-mode: vertical-rl;
		transform: rotate(180deg);
		font-size: 0.74rem;
		font-weight: 900;
		line-height: 1;
		text-transform: uppercase;
	}

	.vibe-review-rail__count {
		display: inline-grid;
		min-width: 1.2rem;
		height: 1.2rem;
		place-items: center;
		border-radius: 999px;
		background: var(--vibe-accent);
		color: var(--button-primary-color, #fff);
		font-size: 0.68rem;
		font-weight: 900;
	}

	.vibe-policy-note {
		border: 1px solid var(--vibe-border);
		border-radius: 8px;
		background: color-mix(in srgb, var(--vibe-page-surface) 74%, transparent);
		padding: 0.65rem;
	}

	.vibe-policy-note--on {
		border-color: color-mix(in srgb, var(--vibe-success) 48%, transparent);
		background: color-mix(in srgb, var(--vibe-success) 8%, var(--vibe-surface));
	}

	.vibe-policy-toggle {
		display: inline-flex;
		align-items: center;
		margin-bottom: 0.2rem;
		color: var(--vibe-text);
		font-size: 0.78rem;
		font-weight: 900;
	}

	:global(.vibe-policy-toggle .muij-checkbox) {
		font-size: inherit;
		font-weight: inherit;
		color: inherit;
	}

	.vibe-policy-note p {
		margin: 0;
		color: var(--vibe-text-muted);
		font-size: 0.78rem;
		line-height: 1.35;
	}

	.vibe-section {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
	}

	.vibe-section__head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 1rem;
	}

	.vibe-section__head h2 {
		margin: 0;
		font-size: 0.98rem;
		letter-spacing: 0;
	}

	.vibe-section__head span {
		color: var(--vibe-text-muted);
		font-size: 0.78rem;
		font-weight: 700;
	}

	.vibe-empty {
		border: 1px dashed var(--vibe-border);
		border-radius: 8px;
		padding: 1rem;
		color: var(--vibe-text-muted);
		font-size: 0.88rem;
		background: color-mix(in srgb, var(--vibe-page-surface) 72%, transparent);
	}

	.vibe-diff-list,
	.vibe-decision-list {
		display: flex;
		flex-direction: column;
		gap: 0.8rem;
	}

	.vibe-diff-card,
	.vibe-decision-row {
		border: 1px solid var(--vibe-border);
		border-radius: 8px;
		background: color-mix(in srgb, var(--vibe-surface) 96%, transparent);
		box-shadow: var(--shadow-sm, 0 1px 2px color-mix(in srgb, var(--vibe-text) 10%, transparent));
	}

	.vibe-diff-card {
		display: flex;
		flex-direction: column;
		gap: 0.7rem;
		padding: 0.85rem;
	}

	.vibe-card-head,
	.vibe-decision-row {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 1rem;
	}

	.vibe-card-title,
	.vibe-decision-title {
		font-weight: 800;
		font-size: 0.94rem;
		line-height: 1.35;
	}

	.vibe-card-meta,
	.vibe-decision-hint {
		display: flex;
		flex-wrap: wrap;
		gap: 0.45rem;
		margin-top: 0.35rem;
		color: var(--vibe-text-muted);
		font-size: 0.78rem;
	}

	.vibe-actions {
		display: inline-flex;
		align-items: center;
		gap: 0.45rem;
		flex-shrink: 0;
	}

	.vibe-btn {
		min-height: 2rem;
		padding: 0.38rem 0.65rem;
		font-size: 0.78rem;
	}

	.vibe-btn--primary {
		border-color: color-mix(in srgb, var(--vibe-accent) 72%, transparent);
		background: var(--vibe-accent);
		color: var(--button-primary-color, #fff);
	}

	.vibe-btn--ghost {
		background: transparent;
	}

	.vibe-decision-row {
		align-items: center;
		padding: 0.8rem;
	}

	.vibe-source {
		margin-bottom: 0.2rem;
		color: var(--vibe-accent);
		font-size: 0.68rem;
		font-weight: 800;
		text-transform: uppercase;
	}

	@media (max-width: 720px) {
		.vibe-review-panel,
		.vibe-review-rail {
			position: static;
			transform: none;
			width: 100%;
			max-height: none;
		}

		.vibe-card-head,
		.vibe-review-head,
		.vibe-decision-row {
			align-items: stretch;
			flex-direction: column;
		}

		.vibe-review-head__actions {
			align-items: stretch;
			flex-direction: column;
		}

		.vibe-actions {
			width: 100%;
		}

		.vibe-actions .vibe-btn,
		.vibe-approve-all,
		.vibe-review-toggle,
		.vibe-review-rail__button {
			width: 100%;
		}

		.vibe-review-rail__button {
			min-height: 2.4rem;
			flex-direction: row;
		}

		.vibe-review-rail__label {
			writing-mode: horizontal-tb;
			transform: none;
		}
	}
</style>
