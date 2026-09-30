<script lang="ts">
	/**
	 * `/attention` — unified human-in-the-loop inbox.
	 *
	 * Single surface for every pending HITL across every source
	 * (approval, clarification, plan_approval, agentic, user_request,
	 * escalation, bot_auth) plus failed executions. Backed by
	 * `pendingHitlStore` + `attentionStore`, which subscribes to the canonical
	 * `HitlRequested` / `HitlResolved` bus events — same source the top-bar
	 * badge/cascade and the chat typing-bubble pill consume. Resolution from
	 * any surface drops the row from all of them in the same frame.
	 *
	 * This surface is for live, just-in-time notifications only. Channel
	 * message follow-ups are patient, not urgent — they live in Today.
	 *
	 * Click any row → `respondToHitl(request)` opens the global
	 * `AttentionPromptModal`, POSTs
	 * canonically to `/api/magician/v2/hitl/{cid}/respond` with the
	 * per-source dispatcher routing.
	 *
	 */
	import { get } from 'svelte/store';
	import { onMount, onDestroy } from 'svelte';
	import { hitlRequestFromCanonicalEvent } from '$lib/hitl/adapters';
	import type { HitlRequest, HitlSource } from '$lib/hitl/types';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import { primaryAgent } from '$lib/stores/agentStore';
	import { PRODUCT_NAME, agentDisplayName } from '$lib/presentationIdentity';
	import { attentionStore } from '$lib/stores/attentionStore';
	import { timedFetch } from '$lib/shared/fetch';
	import { goto } from '$app/navigation';
	import { browser } from '$app/environment';
	import RecordingDot from '$lib/shared/components/RecordingDot.svelte';
	import ServerPager from '$lib/shared/components/ServerPager.svelte';
	import { pageAfterRemoval } from '$lib/shared/components/pageAfterRemoval';
	import {
		ATTENTION_SOURCE_COLORS,
		ATTENTION_SOURCE_LABELS,
		AttentionCategoryTabs,
		AttentionInboxSurface,
		attentionCategoryCountsFromTotals,
		attentionFeedLanesForCategory,
		attentionFrontiersProven,
		attentionGloballyProvenRowCount,
		attentionJumpLandingIndex,
		attentionPageRows,
		attentionPagerView,
		attentionRelativeTime,
		attentionRows,
		attentionSourceCategory,
		createAttentionItemController,
		filterAttentionRows,
		filterAttentionRowsByCategory,
		type AttentionCategory,
		type AttentionDisplayRow,
		type AttentionInboxFeedback,
		type AttentionSourceFrontier,
		type AttentionViewMode,
		type SkillEvolutionGateAction,
		type SkillEvolutionRollbackDecision
	} from '$lib/attention';
	import { meetingSessionLabel, type ActiveMeetingSession } from '$lib/stores/meetingsStore';

	/**
	 * One resolved HITL pair (HitlRequested + matching HitlResolved)
	 * surfaced in the history view. Built by walking the backfill from
	 * `/api/magician/v3/events?category=hitl` and matching on
	 * correlation_id.
	 */
	interface ResolvedRow {
		correlation_id: string;
		source: HitlSource;
		prompt: string;
		hint?: string;
		scope: HitlRequest['scope'];
		requested_at: number;
		resolved_at: number;
		outcome: string;
		decision: string | null;
	}

	let activeCategory: AttentionCategory = 'all';
	$: primaryAgentName = agentDisplayName($primaryAgent);
	let search = '';
	let viewMode: AttentionViewMode = 'pending';
	let resolvedRows: ResolvedRow[] = [];
	let resolvedLoading = false;
	let resolvedError: string | null = null;
	let resolvedFetchedForScope: string | null = null;

	// History is a plain in-memory snapshot (one `backfill_only` fetch), so it
	// pages client-side over the already-filtered array — no cursor involved.
	const RESOLVED_PAGE_SIZE = 20;
	let resolvedPageIndex = 0;

	$: resolvedFilteredRows = resolvedRows.filter((row) => {
		if (activeCategory !== 'all' && attentionSourceCategory(row.source) !== activeCategory) {
			return false;
		}
		const needle = search.trim().toLowerCase();
		if (!needle) return true;
		return `${row.prompt} ${row.hint ?? ''} ${row.correlation_id}`
			.toLowerCase()
			.includes(needle);
	});
	$: resolvedPageCount = Math.max(
		1,
		Math.ceil(resolvedFilteredRows.length / RESOLVED_PAGE_SIZE)
	);
	// A narrowed filter or a fresh fetch can strand the page past the end.
	$: if (resolvedPageIndex > resolvedPageCount - 1) {
		resolvedPageIndex = resolvedPageCount - 1;
	}
	$: resolvedPagedRows = attentionPageRows(
		resolvedFilteredRows,
		resolvedPageIndex,
		RESOLVED_PAGE_SIZE
	);
	$: resolvedPageStart =
		resolvedPagedRows.length === 0 ? 0 : resolvedPageIndex * RESOLVED_PAGE_SIZE + 1;
	$: resolvedPageEnd =
		resolvedPagedRows.length === 0
			? 0
			: resolvedPageIndex * RESOLVED_PAGE_SIZE + resolvedPagedRows.length;

	$: scopeKey = `${$scopeIdentityStore?.principal ?? ''}:${$scopeIdentityStore?.workspace ?? ''}`;
	$: if (viewMode === 'all' && scopeKey && scopeKey !== resolvedFetchedForScope) {
		void fetchResolved();
	}

	async function openApplicationPath(path: string): Promise<void> {
		if (browser && new URL(window.location.href).searchParams.get('native_attention') === '1') {
			try {
				const { invoke } = await import('@tauri-apps/api/core');
				await invoke('open_app_at', { path });
				return;
			} catch {
				// Browser visual-debug sessions fall through to normal navigation.
			}
		}
		await goto(path);
	}

	function openMeetingTranscript(threadId: string | null | undefined): void {
		if (!threadId) return;
		void openApplicationPath(`/t/${encodeURIComponent(threadId)}`);
	}

	async function fetchResolved(): Promise<void> {
		const scope = get(scopeIdentityStore);
		if (!scope?.principal || !scope?.workspace) return;
		resolvedLoading = true;
		resolvedError = null;
		const currentScopeKey = `${scope.principal}:${scope.workspace}`;
		try {
			const params = new URLSearchParams();
			params.set('category', 'hitl');
			params.set('limit', '300');
			// History snapshot only — skip the live-tail phase so the
			// response body ends after backfill drains and
			// `await response.text()` resolves. Without this, the
			// backend's live tail holds the body open forever and
			// `timedFetch` aborts at its 30s default.
			params.set('backfill_only', 'true');
			const response = await timedFetch(
				`/api/magician/v3/events?${params.toString()}`,
				{
				}
			);
			if (!response.ok) {
				throw new Error(`HTTP ${response.status}`);
			}
			const text = await response.text();
			resolvedRows = parseResolvedRows(text);
			resolvedPageIndex = 0;
			resolvedFetchedForScope = currentScopeKey;
		} catch (err) {
			resolvedError = err instanceof Error ? err.message : 'failed to load history';
		} finally {
			if (resolvedFetchedForScope === currentScopeKey || resolvedFetchedForScope === null) {
				resolvedLoading = false;
			}
		}
	}

	/**
	 * Parse the NDJSON event stream into resolved (HitlRequested →
	 * HitlResolved) pairs, keyed by correlation_id. Unmatched
	 * HitlRequested rows are pending — they're already in
	 * `pendingHitlStore` so we drop them here to avoid duplication.
	 */
	function parseResolvedRows(ndjson: string): ResolvedRow[] {
		const requests = new Map<string, HitlRequest>();
		const requestedAt = new Map<string, number>();
		const resolved: Array<{ cid: string; outcome: string; decision: string | null; at: number }> = [];
		for (const line of ndjson.split('\n')) {
			const trimmed = line.trim();
			if (!trimmed) continue;
			let parsed: Record<string, unknown>;
			try {
				parsed = JSON.parse(trimmed);
			} catch {
				continue;
			}
			const eventType = String(parsed.event_type ?? '');
			if (eventType === 'HitlRequested') {
				const request = hitlRequestFromCanonicalEvent(parsed);
				if (!request) continue;
				requests.set(request.identifiers.correlation_id ?? request.id, request);
				const ts = readEventTimestamp(parsed);
				requestedAt.set(request.identifiers.correlation_id ?? request.id, ts ?? Date.now());
			} else if (eventType === 'HitlResolved') {
				const data = (parsed.data as Record<string, unknown> | undefined) ?? parsed;
				const cid =
					(typeof data.correlation_id === 'string' && data.correlation_id) ||
					(typeof data.pause_state_id === 'string' && data.pause_state_id) ||
					(typeof data.approval_id === 'string' && data.approval_id) ||
					null;
				if (!cid) continue;
				const outcome = typeof data.outcome === 'string' ? data.outcome : 'responded';
				const decision = typeof data.decision === 'string' ? data.decision : null;
				const at = readEventTimestamp(parsed) ?? Date.now();
				resolved.push({ cid, outcome, decision, at });
			}
		}
		const out: ResolvedRow[] = [];
		for (const r of resolved) {
			const request = requests.get(r.cid);
			if (!request) continue; // resolved without matching request in window; skip
			out.push({
				correlation_id: r.cid,
				source: request.source,
				prompt: request.prompt,
				hint: request.hint,
				scope: request.scope,
				requested_at: requestedAt.get(r.cid) ?? r.at,
				resolved_at: r.at,
				outcome: r.outcome,
				decision: r.decision
			});
		}
		// Newest-first.
		out.sort((a, b) => b.resolved_at - a.resolved_at);
		return out;
	}

	function readEventTimestamp(raw: Record<string, unknown>): number | null {
		const data = raw.data as Record<string, unknown> | undefined;
		const candidates: unknown[] = [
			raw.timestamp_ms,
			raw.timestamp,
			data?.timestamp,
			data?.timestamp_ms
		];
		for (const v of candidates) {
			if (typeof v === 'number' && Number.isFinite(v)) return v;
		}
		return null;
	}

	// --- Live meeting capture (Meetings surface) ---------------------------
	// Active capture is an attention-grade fact: audio is being recorded RIGHT
	// NOW. The section is client-composed from the same /meetings/active poll
	// the TopBar dot uses, with stop/open-transcript inline so a forgotten
	// listener can be killed from the inbox without a page hop.
	let liveMeetings: ActiveMeetingSession[] = [];
	let meetingsTimer: ReturnType<typeof setInterval> | null = null;
	let stoppingMeetingIds = new Set<string>();

	// --- Inbox paging ------------------------------------------------------
	const ATTENTION_PAGE_SIZE = 20;
	let inboxInitialReady = false;

	async function pollMeetings(): Promise<void> {
		try {
			const res = await fetch('/api/magician/v2/meetings/active');
			if (!res.ok) return;
			const data = await res.json();
			liveMeetings = Array.isArray(data.active) ? data.active : [];
		} catch {
			// Backend unreachable — keep the last value.
		}
	}

	async function stopMeeting(sessionId: string): Promise<void> {
		stoppingMeetingIds = new Set(stoppingMeetingIds).add(sessionId);
		try {
			await fetch(`/api/magician/v2/meetings/${encodeURIComponent(sessionId)}/stop`, {
				method: 'POST'
			});
		} catch {
			// The follow-up poll re-shows it if the stop didn't land.
		} finally {
			const next = new Set(stoppingMeetingIds);
			next.delete(sessionId);
			stoppingMeetingIds = next;
			await pollMeetings();
		}
	}

	onMount(() => {
		// Start the feed-side polling + realtime bridge. The page drives the
		// store while the route is mounted.
		attentionStore.start();
		inboxScopeKey = scopeKey;
		void pollMeetings();
		meetingsTimer = setInterval(() => void pollMeetings(), 15000);
		void hydrateInbox();
	});
	onDestroy(() => {
		attentionStore.stop();
		if (meetingsTimer) clearInterval(meetingsTimer);
	});

	let inboxScopeKey = '';
	$: if (browser && inboxScopeKey && scopeKey !== inboxScopeKey) {
		inboxScopeKey = scopeKey;
		inboxPageIndex = 0;
		inboxPageGeneration += 1;
		inboxInitialReady = false;
		void hydrateInbox();
	}

	// `attentionStore` already strips dismissed failed items from its buckets, so
	// `$attentionRows` is the visible set.
	$: visibleRows = [...$attentionRows].sort((a, b) => b.at - a.at);
	$: diffApprovalRows = visibleRows.filter(
		(row) => !row.failed && row.request?.input_type === 'diff_approval'
	);
	$: attentionInitialLoading = !inboxInitialReady && !$attentionStore.error;
	$: attentionEmptyError =
		visibleRows.length === 0 && !!$attentionStore.error && !$attentionStore.isLoading;

	let inboxPageIndex = 0;
	let inboxPageLoading = false;
	let inboxPagingNotice: string | null = null;
	let inboxPageGeneration = 0;
	/**
	 * How many row actions the reader has started that have not yet been
	 * reconciled. It exists for a gap of a single frame: the store drops the
	 * resolved row synchronously, Svelte flushes before the awaited action
	 * returns, and for that flush the page looks empty with nothing in flight.
	 * The step-back clamp below would take that frame at face value and move the
	 * reader off a page that is about to refill.
	 */
	let inboxRemovalsInFlight = 0;

	$: categoryRows = filterAttentionRowsByCategory(visibleRows, activeCategory);
	$: filteredRows = filterAttentionRows(categoryRows, 'all', search);
	$: inboxSourceFrontiers = categorySourceFrontiers(activeCategory, $attentionStore);
	$: inboxProvenRowCount = attentionGloballyProvenRowCount(
		inboxSourceFrontiers,
		filteredRows.length
	);
	$: inboxProvenRows = filteredRows.slice(0, inboxProvenRowCount);
	$: pagedRows = inboxInitialReady
		? attentionPageRows(inboxProvenRows, inboxPageIndex, ATTENTION_PAGE_SIZE)
		: [];
	$: categoryCounts = attentionCategoryCountsFromTotals($attentionStore.totals);
	$: hasMoreFeedForCategory = attentionFeedLanesForCategory(activeCategory).some(
		(lane) => $attentionStore.pages[lane].has_more
	);
	$: inboxHasServerMore = hasMoreFeedForCategory;
	// A client-side search filters rows the server total knows nothing about, so
	// the total stops being a valid denominator — fall back to what is loaded.
	$: inboxSearchActive = search.trim().length > 0;
	$: inboxPager = attentionPagerView({
		pageIndex: inboxPageIndex,
		pageSize: ATTENTION_PAGE_SIZE,
		visibleRowCount: pagedRows.length,
		provenRowCount: inboxProvenRows.length,
		categoryTotal: inboxSearchActive ? 0 : categoryCounts[activeCategory] ?? 0,
		hasServerMore: inboxHasServerMore
	});
	/**
	 * Step back off a page the reader has emptied — the second half of the
	 * shared removal policy (`pageAfterRemoval`), as a standing rule rather than
	 * a call. Rows leave this inbox from the 15s poll and from other surfaces as
	 * well as from the reader's own click, and only the click has somewhere to
	 * run code, so the condition is the rendered page being empty.
	 *
	 * The only things that hold it off are the two ways this page can still be
	 * filled: a window load in flight, and a row action whose refill has not
	 * picked up yet.
	 *
	 * It deliberately does NOT wait for the cursor to be exhausted. That gate
	 * was the old shape of this rule and it meant the clamp could never fire
	 * while the server had more — which is precisely when a category whose
	 * remaining rows do not survive the filter strands the reader on an empty
	 * page for good.
	 *
	 * "Past the last page with rows" is spelled out of `inboxProvenRows` rather
	 * than read off `pagedRows`, which is the same statement — a page is empty
	 * exactly when its index is past that one — without making the slice this
	 * assignment feeds a dependency of the assignment itself.
	 */
	$: if (
		inboxInitialReady &&
		!inboxPageLoading &&
		inboxRemovalsInFlight === 0 &&
		inboxPageIndex > Math.max(0, Math.ceil(inboxProvenRows.length / ATTENTION_PAGE_SIZE) - 1)
	) {
		inboxPageIndex = Math.max(0, Math.ceil(inboxProvenRows.length / ATTENTION_PAGE_SIZE) - 1);
	}

	function categorySourceFrontiers(
		category: AttentionCategory,
		state = get(attentionStore)
	): AttentionSourceFrontier[] {
		return attentionFeedLanesForCategory(category).map((lane) => ({
			loadedCount: state[lane].length,
			hasMore: state.pages[lane].has_more,
			bufferLimit: Number.MAX_SAFE_INTEGER
		}));
	}

	function currentFilteredRows(): AttentionDisplayRow[] {
		const rows = [...get(attentionRows)].sort((a, b) => b.at - a.at);
		return filterAttentionRows(
			filterAttentionRowsByCategory(rows, activeCategory),
			'all',
			search
		);
	}

	function hasServerMoreForCategory(category: AttentionCategory): boolean {
		const state = get(attentionStore);
		return attentionFeedLanesForCategory(category).some((lane) => state.pages[lane].has_more);
	}

	function categoryFrontierSignature(category: AttentionCategory): string {
		const state = get(attentionStore);
		return attentionFeedLanesForCategory(category)
			.flatMap((lane) => [
				state[lane].length,
				state.pages[lane].next_cursor ?? '',
				state.pages[lane].has_more
			])
			.join(':');
	}

	async function loadRelevantCursorPage(category: AttentionCategory): Promise<boolean> {
		const before = categoryFrontierSignature(category);
		const state = get(attentionStore);
		if (!attentionFeedLanesForCategory(category).some((lane) => state.pages[lane].has_more)) {
			return false;
		}
		await attentionStore.loadMore();
		return categoryFrontierSignature(category) !== before;
	}

	async function ensureInboxWindow(
		requiredRows: number,
		category: AttentionCategory,
		generation: number,
		requestScopeKey: string
	): Promise<boolean> {
		let proofTarget = requiredRows;
		let attempts = 0;
		while (
			generation === inboxPageGeneration &&
			requestScopeKey === scopeKey &&
			attempts < 80
		) {
			const frontiers = categorySourceFrontiers(category);
			if (!attentionFrontiersProven(frontiers, proofTarget)) {
				if (!hasServerMoreForCategory(category)) return false;
				if (!(await loadRelevantCursorPage(category))) return false;
				attempts += 1;
				continue;
			}

			const rowCount = currentFilteredRows().length;
			if (rowCount >= requiredRows) return true;
			if (!hasServerMoreForCategory(category)) {
				return attentionFrontiersProven(frontiers, rowCount);
			}
			proofTarget += ATTENTION_PAGE_SIZE;
			attempts += 1;
		}
		return false;
	}

	async function hydrateInbox(): Promise<void> {
		const generation = ++inboxPageGeneration;
		const requestScopeKey = scopeKey;
		inboxInitialReady = false;
		await attentionStore.refresh();
		if (generation !== inboxPageGeneration || requestScopeKey !== scopeKey) return;
		await ensureInboxWindow(
			ATTENTION_PAGE_SIZE,
			activeCategory,
			generation,
			requestScopeKey
		);
		if (generation === inboxPageGeneration && requestScopeKey === scopeKey) {
			inboxInitialReady = true;
		}
	}

	async function selectInboxCategory(
		event: CustomEvent<{ category: AttentionCategory }>
	): Promise<void> {
		if (event.detail.category === activeCategory || inboxPageLoading) return;
		activeCategory = event.detail.category;
		inboxPageIndex = 0;
		inboxPagingNotice = null;
		const generation = ++inboxPageGeneration;
		const requestScopeKey = scopeKey;
		inboxPageLoading = true;
		try {
			await ensureInboxWindow(
				ATTENTION_PAGE_SIZE,
				activeCategory,
				generation,
				requestScopeKey
			);
		} catch (error) {
			if (generation === inboxPageGeneration && requestScopeKey === scopeKey) {
				inboxPagingNotice = error instanceof Error ? error.message : 'Attention category failed to load.';
			}
		} finally {
			if (generation === inboxPageGeneration && requestScopeKey === scopeKey) {
				inboxPageLoading = false;
			}
		}
	}

	/**
	 * Jump to any 1-based page of the active tab. Backwards is free — those rows
	 * are already buffered. Forwards drives the same cursor loader Next always
	 * used, just with a further target, so First/Last/arbitrary jumps cost only
	 * the pages between here and there.
	 */
	async function gotoInboxPage(targetPage: number): Promise<void> {
		const targetIndex = Math.max(0, Math.floor(targetPage) - 1);
		if (targetIndex === inboxPageIndex || inboxPageLoading) return;
		inboxPagingNotice = null;
		if (targetIndex < inboxPageIndex) {
			inboxPageIndex = targetIndex;
			return;
		}
		const requiredRows = (targetIndex + 1) * ATTENTION_PAGE_SIZE;
		const generation = ++inboxPageGeneration;
		const requestScopeKey = scopeKey;
		inboxPageLoading = true;
		try {
			await ensureInboxWindow(
				requiredRows,
				activeCategory,
				generation,
				requestScopeKey
			);
			if (generation !== inboxPageGeneration || requestScopeKey !== scopeKey) return;
			const provenRows = attentionGloballyProvenRowCount(
				categorySourceFrontiers(activeCategory),
				currentFilteredRows().length
			);
			const landedIndex = attentionJumpLandingIndex(
				inboxPageIndex,
				targetIndex,
				provenRows,
				ATTENTION_PAGE_SIZE
			);
			inboxPageIndex = landedIndex;
			if (landedIndex < targetIndex) {
				inboxPagingNotice = 'You have reached the end of this category.';
			}
		} catch (error) {
			if (generation === inboxPageGeneration && requestScopeKey === scopeKey) {
				inboxPagingNotice = error instanceof Error ? error.message : 'Attention page failed to load.';
			}
		} finally {
			if (generation === inboxPageGeneration && requestScopeKey === scopeKey) {
				inboxPageLoading = false;
			}
		}
	}

	/** How many rows the given 0-based page would render right now. */
	function inboxRowsOnPage(pageIndex: number): number {
		const rows = currentFilteredRows();
		const proven = attentionGloballyProvenRowCount(
			categorySourceFrontiers(activeCategory),
			rows.length
		);
		return attentionPageRows(rows.slice(0, proven), pageIndex, ATTENTION_PAGE_SIZE).length;
	}

	/**
	 * Put the reader's page back together after a row they acted on left it.
	 *
	 * Resolving and dismissing is what this page is FOR, and each one drops a
	 * row out of the buffer with nothing asking the server for a replacement —
	 * so a reader working page 3 of a 20-row page watched it drain toward empty
	 * and then bounce back a page. The shared policy is the fix: re-read the
	 * page you are on, which on this surface means extend the cursor window
	 * until that page is covered again, and step back only when the re-read
	 * comes back with nothing.
	 *
	 * The 15s feed poll re-reads the whole accumulated window and would have
	 * healed this within a tick — this does not race it. Both refresh and
	 * append are coalesced inside `attentionStore` (`inFlightRefresh` /
	 * `inFlightLoadMore`). What it adds over the poll is at most the one cursor
	 * append the poll would never have made.
	 */
	async function reconcileInboxAfterRemoval(): Promise<void> {
		if (!browser || inboxPageLoading) return;
		if (pagedRows.length >= ATTENTION_PAGE_SIZE) return;
		// The CURRENT generation, not a new one: a category switch or a scope
		// change must be able to invalidate this refill, and it has nothing of
		// its own to invalidate — `inboxPageLoading` already keeps it from
		// overlapping a jump.
		const generation = inboxPageGeneration;
		const requestScopeKey = scopeKey;
		const category = activeCategory;
		inboxPageLoading = true;
		try {
			const landing = await pageAfterRemoval(inboxPageIndex + 1, async (page) => {
				// Superseded: report a page that has rows, which is how the policy
				// is told to leave the reader where they are. The result is
				// discarded by the guard below in any case.
				if (generation !== inboxPageGeneration || requestScopeKey !== scopeKey) return 1;
				await ensureInboxWindow(
					page * ATTENTION_PAGE_SIZE,
					category,
					generation,
					requestScopeKey
				);
				if (generation !== inboxPageGeneration || requestScopeKey !== scopeKey) return 1;
				return inboxRowsOnPage(page - 1);
			});
			if (generation !== inboxPageGeneration || requestScopeKey !== scopeKey) return;
			inboxPageIndex = Math.max(0, landing - 1);
		} catch (error) {
			if (generation === inboxPageGeneration && requestScopeKey === scopeKey) {
				inboxPagingNotice =
					error instanceof Error ? error.message : 'Attention page failed to refill.';
			}
		} finally {
			if (generation === inboxPageGeneration && requestScopeKey === scopeKey) {
				inboxPageLoading = false;
			}
		}
	}

	const itemController = createAttentionItemController();
	const attentionActionState = itemController.state;
	let inboxFeedback: AttentionInboxFeedback[] = [];

	$: {
		const next: AttentionInboxFeedback[] = [];
		if ($attentionActionState.error) {
			next.push({ kind: 'error', message: $attentionActionState.error });
		}
		if ($attentionActionState.notice) {
			next.push({ message: $attentionActionState.notice });
		}
		if ($attentionStore.error && visibleRows.length > 0) {
			next.push({
				kind: 'error',
				message: `Attention feed unavailable: ${$attentionStore.error}`
			});
		}
		inboxFeedback = next;
	}

	/**
	 * The counter goes up BEFORE the action, not after it: the store drops the
	 * row and Svelte flushes while this is still awaiting, and that flush is the
	 * frame the clamp would otherwise read as "this page is empty and nothing is
	 * coming".
	 */
	async function activateRow(event: CustomEvent<{ row: AttentionDisplayRow }>): Promise<void> {
		inboxRemovalsInFlight += 1;
		try {
			const result = await itemController.activate(event.detail.row);
			// Only the outcomes that actually take a row out of the inbox — an
			// opened review or an external follow-up leaves the page as it was.
			if (result.status === 'resolved' || result.status === 'dismissed') {
				await reconcileInboxAfterRemoval();
			}
		} finally {
			inboxRemovalsInFlight -= 1;
		}
	}

	/** Approve every pending code change on screen, then refill what they left. */
	async function approveAllDiffs(): Promise<void> {
		inboxRemovalsInFlight += 1;
		try {
			await itemController.approveAllDiffApprovals(diffApprovalRows);
			await reconcileInboxAfterRemoval();
		} finally {
			inboxRemovalsInFlight -= 1;
		}
	}

	function runSkillAction(
		event: CustomEvent<{ row: AttentionDisplayRow; action: SkillEvolutionGateAction }>
	): void {
		void itemController.runSkillEvolutionGate(event.detail.row, event.detail.action);
	}

	function runRollbackDecision(
		event: CustomEvent<{
			row: AttentionDisplayRow;
			decision: SkillEvolutionRollbackDecision;
		}>
	): void {
		void itemController.runRollbackRecommendationDecision(
			event.detail.row,
			event.detail.decision
		);
	}

</script>

<svelte:head>
	<title>Attention · {PRODUCT_NAME}</title>
</svelte:head>

<div class="attention-page presto-gaui-page">
	<header class="attention-page__head">
		<div class="attention-page__title-block">
			<h1 class="attention-page__title">Attention</h1>
			<p class="attention-page__sub">
				{attentionInitialLoading
					? 'Loading attention items.'
					: attentionEmptyError
						? 'Attention feed unavailable.'
						: categoryCounts[activeCategory] === 0
					? 'Nothing waiting on you.'
					: `${categoryCounts[activeCategory]} item${categoryCounts[activeCategory] === 1 ? '' : 's'} need a decision`}
			</p>
		</div>

		<div class="attention-page__head-actions">
			{#if diffApprovalRows.length > 0}
				<button
					type="button"
					class="attention-page__bulk-btn"
					disabled={$attentionActionState.approvingAllDiffs}
					on:click={() => void approveAllDiffs()}
				>
					{$attentionActionState.approvingAllDiffs
						? 'Applying…'
						: `Approve all (${diffApprovalRows.length})`}
				</button>
			{/if}
			<div class="attention-page__view-toggle" role="group" aria-label="View">
				<button
					type="button"
					class="attention-page__view-btn"
					class:active={viewMode === 'pending'}
					on:click={() => (viewMode = 'pending')}
				>
					Pending
				</button>
				<button
					type="button"
					class="attention-page__view-btn"
					class:active={viewMode === 'all'}
					on:click={() => (viewMode = 'all')}
				>
					All
				</button>
			</div>

			<input
				type="search"
				class="attention-page__search"
				placeholder="Filter by prompt, task, agent…"
				bind:value={search}
			/>
		</div>
	</header>

	<AttentionCategoryTabs
		active={activeCategory}
		counts={categoryCounts}
		disabled={inboxPageLoading}
		on:change={selectInboxCategory}
	/>

	{#if inboxPager.pageCount > 1}
		<div class="attention-page__pager attention-page__pager--top">
			<ServerPager
				currentPage={inboxPager.currentPage}
				pageCount={inboxPager.pageCount}
				startItem={inboxPager.startItem}
				endItem={inboxPager.endItem}
				totalItems={inboxPager.totalItems}
				loading={inboxPageLoading}
				ariaLabel="Attention pages (top)"
				on:pagechange={(event) => void gotoInboxPage(event.detail.page)}
			/>
		</div>
	{/if}

	<AttentionInboxSurface
		rows={pagedRows}
		sourceFilter="all"
		search=""
		pageLimit={ATTENTION_PAGE_SIZE}
		initialLoading={attentionInitialLoading ||
			(pagedRows.length === 0 && inboxPageLoading)}
		emptyError={attentionEmptyError ? $attentionStore.error : null}
		feedback={inboxFeedback}
		hydratingKey={$attentionActionState.hydratingKey}
		skillEvolutionActionKey={$attentionActionState.skillEvolutionActionKey}
		rollbackActionKey={$attentionActionState.rollbackActionKey}
		showSearch={false}
		showFilters={false}
		skeletonRows={8}
		on:activate={activateRow}
		on:skillaction={runSkillAction}
		on:rollbackdecision={runRollbackDecision}
	>
		<svelte:fragment slot="before-list">
			{#if liveMeetings.length > 0 && activeCategory === 'all'}
				<section class="attention-page__meetings" aria-label="Live meeting capture">
					<h2 class="attention-page__meetings-title">
						<RecordingDot />
						Live meeting capture
					</h2>
					<ul class="attention-page__meetings-list">
						{#each liveMeetings as s (s.session_id)}
							<li class="attention-page__meeting-row">
								<div class="attention-page__meeting-info">
									<span class="attention-page__meeting-label">{meetingSessionLabel(s)}</span>
									<span class="attention-page__meeting-meta">
										{s.mode === 'attendee' ? `Joined as ${primaryAgentName}` : 'Passive listening'}
										· {s.status}{s.paused ? ' · paused' : ''}
									</span>
								</div>
								<div class="attention-page__meeting-actions">
									<button
										type="button"
										class="attention-page__meeting-btn"
										disabled={!s.thread_id}
										on:click={() => openMeetingTranscript(s.thread_id)}
									>Open transcript</button>
									<button
										type="button"
										class="attention-page__meeting-btn attention-page__meeting-btn--danger"
										disabled={stoppingMeetingIds.has(s.session_id)}
										on:click={() => void stopMeeting(s.session_id)}
									>{stoppingMeetingIds.has(s.session_id)
										? 'Stopping…'
										: s.mode === 'attendee'
											? 'Leave'
											: 'Stop'}</button>
								</div>
							</li>
						{/each}
					</ul>
				</section>
			{/if}
		</svelte:fragment>
	</AttentionInboxSurface>

	<!--
		Pagers appear together or not at all: both are gated on `pageCount > 1`,
		matching the top pager. A single-page category previously showed no pager
		on top and a dead "1 of 1" at the bottom. `inboxPagingNotice` is a LOAD
		ERROR, not a paging hint, so it stays visible independently — hiding it
		with the pager would silence category failures.
	-->
	{#if inboxPagingNotice || inboxPager.pageCount > 1}
		<div class="attention-page__pager">
			{#if inboxPagingNotice}
				<span class="attention-page__pager-notice" role="status">{inboxPagingNotice}</span>
			{/if}
			{#if inboxPager.pageCount > 1}
				<ServerPager
					currentPage={inboxPager.currentPage}
					pageCount={inboxPager.pageCount}
					startItem={inboxPager.startItem}
					endItem={inboxPager.endItem}
					totalItems={inboxPager.totalItems}
					loading={inboxPageLoading}
					ariaLabel="Attention pages (bottom)"
					on:pagechange={(event) => void gotoInboxPage(event.detail.page)}
				/>
			{/if}
		</div>
	{/if}

	{#if viewMode === 'all'}
		<section class="attention-page__history">
			<header class="attention-page__history-head">
				<h2 class="attention-page__history-title">Recently resolved</h2>
				{#if resolvedLoading}
					<span class="attention-page__history-meta">Loading…</span>
				{:else if resolvedError}
					<span class="attention-page__history-meta attention-page__history-meta--err" role="alert">
						{resolvedError}
					</span>
				{:else}
					<span class="attention-page__history-meta">
						{resolvedFilteredRows.length} item{resolvedFilteredRows.length === 1 ? '' : 's'}
					</span>
				{/if}
			</header>

			{#if resolvedPageCount > 1}
				<ServerPager
					currentPage={resolvedPageIndex + 1}
					pageCount={resolvedPageCount}
					startItem={resolvedPageStart}
					endItem={resolvedPageEnd}
					totalItems={resolvedFilteredRows.length}
					loading={resolvedLoading}
					ariaLabel="Resolved history pages (top)"
					on:pagechange={(event) => (resolvedPageIndex = event.detail.page - 1)}
				/>
			{/if}

			{#if !resolvedLoading && resolvedFilteredRows.length === 0 && !resolvedError}
				<div class="attention-page__empty attention-page__empty--quiet">
					<p>No recent history within the canonical event window.</p>
				</div>
			{:else if resolvedFilteredRows.length > 0}
				<ul class="attention-page__list">
					{#each resolvedPagedRows as row (row.correlation_id + '::' + row.resolved_at)}
						<li class="attention-page__row attention-page__row--resolved">
							<div class="attention-page__row-btn">
								<span
									class="attention-page__source"
									style={`--attention-row-color: ${ATTENTION_SOURCE_COLORS[row.source] ?? 'var(--text-secondary)'}`}
								>
									{ATTENTION_SOURCE_LABELS[row.source] ?? row.source}
								</span>

								<div class="attention-page__body">
									<div class="attention-page__prompt">{row.prompt}</div>
									<div class="attention-page__meta">
										<span class="attention-page__outcome attention-page__outcome--{row.outcome}">
											{row.outcome}{row.decision ? ` · ${row.decision}` : ''}
										</span>
										<span>·</span>
										<span>{attentionRelativeTime(row.resolved_at)}</span>
										<span>·</span>
										<span>took {Math.max(0, Math.round((row.resolved_at - row.requested_at) / 1000))}s</span>
									</div>
								</div>

								<span class="attention-page__cta attention-page__cta--quiet">resolved</span>
							</div>
						</li>
					{/each}
				</ul>
				{#if resolvedPageCount > 1}
					<div class="attention-page__pager">
						<ServerPager
							currentPage={resolvedPageIndex + 1}
							pageCount={resolvedPageCount}
							startItem={resolvedPageStart}
							endItem={resolvedPageEnd}
							totalItems={resolvedFilteredRows.length}
							loading={resolvedLoading}
							ariaLabel="Resolved history pages (bottom)"
							on:pagechange={(event) => (resolvedPageIndex = event.detail.page - 1)}
						/>
					</div>
				{/if}
			{/if}
		</section>
	{/if}
</div>

<style>
	.attention-page {
		max-width: 1320px;
		margin: 0 auto;
		padding: 2rem 1.5rem 4rem;
		display: flex;
		flex-direction: column;
		gap: 1.25rem;
	}

	.attention-page__head {
		display: flex;
		align-items: flex-end;
		gap: 1rem;
		justify-content: space-between;
		flex-wrap: wrap;
	}

	.attention-page__title {
		font-size: 1.75rem;
		font-weight: 600;
		margin: 0;
		color: var(--text-primary);
	}

	.attention-page__sub {
		margin: 0.25rem 0 0;
		font-size: var(--text-sm, 0.85rem);
		color: var(--text-muted);
	}

	.attention-page__search {
		min-width: 240px;
		padding: 0.5rem 0.85rem;
		border-radius: var(--radius-md, 8px);
		border: 1px solid var(--border-soft);
		background: var(--bg-card);
		color: var(--text-primary);
		font-size: var(--text-md, 0.95rem);
	}

	.attention-page__search:focus {
		outline: none;
		border-color: var(--accent-primary);
		box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent-primary) 18%, transparent);
	}

	.attention-page__pager {
		display: flex;
		align-items: center;
		justify-content: flex-end;
		gap: 14px;
		min-height: 40px;
	}

	/* Reads as a toolbar row hugging the category tabs rather than floating in
	   the page column's 1.25rem gap. */
	.attention-page__pager--top {
		min-height: 0;
		margin-top: -0.75rem;
		margin-bottom: -0.55rem;
	}

	.attention-page__pager-notice {
		min-width: 0;
		color: var(--text-secondary, var(--text-primary));
		font-size: var(--text-sm, 0.85rem);
		text-align: right;
	}

	.attention-page__empty {
		padding: 3rem 1rem;
		text-align: center;
		color: var(--text-muted);
		border: 1px dashed var(--border-soft);
		border-radius: var(--radius-lg, 12px);
		background: var(--bg-card);
	}

	.attention-page__list {
		list-style: none;
		padding: 0;
		margin: 0;
		display: flex;
		flex-direction: column;
		gap: 0.55rem;
	}
	.attention-page__more {
		display: flex;
		justify-content: center;
		padding: 0.8rem 0 0.2rem;
	}

	.attention-page__row {
		position: relative;
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-md, 8px);
		overflow: hidden;
		transition: border-color 140ms ease, transform 140ms ease, box-shadow 140ms ease;
	}

	.attention-page__row:hover {
		border-color: color-mix(in srgb, var(--accent-primary) 35%, var(--border-soft));
		transform: translateY(-1px);
		box-shadow: 0 4px 12px color-mix(in srgb, var(--text-primary) 8%, transparent);
	}

	.attention-page__row-btn {
		display: grid;
		grid-template-columns: auto 1fr auto;
		align-items: center;
		gap: 1rem;
		width: 100%;
		padding: 0.85rem 1rem;
		background: transparent;
		border: none;
		text-align: left;
		cursor: pointer;
		color: inherit;
		font: inherit;
	}

	.attention-page__source {
		display: inline-flex;
		align-items: center;
		padding: 0.18rem 0.55rem;
		border-radius: 999px;
		border: 1px solid color-mix(in srgb, var(--attention-row-color) 40%, var(--border-soft));
		background: color-mix(in srgb, var(--attention-row-color) 14%, var(--bg-card));
		color: var(--text-primary, #2d3436);
		font-size: var(--text-2xs, 0.72rem);
		font-weight: 600;
		letter-spacing: 0;
		white-space: nowrap;
	}

	.attention-page__body {
		min-width: 0;
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
	}

	.attention-page__prompt {
		color: var(--text-primary);
		font-weight: 500;
		font-size: var(--text-md, 0.95rem);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.attention-page__hint {
		color: var(--text-muted);
		font-size: var(--text-sm, 0.85rem);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.attention-page__meta {
		display: flex;
		gap: 0.4rem;
		color: var(--text-muted);
		font-size: var(--text-xs, 0.78rem);
		opacity: 0.85;
	}

	.attention-page__cta {
		color: var(--accent-primary);
		font-size: var(--text-sm, 0.85rem);
		font-weight: 600;
		opacity: 0.75;
		transition: opacity 140ms ease, transform 140ms ease;
		white-space: nowrap;
	}

	.attention-page__row:hover .attention-page__cta {
		opacity: 1;
		transform: translateX(2px);
	}

	.attention-page__row-actions {
		display: flex;
		justify-content: flex-end;
		gap: 0.5rem;
		padding: 0 1rem 0.85rem;
	}

	.attention-page__action-btn {
		min-height: 32px;
		padding: 0.38rem 0.72rem;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-md, 8px);
		background: color-mix(in srgb, var(--bg-card) 92%, var(--accent-primary));
		color: var(--text-primary);
		font: inherit;
		font-size: var(--text-sm, 0.85rem);
		font-weight: 600;
		cursor: pointer;
		transition: transform 140ms ease, border-color 140ms ease, background 140ms ease;
	}

	.attention-page__action-btn--primary {
		border-color: color-mix(in srgb, var(--accent-primary) 45%, var(--border-soft));
		background: color-mix(in srgb, var(--accent-primary) 14%, var(--bg-card));
	}

	.attention-page__action-btn:hover:not(:disabled) {
		border-color: color-mix(in srgb, var(--accent-primary) 55%, var(--border-soft));
		transform: translateY(-1px);
	}

	.attention-page__action-btn:disabled {
		cursor: not-allowed;
		opacity: 0.55;
	}

	.attention-page__head-actions {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		flex-wrap: wrap;
	}

	.attention-page__bulk-btn {
		border: 1px solid color-mix(in srgb, var(--accent-primary) 55%, var(--border-soft));
		border-radius: var(--radius-md, 8px);
		background: var(--accent-primary);
		color: var(--button-primary-color, #fff);
		padding: 0.45rem 0.8rem;
		font-size: var(--text-sm, 0.85rem);
		font-weight: 700;
		cursor: pointer;
		transition: opacity 120ms ease, transform 120ms ease;
	}

	.attention-page__bulk-btn:not(:disabled):hover {
		transform: translateY(-1px);
	}

	.attention-page__bulk-btn:disabled {
		cursor: not-allowed;
		opacity: 0.6;
	}

	.attention-page__view-toggle {
		display: inline-flex;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-md, 8px);
		overflow: hidden;
		background: var(--bg-card);
	}

	.attention-page__view-btn {
		background: transparent;
		border: none;
		padding: 0.45rem 0.9rem;
		font-size: var(--text-sm, 0.85rem);
		color: var(--text-muted);
		cursor: pointer;
		transition: background 120ms ease, color 120ms ease;
	}

	.attention-page__view-btn + .attention-page__view-btn {
		border-left: 1px solid var(--border-soft);
	}

	.attention-page__view-btn:hover {
		color: var(--text-primary);
		background: var(--bg-soft);
	}

	.attention-page__view-btn.active {
		background: var(--accent-primary-soft, color-mix(in srgb, var(--accent-primary) 14%, transparent));
		color: var(--text-primary);
		box-shadow: inset 0 -2px 0 0 var(--accent-primary);
	}

	.attention-page__history {
		margin-top: 1.5rem;
		display: flex;
		flex-direction: column;
		gap: 0.6rem;
	}

	.attention-page__history-head {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		gap: 0.75rem;
		padding: 0.25rem 0;
		border-bottom: 1px solid var(--border-soft);
	}

	.attention-page__history-title {
		font-size: var(--text-sm, 0.85rem);
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.08em;
		color: var(--text-muted);
		margin: 0;
	}

	.attention-page__history-meta {
		font-size: var(--text-xs, 0.78rem);
		color: var(--text-muted);
	}

	.attention-page__history-meta--err {
		color: var(--color-error, #d23a3a);
	}

	.attention-page__empty--quiet {
		padding: 1.25rem 1rem;
		border: 1px dashed var(--border-soft);
		opacity: 0.7;
	}

	.attention-page__empty--error {
		border-color: color-mix(in srgb, var(--color-error, #d23a3a) 35%, transparent);
		color: var(--color-error, #d23a3a);
		background: color-mix(in srgb, var(--color-error, #d23a3a) 6%, var(--bg-card));
		opacity: 1;
	}

	.attention-page__row--resolved {
		opacity: 0.78;
	}

	.attention-page__row--resolved:hover {
		opacity: 1;
		transform: none;
		box-shadow: none;
		border-color: var(--border-soft);
	}

	.attention-page__outcome {
		font-weight: 600;
		text-transform: lowercase;
		font-size: var(--text-2xs, 0.72rem);
		padding: 0.05rem 0.4rem;
		border-radius: var(--radius-sm, 4px);
		background: var(--bg-soft);
		color: var(--text-muted);
	}

	.attention-page__outcome--responded {
		background: color-mix(in srgb, var(--color-success, #00bb7f) 14%, transparent);
		color: var(--color-success, #00bb7f);
	}

	.attention-page__outcome--expired {
		background: color-mix(in srgb, var(--color-warning, #d28b1a) 14%, transparent);
		color: var(--color-warning, #d28b1a);
	}

	.attention-page__outcome--cancelled,
	.attention-page__outcome--dismissed {
		background: color-mix(in srgb, var(--text-muted) 14%, transparent);
		color: var(--text-muted);
	}

	.attention-page__cta--quiet {
		color: var(--text-muted);
		opacity: 0.6;
	}

	.attention-page__meetings {
		border: 1px solid color-mix(in srgb, var(--color-error, #d23a3a) 35%, transparent);
		border-radius: var(--radius-lg, 12px);
		padding: 0.75rem 0.9rem;
		background: color-mix(in srgb, var(--color-error, #d23a3a) 5%, transparent);
	}

	.attention-page__meetings-title {
		margin: 0 0 0.5rem;
		font-size: var(--text-sm, 0.85rem);
		font-weight: 600;
		display: flex;
		align-items: center;
		gap: 0.45rem;
	}

	.attention-page__meetings-list {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 0.4rem;
	}

	.attention-page__meeting-row {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
	}

	.attention-page__meeting-info {
		display: flex;
		flex-direction: column;
		gap: 0.1rem;
		min-width: 0;
	}

	.attention-page__meeting-label {
		font-size: var(--text-sm, 0.85rem);
		font-weight: 600;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.attention-page__meeting-meta {
		font-size: var(--text-2xs, 0.72rem);
		color: var(--text-muted);
	}

	.attention-page__meeting-actions {
		display: flex;
		gap: 0.4rem;
		flex-shrink: 0;
	}

	.attention-page__meeting-btn {
		padding: 0.3rem 0.65rem;
		border-radius: var(--radius-md, 8px);
		border: 1px solid var(--border-subtle, rgba(0, 0, 0, 0.12));
		background: var(--bg-base, #fff);
		color: var(--text-primary);
		font-size: var(--text-xs, 0.78rem);
		cursor: pointer;
	}

	.attention-page__meeting-btn:disabled {
		opacity: 0.55;
		cursor: default;
	}

	.attention-page__meeting-btn--danger {
		border-color: var(--color-error, #d23a3a);
		color: var(--color-error, #d23a3a);
		background: color-mix(in srgb, var(--color-error, #d23a3a) 10%, transparent);
	}
</style>
