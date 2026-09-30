<script lang="ts">
	/**
	 * Collapsible "What happened" section attached to an assistant
	 * message bubble. Reads the per-message activity rows captured by
	 * `chatTurnActivityStore` when the live `<ChatTurnProgress />` bubble
	 * unmounted at end-of-turn.
	 *
	 * Renders nothing when no rows exist for this message id — most
	 * assistant messages won't have an activity trail (the rows store
	 * is in-memory only, so page-reloaded history shows clean bubbles).
	 *
	 * When rows exist, renders a summary line (`▸ What happened
	 * (N steps · Xs)`) that toggles a vertical list of rows on click —
	 * same row chrome as `<ChatTurnProgress />` so the visual maps
	 * cleanly between live + post-hoc views.
	 */
	import { slide } from 'svelte/transition';
	import {
		activityByMessageIdStore,
		type ChatTurnActivityRow
	} from '$lib/stores/chatTurnActivityStore';
	import type { ChatMessage } from '$lib/stores/chatStore';
	import { getMessageContentBlocks } from '$lib/stores/chatStore';
	import ChatContentBlocks from './chat/ChatContentBlocks.svelte';
	import ChatMarkdown from './chat/ChatMarkdown.svelte';

	export let messageId: string;
	/**
	 * Persistent activity cards (`pack_progress`, `task_status_update`)
	 * that arrived in-turn for this assistant message. Folded by
	 * `attachInTurnActivityToAssistantMessages` in `chatStore`.
	 * Rendered inline inside the collapsed dropdown so a multi-tool
	 * turn shows one "What happened" section instead of N interleaved
	 * timeline cards.
	 */
	export let attachedActivity: ChatMessage[] = [];
	/** Default expanded state. False (the default) starts the section
	 *  collapsed — less noise during history scroll; click `▸ What
	 *  happened` to expand. */
	export let expanded = false;

	let isOpen = expanded;

	// Reasoning rows are filtered from the post-turn dropdown — they
	// add noise without information (the assistant's final reply text
	// already conveys the substance; the live `<ChatTurnProgress />`
	// bubble already showed "thinking..." during execution). The
	// Internals drawer's EventStreamCard still has the full reasoning
	// content for debugging.
	$: allRows = $activityByMessageIdStore.get(messageId) ?? null;
	$: rows = allRows ? allRows.filter((row) => row.kind !== 'reasoning') : null;
	$: totalDurationMs = computeTotalDurationMs(rows);
	$: stepCount = (rows?.length ?? 0) + attachedActivity.length;
	$: hasContent = stepCount > 0;

	// Top-level "Files generated" strip: collect every output file
	// from the attached pack/task activity into one flat list rendered
	// ABOVE the collapsed dropdown. Files are the most concrete output
	// the user cares about — surfacing them at the bubble level means
	// no click required to access them. The dropdown stays for "what
	// tools were called". Per-row file rendering inside the dropdown
	// is dropped to avoid duplication.
	$: assistantOutputFiles = collectAssistantOutputFiles(attachedActivity);
	// Best-guess session id from any attached message (they all belong
	// to the same chat session for an assistant turn).
	$: assistantOutputSessionId = attachedActivity[0]?.session_id ?? '';

	function collectAssistantOutputFiles(items: ChatMessage[]) {
		const all = [] as ReturnType<typeof getMessageContentBlocks>;
		const seen = new Set<string>();
		for (const item of items) {
			for (const block of getMessageContentBlocks(item.content)) {
				// Dedupe by absolute path / relative path so the same
				// file produced by overlapping pack heartbeats doesn't
				// show twice.
				const key =
					('absolute_path' in block && block.absolute_path) ||
					('relative_path' in block && block.relative_path) ||
					('url' in block && block.url) ||
					'';
				if (key && seen.has(key)) continue;
				if (key) seen.add(key);
				all.push(block);
			}
		}
		return all;
	}

	function computeTotalDurationMs(items: ChatTurnActivityRow[] | null): number | null {
		if (!items || items.length === 0) return null;
		const startsAt = Math.min(...items.map((r) => r.startedAt));
		const endsAt = Math.max(
			...items.map((r) => r.startedAt + (r.durationMs ?? 0))
		);
		const diff = endsAt - startsAt;
		return Number.isFinite(diff) && diff > 0 ? diff : null;
	}

	function summarySuffix(): string {
		const parts: string[] = [`${stepCount} step${stepCount === 1 ? '' : 's'}`];
		if (totalDurationMs && totalDurationMs > 0) {
			const seconds = totalDurationMs / 1000;
			parts.push(seconds >= 10 ? `${seconds.toFixed(0)}s` : `${seconds.toFixed(1)}s`);
		}
		return parts.join(' · ');
	}

	function attachedActivityLabel(msg: ChatMessage): string {
		if (msg.content.type === 'task_status_update') {
			const taskId = msg.content.task_id ?? 'task';
			return `Task ${taskId}: ${msg.content.status ?? 'unknown'}`;
		}
		return 'activity';
	}

	function attachedActivitySummary(msg: ChatMessage): string | null {
		if (msg.content.type === 'task_status_update') {
			return msg.content.summary ?? null;
		}
		return null;
	}

	function attachedActivityStatus(
		msg: ChatMessage
	): 'done' | 'failed' | 'waiting' | 'running' {
		const status = msg.content.type === 'task_status_update' ? msg.content.status ?? '' : '';
		if (status === 'completed') return 'done';
		if (status === 'failed' || status === 'cancelled') return 'failed';
		if (status === 'waiting' || status === 'paused') return 'waiting';
		return 'running';
	}
</script>

{#if hasContent || assistantOutputFiles.length > 0}
	<div class="assistant-activity">
		{#if assistantOutputFiles.length > 0}
			<!-- Top-level files strip: every output file from in-turn
			     activity surfaces here so the user doesn't have to
			     expand the dropdown to access them. -->
			<div class="assistant-activity__files">
				<div class="assistant-activity__files-label" aria-hidden="true">
					Files generated
				</div>
				<ChatContentBlocks
					sessionId={assistantOutputSessionId}
					blocks={assistantOutputFiles}
				/>
			</div>
		{/if}
		{#if hasContent}
		<!-- svelte-ignore a11y_no_static_element_interactions -->
		<button
			type="button"
			class="assistant-activity__toggle"
			aria-expanded={isOpen}
			on:click={() => (isOpen = !isOpen)}
		>
			<span class="assistant-activity__chevron" aria-hidden="true">
				{isOpen ? '▾' : '▸'}
			</span>
			<span class="assistant-activity__label">What happened</span>
			<span class="assistant-activity__meta">{summarySuffix()}</span>
		</button>
		{/if}
		{#if isOpen}
			<ul class="assistant-activity__list" transition:slide={{ duration: 140 }}>
				{#if rows}
					{#each rows as row (row.key)}
						<li
							class="assistant-activity__row assistant-activity__row--{row.tone}"
							class:assistant-activity__row--done={row.status === 'done'}
							class:assistant-activity__row--failed={row.status === 'failed'}
							class:assistant-activity__row--waiting={row.status === 'waiting'}
						>
							<span class="assistant-activity__status" aria-hidden="true">
								{#if row.status === 'done'}
									✓
								{:else if row.status === 'failed'}
									✕
								{:else if row.status === 'waiting'}
									⏸
								{:else}
									◐
								{/if}
							</span>
							<span class="assistant-activity__row-label">{row.label}</span>
							{#if row.detail}
								<div class="assistant-activity__row-detail">
									<ChatMarkdown
										content={row.detail}
										sessionId={assistantOutputSessionId || null}
									/>
								</div>
							{/if}
							{#if row.durationMs}
								<span class="assistant-activity__row-duration">
									{Math.round(row.durationMs)}ms
								</span>
							{/if}
						</li>
					{/each}
				{/if}
				{#each attachedActivity as msg (msg.id)}
					{@const status = attachedActivityStatus(msg)}
					{@const summary = attachedActivitySummary(msg)}
					<li
						class="assistant-activity__row assistant-activity__row--tool"
						class:assistant-activity__row--done={status === 'done'}
						class:assistant-activity__row--failed={status === 'failed'}
						class:assistant-activity__row--waiting={status === 'waiting'}
					>
						<span class="assistant-activity__status" aria-hidden="true">
							{#if status === 'done'}
								✓
							{:else if status === 'failed'}
								✕
							{:else if status === 'waiting'}
								⏸
							{:else}
								◐
							{/if}
						</span>
						<span class="assistant-activity__row-label">{attachedActivityLabel(msg)}</span>
						{#if summary}
							<div class="assistant-activity__row-detail">
								<ChatMarkdown content={summary} sessionId={msg.session_id} />
							</div>
						{/if}
						<!-- Output files render at the bubble level (top
						     "Files generated" strip above the dropdown) so
						     they're accessible without expanding. No per-row
						     file rendering here to avoid duplication. -->
					</li>
				{/each}
			</ul>
		{/if}
	</div>
{/if}

<style>
	.assistant-activity {
		margin-top: 0.5rem;
		font-family: var(--font-mono, ui-monospace, monospace);
		font-size: 0.72rem;
		border-top: 1px dashed
			color-mix(in srgb, var(--border-soft, currentColor) 30%, transparent);
		padding-top: 0.35rem;
	}

	.assistant-activity__toggle {
		appearance: none;
		background: transparent;
		border: none;
		padding: 0.15rem 0.25rem;
		display: inline-flex;
		align-items: center;
		gap: 0.45rem;
		color: var(--text-muted);
		font-family: inherit;
		font-size: inherit;
		cursor: pointer;
		border-radius: 4px;
		transition: background 120ms ease, color 120ms ease;
	}

	.assistant-activity__toggle:hover {
		background: color-mix(in srgb, var(--accent-primary) 8%, transparent);
		color: var(--text-primary);
	}

	.assistant-activity__chevron {
		display: inline-block;
		width: 0.7rem;
		opacity: 0.7;
	}

	.assistant-activity__label {
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.1em;
		font-size: 0.66rem;
	}

	.assistant-activity__meta {
		opacity: 0.7;
		font-size: 0.66rem;
	}

	.assistant-activity__list {
		margin: 0.35rem 0 0 0;
		padding: 0 0 0 1rem;
		list-style: none;
		display: flex;
		flex-direction: column;
		gap: 0.18rem;
	}

	.assistant-activity__row {
		display: grid;
		grid-template-columns: 0.8rem max-content 1fr max-content;
		align-items: baseline;
		gap: 0.45rem;
		line-height: 1.35;
		min-width: 0;
	}

	.assistant-activity__status {
		opacity: 0.7;
		font-size: 0.74rem;
	}

	.assistant-activity__row--done .assistant-activity__status {
		color: var(--color-success, #4ea64e);
	}

	.assistant-activity__row--failed .assistant-activity__status {
		color: var(--color-error, #b04545);
	}

	.assistant-activity__row--waiting .assistant-activity__status {
		color: var(--accent-primary);
	}

	.assistant-activity__row-label {
		font-weight: 600;
		color: var(--text-primary);
		white-space: nowrap;
	}

	.assistant-activity__row-detail {
		color: var(--text-secondary);
		overflow-wrap: anywhere;
		word-break: break-word;
		min-width: 0;
		opacity: 0.85;
	}

	/* `row.detail` and pack/task `summary` now render through ChatMarkdown,
	   which emits a wrapping <div class="chat-markdown"> with default block
	   spacing + body font. Clamp it back to inline-friendly tokens so the
	   row stays a single grid line and the path icons sit flush with the
	   surrounding mono text. */
	.assistant-activity__row-detail :global(.chat-markdown) {
		font-family: inherit;
		font-size: inherit;
		line-height: inherit;
		color: inherit;
		margin: 0;
	}
	.assistant-activity__row-detail :global(.chat-markdown > *:first-child) {
		margin-top: 0;
	}
	.assistant-activity__row-detail :global(.chat-markdown > *:last-child) {
		margin-bottom: 0;
	}
	.assistant-activity__row-detail :global(.chat-markdown p) {
		margin: 0;
		display: inline;
	}

	.assistant-activity__row-duration {
		color: var(--text-muted);
		opacity: 0.7;
		font-size: 0.66rem;
	}

	.assistant-activity__row--tool .assistant-activity__row-label {
		color: var(--accent-primary);
	}

	.assistant-activity__row--reasoning .assistant-activity__row-label {
		color: var(--text-muted);
		font-style: italic;
	}

	.assistant-activity__row--error .assistant-activity__row-label {
		color: var(--color-error, #b04545);
	}

	/* Output blocks (image previews, file links from pack runs) folded
	   into the activity dropdown via `attachedActivity`. Grid-spans the
	   detail + duration columns so the preview can breathe; small max
	   height to keep the dropdown compact. */
	.assistant-activity__row-blocks {
		grid-column: 2 / -1;
		margin-top: 0.25rem;
		max-width: 100%;
	}

	/* Top-level "Files generated" strip above the dropdown toggle.
	   Always visible (no click required) so output files are reachable
	   immediately. The outer .assistant-activity container's border-top
	   already provides the visual separator from the assistant text. */
	.assistant-activity__files {
		display: flex;
		flex-direction: column;
		gap: 0.3rem;
		margin-bottom: 0.4rem;
	}

	.assistant-activity__files-label {
		font-family: var(--font-mono, ui-monospace, monospace);
		font-size: 0.66rem;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.1em;
		color: var(--text-muted);
		opacity: 0.85;
	}
</style>
