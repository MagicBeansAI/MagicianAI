<script lang="ts">
	/**
	 * Per-request activity card. ONE card per user message, anchored
	 * under the user message that started the request. Never unmounts
	 * during the session. Reads from `chatTurnEventsStore`, which
	 * sources both REST refresh and live SSE from the per-chat-turn
	 * endpoint (`/api/magician/v2/chat/sessions/{sid}/turns/{cid}/events`
	 * and `/events/stream`) — both paths consume the same per-turn
	 * projection that `ChatTurnEventSink` writes, so refresh-view ==
	 * live-view by construction.
	 *
	 * Display:
	 *   - Header pip: "Activity · N events" with running spinner while
	 *     anything is in-flight.
	 *   - Inline body: tail of the 5 most recent activity rows, each a
	 *     single line with ellipsis. Mostly-stable order; rows update
	 *     in place (a long-running tool call patches its row, doesn't
	 *     scroll past).
	 *   - "Show all" button opens a full-screen scrollable modal with
	 *     every row, full text, no truncation, grouped by agent.
	 */
	import { onDestroy, onMount, createEventDispatcher } from 'svelte';
	import { get } from 'svelte/store';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import {
		chatTurnEventsStore,
		subscribeToChatTurn,
		fetchEventsPage,
		type RawTurnEvent,
	} from '$lib/stores/chatTurnEventsStore';
	import { timedFetch } from '$lib/shared/fetch';
	import { totalUsage, type TokenUsage } from '$lib/llm/tokenUsage';
	import {
		coordinateExecutionControl,
		executionControlBusy
	} from '$lib/magician/execution/controlClient';
	import {
		attachPathActions,
		type PathActionHandlers,
		type PathSize,
	} from '$lib/magician/links/pathActions';
	import { deriveTaskBackedInspectTarget } from './requestActivityInspection';
	import { tutorOverlayTransitionForEvent } from './tutorOverlayLifecycle';
	import {
		parseCompleteResultOwner,
		reconstructCompleteResult,
		resolveCompleteResultReadTarget,
		type CompleteResultEntry,
		type CompleteResultOwner,
	} from './completeResult';

	export let sessionId: string = '';
	export let chatTurnId: string;
	/**
	 * True when this is the currently-in-flight request — the card
	 * opens an SSE subscription for live updates. False (default) for
	 * historical turns — REST page fetch only, no live tail, bounded
	 * memory + no idle SSE traffic per old turn. The chat page passes
	 * `live={true}` for the user message that's still processing.
	 */
	export let live: boolean = false;
	/**
	 * `embedded` flips the card from a free-standing block (its own
	 * border + background) to an in-bubble strip — separator on top,
	 * no outer border. Used when the card sits *inside* the assistant
	 * `chat-bubble` (the new "still working / finished working" affordance):
	 * the bubble already owns the surface; the card is just a status
	 * coda inside it.
	 */
	export let embedded: boolean = false;
	/** Canonical task/root target supplied by task-status surfaces. Event-derived
	 * targets remain the fallback only when the activity includes a durable task. */
	export let controlExecutionId: string | null = null;
	export let controlTaskId: string | null = null;
	export let showExecutionStop = true;

	// Lets the host (ChatPanel) open the deep-work panel for the run this
	// card is showing — surfaced as an "Inspect run →" affordance in the
	// header, so the panel is reachable directly from the activity strip and
	// doesn't depend on a separate standalone task-status card existing.
	const dispatch = createEventDispatcher<{
		inspect: { taskId: string; executionId: string };
	}>();

	type EventKind = 'llm' | 'reasoning' | 'tool' | 'lifecycle' | 'file' | 'other';
	type EventStatus = 'running' | 'done' | 'failed' | 'waiting' | 'idle';

	interface FileRef {
		absolutePath: string;
		label: string;
	}

	interface Leaf {
		key: string;
		kind: EventKind;
		agentId: string;
		label: string;
		fullText: string | null;
		detail: string | null;
		status: EventStatus;
		startedAt: number;
		durationMs: number | null;
		files: FileRef[];
		resultRef?: string | null;
		resultHash?: string | null;
		resultSizeBytes?: number | null;
		resultOwner?: CompleteResultOwner | null;
		resultTaskId?: string | null;
		resultExecutionId?: string | null;
	}
	const terminalTutorRunIds = new Set<string>();

	// ─── Shared file-action handlers ───────────────────────────────────
	// Routes through the chat-session open-file / open-folder endpoints,
	// matching how `ChatMarkdown` invokes the same affordances. Without a
	// sessionId we degrade to no-op (the card still renders, the buttons
	// just don't do anything — same as no chat scope).
	function buildScopeQuery(): string {
		return '';
	}
	async function openPath(absolutePath: string, action: 'file' | 'folder'): Promise<void> {
		if (!sessionId) return;
		const endpoint =
			`/api/magician/v2/chat/sessions/${encodeURIComponent(sessionId)}` +
			`/outputs/open-${action}${buildScopeQuery()}`;
		try {
			await timedFetch(endpoint, {
				method: 'POST',
				headers: { 'Content-Type': 'application/json' },
				body: JSON.stringify({ absolute_path: absolutePath }),
			});
		} catch {
			// Best-effort — backend rejection / network error is silent.
			// The user sees no visible change and can click again.
		}
	}
	const pathHandlers: PathActionHandlers = {
		onOpenFile: (absolutePath) => openPath(absolutePath, 'file'),
		onRevealFolder: (absolutePath) => openPath(absolutePath, 'folder'),
	};

	// Cancel/Stop a still-running execution directly from the activity card.
	// Reconstructed from REST-loaded events, so the Stop survives a page refresh
	// (the earlier bug: a refresh lost the only in-memory cancel path). The
	// backend cancel is best-effort + tolerant of an already-gone execution.
	let cancelling = false;
	let cancelled = false;
	async function cancelRun(): Promise<void> {
		const executionId = inspectTarget?.executionId;
		if (!executionId || cancelling || $executionControlBusy.has(executionId)) return;
		cancelling = true;
		try {
			const res = await coordinateExecutionControl(executionId, () =>
				timedFetch(
					`/api/magician/v2/executions/${encodeURIComponent(executionId)}/cancel${buildScopeQuery()}`,
					{ method: 'POST', headers: { 'Content-Type': 'application/json' } }
				)
			);
			if (res.ok) cancelled = true;
		} catch {
			// Best-effort — the user can click again.
		} finally {
			cancelling = false;
		}
	}

	// Svelte action: attach `pathActions` open/reveal icons next to an
	// anchor element after the DOM lands. Keeps the imperative DOM
	// helper API intact (used by ChatMarkdown.svelte too) so we get the
	// exact same visual language and click semantics.
	function pathActionAttach(node: HTMLElement, params: { path: string; size: PathSize }) {
		const attach = (path: string, size: PathSize) => {
			// Clear any previous attachment from a re-mount.
			const sibling = node.nextElementSibling;
			if (sibling instanceof HTMLElement && sibling.dataset.pathActions === '1') {
				sibling.remove();
			}
			attachPathActions(node, path, size, pathHandlers);
		};
		attach(params.path, params.size);
		return {
			update(next: { path: string; size: PathSize }) {
				attach(next.path, next.size);
			},
			destroy() {
				const sibling = node.nextElementSibling;
				if (sibling instanceof HTMLElement && sibling.dataset.pathActions === '1') {
					sibling.remove();
				}
			},
		};
	}

	const MAX_INLINE_ROWS = 5;
	const LABEL_MAX_CHARS = 110;

	let modalOpen = false;
	let resultViewerOpen = false;
	let resultViewerLoading = false;
	let resultViewerError: string | null = null;
	let resultViewerTitle = 'Complete tool result';
	let resultViewerText = '';
	let resultViewerHash: string | null = null;

	async function openFullResult(leaf: Leaf): Promise<void> {
		if (!sessionId || !leaf.resultRef || resultViewerLoading) return;
		resultViewerOpen = true;
		resultViewerLoading = true;
		resultViewerError = null;
		resultViewerText = '';
		resultViewerHash = leaf.resultHash ?? null;
		resultViewerTitle = `${leaf.label} · complete result`;
		try {
			const readTarget = resolveCompleteResultReadTarget({
				owner: leaf.resultOwner,
				hostSessionId: sessionId,
				legacyTaskId: leaf.resultTaskId,
				legacyExecutionId: leaf.resultExecutionId,
			});
			if (!readTarget) {
				throw new Error('This complete result is no longer attached to a readable chat or task owner.');
			}
			let cursor: string | null = null;
			const entries: CompleteResultEntry[] = [];
			let reconstructionVersion: number | null = null;
			let expectedPageStart = 0;
			let expectedTotalEntries: number | null = null;
			let expectedContentHash: string | null = leaf.resultHash ?? null;
			let expectedSelectionPaths: string[] | null = null;
			for (let pageIndex = 0; pageIndex < 256; pageIndex += 1) {
				const response = await timedFetch(
					`${readTarget.path}${buildScopeQuery()}`,
					{
						method: 'POST',
						headers: { 'Content-Type': 'application/json' },
						body: JSON.stringify({
							result_ref: leaf.resultRef,
							cursor,
							field_paths: [],
							max_records: 100,
							execution_id: readTarget.executionId ?? undefined,
						}),
					}
				);
				if (!response.ok) {
					const body = await response.json().catch(() => ({}));
					throw new Error(String((body as { error?: unknown }).error ?? `HTTP ${response.status}`));
				}
				const body = (await response.json()) as {
					page?: {
						content_ref?: string;
						content_hash?: string;
						reconstruction_version?: number;
						selection_paths?: string[];
						entries?: CompleteResultEntry[];
						page_start?: number;
						total_records?: number;
						total_entries?: number;
						next_cursor?: string;
					};
				};
				if (!body.page) throw new Error('Complete result response was missing its page.');
				if (body.page.content_ref != null && body.page.content_ref !== leaf.resultRef) {
					throw new Error('Complete result response changed result identity between pages.');
				}
				if (!body.page.content_hash) {
					throw new Error('Complete result response was missing its verified content hash.');
				}
				if (expectedContentHash != null && expectedContentHash !== body.page.content_hash) {
					throw new Error('Complete result content hash changed between pages.');
				}
				expectedContentHash = body.page.content_hash;
				resultViewerHash = body.page.content_hash;
				const pageVersion = body.page.reconstruction_version ?? 0;
				if (reconstructionVersion != null && reconstructionVersion !== pageVersion) {
					throw new Error('Complete result changed reconstruction version between pages.');
				}
				reconstructionVersion = pageVersion;
				const pageEntries = body.page.entries ?? [];
				if (body.page.page_start !== expectedPageStart) {
					throw new Error('Complete result pages were missing, duplicated, or out of order.');
				}
				const pageTotal = body.page.total_entries ?? body.page.total_records;
				if (typeof pageTotal !== 'number' || !Number.isSafeInteger(pageTotal) || pageTotal < 0) {
					throw new Error('Complete result response contained an invalid entry count.');
				}
				if (expectedTotalEntries != null && expectedTotalEntries !== pageTotal) {
					throw new Error('Complete result entry count changed between pages.');
				}
				expectedTotalEntries = pageTotal;
				const selectionPaths = body.page.selection_paths ?? [''];
				if (
					expectedSelectionPaths != null &&
					JSON.stringify(expectedSelectionPaths) !== JSON.stringify(selectionPaths)
				) {
					throw new Error('Complete result field selection changed between pages.');
				}
				expectedSelectionPaths = selectionPaths;
				if (cursor && pageEntries.length === 0) {
					throw new Error('Complete result cursor made no forward progress.');
				}
				entries.push(...pageEntries);
				expectedPageStart += pageEntries.length;
				cursor = body.page.next_cursor ?? null;
				if (!cursor) {
					if (expectedTotalEntries !== entries.length) {
						throw new Error('Complete result ended before every entry was returned.');
					}
					break;
				}
				if (pageIndex === 255) throw new Error('Complete result exceeded the safe page limit.');
			}
			const value = reconstructCompleteResult(entries, reconstructionVersion ?? 0);
			resultViewerText = JSON.stringify(value, null, 2);
		} catch (error) {
			resultViewerError = error instanceof Error ? error.message : String(error);
		} finally {
			resultViewerLoading = false;
		}
	}
	/** Modal layout mode. Default flat = chronological linear list;
	 *  tree = group rows by agent_id under collapsible sections. Inline
	 *  card always uses flat — the tree toggle is modal-only now. */
	let view: 'flat' | 'tree' = 'flat';
	let unsubscribe: (() => void) | null = null;
	let subscribedKey: string | null = null;
	let initialFetchedKey: string | null = null;
	/**
	 * Inline expand/collapse. Default = `live` so an in-flight turn
	 * pops open immediately (the user wants to watch it work), and a
	 * refreshed/historical turn collapses to a one-line header (the
	 * activity is supporting context, not the headline). Click the
	 * header to override; once toggled, we stop following `live` so
	 * user intent wins for the rest of the card's lifetime.
	 */
	let expanded = live;
	let userToggledExpand = false;
	function toggleExpanded() {
		userToggledExpand = true;
		expanded = !expanded;
	}
	// Follow `live` transitions (in-flight → finished) only if the user
	// hasn't manually overridden — once they click, we respect that.
	$: if (!userToggledExpand) expanded = live;

	// Reactive scope — `$scopeIdentityStore` subscribes properly. The
	// previous `get(scopeIdentityStore)` was a one-shot read with no
	// dependency tracking; if scope was empty when the card mounted,
	// the subscribe guard silently bailed and the card never received
	// events.
	$: scope = $scopeIdentityStore;

	// Subscribe (live mode) when both scope+turnId are ready AND this
	// card is the in-flight one. Historical cards skip the SSE
	// entirely — they fetch a REST page once on mount and rely on the
	// modal's "Load more" for paging older. Composite key dedupes so
	// we don't tear-down/re-open on every reactive bump.
	$: if (live) {
		maybeSubscribe(scope?.principal, scope?.workspace, chatTurnId);
	} else {
		// Card flipped from live → historical (e.g. parent re-rendered
		// after a new turn replaced this one as the in-flight). Drop
		// the SSE subscription; events already in the store stay.
		unsubscribe?.();
		unsubscribe = null;
		subscribedKey = null;
	}

	// Historical cards: one-shot REST fetch on first valid scope+turnId.
	// Caches per turnId so re-mounts don't re-fetch.
	$: maybeFetchInitialPage(scope?.principal, scope?.workspace, chatTurnId);

	function maybeSubscribe(
		principal: string | undefined | null,
		workspace: string | undefined | null,
		turnId: string
	) {
		if (!principal || !workspace || !turnId || !sessionId) return;
		const key = `${principal}::${workspace}::${sessionId}::${turnId}`;
		if (key === subscribedKey) return;
		unsubscribe?.();
		subscribedKey = key;
		unsubscribe = subscribeToChatTurn(
			{ principal, workspace },
			sessionId,
			turnId
		);
	}

	async function maybeFetchInitialPage(
		principal: string | undefined | null,
		workspace: string | undefined | null,
		turnId: string
	) {
		if (!principal || !workspace || !turnId || !sessionId) return;
		// Key includes sessionId so the fetch retries if the parent
		// re-keys with a new session for the same turn id (rare but
		// possible across aggregate views). Mirrors `maybeSubscribe`.
		const key = `${principal}::${workspace}::${sessionId}::${turnId}`;
		if (key === initialFetchedKey) return;
		initialFetchedKey = key;
		// Per-chat-turn endpoint returns the full event log in one
		// response — no pagination cursor. Refresh and the live SSE
		// tail both consume the same projection from the sink, so the
		// initial fetch is the only REST call this card ever makes.
		await fetchEventsPage({ principal, workspace }, sessionId, turnId);
	}
	// Raw events for this request — owned by `chatTurnEventsStore`, not
	// by this component, so card unmount/remount (e.g. when the
	// optimistic user msg is replaced by the authoritative server msg
	// and Svelte's keyed `#each` re-keys it) loses nothing. We're a
	// pure renderer reacting to the store; the store owns the SSE.
	$: rawEvents = $chatTurnEventsStore.get(chatTurnId) ?? [];
	$: leaves = deriveLeaves(rawEvents);
	/**
	 * Prompt-cache summary for the turn. Sums prompt and cache-read
	 * tokens across every `llm.succeeded` event the turn produced and
	 * returns null when the turn either has no LLM calls or none of the
	 * calls reported usage. The chip in the header reads this so an
	 * operator can see at a glance whether prefix caching is landing —
	 * a long warm chat should show 80%+ hit rates, while a cold first
	 * turn after a system-prompt edit drops to 0% as the new prefix
	 * gets written but not yet read.
	 */
	$: cacheSummary = deriveCacheSummary(rawEvents);
	// Deep-work panel target = the durable delegated task execution this card is
	// showing. Inline chat/Tutor/Thinking Map calls stamp their chat-turn id as an
	// execution_id for telemetry, but have no task-backed execution panel; both
	// ids are therefore required before Inspect/Stop is offered.
	$: inspectTarget = controlExecutionId?.trim() && controlTaskId?.trim()
		? { taskId: controlTaskId.trim(), executionId: controlExecutionId.trim() }
		: deriveInspectTarget(rawEvents);
	/**
	 * Header count = the number of rendered rows (leaves), NOT the raw
	 * event count. A "hi" turn produces ~7 raw events but renders as 2
	 * rows (one LLM call + one reasoning), so a raw-count badge ("7
	 * events") felt misleading next to "2 visible rows" — the user
	 * couldn't account for the missing 5. Tying the badge to `leaves`
	 * makes the number self-consistent with what's on screen.
	 */
	$: totalEvents = leaves.length;
	$: inlineLeaves = leaves.slice(-MAX_INLINE_ROWS);
	$: hiddenCount = Math.max(0, leaves.length - inlineLeaves.length);
	/** Tree-view inline summary: one synthetic row per agent showing
	 *  count + status of the most recent leaf for that agent. Capped at
	 *  MAX_INLINE_ROWS so it never blows the card height; agents beyond
	 *  the cap fold into a "+N more agents" row. */
	$: inlineAgentSummary = buildAgentSummary(leaves);
	$: anyRunning = leaves.some((l) => l.status === 'running');
	/** "Complete" = the request has emitted at least one event, nothing
	 *  is currently running, and the card is no longer subscribed live.
	 *  Used to flip the embedded card's accent border from "still
	 *  working" (info) to "done" (success). For the floating (non-embedded)
	 *  card we keep the neutral teal accent unchanged — the user only asked
	 *  for the "done" cue inside the assistant bubble. */
	$: isComplete = !live && !anyRunning && leaves.length > 0;
	/** Anchor for per-row elapsed timestamps. First observed event's
	 *  startedAt; we don't track request-send time on the client so this
	 *  is the closest correlate. Falls back to `Date.now()` until the
	 *  first event lands. */
	$: requestStart = leaves[0]?.startedAt ?? Date.now();
	/** Pre-grouped tree projection for the tree-view modal. Group order
	 *  follows the first appearance of each agent_id. */
	$: agentTree = buildAgentTree(leaves);

	interface AgentGroup {
		agentId: string;
		leaves: Leaf[];
	}

	function buildAgentTree(allLeaves: Leaf[]): AgentGroup[] {
		const order: string[] = [];
		const byAgent = new Map<string, Leaf[]>();
		for (const leaf of allLeaves) {
			if (!byAgent.has(leaf.agentId)) {
				order.push(leaf.agentId);
				byAgent.set(leaf.agentId, []);
			}
			byAgent.get(leaf.agentId)!.push(leaf);
		}
		return order.map((agentId) => ({ agentId, leaves: byAgent.get(agentId)! }));
	}

	interface AgentSummaryRow {
		agentId: string;
		count: number;
		latest: Leaf;
		anyRunning: boolean;
		anyFailed: boolean;
	}

	/** Collapse leaves into one row per agent for the inline tree view.
	 *  Latest agent ordering (most-recently-active first) so the card's
	 *  most relevant agent is at the top. */
	function buildAgentSummary(allLeaves: Leaf[]): AgentSummaryRow[] {
		const groups = buildAgentTree(allLeaves);
		const rows: AgentSummaryRow[] = groups.map((g) => {
			const latest = g.leaves[g.leaves.length - 1];
			return {
				agentId: g.agentId,
				count: g.leaves.length,
				latest,
				anyRunning: g.leaves.some((l) => l.status === 'running'),
				anyFailed: g.leaves.some((l) => l.status === 'failed'),
			};
		});
		// Sort by latest activity desc (most-recent agent at top), then
		// cap at MAX_INLINE_ROWS. The card body intentionally stays
		// short — the modal carries the full surface.
		rows.sort((a, b) => b.latest.startedAt - a.latest.startedAt);
		return rows.slice(0, MAX_INLINE_ROWS);
	}

	function formatElapsed(ms: number): string {
		if (!Number.isFinite(ms) || ms < 0) return '+0.0s';
		if (ms < 1000) return `+${ms}ms`;
		const seconds = ms / 1000;
		if (seconds < 60) return `+${seconds.toFixed(1)}s`;
		const minutes = Math.floor(seconds / 60);
		const remSec = Math.round(seconds - minutes * 60);
		return `+${minutes}m${remSec}s`;
	}

	/**
	 * Pure coalesce: turn a list of raw events into an ordered list of
	 * leaves. Re-runs on every event push (cheap — events are bounded
	 * per request). Replaces the prior imperative upsert/patch model
	 * that mutated component-local state; that broke when the card
	 * unmounted (Svelte re-keyed message rows after the
	 * optimistic→authoritative swap) because the accumulated state was
	 * tied to the component instance. With raw events in a shared
	 * store, remount loses nothing.
	 */
	interface CacheSummary {
		inputTokens: number;
		cachedTokens: number;
		cacheCreationTokens: number | null;
		outputTokens: number;
		callCount: number;
		hitPct: number;
	}

	/**
	 * **The summation and the hit rate are `$lib/llm/tokenUsage`'s, not
	 * this card's.** They were written here first and then written a
	 * second time for the task panel's Run act, which is two copies of
	 * one non-obvious fact — this provider's `input_tokens` already
	 * INCLUDES the cached portion, so the rate is `cached / input` and
	 * never `cached / (input + cached)`. What stays here is the part only
	 * this card knows: which events count, and where the four figures sit
	 * on its payload.
	 *
	 * One behaviour change, deliberate: a record is now skipped when it
	 * reported NO figure, where before it was skipped when all four
	 * summed to zero. An event that explicitly reports `0` did report a
	 * measurement, and absence is the thing worth skipping.
	 */
	function deriveCacheSummary(events: RawTurnEvent[]): CacheSummary | null {
		const usage: TokenUsage[] = [];
		for (const parsed of events) {
			const evt = parseEvent(parsed);
			// Count both the chat-side `llm.succeeded` events AND the
			// delegate flat-loop's typed `LLMResponseReceived` (whose data
			// carries the same token fields) so the header total is
			// CUMULATIVE across every LLM call in the turn, not just the
			// chat-side ones.
			if (!evt || (evt.eventType !== 'llm.succeeded' && evt.eventType !== 'LLMResponseReceived'))
				continue;
			const correlation = evt.payload.correlation as Record<string, unknown> | undefined;
			const callId = evt.payload.llm_call_id ?? correlation?.llm_call_id;
			const availability = (evt.payload.usage_availability ?? correlation?.usage_availability) as Record<string, boolean> | undefined;
			usage.push({
				callId: typeof callId === 'string' ? callId : undefined,
				input: availability?.tokens === false ? null : numberOrNull(evt.payload.input_tokens),
				output: availability?.tokens === false ? null : numberOrNull(evt.payload.output_tokens),
				cacheRead: availability?.cache_read === false ? null : numberOrNull(evt.payload.cache_read_tokens),
				cacheCreation: availability?.cache_write === false ? null : numberOrNull(evt.payload.cache_creation_tokens),
			});
		}
		const totals = totalUsage(usage);
		if (totals === null || totals.cachedPercent === null) return null;
		// The chip's own shape keeps plain numbers: it renders a
		// `toLocaleString()` tooltip and a percentage, both of which need a
		// number to print. The `?? 0` is that coercion done once, at the
		// boundary — the totals themselves distinguish "nothing reported"
		// from "reported zero", and the task panel's rows depend on it.
		return {
			inputTokens: totals.input ?? 0,
			cachedTokens: totals.cacheRead ?? 0,
			cacheCreationTokens: totals.cacheCreation,
			outputTokens: totals.output ?? 0,
			callCount: totals.calls,
			hitPct: totals.cachedPercent ?? 0,
		};
	}

	function numberOrNull(v: unknown): number | null {
		return typeof v === 'number' && Number.isFinite(v) ? v : null;
	}

	function deriveInspectTarget(
		events: RawTurnEvent[]
	): { taskId: string; executionId: string } | null {
		return deriveTaskBackedInspectTarget(events.map((event) => parseEvent(event)?.payload));
	}

	function formatTokenCount(n: number): string {
		if (!Number.isFinite(n) || n <= 0) return '0';
		if (n < 1000) return String(Math.round(n));
		if (n < 10_000) return `${(n / 1000).toFixed(1)}k`;
		if (n < 1_000_000) return `${Math.round(n / 1000)}k`;
		return `${(n / 1_000_000).toFixed(1)}M`;
	}

	function deriveLeaves(events: RawTurnEvent[]): Leaf[] {
		const byKey = new Map<string, Leaf>();
		const order: string[] = [];

		function upsert(leaf: Leaf) {
			if (!byKey.has(leaf.key)) {
				order.push(leaf.key);
			}
			byKey.set(leaf.key, leaf);
		}
		function patch(key: string, p: Partial<Leaf>) {
			const existing = byKey.get(key);
			if (!existing) return;
			const next = { ...existing, ...p };
			byKey.set(key, next);
			// On terminal-state transition (running/waiting → done|failed)
			// bubble the leaf to the end of `order` so it lands in the
			// inline tail's last-5 slice. Without this, a long-running
			// delegate or tool that completes after many subsequent
			// events stays pinned to its original position, scrolls off
			// the bounded tail, and the user never sees "X completed".
			if (
				(next.status === 'done' || next.status === 'failed') &&
				existing.status !== next.status
			) {
				const idx = order.indexOf(key);
				if (idx !== -1 && idx !== order.length - 1) {
					order.splice(idx, 1);
					order.push(key);
				}
			}
		}

		for (const parsed of events) {
			applyEvent(parsed, upsert, patch, byKey);
		}

		// If the task itself has reached a terminal state, any still-
		// running child rows in this per-turn projection are stale:
		// REST replay can miss a tool/LLM/reasoning terminal event, and
		// backend guards can intentionally drop a step.completed signal
		// after a rejected terminal yield. Without this cleanup a hard
		// refresh can show all historical rows but keep the header
		// spinner forever. Keep genuinely-active turns alive by bailing
		// whenever any task row is still running.
		const taskLeaves = Array.from(byKey.entries()).filter(([key]) =>
			key.startsWith('task::')
		);
		if (
			taskLeaves.length > 0 &&
			!taskLeaves.some(([, leaf]) => leaf.status === 'running')
		) {
			const terminalStatus: Extract<EventStatus, 'done' | 'failed'> =
				taskLeaves.some(([, leaf]) => leaf.status === 'failed') ? 'failed' : 'done';
			for (const [key, leaf] of byKey.entries()) {
				if (!key.startsWith('task::') && leaf.status === 'running') {
					patch(key, {
						status: terminalStatus,
						detail:
							leaf.detail ??
							(terminalStatus === 'failed'
								? 'Stopped when task failed'
								: 'Settled when task finished')
					});
				}
			}
		}

		return order.map((k) => byKey.get(k)).filter((l): l is Leaf => l !== undefined);
	}

	function trimTo(value: string, max: number): string {
		const trimmed = value.replace(/\s+/g, ' ').trim();
		if (trimmed.length <= max) return trimmed;
		return `${trimmed.slice(0, max - 1)}…`;
	}

	// Render a latency value with the unit that reads most naturally
	// at the magnitude — sub-second stays in ms (LLM first-byte is
	// often 400–900 ms; "0.7s" loses precision), second-or-more flips
	// to a one-decimal `s` so a 19-second chip doesn't sit on the row
	// as "19421ms" and force the operator to count digits.
	function formatLatency(ms: number): string {
		if (!Number.isFinite(ms) || ms < 0) return '—';
		if (ms < 1000) return `${Math.round(ms)}ms`;
		return `${(ms / 1000).toFixed(1)}s`;
	}

	// Compact `TTFT (total)` pair for the LLM activity row. First
	// number is time-to-first-token (model started speaking); the
	// parenthesized number is the full call duration. One side may be
	// null mid-flight — TTFT lands at `llm.first_token` while total is
	// still in flight, so total renders as `…`. After `llm.succeeded`,
	// both values are known and the row reads e.g. `1.2s (4.5s)`. When
	// TTFT is absent (tool-call-only response, all-reasoning turn,
	// non-streaming provider), only the total survives so the row
	// falls back to a single-value render — no orphaned parens.
	function formatLatencyPair(ttftMs: number | null, totalMs: number | null): string | null {
		const hasTtft = typeof ttftMs === 'number' && Number.isFinite(ttftMs);
		const hasTotal = typeof totalMs === 'number' && Number.isFinite(totalMs);
		if (!hasTtft && !hasTotal) return null;
		if (hasTtft && !hasTotal) return `${formatLatency(ttftMs!)} (…)`;
		if (!hasTtft && hasTotal) return formatLatency(totalMs!);
		return `${formatLatency(ttftMs!)} (${formatLatency(totalMs!)})`;
	}

	function extractTimestamp(parsed: Record<string, unknown>): number {
		const candidates: unknown[] = [
			(parsed as { timestamp_ms?: unknown }).timestamp_ms,
			((parsed.data as Record<string, unknown>) ?? {}).timestamp_ms,
			((parsed.data as Record<string, unknown>) ?? {}).timestamp,
			((((parsed.data as Record<string, unknown>) ?? {}).event as Record<string, unknown>) ?? {})
				.timestamp,
		];
		for (const c of candidates) {
			if (typeof c === 'number' && Number.isFinite(c)) return c;
		}
		return Date.now();
	}

	interface ParsedEvent {
		ts: number;
		eventType: string;
		agentId: string;
		callId: string | null;
		traceId: string | null;
		payload: Record<string, unknown>;
	}

	function parseEvent(parsed: Record<string, unknown>): ParsedEvent | null {
		const outerType = String(parsed.event_type ?? '');
		if (!outerType || outerType.startsWith('__events_')) return null;

		let eventType = outerType;
		let payload: Record<string, unknown> = {};
		let agentId: string | null = null;
		const data = parsed.data as Record<string, unknown> | undefined;
		if (outerType === 'AgentEvent' && data) {
			const inner = data.event as Record<string, unknown> | undefined;
			if (inner && typeof inner.event_type === 'string') {
				eventType = inner.event_type;
				payload = (inner.payload as Record<string, unknown>) ?? {};
				agentId =
					typeof inner.agent_id === 'string' ? (inner.agent_id as string) : null;
			}
		} else if (data) {
			payload = data;
			agentId =
				typeof data.agent_id === 'string' ? (data.agent_id as string) : null;
		}

		// Flat-loop typed transport variants → activity rows. The agentic
		// flat loop (executor.rs) emits a delegated agent's tool/LLM steps as
		// TYPED RuntimeTransportEvent variants (`AgenticActionExecuted`,
		// `LLMRequestSent`, `LLMResponseReceived`) — NOT AgentEvent-enveloped
		// `tool.*`/`llm.*` — so they matched none of the branches below and a
		// delegate's steps never rendered. Tool actions map cleanly onto the
		// `tool.call.*` branch because `iteration` yields a unique per-action
		// key. LLM calls do NOT (they share no per-call id — `step_index` is
		// null, `step_id`/`capability` are constant across iterations), so
		// they get a dedicated, timestamp-keyed branch in `applyEvent`.
		if (outerType === 'AgenticActionExecuted') {
			eventType = payload.success === false ? 'tool.call.failed' : 'tool.call.finished';
			payload = {
				...payload,
				// Prefer the specific tool (`target`, e.g. "shell" / "tool_search")
				// over the action CATEGORY (`action_type`, e.g. "pack"), so rows
				// read "shell returned" not "pack returned" — and match the
				// deep-work panel's labelling.
				tool_name: payload.target ?? payload.action_type,
				// Include `timestamp` (distinct per emit): a loop iteration can
				// execute MULTIPLE tool actions, so `iteration` alone is not
				// unique and would collapse same-iteration actions into one row.
				// The store dedup already drops true duplicates, so each row
				// here is a distinct action.
				call_id: `${(payload.step_id as string) ?? 'step'}-${(payload.iteration as number) ?? 0}-${(payload.timestamp as number) ?? 0}`,
				duration_ms: payload.latency_ms
			};
		}

		// Fanout-aware normalisation: when the backend's chat-fanout
		// emits a re-stamped copy of a sub-agent's event (see
		// realtime_events.rs:2870-3026), it puts the CHAT agent's id on
		// the envelope (so the chat-scoped subscription receives it)
		// and preserves the true emitter under `payload.origin_agent_id`.
		// Prefer `origin_agent_id` here so the primary emit AND the
		// fanout copy of the same logical event normalise to the same
		// agent — without this, every leaf appears twice in the request
		// activity card (one under each agent prefix).
		const originAgentId =
			typeof payload?.origin_agent_id === 'string'
				? (payload.origin_agent_id as string)
				: null;
		const rawAgent = originAgentId ?? agentId ?? 'system';
		// Synthetic agent_ids (`task_*`, `exec_*`, `cycle_*`) come from
		// internal execution contexts using their own ctx ids as the envelope
		// `agent_id`. Use a generic label so the prefix stays meaningful.
		let normalizedAgent = rawAgent;
		if (
			normalizedAgent.startsWith('task_') ||
			normalizedAgent.startsWith('exec_') ||
			normalizedAgent.startsWith('cycle_')
		) {
			normalizedAgent = 'execution';
		}

		// `call_id` is namespaced by the fanout (`delegate-<exec>/<orig>`)
		// to keep parallel delegate tool-calls from colliding on the
		// chat side; that namespacing makes the SAME logical tool call
		// look like two distinct calls (primary keyed on `<orig>`,
		// fanout keyed on `delegate-<exec>/<orig>`). Prefer the
		// preserved `origin_call_id` so both copies collapse to one row.
		const canonicalCallId =
			(payload?.origin_call_id as string | undefined) ??
			(payload?.call_id as string | undefined) ??
			(payload?.tool_call_id as string | undefined) ??
			null;

		return {
			ts: extractTimestamp(parsed),
			eventType,
			agentId: normalizedAgent,
			callId: canonicalCallId,
			traceId: (payload?.trace_id as string | undefined) ?? null,
			payload,
		};
	}

	async function notifyTutorOverlayStatusFromActivity(
		status: 'working' | 'idle',
		payload?: Record<string, unknown> | null
	): Promise<void> {
		if (typeof window === 'undefined' || !('__TAURI_INTERNALS__' in window)) return;
		const payloadSessionId =
			typeof payload?.chat_session_id === 'string'
				? payload.chat_session_id
				: typeof payload?.session_id === 'string'
					? payload.session_id
					: sessionId;
		if (!payloadSessionId) return;
		try {
			const { invoke } = await import('@tauri-apps/api/core');
			await invoke('show_tutor_overlay_status', {
				status,
				sessionId: payloadSessionId
			});
		} catch (error) {
			console.warn('[RequestActivityCard] tutor overlay status update failed:', error);
		}
	}

	function applyEvent(
		parsed: RawTurnEvent,
		upsertLeaf: (leaf: Leaf) => void,
		patchLeaf: (key: string, patch: Partial<Leaf>) => void,
		leafByKey: Map<string, Leaf>
	) {
		const evt = parseEvent(parsed);
		if (!evt) return;

		if (evt.eventType.startsWith('tutor.')) {
			const runId = (evt.payload?.run_id as string | undefined) ?? 'active';
			const overlayTransition = tutorOverlayTransitionForEvent(
				evt.eventType,
				runId,
				live,
				terminalTutorRunIds
			);
			if (overlayTransition) {
				void notifyTutorOverlayStatusFromActivity(overlayTransition, evt.payload);
			} else if (live && terminalTutorRunIds.has(runId)) {
				console.debug('[RequestActivityCard] ignored stale tutor overlay working event after terminal run', {
					runId,
					eventType: evt.eventType
				});
			}
			const runKey = `${evt.agentId}::tutor::${runId}`;
			const stepId =
				(evt.payload?.step_id as string | undefined) ??
				`step-${evt.payload?.step_count ?? evt.payload?.step_kind ?? evt.ts}`;
			const stepKey = `${evt.agentId}::tutor::${runId}::${stepId}`;
			const note = (evt.payload?.note as string | undefined) ?? null;
			const goal = (evt.payload?.goal as string | undefined) ?? null;
			const target = (evt.payload?.target as string | undefined) ?? null;
			const stepLabel = (evt.payload?.step_label as string | undefined) ?? null;
			const expectedState = (evt.payload?.expected_state as string | undefined) ?? null;
			const detail =
				[target, stepLabel, expectedState, note]
					.find((value) => typeof value === 'string' && value.trim().length > 0) ?? null;
			const closeRunningTutorStepLeaves = (status: 'done' | 'failed') => {
				const prefix = `${runKey}::`;
				for (const [key, leaf] of leafByKey.entries()) {
					if (key.startsWith(prefix) && leaf.status === 'running') {
						patchLeaf(key, { status });
					}
				}
			};

			if (evt.eventType === 'tutor.run.started') {
				if (leafByKey.has(runKey)) return;
				upsertLeaf({
					key: runKey,
					kind: 'lifecycle',
					agentId: evt.agentId,
					label: 'Tutor started',
					fullText: goal,
					detail: goal ? trimTo(goal, 180) : null,
					status: 'running',
					startedAt: evt.ts,
					durationMs: null,
					files: []
				});
				return;
			}
			if (evt.eventType === 'tutor.run.completed') {
				closeRunningTutorStepLeaves('done');
				const patch = {
					label: 'Tutor completed',
					status: 'done' as const,
					detail: goal ? trimTo(goal, 180) : null,
					fullText: goal
				};
				if (leafByKey.has(runKey)) {
					patchLeaf(runKey, patch);
				} else {
					upsertLeaf({
						key: runKey,
						kind: 'lifecycle',
						agentId: evt.agentId,
						startedAt: evt.ts,
						durationMs: null,
						files: [],
						...patch
					});
				}
				return;
			}
			if (evt.eventType === 'tutor.run.failed') {
				closeRunningTutorStepLeaves('failed');
				const failure = note || goal || 'Tutor run failed';
				const patch = {
					label: 'Tutor failed',
					status: 'failed' as const,
					detail: trimTo(failure, 180),
					fullText: failure
				};
				if (leafByKey.has(runKey)) {
					patchLeaf(runKey, patch);
				} else {
					upsertLeaf({
						key: runKey,
						kind: 'lifecycle',
						agentId: evt.agentId,
						startedAt: evt.ts,
						durationMs: null,
						files: [],
						...patch
					});
				}
				return;
			}

			const stepPatch = (() => {
				switch (evt.eventType) {
					case 'tutor.step.observed':
						return { label: 'Observed screen', status: 'done' as const };
					case 'tutor.step.target_resolved':
						return { label: 'Resolved target', status: 'done' as const };
					case 'tutor.step.drawing':
						return { label: 'Drew marker', status: 'done' as const };
					case 'tutor.step.action_delegated':
						return { label: 'Action delegated', status: 'running' as const };
					case 'tutor.step.verifying':
						return { label: 'Verifying action', status: 'running' as const };
					case 'tutor.step.verified':
						return { label: 'Verified action', status: 'done' as const };
					case 'tutor.step.failed':
						return { label: 'Tutor step failed', status: 'failed' as const };
					case 'tutor.step.recovering':
						return { label: 'Recovering tutor flow', status: 'running' as const };
					case 'tutor.step.clearing':
						return { label: 'Cleared tutor marks', status: 'done' as const };
					default:
						return null;
				}
			})();
			if (!stepPatch) return;

			const fullText = [stepLabel, expectedState, note]
				.filter((value): value is string => typeof value === 'string' && value.trim().length > 0)
				.join('\n');
			const leafPatch = {
				...stepPatch,
				detail: detail ? trimTo(detail, 180) : null,
				fullText: fullText || null
			};
			if (leafByKey.has(stepKey)) {
				patchLeaf(stepKey, leafPatch);
			} else {
				upsertLeaf({
					key: stepKey,
					kind: 'lifecycle',
					agentId: evt.agentId,
					startedAt: evt.ts,
					durationMs: null,
					files: [],
					...leafPatch
				});
			}
			return;
		}

		if (evt.eventType.startsWith('coding.')) {
			const shadowId =
				(evt.payload?.shadow_workspace_id as string | undefined) ?? 'coding';
			const profile = evt.payload?.coding_profile as Record<string, unknown> | undefined;
			const profileLabel =
				(typeof profile?.label === 'string' && profile.label) ||
				(typeof profile?.id === 'string' && profile.id) ||
				'Coding agent';
			const mainKey = `${evt.agentId}::coding::${shadowId}`;
			if (evt.eventType === 'coding.started') {
				upsertLeaf({
					key: mainKey,
					kind: 'lifecycle',
					agentId: evt.agentId,
					label: `Coding with ${profileLabel}`,
					fullText: (evt.payload?.prompt_preview as string | undefined) ?? null,
					detail:
						(typeof profile?.model === 'string' && profile.model) ||
						(evt.payload?.engine as string | undefined) ||
						null,
					status: 'running',
					startedAt: evt.ts,
					durationMs: null,
					files: []
				});
				return;
			}
			if (evt.eventType === 'coding.agent_started') {
				if (!leafByKey.has(mainKey)) {
					upsertLeaf({
						key: mainKey,
						kind: 'lifecycle',
						agentId: evt.agentId,
						label: `Coding with ${profileLabel}`,
						fullText: null,
						detail: null,
						status: 'running',
						startedAt: evt.ts,
						durationMs: null,
						files: []
					});
				}
				return;
			}
			if (evt.eventType === 'coding.message') {
				const delta = (evt.payload?.delta as string | undefined) ?? '';
				if (!delta) return;
				const existing = leafByKey.get(mainKey);
				const merged = `${existing?.fullText ?? ''} ${delta}`.trim().replace(/\s+/g, ' ');
				const label = merged
					? `Coding agent: ${trimTo(merged, LABEL_MAX_CHARS)}`
					: `Coding with ${profileLabel}`;
				if (!existing) {
					upsertLeaf({
						key: mainKey,
						kind: 'lifecycle',
						agentId: evt.agentId,
						label,
						fullText: merged,
						detail: null,
						status: 'running',
						startedAt: evt.ts,
						durationMs: null,
						files: []
					});
				} else {
					patchLeaf(mainKey, { label, fullText: merged, status: 'running' });
				}
				return;
			}
			if (evt.eventType === 'coding.tool.started' || evt.eventType === 'coding.tool.finished') {
				const tool =
					(evt.payload?.tool_name as string | undefined) ||
					(evt.payload?.tool as string | undefined) ||
					'coding agent tool';
				const callId =
					(evt.payload?.tool_call_id as string | undefined) ??
					`${evt.payload?.sequence ?? evt.ts}`;
				const key = `${evt.agentId}::coding-tool::${shadowId}::${callId}`;
				if (evt.eventType === 'coding.tool.started') {
					if (leafByKey.has(key)) return;
					upsertLeaf({
						key,
						kind: 'tool',
						agentId: evt.agentId,
						label: `Coding agent tool: ${tool}`,
						fullText: null,
						detail: null,
						status: 'running',
						startedAt: evt.ts,
						durationMs: null,
						files: []
					});
				} else {
					if (leafByKey.has(key)) {
						patchLeaf(key, {
							label: `Coding agent tool finished: ${tool}`,
							status: 'done'
						});
					} else {
						upsertLeaf({
							key,
							kind: 'tool',
							agentId: evt.agentId,
							label: `Coding agent tool finished: ${tool}`,
							fullText: null,
							detail: null,
							status: 'done',
							startedAt: evt.ts,
							durationMs: null,
							files: []
						});
					}
				}
				return;
			}
			if (evt.eventType === 'coding.approval_requested') {
				const fileCount = evt.payload?.file_count as number | undefined;
				patchLeaf(mainKey, {
					label: 'Coding proposal ready',
					status: 'waiting',
					detail:
						typeof fileCount === 'number'
							? `${fileCount} file${fileCount === 1 ? '' : 's'}`
							: null
				});
				return;
			}
			if (evt.eventType === 'coding.completed') {
				const pendingApproval = evt.payload?.pending_approval === true;
				const noChange = evt.payload?.no_change === true;
				const assistantText = (evt.payload?.assistant_text as string | undefined) ?? null;
				const label = pendingApproval
					? 'Coding proposal ready'
					: noChange
						? 'Coding finished with no changes'
						: 'Coding finished';
				if (leafByKey.has(mainKey)) {
					patchLeaf(mainKey, {
						label,
						status: pendingApproval ? 'waiting' : 'done',
						fullText: assistantText ?? leafByKey.get(mainKey)?.fullText ?? null
					});
				} else {
					upsertLeaf({
						key: mainKey,
						kind: 'lifecycle',
						agentId: evt.agentId,
						label,
						fullText: assistantText,
						detail: null,
						status: pendingApproval ? 'waiting' : 'done',
						startedAt: evt.ts,
						durationMs: null,
						files: []
					});
				}
				return;
			}
			if (evt.eventType === 'coding.failed') {
				const error = (evt.payload?.error as string | undefined) ?? 'coding failed';
				if (leafByKey.has(mainKey)) {
					patchLeaf(mainKey, {
						label: 'Coding failed',
						status: 'failed',
						detail: trimTo(error, 200),
						fullText: error
					});
				} else {
					upsertLeaf({
						key: mainKey,
						kind: 'lifecycle',
						agentId: evt.agentId,
						label: 'Coding failed',
						fullText: error,
						detail: trimTo(error, 200),
						status: 'failed',
						startedAt: evt.ts,
						durationMs: null,
						files: []
					});
				}
				return;
			}
			return;
		}

		// Flat-loop LLM call (typed `LLMResponseReceived`). Rendered as a
		// self-contained row keyed by timestamp: the typed LLM events carry
		// no per-call id to pair request↔response or coalesce by, so each
		// completed call is its own row. `LLMRequestSent` is skipped — the
		// response already carries capability + outcome + latency.
		if (evt.eventType === 'LLMResponseReceived') {
			const cap = (evt.payload?.capability as string | undefined) || 'model';
			const ok = evt.payload?.success !== false;
			const ms = evt.payload?.latency_ms as number | undefined;
			const summary =
				(evt.payload?.decision_summary as string | undefined) ||
				(evt.payload?.error as string | undefined) ||
				null;
			upsertLeaf({
				key: `${evt.agentId}::llm::${evt.ts}`,
				kind: 'llm',
				agentId: evt.agentId,
				label: `Thinking with ${cap}`,
				fullText: summary,
				detail: typeof ms === 'number' ? `${Math.round(ms)}ms` : null,
				status: ok ? 'done' : 'failed',
				startedAt: evt.ts,
				durationMs: ms ?? null,
				files: []
			});
			return;
		}

		// LLM lifecycle (coalesced by trace_id or iteration)
		if (evt.eventType.startsWith('llm.')) {
			const tracePart =
				evt.traceId ??
				(evt.payload?.iteration as number | undefined)?.toString() ??
				'llm';
			const key = `${evt.agentId}::llm::${tracePart}`;
			if (evt.eventType === 'llm.requested') {
				// `requested` is upsert-if-missing — same out-of-order
				// reasoning as the reasoning.start guard above. A late
				// `requested` arriving after `succeeded` would otherwise
				// flip the leaf from "Done" back to "running" and lose
				// the duration detail patched by `succeeded`.
				if (leafByKey.has(key)) return;
				const model =
					(evt.payload?.model as string | undefined) ||
					(evt.payload?.capability as string | undefined) ||
					'model';
				upsertLeaf({
					key,
					kind: 'llm',
					agentId: evt.agentId,
					label: `Thinking with ${model}`,
					fullText: null,
					detail: null,
					status: 'running',
					startedAt: evt.ts,
					durationMs: null,
					files: [],
				});
				return;
			}
			if (evt.eventType === 'llm.first_token') {
				// Time-to-first-token chip. The row stays in `running`
				// because the LLM call hasn't finished — more tokens
				// are still streaming — but the detail flips from
				// nothing to `TTFT (…)` so the operator can see the
				// model started speaking. `llm.succeeded` rewrites the
				// detail to `TTFT (total)` when the call settles.
				const ttftMs = evt.payload?.duration_ms as number | undefined;
				if (typeof ttftMs === 'number') {
					patchLeaf(key, {
						detail: formatLatencyPair(ttftMs, null),
					});
				}
				return;
			}
			if (evt.eventType === 'llm.succeeded') {
				const ms = evt.payload?.duration_ms as number | undefined;
				const ttftMs = evt.payload?.ttft_ms as number | undefined | null;
				// Keep the running "Thinking with <model>" label as-is on
				// success — just flip the status icon to done and attach
				// the duration. The previous "Response ready" relabel was
				// noise: the user already sees the actual response in the
				// bubble above the activity strip, so a redundant
				// terminal row labeled "Response ready" added nothing.
				// Retaining the leaf (no longer deleted) ensures simple
				// LLM-only turns still surface at least one activity row
				// after page reload, so the embedded strip doesn't
				// disappear entirely for "just a chat reply" turns.
				patchLeaf(key, {
					status: 'done',
					detail: formatLatencyPair(
						typeof ttftMs === 'number' ? ttftMs : null,
						typeof ms === 'number' ? ms : null
					),
					durationMs: ms ?? null,
				});
				return;
			}
			if (evt.eventType === 'llm.failed') {
				const err = (evt.payload?.error as string | undefined) ?? 'error';
				const model =
					(evt.payload?.model as string | undefined) ||
					(evt.payload?.capability as string | undefined) ||
					'model';
				// Upsert-if-missing — a failure that arrives without a
				// preceding `requested` (event reordering, or a chat
				// turn whose prompt-build step exploded before
				// `llm.requested` had a chance to fire) would
				// otherwise leave no row at all and the activity card
				// would look as if nothing ever happened. With this
				// branch the failure always surfaces a terminal row,
				// matching what the user sees in the inline error
				// bubble.
				if (leafByKey.has(key)) {
					patchLeaf(key, {
						label: `Thinking with ${model}`,
						status: 'failed',
						detail: trimTo(err, 160),
					});
				} else {
					upsertLeaf({
						key,
						kind: 'llm',
						agentId: evt.agentId,
						label: `Thinking with ${model}`,
						fullText: null,
						detail: trimTo(err, 160),
						status: 'failed',
						startedAt: evt.ts,
						durationMs:
							typeof evt.payload?.duration_ms === 'number'
								? (evt.payload.duration_ms as number)
								: null,
						files: [],
					});
				}
				return;
			}
			return;
		}

		// Reasoning — "Thought: <text>" rows. Coalesce per trace_id.
		if (evt.eventType.startsWith('reasoning.')) {
			const key = `${evt.agentId}::reasoning::${evt.traceId ?? 'r'}`;
			if (evt.eventType === 'reasoning.start') {
				// `start` is a no-op when a leaf already exists. The
				// backend often emits `start`/`content`/`end` for one
				// reasoning trace at the same millisecond, and the
				// `/events/page` REST handler can deliver them in
				// non-causal order. Without this guard, a late-
				// arriving `start` would overwrite the leaf that
				// `content` already populated — resetting the label
				// to "Thought: …" with no text. Keeping `start` as
				// upsert-if-missing preserves whichever event won the
				// race to land first.
				if (!leafByKey.has(key)) {
					upsertLeaf({
						key,
						kind: 'reasoning',
						agentId: evt.agentId,
						label: 'Thought: …',
						fullText: '',
						detail: null,
						status: 'running',
						startedAt: evt.ts,
						durationMs: null,
						files: [],
					});
				}
				return;
			}
			if (evt.eventType === 'reasoning.content') {
				const delta =
					(evt.payload?.delta as string | undefined) ??
					(evt.payload?.content as string | undefined) ??
					'';
				const existing = leafByKey.get(key);
				const merged = ((existing?.fullText ?? '') + ' ' + delta).trim();
				const cleaned = merged.replace(/\s+/g, ' ');
				const label = cleaned ? `Thought: ${trimTo(cleaned, LABEL_MAX_CHARS)}` : 'Thought: …';
				if (!existing) {
					upsertLeaf({
						key,
						kind: 'reasoning',
						agentId: evt.agentId,
						label,
						fullText: cleaned,
						detail: null,
						status: 'running',
						startedAt: evt.ts,
						durationMs: null,
						files: [],
					});
				} else {
					patchLeaf(key, { label, fullText: cleaned });
				}
				return;
			}
			if (evt.eventType === 'reasoning.end') {
				patchLeaf(key, { status: 'done' });
				return;
			}
			return;
		}

		// Tool lifecycle (coalesced by call_id)
		if (evt.eventType.startsWith('tool.call.') || evt.eventType.startsWith('tool.')) {
			const cid = evt.callId ?? `anon-${evt.ts}`;
			const tool =
				(evt.payload?.tool_name as string | undefined) ||
				(evt.payload?.tool as string | undefined) ||
				(evt.payload?.name as string | undefined) ||
				'tool';
			const key = `${evt.agentId}::tool::${cid}`;
			if (evt.eventType === 'tool.result.projected') {
				const resultRef = evt.payload?.result_ref as string | undefined;
				if (!resultRef) return;
				const resultPatch: Partial<Leaf> = {
					resultRef,
					resultHash: (evt.payload?.content_hash as string | undefined) ?? null,
					resultSizeBytes: (evt.payload?.size_bytes as number | undefined) ?? null,
					resultOwner: parseCompleteResultOwner(evt.payload?.result_owner),
					resultTaskId: (evt.payload?.task_id as string | undefined) ?? null,
					resultExecutionId: (evt.payload?.execution_id as string | undefined) ?? null,
				};
				if (leafByKey.has(key)) {
					patchLeaf(key, resultPatch);
				} else {
					upsertLeaf({
						key,
						kind: 'tool',
						agentId: evt.agentId,
						label: `${tool} result available`,
						fullText: null,
						detail: null,
						status: 'done',
						startedAt: evt.ts,
						durationMs: null,
						files: [],
						...resultPatch,
					});
				}
				return;
			}
			if (evt.eventType.endsWith('.started')) {
				// Upsert-if-missing — see reasoning.start / llm.requested
				// for rationale. A late `started` would otherwise wipe
				// the patched `done` state + files attached by
				// `.finished` / `.succeeded`.
				if (leafByKey.has(key)) return;
				// tactical pattern T4: special-case delegate_to_agent so the chat
				// activity card surfaces parallel fan-out explicitly
				// ("Decomposing into 5 parallel agents") instead of a
				// generic "Calling delegate_to_agent". The args payload
				// carries delegation_targets directly per the runtime
				// emission contract (see chat-turn JSONL trace).
				let label = `Calling ${tool}`;
				let detail: string | null = null;
				if (tool === 'delegate_to_agent') {
					const args =
						(evt.payload?.args as Record<string, unknown> | undefined) ?? {};
					const targets = Array.isArray(args.delegation_targets)
						? (args.delegation_targets as unknown[])
						: [];
					const targetIds = targets
						.map((entry) => {
							if (!entry || typeof entry !== 'object') return null;
							const obj = entry as Record<string, unknown>;
							return typeof obj.target_agent_id === 'string'
								? obj.target_agent_id
								: null;
						})
						.filter((id): id is string => !!id);
					if (targetIds.length >= 2) {
						label = `Decomposing into ${targetIds.length} parallel agents`;
						detail = targetIds.join(', ');
					} else if (targetIds.length === 1) {
						label = `Delegating to ${targetIds[0]}`;
					}
				}
				upsertLeaf({
					key,
					kind: 'tool',
					agentId: evt.agentId,
					label,
					fullText: null,
					detail,
					status: 'running',
					startedAt: evt.ts,
					durationMs: null,
					files: [],
				});
				return;
			}
			if (evt.eventType.endsWith('.args')) {
				if (!leafByKey.has(key)) {
					upsertLeaf({
						key,
						kind: 'tool',
						agentId: evt.agentId,
						label: `Calling ${tool}`,
						fullText: null,
						detail: null,
						status: 'running',
						startedAt: evt.ts,
						durationMs: null,
						files: [],
					});
				}
				return;
			}
			if (evt.eventType.endsWith('.succeeded') || evt.eventType.endsWith('.finished')) {
				const ms = evt.payload?.duration_ms as number | undefined;
				const preview = (evt.payload?.content_preview as string | undefined) ?? null;
				const filesFromTool = extractFiles(evt.payload);
				// Terminal-state arrives WITHOUT a preceding `.started`/`.args`
				// for two reasons we observe in the field:
				//   1. Pagination — only `.finished` is in the first page;
				//      the matching `.started` lives in an older page the
				//      user hasn't loaded yet.
				//   2. Out-of-order arrival — same-millisecond events from
				//      the REST handler don't preserve causal order.
				// In both cases the patch-only branch silently lost the
				// row. Upsert-if-missing so a "tool returned" leaf appears
				// regardless of what preceded it.
				if (!leafByKey.has(key)) {
					upsertLeaf({
						key,
						kind: 'tool',
						agentId: evt.agentId,
						label: `${tool} returned`,
						fullText: preview,
						detail: ms ? `${Math.round(ms)}ms` : null,
						status: 'done',
						startedAt: evt.ts,
						durationMs: ms ?? null,
						files: filesFromTool,
					});
					return;
				}
				patchLeaf(key, {
					label: `${tool} returned`,
					status: 'done',
					detail: ms ? `${Math.round(ms)}ms` : null,
					fullText: preview,
					durationMs: ms ?? null,
					files: filesFromTool,
				});
				return;
			}
			if (evt.eventType.endsWith('.failed')) {
				const err = (evt.payload?.error as string | undefined) ?? 'failed';
				// Same upsert-if-missing rationale as the success branch.
				if (!leafByKey.has(key)) {
					upsertLeaf({
						key,
						kind: 'tool',
						agentId: evt.agentId,
						label: `${tool} failed`,
						fullText: err,
						detail: trimTo(err, 200),
						status: 'failed',
						startedAt: evt.ts,
						durationMs: null,
						files: [],
					});
					return;
				}
				patchLeaf(key, {
					label: `${tool} failed`,
					status: 'failed',
					detail: trimTo(err, 200),
					fullText: err,
				});
				return;
			}
			return;
		}

		// Task lifecycle (delegate / spawned task). Coalesce by
		// task_id so a single task's running → completed/failed
		// transition becomes one row that morphs through statuses,
		// not multiple rows. Replaces the legacy "Delegated agent X
		// completed" pill that used to be a synthesized chat message.
		// `status` semantics: 'completed' | 'failed' | 'cancelled'
		// (terminal) or 'running' | 'planning' | 'executing' (in-flight).
		if (
			evt.eventType === 'task.status_changed' ||
			evt.eventType === 'chat.delegate.status_changed'
		) {
			const taskId =
				(evt.payload?.task_id as string | undefined) ?? `task-${evt.ts}`;
			const targetAgent =
				(evt.payload?.chat_inline_delegate_agent_id as string | undefined) ??
				(evt.payload?.target_agent_id as string | undefined) ??
				(evt.payload?.origin_agent_id as string | undefined) ??
				evt.agentId;
			const displayLabel =
				(evt.payload?.display_label as string | undefined)?.trim() || targetAgent;
			const status = (evt.payload?.status as string | undefined) ?? 'running';
			const summary = (evt.payload?.summary as string | undefined) ?? null;
			const synthesisPending = evt.payload?.synthesis_pending === true;
			const key = `task::${taskId}`;
			const leafStatus: EventStatus =
				synthesisPending
					? 'running'
					: status === 'completed' || status === 'succeeded'
					? 'done'
					: status === 'failed' || status === 'cancelled'
					? 'failed'
					: 'running';
			const label =
				synthesisPending
					? `${displayLabel} preparing final result`
					: leafStatus === 'done'
					? `${displayLabel} completed`
					: leafStatus === 'failed'
					? `${displayLabel} ${status}`
					: `${displayLabel} ${status}`;
			const fullText = synthesisPending
				? summary ?? 'Task completed. Preparing final result...'
				: summary;
			const existing = leafByKey.get(key);
			if (!existing) {
				upsertLeaf({
					key,
					kind: 'lifecycle',
					agentId: targetAgent,
					label,
					fullText,
					detail: null,
					status: leafStatus,
					startedAt: evt.ts,
					durationMs: null,
					files: extractFiles(evt.payload),
				});
			} else {
				patchLeaf(key, {
					label,
					status: leafStatus,
					fullText: fullText ?? existing.fullText,
					files:
						extractFiles(evt.payload).length > 0
							? extractFiles(evt.payload)
							: existing.files,
				});
			}
			return;
		}

		// Async output-synthesis success (v0.6.682+ paired with the
		// chat watcher's post-terminal SynthesisReadiness poll). Fires
		// 5–60s AFTER the corresponding `chat.delegate.status_changed`
		// — the task is already terminal, but the synthesized user-
		// output has just landed. Morph the existing `task::*` leaf to
		// surface the synthesized summary inline.
		//
		// v0.6.686 fix 3: PRESERVE the existing leaf status when the
		// task was cancelled or failed. Previously hard-coded
		// `status: 'done'`, which morphed a user-cancelled task into
		// a green "output ready" pill even though the task itself
		// didn't succeed. `terminal_task_status` in the payload
		// (added v0.6.686) makes this decision authoritative; for
		// older events without it, fall back to the existing leaf's
		// status when non-running.
		if (evt.eventType === 'chat.delegate.output_ready') {
			const taskId = evt.payload?.task_id as string | undefined;
			if (!taskId) return;
			const summaryPreview =
				(evt.payload?.summary_preview as string | undefined) ?? null;
			const terminalTaskStatus = evt.payload?.terminal_task_status as
				| string
				| undefined;
			const key = `task::${taskId}`;
			const existing = leafByKey.get(key);
			const morphStatus: EventStatus =
				terminalTaskStatus === 'cancelled' ||
				terminalTaskStatus === 'failed' ||
				existing?.status === 'failed'
					? 'failed'
					: 'done';
			const labelSuffix =
				morphStatus === 'failed'
					? terminalTaskStatus === 'cancelled'
						? 'cancelled, output ready'
						: 'failed, output ready'
					: 'output ready';
			if (existing) {
				patchLeaf(key, {
					label: `${existing.agentId} — ${labelSuffix}`,
					status: morphStatus,
					fullText: summaryPreview ?? existing.fullText,
				});
			} else {
				upsertLeaf({
					key,
					kind: 'lifecycle',
					agentId:
						(evt.payload?.target_agent_id as string | undefined) ?? evt.agentId,
					label: labelSuffix,
					fullText: summaryPreview,
					detail: null,
					status: morphStatus,
					startedAt: evt.ts,
					durationMs: null,
					files: [],
				});
			}
			return;
		}

		// Async output-synthesis failure — the 1.1/1.2/1.3 LLM
		// pipeline exhausted retries OR hit the watcher's 5m timeout.
		// Outputs are unavailable; flip the existing task leaf to
		// `failed` and surface the failure stage + last_error.
		if (evt.eventType === 'chat.delegate.output_failed') {
			const taskId = evt.payload?.task_id as string | undefined;
			if (!taskId) return;
			const stage = (evt.payload?.stage as string | undefined) ?? 'synthesis';
			const lastError =
				(evt.payload?.last_error as string | undefined) ?? 'unknown error';
			const synthetic = evt.payload?.synthetic === true;
			const reasonNote = synthetic
				? 'synthesis timed out'
				: `synthesis failed (${stage})`;
			const detailText = `${reasonNote}: ${lastError}`;
			const key = `task::${taskId}`;
			const existing = leafByKey.get(key);
			if (existing) {
				patchLeaf(key, {
					label: `${existing.agentId} — ${reasonNote}`,
					status: 'failed',
					fullText: detailText,
				});
			} else {
				upsertLeaf({
					key,
					kind: 'lifecycle',
					agentId:
						(evt.payload?.target_agent_id as string | undefined) ?? evt.agentId,
					label: reasonNote,
					fullText: detailText,
					detail: null,
					status: 'failed',
					startedAt: evt.ts,
					durationMs: null,
					files: [],
				});
			}
			return;
		}

		// Artifact / output creation — one leaf per file, clickable via
		// the shared pathActions handlers. Routed through the same chat-
		// scoped open-file/open-folder endpoints as the chat bubble's
		// rich tool results, so the visual + click semantics match.
		if (evt.eventType === 'artifact.created' || evt.eventType === 'output.created') {
			const files = extractFiles(evt.payload);
			if (files.length === 0) {
				// Some artifact emits carry only a download_url, no
				// absolute path — render a generic "Created X" row in
				// that case so the user still sees the activity.
				const name =
					(evt.payload?.display_name as string | undefined) ||
					(evt.payload?.name as string | undefined) ||
					'artifact';
				upsertLeaf({
					key: `${evt.agentId}::artifact::${evt.ts}::${name}`,
					kind: 'file',
					agentId: evt.agentId,
					label: `Produced ${name}`,
					fullText: null,
					detail: null,
					status: 'done',
					startedAt: evt.ts,
					durationMs: null,
					files: [],
				});
				return;
			}
			for (const file of files) {
				const key = `${evt.agentId}::file::${file.absolutePath}`;
				upsertLeaf({
					key,
					kind: 'file',
					agentId: evt.agentId,
					label: `Produced ${file.label}`,
					fullText: file.absolutePath,
					detail: null,
					status: 'done',
					startedAt: evt.ts,
					durationMs: null,
					files: [file],
				});
			}
			return;
		}

		// Drop everything else — keeps the tail readable. Full event
		// stream is still in `/events` for power users.
	}

	/**
	 * Extract file references from a heterogeneous event payload. Supports
	 * the common emit shapes:
	 *   - `output_files: [{absolute_path, label, ...}]` (pack_progress)
	 *   - `files: [...]` (generic)
	 *   - `absolute_path: "..."` + optional `display_name` (artifact.created)
	 *   - `content_blocks: [{type:'file', absolute_path}]` (rich tool result)
	 */
	function extractFiles(payload: Record<string, unknown> | undefined): FileRef[] {
		if (!payload) return [];
		const out: FileRef[] = [];

		const candidateArrays: unknown[] = [
			payload.output_files,
			payload.files,
			payload.content_blocks,
		];
		for (const arr of candidateArrays) {
			if (!Array.isArray(arr)) continue;
			for (const entry of arr) {
				if (!entry || typeof entry !== 'object') continue;
				const obj = entry as Record<string, unknown>;
				const absolutePath =
					(obj.absolute_path as string | undefined) ??
					(obj.path as string | undefined) ??
					'';
				if (!absolutePath || !absolutePath.startsWith('/')) continue;
				const label =
					(obj.label as string | undefined) ||
					(obj.display_name as string | undefined) ||
					(obj.relative_path as string | undefined) ||
					absolutePath.split('/').pop() ||
					absolutePath;
				out.push({ absolutePath, label });
			}
		}

		if (out.length === 0) {
			const absolutePath = (payload.absolute_path as string | undefined) ?? '';
			if (absolutePath.startsWith('/')) {
				const label =
					(payload.display_name as string | undefined) ||
					(payload.name as string | undefined) ||
					absolutePath.split('/').pop() ||
					absolutePath;
				out.push({ absolutePath, label });
			}
		}

		return out;
	}

	function statusIcon(status: EventStatus): string {
		switch (status) {
			case 'done':
				return '✓';
			case 'failed':
				return '✗';
			case 'waiting':
				return '⏸';
			case 'running':
				return '◐';
			default:
				return '•';
		}
	}

	function agentLabel(agentId: string): string {
		if (!agentId || agentId === 'system' || agentId === 'execution') return '';
		// Short alias for the most common agent so the line stays compact.
		return `[${agentId}] `;
	}

	function handleKeydown(e: KeyboardEvent) {
		if (modalOpen && e.key === 'Escape') {
			modalOpen = false;
		}
	}

	// Note: subscription is wired via the reactive `maybeSubscribe`
	// declaration above so it activates as soon as scope hydrates
	// (often a tick after mount). onMount-only subscribe was the
	// silent-failure mode where the card sat empty forever when scope
	// arrived late. We still release our ref on destroy.
	onDestroy(() => {
		unsubscribe?.();
		unsubscribe = null;
		subscribedKey = null;
	});
</script>

<svelte:window on:keydown={handleKeydown} />

{#if !embedded || live || leaves.length > 0}
<!-- When the card is embedded inside an assistant bubble and the turn
     produced no *visible* activity rows (and isn't actively live-
     streaming), suppress the strip entirely. We gate on `leaves.length`
     rather than `totalEvents` because the raw event stream includes
     meta-events that never produce a leaf (lifecycle book-keeping, LLM
     successes we deliberately drop, etc.) — "Activity · 1 event" with
     no rows below it is worse than rendering nothing. The free-standing
     (non-embedded) card always renders so historical turns still
     surface their (empty) state explicitly. -->
<div
	class="request-activity"
	class:request-activity--embedded={embedded}
	class:request-activity--live={live}
	class:request-activity--complete={isComplete}
>
	<button
		type="button"
		class="request-activity__header"
		class:request-activity__header--expanded={expanded}
		on:click={toggleExpanded}
		aria-expanded={expanded}
		aria-controls="request-activity-body-{chatTurnId}"
		title={expanded ? 'Hide steps' : 'Show steps'}
	>
		<span class="request-activity__caret" aria-hidden="true">▸</span>
		<span class="request-activity__title">Steps</span>
		<span class="request-activity__count">
			{totalEvents}
			{#if live || anyRunning}
				<!-- Working spinner: show whenever the card is live-tailing
				     (events flowing) OR a leaf is actively running. Gating on
				     `live` (not just `anyRunning`) keeps it steady across a
				     delegate's between-iteration gaps, where every leaf is
				     momentarily "done" but the run is still going. -->
				<span class="request-activity__spinner" aria-label="Working"></span>
			{/if}
		</span>
		{#if cacheSummary}
			<!-- Prompt-cache chip: per-turn cache hit rate. Visible only when
			     the turn made at least one LLM call that reported usage. Tone
			     keys off hit-pct so a glance reads "warm" vs "cold". Title
			     surfaces the underlying numbers for operators who want detail. -->
			<span
				class="request-activity__cache"
				class:request-activity__cache--warm={cacheSummary.hitPct >= 50}
				class:request-activity__cache--cold={cacheSummary.hitPct < 10}
				title={`Prompt cache: ${cacheSummary.cachedTokens.toLocaleString()} of ${cacheSummary.inputTokens.toLocaleString()} input tokens served from cache across ${cacheSummary.callCount} call${cacheSummary.callCount === 1 ? '' : 's'}. Cache writes: ${cacheSummary.cacheCreationTokens?.toLocaleString() ?? 'not reported'}.`}
				aria-label={`Prompt cache hit rate ${cacheSummary.hitPct.toFixed(0)} percent`}
			>
				<span class="request-activity__cache-pct">{cacheSummary.hitPct.toFixed(0)}%</span>
				<span class="request-activity__cache-label">cached</span>
				<span class="request-activity__cache-detail">· {formatTokenCount(cacheSummary.cachedTokens)} / {formatTokenCount(cacheSummary.inputTokens)} tok</span>
			</span>
		{/if}
		<!-- "Show all" only matters when there's MORE than what the
		     inline tail can fit. With ≤ MAX_INLINE_ROWS leaves the
		     modal would just re-display the same rows (full text vs
		     truncated). And even then we only surface it when the
		     section is expanded — collapsed mode keeps the header
		     to the minimum (caret + label + count). -->
		{#if expanded && hiddenCount > 0}
			<span
				class="request-activity__show-all"
				on:click|stopPropagation={() => (modalOpen = true)}
				title="Open full activity log"
				role="button"
				tabindex="0"
				on:keydown={(e) => {
					if (e.key === 'Enter' || e.key === ' ') {
						e.preventDefault();
						modalOpen = true;
					}
				}}
			>
				Show all ({hiddenCount} more)
			</span>
		{/if}
		{#if showExecutionStop && anyRunning && inspectTarget?.executionId && !cancelled}
			<!-- Stop the still-running execution. Gated on `anyRunning` (a leaf
			     never closed — incl. an orphaned/stuck run) + an execution to
			     target. Rebuilt from REST events, so it survives a refresh. -->
			<span
				class="request-activity__stop"
				on:click|stopPropagation={() => void cancelRun()}
				title="Cancel this run"
				role="button"
				tabindex="0"
				on:keydown={(e) => {
					if (e.key === 'Enter' || e.key === ' ') {
						e.preventDefault();
						void cancelRun();
					}
				}}
			>
				{cancelling ? 'Stopping…' : 'Stop'}
			</span>
		{/if}
		{#if inspectTarget}
			<!-- Deep-work panel trigger. Lives inside the header (like
			     "Show all") as a role=button span so it doesn't nest a
			     <button>; stops propagation so it never toggles the strip. -->
			<span
				class="request-activity__inspect"
				on:click|stopPropagation={() => inspectTarget && dispatch('inspect', inspectTarget)}
				title="Open the deep-work panel for this run"
				role="button"
				tabindex="0"
				on:keydown={(e) => {
					if (e.key === 'Enter' || e.key === ' ') {
						e.preventDefault();
						if (inspectTarget) dispatch('inspect', inspectTarget);
					}
				}}
			>
				Inspect run →
			</span>
		{/if}
	</button>

	{#if expanded && inlineLeaves.length > 0}
		<!-- Inline view is always chronological (flat). The tree
		     grouping is modal-only — kept simpler in the bubble. -->
		<ul
			id="request-activity-body-{chatTurnId}"
			class="request-activity__tail"
		>
			{#each inlineLeaves as leaf (leaf.key)}
				<li
					class="request-activity__row"
					class:request-activity__row--done={leaf.status === 'done'}
					class:request-activity__row--failed={leaf.status === 'failed'}
					class:request-activity__row--waiting={leaf.status === 'waiting'}
					class:request-activity__row--running={leaf.status === 'running'}
					title={leaf.fullText ?? leaf.label}
				>
					<span class="request-activity__row-status" aria-hidden="true">
						{statusIcon(leaf.status)}
					</span>
					<span class="request-activity__row-label">
						{#if agentLabel(leaf.agentId)}<span class="request-activity__row-agent"
								>{agentLabel(leaf.agentId)}</span
							>{/if}{leaf.label}
					</span>
					{#each leaf.files as file (file.absolutePath)}
						<!-- svelte-ignore a11y_missing_attribute -->
						<a
							class="request-activity__row-file"
							href={`file://${file.absolutePath}`}
							on:click|preventDefault={() => void openPath(file.absolutePath, 'file')}
							title={file.absolutePath}
							use:pathActionAttach={{ path: file.absolutePath, size: 'sm' }}
						>
							{file.label}
						</a>
					{/each}
					{#if leaf.resultRef}
						<button
							type="button"
							class="request-activity__result-button"
							on:click|stopPropagation={() => void openFullResult(leaf)}
						>
							Open full result
						</button>
					{/if}
					{#if leaf.detail}
						<span class="request-activity__row-detail">{leaf.detail}</span>
					{/if}
				</li>
			{/each}
		</ul>
	{/if}
</div>
{/if}

{#if modalOpen}
	<!-- Full activity log modal. Plain fixed overlay with a scroll
	     pane; click-outside or Escape closes. No tree grouping —
	     chronological flat list with [agent] prefix for context, matching
	     the inline tail's mental model but with full text instead of
	     ellipsis. -->
	<!-- svelte-ignore a11y_click_events_have_key_events -->
	<!-- svelte-ignore a11y_no_static_element_interactions -->
	<div
		class="request-activity-modal__scrim"
		on:click={() => (modalOpen = false)}
	>
		<div
			class="request-activity-modal__panel"
			role="dialog"
			aria-modal="true"
			aria-label="Full activity log"
			tabindex="-1"
			on:click|stopPropagation
		>
			<div class="request-activity-modal__head">
				<span class="request-activity-modal__title">Activity · {totalEvents} events</span>
				<div class="request-activity-modal__view-toggle" role="group" aria-label="View">
					<button
						type="button"
						class="request-activity-modal__view-btn"
						class:request-activity-modal__view-btn--active={view === 'flat'}
						on:click={() => (view = 'flat')}
					>
						Flat
					</button>
					<button
						type="button"
						class="request-activity-modal__view-btn"
						class:request-activity-modal__view-btn--active={view === 'tree'}
						on:click={() => (view = 'tree')}
					>
						Tree
					</button>
				</div>
				<button
					type="button"
					class="request-activity-modal__close"
					on:click={() => (modalOpen = false)}
					aria-label="Close"
				>
					×
				</button>
			</div>
			<div class="request-activity-modal__body">
				{#if leaves.length === 0}
					<div class="request-activity-modal__empty">No activity yet.</div>
				{:else if view === 'tree'}
					<!-- Tree view: events grouped by agent, each agent
					     section collapsible (the section header click is the
					     toggle). Rows inside follow the same row chrome as
					     the flat view, minus the per-row agent prefix. -->
					{#each agentTree as group (group.agentId)}
						<section class="request-activity-modal__group">
							<header class="request-activity-modal__group-header">
								<span class="request-activity-modal__group-name">
									{agentLabel(group.agentId).trim() || 'system'}
								</span>
								<span class="request-activity-modal__group-count">
									{group.leaves.length} {group.leaves.length === 1 ? 'event' : 'events'}
								</span>
							</header>
							<ul class="request-activity-modal__list">
								{#each group.leaves as leaf (leaf.key)}
									<li
										class="request-activity-modal__row"
										class:request-activity-modal__row--done={leaf.status === 'done'}
										class:request-activity-modal__row--failed={leaf.status === 'failed'}
										class:request-activity-modal__row--running={leaf.status === 'running'}
									>
										<span class="request-activity-modal__row-status" aria-hidden="true">
											{statusIcon(leaf.status)}
										</span>
										<div class="request-activity-modal__row-content">
											<div class="request-activity-modal__row-label">
												<span class="request-activity-modal__row-ts" title={new Date(leaf.startedAt).toLocaleString()}>
													{formatElapsed(leaf.startedAt - requestStart)}
												</span>
												{leaf.label}
											</div>
											{#if leaf.files.length > 0}
												<ul class="request-activity-modal__row-files">
													{#each leaf.files as file (file.absolutePath)}
														<li class="request-activity-modal__row-file">
															<!-- svelte-ignore a11y_missing_attribute -->
															<a
																class="request-activity-modal__row-file-link"
																href={`file://${file.absolutePath}`}
																on:click|preventDefault={() => void openPath(file.absolutePath, 'file')}
																title={file.absolutePath}
																use:pathActionAttach={{ path: file.absolutePath, size: 'md' }}
															>
																{file.label}
															</a>
														</li>
													{/each}
												</ul>
											{/if}
											{#if leaf.resultRef}
												<button type="button" class="request-activity-modal__result-button" on:click={() => void openFullResult(leaf)}>
													Open complete result
												</button>
											{/if}
											{#if leaf.fullText && leaf.fullText.length > leaf.label.length && leaf.kind !== 'file'}
												<div class="request-activity-modal__row-full">{leaf.fullText}</div>
											{/if}
											{#if leaf.detail}
												<div class="request-activity-modal__row-detail">{leaf.detail}</div>
											{/if}
										</div>
									</li>
								{/each}
							</ul>
						</section>
					{/each}
				{:else}
					<!-- Flat view: strictly chronological — one row per leaf
					     in insertion order, agent prefix on each row. No
					     grouping, no sticky section headers. Matches the
					     embedded/inline tail's flat behavior; "Tree" is the
					     view to use when you want grouping by agent. -->
					<ul class="request-activity-modal__list">
						{#each leaves as leaf (leaf.key)}
							<li
								class="request-activity-modal__row"
								class:request-activity-modal__row--done={leaf.status === 'done'}
								class:request-activity-modal__row--failed={leaf.status === 'failed'}
								class:request-activity-modal__row--running={leaf.status === 'running'}
							>
								<span class="request-activity-modal__row-status" aria-hidden="true">
									{statusIcon(leaf.status)}
								</span>
								<div class="request-activity-modal__row-content">
									<div class="request-activity-modal__row-label">
										<span class="request-activity-modal__row-ts" title={new Date(leaf.startedAt).toLocaleString()}>
											{formatElapsed(leaf.startedAt - requestStart)}
										</span>
										<span class="request-activity-modal__row-agent-prefix">
											[{agentLabel(leaf.agentId).trim() || 'system'}]
										</span>
										{leaf.label}
									</div>
									{#if leaf.files.length > 0}
										<ul class="request-activity-modal__row-files">
											{#each leaf.files as file (file.absolutePath)}
												<li class="request-activity-modal__row-file">
													<!-- svelte-ignore a11y_missing_attribute -->
													<a
														class="request-activity-modal__row-file-link"
														href={`file://${file.absolutePath}`}
														on:click|preventDefault={() => void openPath(file.absolutePath, 'file')}
														title={file.absolutePath}
														use:pathActionAttach={{ path: file.absolutePath, size: 'md' }}
													>
														{file.label}
													</a>
												</li>
											{/each}
										</ul>
									{/if}
									{#if leaf.resultRef}
										<button type="button" class="request-activity-modal__result-button" on:click={() => void openFullResult(leaf)}>
											Open complete result
										</button>
									{/if}
									{#if leaf.fullText && leaf.fullText.length > leaf.label.length && leaf.kind !== 'file'}
										<div class="request-activity-modal__row-full">{leaf.fullText}</div>
									{/if}
									{#if leaf.detail}
										<div class="request-activity-modal__row-detail">{leaf.detail}</div>
									{/if}
								</div>
							</li>
						{/each}
					</ul>
				{/if}
				<!--
					Pagination removed: the per-turn endpoint returns the
					full event log in one response, so "Load older" /
					"End of activity" are both meaningless. The activity
					card now renders everything the sink wrote and the
					live SSE tail keeps it in sync — there is nothing
					more to load.
				-->
			</div>
		</div>
	</div>
{/if}

{#if resultViewerOpen}
	<div class="request-result-viewer" role="presentation" on:click={() => (resultViewerOpen = false)}>
		<dialog open class="request-result-viewer__panel" aria-label={resultViewerTitle} on:click|stopPropagation>
			<header class="request-result-viewer__header">
				<div>
					<h3>{resultViewerTitle}</h3>
					{#if resultViewerHash}<p>Verified content · {resultViewerHash.slice(0, 12)}</p>{/if}
				</div>
				<button type="button" aria-label="Close complete result" on:click={() => (resultViewerOpen = false)}>×</button>
			</header>
			<div class="request-result-viewer__body">
				{#if resultViewerLoading}
					<p class="request-result-viewer__state">Loading the complete authorized result…</p>
				{:else if resultViewerError}
					<p class="request-result-viewer__error">{resultViewerError}</p>
				{:else}
					<pre>{resultViewerText}</pre>
				{/if}
			</div>
		</dialog>
	</div>
{/if}

<style>
	/* All colors / borders here go through the project's design tokens
	 * (`--bg-*`, `--text-*`, `--border-*`, `--accent-secondary`,
	 * `--color-success/error/warning/info`) defined in `app.css` for
	 * each theme. Each var has a hex fallback so the card still renders
	 * sensibly if a future theme drops one. Scrim + shadow derive from
	 * `--text-primary` via `color-mix` in oklab so they adapt to theme
	 * luminance (dim on cream paper, lift on dark). */

	.request-activity {
		/* Match the look-and-feel of the existing in-chat action
		 * cards (`.chat-status-alert`, `.chat-executed-alert`) which
		 * use the project's design tokens (`--bg-soft`, `--border-soft`,
		 * `--text-primary`). That keeps the card visually consistent
		 * with the cream-paper / dark themes the rest of the chat
		 * surface uses, instead of fighting it with daisyUI's
		 * `--color-base-*` which renders white on cream paper. */
		margin: 0;
		padding: 0.55rem 0.8rem;
		font-size: 0.82rem;
		color: var(--text-primary, #2d3436);
		background: var(--bg-soft, #f6f1e8);
		border: 1px solid var(--border-soft, #eee4dc);
		border-left: 3px solid var(--accent-secondary, #4ecdc4);
		border-radius: var(--radius-sm, 6px);
		max-width: min(100%, 600px);
		min-width: 0;
		box-sizing: border-box;
		transition: border-color 240ms ease, background 240ms ease;
	}
	/* Embedded variant — card sits *inside* the assistant chat-bubble.
	 * The bubble already provides surface + border, so we strip the
	 * outer chrome and become a soft top-separated strip.
	 *
	 * Visual hierarchy: the assistant's actual reply is the primary
	 * element in the bubble; the activity strip is a subordinate
	 * footnote. We enforce this with (a) a generous top margin to put
	 * breathing room between the reply text and the strip, (b) a left
	 * indent so the strip clearly nests under the reply, and (c) a
	 * smaller `font-size` that cascades to every child row.
	 *
	 * Color sourcing notes (the project has a dozen+ themes, light/dark/
	 * brutalist/etc., so every color must come from a token):
	 *   - `--border-default` (not `--border-soft`) for the separator —
	 *     the neutral chat-bubble is painted with `--bg-card`, which in
	 *     the cream-paper theme is `#ffffff`. `--border-soft` (#eee4dc)
	 *     sits a single shade off white and is essentially invisible on
	 *     a white bubble. `--border-default` (#ddd3ca) has the contrast
	 *     to read as an actual divider.
	 *   - `color: inherit` propagates the bubble's text color (which is
	 *     already `--text-primary` for neutral, `--text-on-accent` for
	 *     primary). Means the strip reads correctly in every theme
	 *     without having to opt-in per bubble class. */
	.request-activity--embedded {
		background: transparent;
		border: none;
		border-radius: 0;
		padding: 0.35rem 0 0 0;
		margin-top: 0.5rem;
		max-width: 100%;
		color: inherit;
		font-size: 0.72rem;
		opacity: 0.95;
	}
	.request-activity--embedded .request-activity__row {
		padding: 0.08rem 0;
		line-height: 1.35;
	}
	.request-activity__header {
		display: flex;
		align-items: center;
		gap: 0.4rem;
		margin-bottom: 0.15rem;
		padding: 0.1rem 0;
		background: transparent;
		border: none;
		cursor: pointer;
		color: inherit;
		font: inherit;
		text-align: left;
		width: 100%;
	}
	.request-activity__header:hover .request-activity__title {
		opacity: 1;
	}
	/* The disclosure caret rotates 90° when expanded — same visual
	 * grammar as a native `<details>` triangle. Tiny + muted so the
	 * "Steps" label is what reads first. */
	.request-activity__caret {
		font-size: 0.7em;
		opacity: 0.55;
		transition: transform 140ms ease;
		display: inline-block;
		line-height: 1;
	}
	.request-activity__header--expanded .request-activity__caret {
		transform: rotate(90deg);
	}
	/* Title color echoes the user-message bubble background so the
	 * activity strip reads as a quiet callback to "the thing they
	 * asked". `--accent-primary` is the same token daisy-ui's
	 * `chat-bubble-primary` resolves to via our theme, so the tie
	 * holds across every theme. */
	.request-activity__title {
		font-weight: 600;
		color: var(--accent-primary);
		opacity: 0.85;
		letter-spacing: 0.01em;
	}
	.request-activity__count {
		display: inline-flex;
		align-items: center;
		gap: 0.35rem;
		opacity: 0.55;
		font-size: 0.85em;
		font-variant-numeric: tabular-nums;
	}
	/* Tiny circular spinner shown next to the count while any leaf is
	 * still running. Sized to the count's x-height so it reads as a
	 * sibling to the number, not a separate ornament. Uses the same
	 * `--accent-primary` as the title to keep the running-state visual
	 * coherent with the section's identity color. */
	.request-activity__spinner {
		display: inline-block;
		width: 0.7em;
		height: 0.7em;
		border-radius: 50%;
		border: 1.5px solid color-mix(in srgb, var(--accent-primary) 30%, transparent);
		border-top-color: var(--accent-primary);
		animation: request-activity-spin 0.9s linear infinite;
	}
	@keyframes request-activity-spin {
		to { transform: rotate(360deg); }
	}
	/* Prompt-cache chip in the header. Subtle by default — same accent
	 * family as the title so it reads as part of the section identity
	 * rather than a foreign badge. The `--warm` modifier shifts the
	 * border + pct color toward the success tone when caching is
	 * landing (≥50% hit rate); `--cold` falls back to the muted error
	 * tone when caching has dropped out entirely (<10% hit). Defaults
	 * use only theme variables so the chip recolors with the active
	 * theme without component-level overrides. */
	.request-activity__cache {
		display: inline-flex;
		align-items: baseline;
		gap: 0.3rem;
		padding: 0.1rem 0.45rem;
		border: 1px solid color-mix(in srgb, var(--accent-secondary, #4ecdc4) 35%, transparent);
		border-radius: 999px;
		background: color-mix(in srgb, var(--accent-secondary, #4ecdc4) 8%, transparent);
		color: var(--text-secondary, var(--text-primary));
		font-size: 0.72em;
		line-height: 1.4;
		font-variant-numeric: tabular-nums;
		cursor: default;
	}
	.request-activity__cache--warm {
		border-color: color-mix(in srgb, var(--color-success, #00bb7f) 45%, transparent);
		background: color-mix(in srgb, var(--color-success, #00bb7f) 10%, transparent);
	}
	.request-activity__cache--cold {
		border-color: color-mix(in srgb, var(--color-warning, #ffe66d) 45%, transparent);
		background: color-mix(in srgb, var(--color-warning, #ffe66d) 10%, transparent);
	}
	.request-activity__cache-pct {
		font-weight: 600;
		color: var(--accent-secondary, #4ecdc4);
	}
	.request-activity__cache--warm .request-activity__cache-pct {
		color: var(--color-success, #00bb7f);
	}
	.request-activity__cache--cold .request-activity__cache-pct {
		color: var(--color-warning, #ffe66d);
	}
	.request-activity__cache-label {
		opacity: 0.7;
		letter-spacing: 0.02em;
	}
	.request-activity__cache-detail {
		opacity: 0.55;
		font-family: var(--font-mono, monospace);
	}
	/* "Show all" pushed to the far right of the header. Renders as a
	 * subtle link-style affordance — no bg/border so it doesn't compete
	 * with the title or the disclosure caret for attention. */
	.request-activity__show-all {
		margin-left: auto;
		background: transparent;
		border: none;
		color: inherit;
		cursor: pointer;
		font-size: 0.85em;
		padding: 0.1rem 0.35rem;
		opacity: 0.65;
		text-decoration: underline;
		text-underline-offset: 2px;
	}
	.request-activity__show-all:hover {
		opacity: 1;
	}
	.request-activity__inspect {
		margin-left: auto;
		flex: 0 0 auto;
		font-size: 0.72rem;
		font-weight: 600;
		color: var(--accent-secondary, #4ecdc4);
		background: transparent;
		border: 1px solid var(--border-soft, #eee4dc);
		border-radius: var(--radius-full, 999px);
		padding: 0.05rem 0.5rem;
		cursor: pointer;
		white-space: nowrap;
		transition: background 0.12s ease, color 0.12s ease;
	}
	.request-activity__inspect:hover {
		background: var(--accent-secondary-soft, rgba(78, 205, 196, 0.16));
		color: var(--text-primary, #2d3436);
	}
	.request-activity__stop {
		margin-left: auto;
		flex: 0 0 auto;
		font-size: 0.72rem;
		font-weight: 600;
		color: var(--color-error, #d23a3a);
		background: transparent;
		border: 1px solid color-mix(in srgb, var(--color-error, #d23a3a) 40%, transparent);
		border-radius: var(--radius-full, 999px);
		padding: 0.05rem 0.5rem;
		cursor: pointer;
		white-space: nowrap;
		transition:
			background 0.12s ease,
			color 0.12s ease;
	}
	.request-activity__stop:hover {
		background: color-mix(in srgb, var(--color-error, #d23a3a) 14%, transparent);
	}
	.request-activity__tail {
		list-style: none;
		padding: 0;
		margin: 0;
	}
	.request-activity__row {
		display: flex;
		align-items: baseline;
		gap: 0.4rem;
		padding: 0.1rem 0;
		line-height: 1.4;
		white-space: nowrap;
		overflow: hidden;
	}
	.request-activity__row-status {
		flex: 0 0 1ch;
		opacity: 0.7;
		font-family: var(--font-mono, monospace);
	}
	.request-activity__row--done .request-activity__row-status {
		color: var(--color-success, #00bb7f);
		opacity: 1;
	}
	.request-activity__row--failed .request-activity__row-status {
		color: var(--color-error, #ff6b6b);
		opacity: 1;
	}
	.request-activity__row--waiting .request-activity__row-status {
		color: var(--color-warning, #ffe66d);
		opacity: 1;
	}
	.request-activity__row--running .request-activity__row-status {
		color: var(--color-info, #4d9de0);
		opacity: 1;
	}
	.request-activity__row-label {
		flex: 1 1 auto;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		min-width: 0;
	}
	.request-activity__row-agent {
		opacity: 0.55;
		font-size: 0.9em;
	}
	.request-activity__row-detail {
		flex: 0 0 auto;
		opacity: 0.55;
		font-size: 0.85em;
		font-family: var(--font-mono, monospace);
	}
	.request-activity__row-file {
		flex: 0 0 auto;
		display: inline-flex;
		align-items: center;
		gap: 0.25rem;
		color: var(--accent-secondary, #4ecdc4);
		font-size: 0.92em;
		text-decoration: none;
		max-width: 28ch;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.request-activity__row-file:hover {
		text-decoration: underline;
	}
	.request-activity-modal__row-files {
		list-style: none;
		padding: 0;
		margin: 0.35rem 0 0 0;
		display: flex;
		flex-direction: column;
		gap: 0.2rem;
	}
	.request-activity-modal__row-file {
		display: flex;
		align-items: center;
		gap: 0.3rem;
	}
	.request-activity-modal__row-file-link {
		color: var(--accent-secondary, #4ecdc4);
		text-decoration: none;
		font-size: 0.88rem;
		word-break: break-word;
		overflow-wrap: anywhere;
	}
	.request-activity-modal__row-file-link:hover {
		text-decoration: underline;
	}

	/* ─── Modal ─────────────────────────────────────────────────────── */
	.request-activity-modal__scrim {
		position: fixed;
		inset: 0;
		/* Scrim deliberately uses oklab so it darkens any theme
		 * uniformly — base-content at low alpha lifts in dark mode
		 * and dims in light mode without us picking a fixed hex. */
		background: color-mix(in oklab, var(--text-primary, #2d3436) 45%, transparent);
		display: flex;
		align-items: center;
		justify-content: center;
		z-index: 90;
		padding: 2rem;
	}
	.request-activity-modal__panel {
		background: var(--bg-card, #ffffff);
		color: var(--text-primary, #2d3436);
		border: 1px solid var(--border-soft, #eee4dc);
		border-radius: 10px;
		box-shadow: 0 18px 48px color-mix(in oklab, var(--text-primary, #2d3436) 28%, transparent);
		max-width: 900px;
		width: 100%;
		max-height: 80vh;
		display: flex;
		flex-direction: column;
		overflow: hidden;
	}
	.request-activity-modal__head {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		padding: 0.8rem 1rem;
		border-bottom: 1px solid var(--border-soft, #eee4dc);
	}
	.request-activity-modal__title {
		font-weight: 600;
		font-size: 0.95rem;
		flex: 0 0 auto;
	}
	.request-activity-modal__view-toggle {
		display: inline-flex;
		margin-left: auto;
		border: 1px solid var(--border-soft);
		border-radius: 6px;
		overflow: hidden;
		background: var(--bg-card);
	}
	.request-activity-modal__view-btn {
		background: transparent;
		border: none;
		padding: 0.2rem 0.7rem;
		font-size: 0.8rem;
		cursor: pointer;
		color: var(--text-muted);
		transition: background 120ms ease, color 120ms ease;
	}
	.request-activity-modal__view-btn:hover {
		color: var(--text-primary);
		background: var(--bg-soft);
	}
	.request-activity-modal__view-btn--active {
		background: var(--accent-primary-soft, color-mix(in srgb, var(--accent-primary) 14%, transparent));
		color: var(--text-primary);
		box-shadow: inset 0 -2px 0 0 var(--accent-primary);
	}
	.request-activity-modal__view-btn + .request-activity-modal__view-btn {
		border-left: 1px solid var(--border-soft);
	}
	.request-activity-modal__close {
		background: transparent;
		border: none;
		font-size: 1.5rem;
		cursor: pointer;
		opacity: 0.6;
		padding: 0 0.5rem;
		color: inherit;
	}
	.request-activity-modal__close:hover {
		opacity: 1;
	}
	.request-activity-modal__body {
		overflow-y: auto;
		padding: 0.5rem 1rem 1rem 1rem;
		flex: 1 1 auto;
	}
	.request-activity-modal__empty {
		opacity: 0.5;
		font-style: italic;
		padding: 2rem 0;
		text-align: center;
	}
	.request-activity-modal__list {
		list-style: none;
		padding: 0;
		margin: 0;
	}
	.request-activity-modal__group {
		margin: 0 0 0.5rem 0;
	}
	.request-activity-modal__group-header {
		display: flex;
		align-items: baseline;
		gap: 0.5rem;
		padding: 0.4rem 0.25rem;
		border-bottom: 1px solid var(--border-soft, #eee4dc);
		margin-bottom: 0.2rem;
	}
	.request-activity-modal__group-name {
		font-weight: 600;
		font-size: 0.88rem;
		color: var(--text-primary, #2d3436);
	}
	.request-activity-modal__group-count {
		opacity: 0.55;
		font-size: 0.8rem;
	}
	.request-activity-modal__row-ts {
		display: inline-block;
		min-width: 4.5ch;
		text-align: right;
		opacity: 0.5;
		font-size: 0.78rem;
		font-family: var(--font-mono, monospace);
		margin-right: 0.5rem;
		font-weight: normal;
		color: var(--text-primary, #2d3436);
	}
	/* `[Agent]` prefix on each flat-view row. Muted so the event label
	 * stays the dominant element; the prefix is just enough context to
	 * say who emitted the event. */
	.request-activity-modal__row-agent-prefix {
		display: inline-block;
		margin-right: 0.4rem;
		opacity: 0.65;
		font-size: 0.78rem;
		font-family: var(--font-mono, monospace);
		color: var(--text-secondary, #5f6668);
	}
	.request-activity-modal__row {
		display: flex;
		gap: 0.6rem;
		padding: 0.4rem 0;
		border-bottom: 1px dashed var(--bg-soft, #f6f1e8);
	}
	.request-activity-modal__row:last-child {
		border-bottom: none;
	}
	.request-activity-modal__row-status {
		flex: 0 0 1ch;
		font-family: var(--font-mono, monospace);
		padding-top: 0.1rem;
	}
	.request-activity-modal__row--done .request-activity-modal__row-status {
		color: var(--color-success, #00bb7f);
	}
	.request-activity-modal__row--failed .request-activity-modal__row-status {
		color: var(--color-error, #ff6b6b);
	}
	.request-activity-modal__row--running .request-activity-modal__row-status {
		color: var(--color-info, #4d9de0);
	}
	.request-activity-modal__row-content {
		flex: 1 1 auto;
		min-width: 0;
	}
	.request-activity-modal__row-label {
		font-size: 0.88rem;
		word-break: break-word;
		overflow-wrap: anywhere;
	}
	.request-activity-modal__row-full {
		margin-top: 0.25rem;
		font-size: 0.82rem;
		opacity: 0.85;
		white-space: pre-wrap;
		word-break: break-word;
		overflow-wrap: anywhere;
		line-height: 1.45;
	}
	.request-activity-modal__row-detail {
		margin-top: 0.2rem;
		font-size: 0.75rem;
		opacity: 0.55;
		font-family: var(--font-mono, monospace);
	}
	.request-activity__result-button,
	.request-activity-modal__result-button {
		border: 1px solid color-mix(in oklab, var(--accent-secondary, #4d9de0) 45%, transparent);
		background: color-mix(in oklab, var(--accent-secondary, #4d9de0) 10%, transparent);
		color: var(--accent-secondary, #377fb8);
		border-radius: 0.4rem;
		font: inherit;
		font-size: 0.75rem;
		font-weight: 600;
		padding: 0.18rem 0.45rem;
		cursor: pointer;
		white-space: nowrap;
	}
	.request-activity-modal__result-button {
		margin-top: 0.35rem;
	}
	.request-result-viewer {
		position: fixed;
		inset: 0;
		z-index: 1200;
		display: grid;
		place-items: center;
		padding: 1rem;
		background: color-mix(in oklab, var(--text-primary, #202426) 52%, transparent);
		backdrop-filter: blur(4px);
	}
	.request-result-viewer__panel {
		display: flex;
		flex-direction: column;
		width: min(68rem, 96vw);
		height: min(48rem, 90vh);
		border: 1px solid var(--border-soft, #ded7ce);
		border-radius: 0.9rem;
		background: var(--bg-primary, #fffdfa);
		color: var(--text-primary, #202426);
		box-shadow: 0 1.2rem 4rem color-mix(in oklab, var(--text-primary, #202426) 30%, transparent);
		overflow: hidden;
	}
	.request-result-viewer__header {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 1rem;
		padding: 0.9rem 1rem;
		border-bottom: 1px solid var(--border-soft, #ded7ce);
	}
	.request-result-viewer__header h3,
	.request-result-viewer__header p { margin: 0; }
	.request-result-viewer__header p {
		margin-top: 0.2rem;
		font: 0.72rem var(--font-mono, monospace);
		color: var(--text-secondary, #677074);
	}
	.request-result-viewer__header button {
		border: 0;
		background: transparent;
		color: var(--text-secondary, #677074);
		font-size: 1.5rem;
		cursor: pointer;
	}
	.request-result-viewer__body {
		flex: 1 1 auto;
		overflow: auto;
		padding: 1rem;
	}
	.request-result-viewer__body pre {
		margin: 0;
		font: 0.78rem/1.5 var(--font-mono, monospace);
		white-space: pre-wrap;
		word-break: break-word;
	}
	.request-result-viewer__state { color: var(--text-secondary, #677074); }
	.request-result-viewer__error { color: var(--color-error, #d94b4b); }
</style>
