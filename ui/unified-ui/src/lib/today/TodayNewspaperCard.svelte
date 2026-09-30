<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import type { ChannelFollowUp } from '$lib/stores/channelNeedsYouStore';
	import type { ResurfacingCard } from './resurfacingQueries';
	import type { CanonicalAttentionItem } from '$lib/attention/canonicalAttentionProjection';

	export let item: CanonicalAttentionItem | null = null;
	export let followUp: ChannelFollowUp | null = null;
	export let worth: ResurfacingCard | null = null;
	export let debug = false;
	export let busy = false;
	export let compact = false;

	const dispatch = createEventDispatcher<{
		action: { kind: 'primary' | 'useful' | 'acknowledge' | 'dismiss' | 'snooze'; reason?: string; actionId?: string };
		read: { followUp?: ChannelFollowUp; worth?: ResurfacingCard };
	}>();

	let showMenu = false;
	let expanded = false;

	$: title = computeTitle(item, followUp, worth);
	$: summary = computeSummary(item, followUp, worth);
	$: category = computeCategory(item, followUp, worth);
	$: primaryLabel = computePrimaryLabel(item, followUp, worth);

	function computeTitle(
		item: CanonicalAttentionItem | null,
		followUp: ChannelFollowUp | null,
		worth: ResurfacingCard | null
	): string {
		if (followUp?.subject?.trim()) return followUp.subject.trim();
		if (worth?.source_title?.trim()) return worth.source_title.trim();
		if (worth?.line?.trim()) return worth.line.trim();
		if (item) {
			if (item.origin_lane === 'follow_up' && 'subject' in item.payload && item.payload.subject?.trim()) {
				return item.payload.subject.trim();
			}
			if ('source_title' in item.payload && item.payload.source_title?.trim()) {
				return item.payload.source_title.trim();
			}
			if ('line' in item.payload && item.payload.line?.trim()) {
				return item.payload.line.trim();
			}
		}
		return 'Untitled Dispatch';
	}

	function computeSummary(
		item: CanonicalAttentionItem | null,
		followUp: ChannelFollowUp | null,
		worth: ResurfacingCard | null
	): string {
		if (followUp?.summary?.trim()) return followUp.summary.trim();
		if (followUp?.reason?.trim()) return followUp.reason.trim();
		if (worth?.summary?.trim()) return worth.summary.trim();
		if (worth?.why_now?.trim()) return worth.why_now.trim();
		if (item) {
			if ('summary' in item.payload && item.payload.summary?.trim()) {
				return item.payload.summary.trim();
			}
			if ('reason' in item.payload && item.payload.reason?.trim()) {
				return item.payload.reason.trim();
			}
			if ('why_now' in item.payload && item.payload.why_now?.trim()) {
				return item.payload.why_now.trim();
			}
		}
		return '';
	}

	function computeCategory(
		item: CanonicalAttentionItem | null,
		followUp: ChannelFollowUp | null,
		worth: ResurfacingCard | null
	): string {
		if (followUp) {
			return `DISPATCH · ${followUp.provider?.toUpperCase() || 'CORRESPONDENCE'}`;
		}
		if (worth) {
			return `READING ROOM · ${worth.source_kind?.replace(/_/g, ' ').toUpperCase() || 'NOTE'}`;
		}
		if (item?.origin_lane === 'follow_up') {
			return 'DISPATCH · CORRESPONDENCE';
		}
		return 'READING ROOM · KNOWLEDGE';
	}

	function computePrimaryLabel(
		item: CanonicalAttentionItem | null,
		followUp: ChannelFollowUp | null,
		worth: ResurfacingCard | null
	): string | null {
		// The parent maps primary to `approve` (follow-up) and `open` (worth),
		// so the label names that action rather than a descriptor it won't run.
		if (item && !followUp && !worth) {
			const approveAct = item.actions.find(a => a.kind === 'approve');
			if (approveAct) return approveAct.label;
		}
		return followUp ? 'Do it' : worth ? 'Open' : null;
	}

	const DISMISS_REASONS = [
		{ code: 'already_handled', label: 'Already handled' },
		{ code: 'not_relevant', label: 'Not relevant' },
		{ code: 'wrong_classification', label: "Shouldn't be flagged", followUpOnly: true },
		{ code: 'spam', label: 'Spam / low priority' }
	];

	// Follow-up snooze carries no duration on the wire (the annotation drops
	// out of Today); resurfacing has no snooze at all. Offer only what exists.
	$: canSnooze = Boolean(followUp) || item?.origin_lane === 'follow_up';

	function handlePrimary(): void {
		dispatch('action', { kind: 'primary' });
	}

	function handleUseful(): void {
		dispatch('action', { kind: 'useful' });
	}

	function handleAcknowledge(): void {
		dispatch('action', { kind: 'acknowledge' });
	}

	function handleDismiss(reason?: string): void {
		showMenu = false;
		dispatch('action', { kind: 'dismiss', reason });
	}

	function handleSnooze(): void {
		showMenu = false;
		dispatch('action', { kind: 'snooze' });
	}

	let dismissWrapEl: HTMLElement | null = null;
	let caretBtnEl: HTMLButtonElement | null = null;

	function handleWindowClick(event: MouseEvent): void {
		if (!showMenu) return;
		const target = event.target as Node;
		if (dismissWrapEl && dismissWrapEl.contains(target)) return;
		showMenu = false;
	}

	function handleWindowKeydown(event: KeyboardEvent): void {
		if (!showMenu || event.key !== 'Escape') return;
		event.preventDefault();
		showMenu = false;
		caretBtnEl?.focus();
	}

	function toggleExpand(e: MouseEvent | KeyboardEvent): void {
		// Only expand if clicking the card body, not an interactive button
		const target = e.target as HTMLElement;
		if (target.closest('button') || target.closest('a') || target.closest('.np-menu')) return;
		expanded = !expanded;
		if (expanded) {
			dispatch('read', { followUp: followUp ?? undefined, worth: worth ?? undefined });
		}
	}
</script>

<svelte:window on:click={handleWindowClick} on:keydown={handleWindowKeydown} />

<!-- svelte-ignore a11y_click_events_have_key_events -->
<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<article
	class="np-card"
	class:np-card--compact={compact}
	class:np-card--expanded={expanded}
	class:np-card--busy={busy}
	on:click={toggleExpand}
>
	<!-- Eyebrow / Kicker -->
	<header class="np-card__header">
		<div class="np-card__kicker">
			<span class="np-kicker-dot"></span>
			<span class="np-kicker-text">{category}</span>
			{#if followUp?.sender}
				<span class="np-kicker-sep">·</span>
				<span class="np-kicker-sender">{followUp.sender}</span>
			{/if}
		</div>
		{#if debug}
			<div class="np-debug-tag" title="Attention bandit / rank diagnostic">
				{#if item?.served_lane}
					<span>lane: {item.served_lane}</span>
				{/if}
				{#if worth?.learning_score != null}
					<span>score: {worth.learning_score.toFixed(2)}</span>
				{/if}
			</div>
		{/if}
	</header>

	<!-- Headline -->
	<h3 class="np-card__headline">
		{title}
	</h3>

	<!-- Summary / Deck -->
	{#if summary}
		<p class="np-card__summary">
			{summary}
		</p>
	{/if}

	<!-- Expanded Body / Evidence (Progressive on-demand) -->
	{#if expanded}
		<div class="np-card__expanded-content">
			{#if followUp?.open_url}
				<div class="np-expanded-row">
					<a href={followUp.open_url} target="_blank" rel="noopener noreferrer" class="np-link-out">
						Open original in {followUp.provider || 'app'}
						<svg viewBox="0 0 24 24" width="12" height="12" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
							<line x1="7" y1="17" x2="17" y2="7"></line>
							<polyline points="7 7 17 7 17 17"></polyline>
						</svg>
					</a>
				</div>
			{/if}
			{#if worth?.why_now && worth.why_now !== summary}
				<div class="np-expanded-why">
					<strong>Why surfaced:</strong> {worth.why_now}
				</div>
			{/if}
		</div>
	{/if}

	<!-- Footer: Maximum 4 Well-Placed Actions -->
	<footer class="np-card__footer" aria-label="Card actions">
		<div class="np-actions">
			<!-- 1. Primary contextual action (if exists) -->
			{#if primaryLabel}
				<button
					type="button"
					class="np-btn np-btn--primary"
					disabled={busy}
					on:click|stopPropagation={handlePrimary}
					title="Execute or start this item"
				>
					<span class="np-btn-icon">⚡</span>
					<span>{primaryLabel}</span>
				</button>
			{/if}

			<!-- 2. Mark Useful (Positive feedback) -->
			<button
				type="button"
				class="np-btn np-btn--useful"
				disabled={busy}
				on:click|stopPropagation={handleUseful}
				title="Mark useful — nudges similar items up in your morning paper"
			>
				<Icon name="check" size={13} />
				<span>Useful</span>
			</button>

			<!-- 3. Acknowledge (Neutral feedback) -->
			<button
				type="button"
				class="np-btn np-btn--ack"
				disabled={busy}
				on:click|stopPropagation={handleAcknowledge}
				title="Acknowledge — clear without altering model preference"
			>
				<Icon name="archive" size={13} />
				<span>Seen</span>
			</button>

			<!-- 4. Dismiss split-button with progressive reason popover -->
			<div class="np-dismiss-wrap" bind:this={dismissWrapEl}>
				<div class="np-split-btn">
					<button
						type="button"
						class="np-btn np-btn--dismiss np-split-main"
						disabled={busy}
						on:click|stopPropagation={() => handleDismiss()}
						title="Dismiss item"
					>
						<Icon name="x" size={13} />
						<span>Dismiss</span>
					</button>
					<button
						type="button"
						class="np-btn np-btn--dismiss np-split-caret"
						disabled={busy}
						aria-label="More dismissal and snooze options"
						aria-haspopup="menu"
						aria-expanded={showMenu}
						bind:this={caretBtnEl}
						on:click|stopPropagation={() => (showMenu = !showMenu)}
					>
						▾
					</button>
				</div>

				<!-- Progressive Disclosure Popover -->
				{#if showMenu}
					<div class="np-menu" role="menu" tabindex="-1" on:click|stopPropagation on:keydown|stopPropagation>
						<div class="np-menu__section-title">Dismiss because…</div>
						{#each DISMISS_REASONS.filter((r) => !(worth && r.followUpOnly)) as reason (reason.code)}
							<button
								type="button"
								class="np-menu-item"
								role="menuitem"
								on:click={() => handleDismiss(reason.code)}
							>
								{reason.label}
							</button>
						{/each}

						{#if canSnooze}
							<div class="np-menu__divider"></div>
							<button
								type="button"
								class="np-menu-item"
								role="menuitem"
								on:click={() => handleSnooze()}
							>
								Snooze — hide from Today
							</button>
						{/if}
					</div>
				{/if}
			</div>
		</div>
	</footer>
</article>

<style>
	.np-card {
		position: relative;
		display: flex;
		flex-direction: column;
		gap: 0.65rem;
		padding: 1.1rem 1.25rem;
		background: color-mix(in srgb, var(--bg-card, #ffffff) 96%, transparent);
		border: 1px solid color-mix(in srgb, var(--border-soft, rgba(0, 0, 0, 0.12)) 80%, transparent);
		border-radius: var(--radius-md, 8px);
		box-shadow: 0 1px 3px rgba(0, 0, 0, 0.03);
		transition: transform 0.18s var(--ease-settle, ease), box-shadow 0.18s var(--ease-settle, ease), border-color 0.18s ease;
		cursor: pointer;
	}

	.np-card:hover {
		border-color: color-mix(in srgb, var(--border-default, rgba(0, 0, 0, 0.22)) 90%, transparent);
		box-shadow: 0 4px 12px rgba(0, 0, 0, 0.05);
	}

	.np-card--busy {
		opacity: 0.6;
		pointer-events: none;
	}

	.np-card--compact {
		padding: 0.85rem 1rem;
		gap: 0.45rem;
	}

	.np-card__header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
	}

	.np-card__kicker {
		display: inline-flex;
		align-items: center;
		gap: 0.45rem;
		font-family: var(--font-mono, monospace);
		font-size: 0.68rem;
		font-weight: 600;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		color: var(--text-muted, #71717a);
	}

	.np-kicker-dot {
		width: 5px;
		height: 5px;
		border-radius: 50%;
		background: var(--accent-primary, #b45309);
	}

	.np-kicker-sep {
		opacity: 0.5;
	}

	.np-kicker-sender {
		font-weight: 500;
		color: var(--text-secondary, #52525b);
		text-transform: none;
		max-width: 14rem;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.np-debug-tag {
		font-family: var(--font-mono, monospace);
		font-size: 0.65rem;
		background: color-mix(in srgb, var(--color-info, #2563eb) 12%, transparent);
		color: var(--color-info, #2563eb);
		padding: 0.1rem 0.35rem;
		border-radius: 4px;
		display: flex;
		gap: 0.35rem;
	}

	.np-card__headline {
		margin: 0;
		font-family: var(--font-display, 'Newsreader', serif);
		font-size: 1.15rem;
		font-weight: 600;
		line-height: 1.32;
		color: var(--text-primary, #18181b);
		letter-spacing: -0.01em;
	}

	.np-card--compact .np-card__headline {
		font-size: 1.05rem;
	}

	.np-card__summary {
		margin: 0;
		font-family: var(--font-primary, sans-serif);
		font-size: 0.88rem;
		line-height: 1.5;
		color: var(--text-secondary, #52525b);
	}

	.np-card__expanded-content {
		margin-top: 0.25rem;
		padding-top: 0.5rem;
		border-top: 1px dashed color-mix(in srgb, var(--border-soft) 70%, transparent);
		font-size: 0.82rem;
		color: var(--text-secondary);
	}

	.np-expanded-row {
		margin-bottom: 0.35rem;
	}

	.np-link-out {
		display: inline-flex;
		align-items: center;
		gap: 0.25rem;
		color: var(--accent-primary);
		text-decoration: underline;
		font-size: 0.8rem;
	}

	.np-expanded-why {
		font-style: italic;
		color: var(--text-muted);
	}

	/* Footer: Max 4 Buttons */
	.np-card__footer {
		margin-top: 0.25rem;
		display: flex;
		align-items: center;
		justify-content: flex-start;
	}

	.np-actions {
		display: flex;
		align-items: center;
		gap: 0.45rem;
		flex-wrap: wrap;
	}

	.np-btn {
		display: inline-flex;
		align-items: center;
		gap: 0.35rem;
		font-family: var(--font-primary, sans-serif);
		font-size: 0.78rem;
		font-weight: 500;
		line-height: 1;
		padding: 0.38rem 0.65rem;
		border-radius: var(--radius-sm, 6px);
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.15));
		background: var(--bg-surface, #fbfaf8);
		color: var(--text-secondary, #3f3f46);
		cursor: pointer;
		white-space: nowrap;
		transition: background 0.15s ease, color 0.15s ease, border-color 0.15s ease;
	}

	.np-btn:hover {
		background: var(--bg-card, #ffffff);
		color: var(--text-primary, #18181b);
		border-color: var(--border-default, rgba(0, 0, 0, 0.25));
	}

	.np-btn--primary {
		background: var(--accent-primary, #b45309);
		border-color: transparent;
		color: #ffffff;
		font-weight: 600;
	}

	.np-btn--primary:hover {
		background: color-mix(in srgb, var(--accent-primary, #b45309) 88%, #000);
		color: #ffffff;
	}

	.np-btn--useful:hover {
		background: color-mix(in srgb, #10b981 12%, var(--bg-card));
		border-color: #10b981;
		color: #047857;
	}

	.np-btn--ack:hover {
		background: color-mix(in srgb, var(--accent-primary, #b45309) 10%, var(--bg-card));
		border-color: var(--accent-primary, #b45309);
		color: var(--accent-primary, #b45309);
	}

	.np-btn--dismiss:hover {
		background: color-mix(in srgb, #ef4444 10%, var(--bg-card));
		border-color: #ef4444;
		color: #b91c1c;
	}

	.np-btn-icon {
		font-size: 0.85rem;
		line-height: 1;
	}

	/* Split button for Dismiss */
	.np-dismiss-wrap {
		position: relative;
		display: inline-flex;
	}

	.np-split-btn {
		display: inline-flex;
	}

	.np-split-main {
		border-top-right-radius: 0;
		border-bottom-right-radius: 0;
	}

	.np-split-caret {
		border-top-left-radius: 0;
		border-bottom-left-radius: 0;
		border-left: 0;
		padding-left: 0.35rem;
		padding-right: 0.35rem;
		font-size: 0.72rem;
	}

	/* Progressive Menu */
	.np-menu {
		position: absolute;
		bottom: calc(100% + 0.35rem);
		right: 0;
		z-index: 30;
		min-width: 13rem;
		display: flex;
		flex-direction: column;
		padding: 0.35rem;
		border-radius: var(--radius-sm, 6px);
		border: 1px solid var(--border-default, rgba(0, 0, 0, 0.2));
		background: var(--bg-card, #ffffff);
		box-shadow: 0 8px 24px rgba(0, 0, 0, 0.15);
	}

	.np-menu__section-title {
		font-family: var(--font-mono, monospace);
		font-size: 0.65rem;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		color: var(--text-muted, #71717a);
		padding: 0.3rem 0.5rem 0.15rem;
	}

	.np-menu__divider {
		height: 1px;
		background: var(--border-soft, rgba(0, 0, 0, 0.1));
		margin: 0.25rem 0;
	}

	.np-menu-item {
		text-align: left;
		font-family: var(--font-primary, sans-serif);
		font-size: 0.78rem;
		padding: 0.38rem 0.55rem;
		border: 0;
		background: transparent;
		color: var(--text-secondary, #3f3f46);
		border-radius: 4px;
		cursor: pointer;
		transition: background 0.12s ease, color 0.12s ease;
	}

	.np-menu-item:hover {
		background: color-mix(in srgb, var(--accent-primary) 8%, var(--bg-surface));
		color: var(--text-primary, #18181b);
	}
</style>
