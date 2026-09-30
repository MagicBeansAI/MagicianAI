<script lang="ts" context="module">
	/**
	 * Per-file diff payload — the shape every data source must produce.
	 * Mirrors the backend `DiffFile` shape that
	 * `interactive_process_api::session_diff_handler` already returns.
	 *
	 * Re-used by chat-flow consumers (`FileEditTransaction` tool result,
	 * `HitlRequested { input_type: "diff_approval" }` payload) so the
	 * component is data-source agnostic — anything that can produce
	 * `DiffFile[]` can render through DiffStrip.
	 */
	export interface DiffFile {
		path: string;
		status: string;
		additions: number;
		deletions: number;
		unified_diff: string;
	}

	export interface DiffPayload {
		files: DiffFile[];
		working_dir?: string | null;
		note?: string | null;
	}

	export type DiffLifecycle = 'live' | 'pending' | 'applied' | 'reverted';
</script>

<script lang="ts">
	/**
	 * DiffStrip — generalized file-diff renderer.
	 *
	 * Originally a Developer Mode file-edit summary for a live
	 * `interactive_process` session (poll `/interactive-sessions/{id}/diff`
	 * every 3s, render the diff against git HEAD). The component has been
	 * generalized to fit anywhere a unified diff needs to render — chat
	 * coding cards, HITL approval modals, snapshot timeline previews,
	 * plan-mode previews — without touching the existing Dev Mode usage.
	 *
	 * Data source — provide exactly one:
	 *   - `sessionId` (current Dev Mode): polls the
	 *     `/interactive-sessions/{id}/diff` endpoint at `pollIntervalMs`.
	 *   - `diff` (event-driven): the consumer passes the diff payload
	 *     directly (typically from a tool-result event or a
	 *     HitlRequested payload). No fetch, no polling.
	 *
	 * Lifecycle drives the action buttons:
	 *   - `live` (default): read-only — preserves current Dev Mode
	 *     "watch what the agent is doing" semantics.
	 *   - `pending`: whole-card Apply / Reject footer (chat coding
	 *     cards before write; HITL approval modal). Per-file Revert is
	 *     hidden because nothing has been written yet.
	 *   - `applied`: per-file Undo button (post-write rollback within
	 *     snapshot window).
	 *   - `reverted`: read-only history view with a "Reverted." footer.
	 *
	 * Backward compatibility: the original `<DiffStrip sessionId={...} />`
	 * call site keeps working exactly as before. Every new prop has a
	 * default that reproduces v0.6.583 behavior (live mode, polling at
	 * 3s, no action buttons in the row).
	 *
	 * Per-hunk selection / split view / syntax highlighting are TODO —
	 * see `docs/plans/2026-05-18-magician-coding-agent-vision.md` Phase 2.
	 */
	import { createEventDispatcher, onDestroy, onMount } from 'svelte';
	import { timedFetch } from '$lib/shared/fetch';
	import type { HighlighterCore } from '@shikijs/core';
	import type { BundledLanguage, BundledTheme } from 'shiki';
	import {
		BUNDLED_THEMES,
		ensureLanguagesLoaded,
		highlightLine,
		languageForPath,
		loadHighlighter,
	} from './diffSyntaxHighlight';
	import { computeWordDiff, loadDiff, type WordDiffSegment } from './diffWordLevel';

	/** PTY-source mode (current Dev Mode). Mutually exclusive with `diff`. */
	export let sessionId: string | null = null;
	/** Event-driven mode — consumer passes the diff payload directly. */
	export let diff: DiffPayload | null = null;
	/**
	 * Polling cadence for `sessionId` mode. Defaults to 3000ms to match
	 * v0.6.583 Dev Mode behavior. Set to 0 to disable polling entirely
	 * (useful for one-shot snapshots or `diff`-prop callers).
	 */
	export let pollIntervalMs: number = 3000;
	/**
	 * Lifecycle state drives which action buttons render. Defaults to
	 * `live` so the original Dev Mode usage is unchanged.
	 */
	export let lifecycle: DiffLifecycle = 'live';
	/** Render Apply / Reject footer when lifecycle === 'pending'. */
	export let allowApprove: boolean = false;
	/**
	 * Render per-file Apply / Reject buttons alongside the whole-card
	 * footer when lifecycle === 'pending'. Useful for the coding agent
	 * surface where the LLM proposes N file edits and the user wants
	 * to accept a subset. Defaults to `false` so the existing
	 * approval flow stays whole-card-only.
	 */
	export let allowPerFileApproval: boolean = false;
	/**
	 * Render per-file Revert / Undo button when lifecycle is `live` or
	 * `applied`. Defaults to `false` to match current Dev Mode (which
	 * intentionally doesn't expose a Revert button today — the agent
	 * does the revert through tool calls).
	 */
	export let allowRevert: boolean = false;
	/** When true, the file list is one-line and the diff body is hidden
	 *  by default. Defaults to `false` (expanded) to match v0.6.583. */
	export let compact: boolean = false;
	/**
	 * Override the per-file expand state from the parent (controlled
	 * mode). When null (default), DiffStrip manages expand state
	 * internally (uncontrolled, current behavior).
	 */
	export let expandedPaths: Set<string> | null = null;
	/** Optional title; falls back to "Files changed" to match v0.6.583. */
	export let title: string = 'Files changed';
	/**
	 * Render a "3 files, +47/-12" summary next to the title when files
	 * list is non-empty. Defaults to `false` so the existing header
	 * stays single-count to match v0.6.583.
	 */
	export let showStats: boolean = false;
	/**
	 * Show a "Copy diff" button in the per-file actions row. Writes the
	 * raw unified-diff string to the clipboard. Defaults to `false`.
	 */
	export let allowCopy: boolean = false;
	/**
	 * Truncate per-file diffs longer than this many lines, surfacing a
	 * "Show full diff (N lines)" button instead. Prevents the chat
	 * surface from hanging on a 10K-line refactor. Set to 0 to disable
	 * truncation. Defaults to 500 lines — generous for most edits,
	 * protective against pathological cases.
	 */
	export let truncateLinesThreshold: number = 500;
	/**
	 * Render old/new line-number gutters next to each diff line. Parses
	 * hunk headers (`@@ -1,5 +1,7 @@`) to compute the columns. Defaults
	 * to `false` so the existing single-pre rendering is preserved.
	 */
	export let showLineNumbers: boolean = false;
	/**
	 * Render an "Expand all / Collapse all" pair in the header when
	 * there are 2+ files. Defaults to `false`. Operates on the
	 * uncontrolled `internalExpanded` set; ignored when `expandedPaths`
	 * is provided (controlled mode — parent owns expand state).
	 */
	export let showExpandAll: boolean = false;
	/**
	 * Third data-source mode — poll an arbitrary URL that returns a
	 * `DiffPayload`-shaped JSON body. Mutually exclusive with
	 * `sessionId` and `diff`. Useful for non-PTY backends (snapshot
	 * comparison endpoints, plan-preview endpoints) that already
	 * serve the same shape. Polled at `pollIntervalMs`.
	 */
	export let endpoint: string | null = null;
	/**
	 * Auto-expand the first file when the component mounts (or when the
	 * files list first becomes non-empty). Most diffs have one file or
	 * the first file is the most relevant; this saves one click.
	 * Defaults to `false` to preserve current Dev Mode behavior
	 * (everything collapsed at mount).
	 */
	export let autoExpandFirst: boolean = false;
	/**
	 * Wrap long lines instead of horizontal-scrolling them. Useful for
	 * minified JS, base64 payloads, long URLs. Defaults to `false` so
	 * the current monospace alignment is preserved.
	 */
	export let wrapLongLines: boolean = false;
	/**
	 * Make each file's row header stick to the top of the scroll
	 * container when scrolling through a long diff body. CSS-only
	 * (`position: sticky`). Defaults to `false` to preserve current
	 * Dev Mode layout (which is short enough not to need it).
	 */
	export let stickyFileHeaders: boolean = false;
	/**
	 * Collapse a file's diff body automatically after Apply / Reject /
	 * Revert resolves it. Frees vertical space in the coding agent's
	 * chat surface where dozens of resolved cards would otherwise pile
	 * up. Defaults to `false` (rows stay expanded after action).
	 */
	export let autoCollapseAfterAction: boolean = false;
	/**
	 * Hover a diff line → subtle highlight that helps eye-track across
	 * long diffs. CSS-only. Defaults to `false` to preserve the current
	 * non-interactive feel of the diff body.
	 */
	export let hoverLineHighlight: boolean = false;
	/**
	 * Layout of the diff body — `unified` (default, current behavior:
	 * one column with `+`/`-`/` ` markers) or `split` (side-by-side:
	 * left column for the old side, right column for the new side).
	 * Split pairs consecutive del/add lines as modifications; lone
	 * del/add lines render on one side with an empty cell on the other.
	 * Hunk headers and meta lines span both columns in split view.
	 *
	 * Auto-collapses to `unified` when the container is narrower than
	 * `splitStackBelowPx` so the side-by-side layout doesn't squeeze
	 * each side to unreadable widths in narrow contexts (chat sidebar,
	 * snapshot preview, modals).
	 */
	export let view: 'unified' | 'split' = 'unified';
	/**
	 * Threshold (px) below which `view="split"` auto-collapses to
	 * unified. The component measures its own width via Svelte's
	 * `bind:clientWidth` (ResizeObserver under the hood) and re-evaluates
	 * on resize. Set to 0 to disable the auto-collapse and always render
	 * the layout the consumer requested. Defaults to 600 — each side
	 * needs ~300px of room to fit roughly 40-50 chars of monospace
	 * code; below that the side-by-side view stops being useful.
	 */
	export let splitStackBelowPx: number = 600;
	/**
	 * Enable keyboard navigation when the component (or any descendant
	 * outside an input) has focus:
	 *   j / ArrowDown → next file
	 *   k / ArrowUp   → previous file
	 *   e             → toggle expand on the active file
	 *   a             → apply active file (lifecycle=pending,
	 *                   allowPerFileApproval)
	 *   r             → reject active file (same conditions)
	 *   x             → revert active file (lifecycle=live/applied,
	 *                   allowRevert)
	 * Defaults to `false` so no `tabindex` / keydown listener attaches.
	 */
	export let enableKeyboardNav: boolean = false;
	/**
	 * Make each file's diff body user-resizable via the native CSS
	 * `resize: vertical` handle (small triangle in the bottom-right
	 * corner). When `true`, the 360px max-height clamp is removed and
	 * the pre starts at 360px but can be dragged taller or shorter
	 * (min 80px). Defaults to `false` to preserve the current fixed
	 * 360px max-height behavior.
	 */
	export let allowResize: boolean = false;
	/**
	 * Surface git's similarity hint on rename/copy statuses (`R85`,
	 * `C70`, …) as a small percentage badge next to the status letter.
	 * When `false` (default), the status renders as the raw string
	 * (matches current Dev Mode behavior). The tooltip is *always*
	 * upgraded from raw to readable label (`Renamed (85% similarity)`)
	 * because the displayed text is unchanged — strict improvement.
	 */
	export let showSimilarity: boolean = false;
	/**
	 * Apply syntax highlighting to diff lines via Shiki. The Shiki
	 * bundle (~600KB-1MB, TextMate grammars + Oniguruma WASM) is
	 * lazy-loaded on first use, so consumers that don't opt in pay
	 * zero bundle cost. Language is auto-detected from each file's
	 * extension; unknown extensions render as plain text.
	 *
	 * Defaults to `false` so the Shiki bundle stays unloaded for
	 * non-coding-agent surfaces (the current Dev Mode usage included).
	 */
	export let syntaxHighlight: boolean = false;
	/**
	 * Shiki theme to use when `syntaxHighlight` is on. Defaults to
	 * `github-light`. Pass `github-dark` for dark surfaces. Themes
	 * are pre-loaded together so switching is instant (no re-fetch).
	 */
	export let syntaxTheme: 'github-light' | 'github-dark' = 'github-light';
	/**
	 * Highlight word-level differences inside consecutive del/add
	 * modification pairs. Uses jsdiff's `diffWordsWithSpace` to compute
	 * per-token segments; the differing words get a stronger highlight
	 * overlay on top of the line-level red/green background. The
	 * `diff` package (~15KB gzipped) is lazy-loaded on first use.
	 *
	 * Defaults to `false`. Works in both unified and split layouts —
	 * the unified path detects modification pairs as consecutive
	 * `-` lines followed by `+` lines and pairs them by index.
	 */
	export let wordLevelDiff: boolean = false;

	const dispatch = createEventDispatcher<{
		/** Apply the whole transaction (lifecycle === 'pending'). */
		apply: { files: DiffFile[] };
		/** Reject the whole transaction (lifecycle === 'pending'). */
		reject: { files: DiffFile[] };
		/** Apply a single file (lifecycle === 'pending' &&
		 *  allowPerFileApproval). */
		applyFile: { file: DiffFile };
		/** Reject a single file (lifecycle === 'pending' &&
		 *  allowPerFileApproval). */
		rejectFile: { file: DiffFile };
		/** Revert a single file (lifecycle === 'live' || 'applied'). */
		revert: { file: DiffFile };
		/** User clicked Copy on a file's diff (already copied to
		 *  clipboard by the time this fires; consumer can show a
		 *  toast / tracking ping). */
		copy: { file: DiffFile };
		/** User toggled the expand state of a file. */
		toggle: { path: string; expanded: boolean };
		/** User clicked "Show full diff" on a truncated file. */
		expandFull: { file: DiffFile };
	}>();

	let files: DiffFile[] = [];
	let error: string | null = null;
	let lastNote: string | null = null;
	let internalExpanded: Set<string> = new Set();
	$: expanded = expandedPaths ?? internalExpanded;
	let pollTimer: ReturnType<typeof setInterval> | null = null;
	let isReverting: Set<string> = new Set();
	/** Per-file "show full diff" override — when a path is in this set
	 *  its truncation is bypassed. Independent of `expanded`. */
	let showFullDiff: Set<string> = new Set();
	/** Per-file copy feedback ("Copied!" pill for 1.5s after click). */
	let recentlyCopied: Set<string> = new Set();

	// When the `diff` prop is provided, mirror it into our internal
	// state so the same render path works for both modes. Reactive on
	// the prop ref so consumers can swap diffs without re-mounting.
	$: if (diff) {
		files = diff.files ?? [];
		lastNote = diff.note ?? null;
		error = null;
	}

	// Polling is only meaningful in a source mode where we own the fetch
	// (sessionId OR endpoint). Skip when the consumer drove us with a
	// `diff` prop, or when pollIntervalMs is 0.
	$: shouldPoll =
		!diff && (sessionId != null || endpoint != null) && pollIntervalMs > 0;

	async function fetchDiff(): Promise<void> {
		// Pick the source URL — `sessionId` wins when both are set so
		// existing Dev Mode behavior is unchanged if a consumer somehow
		// passes both (which they shouldn't — mutually exclusive by
		// design, but defensive ordering helps).
		let url: string | null = null;
		if (sessionId) {
			url = `/api/magician/v2/interactive-sessions/${encodeURIComponent(sessionId)}/diff`;
		} else if (endpoint) {
			url = endpoint;
		}
		if (!url) return;
		try {
			const resp = await timedFetch(url);
			if (!resp.ok) {
				error = `HTTP ${resp.status}`;
				return;
			}
			const body = (await resp.json()) as DiffPayload;
			files = body.files ?? [];
			lastNote = body.note ?? null;
			error = null;
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		}
	}

	/**
	 * Auto-expand the first file when the files list first becomes
	 * non-empty (only in uncontrolled-expand mode and only on first
	 * appearance, so re-fetches that add files don't surprise the user
	 * by jumping their scroll).
	 */
	let autoExpandedOnce = false;
	$: if (
		autoExpandFirst &&
		!autoExpandedOnce &&
		expandedPaths === null &&
		files.length > 0
	) {
		internalExpanded = new Set([files[0].path]);
		autoExpandedOnce = true;
	}

	function toggle(path: string): void {
		const next = new Set(expanded);
		const willExpand = !next.has(path);
		if (willExpand) {
			next.add(path);
		} else {
			next.delete(path);
		}
		if (expandedPaths === null) {
			internalExpanded = next;
		}
		dispatch('toggle', { path, expanded: willExpand });
	}

	async function revertFile(file: DiffFile): Promise<void> {
		if (isReverting.has(file.path)) return;
		isReverting = new Set([...isReverting, file.path]);
		try {
			dispatch('revert', { file });
			maybeCollapseAfterAction(file.path);
		} finally {
			isReverting.delete(file.path);
			isReverting = new Set(isReverting);
		}
	}

	function approveAll(): void {
		dispatch('apply', { files });
		if (autoCollapseAfterAction && expandedPaths === null) {
			internalExpanded = new Set();
		}
	}

	function rejectAll(): void {
		dispatch('reject', { files });
		if (autoCollapseAfterAction && expandedPaths === null) {
			internalExpanded = new Set();
		}
	}

	function approveFile(file: DiffFile): void {
		dispatch('applyFile', { file });
		maybeCollapseAfterAction(file.path);
	}

	function rejectOneFile(file: DiffFile): void {
		dispatch('rejectFile', { file });
		maybeCollapseAfterAction(file.path);
	}

	function maybeCollapseAfterAction(path: string): void {
		if (!autoCollapseAfterAction || expandedPaths !== null) return;
		const next = new Set(internalExpanded);
		next.delete(path);
		internalExpanded = next;
	}

	async function copyDiff(file: DiffFile): Promise<void> {
		try {
			await navigator.clipboard.writeText(file.unified_diff);
			recentlyCopied = new Set([...recentlyCopied, file.path]);
			dispatch('copy', { file });
			setTimeout(() => {
				recentlyCopied.delete(file.path);
				recentlyCopied = new Set(recentlyCopied);
			}, 1500);
		} catch {
			// Clipboard access denied (insecure context / permission); silent.
			// Consumer can listen on 'copy' for fallback handling.
		}
	}

	function expandFullDiff(file: DiffFile): void {
		showFullDiff = new Set([...showFullDiff, file.path]);
		dispatch('expandFull', { file });
	}

	/**
	 * Render-side helper: returns either the full unified diff (when
	 * truncation is disabled, under threshold, or the user already
	 * clicked "Show full"), or a truncated slice with a trailing marker.
	 * Returns `{ lines, truncated, totalLines }` so the template can
	 * render the "Show full" button only when actually truncated.
	 */
	function visibleDiffLines(file: DiffFile): {
		lines: string[];
		truncated: boolean;
		totalLines: number;
	} {
		const all = file.unified_diff.split('\n');
		if (
			truncateLinesThreshold <= 0 ||
			all.length <= truncateLinesThreshold ||
			showFullDiff.has(file.path)
		) {
			return { lines: all, truncated: false, totalLines: all.length };
		}
		return {
			lines: all.slice(0, truncateLinesThreshold),
			truncated: true,
			totalLines: all.length,
		};
	}

	function classifyLine(line: string): string {
		if (line.startsWith('+++') || line.startsWith('---')) return 'diff-line--meta';
		if (line.startsWith('@@')) return 'diff-line--hunk';
		if (line.startsWith('+')) return 'diff-line--add';
		if (line.startsWith('-')) return 'diff-line--del';
		return 'diff-line--ctx';
	}

	/**
	 * Detect binary-file diffs. Git emits `Binary files a/foo and b/foo
	 * differ` (or `Binary files /dev/null and b/foo differ` for adds)
	 * instead of a line-by-line diff. We render a clean placeholder
	 * for these instead of a useless empty diff or garbage bytes.
	 *
	 * Also treats truly-empty unified_diff as binary-like (defensive —
	 * shouldn't happen in practice but avoids a broken render).
	 */
	function isBinaryFileDiff(file: DiffFile): boolean {
		const trimmed = file.unified_diff?.trim() ?? '';
		if (!trimmed) return true;
		return /^Binary files .* differ$/m.test(trimmed);
	}

	/**
	 * Annotate each diff line with its old-side and new-side line
	 * numbers, parsed from the most recent
	 * `@@ -old_start,_ +new_start,_ @@` hunk header. Returns parallel
	 * arrays so the template can render gutters next to each
	 * `lines[i]`.
	 *
	 * Rules:
	 *   - Meta / hunk-header lines get `null, null` (no gutter).
	 *   - Context lines (` `) advance both old and new counters.
	 *   - Add lines (`+`) advance only the new counter (old is null).
	 *   - Del lines (`-`) advance only the old counter (new is null).
	 */
	const HUNK_HEADER_RE = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/;
	function computeLineNumbers(lines: string[]): {
		old: (number | null)[];
		neu: (number | null)[];
	} {
		const old: (number | null)[] = [];
		const neu: (number | null)[] = [];
		let oldNext = 0;
		let newNext = 0;
		for (const line of lines) {
			if (line.startsWith('@@')) {
				const match = line.match(HUNK_HEADER_RE);
				if (match) {
					oldNext = Number(match[1]);
					newNext = Number(match[2]);
				}
				old.push(null);
				neu.push(null);
				continue;
			}
			if (line.startsWith('+++') || line.startsWith('---')) {
				old.push(null);
				neu.push(null);
				continue;
			}
			if (line.startsWith('+')) {
				old.push(null);
				neu.push(newNext);
				newNext += 1;
				continue;
			}
			if (line.startsWith('-')) {
				old.push(oldNext);
				neu.push(null);
				oldNext += 1;
				continue;
			}
			// Context (or trailing empty string from .split('\n')).
			old.push(oldNext);
			neu.push(newNext);
			oldNext += 1;
			newNext += 1;
		}
		return { old, neu };
	}

	function expandAll(): void {
		if (expandedPaths !== null) return; // controlled mode — parent owns
		internalExpanded = new Set(files.map((f) => f.path));
	}

	function collapseAll(): void {
		if (expandedPaths !== null) return;
		internalExpanded = new Set();
	}

	/**
	 * Turn the raw git status field into a readable label for the
	 * tooltip. Recognises:
	 *   `A` → Added       `M` → Modified
	 *   `D` → Deleted     `T` → Type change
	 *   `R<NN>` → Renamed (NN% similarity)
	 *   `C<NN>` → Copied (NN% similarity)
	 * Unknown statuses pass through unchanged.
	 */
	const SIMILARITY_RE = /^([RC])(\d+)$/;
	function statusLabel(status: string): string {
		if (!status) return '';
		if (status === 'A') return 'Added';
		if (status === 'M') return 'Modified';
		if (status === 'D') return 'Deleted';
		if (status === 'T') return 'Type change';
		const match = status.match(SIMILARITY_RE);
		if (match) {
			const kind = match[1] === 'R' ? 'Renamed' : 'Copied';
			return `${kind} (${match[2]}% similarity)`;
		}
		return status;
	}

	/**
	 * Extract the numeric similarity (NN) from rename/copy statuses
	 * (`R85`, `C70`). Returns null for plain `M`/`A`/`D`/`T` or unknown.
	 */
	function statusSimilarity(status: string): number | null {
		const match = status?.match(SIMILARITY_RE);
		return match ? Number(match[2]) : null;
	}

	/**
	 * Side-by-side row model. Each row is either a meta/hunk header
	 * spanning both columns, or a `pair` with possibly-null old/new
	 * cells (lone del = old set, new null; lone add = old null, new
	 * set; modification = both set; context = both set with same text).
	 *
	 * Pairing rule: walk lines linearly. Accumulate consecutive del
	 * lines into `pendingDels` and consecutive add lines into
	 * `pendingAdds`. On any non-del/add boundary (context, hunk, meta,
	 * end), flush them by index: `out[i] = { old: dels[i], new: adds[i] }`,
	 * filling missing sides with null. This mirrors the GitHub /
	 * git-diff-side-by-side convention without needing a real LCS.
	 */
	type SplitRow =
		| { kind: 'meta'; text: string }
		| { kind: 'hunk'; text: string }
		| {
				kind: 'pair';
				old: { line: number; text: string; cls: 'context' | 'del' } | null;
				new: { line: number; text: string; cls: 'context' | 'add' } | null;
		  };

	function toSplitRows(lines: string[]): SplitRow[] {
		const out: SplitRow[] = [];
		let oldNext = 0;
		let newNext = 0;
		let pendingDels: { line: number; text: string }[] = [];
		let pendingAdds: { line: number; text: string }[] = [];

		const flush = () => {
			const max = Math.max(pendingDels.length, pendingAdds.length);
			for (let i = 0; i < max; i += 1) {
				const d = pendingDels[i];
				const a = pendingAdds[i];
				out.push({
					kind: 'pair',
					old: d ? { line: d.line, text: d.text, cls: 'del' } : null,
					new: a ? { line: a.line, text: a.text, cls: 'add' } : null,
				});
			}
			pendingDels = [];
			pendingAdds = [];
		};

		for (const line of lines) {
			if (line.startsWith('@@')) {
				flush();
				const m = line.match(HUNK_HEADER_RE);
				if (m) {
					oldNext = Number(m[1]);
					newNext = Number(m[2]);
				}
				out.push({ kind: 'hunk', text: line });
				continue;
			}
			if (line.startsWith('+++') || line.startsWith('---')) {
				flush();
				out.push({ kind: 'meta', text: line });
				continue;
			}
			if (line.startsWith('-')) {
				pendingDels.push({ line: oldNext, text: line.slice(1) });
				oldNext += 1;
				continue;
			}
			if (line.startsWith('+')) {
				pendingAdds.push({ line: newNext, text: line.slice(1) });
				newNext += 1;
				continue;
			}
			// Context line — flush any pending del/add pairs first, then
			// emit a context pair on both sides with identical text.
			flush();
			const ctxText = line.startsWith(' ') ? line.slice(1) : line;
			out.push({
				kind: 'pair',
				old: { line: oldNext, text: ctxText, cls: 'context' },
				new: { line: newNext, text: ctxText, cls: 'context' },
			});
			oldNext += 1;
			newNext += 1;
		}
		flush();
		return out;
	}

	/**
	 * Keyboard navigation. Active when `enableKeyboardNav: true`. The
	 * section element gets a `tabindex` so it can receive focus; the
	 * keydown handler is gated on `event.target === event.currentTarget`
	 * so typing inside child inputs / buttons doesn't trigger nav.
	 *
	 * Keys (see `enableKeyboardNav` prop doc for the full list).
	 */
	let activeIndex = 0;
	$: if (activeIndex >= files.length) {
		activeIndex = Math.max(files.length - 1, 0);
	}

	function handleKeydown(event: KeyboardEvent): void {
		if (!enableKeyboardNav) return;
		// Only handle keys when the section itself has focus — not when
		// a child button / input does. Without this, pressing `j` while
		// editing a textarea would jump files.
		if (event.target !== event.currentTarget) return;
		if (files.length === 0) return;
		const file = files[activeIndex];
		switch (event.key) {
			case 'j':
			case 'ArrowDown':
				event.preventDefault();
				activeIndex = (activeIndex + 1) % files.length;
				return;
			case 'k':
			case 'ArrowUp':
				event.preventDefault();
				activeIndex = (activeIndex - 1 + files.length) % files.length;
				return;
			case 'e':
			case 'Enter':
			case ' ':
				event.preventDefault();
				if (file) toggle(file.path);
				return;
			case 'a':
				if (file && showPerFileApproval) {
					event.preventDefault();
					approveFile(file);
				}
				return;
			case 'r':
				if (file && showPerFileApproval) {
					event.preventDefault();
					rejectOneFile(file);
				}
				return;
			case 'x':
				if (file && showRevertButton) {
					event.preventDefault();
					void revertFile(file);
				}
				return;
		}
	}

	onMount(() => {
		if (shouldPoll) {
			void fetchDiff();
			pollTimer = setInterval(() => void fetchDiff(), pollIntervalMs);
		}
	});

	onDestroy(() => {
		if (pollTimer) clearInterval(pollTimer);
	});

	$: changeCount = files.length;
	$: showRevertButton = allowRevert && (lifecycle === 'live' || lifecycle === 'applied');
	$: revertLabel = lifecycle === 'applied' ? 'Undo' : 'Revert';
	$: showPerFileApproval = allowPerFileApproval && lifecycle === 'pending';
	$: totalAdditions = files.reduce((sum, f) => sum + (f.additions ?? 0), 0);
	$: totalDeletions = files.reduce((sum, f) => sum + (f.deletions ?? 0), 0);
	$: expandAllVisible =
		showExpandAll && expandedPaths === null && files.length >= 2;
	$: allExpanded = expandAllVisible && files.every((f) => expanded.has(f.path));

	/**
	 * Section width — populated by Svelte's `bind:clientWidth` on the
	 * `<section>` element. Initial value `0` means "not yet measured";
	 * the `effectiveView` derivation trusts the `view` prop until the
	 * first measurement lands so wide-screen mounts don't flash a
	 * unified layout that immediately switches to split.
	 */
	let sectionWidth = 0;
	$: effectiveView =
		view === 'split' && splitStackBelowPx > 0 && sectionWidth > 0 && sectionWidth < splitStackBelowPx
			? 'unified'
			: view;

	/**
	 * Lazy-load Shiki on first `syntaxHighlight={true}`. The highlighter
	 * core is a process-wide singleton — subsequent mounts that opt in
	 * share it. Individual language grammars are loaded below only when
	 * a matching file is expanded.
	 *
	 * Theme is treated separately: shiki is created with both bundled
	 * themes pre-loaded, so switching themes is instant (no re-fetch).
	 */
	let highlighter: HighlighterCore | null = null;
	// `highlighterAttempted` prevents the reactive block from re-firing
	// every tick when the load fails: without it, `highlighter` stays
	// null, the condition stays true, and we'd attach new `.then`/
	// `.catch` handlers to the cached rejected promise on every
	// Svelte invalidation (a leak of microtasks). One attempt per
	// mount; failure falls back to plain text permanently for this
	// instance.
	let highlighterAttempted = false;
	$: if (syntaxHighlight && !highlighter && !highlighterAttempted) {
		highlighterAttempted = true;
		loadHighlighter()
			.then((h) => {
				highlighter = h;
			})
			.catch(() => {
				// Bundle fetch failed (offline, blocked, etc.) — leave
				// highlighter null so we fall back to plain text on
				// every line. No user-facing error; the diff still
				// renders correctly without colors.
			});
	}

	$: expandedSyntaxLanguages =
		syntaxHighlight && highlighter
			? [
					...new Set(
						files
							.filter((file) => expanded.has(file.path) && !isBinaryFileDiff(file))
							.map((file) => languageForPath(file.path))
							.filter((language): language is BundledLanguage => language !== null)
					),
				].sort()
			: [];
	$: expandedSyntaxLanguageKey = expandedSyntaxLanguages.join('|');
	let loadedSyntaxLanguageKey = '';
	let syntaxLanguageRevision = 0;
	$: if (
		syntaxHighlight &&
		highlighter &&
		expandedSyntaxLanguageKey &&
		expandedSyntaxLanguageKey !== loadedSyntaxLanguageKey
	) {
		const requestedLanguageKey = expandedSyntaxLanguageKey;
		ensureLanguagesLoaded(highlighter, expandedSyntaxLanguages)
			.then(() => {
				if (requestedLanguageKey !== expandedSyntaxLanguageKey) return;
				loadedSyntaxLanguageKey = requestedLanguageKey;
				// Trigger a render pass so lines that rendered as escaped
				// plain text repaint with syntax colors after their grammar
				// chunk arrives.
				syntaxLanguageRevision += 1;
			})
			.catch(() => {
				// Fail soft — highlighting is decorative. The diff body
				// remains readable as escaped plain text.
			});
	}

	/**
	 * Lazy-load jsdiff on first `wordLevelDiff={true}`. Same pattern:
	 * singleton Promise, reactive setter, fail-soft to plain rendering.
	 */
	let diffMod: typeof import('diff') | null = null;
	// Same attempted-flag pattern as the Shiki loader above; see
	// `highlighterAttempted` doc.
	let diffModAttempted = false;
	$: if (wordLevelDiff && !diffMod && !diffModAttempted) {
		diffModAttempted = true;
		loadDiff()
			.then((m) => {
				diffMod = m;
			})
			.catch(() => {
				// Same fail-soft behavior — the diff renders without
				// word-level overlays.
			});
	}

	/** Cached language per file path (computed once per file). */
	function langForFile(path: string): BundledLanguage | null {
		return languageForPath(path);
	}

	/**
	 * Render one diff line as HTML — either syntax-highlighted (when
	 * the highlighter is ready and the language is known) or escaped
	 * plain text. The leading +/- marker is preserved either way.
	 *
	 * Caller renders the result with `{@html ...}`. The escape logic
	 * inside `highlightLine` prevents XSS even on plain-text fallback.
	 */
	function renderLineHtml(line: string, lang: BundledLanguage | null): string {
		void syntaxLanguageRevision;
		return highlightLine(highlighter, line, lang, syntaxTheme);
	}

	/**
	 * Per-line length cap above which word-level diff is skipped. The
	 * underlying `diffWordsWithSpace` is O(N×M) where N+M is line
	 * length, so a single minified-JS or base64-blob line over ~100KB
	 * locks the UI thread for seconds. 4000 chars (≈ 60 lines of
	 * normal code wrapped onto one row) is generous for real code and
	 * cheap enough to compute synchronously. Lines over the cap still
	 * render the line-level red/green tint and syntax highlighting —
	 * they just lose the per-word overlay.
	 */
	const WORD_DIFF_MAX_LINE_CHARS = 4000;

	/**
	 * Render a side of a split-view modification cell with word-level
	 * overlay segments wrapped in inline marks. Used for both old
	 * (removed words) and new (added words) sides of a pair.
	 *
	 * Falls back to plain escaped text when jsdiff hasn't loaded,
	 * when there's no pair to compare against, OR when either side
	 * exceeds `WORD_DIFF_MAX_LINE_CHARS` (DoS guard).
	 */
	function renderSplitCellHtml(
		text: string,
		side: 'old' | 'new',
		pairOther: string | null,
		lang: BundledLanguage | null
	): string {
		// Word-level overlay only applies when both sides of a pair
		// have content (a true modification, not a lone add/del) AND
		// both fit under the per-line length cap.
		if (
			wordLevelDiff &&
			diffMod &&
			pairOther !== null &&
			text.length <= WORD_DIFF_MAX_LINE_CHARS &&
			pairOther.length <= WORD_DIFF_MAX_LINE_CHARS
		) {
			const wd = computeWordDiff(diffMod, side === 'old' ? text : pairOther, side === 'old' ? pairOther : text);
			if (wd) {
				const segments: WordDiffSegment[] = side === 'old' ? wd.old : wd.new;
				return segments
					.map((seg) => {
						const html = highlighter && lang
							? highlightLine(highlighter, seg.value, lang, syntaxTheme)
							: escapeHtmlLocal(seg.value);
						if (seg.added) return `<mark class="diff-word--add">${html}</mark>`;
						if (seg.removed) return `<mark class="diff-word--del">${html}</mark>`;
						return html;
					})
					.join('');
			}
		}
		return highlighter && lang
			? highlightLine(highlighter, text, lang, syntaxTheme)
			: escapeHtmlLocal(text);
	}

	function escapeHtmlLocal(s: string): string {
		return s
			.replace(/&/g, '&amp;')
			.replace(/</g, '&lt;')
			.replace(/>/g, '&gt;');
	}

	/**
	 * Detect word-level partner lines in unified view.
	 *
	 * Walk the diff lines, and for each contiguous run of `-` lines
	 * immediately followed by a contiguous run of `+` lines, pair
	 * them by index: the i-th `-` line in the del-run pairs with the
	 * i-th `+` line in the add-run. Pairs are stored as parallel
	 * arrays so the per-line renderer can look up its partner in
	 * O(1) without re-walking.
	 *
	 * Returns the array of partner texts (or null) sized to match
	 * `lines.length`. Marker characters are stripped from the partner
	 * text so the word diff operates on the actual code content.
	 */
	function computeUnifiedPairs(lines: string[]): (string | null)[] {
		const partners: (string | null)[] = new Array(lines.length).fill(null);
		let i = 0;
		while (i < lines.length) {
			const line = lines[i];
			// A bare `-` line (not the `---` meta header) starts a
			// potential modification block.
			if (line.startsWith('-') && !line.startsWith('---')) {
				const delStart = i;
				while (
					i < lines.length &&
					lines[i].startsWith('-') &&
					!lines[i].startsWith('---')
				) {
					i += 1;
				}
				const delEnd = i;
				// Consecutive `+` lines (no intervening context) form
				// the paired add-run.
				const addStart = i;
				while (
					i < lines.length &&
					lines[i].startsWith('+') &&
					!lines[i].startsWith('+++')
				) {
					i += 1;
				}
				const addEnd = i;
				const pairCount = Math.min(delEnd - delStart, addEnd - addStart);
				for (let j = 0; j < pairCount; j += 1) {
					const delIdx = delStart + j;
					const addIdx = addStart + j;
					partners[delIdx] = lines[addIdx].slice(1);
					partners[addIdx] = lines[delIdx].slice(1);
				}
			} else {
				i += 1;
			}
		}
		return partners;
	}

	/**
	 * Per-line render for the unified view. Handles three cases:
	 *   - hunk / meta lines: escape only (these aren't source code)
	 *   - paired modification lines (when `wordLevelDiff` + `diffMod`):
	 *     compute word-level segments against the partner line, wrap
	 *     changed words in `<mark>` tags
	 *   - everything else: syntax-highlight via shiki (when ready)
	 *
	 * Falls back to plain escaped text at every uncertain step so a
	 * missing highlighter / unknown language / failed word-diff never
	 * breaks the render.
	 */
	function renderUnifiedLineHtml(
		line: string,
		lang: BundledLanguage | null,
		partnerLine: string | null
	): string {
		if (
			line.startsWith('@@') ||
			line.startsWith('+++') ||
			line.startsWith('---')
		) {
			return escapeHtmlLocal(line);
		}
		const marker = line[0];
		const isAdd = marker === '+';
		const isDel = marker === '-';
		// Word-level overlay only when both sides fit under the per-line
		// length cap. `diffWordsWithSpace` is O(N×M); without this gate
		// a 100KB minified-JS line blocks the UI thread for seconds.
		// See `WORD_DIFF_MAX_LINE_CHARS` doc for the rationale.
		const text = isAdd || isDel ? line.slice(1) : '';
		if (
			wordLevelDiff &&
			diffMod &&
			partnerLine !== null &&
			(isAdd || isDel) &&
			text.length <= WORD_DIFF_MAX_LINE_CHARS &&
			partnerLine.length <= WORD_DIFF_MAX_LINE_CHARS
		) {
			const wd = computeWordDiff(
				diffMod,
				isDel ? text : partnerLine,
				isDel ? partnerLine : text
			);
			if (wd) {
				const segments: WordDiffSegment[] = isDel ? wd.old : wd.new;
				const inner = segments
					.map((seg) => {
						const html =
							highlighter && lang
								? highlightLine(highlighter, seg.value, lang, syntaxTheme)
								: escapeHtmlLocal(seg.value);
						if (seg.added) return `<mark class="diff-word--add">${html}</mark>`;
						if (seg.removed) return `<mark class="diff-word--del">${html}</mark>`;
						return html;
					})
					.join('');
				return escapeHtmlLocal(marker) + inner;
			}
		}
		return renderLineHtml(line, lang);
	}

	/**
	 * Suppress the unused-import warning for `BUNDLED_THEMES` and
	 * `BundledTheme` — they're exported from the helper module for
	 * type-checking consumer code (e.g. a future `<DiffStripThemePicker />`)
	 * but not directly referenced inside the component. A reference
	 * here keeps the type information reachable through the bundler.
	 */
	const _retainedThemeTypes: { themes: typeof BUNDLED_THEMES; one?: BundledTheme } = {
		themes: BUNDLED_THEMES,
	};
</script>

<!--
  svelte-ignore a11y_no_static_element_interactions
  svelte-ignore a11y_no_noninteractive_tabindex
  svelte-ignore a11y_no_noninteractive_element_interactions
  When enableKeyboardNav is on we give the section a tabindex so it can
  receive focus and dispatch keydown to handleKeydown. The events on
  this element are intentionally non-interactive at the section level
  (clicks land on child buttons); the a11y warnings are silenced
  because the keyboard navigation is the explicit accessibility win.
-->
<section
	class="diff-strip"
	class:diff-strip--compact={compact}
	class:diff-strip--wrap={wrapLongLines}
	class:diff-strip--sticky-headers={stickyFileHeaders}
	class:diff-strip--hover-lines={hoverLineHighlight}
	class:diff-strip--split={effectiveView === 'split'}
	class:diff-strip--split-collapsed={view === 'split' && effectiveView === 'unified'}
	class:diff-strip--resizable={allowResize}
	class:diff-strip--keyboard={enableKeyboardNav}
	aria-label="Files changed"
	tabindex={enableKeyboardNav ? 0 : undefined}
	bind:clientWidth={sectionWidth}
	on:keydown={enableKeyboardNav ? handleKeydown : undefined}
>
	<header class="diff-strip__head">
		<slot name="header">
			<span class="diff-strip__title">{title}</span>
			<span class="diff-strip__count">{changeCount}</span>
			{#if showStats && files.length > 0}
				<span class="diff-strip__stats">
					<span
						class="diff-strip__add"
						title="{totalAdditions} added line{totalAdditions === 1 ? '' : 's'}"
					>+{totalAdditions}</span>
					<span
						class="diff-strip__del"
						title="{totalDeletions} removed line{totalDeletions === 1 ? '' : 's'}"
					>-{totalDeletions}</span>
				</span>
			{/if}
			{#if expandAllVisible}
				<button
					type="button"
					class="diff-strip__header-action"
					on:click={() => (allExpanded ? collapseAll() : expandAll())}
				>{allExpanded ? 'Collapse all' : 'Expand all'}</button>
			{/if}
		</slot>
	</header>

	{#if error}
		<p class="diff-strip__error">Error: {error}</p>
	{:else if lastNote && files.length === 0}
		<p class="diff-strip__note">{lastNote}</p>
	{:else if files.length === 0}
		<slot name="empty">
			<p class="diff-strip__empty">No edits yet.</p>
		</slot>
	{:else}
		<ul class="diff-strip__list">
			{#each files as file, fileIdx (file.path)}
				{@const similarity = showSimilarity ? statusSimilarity(file.status) : null}
				<li
					class="diff-strip__entry"
					class:diff-strip__entry--active={enableKeyboardNav && fileIdx === activeIndex}
				>
					<button
						type="button"
						class="diff-strip__row"
						aria-expanded={expanded.has(file.path)}
						title={statusLabel(file.status)}
						on:click={() => toggle(file.path)}
					>
						<span class="diff-strip__status">{file.status}</span>
						{#if similarity !== null}
							<span class="diff-strip__similarity">{similarity}%</span>
						{/if}
						<span class="diff-strip__path">{file.path}</span>
						<span class="diff-strip__counts">
							<span
								class="diff-strip__add"
								title="{file.additions} added line{file.additions === 1 ? '' : 's'}"
							>+{file.additions}</span>
							<span
								class="diff-strip__del"
								title="{file.deletions} removed line{file.deletions === 1 ? '' : 's'}"
							>-{file.deletions}</span>
						</span>
					</button>
					{#if expanded.has(file.path)}
						{@const lineSlice = visibleDiffLines(file)}
						{@const binary = isBinaryFileDiff(file)}
						{@const gutters = !binary && effectiveView === 'unified' && showLineNumbers ? computeLineNumbers(lineSlice.lines) : null}
						{@const splitRows = !binary && effectiveView === 'split' ? toSplitRows(lineSlice.lines) : null}
						{@const lang = !binary && (syntaxHighlight || wordLevelDiff) ? langForFile(file.path) : null}
						{@const useEnhancedRender = !binary && (syntaxHighlight || wordLevelDiff)}
						{@const unifiedPartners = !binary && wordLevelDiff && effectiveView === 'unified' ? computeUnifiedPairs(lineSlice.lines) : null}
						<div
							class="diff-strip__diff"
							class:diff-strip__diff--gutter={!!gutters}
							class:diff-strip__diff--split={!!splitRows}
							class:diff-strip__diff--syntax={!!useEnhancedRender}
						>
							{#if binary}
								<p class="diff-strip__binary">Binary file — diff not shown.</p>
							{:else if splitRows}
								<div class="diff-strip__split-grid">
									{#each splitRows as row, i (i)}
										{#if row.kind === 'meta'}
											<div class="diff-strip__split-meta diff-line--meta">{row.text}</div>
										{:else if row.kind === 'hunk'}
											<div class="diff-strip__split-meta diff-line--hunk">{row.text}</div>
										{:else}
											{#if showLineNumbers}
												<span class="diff-strip__gutter diff-strip__split-gutter--old">{row.old?.line ?? ''}</span>
											{/if}
											{#if useEnhancedRender && row.old}
												<span
													class="diff-strip__split-cell diff-line--{row.old.cls}"
												>{@html renderSplitCellHtml(row.old.text, 'old', row.new ? row.new.text : null, lang)}</span>
											{:else}
												<span class="diff-strip__split-cell diff-line--{row.old?.cls ?? 'empty'}">{row.old?.text ?? ''}</span>
											{/if}
											{#if showLineNumbers}
												<span class="diff-strip__gutter diff-strip__split-gutter--new">{row.new?.line ?? ''}</span>
											{/if}
											{#if useEnhancedRender && row.new}
												<span
													class="diff-strip__split-cell diff-line--{row.new.cls}"
												>{@html renderSplitCellHtml(row.new.text, 'new', row.old ? row.old.text : null, lang)}</span>
											{:else}
												<span class="diff-strip__split-cell diff-line--{row.new?.cls ?? 'empty'}">{row.new?.text ?? ''}</span>
											{/if}
										{/if}
									{/each}
								</div>
							{:else if gutters}
								<pre class="diff-strip__pre--gutter">
{#each lineSlice.lines as line, i (i)}
<span class="diff-strip__gutter">{gutters.old[i] ?? ''}</span><span class="diff-strip__gutter">{gutters.neu[i] ?? ''}</span><span class={classifyLine(line)}>{#if useEnhancedRender}{@html renderUnifiedLineHtml(line, lang, unifiedPartners ? unifiedPartners[i] : null)}{:else}{line}{/if}
</span>{/each}
								</pre>
							{:else}
								<pre>
{#each lineSlice.lines as line, i (i)}
<span class={classifyLine(line)}>{#if useEnhancedRender}{@html renderUnifiedLineHtml(line, lang, unifiedPartners ? unifiedPartners[i] : null)}{:else}{line}{/if}
</span>{/each}
								</pre>
							{/if}
							{#if lineSlice.truncated && !binary}
								<div class="diff-strip__truncated">
									<span class="diff-strip__truncated-note">
										Showing first {truncateLinesThreshold} of {lineSlice.totalLines} lines
									</span>
									<button
										type="button"
										class="diff-strip__btn"
										on:click={() => expandFullDiff(file)}
									>Show full diff</button>
								</div>
							{/if}
							<div class="diff-strip__actions">
								<button
									type="button"
									class="diff-strip__btn"
									on:click={() => toggle(file.path)}
								>Hide</button>
								{#if allowCopy}
									<button
										type="button"
										class="diff-strip__btn"
										on:click={() => void copyDiff(file)}
									>{recentlyCopied.has(file.path) ? 'Copied!' : 'Copy diff'}</button>
								{/if}
								{#if showPerFileApproval}
									<button
										type="button"
										class="diff-strip__btn diff-strip__btn--secondary"
										on:click={() => rejectOneFile(file)}
									>Reject</button>
									<button
										type="button"
										class="diff-strip__btn diff-strip__btn--primary"
										on:click={() => approveFile(file)}
									>Apply</button>
								{/if}
								{#if showRevertButton}
									<button
										type="button"
										class="diff-strip__btn diff-strip__btn--danger"
										disabled={isReverting.has(file.path)}
										on:click={() => void revertFile(file)}
									>{revertLabel}</button>
								{/if}
							</div>
						</div>
					{/if}
				</li>
			{/each}
		</ul>

		{#if lifecycle === 'pending' && allowApprove}
			<div class="diff-strip__footer-actions">
				<slot name="actions">
					<button
						type="button"
						class="diff-strip__btn diff-strip__btn--secondary"
						on:click={rejectAll}
					>Reject</button>
					<button
						type="button"
						class="diff-strip__btn diff-strip__btn--primary"
						on:click={approveAll}
					>Apply</button>
				</slot>
			</div>
		{:else if lifecycle === 'reverted'}
			<p class="diff-strip__reverted">Reverted.</p>
		{/if}
	{/if}
</section>

<style>
	.diff-strip {
		display: flex;
		flex-direction: column;
		gap: 4px;
		padding: 8px 0 12px;
		border-top: 1px solid var(--border-soft, rgba(0, 0, 0, 0.08));
		/* Diff palette — anchored to magician's semantic color vars so
		   added/removed lines glow the same green/red as success/error
		   chips elsewhere in the UI. The soft/word tints are
		   color-mixed from the same source so the whole palette shifts
		   with the theme; a per-theme override can replace any of
		   these vars individually. */
		--diff-add-fg: var(--color-success, #1f7a3e);
		--diff-del-fg: var(--color-error, #c0392b);
		--diff-add-bg: color-mix(in srgb, var(--color-success, #1f7a3e) 10%, transparent);
		--diff-del-bg: color-mix(in srgb, var(--color-error, #c0392b) 10%, transparent);
		--diff-add-bg-hover: color-mix(in srgb, var(--color-success, #1f7a3e) 22%, transparent);
		--diff-del-bg-hover: color-mix(in srgb, var(--color-error, #c0392b) 22%, transparent);
		--diff-add-word: color-mix(in srgb, var(--color-success, #1f7a3e) 36%, transparent);
		--diff-del-word: color-mix(in srgb, var(--color-error, #c0392b) 36%, transparent);
		--diff-add-text: var(--text-primary, #14532d);
		--diff-del-text: var(--text-primary, #7f1d1d);
		--diff-hunk-bg: var(--bg-soft, rgba(0, 0, 0, 0.04));
		--diff-hunk-fg: var(--text-muted, #6b7280);
		--diff-line-hover: var(--bg-soft, rgba(0, 0, 0, 0.06));
		--diff-empty-stripe: color-mix(in srgb, var(--text-muted, #888) 8%, transparent);
	}

	.diff-strip--compact {
		gap: 2px;
		padding: 4px 0 6px;
	}

	.diff-strip__head {
		display: flex;
		align-items: baseline;
		gap: 8px;
		padding: 4px 4px 6px;
	}

	.diff-strip__title {
		font-family: var(--font-primary);
		font-size: 11px;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.06em;
		color: var(--text-primary, #1a1a1a);
	}

	.diff-strip__count {
		font-family: var(--font-mono);
		font-size: 10.5px;
		color: var(--text-muted, #888);
	}

	.diff-strip__stats {
		display: inline-flex;
		gap: 6px;
		margin-left: 4px;
		font-family: var(--font-mono);
		font-size: 10.5px;
	}

	.diff-strip__truncated {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 8px;
		padding: 6px 8px;
		border-top: 1px dashed var(--border-soft, rgba(0, 0, 0, 0.12));
		background: var(--bg-soft, rgba(0, 0, 0, 0.03));
	}

	.diff-strip__truncated-note {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-muted, #888);
	}

	.diff-strip__binary {
		margin: 0;
		padding: 16px 12px;
		font-family: var(--font-primary);
		font-size: 12px;
		color: var(--text-muted, #888);
		text-align: center;
		font-style: italic;
	}

	.diff-strip__gutter {
		display: inline-block;
		min-width: 3.5ch;
		padding: 0 6px 0 0;
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-muted, #aaa);
		text-align: right;
		user-select: none;
		/* Subtle vertical separator between the two gutters and the
		   diff body so the eye can scan line numbers without the
		   diff text blending in. */
		border-right: 1px solid var(--border-soft, rgba(0, 0, 0, 0.06));
	}

	.diff-strip__gutter + .diff-strip__gutter {
		margin-left: 0;
		padding-left: 6px;
	}

	.diff-strip__gutter + .diff-strip__gutter + span {
		padding-left: 8px;
	}

	.diff-strip__header-action {
		margin-left: auto;
		padding: 2px 8px;
		font-family: var(--font-primary);
		font-size: 10.5px;
		color: var(--text-muted, #888);
		background: transparent;
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.12));
		border-radius: 4px;
		cursor: pointer;
	}

	.diff-strip__header-action:hover {
		background: var(--bg-soft, rgba(0, 0, 0, 0.05));
		color: var(--text-primary, #1a1a1a);
	}

	/* --- wrapLongLines: wrap instead of horizontal-scroll. Keeps the
	   diff body's max-height + vertical scroll; just changes the
	   horizontal overflow strategy. Affects pre elements inside .diff. */
	.diff-strip.diff-strip--wrap .diff-strip__diff pre {
		white-space: pre-wrap;
		word-break: break-all;
		overflow-wrap: anywhere;
	}

	/* --- stickyFileHeaders: each row's button sticks to the top of the
	   scroll container while its expanded body scrolls underneath. The
	   `z-index` keeps it above the diff body, and the solid background
	   prevents the diff text from bleeding through. */
	.diff-strip.diff-strip--sticky-headers .diff-strip__row {
		position: sticky;
		top: 0;
		z-index: 2;
		background: var(--bg-card, #ffffff);
		border-bottom: 1px solid var(--border-soft, rgba(0, 0, 0, 0.04));
	}

	/* --- hoverLineHighlight: subtle row highlight on hover inside the
	   diff body. We target spans inside .diff-strip__diff pre via a
	   `:global()` hook because each line is rendered as a span. The
	   `:hover` color is intentionally muted so it doesn't fight the
	   green/red add/del backgrounds. */
	/* Scope hover to diff-line spans only (not gutter / syntax /
	   word-mark spans). Pre-fix, the bare `:global(span):hover`
	   matched everything inside `<pre>` including the line-number
	   gutters, so hovering the line numbers fired the highlight too.
	   The five rules below cover every line-class produced by
	   `classifyLine()`; gutter spans (`.diff-strip__gutter`) do NOT
	   carry any `diff-line--*` class so they're naturally excluded. */
	.diff-strip.diff-strip--hover-lines .diff-strip__diff pre :global(.diff-line--ctx):hover,
	.diff-strip.diff-strip--hover-lines .diff-strip__diff pre :global(.diff-line--hunk):hover,
	.diff-strip.diff-strip--hover-lines .diff-strip__diff pre :global(.diff-line--meta):hover {
		background: var(--diff-line-hover);
	}
	.diff-strip.diff-strip--hover-lines .diff-strip__diff pre :global(.diff-line--add):hover {
		background: var(--diff-add-bg-hover);
	}
	.diff-strip.diff-strip--hover-lines .diff-strip__diff pre :global(.diff-line--del):hover {
		background: var(--diff-del-bg-hover);
	}
	/* Same for the split-view cells. */
	.diff-strip.diff-strip--hover-lines .diff-strip__split-cell.diff-line--add:hover {
		background: var(--diff-add-bg-hover);
	}
	.diff-strip.diff-strip--hover-lines .diff-strip__split-cell.diff-line--del:hover {
		background: var(--diff-del-bg-hover);
	}
	.diff-strip.diff-strip--hover-lines .diff-strip__split-cell.diff-line--context:hover {
		background: var(--diff-line-hover);
	}

	/* --- allowResize: native CSS vertical resize handle. The 360px
	   max-height clamp on the default pre is replaced with an explicit
	   `height` so resize has range; min-height prevents collapse. */
	.diff-strip.diff-strip--resizable .diff-strip__diff pre {
		resize: vertical;
		max-height: none;
		height: 360px;
		min-height: 80px;
	}
	.diff-strip.diff-strip--resizable .diff-strip__split-grid {
		resize: vertical;
		max-height: none;
		height: 360px;
		min-height: 80px;
		overflow: auto;
	}

	/* --- enableKeyboardNav: active-row indicator (visible only when
	   the section actually has focus, to avoid distracting the user
	   when the component isn't keyboard-driven). */
	.diff-strip.diff-strip--keyboard:focus {
		outline: 2px solid var(--accent-primary, #c2502a);
		outline-offset: -2px;
		border-radius: 4px;
	}
	.diff-strip.diff-strip--keyboard:focus
		.diff-strip__entry--active
		.diff-strip__row {
		background: var(--accent-primary-soft, color-mix(in srgb, var(--accent-primary, #c2502a) 12%, transparent));
		box-shadow: inset 2px 0 0 var(--accent-primary, #c2502a);
	}

	/* --- showSimilarity: small percentage badge after the status. */
	.diff-strip__similarity {
		display: inline-block;
		padding: 0 4px;
		margin-left: -2px;
		font-family: var(--font-mono);
		font-size: 9.5px;
		font-weight: 600;
		color: var(--text-muted, #888);
		background: var(--bg-soft, rgba(0, 0, 0, 0.05));
		border-radius: 3px;
	}

	/* --- view="split": side-by-side grid. Four-column grid when
	   showLineNumbers is on (old-num | old-text | new-num | new-text);
	   two-column grid otherwise (old-text | new-text). Meta and hunk
	   rows span all columns via grid-column: 1 / -1. */
	.diff-strip__split-grid {
		display: grid;
		grid-template-columns: 1fr 1fr;
		gap: 0;
		margin: 0;
		padding: 0;
		font-family: var(--font-mono);
		font-size: 11.5px;
		line-height: 1.45;
		max-height: 360px;
		overflow: auto;
		/* Match the unified `.diff-strip__diff` body bg so split + unified
		   views read as the same surface against the drawer. */
		background: var(--bg-card, #fff);
	}
	/* Four-column grid (old-num | old-text | new-num | new-text)
	   triggered by the presence of gutter spans in the template. */
	.diff-strip__split-grid:has(.diff-strip__split-gutter--old) {
		grid-template-columns: auto 1fr auto 1fr;
	}
	.diff-strip__split-cell {
		padding: 0 8px;
		white-space: pre;
		overflow: hidden;
		text-overflow: ellipsis;
		min-width: 0;
	}
	.diff-strip.diff-strip--wrap .diff-strip__split-cell {
		white-space: pre-wrap;
		word-break: break-all;
		overflow-wrap: anywhere;
	}
	.diff-strip__split-meta {
		grid-column: 1 / -1;
		padding: 2px 8px;
		font-family: var(--font-mono);
		font-size: 11.5px;
		line-height: 1.45;
	}
	.diff-strip__split-gutter--old,
	.diff-strip__split-gutter--new {
		border-right: 1px solid var(--border-soft, rgba(0, 0, 0, 0.06));
		text-align: right;
	}
	/* Subtle vertical separator between old/new in split view so the
	   eye doesn't merge the two sides. */
	.diff-strip__split-grid > :nth-child(2n + 1) {
		border-right: 1px solid var(--border-soft, rgba(0, 0, 0, 0.06));
	}
	.diff-strip__split-grid:has(.diff-strip__split-gutter--old)
		> :nth-child(2n + 1) {
		border-right: none;
	}
	.diff-strip__split-grid:has(.diff-strip__split-gutter--old)
		> :nth-child(4n + 2) {
		border-right: 1px solid var(--border-soft, rgba(0, 0, 0, 0.12));
	}
	/* Empty cell styling — give it a faint stripe so it reads as
	   "intentionally blank" rather than "the diff broke". */
	.diff-strip__split-cell.diff-line--empty {
		background: repeating-linear-gradient(
			45deg,
			var(--diff-empty-stripe),
			var(--diff-empty-stripe) 4px,
			transparent 4px,
			transparent 8px
		);
	}

	/* --- syntaxHighlight: Shiki emits `<span style="color: #...">`
	   inline spans. When we layer those on top of the diff-line color
	   classes (`.diff-line--add` / `.diff-line--del`), the background
	   tint stays + the foreground colors come from shiki. The cells
	   are intentionally NOT given an opaque background here; the diff
	   line tint shows through. */
	.diff-strip__diff--syntax pre {
		color: var(--text-primary, #1a1a1a);
	}

	/* Shiki inserts spans with `style="color: #..."` for each token —
	   those colors come straight through; no extra rules needed.
	   Documented here for future maintainers so nobody tries to
	   "fix" them by overriding the inline styles. */

	/* --- wordLevelDiff: word marks layered ON TOP of the line-level
	   add/del background. Stronger fill + slight outline so the
	   changed words pop without being garish. Inherits text color
	   from the surrounding line (or from shiki spans when both are on). */
	:global(mark.diff-word--add) {
		background: var(--diff-add-word, color-mix(in srgb, var(--color-success, #1f7a3e) 36%, transparent));
		color: inherit;
		border-radius: 2px;
		padding: 0;
		box-decoration-break: clone;
	}

	:global(mark.diff-word--del) {
		background: var(--diff-del-word, color-mix(in srgb, var(--color-error, #c0392b) 36%, transparent));
		color: inherit;
		border-radius: 2px;
		padding: 0;
		box-decoration-break: clone;
		text-decoration: line-through;
		text-decoration-color: color-mix(in srgb, var(--color-error, #c0392b) 55%, transparent);
	}

	.diff-strip__error,
	.diff-strip__note,
	.diff-strip__empty,
	.diff-strip__reverted {
		margin: 4px 4px;
		font-size: 12px;
		color: var(--text-muted, #888);
	}

	.diff-strip__error {
		color: var(--color-error, #c0392b);
	}

	.diff-strip__reverted {
		font-style: italic;
	}

	.diff-strip__list {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 2px;
	}

	.diff-strip__row {
		display: flex;
		align-items: baseline;
		gap: 8px;
		width: 100%;
		padding: 6px 8px;
		background: transparent;
		border: 0;
		border-radius: 4px;
		cursor: pointer;
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--text-primary, #1a1a1a);
		text-align: left;
	}

	.diff-strip__row:hover {
		background: var(--bg-soft, rgba(0, 0, 0, 0.05));
	}

	.diff-strip__status {
		display: inline-block;
		width: 14px;
		text-align: center;
		font-weight: 700;
		color: var(--text-muted, #888);
	}

	.diff-strip__path {
		flex: 1 1 auto;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.diff-strip__counts {
		display: inline-flex;
		gap: 6px;
		font-size: 11px;
	}

	.diff-strip__add {
		color: var(--diff-add-fg);
	}

	.diff-strip__del {
		color: var(--diff-del-fg);
	}

	.diff-strip__diff {
		margin: 4px 8px 8px;
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.1));
		border-radius: 4px;
		/* `--bg-card` instead of `--bg-elevated` so the diff body sits
		   visually distinct from the surrounding drawer surface
		   (which also uses `--bg-elevated`). Without this the diff
		   block had no contrast against the drawer and read as
		   "unthemed slab of color". */
		background: var(--bg-card, #fff);
	}

	.diff-strip__diff pre {
		margin: 0;
		padding: 6px 8px;
		font-family: var(--font-mono);
		font-size: 11.5px;
		line-height: 1.45;
		max-height: 360px;
		overflow: auto;
		white-space: pre;
	}

	.diff-strip__diff :global(.diff-line--add) {
		background: var(--diff-add-bg);
		color: var(--diff-add-text);
	}

	.diff-strip__diff :global(.diff-line--del) {
		background: var(--diff-del-bg);
		color: var(--diff-del-text);
	}

	.diff-strip__diff :global(.diff-line--hunk) {
		color: var(--diff-hunk-fg);
		background: var(--diff-hunk-bg);
	}

	.diff-strip__diff :global(.diff-line--meta) {
		color: var(--text-muted, #888);
	}

	.diff-strip__diff :global(.diff-line--ctx) {
		color: var(--text-primary, #1a1a1a);
	}

	.diff-strip__actions,
	.diff-strip__footer-actions {
		display: flex;
		justify-content: flex-end;
		gap: 6px;
		padding: 6px 8px;
		border-top: 1px solid var(--border-soft, rgba(0, 0, 0, 0.08));
	}

	.diff-strip__footer-actions {
		margin-top: 6px;
		padding: 8px 4px 0;
	}

	.diff-strip__btn {
		font-family: var(--font-primary);
		font-size: 11px;
		padding: 3px 10px;
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.12));
		background: transparent;
		color: var(--text-primary, #1a1a1a);
		border-radius: 4px;
		cursor: pointer;
	}

	.diff-strip__btn:hover:not(:disabled) {
		background: var(--bg-soft, rgba(0, 0, 0, 0.05));
	}

	.diff-strip__btn--danger:hover:not(:disabled) {
		background: var(--color-error-soft, color-mix(in srgb, var(--color-error, #c0392b) 12%, transparent));
		color: var(--color-error, #c0392b);
	}

	.diff-strip__btn--primary {
		background: var(--accent-primary, #c2502a);
		color: var(--accent-contrast, var(--text-on-accent, #fff));
		border-color: var(--accent-primary, #c2502a);
	}

	.diff-strip__btn--primary:hover:not(:disabled) {
		background: var(--accent-primary-hover, #a8431f);
	}

	.diff-strip__btn--secondary {
		color: var(--text-muted, #888);
	}

	.diff-strip__btn:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}
</style>
