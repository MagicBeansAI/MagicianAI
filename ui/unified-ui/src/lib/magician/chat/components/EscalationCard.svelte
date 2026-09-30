<script lang="ts">
	import Button from '$lib/magician/components/generative/Button.svelte';
	import ChatContentBlocks from '$lib/magician/components/chat/ChatContentBlocks.svelte';
	import ChatMarkdown from '$lib/magician/components/chat/ChatMarkdown.svelte';
	import {
		getEscalationResolvedSummary,
		type ChatMessage,
		type EscalationOption
	} from '$lib/stores/chatStore';

	// Renders both escalation faces: the action-required card
	// (content.type === 'escalation') and the resolution notice
	// (content.type === 'escalation_resolved'). The label/dim/actions
	// decisions depend on ChatPanel state (thread tasks, read-only mode,
	// in-flight responses), so the parent computes them and passes the
	// results down; the canonical HITL dispatch stays in ChatPanel and
	// arrives as the onRespond callback.
	export let message: ChatMessage;
	export let statusLabel = '';
	export let dimmed = false;
	export let showActions = false;
	export let inactiveReason: string | null = null;
	export let disabled = false;
	export let onRespond: (
		executionId: string,
		pauseStateId: string,
		option: EscalationOption,
		escalationType?: string,
		requestId?: string,
		inputType?: string
	) => void = () => {};
</script>

{#if message.content.type === 'escalation'}
	{@const content = message.content}
	<div class="chat-escalation-card" class:chat-escalation-resolved={dimmed}>
		<div class="chat-escalation-header">
			<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M10.29 3.86L1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0z"/><line x1="12" y1="9" x2="12" y2="13"/><line x1="12" y1="17" x2="12.01" y2="17"/></svg>
			<span class="chat-escalation-title">
				{statusLabel}
			</span>
		</div>
		<div class="chat-escalation-question">
			<ChatMarkdown content={content.question} sessionId={message.session_id} />
		</div>
		{#if showActions}
			<div class="chat-escalation-actions">
				{#each content.options as option}
					<Button
						label={option.label}
						size="sm"
						variant={option.id === 'deny' || option.id === 'stop' || option.id === 'done' ? 'outline' : 'primary'}
						{disabled}
						on:click={() =>
							onRespond(
								content.execution_id ?? '',
								content.pause_state_id ?? '',
								option,
								content.escalation_type,
								content.request_id,
								content.input_type
							)}
					/>
				{/each}
			</div>
		{/if}
		{#if inactiveReason}
			<span class="chat-escalation-resolved-label">{inactiveReason}</span>
		{/if}
	</div>
{:else if message.content.type === 'escalation_resolved'}
	<!-- Escalation resolution notice — summary + deliverable
	     output files share the same card so the answer reads
	     as one unit instead of summary-left / file-right.
	-->
	<div class="chat-escalation-resolved-card">
		<div class="chat-escalation-resolved-row">
			<svg xmlns="http://www.w3.org/2000/svg" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="20 6 9 17 4 12"/></svg>
			<span class="chat-escalation-resolved-text">
				{getEscalationResolvedSummary(message.content)}
			</span>
		</div>
		{#if (message.content.output_files?.length ?? 0) > 0}
			<div class="chat-escalation-resolved-files">
				<ChatContentBlocks
					sessionId={message.session_id}
					blocks={message.content.output_files ?? []}
				/>
			</div>
		{/if}
	</div>
{/if}

<style>
	/* ===== Escalation Cards ===== */
	.chat-escalation-card {
		max-width: min(100%, 500px);
		min-width: 0;
		border-radius: var(--radius-sm);
		padding: 0.75rem 0.85rem;
		background: var(--color-warning-soft);
		border: 1px solid var(--color-warning);
		color: var(--text-primary, #2d3436);
	}

	/* Resolved / stale / no-longer-active state.
	   Was `opacity: 0.7` — too subtle to read as "this is done" at a
	   glance, especially against the warning-tinted active state. Drop
	   harder + remove the warning border/icon color so it visually
	   collapses out of the active-attention layer. Animated transition
	   gives a clear "your answer landed" cue.
	   The dim is never the ONLY state signal: dimmed cards always render
	   the "Resolved" / "No Longer Active" header title plus the
	   inactive-reason footer text, so the state survives without color/
	   opacity perception. No hover-un-dim — state presentation should not
	   change under the pointer. */
	.chat-escalation-card {
		transition:
			opacity 220ms ease,
			background 220ms ease,
			border-color 220ms ease,
			filter 220ms ease;
	}
	.chat-escalation-card.chat-escalation-resolved {
		opacity: 0.5;
		background: var(--bg-soft, #f6f1e8);
		border-color: var(--border-soft, #eee4dc);
		filter: grayscale(0.6);
	}

	.chat-escalation-header {
		display: flex;
		align-items: center;
		gap: 0.4rem;
		margin-bottom: 0.35rem;
		color: var(--color-warning);
	}

	.chat-escalation-resolved .chat-escalation-header {
		color: var(--text-muted, #8f9799);
	}

	.chat-escalation-title {
		font-size: var(--text-xs);
		font-weight: 700;
	}

	.chat-escalation-question {
		font-size: 0.8rem;
		line-height: 1.45;
		margin: 0 0 0.5rem;
		white-space: pre-wrap;
		overflow-wrap: anywhere;
		word-break: break-word;
	}

	.chat-escalation-actions {
		display: flex;
		gap: 0.4rem;
		flex-wrap: wrap;
	}

	.chat-escalation-resolved-label {
		font-size: var(--text-2xs);
		color: var(--text-muted, #8f9799);
		font-style: italic;
	}

	.chat-escalation-resolved-card {
		display: flex;
		flex-direction: column;
		gap: 0.55rem;
		/* Wider than the bare-summary card was; we now embed
		   structured output deliverables (markdown previews, JSON
		   inline viewers, image thumbnails) inside the same box, so
		   500px is too narrow. Keep a generous cap so the card hugs
		   content instead of stretching across the whole chat
		   column. */
		max-width: min(100%, 720px);
		min-width: 0;
		font-size: var(--text-xs);
		border-radius: var(--radius-sm);
		padding: 0.65rem 0.85rem;
		background: var(--color-success-soft);
		border: 1px solid var(--color-success);
		color: var(--text-secondary, #5f6668);
	}

	.chat-escalation-resolved-row {
		display: flex;
		align-items: flex-start;
		gap: 0.4rem;
	}

	.chat-escalation-resolved-files {
		/* Soft divider so the deliverables visually attach to the
		   summary but read as a distinct sub-section of the card. */
		border-top: 1px solid color-mix(in srgb, var(--color-success) 35%, transparent);
		padding-top: 0.55rem;
		margin-top: 0.1rem;
	}

	.chat-escalation-resolved-text {
		font-size: var(--text-2xs);
		white-space: pre-wrap;
		overflow-wrap: anywhere;
		word-break: break-word;
	}
</style>
