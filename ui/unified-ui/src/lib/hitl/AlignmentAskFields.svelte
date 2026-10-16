<script lang="ts">
	/**
	 * A preference alignment hold, answered on the web (P6.1).
	 *
	 * The ask arrives with only its public projection (choice ids and safe
	 * labels). This is an owner-private surface, so it reads why the act was
	 * held — the explanation, the preferences it rests on, the exact wording
	 * of a proposed lasting change — from the owner-private route, and keeps
	 * three things visibly apart:
	 *
	 * 1. what to do with **this act** (allow once, do something else, cancel);
	 * 2. what Magician **remembers** (change or remember a preference), which
	 *    never decides the act and is never preselected;
	 * 3. a **reply** in the owner's own words, which is an instruction, not
	 *    consent.
	 *
	 * A lasting change is two steps: the first answer only asks the server to
	 * read the exact wording back; the second confirms that readback with its
	 * token, which only this private read shows.
	 */
	import { createEventDispatcher, onDestroy, onMount } from 'svelte';

	import Button from '$lib/magician/components/generative/Button.svelte';
	import TextArea from '$lib/magician/components/generative/TextArea.svelte';
	import type { AttentionPromptResult } from '$lib/stores/attentionPromptStore';

	import {
		ALIGNMENT_REPLY_CHOICE,
		deadlineLine,
		type AlignmentDetail,
		type AlignmentProjection
	} from './alignmentAsk';
	import { fetchAlignmentDetail } from './alignmentDetail';

	export let correlationId: string;
	export let projection: AlignmentProjection;
	export let busy = false;

	const dispatch = createEventDispatcher<{ answer: AttentionPromptResult }>();

	let state: 'loading' | 'ready' | 'gone' | 'unavailable' = 'loading';
	let detail: AlignmentDetail | null = null;
	let replyText = '';
	let wordingText = '';
	let changing = false;
	let nowMs = Date.now();
	let ticker: ReturnType<typeof setInterval> | null = null;

	async function load(): Promise<void> {
		const outcome = await fetchAlignmentDetail(correlationId);
		if (outcome.status === 'ok') {
			detail = outcome.detail;
			state = 'ready';
		} else {
			state = outcome.status;
		}
	}

	onMount(() => {
		void load();
		ticker = setInterval(() => (nowMs = Date.now()), 30_000);
	});
	onDestroy(() => {
		if (ticker) clearInterval(ticker);
	});

	function answer(choiceId: string, input?: string): void {
		if (busy) return;
		dispatch('answer', { kind: 'choice', choiceId, input });
	}

	function sendReply(): void {
		const words = replyText.trim();
		if (words.length === 0) return;
		answer(ALIGNMENT_REPLY_CHOICE, words);
	}

	function proposeWording(): void {
		const words = wordingText.trim();
		if (words.length === 0) return;
		answer('change_preference', words);
	}

	function gateWords(gate: string): string {
		switch (gate) {
			case 'approval_rule':
			case 'plane_approval':
				return 'One of your approval rules matched it.';
			case 'criticality':
				return 'It is a high-impact action.';
			default:
				return 'It was marked as needing confirmation.';
		}
	}

	function statusWords(status: string): string {
		switch (status) {
			case 'direct_owner_statement':
			case 'confirmed':
				return 'You said';
			case 'inferred':
				return 'Magician noticed';
			default:
				return 'Noted';
		}
	}

	// Before the private read lands (or if it cannot), the public choices:
	// the act's own, never the lasting ones, which need the readback.
	$: publicActChoices = projection.choices.filter(
		(choice) =>
			!['change_preference', 'remember_preference', 'confirm_wording'].includes(choice.id)
	);
	// Only a private read marks which choices are lasting; anything else keeps
	// the public act choices, so a lasting change never shows as an answer.
	$: actChoices = detail?.private
		? detail.choices.filter((choice) => !choice.lasting && choice.id !== 'keep_as_is')
		: publicActChoices;
	$: lastingChoices = detail?.private ? detail.choices.filter((choice) => choice.lasting) : [];
	$: stage = detail?.stage ?? projection.stage;
	$: deadline = deadlineLine(detail?.deadlineAtMs ?? projection.deadlineAtMs, nowMs);
</script>

<div class="alignment" data-stage={stage} data-testid="alignment-ask">
	{#if state === 'gone'}
		<p class="alignment__note" role="status">
			This review was already answered or replaced. Open the current one to continue.
		</p>
	{:else}
		{#if state === 'loading'}
			<p class="alignment__note" role="status">Loading why this was held…</p>
		{:else if state === 'unavailable' || (detail && !detail.private)}
			<p class="alignment__note" role="status">
				The reason could not be loaded here. Your answer still works.
			</p>
		{/if}

		{#if detail?.retryReason}
			<p class="alignment__retry" role="status">{detail.retryReason}</p>
		{/if}

		{#if detail?.private && stage === 'decide'}
			{#if detail.explanation}
				<p class="alignment__why" data-testid="alignment-explanation">{detail.explanation}</p>
			{/if}
			{#if detail.actSummary}
				<p class="alignment__act"><span>Held:</span> {detail.actSummary}</p>
			{/if}
			{#if detail.alsoCovers.length > 0}
				<div class="alignment__covers" data-testid="alignment-also-covers">
					<p>It also needs your approval:</p>
					<ul>
						{#each detail.alsoCovers as covered (covered.gate)}
							<li>{covered.reason || gateWords(covered.gate)}</li>
						{/each}
					</ul>
					<p>Allowing it approves it too. Any other answer leaves it unapproved.</p>
				</div>
			{/if}
			{#if detail.intentions.length > 0}
				<ul class="alignment__intentions" aria-label="What this is based on">
					{#each detail.intentions as intention (intention.id)}
						<li>
							<span class="alignment__source">{statusWords(intention.assertionStatus)}</span>
							{intention.statement}
						</li>
					{/each}
				</ul>
			{/if}
		{/if}

		{#if stage === 'confirm_wording'}
			{#if detail?.private && detail.readback}
				{@const readback = detail.readback}
				<section class="alignment__lasting" aria-label="Confirm the exact wording">
					<p class="alignment__heading">Save exactly this as your preference?</p>
					<blockquote class="alignment__wording" data-testid="alignment-readback">
						{readback.wording}
					</blockquote>
					{#if readback.superseded}
						<p class="alignment__superseded">Replaces: {readback.superseded}</p>
					{/if}
					<p class="alignment__note">
						This changes what Magician remembers. It does not run the held action; that
						is checked again afterwards.
					</p>
					<div class="hitl-decision">
						<Button
							label="Don't change it"
							variant="outline"
							size="sm"
							disabled={busy}
							on:click={() => answer('keep_as_is')}
						/>
						<Button
							label="Cancel the action"
							variant="outline"
							size="sm"
							disabled={busy}
							on:click={() => answer('cancel')}
						/>
						<Button
							label="Save this wording"
							variant="outline"
							size="sm"
							disabled={busy}
							on:click={() => answer('confirm_wording', readback.token)}
						/>
					</div>
				</section>
			{:else if state !== 'loading'}
				<div class="hitl-decision">
					<Button
						label="Don't change it"
						variant="outline"
						size="sm"
						disabled={busy}
						on:click={() => answer('keep_as_is')}
					/>
					<Button
						label="Cancel the action"
						variant="outline"
						size="sm"
						disabled={busy}
						on:click={() => answer('cancel')}
					/>
				</div>
			{/if}
		{:else}
			<section class="alignment__act-choices" aria-label="This action">
				<div class="hitl-decision">
					{#each actChoices as choice (choice.id)}
						<!-- "Cancel the action", so it never reads as the dialog's own Cancel,
						     which closes the prompt and leaves the hold waiting. -->
						<Button
							label={choice.id === 'cancel' ? 'Cancel the action' : choice.label}
							variant="outline"
							size="sm"
							disabled={busy}
							on:click={() => answer(choice.id)}
						/>
					{/each}
				</div>
			</section>

			{#if lastingChoices.length > 0}
				<section class="alignment__lasting" aria-label="What Magician remembers">
					<p class="alignment__heading">Change what Magician remembers</p>
					{#each lastingChoices as choice (choice.id)}
						{#if choice.needsWording}
							{#if changing}
								<TextArea
									rows={2}
									bind:value={wordingText}
									label="New wording for the preference"
									placeholder="Say it the way it should be remembered…"
									ariaLabel="New wording for the preference"
								/>
								<div class="hitl-decision">
									<Button
										label="Back"
										variant="outline"
										size="sm"
										disabled={busy}
										on:click={() => (changing = false)}
									/>
									<Button
										label="Read it back"
										variant="outline"
										size="sm"
										disabled={busy || wordingText.trim().length === 0}
										on:click={proposeWording}
									/>
								</div>
							{:else}
								<Button
									label={choice.label}
									variant="outline"
									size="sm"
									disabled={busy}
									on:click={() => (changing = true)}
								/>
							{/if}
						{:else}
							{#if choice.id === 'remember_preference' && detail?.remember}
								<p class="alignment__superseded">“{detail.remember.statement}”</p>
							{/if}
							<Button
								label={choice.label}
								variant="outline"
								size="sm"
								disabled={busy}
								on:click={() => answer(choice.id)}
							/>
						{/if}
					{/each}
				</section>
			{/if}
		{/if}

		{#if projection.freeText}
			<section class="alignment__reply" aria-label="Reply">
				<TextArea
					rows={2}
					bind:value={replyText}
					label="Reply with what to do instead"
					placeholder="For example: order the paneer version instead"
					ariaLabel="Reply with what to do instead"
				/>
				<div class="hitl-decision">
					<Button
						label="Send reply"
						variant="outline"
						size="sm"
						disabled={busy || replyText.trim().length === 0}
						on:click={sendReply}
					/>
				</div>
			</section>
		{/if}

		{#if deadline}
			<p class="alignment__deadline">{deadline}</p>
		{/if}
	{/if}
</div>

<style>
	.alignment {
		display: flex;
		flex-direction: column;
		gap: 0.6rem;
	}

	.alignment__why {
		margin: 0;
		font-size: 0.85rem;
		line-height: 1.45;
		color: var(--text-body);
	}

	.alignment__covers {
		margin: 0;
		font-size: 0.85rem;
	}

	.alignment__covers p {
		margin: 0;
	}

	.alignment__covers ul {
		margin: 0.25rem 0;
		padding-left: 1.25rem;
	}

	.alignment__act,
	.alignment__note,
	.alignment__retry,
	.alignment__deadline,
	.alignment__superseded {
		margin: 0;
		font-size: 0.75rem;
		line-height: 1.4;
		color: var(--text-secondary);
	}

	.alignment__act span {
		font-weight: 600;
	}

	.alignment__retry {
		color: var(--text-body);
	}

	.alignment__intentions {
		margin: 0;
		padding-left: 1rem;
		font-size: 0.8rem;
		line-height: 1.4;
		color: var(--text-body);
	}

	.alignment__source {
		font-size: 0.6875rem;
		color: var(--text-secondary);
		margin-right: 0.35rem;
	}

	/* Lasting changes sit apart from the act's choices: a different border
	   and a heading, so changing a preference never reads as approving. */
	.alignment__lasting {
		display: flex;
		flex-direction: column;
		gap: 0.45rem;
		padding: 0.6rem;
		border: 1px dashed var(--border-soft);
		border-radius: var(--radius-sm);
	}

	.alignment__heading {
		margin: 0;
		font-size: 0.75rem;
		font-weight: 600;
		color: var(--text-body);
	}

	.alignment__wording {
		margin: 0;
		padding: 0.4rem 0.6rem;
		border-left: 2px solid var(--border-soft);
		font-size: 0.85rem;
		color: var(--text-body);
		overflow-wrap: anywhere;
	}

	.alignment__reply {
		display: flex;
		flex-direction: column;
		gap: 0.35rem;
	}

	.hitl-decision {
		display: flex;
		flex-wrap: wrap;
		gap: 0.4rem;
	}
</style>
