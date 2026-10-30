<script lang="ts">
	/**
	 * The Morning Brief deck's detail view for its top card.
	 *
	 * It hosts the existing full control sets rather than re-implementing any:
	 * `ChannelFollowUpActions` for a follow-up (instruction before a task,
	 * compose/send for declared channel actions, reminder, message body) and
	 * `ResurfacingDetailPanel` + `ResurfacingCardActions` for a Worth card. The
	 * deck's Act menu opens this view and then `start()`s the chosen entry, so
	 * every action runs through the flow its own button uses.
	 *
	 * It renders inline under the deck, not in a modal: those control sets open
	 * their own modals, and a modal around them would fight their focus traps.
	 */
	import { createEventDispatcher, onDestroy } from 'svelte';

	import ChannelFollowUpActions from '$lib/channel/ChannelFollowUpActions.svelte';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import { showError, showWarning } from '$lib/shared/stores/notifications';
	import { openWorthLinkInGesture, type DeckActEntry } from './deckActMenu';
	import ResurfacingCardActions from './ResurfacingCardActions.svelte';
	import ResurfacingDetailPanel from './ResurfacingDetailPanel.svelte';
	import {
		fetchResurfacingDetail,
		fetchResurfacingOriginal,
		type ResurfacingActionKind,
		type ResurfacingContextualActionResult,
		type ResurfacingDetail
	} from './resurfacingQueries';
	import type { TriageCardData } from './TodayTriageDeck.svelte';

	export let card: TriageCardData;
	export let chatThreadId: string | null = null;

	const dispatch = createEventDispatcher<{
		close: void;
		/** The follow-up's own action resolved it server-side; drop the card. */
		resolved: { message: string };
		/** Worth "Open source": the deck's Open (mark useful + open). */
		open: void;
	}>();

	$: followUp = card.rawFollowUp ?? null;
	$: worth = card.rawWorth ?? null;

	let followUpActions: ChannelFollowUpActions | null = null;
	let cardActions: ResurfacingCardActions | null = null;

	let detail: ResurfacingDetail | null = null;
	let detailLoading = false;
	let detailError: string | null = null;
	let originalLoading = false;
	let deeperSummary: Extract<ResurfacingContextualActionResult, { kind: 'deeper_summary' }> | null = null;
	let requestId = 0;
	let destroyed = false;
	let loadedFor: string | null = null;

	onDestroy(() => {
		destroyed = true;
	});

	$: if (worth && loadedFor !== worth.candidate_id) {
		loadedFor = worth.candidate_id;
		detail = null;
		deeperSummary = null;
		void loadDetail();
	}

	async function loadDetail(): Promise<void> {
		if (!worth) return;
		const candidateId = worth.candidate_id;
		const rid = ++requestId;
		detailLoading = true;
		detailError = null;
		try {
			const loaded = await fetchResurfacingDetail(candidateId);
			if (destroyed || rid !== requestId) return;
			detail = loaded;
		} catch (error) {
			if (destroyed || rid !== requestId) return;
			detailError = error instanceof Error ? error.message : 'Current details are unavailable.';
		} finally {
			if (!destroyed && rid === requestId) detailLoading = false;
		}
	}

	async function showOriginal(): Promise<void> {
		if (!worth || detail?.original) return;
		const candidateId = worth.candidate_id;
		const rid = ++requestId;
		originalLoading = true;
		try {
			const loaded = await fetchResurfacingOriginal(candidateId);
			if (destroyed || rid !== requestId) return;
			detail = loaded;
			if (!loaded.original) showWarning('Original content is unavailable');
		} catch (error) {
			if (destroyed || rid !== requestId) return;
			showError(
				'Could not load the original',
				error instanceof Error ? error.message : 'Original content is unavailable.'
			);
		} finally {
			if (!destroyed && rid === requestId) originalLoading = false;
		}
	}

	function runWorthKind(kind: ResurfacingActionKind | string): void {
		if (kind === 'view_details') void loadDetail();
		else if (kind === 'show_original') void showOriginal();
		else if (kind === 'open_source') {
			openWorthLinkInGesture(worth);
			dispatch('open');
		}
		else cardActions?.startAction(kind as ResurfacingActionKind);
	}

	function showDeeperSummary(summary: unknown): void {
		deeperSummary = summary as Extract<ResurfacingContextualActionResult, { kind: 'deeper_summary' }>;
	}

	/** Run an Act-menu entry through this view's own controls. */
	export function start(entry: DeckActEntry): void {
		if (entry.kind === 'channel_action') followUpActions?.startChannelAction(entry.descriptor.id);
		else if (entry.kind === 'start_task') followUpActions?.startFollowUpTask();
		else if (entry.kind === 'resurfacing_action') runWorthKind(entry.action);
	}
</script>

<section class="deck-detail" data-testid="today-deck-detail" aria-label="Card details">
	{#if followUp}
		<header class="deck-detail__top">
			<span class="deck-detail__kicker">Details</span>
			<button
				type="button"
				class="deck-detail__close"
				aria-label="Close details"
				title="Close details"
				on:click={() => dispatch('close')}
			>
				<Icon name="x" size={16} />
			</button>
		</header>
		{#if followUp.reason?.trim() && followUp.reason.trim() !== followUp.summary?.trim()}
			<p class="deck-detail__reason">{followUp.reason.trim()}</p>
		{/if}
		<ChannelFollowUpActions
			{followUp}
			bind:this={followUpActions}
			on:resolved={(event) => dispatch('resolved', { message: event.detail.message })}
			on:failed={(event) => showError('Action failed', event.detail.error)}
		/>
	{:else if worth}
		<ResurfacingDetailPanel
			card={worth}
			{detail}
			loading={detailLoading}
			loadError={detailError}
			{originalLoading}
			{deeperSummary}
			on:close={() => dispatch('close')}
			on:action={(event) => runWorthKind(event.detail.kind)}
		/>
		<div class="deck-detail__more">
			<ResurfacingCardActions
				card={worth}
				{detail}
				{chatThreadId}
				bind:this={cardActions}
				on:read={(event) => runWorthKind(event.detail.kind)}
				on:refresh={() => void loadDetail()}
				on:deepersummary={(event) => showDeeperSummary(event.detail.summary)}
			/>
		</div>
	{/if}
</section>

<style>
	.deck-detail {
		width: 100%;
		max-width: 40rem;
		margin-top: 1rem;
		padding: 0.9rem 1.1rem;
		display: grid;
		gap: 0.65rem;
		background: var(--bg-card, #ffffff);
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.12));
		border-radius: var(--radius-lg, 12px);
		box-sizing: border-box;
		user-select: text;
	}

	.deck-detail__top {
		display: flex;
		align-items: center;
		justify-content: space-between;
	}

	.deck-detail__kicker {
		font-family: var(--font-mono, monospace);
		font-size: 0.68rem;
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.08em;
		color: var(--text-muted);
	}

	.deck-detail__close {
		display: inline-flex;
		padding: 0.2rem;
		border: 0;
		background: transparent;
		color: var(--text-secondary);
		cursor: pointer;
	}

	.deck-detail__reason {
		margin: 0;
		font-size: 0.88rem;
		line-height: 1.5;
		color: var(--text-secondary);
	}

	.deck-detail__more {
		display: flex;
		align-items: center;
		justify-content: flex-end;
		gap: 0.5rem;
		font-size: 0.78rem;
		color: var(--text-muted);
	}
</style>
