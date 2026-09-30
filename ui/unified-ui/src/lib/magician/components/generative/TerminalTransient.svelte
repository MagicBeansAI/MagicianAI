<script lang="ts">
	import { afterUpdate } from 'svelte';

	export let line: string | undefined = undefined;
	export let seq: number | undefined = undefined;
	export let label: string = '';
	export let maxLines: number = 100;
	// R104: Sanitize maxLines — NaN or non-positive values fall back to default
	$: safeMaxLines = (Number.isFinite(+maxLines) && +maxLines > 0) ? Math.floor(+maxLines) : 100;
	export let autoscroll: boolean = true;

	// R303: Use keyed entries for stable DOM reconciliation — when maxLines cap
	// trims from the beginning, only 1 removal + 1 insertion instead of N updates.
	interface LineEntry { id: number; text: string; }
	let lineEntries: LineEntry[] = [];
	let nextLineId = 0;

	// R579: Trim existing buffer when safeMaxLines decreases (e.g., prop change
	// from 100 to 10). Without this, the component shows more lines than the cap
	// allows until the next line append triggers the cap logic.
	$: if (safeMaxLines < lineEntries.length) {
		lineEntries = lineEntries.slice(-safeMaxLines);
	}
	let scrollEl: HTMLElement;
	let userScrolledUp = false;
	let lastSeq = -1;
	// R530: Sentinel value ensures the first empty-string line is not dedup'd
	// against the initial state when seq is absent.
	let lastAppended: string | null = null;

	// Append new line when `seq` changes (R17 dedup via monotonic counter).
	// When seq is present: use seq-based dedup (handles identical consecutive lines).
	// When seq is absent: fall back to string comparison (snapshot recovery, testing).
	// The emitter reuses the same component_id for all TerminalTransient
	// upserts, so MuijStore replaces `line` on each delta. This reactive
	// block accumulates lines into an internal history.
	// R302: Use `line != null` instead of `line` to preserve empty-string lines (visual separators).
	// R688: Added third seq=0 case — when both old and new cycle start at seq=0,
	// fall back to content comparison to avoid dedup'ing the new cycle's first line.
	$: if (line != null && (seq != null ? (seq !== lastSeq || (seq === 0 && lastSeq > 0) || (seq === 0 && lastSeq === 0 && line !== lastAppended)) : (lastAppended === null || line !== lastAppended))) {
		// R34+R114: seq at or below lastSeq signals a new cycle — clear stale history
		// from a prior cycle (guards against missed AgentCycleCompleted events).
		if (seq != null && lastSeq >= 0 && seq <= lastSeq) {
			lineEntries = [];
			nextLineId = 0;  // R692: Reset counter on cycle boundary
			lastAppended = null;  // R134: Reset dedup state on cycle boundary
		}
		if (seq != null) lastSeq = seq;
		lastAppended = line;
		const entry: LineEntry = { id: nextLineId++, text: line };
		// R529: When safeMaxLines=1, slice(-0)===slice(0) returns the full array.
		// Guard: if cap is 1, just replace with the new entry.
		lineEntries =
			lineEntries.length >= safeMaxLines
				? (safeMaxLines <= 1 ? [entry] : [...lineEntries.slice(-(safeMaxLines - 1)), entry])
				: [...lineEntries, entry];
	}

	// Auto-scroll to bottom after DOM update (unless user scrolled up)
	afterUpdate(() => {
		if (autoscroll && !userScrolledUp && scrollEl) {
			scrollEl.scrollTop = scrollEl.scrollHeight;
		}
	});

	function handleScroll() {
		if (!scrollEl) return;
		const { scrollTop, scrollHeight, clientHeight } = scrollEl;
		userScrolledUp = scrollHeight - scrollTop - clientHeight > 20;
	}
</script>

<!-- R665: Live region exists in DOM before first line insertion so assistive tech
     can detect it. Hidden via CSS when empty; `role="log"` inside conditional block. -->
<div class="muij-terminal" class:muij-terminal-empty={lineEntries.length === 0} role="region" aria-label={(label && label.trim()) || 'Agent Log'}>
	{#if lineEntries.length > 0}
		<!-- R528: Removed hardcoded fallback ID to avoid duplicate DOM IDs across instances -->
		<div class="muij-terminal-header">
			<span class="muij-terminal-dot"></span>
			<!-- R531: Trim label to prevent whitespace-only values from suppressing fallback -->
		<span class="muij-terminal-title">{(label && label.trim()) || 'Agent Log'}</span>
			<span class="muij-terminal-count">{lineEntries.length}</span>
		</div>
	{/if}
	<!-- R334: aria-label provides accessible name for the log region -->
	<!-- R666: aria-relevant="additions" prevents re-announcement when buffer trims old lines -->
	<div class="muij-terminal-body" role="log" aria-live="polite" aria-relevant="additions" aria-label={(label && label.trim()) || 'Agent Log'} bind:this={scrollEl} on:scroll={handleScroll}>
		{#each lineEntries as entry (entry.id)}
			<div class="muij-terminal-line">{entry.text}</div>
		{/each}
	</div>
</div>

<style>
	.muij-terminal {
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-md);
		overflow: hidden;
		background: var(--muij-terminal-bg, #111827);
		font-family: var(--font-mono);
		font-size: 0.75rem;
		line-height: 1.5;
		width: 100%;
	}

	/* R665: Visually hidden when empty but remains in accessibility tree
	   so screen readers register the aria-live region before first insertion. */
	.muij-terminal-empty {
		position: absolute;
		width: 1px;
		height: 1px;
		padding: 0;
		margin: -1px;
		overflow: hidden;
		clip: rect(0, 0, 0, 0);
		white-space: nowrap;
		border: 0;
	}

	.muij-terminal-header {
		display: flex;
		align-items: center;
		gap: 6px;
		padding: 6px 10px;
		background: var(--muij-terminal-header-bg, #0d1117);
		border-bottom: 1px solid var(--muij-terminal-header-border, #21262d);
		color: var(--muij-terminal-header-color, #8b949e);
		font-size: 0.65rem;
		user-select: none;
	}

	.muij-terminal-dot {
		width: 8px;
		height: 8px;
		border-radius: 50%;
		background: var(--accent-primary);
		flex-shrink: 0;
	}

	.muij-terminal-title {
		flex: 1;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.muij-terminal-count {
		color: var(--muij-terminal-count-color, #6e7681);
	}

	.muij-terminal-body {
		max-height: 200px;
		overflow-y: auto;
		padding: 6px 10px;
		scrollbar-width: thin;
		scrollbar-color: var(--muij-terminal-scrollbar, #30363d) transparent;
	}

	.muij-terminal-line {
		color: var(--muij-terminal-line-color, #e6edf3);
		padding: 1px 0;
		word-break: break-all;
	}

	.muij-terminal-body::-webkit-scrollbar {
		width: 6px;
	}

	.muij-terminal-body::-webkit-scrollbar-track {
		background: transparent;
	}

	.muij-terminal-body::-webkit-scrollbar-thumb {
		background: var(--muij-terminal-scrollbar, #30363d);
		border-radius: 3px;
	}

	.muij-terminal-body::-webkit-scrollbar-thumb:hover {
		background: var(--muij-terminal-scrollbar-hover, #484f58);
	}
</style>
