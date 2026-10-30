<script lang="ts">
	/**
	 * A confirmation's structured approval card (`schema.approval_preview`).
	 *
	 * Generic over the tool that produced it: who wants to do what, the
	 * agent's stated reason (labelled as the agent's words), the change as a
	 * diff, the evidence offered, and warnings that must be read before
	 * deciding. The raw call stays one click away for auditing.
	 *
	 * Everything here is rendered as text. `why`, `evidence` and the diff lines
	 * are agent-authored, and Svelte's `{…}` escapes them — no `{@html}`.
	 * The diff reuses `DiffStrip`, the renderer the `diff_approval` shape uses.
	 */
	import DiffStrip from '$lib/shell/DiffStrip.svelte';
	import type { HitlApprovalPreview } from './types';

	export let preview: HitlApprovalPreview;

	$: diff = preview.diff;
	$: changeLabel = diff ? `+${diff.added} lines, −${diff.removed} lines` : null;
</script>

<section class="approval-preview" data-testid="approval-preview">
	<p class="approval-preview__title">{preview.title}</p>

	{#if preview.warnings.length > 0}
		<div class="approval-preview__warnings" role="alert">
			{#each preview.warnings as warning (warning.code + warning.message)}
				<p class="approval-preview__warning" data-warning-code={warning.code}>{warning.message}</p>
			{/each}
		</div>
	{/if}

	{#if preview.why}
		<div class="approval-preview__why">
			<span class="approval-preview__label">{preview.why_label ?? "Why (agent's words)"}</span>
			<p class="approval-preview__why-text">{preview.why}</p>
		</div>
	{/if}

	{#if diff}
		<div class="approval-preview__changes">
			<span class="approval-preview__label">Changes</span>
			<span class="approval-preview__stats" data-testid="approval-preview-stats">{changeLabel}</span>
		</div>
		{#if diff.unified_diff}
			<DiffStrip
				diff={{
					files: [
						{
							path: diff.path,
							status: 'M',
							additions: diff.added,
							deletions: diff.removed,
							unified_diff: diff.unified_diff
						}
					]
				}}
				lifecycle="pending"
				title="Proposed change"
				showStats={false}
				allowCopy={true}
				autoExpandFirst={true}
				showLineNumbers={true}
				wrapLongLines={true}
				truncateLinesThreshold={0}
			/>
		{/if}
		{#if diff.truncated && diff.omitted_changed_lines > 0}
			<p class="approval-preview__more">… {diff.omitted_changed_lines} more changed lines</p>
		{/if}
	{/if}

	{#each preview.notes as note (note)}
		<p class="approval-preview__note">{note}</p>
	{/each}

	{#if preview.evidence.length > 0}
		<div class="approval-preview__evidence">
			<span class="approval-preview__label">Evidence</span>
			<ul>
				{#each preview.evidence as entry, index (index)}
					<li>{entry}</li>
				{/each}
			</ul>
		</div>
	{/if}

	{#if preview.raw}
		<details class="approval-preview__raw">
			<summary>Raw call</summary>
			<pre>{preview.raw}</pre>
		</details>
	{/if}
</section>

<style>
	.approval-preview {
		display: flex;
		flex-direction: column;
		gap: 0.6rem;
		min-width: 0;
	}

	.approval-preview__title {
		margin: 0;
		font-weight: 600;
		color: var(--text-primary, inherit);
	}

	.approval-preview__warnings {
		border: 1px solid var(--status-warning, #d97706);
		background: color-mix(in srgb, var(--status-warning, #d97706) 12%, transparent);
		border-radius: 0.5rem;
		padding: 0.5rem 0.75rem;
	}

	.approval-preview__warning {
		margin: 0;
		font-weight: 600;
		color: var(--text-primary, inherit);
	}

	.approval-preview__warning + .approval-preview__warning {
		margin-top: 0.35rem;
	}

	.approval-preview__label {
		font-size: 0.75rem;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.04em;
		color: var(--text-secondary, inherit);
	}

	.approval-preview__why-text {
		margin: 0.2rem 0 0;
		white-space: pre-wrap;
		overflow-wrap: anywhere;
	}

	.approval-preview__changes {
		display: flex;
		align-items: baseline;
		gap: 0.5rem;
	}

	.approval-preview__stats {
		font-family: var(--font-mono, ui-monospace, monospace);
		font-size: 0.8rem;
	}

	.approval-preview__more,
	.approval-preview__note {
		margin: 0;
		font-size: 0.8rem;
		color: var(--text-secondary, inherit);
	}

	.approval-preview__evidence ul {
		margin: 0.2rem 0 0;
		padding-left: 1.1rem;
	}

	.approval-preview__evidence li {
		overflow-wrap: anywhere;
	}

	.approval-preview__raw summary {
		cursor: pointer;
		font-size: 0.8rem;
		color: var(--text-secondary, inherit);
	}

	.approval-preview__raw pre {
		margin: 0.35rem 0 0;
		max-height: 12rem;
		overflow: auto;
		white-space: pre-wrap;
		overflow-wrap: anywhere;
		font-size: 0.75rem;
	}
</style>
