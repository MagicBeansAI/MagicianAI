<script lang="ts">
	import ResurfacingActionDialog from '$lib/today/ResurfacingActionDialog.svelte';
	import { postChannelFollowUpContextualAction } from '$lib/today/resurfacingQueries';
	// Shared interaction affordances for a channel follow-up — the Do it /
	// Acknowledged / Snooze / Dismiss / Open actions, an optional agent HINT for
	// "Do it", and an on-demand "Show message" expander (fetches the actual body
	// live; it's never stored). Used wherever channel follow-ups are
	// surfaced so the wiring lives in one place.
	// Card-removing lifecycle calls enter the shared optimistic queue before
	// transport starts. Owning lists subscribe to that queue, so the card leaves
	// immediately and reappears automatically if the call fails.
	import { createEventDispatcher, onDestroy } from 'svelte';
	import AttentionActionabilityBadge from '$lib/attention/AttentionActionabilityBadge.svelte';
	import { attentionFeedbackAttribution } from '$lib/attention/attentionVisibility';
	import {
		followUpAttentionMutationKey,
		optimisticAttentionMutationQueue
	} from '$lib/attention/optimisticAttentionMutationQueue';
	import { showError } from '$lib/shared/stores/notifications';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import {
		feedbackReceiptMessage,
		followUpRankDeltaLabel,
		parseChannelFollowUpLearningRank,
		type AttentionFeedbackReceipt,
		type ChannelFollowUpDismissReason
	} from './channelFollowUpLearning';
	import Modal from '$lib/magician/components/generative/Modal.svelte';
	import {
		OVERLAY_IDS,
		OVERLAY_PRIORITIES,
		release,
		requestFocus
	} from '$lib/shell/overlayCoordinator';
	import {
		approveChannelFollowUp,
		usefulChannelFollowUp,
		acknowledgeChannelFollowUp,
		snoozeChannelFollowUp,
		dismissChannelFollowUp,
		dismissChannelFollowUpWithReason,
		reviewStaleChannelFollowUp,
		fetchChannelMessage,
		fetchChannelWritingPreferences,
		learnChannelWritingPreference,
		updateChannelWritingPreference,
		composeChannelAction,
		commitChannelAction,
		type ChannelActionDescriptor,
		type ChannelFollowUp,
		type ChannelMessageView,
		type ChannelWritingPreference
	} from '$lib/stores/channelNeedsYouStore';

	export let followUp: ChannelFollowUp;
	/** Denser buttons for tight rows (Attention inbox). */
	export let compact = false;

	// Dismissal reasons — the negative signal, recorded so learning can tell WHY
	// (spam vs already-handled vs delegated…). Static for now; LLM-suggested
	// context-specific reasons are a follow-up.
	const DISMISS_REASONS: { code: ChannelFollowUpDismissReason; label: string }[] = [
		{ code: 'spam', label: 'Spam / junk' },
		{ code: 'already_handled', label: 'Already taken care of' },
		{ code: 'duplicate', label: 'Duplicate request' },
		{ code: 'delegated', label: 'Someone else handles this' },
		{ code: 'not_relevant', label: 'Not relevant to me' },
		{ code: 'wrong_classification', label: "Shouldn't have been flagged" }
	];
	let showDismiss = false;
	// The ▾ menu next to "Show message" holding the secondary Open + Writing style.
	let showMsgMenu = false;
	// Wrappers for the two split-button menus. A window-level click outside an
	// open menu dismisses it (parity with the Worth-a-look band); the caret and
	// menu items live inside the wrapper, so opening/selecting never self-closes.
	let dismissWrapEl: HTMLDivElement | null = null;
	let msgWrapEl: HTMLDivElement | null = null;

	function handleWindowClick(event: MouseEvent): void {
		const target = event.target as Node;
		if (showDismiss && dismissWrapEl && !dismissWrapEl.contains(target)) showDismiss = false;
		if (showMsgMenu && msgWrapEl && !msgWrapEl.contains(target)) showMsgMenu = false;
	}

	type ActionResult = {
		ok: boolean;
		taskId?: string;
		error?: string;
		feedbackReceipt?: AttentionFeedbackReceipt | null;
	};
	// The static lifecycle actions plus the dynamic adapter-declared channel
	// actions, keyed `channel:${descriptor.id}` so they share the busy/event
	// plumbing without hardcoding any specific channel action.
	type ChannelFollowUpAction =
		| 'approve'
		| 'useful'
		| 'acknowledge'
		| 'snooze'
		| 'dismiss'
		| 'review'
		| `channel:${string}`;

	const dispatch = createEventDispatcher<{
		resolved: {
			id: string;
			action: ChannelFollowUpAction;
			taskId?: string;
			message: string;
			feedbackReceipt: AttentionFeedbackReceipt | null;
		};
		failed: { id: string; action: ChannelFollowUpAction; error: string };
	}>();

	let busy: ChannelFollowUpAction | null = null;
	// Reminder creation runs through the shared contextual-action path rather
	// than the channel adapters: it is lane-independent, so the same server
	// contract (and the same idempotency guarantees) back both attention lanes.
	let reminderOpen = false;
	let reminderBusy = false;
	let reminderError: string | null = null;

	async function submitReminder(
		event: CustomEvent<{ kind: string; input: Record<string, unknown> }>
	): Promise<void> {
		reminderBusy = true;
		reminderError = null;
		try {
			const response = await postChannelFollowUpContextualAction(followUp.annotation_id, {
				kind: 'create_reminder',
				// A fresh key per submission; retrying the same submission replays
				// the stored receipt server-side instead of creating a second
				// reminder.
				idempotency_key: crypto.randomUUID(),
				content_revision: followUp.source_revision ?? null,
				input: event.detail.input
			});
			reminderOpen = false;
			const result = response.result;
			dispatch('resolved', {
				// `id` is load-bearing, not decoration: the inbox removes a
				// resolved row BY id (AttentionInboxSurface forwards it to
				// `followupresolved` → `removeResolvedFollowUp`). Dispatching
				// without it — as this path did, alongside an `ok` the event
				// has never carried — left a follow-up sitting in the inbox
				// forever after its reminder was created.
				id: followUp.annotation_id,
				action: 'useful',
				message:
					result && result.kind === 'reminder'
						? 'Apple Reminder created'
						: 'Reminder created',
				// The contextual-action endpoint returns no attention feedback
				// receipt; the other two paths pass the one their call yields.
				feedbackReceipt: null
			});
		} catch (error) {
			reminderError = error instanceof Error ? error.message : String(error);
		} finally {
			reminderBusy = false;
		}
	}

	// Agent hint for "Do it" — captured in a centered modal on click (hints can
	// be long).
	let showHintModal = false;
	let hint = '';

	// On-demand actual message evidence (one message, or a coalesced batch).
	let showMessage = false;
	let messageView: ChannelMessageView | null = null;
	let messageLoading = false;
	let messageTried = false;
	let showWritingModal = false;
	let writingPreferences: ChannelWritingPreference[] = [];
	let writingLoading = false;
	let writingSaving = false;
	let writingError = '';
	let writingScope: 'sender' | 'domain' = 'sender';
	let writingStatement = '';
	let writingPromote = false;

	// Generic channel-execution actions (adapter-declared, e.g. iMessage "Reply").
	// Fully descriptor-driven — no channel-specific branching anywhere.
	$: channelActions = followUp.available_actions ?? [];
	$: learningRank = parseChannelFollowUpLearningRank(followUp);
	$: rankDeltaLabel = followUpRankDeltaLabel(
		learningRank,
		followUp.semantic_ranking_enabled === true
	);
	$: feedbackAttribution = attentionFeedbackAttribution(
		followUp.decision_item ?? null,
		followUp.routing_page?.impression_policy ?? null,
		'follow_up'
	);
	const channelActionKey = (actionId: string): ChannelFollowUpAction =>
		`channel:${actionId}`;

	// Compose modal — the draft/edit/redraft/send flow for a `needs_compose`
	// descriptor. One modal drives whichever descriptor is active.
	let composeAction: ChannelActionDescriptor | null = null;
	let composeText = '';
	let composeId: string | null = null;
	let composeHint = '';
	let composeLoading = false;
	let composeSending = false;
	let composeError = '';

	async function run(
		action: ChannelFollowUpAction,
		fn: (id: string) => Promise<ActionResult>,
		message: string,
		eagerLifecycle = false
	) {
		if (busy) return;
		busy = action;
		const res = eagerLifecycle
			? await optimisticAttentionMutationQueue.enqueue(
				followUpAttentionMutationKey(followUp.annotation_id, $scopeIdentityStore),
				() => fn(followUp.annotation_id)
			  )
			: await fn(followUp.annotation_id);
		busy = null;
		if (res.ok) {
			const actionMessage = res.taskId ? `${message} (task ${res.taskId})` : message;
			dispatch('resolved', {
				id: followUp.annotation_id,
				action,
				taskId: res.taskId,
				message: feedbackReceiptMessage(actionMessage, res.feedbackReceipt ?? null),
				feedbackReceipt: res.feedbackReceipt ?? null
			});
		} else if (eagerLifecycle) {
			// The row may already have unmounted after optimistic suppression.
			// Surface the failure here; releasing the queue tombstone restores it.
			showError('Message action failed', res.error ?? 'failed');
		} else {
			dispatch('failed', {
				id: followUp.annotation_id,
				action,
				error: res.error ?? 'failed'
			});
		}
	}

	function openHintDialog(): void {
		hint = '';
		const granted = requestFocus({
			id: OVERLAY_IDS.attentionChannelChild,
			priority: OVERLAY_PRIORITIES.attentionInput,
			onClose: closeHintDialog
		});
		if (granted) showHintModal = true;
	}

	function closeHintDialog(): void {
		showHintModal = false;
		release(OVERLAY_IDS.attentionChannelChild);
	}

	function confirmDoIt() {
		const h = hint.trim() ? hint : undefined;
		closeHintDialog();
		void run(
			'approve',
			(id) => approveChannelFollowUp(id, h, feedbackAttribution),
			'Follow-up task created'
		);
	}

	// --- Generic channel-execution actions ---------------------------------

	function onChannelAction(descriptor: ChannelActionDescriptor): void {
		if (busy) return;
		if (descriptor.needs_compose) {
			void openComposeDialog(descriptor);
			return;
		}
		// Direct commit (no draft). Guard destructive actions behind a confirm.
		if (descriptor.confirm && typeof window !== 'undefined') {
			const proceed = window.confirm(`${descriptor.label}?`);
			if (!proceed) return;
		}
		void run(
			channelActionKey(descriptor.id),
			(id) => commitChannelAction(id, descriptor.id, {}, feedbackAttribution),
			descriptor.label
		);
	}

	async function openComposeDialog(descriptor: ChannelActionDescriptor): Promise<void> {
		const granted = requestFocus({
			id: OVERLAY_IDS.attentionChannelChild,
			priority: OVERLAY_PRIORITIES.attentionInput,
			onClose: closeComposeDialog
		});
		if (!granted) return;
		composeAction = descriptor;
		composeText = '';
		composeId = null;
		composeHint = '';
		composeError = '';
		composeSending = false;
		await draftCompose();
	}

	function closeComposeDialog(): void {
		composeAction = null;
		release(OVERLAY_IDS.attentionChannelChild);
	}

	/** (Re)draft the outgoing body — `composeHint` re-drafts when set. */
	async function draftCompose(): Promise<void> {
		if (!composeAction || composeLoading) return;
		composeLoading = true;
		composeError = '';
		const hintValue = composeHint.trim() ? composeHint : undefined;
		const res = await composeChannelAction(followUp.annotation_id, composeAction.id, hintValue);
		composeLoading = false;
		if (!res.ok) {
			composeError = res.error ?? 'Could not draft a message.';
			return;
		}
		composeId = res.composeId ?? null;
		composeText = res.text ?? '';
	}

	async function sendCompose(): Promise<void> {
		if (!composeAction || composeSending || composeLoading) return;
		const descriptor = composeAction;
		const actionKey = channelActionKey(descriptor.id);
		composeSending = true;
		busy = actionKey;
		composeError = '';
		const res = await commitChannelAction(followUp.annotation_id, descriptor.id, {
			body: composeText,
			compose_id: composeId ?? undefined
		}, feedbackAttribution);
		composeSending = false;
		busy = null;
		if (!res.ok) {
			composeError = res.error ?? 'Could not send.';
			dispatch('failed', {
				id: followUp.annotation_id,
				action: actionKey,
				error: res.error ?? 'failed'
			});
			return;
		}
		closeComposeDialog();
		dispatch('resolved', {
			id: followUp.annotation_id,
			action: actionKey,
			message: feedbackReceiptMessage(descriptor.label, res.feedbackReceipt ?? null),
			feedbackReceipt: res.feedbackReceipt ?? null
		});
	}

	async function toggleMessage() {
		showMessage = !showMessage;
		if (showMessage && !messageTried) {
			messageTried = true;
			messageLoading = true;
			messageView = await fetchChannelMessage(followUp.annotation_id);
			messageLoading = false;
		}
	}

	async function openWritingDialog(): Promise<void> {
		const granted = requestFocus({
			id: OVERLAY_IDS.attentionChannelChild,
			priority: OVERLAY_PRIORITIES.attentionInput,
			onClose: closeWritingDialog
		});
		if (!granted) return;
		showWritingModal = true;
		writingError = '';
		writingLoading = true;
		writingPreferences = await fetchChannelWritingPreferences(followUp.annotation_id);
		writingLoading = false;
	}

	function closeWritingDialog(): void {
		showWritingModal = false;
		release(OVERLAY_IDS.attentionChannelChild);
	}

	async function saveWritingPreference(): Promise<void> {
		const statement = writingStatement.trim();
		if (!statement || writingSaving) return;
		writingSaving = true;
		writingError = '';
		const result = await learnChannelWritingPreference(
			followUp.annotation_id,
			writingScope,
			statement,
			writingPromote
		);
		writingSaving = false;
		if (!result.ok) {
			writingError = result.error ?? 'Could not save writing preference.';
			return;
		}
		writingStatement = '';
		writingPreferences = await fetchChannelWritingPreferences(followUp.annotation_id);
	}

	async function changeWritingPreference(
		preference: ChannelWritingPreference,
		action: 'promote' | 'dismiss'
	): Promise<void> {
		writingError = '';
		const result = await updateChannelWritingPreference(preference.id, action);
		if (!result.ok) {
			writingError = result.error ?? `Could not ${action} preference.`;
			return;
		}
		writingPreferences = await fetchChannelWritingPreferences(followUp.annotation_id);
	}

	onDestroy(() => release(OVERLAY_IDS.attentionChannelChild));
</script>

<svelte:window on:click={handleWindowClick} />

<div class="mfa-wrap">
	{#if rankDeltaLabel}
		<div
			class="mfa-learning-rank"
			class:mfa-learning-rank--active={followUp.semantic_ranking_enabled === true}
			title={learningRank?.learning_score == null
				? 'Compared with the unchanged baseline order'
				: `Learning score ${learningRank.learning_score.toFixed(3)} · compared with the unchanged baseline order`}
			data-testid="follow-up-rank-delta"
		>
			<span aria-hidden="true">{(learningRank?.rank_delta ?? 0) > 0 ? '↑' : (learningRank?.rank_delta ?? 0) < 0 ? '↓' : '→'}</span>
			{rankDeltaLabel}
		</div>
	{/if}
	<AttentionActionabilityBadge metadata={followUp.actionability ?? null} />
	<div class="mfa" class:compact>
		{#if followUp.review_required}
			<button
				type="button"
				class="mfa-btn mfa-btn--primary"
				disabled={!!busy}
				title="Review the newer message and return this stale draft to the approval lane"
				on:click={() => run('review', reviewStaleChannelFollowUp, 'Draft review re-opened', true)}
			>
				{busy === 'review' ? '…' : 'Review & re-open'}
			</button>
		{:else}
		<button
			type="button"
			class="mfa-btn mfa-btn--primary"
			disabled={!!busy}
			title="Create a follow-up task for the assistant to act on this thread"
			on:click={openHintDialog}
		>
			{busy === 'approve' ? '…' : 'Do it'}
		</button>
		<button
			type="button"
			class="mfa-btn"
			disabled={!!busy}
			title="Positive — worth surfacing (more of this kind). Does not move this out of For you."
			on:click={() =>
				run('useful', (id) => usefulChannelFollowUp(id, feedbackAttribution), 'Marked useful', true)}
		>
			{busy === 'useful' ? '…' : 'Useful'}
		</button>
		<button
			type="button"
			class="mfa-btn"
			disabled={!!busy}
			title="Wrong lane — this should not have been For you. Similar future cards go to Worth a look."
			on:click={() =>
				run(
					'dismiss',
					(id) =>
						dismissChannelFollowUpWithReason(id, 'wrong_classification', feedbackAttribution),
					"Dismissed — shouldn't have been flagged",
					true
				)}
		>
			{busy === 'dismiss' ? '…' : "Shouldn't have been flagged"}
		</button>
		<button
			type="button"
			class="mfa-btn"
			disabled={!!busy}
			title="Neutral — seen, no action needed (no learning signal)"
			on:click={() =>
				run(
					'acknowledge',
					(id) => acknowledgeChannelFollowUp(id, feedbackAttribution),
					'Acknowledged',
					true
				)}
		>
			{busy === 'acknowledge' ? '…' : 'Acknowledge'}
		</button>
		<button
			type="button"
			class="mfa-btn"
			disabled={!!busy}
			on:click={() => run('snooze', snoozeChannelFollowUp, 'Snoozed', true)}
		>
			Snooze
		</button>
		<div class="mfa-dismiss" bind:this={dismissWrapEl}>
			<!-- Split button: the main part dismisses immediately with NO reason (one
			     click); the ▾ caret opens the optional reason list. -->
			<div class="mfa-split">
				<button
					type="button"
					class="mfa-btn mfa-split-main"
					disabled={!!busy}
					title="Dismiss (no reason)"
					on:click={() =>
						void run(
							'dismiss',
							(id) => dismissChannelFollowUp(id, feedbackAttribution),
							'Dismissed',
							true
						)}
				>
					{busy === 'dismiss' ? '…' : 'Dismiss'}
				</button>
				<button
					type="button"
					class="mfa-btn mfa-split-caret"
					disabled={!!busy}
					title="Dismiss with a reason"
					aria-label="Dismiss with a reason"
					aria-haspopup="menu"
					aria-expanded={showDismiss}
					on:click={() => (showDismiss = !showDismiss)}
				>
					▾
				</button>
			</div>
			{#if showDismiss}
				<div class="mfa-menu" role="menu">
					{#each DISMISS_REASONS as r (r.code)}
						<button
							type="button"
							class="mfa-menu-item"
							role="menuitem"
							on:click={() => {
								showDismiss = false;
								void run(
									'dismiss',
									(id) => dismissChannelFollowUpWithReason(id, r.code, feedbackAttribution),
									`Dismissed — ${r.label}`,
									true
								);
							}}
						>
							{r.label}
						</button>
					{/each}
				</div>
			{/if}
		</div>
		{/if}
		{#each channelActions as descriptor (descriptor.id)}
			<button
				type="button"
				class="mfa-btn mfa-btn--channel"
				disabled={!!busy}
				title={descriptor.needs_compose
					? `Draft and send: ${descriptor.label}`
					: descriptor.label}
				on:click={() => onChannelAction(descriptor)}
			>
				{#if descriptor.icon}<span aria-hidden="true">{descriptor.icon}</span>{/if}
				{busy === channelActionKey(descriptor.id) ? '…' : descriptor.label}
			</button>
		{/each}
		<!-- Split button: main toggles the inline message body; the ▾ caret opens a
		     menu with the secondary Open + Writing style. -->
		<div class="mfa-showmsg" bind:this={msgWrapEl}>
			<div class="mfa-split">
				<button
					type="button"
					class="mfa-btn mfa-split-main mfa-btn--icon"
					class:active={showMessage}
					on:click={toggleMessage}
				>
					{showMessage ? 'Hide message' : 'Show message'}
				</button>
				<button
					type="button"
					class="mfa-btn mfa-split-caret"
					disabled={!!busy}
					title="More: open the thread or set a writing style"
					aria-label="Message options"
					aria-haspopup="menu"
					aria-expanded={showMsgMenu}
					on:click={() => (showMsgMenu = !showMsgMenu)}
				>
					▾
				</button>
			</div>
			{#if showMsgMenu}
				<div class="mfa-menu" role="menu">
					{#if followUp.open_url}
						<a
							class="mfa-menu-item"
							role="menuitem"
							href={followUp.open_url}
							target="_blank"
							rel="noopener"
							title={followUp.account_email
								? `Opens the thread in ${followUp.account_email}`
								: 'Open the thread'}
							on:click={() => (showMsgMenu = false)}
						>
							Open
						</a>
					{/if}
					<button
						type="button"
						class="mfa-menu-item"
						role="menuitem"
						disabled={!!busy}
						on:click={() => {
							showMsgMenu = false;
							void openWritingDialog();
						}}
					>
						Writing style
					</button>
					<button
						type="button"
						class="mfa-menu-item"
						role="menuitem"
						disabled={!!busy || reminderBusy}
						on:click={() => {
							showMsgMenu = false;
							reminderError = null;
							reminderOpen = true;
						}}
					>
						Create reminder
					</button>
				</div>
			{/if}
		</div>
	</div>

	{#if showMessage}
		<div class="mfa-message">
			{#if messageLoading}
				<span class="mfa-message-note">Fetching message…</span>
			{:else if messageView}
				{#if messageView.has_newer}
					<div class="mfa-message-warn">
						⚠ A newer message arrived after the one this was summarized from — showing
						the summarized message.
					</div>
				{/if}
				{#if messageView.summary}
					<div class="mfa-message-summary">
						<span class="mfa-message-tag">Summary</span>
						{messageView.summary}
					</div>
				{/if}
				{#if messageView.evidence_messages.length > 1}
					<div class="mfa-message-tag">Evidence batch</div>
					{#each messageView.evidence_messages as evidence, index}
						<div class="mfa-evidence">
							<div class="mfa-evidence-title">
								Message {index + 1}{evidence.subject ? ` · ${evidence.subject}` : ''}
							</div>
							{#if evidence.body}
								<pre class="mfa-message-body">{evidence.body}</pre>
							{:else}
								<span class="mfa-message-note">Body unavailable for this evidence message.</span>
							{/if}
						</div>
					{/each}
				{:else if messageView.body}
					<div class="mfa-message-tag">Message</div>
					<pre class="mfa-message-body">{messageView.body}</pre>
				{:else}
					<span class="mfa-message-note"
						>Body unavailable (may be suppressed, deleted, or the account isn't
						reachable) — the summary above is what was classified.</span
					>
				{/if}
			{:else}
				<span class="mfa-message-note"
					>Message unavailable (may be suppressed, deleted, or the account isn't reachable).</span
				>
			{/if}
		</div>
	{/if}
</div>

<Modal
	open={showHintModal}
	title="Do it - instruction for the agent"
	size="md"
	layer="attention-child"
	idBase="channel-follow-up-action"
	initialFocusSelector=".mfa-modal-text"
	returnFocusSelector="[data-attention-center-dialog], [data-attention-trigger]"
	on:close={closeHintDialog}
>
	<div class="mfa-modal-content">
		<p class="mfa-modal-sub">{followUp.subject || '(no subject)'}</p>
		<textarea
			class="mfa-modal-text"
			rows="5"
			bind:value={hint}
			aria-label="Optional instruction for the agent"
			placeholder="Optional: tell the agent what to do (e.g. draft a polite decline; confirm the meeting for Tuesday; forward to accounts)…"
		></textarea>
		<div class="mfa-modal-actions">
			<button type="button" class="mfa-btn" on:click={closeHintDialog}>Cancel</button>
			<button type="button" class="mfa-btn mfa-btn--primary" disabled={!!busy} on:click={confirmDoIt}>
				Create follow-up task
			</button>
		</div>
	</div>
</Modal>

<Modal
	open={showWritingModal}
	title="Writing preferences"
	size="md"
	layer="attention-child"
	idBase="channel-writing-preferences"
	initialFocusSelector=".mfa-writing-text"
	returnFocusSelector="[data-attention-center-dialog], [data-attention-trigger]"
	on:close={closeWritingDialog}
>
	<div class="mfa-modal-content">
		<p class="mfa-modal-sub">
			Exact statements used for {followUp.sender || followUp.subject || 'this conversation'}.
		</p>
		{#if writingLoading}
			<p class="mfa-message-note">Loading preferences…</p>
		{:else if writingPreferences.length > 0}
			<div class="mfa-writing-list">
				{#each writingPreferences as preference (preference.id)}
					<div class="mfa-writing-item">
						<div>
							<strong>{preference.statement}</strong>
							<span>{preference.scope_kind} · {preference.status} · evidence {preference.evidence_count}</span>
						</div>
						<div class="mfa-writing-actions">
							{#if preference.status === 'candidate'}
								<button class="mfa-btn" type="button" on:click={() => changeWritingPreference(preference, 'promote')}>Promote</button>
							{/if}
							<button class="mfa-btn" type="button" on:click={() => changeWritingPreference(preference, 'dismiss')}>Dismiss</button>
						</div>
					</div>
				{/each}
			</div>
		{:else}
			<p class="mfa-message-note">No learned writing preferences yet.</p>
		{/if}
		<label class="mfa-writing-label">
			<span>Learn an exact statement</span>
			<textarea
				class="mfa-modal-text mfa-writing-text"
				rows="3"
				bind:value={writingStatement}
				placeholder="For example: Keep replies concise and use a friendly greeting."
			></textarea>
		</label>
		<div class="mfa-writing-options">
			<label>
				<span>Apply to</span>
				<select bind:value={writingScope}>
					<option value="sender">This sender</option>
					<option value="domain">This sender's domain</option>
				</select>
			</label>
			<label class="mfa-writing-check">
				<input type="checkbox" bind:checked={writingPromote} /> Promote immediately
			</label>
		</div>
		{#if writingError}<p class="mfa-message-warn">{writingError}</p>{/if}
		<div class="mfa-modal-actions">
			<button type="button" class="mfa-btn" on:click={closeWritingDialog}>Close</button>
			<button
				type="button"
				class="mfa-btn mfa-btn--primary"
				disabled={writingSaving || !writingStatement.trim()}
				on:click={saveWritingPreference}
			>
				{writingSaving ? 'Saving…' : 'Learn preference'}
			</button>
		</div>
	</div>
</Modal>

<Modal
	open={!!composeAction}
	title={composeAction ? composeAction.label : ''}
	size="md"
	layer="attention-child"
	idBase="channel-action-compose"
	initialFocusSelector=".mfa-compose-text"
	returnFocusSelector="[data-attention-center-dialog], [data-attention-trigger]"
	on:close={closeComposeDialog}
>
	<div class="mfa-modal-content">
		<p class="mfa-modal-sub">{followUp.subject || followUp.sender || '(no subject)'}</p>
		<textarea
			class="mfa-modal-text mfa-compose-text"
			rows="6"
			bind:value={composeText}
			disabled={composeLoading}
			aria-label="Draft message — edit before sending"
			placeholder={composeLoading ? 'Drafting…' : 'The drafted message appears here — edit before sending.'}
		></textarea>
		{#if composeLoading}
			<p class="mfa-message-note">Drafting a message…</p>
		{/if}
		{#if composeError}<p class="mfa-message-warn">{composeError}</p>{/if}
		<div class="mfa-compose-redraft">
			<input
				type="text"
				class="mfa-compose-hint"
				bind:value={composeHint}
				disabled={composeLoading}
				aria-label="Redraft hint"
				placeholder="Optional: nudge the redraft (e.g. shorter, warmer)…"
			/>
			<button
				type="button"
				class="mfa-btn"
				disabled={composeLoading || composeSending}
				on:click={draftCompose}
			>
				{composeLoading ? '…' : 'Redraft'}
			</button>
		</div>
		<div class="mfa-modal-actions">
			<button type="button" class="mfa-btn" on:click={closeComposeDialog}>Cancel</button>
			<button
				type="button"
				class="mfa-btn mfa-btn--primary"
				disabled={composeLoading || composeSending || !composeText.trim()}
				on:click={sendCompose}
			>
				{composeSending
					? 'Sending…'
					: composeAction && composeAction.confirm
						? `Confirm & ${composeAction.label}`
						: 'Send'}
			</button>
		</div>
	</div>
</Modal>

<ResurfacingActionDialog
	open={reminderOpen}
	kind="create_reminder"
	busy={reminderBusy}
	serverError={reminderError}
	targetKey={followUp.annotation_id}
	sourceLabel={followUp.subject || followUp.summary || 'Follow-up'}
	sourceNote={followUp.summary || followUp.reason || followUp.subject || ''}
	on:cancel={() => (reminderOpen = false)}
	on:submit={(event) => void submitReminder(event)}
/>

<style>
	.mfa-wrap {
		display: flex;
		flex-direction: column;
		gap: 0.4rem;
		min-width: 0;
	}
	.mfa {
		display: flex;
		flex-wrap: wrap;
		gap: 0.4rem;
		align-items: center;
	}
	.mfa-btn {
		display: inline-flex;
		align-items: center;
		gap: 0.3rem;
		font: inherit;
		font-size: 0.78rem;
		line-height: 1;
		padding: 0.34rem 0.7rem;
		border-radius: 0.45rem;
		border: 1px solid var(--border-default, var(--border-soft));
		background: var(--bg-card);
		color: var(--text-secondary);
		cursor: pointer;
		text-decoration: none;
		white-space: nowrap;
		transition:
			background 0.15s ease,
			color 0.15s ease,
			border-color 0.15s ease;
	}
	.mfa.compact .mfa-btn {
		font-size: 0.78rem;
		padding: 0.26rem 0.55rem;
	}
	.mfa-btn:hover {
		color: var(--text-primary);
		border-color: var(--border-default);
		background: var(--bg-soft);
	}
	.mfa-btn:disabled {
		opacity: 0.55;
		cursor: default;
	}
	.mfa-btn--primary {
		background: var(--accent-primary);
		border-color: transparent;
		color: var(--accent-on-primary, #fff);
	}
	.mfa-btn--primary:hover {
		background: color-mix(in srgb, var(--accent-primary) 88%, #000);
		color: var(--accent-on-primary, #fff);
	}
	.mfa-btn--icon {
		color: var(--text-muted);
	}
	.mfa-btn--channel {
		border-color: color-mix(in srgb, var(--accent-primary) 45%, transparent);
		color: var(--accent-primary);
	}
	.mfa-btn--channel:hover {
		background: color-mix(in srgb, var(--accent-primary) 10%, var(--bg-card));
		border-color: var(--accent-primary);
		color: var(--accent-primary);
	}
	.mfa-compose-redraft {
		display: flex;
		gap: 0.4rem;
		align-items: center;
	}
	.mfa-compose-hint {
		flex: 1 1 auto;
		min-width: 0;
		box-sizing: border-box;
		font: inherit;
		font-size: 0.82rem;
		padding: 0.4rem 0.55rem;
		border-radius: 0.45rem;
		border: 1px solid var(--border-default, var(--border-soft));
		background: var(--bg-soft);
		color: var(--text-primary);
	}
	.mfa-split {
		display: inline-flex;
	}
	/* Join the two halves so they read as one button. */
	.mfa-split-main {
		border-top-right-radius: 0;
		border-bottom-right-radius: 0;
	}
	.mfa-split-caret {
		border-top-left-radius: 0;
		border-bottom-left-radius: 0;
		border-left: 0;
		padding-left: 0.45rem;
		padding-right: 0.45rem;
	}
	.mfa-dismiss,
	.mfa-showmsg {
		position: relative;
		display: inline-flex;
	}
	.mfa-menu {
		position: absolute;
		top: calc(100% + 0.3rem);
		left: 0;
		z-index: 20;
		min-width: 12rem;
		display: flex;
		flex-direction: column;
		padding: 0.25rem;
		border-radius: 0.5rem;
		border: 1px solid var(--border-default, var(--border-soft));
		background: var(--bg-card);
		box-shadow: 0 8px 24px rgba(0, 0, 0, 0.25);
	}
	.mfa.compact .mfa-menu {
		right: 0;
		left: auto;
	}
	.mfa-menu-item {
		text-align: left;
		font: inherit;
		font-size: 0.78rem;
		padding: 0.4rem 0.55rem;
		border: 0;
		border-radius: 0.35rem;
		background: transparent;
		color: var(--text-primary);
		cursor: pointer;
		white-space: nowrap;
	}
	.mfa-menu-item:hover {
		background: var(--bg-soft);
	}
	.mfa-btn--icon.active {
		color: var(--accent-primary);
		border-color: color-mix(in srgb, var(--accent-primary) 45%, transparent);
	}
	.mfa-modal-content {
		display: flex;
		flex-direction: column;
		gap: 0.6rem;
	}
	.mfa-modal-sub {
		margin: 0;
		font-size: 0.82rem;
		color: var(--text-muted);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.mfa-modal-text {
		width: 100%;
		box-sizing: border-box;
		font: inherit;
		font-size: 0.85rem;
		line-height: 1.45;
		padding: 0.55rem 0.65rem;
		border-radius: 0.5rem;
		border: 1px solid var(--border-default, var(--border-soft));
		background: var(--bg-soft);
		color: var(--text-primary);
		resize: vertical;
	}
	.mfa-modal-actions {
		display: flex;
		justify-content: flex-end;
		gap: 0.5rem;
	}
	.mfa-writing-list {
		display: flex;
		flex-direction: column;
		gap: 0.45rem;
		max-height: 13rem;
		overflow: auto;
	}
	.mfa-writing-item {
		display: flex;
		justify-content: space-between;
		gap: 0.6rem;
		padding: 0.55rem;
		border: 1px solid var(--border-soft);
		border-radius: 0.45rem;
		background: var(--bg-soft);
		font-size: 0.8rem;
	}
	.mfa-writing-item strong,
	.mfa-writing-item span {
		display: block;
	}
	.mfa-writing-item span {
		margin-top: 0.2rem;
		color: var(--text-muted);
		font-size: 0.72rem;
	}
	.mfa-writing-actions {
		display: flex;
		gap: 0.3rem;
		align-items: flex-start;
	}
	.mfa-writing-label,
	.mfa-writing-options label {
		display: flex;
		flex-direction: column;
		gap: 0.3rem;
		font-size: 0.78rem;
		color: var(--text-secondary);
	}
	.mfa-writing-options {
		display: flex;
		align-items: end;
		justify-content: space-between;
		gap: 0.75rem;
	}
	.mfa-writing-options select {
		font: inherit;
		padding: 0.35rem 0.45rem;
		border: 1px solid var(--border-default, var(--border-soft));
		border-radius: 0.4rem;
		background: var(--bg-card);
		color: var(--text-primary);
	}
	.mfa-writing-options .mfa-writing-check {
		flex-direction: row;
		align-items: center;
		padding-bottom: 0.35rem;
	}
	.mfa-message {
		border: 1px solid var(--border-soft);
		border-radius: 0.5rem;
		background: var(--bg-soft);
		padding: 0.5rem 0.6rem;
		max-height: 18rem;
		overflow: auto;
	}
	.mfa-message-note {
		font-size: 0.78rem;
		color: var(--text-muted);
	}
	.mfa-message-warn {
		font-size: 0.76rem;
		color: var(--color-warning, #d28b1a);
		margin-bottom: 0.4rem;
	}
	.mfa-message-tag {
		display: inline-block;
		font-size: 0.62rem;
		font-weight: 700;
		letter-spacing: 0.05em;
		text-transform: uppercase;
		color: var(--text-muted);
		margin-bottom: 0.15rem;
	}
	.mfa-message-summary {
		font-size: 0.82rem;
		color: var(--text-primary);
		line-height: 1.4;
		padding: 0.4rem 0.5rem;
		margin-bottom: 0.5rem;
		border-radius: 0.4rem;
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
	}
	.mfa-message-summary .mfa-message-tag {
		margin-right: 0.35rem;
		margin-bottom: 0;
	}
	.mfa-evidence {
		padding: 0.45rem 0;
		border-top: 1px solid var(--border-soft);
	}
	.mfa-evidence:first-of-type {
		border-top: 0;
	}
	.mfa-evidence-title {
		margin-bottom: 0.25rem;
		font-size: 0.76rem;
		color: var(--text-muted);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.mfa-message-body {
		margin: 0;
		font-family: var(--font-mono, monospace);
		font-size: 0.78rem;
		line-height: 1.4;
		white-space: pre-wrap;
		word-break: break-word;
		color: var(--text-primary);
	}
	.mfa-learning-rank {
		display: inline-flex;
		align-items: center;
		gap: 0.3rem;
		align-self: flex-start;
		padding: 0.2rem 0.5rem;
		border: 1px solid var(--border-soft);
		border-radius: 999px;
		background: var(--bg-soft);
		color: var(--text-muted, var(--text-secondary));
		font-size: 0.72rem;
		font-variant-numeric: tabular-nums;
	}
	.mfa-learning-rank--active {
		border-color: color-mix(in srgb, var(--color-success, #2f8f5b) 35%, var(--border-soft));
		background: color-mix(in srgb, var(--color-success, #2f8f5b) 9%, var(--bg-card));
		color: var(--color-success, #2f8f5b);
	}

	@media (max-width: 420px) {
		.mfa.compact {
			align-items: stretch;
		}
		.mfa.compact .mfa-btn,
		.mfa.compact .mfa-dismiss {
			flex: 1 1 9rem;
			justify-content: center;
		}
	}
</style>