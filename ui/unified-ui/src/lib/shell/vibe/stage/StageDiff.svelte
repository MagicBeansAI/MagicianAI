<script lang="ts">
	/**
	 * Stage · Diff — consolidated per-file/per-hunk review, folding the old
	 * floating VibeReviewPanel into the cockpit stage. Code changes (DiffStrip)
	 * + the auto-apply policy toggle + Apply-all, plus the non-diff "Other
	 * decisions" HITL responder. Wired to `vibeHitlStore` by the shell.
	 */
	import { createEventDispatcher, onMount, tick } from 'svelte';
	import Checkbox from '$lib/magician/components/generative/Checkbox.svelte';
	import DiffStrip from '$lib/shell/DiffStrip.svelte';
	import type { HitlRequest, HitlSource } from '$lib/hitl/types';
	import {
		isPendingDiffRow,
		noticeParts,
		splitNoticeRows,
		type VibeRow
	} from '$lib/stores/vibeHitlStore';

	export let orderedDiffRows: VibeRow[] = [];
	export let orderedOtherRows: VibeRow[] = [];
	export let autoApplyCodeProposals = false;
	export let bulkApplying = false;
	export let actingKeys: Set<string> = new Set();
	/** Optional deep-link from the Code tab: on mount, scroll to the change set
	 *  that touches this file (the tab switch remounts this component, so a
	 *  one-shot mount scroll is all the hook needs). */
	export let scrollToPath: string | null = null;

	let cardEls: HTMLElement[] = [];
	onMount(async () => {
		if (!scrollToPath) return;
		await tick();
		const idx = orderedDiffRows.findIndex((row) =>
			(row.request.schema.files ?? []).some((f) => f.path === scrollToPath)
		);
		if (idx >= 0) cardEls[idx]?.scrollIntoView({ block: 'start' });
	});

	const SOURCE_LABELS: Record<HitlSource, string> = {
		approval: 'Approval',
		clarification: 'Clarification',
		plan_approval: 'Plan approval',
		user_request: 'User request',
		agentic: 'Agentic pause',
		escalation: 'Escalation',
		diff_approval: 'Code changes',
		service_health: 'Service health',
		mcp_oauth: 'Sign-in',
		bot_auth: 'Bot auth'
	};

	const dispatch = createEventDispatcher<{
		approveAll: void;
		toggleAutoApply: { value: boolean };
		applyDiff: { request: HitlRequest };
		rejectDiff: { request: HitlRequest };
		applyFile: { request: HitlRequest; path: string };
		rejectFile: { request: HitlRequest; path: string };
		respond: { row: VibeRow };
		acknowledge: { row: VibeRow };
	}>();

	// Only a pending proposal is reviewed: a durable row of a finished run is
	// already applied or rejected, and a briefing is a notice, not a decision.
	$: pendingDiffCount = orderedDiffRows.filter(isPendingDiffRow).length;
	$: split = splitNoticeRows(orderedOtherRows);
	$: noticeRows = split.notices;
	$: decisionRows = split.decisions;

	function resolvedLabel(status: unknown): string {
		const s = String(status ?? '').toLowerCase();
		if (s.includes('reject')) return 'Rejected';
		if (s.includes('partial')) return 'Partially applied';
		if (s.includes('appl')) return 'Applied';
		return 'Resolved';
	}
	function noticeTitle(row: VibeRow): string | null {
		const { title, message } = noticeParts(row.request);
		return title && !message.startsWith(title) ? title : null;
	}
	function plural(count: number, word: string): string {
		return `${count} ${word}${count === 1 ? '' : 's'}`;
	}
	$: resolvedDiffCount = orderedDiffRows.length - pendingDiffCount;
	$: summaryParts = [
		pendingDiffCount > 0 ? plural(pendingDiffCount, 'change set') : null,
		resolvedDiffCount > 0 ? plural(resolvedDiffCount, 'resolved change set') : null,
		decisionRows.length > 0 ? plural(decisionRows.length, 'decision') : null,
		noticeRows.length > 0 ? plural(noticeRows.length, 'notice') : null
	].filter((part): part is string => Boolean(part));

	function rowKey(request: HitlRequest): string {
		return (
			request.schema.proposal_id ??
			request.schema.transaction_id ??
			request.identifiers.correlation_id ??
			request.id
		);
	}
	function diffStat(row: VibeRow, kind: 'additions' | 'deletions'): number {
		return (row.request.schema.files ?? []).reduce((sum, file) => sum + file[kind], 0);
	}
	function relativeTime(ms: number): string {
		const diff = Date.now() - ms;
		const minutes = Math.round(diff / 60_000);
		if (minutes < 1) return 'just now';
		if (minutes < 60) return `${minutes}m ago`;
		const hours = Math.round(minutes / 60);
		if (hours < 48) return `${hours}h ago`;
		return `${Math.round(hours / 24)}d ago`;
	}
</script>

<div class="diff-tab">
	<div class="diff-tab__bar">
		<div class="policy" class:policy--on={autoApplyCodeProposals}>
			<Checkbox
				label={autoApplyCodeProposals ? 'Auto-apply on' : 'Manual review'}
				checked={autoApplyCodeProposals}
				on:change={(event) => dispatch('toggleAutoApply', { value: event.detail.checked })}
			/>
		</div>
		<span class="diff-tab__count">{summaryParts.length > 0 ? summaryParts.join(' · ') : 'Nothing to review'}</span>
		{#if pendingDiffCount > 0}
			<button
				type="button"
				class="vbtn vbtn--primary"
				disabled={bulkApplying}
				on:click={() => dispatch('approveAll')}
			>{bulkApplying ? 'Applying…' : 'Apply all changes'}</button>
		{/if}
	</div>

	<div class="diff-tab__scroll">
		{#if orderedDiffRows.length === 0 && orderedOtherRows.length === 0}
			<div class="diff-tab__empty">
				Nothing to review. Code changes the coding agent stages will appear here, sandboxed until you apply them.
			</div>
		{/if}

		{#each noticeRows as row (row.key)}
			{@const title = noticeTitle(row)}
			<article class="notice-row" aria-label="Notice">
				<div>
					<div class="decision-row__source">Notice</div>
					{#if title}<div class="decision-row__title">{title}</div>{/if}
					<div class="notice-row__message">{noticeParts(row.request).message}</div>
				</div>
				<button
					type="button"
					class="vbtn"
					disabled={actingKeys.has(row.key)}
					on:click={() => dispatch('acknowledge', { row })}
				>{actingKeys.has(row.key) ? 'Acknowledging…' : 'Acknowledge'}</button>
			</article>
		{/each}

		{#each orderedDiffRows as row, rowIndex (row.key)}
			{@const request = row.request}
			{@const files = request.schema.files ?? []}
			{@const key = rowKey(request)}
			{@const pending = isPendingDiffRow(row)}
			<article class="diff-card" class:diff-card--resolved={!pending} bind:this={cardEls[rowIndex]}>
				<header class="diff-card__head">
					<div>
						<div class="diff-card__title">{request.schema.rationale ?? request.prompt}</div>
						<div class="diff-card__meta">
							<span>{files.length} file{files.length === 1 ? '' : 's'}</span>
							<span>+{diffStat(row, 'additions')} / -{diffStat(row, 'deletions')}</span>
							<span>{relativeTime(row.entry.at)}</span>
							{#if pending}<span class="diff-card__sandbox">Sandboxed · 0 to repo</span>{/if}
						</div>
					</div>
					{#if pending}
						<div class="diff-card__actions">
							<button type="button" class="vbtn vbtn--ghost" disabled={actingKeys.has(key) || bulkApplying} on:click={() => dispatch('rejectDiff', { request })}>Reject</button>
							<button type="button" class="vbtn vbtn--primary" disabled={actingKeys.has(key) || bulkApplying} on:click={() => dispatch('applyDiff', { request })}>{actingKeys.has(key) ? 'Applying…' : 'Apply'}</button>
						</div>
					{:else}
						<span class="diff-card__status">{resolvedLabel(request.raw?.status)}</span>
					{/if}
				</header>
				<DiffStrip
					diff={{ files, note: request.schema.proposal_id ?? request.schema.transaction_id ?? key }}
					lifecycle={pending ? 'pending' : 'applied'}
					title={pending ? 'Staged changes' : 'Changes'}
					view="split"
					showStats={true}
					allowCopy={true}
					autoExpandFirst={true}
					showLineNumbers={true}
					wrapLongLines={true}
					syntaxHighlight={true}
					wordLevelDiff={true}
					pollIntervalMs={0}
					truncateLinesThreshold={500}
					allowPerFileApproval={pending && files.length > 1}
					on:applyFile={(e) => dispatch('applyFile', { request, path: e.detail.file.path })}
					on:rejectFile={(e) => dispatch('rejectFile', { request, path: e.detail.file.path })}
				/>
			</article>
		{/each}

		{#if decisionRows.length > 0}
			<h3 class="diff-tab__section">Other decisions</h3>
			{#each decisionRows as row (row.key)}
				<article class="decision-row">
					<div>
						<div class="decision-row__source">{SOURCE_LABELS[row.request.source] ?? row.request.source}</div>
						<div class="decision-row__title">{row.request.prompt}</div>
						{#if row.request.hint}<div class="decision-row__hint">{row.request.hint}</div>{/if}
					</div>
					<button type="button" class="vbtn vbtn--primary" on:click={() => dispatch('respond', { row })}>Respond</button>
				</article>
			{/each}
		{/if}
	</div>
</div>

<style>
	.diff-tab {
		display: flex;
		flex-direction: column;
		min-height: 0;
		height: 100%;
	}
	.diff-tab__bar {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		padding: 0.6rem 0.8rem;
		border-bottom: 1px solid var(--vibe-border);
	}
	.diff-tab__count {
		margin-left: auto;
		font-size: 0.74rem;
		color: var(--vibe-text-muted);
	}
	.diff-tab__scroll {
		flex: 1;
		min-height: 0;
		overflow: auto;
		display: flex;
		flex-direction: column;
		gap: 0.8rem;
		padding: 0.8rem;
	}
	.diff-tab__empty {
		margin: auto;
		max-width: 24rem;
		text-align: center;
		color: var(--vibe-text-muted);
		font-size: 0.88rem;
		line-height: 1.5;
		padding: 2rem 1rem;
	}
	.diff-tab__section {
		margin: 0.4rem 0 0;
		font-family: var(--font-display, inherit);
		font-size: 0.85rem;
		font-weight: 600;
		color: var(--vibe-text-muted);
	}

	.policy {
		display: inline-flex;
		align-items: center;
		font-size: 0.76rem;
		font-weight: 600;
		color: var(--vibe-text);
	}

	:global(.policy .muij-checkbox) {
		font-size: inherit;
		font-weight: inherit;
		color: inherit;
	}

	.policy--on {
		color: var(--vibe-success);
	}

	.diff-card {
		display: flex;
		flex-direction: column;
		gap: 0.6rem;
		border: 1px solid var(--vibe-border);
		border-radius: var(--radius-md, 18px);
		background: var(--vibe-surface);
		padding: 0.85rem;
	}
	.diff-card__head {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 1rem;
	}
	.diff-card__title {
		font-weight: 600;
		font-size: 0.92rem;
		line-height: 1.35;
	}
	.diff-card__meta {
		display: flex;
		flex-wrap: wrap;
		gap: 0.45rem;
		margin-top: 0.3rem;
		color: var(--vibe-text-muted);
		font-size: 0.74rem;
	}
	.diff-card__sandbox {
		color: var(--vibe-success);
	}
	.diff-card__actions {
		display: inline-flex;
		gap: 0.45rem;
		flex-shrink: 0;
	}

	.diff-card__status {
		flex-shrink: 0;
		color: var(--vibe-text-muted);
		font-size: 0.76rem;
		font-weight: 600;
	}

	.decision-row,
	.notice-row {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 1rem;
		border: 1px solid var(--vibe-border);
		border-radius: var(--radius-md, 18px);
		background: var(--vibe-surface);
		padding: 0.8rem;
	}
	.decision-row__source {
		font-size: 0.66rem;
		font-weight: 700;
		letter-spacing: 0.03em;
		color: var(--vibe-accent);
		margin-bottom: 0.18rem;
	}
	.decision-row__title {
		font-weight: 600;
		font-size: 0.9rem;
		line-height: 1.35;
	}
	.notice-row__message {
		color: var(--vibe-text);
		font-size: 0.84rem;
		line-height: 1.45;
		white-space: pre-line;
	}
	.decision-row__hint {
		margin-top: 0.25rem;
		color: var(--vibe-text-muted);
		font-size: 0.78rem;
	}

	.vbtn {
		min-height: 2rem;
		border: 1px solid var(--vibe-border-strong);
		border-radius: var(--radius-sm, 10px);
		background: var(--vibe-surface);
		color: var(--vibe-text);
		font: inherit;
		font-size: 0.78rem;
		font-weight: 600;
		padding: 0.35rem 0.7rem;
		cursor: pointer;
	}
	.vbtn--ghost {
		background: transparent;
	}
	.vbtn--primary {
		border-color: color-mix(in srgb, var(--vibe-accent) 72%, transparent);
		background: var(--accent-fill);
		color: var(--button-primary-color, #fff);
	}
	.vbtn:disabled {
		opacity: 0.55;
		cursor: not-allowed;
	}

	@media (max-width: 720px) {
		.diff-card__head,
		.decision-row,
		.notice-row {
			flex-direction: column;
			align-items: stretch;
		}
		.diff-card__actions .vbtn {
			flex: 1;
		}
	}
</style>
