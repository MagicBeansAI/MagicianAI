<!--
  /thinking-maps/[map_id] — the Live Thinking Map detail surface, now a full
  INTERACTIVE BRAINSTORM.

  Flow (all owner-authored, server-authoritative):
    · Capture      — the BrainstormComposer builds an owner `add_node` op (fresh
                     uuid node, chosen kind, `owner_spoken`, parented under the
                     selected node) and applies it via `applyOperations`.
    · Interpret    — Continue / Break-open call `interpret(id, { text, intent })`,
                     folding an utterance into the map (adds provisional nodes).
    · Clarify      — open clarifications surface in ClarificationPanel; Answer /
                     Dismiss apply a `resolve_clarification` op.
    · Restructure  — Reorganize calls `consolidate(id)` (stages a proposal);
                     ProposalCard confirms / rejects it via `decideProposal`.

  Live update: instead of a one-shot `getMap`, a per-map `createSharedPoll` over
  `getMap(id)` drives the whole surface (canvas + inspector + panels). It idles
  at 4s and holds a fast lease (~1.2s) while the page is mounted; every mutation
  calls `pollNow()` so the map reflects new/AI nodes quickly. The latest polled
  `map.revision` feeds the next op's `base_revision`; a `revision_conflict` just
  means the next poll's fresher revision lets the user retry. No optimistic UI —
  the server is authoritative.

  Time Loom (replay): the collapsible TimeLoom panel under the canvas scrubs
  the map's append-only event log. While scrubbed away from "now" the page is
  in HISTORY MODE — `historyState` (seq/rev + the replayed map) drives the
  canvas read-only, a banner marks the viewed point, every mutation control is
  hidden/disabled, drags are ignored, and the inspector reveals the selected
  node's causal `source_refs`. "Back to now" (banner or panel) exits;
  "Restore as branch" forks a new map at the scrubbed sequence and navigates
  to it.
-->
<script lang="ts">
	import { onDestroy } from 'svelte';
	import { get } from 'svelte/store';
	import { page } from '$app/stores';
	import { goto } from '$app/navigation';
	import {
		getMap,
		applyOperations,
		interpret,
		consolidate,
		decideProposal,
		attachSession,
		detachSession,
		listChatSessions,
		promoteNode,
		registerTutorContext,
		activeChatSessionId,
		type ChatSessionSummary,
		type PromoteTarget
	} from '$lib/thinkingMaps/api';
	import { ensureMediaSessionStarted } from '$lib/media/session';
	import { mediaSessionStore } from '$lib/media/store';
	import {
		startVoiceCall,
		stopVoiceCall,
		voiceCallStore,
		voiceTranscriptStore
	} from '$lib/media/voice/realtimeVoiceClient';
	import type {
		ApplyOutcome,
		Clarification,
		ClarificationState,
		InterpretIntent,
		MapOperation,
		PromotedRef,
		RestructureProposal,
		ThinkingMap,
		ThinkingNode
	} from '$lib/types/thinkingMap';
	import { createSharedPoll, type SharedPoll } from '$lib/stores/sharedPoll';
	import { v2Events, getV2EventSequence } from '$lib/realtime/v2-websocket';
	import {
		interpretProgressApplies,
		interpretStageLine,
		parseInterpretStage,
		type InterpretStageLine
	} from '$lib/thinkingMaps/interpretProgress';
	import type { HistoryView } from '$lib/thinkingMaps/timeLoom';
	import ThinkingMapCanvas from '$lib/thinkingMaps/ThinkingMapCanvas.svelte';
	import BrainstormComposer from '$lib/thinkingMaps/BrainstormComposer.svelte';
	import ClarificationPanel from '$lib/thinkingMaps/ClarificationPanel.svelte';
	import ProposalCard from '$lib/thinkingMaps/ProposalCard.svelte';
	import TimeLoom from '$lib/thinkingMaps/TimeLoom.svelte';
	import Button from '$lib/magician/components/native/Button.svelte';
	import Badge from '$lib/magician/components/native/Badge.svelte';
	import Spinner from '$lib/magician/components/native/Spinner.svelte';

	$: mapId = $page.params.map_id ?? '';

	let map: ThinkingMap | null = null;
	/** True only until the FIRST poll lands for the current id. */
	let isLoading = true;
	let loadError: string | null = null;

	let selectedNodeId = '';
	/** One shared lock so every mutation button freezes together. */
	let actionBusy = false;
	let actionError: string | null = null;

	// ── Time Loom (history mode) ─────────────────────────────────────────────────
	//
	// While the Time Loom is scrubbed away from "now", `historyState` carries the
	// viewed sequence/revision plus the replayed map once fetched (`map: null`
	// while the debounced replay request is in flight). History mode is strictly
	// READ-ONLY: the canvas renders the historical map, every mutation control is
	// disabled/hidden, and drags are ignored. The live poll keeps running so the
	// then/now comparison and "Back to now" always land on a fresh present.
	let historyState: HistoryView | null = null;
	let timeLoom: TimeLoom | null = null;

	/** What the canvas + inspector show: the replayed map in history mode
	 *  (falling back to the live map while the replay is still loading). */
	$: displayMap = historyState?.map ?? map;

	// ── Live poll (per-map instance; rebuilt when the id changes) ────────────────
	let poll: SharedPoll<ThinkingMap> | null = null;
	let unsubValue: (() => void) | null = null;
	let releaseFast: (() => void) | null = null;

	function teardownPoll(): void {
		unsubValue?.();
		unsubValue = null;
		releaseFast?.();
		releaseFast = null;
		poll = null;
	}

	function startPoll(id: string): void {
		teardownPoll();
		isLoading = true;
		loadError = null;
		map = null;

		const p = createSharedPoll<ThinkingMap>({
			fetcher: () => getMap(id),
			idleMs: 4_000,
			// While the page is open we hold a fast lease for snappy reflection.
			fastMs: 1_200
		});
		poll = p;

		// `.value` is a Svelte readable — subscribe manually so we can null it
		// out and re-subscribe cleanly when the id changes.
		unsubValue = p.value.subscribe((v) => {
			if (v) {
				map = v;
				loadError = null;
				isLoading = false;
			}
		});

		// A dead backend keeps `.value` at null; surface a load error once we've
		// waited past the first idle interval with nothing.
		void getMap(id).catch((err) => {
			if (!map) {
				loadError = err instanceof Error ? err.message : String(err);
				isLoading = false;
			}
		});

		releaseFast = p.requestFast();
	}

	function pollNow(): void {
		poll?.pollNow();
	}

	// (Re)start the poll whenever the map id changes.
	let lastId = '';
	$: if (mapId && mapId !== lastId) {
		lastId = mapId;
		selectedNodeId = '';
		actionError = null;
		// A new map means a new timeline — leave history mode (the TimeLoom
		// itself remounts via {#key mapId}).
		historyState = null;
		// Leaving one map for another while listening: detach from the OLD map.
		if (attachedTo) void stopListen();
		if (attachedConversations.length > 0) void detachAllConversations();
		showConversationPicker = false;
		conversationError = null;
		listenError = null;
		// The strip narrates a run on the PREVIOUS map. Its settle handler is
		// ownership-guarded, so clearing here cannot be undone by it — and a new
		// board opening under the old board's "Exploring…" reads as this map
		// already thinking.
		inFlightUtteranceId = null;
		progressLine = null;
		startPoll(mapId);
	}

	onDestroy(() => {
		teardownPoll();
		unsubEvents();
		clearTimeout(promoteSuccessTimer);
		// Best-effort: leaving the page ends Listen mode (detach + hang up) so the
		// server stops auto-mapping and the mic is released — and detaches any
		// conversations this page attached.
		if (attachedTo || isListening || listenActivating) void stopListen();
		if (attachedConversations.length > 0) void detachAllConversations();
	});

	// ── Realtime push: ThinkingMapUpdated → immediate re-poll ────────────────────
	//
	// The backend emits a lightweight `ThinkingMapUpdated` change notice on every
	// applied envelope (any client, any surface, the ambient coordinator). It's a
	// poll ACCELERATOR, not a data channel: on a notice for THIS map we `pollNow`
	// and let the authoritative `getMap` fetch do the work — a missed notice
	// (reconnect, dropped frame) costs one poll interval, never correctness.
	//
	// `ThinkingMapInterpretProgress` rides the same subscription: the server
	// narrates each stage of an owner-triggered `/interpret`, and the strip under
	// the composer follows along. Best-effort exactly like the notice — a missed
	// stage costs a line, never correctness.
	let inFlightUtteranceId: string | null = null;
	let progressLine: InterpretStageLine | null = null;
	let lastSeenEventSeq = 0;
	const unsubEvents = v2Events.subscribe((events) => {
		for (const event of events) {
			const seq = getV2EventSequence(event);
			if (seq <= lastSeenEventSeq) continue;
			lastSeenEventSeq = seq;
			if (event.event_type === 'ThinkingMapUpdated' && event.data.map_id === mapId) {
				pollNow();
			}
			if (
				event.event_type === 'ThinkingMapInterpretProgress' &&
				interpretProgressApplies(event.data, mapId, inFlightUtteranceId)
			) {
				const stage = parseInterpretStage(event.data.stage);
				if (stage) progressLine = interpretStageLine(stage, event.data.node_count ?? null);
			}
		}
	});

	// ── Ambient "Listen" mode (speak → the map builds itself) ────────────────────
	//
	// Starts a hands-free realtime voice call and attaches the tab's MEDIA session
	// id to this map (`POST /{id}/sessions`). The server-side ambient coordinator
	// matches each finalized spoken user turn via the message's
	// `presence_session_id` (= that media session id — the voice orchestrator's
	// chat session id is server-derived and never visible here) and auto-maps it.
	// The existing fast poll (~1.2s lease) surfaces the new nodes; each finished
	// user turn also triggers an immediate `pollNow`.

	let isListening = false;
	let listenActivating = false;
	let listenError: string | null = null;
	/** What we attached server-side (for detach — survives a mapId change). */
	let attachedTo: { mapId: string; sessionId: string } | null = null;

	/** Resolve once the call reaches `connected` (true) or errors/times out (false). */
	function waitForCallLive(timeoutMs = 20_000): Promise<boolean> {
		return new Promise((resolve) => {
			let settled = false;
			const done = (ok: boolean): void => {
				if (settled) return;
				settled = true;
				clearTimeout(timer);
				unsub();
				resolve(ok);
			};
			const timer = setTimeout(() => done(false), timeoutMs);
			const unsub = voiceCallStore.subscribe((s) => {
				if (s.state === 'connected') done(true);
				else if (s.state === 'error') done(false);
			});
		});
	}

	async function startListen(): Promise<void> {
		if (isListening || listenActivating || !mapId) return;
		listenActivating = true;
		listenError = null;
		const forMap = mapId;
		try {
			// The WHOLE start sequence is bounded by one timeout: `startVoiceCall`
			// itself can stall indefinitely (mic acquisition, control-WS handshake,
			// the server's provider connect), so guarding only the post-resolve
			// wait would leave the button stuck on "Starting…" forever.
			const live = await Promise.race([
				(async () => {
					// The shell registers the tab's media session at boot; this is
					// an idempotent safety net (deduped in-flight).
					await ensureMediaSessionStarted({});
					await startVoiceCall({ threadId: `thinking-map-${forMap}`, mode: 'hands_free' });
					return waitForCallLive();
				})(),
				new Promise<false>((resolve) => setTimeout(() => resolve(false), 25_000))
			]);
			if (!live) {
				throw new Error(get(voiceCallStore).error ?? "The voice call didn't start.");
			}
			const sessionId = get(mediaSessionStore).session?.session_id;
			if (!sessionId) throw new Error('No media session is registered.');
			await attachSession(forMap, sessionId);
			attachedTo = { mapId: forMap, sessionId };
			isListening = true;
		} catch (err) {
			listenError = err instanceof Error ? err.message : String(err);
			// Aborts a still-pending start too (generation bump + abort signal).
			stopVoiceCall();
		} finally {
			listenActivating = false;
		}
	}

	async function stopListen(): Promise<void> {
		const attached = attachedTo;
		attachedTo = null;
		isListening = false;
		listenActivating = false;
		stopVoiceCall();
		if (attached) {
			// Idempotent server-side; a failed detach just means the binding dies
			// with the voice session (no more turns arrive for it anyway).
			try {
				await detachSession(attached.mapId, attached.sessionId);
			} catch {
				/* best-effort */
			}
		}
	}

	function toggleListen(): void {
		if (isListening || listenActivating) void stopListen();
		else void startListen();
	}

	// The call dropped underneath us (network, ended elsewhere) → leave Listen
	// mode honestly. `stopListen` clears `isListening` so this doesn't loop.
	$: if (isListening && ($voiceCallStore.state === 'error' || $voiceCallStore.state === 'idle')) {
		listenError = $voiceCallStore.error ?? 'The voice call ended.';
		void stopListen();
	}

	// Nudge the poll the moment a spoken user turn finalizes — the coordinator
	// needs a beat to interpret + apply, and the 1.2s fast lease catches the rest.
	let seenDoneUserTurns = 0;
	$: {
		const doneTurns = $voiceTranscriptStore.turns.filter(
			(t) => t.speaker === 'user' && t.done
		).length;
		if (isListening && doneTurns > seenDoneUserTurns) pollNow();
		seenDoneUserTurns = doneTurns;
	}

	/** The latest (possibly partial) spoken user text, for the live caption. */
	$: liveCaption = (() => {
		const turns = $voiceTranscriptStore.turns;
		for (let i = turns.length - 1; i >= 0; i--) {
			if (turns[i].speaker === 'user' && turns[i].text.trim()) return turns[i].text.trim();
		}
		return '';
	})();

	// ── "Attach a conversation" — map a meeting/chat thread onto this board ──────
	//
	// The ambient coordinator maps a conversation's turns (user turns + the
	// meeting bot's transcript lines) once its CHAT SESSION id is attached. This
	// picker lists recent conversations and attaches the chosen one; the page's
	// push subscription + poll surface the auto-mapped nodes. NOTE the server
	// registry is in-memory — attachments do not survive a backend restart, so
	// the chips here are session-local state, not durable configuration.

	let showConversationPicker = false;
	let conversationChoices: ChatSessionSummary[] = [];
	let conversationFilter = '';
	let conversationsLoading = false;
	let conversationError: string | null = null;
	/** Conversations attached from THIS page. Each chip remembers the map it
	 *  was attached to so cleanup still works after a map switch. */
	let attachedConversations: { id: string; label: string; mapId: string }[] = [];

	function conversationLabel(s: ChatSessionSummary): string {
		return (s.title?.trim() || s.ui_thread_id?.trim() || s.id).slice(0, 60);
	}

	$: filteredConversations = conversationFilter.trim()
		? conversationChoices.filter((s) =>
				conversationLabel(s).toLowerCase().includes(conversationFilter.trim().toLowerCase())
			)
		: conversationChoices;

	async function openConversationPicker(): Promise<void> {
		showConversationPicker = !showConversationPicker;
		if (!showConversationPicker || conversationChoices.length > 0) return;
		conversationsLoading = true;
		conversationError = null;
		try {
			conversationChoices = (await listChatSessions()).slice(0, 30);
		} catch (err) {
			conversationError = err instanceof Error ? err.message : String(err);
		} finally {
			conversationsLoading = false;
		}
	}

	async function attachConversation(s: ChatSessionSummary): Promise<void> {
		if (!mapId || attachedConversations.some((a) => a.id === s.id)) return;
		try {
			await attachSession(mapId, s.id);
			attachedConversations = [
				...attachedConversations,
				{ id: s.id, label: conversationLabel(s), mapId }
			];
			showConversationPicker = false;
			pollNow();
		} catch (err) {
			conversationError = err instanceof Error ? err.message : String(err);
		}
	}

	async function detachConversation(id: string): Promise<void> {
		const chip = attachedConversations.find((a) => a.id === id);
		attachedConversations = attachedConversations.filter((a) => a.id !== id);
		if (!chip) return;
		try {
			await detachSession(chip.mapId, id);
		} catch {
			/* idempotent server-side */
		}
	}

	// Leaving the page (or switching maps) detaches this page's conversations —
	// mirrors Listen-mode cleanup so nothing keeps auto-mapping unattended.
	async function detachAllConversations(): Promise<void> {
		const attached = attachedConversations;
		attachedConversations = [];
		for (const a of attached) {
			try {
				await detachSession(a.mapId, a.id);
			} catch {
				/* best-effort */
			}
		}
	}

	// ── Mutation helpers (build envelopes off the latest polled revision) ────────
	function uuid(): string {
		if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') {
			return crypto.randomUUID();
		}
		return `k-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
	}

	/** Run a mutation under the shared lock, then refresh via the poll. */
	async function withBusy<T>(fn: () => Promise<T>): Promise<T> {
		actionBusy = true;
		actionError = null;
		try {
			return await fn();
		} finally {
			actionBusy = false;
			pollNow();
		}
	}

	/** Owner op batch — `base_revision` comes from the latest polled map. */
	async function applyOps(operations: MapOperation[]): Promise<void> {
		if (!mapId || !map) throw new Error('Map not loaded yet');
		await withBusy(() =>
			applyOperations(mapId, {
				operations,
				idempotency_key: uuid(),
				base_revision: map!.revision
			})
		);
	}

	/** Continue / Break-open. Returns 'empty' when the AI added nothing. */
	async function runInterpret(text: string, intent: InterpretIntent): Promise<'ok' | 'empty'> {
		if (!mapId) throw new Error('Map not loaded yet');
		// Minted here and sent with the request: the server narrates each stage
		// as a `ThinkingMapInterpretProgress` event tagged with this id, which is
		// how the strip claims narration for exactly this run — and ignores an
		// ambient auto-map or another tab thinking on the same board.
		const utteranceId = uuid();
		inFlightUtteranceId = utteranceId;
		progressLine = interpretStageLine('facilitating', null);
		try {
			const outcome = await withBusy<ApplyOutcome>(() =>
				interpret(mapId, {
					text,
					intent,
					utterance_id: utteranceId,
					// Interpretation focus is part of THIS request. Depending on a prior
					// shared-view mutation made fast clicks and multiple tabs fall back
					// to the map's older root selection.
					...(selectedNodeId ? { focus_node_id: selectedNodeId } : {})
				})
			);
			return outcome.outcome === 'no_operations' ? 'empty' : 'ok';
		} finally {
			// Settling is this response's job, not the realtime idle's — the
			// event can outrun the response body, and clearing early would show
			// "Ready" over a run still being applied. Ownership-guarded: if a
			// newer run has already claimed the strip, a stale settle must not
			// tear it down (today `actionBusy` serialises runs, but a guard
			// that only holds by way of a button's disabled state is luck).
			if (inFlightUtteranceId === utteranceId) {
				inFlightUtteranceId = null;
				progressLine = null;
			}
		}
	}

	/** Answer / Dismiss an open clarification. */
	async function resolveClarification(
		clarificationId: string,
		state: ClarificationState,
		answer: string | null
	): Promise<void> {
		if (!mapId || !map) throw new Error('Map not loaded yet');
		const op: MapOperation =
			answer === null
				? { op: 'resolve_clarification', clarification_id: clarificationId, state }
				: { op: 'resolve_clarification', clarification_id: clarificationId, state, answer };
		await withBusy(() =>
			applyOperations(mapId, {
				operations: [op],
				idempotency_key: uuid(),
				base_revision: map!.revision
			})
		);
	}

	/** Reorganize → stage a restructure proposal. */
	async function runConsolidate(): Promise<void> {
		if (!mapId || actionBusy) return;
		try {
			const outcome = await withBusy<ApplyOutcome>(() => consolidate(mapId));
			if (outcome.outcome === 'no_operations') {
				actionError = 'Already tidy — no restructure to suggest.';
			}
		} catch (err) {
			actionError = err instanceof Error ? err.message : String(err);
		}
	}

	/** Confirm / Reject a staged proposal. */
	async function decide(proposalId: string, decision: 'confirm' | 'reject'): Promise<void> {
		if (!mapId) throw new Error('Map not loaded yet');
		await withBusy(() => decideProposal(mapId, proposalId, decision));
	}

	// ── Promote (node → durable Task / Memory object) ────────────────────────────
	//
	// One `promoteNode` POST per click. A 409 `confirmation_required` (the node
	// isn't owner-asserted — AI-suggested / participant / imported) flips an
	// inline confirm affordance; retrying with `confirm: true` records an owner
	// assertion server-side and promotes. Success flashes the created object id
	// briefly; the poll then carries back the node's `promoted_refs`, which swaps
	// the button for a "→ task / memory" badge (idempotent server-side anyway).
	let promoteConfirm: PromoteTarget | null = null;
	let promoteError: string | null = null;
	let promoteSuccess: { target: PromoteTarget; objectId: string } | null = null;
	let promoteSuccessTimer: ReturnType<typeof setTimeout> | undefined;

	function resetPromoteState(): void {
		promoteConfirm = null;
		promoteError = null;
		promoteSuccess = null;
		clearTimeout(promoteSuccessTimer);
	}

	// Selecting a different node (or map) drops any in-flight confirm/flash.
	let promoteStateForNode = '';
	$: if (selectedNodeId !== promoteStateForNode) {
		promoteStateForNode = selectedNodeId;
		resetPromoteState();
		askTutorError = null;
	}

	// ── "Ask Tutor" (node → tutor grounding → /chat) ─────────────────────────────
	//
	// One click: resolve the user's ACTIVE chat session (`/chat/active`, the same
	// session the /chat page shows), register a tutor-context binding for the
	// selected node (`POST /{id}/tutor-context` — the next `@tutor` run in that
	// session receives this node's bounded map digest as background grounding),
	// then navigate to /chat. Server-side TTL; re-clicking simply re-registers.
	let askTutorBusy = false;
	let askTutorError: string | null = null;

	async function askTutorAboutNode(): Promise<void> {
		if (!mapId || !selectedNodeId || askTutorBusy) return;
		askTutorBusy = true;
		askTutorError = null;
		try {
			const sessionId = await activeChatSessionId('general');
			await registerTutorContext(mapId, sessionId, selectedNodeId);
			await goto('/chat');
		} catch (err) {
			askTutorError = err instanceof Error ? err.message : String(err);
		} finally {
			askTutorBusy = false;
		}
	}

	async function runPromote(target: PromoteTarget, confirm = false): Promise<void> {
		if (!mapId || !selectedNodeId || actionBusy) return;
		const nodeId = selectedNodeId;
		promoteError = null;
		try {
			const outcome = await withBusy(() => promoteNode(mapId, nodeId, target, confirm));
			promoteConfirm = null;
			promoteSuccess = { target, objectId: outcome.object_id };
			clearTimeout(promoteSuccessTimer);
			promoteSuccessTimer = setTimeout(() => (promoteSuccess = null), 6_000);
		} catch (err) {
			const message = err instanceof Error ? err.message : String(err);
			if (message === 'confirmation_required' && !confirm) {
				promoteConfirm = target;
			} else {
				promoteError = message;
			}
		}
	}

	/** The node's existing promotion link of `kind`, if any. */
	function promotedRefOf(node: ThinkingNode, kind: PromoteTarget): PromotedRef | undefined {
		return (node.promoted_refs ?? []).find((r) => r.destination_kind === kind);
	}

	function onSelectNode(e: CustomEvent<{ nodeId: string }>): void {
		selectedNodeId = e.detail.nodeId;
	}

	/**
	 * Persist a dragged node's new position. The canvas emits `moveNode` on drag
	 * END only; we build a `move_node` op off the LATEST polled revision so it
	 * doesn't fight the live poll (the next poll carries this same position back,
	 * and the canvas treats a positioned node as anchored). A revision_conflict
	 * just means a fresher poll will let the next drag land — no optimistic UI.
	 */
	async function onMoveNode(
		e: CustomEvent<{ node_id: string; x: number; y: number }>
	): Promise<void> {
		// History mode is read-only — a drag on the historical canvas must never
		// write a position back into the LIVE map.
		if (!mapId || !map || historyState) return;
		const { node_id, x, y } = e.detail;
		const op: MapOperation = { op: 'move_node', node_id, position: { x, y } };
		try {
			await withBusy(() =>
				applyOperations(mapId, {
					operations: [op],
					idempotency_key: uuid(),
					base_revision: map!.revision
				})
			);
		} catch (err) {
			actionError = err instanceof Error ? err.message : String(err);
		}
	}

	// ── Time Loom event handlers ─────────────────────────────────────────────────
	function onHistory(e: CustomEvent<HistoryView | null>): void {
		historyState = e.detail;
	}

	function onRestored(e: CustomEvent<{ mapId: string }>): void {
		historyState = null;
		void goto(`/thinking-maps/${encodeURIComponent(e.detail.mapId)}`);
	}

	// ── Derived views off the DISPLAYED map (historical in history mode) ─────────
	$: selectedNode =
		displayMap && selectedNodeId
			? (displayMap.nodes[selectedNodeId] as ThinkingNode | undefined)
			: undefined;

	$: openClarifications = map
		? (Object.values(map.clarifications) as Clarification[]).filter((c) => c.state === 'open')
		: [];

	$: firstProposal = map
		? (Object.values(map.proposals) as RestructureProposal[]).find((p) => p.state === 'proposed')
		: undefined;

	$: nodeLabels = map
		? Object.fromEntries(
				(Object.values(map.nodes) as ThinkingNode[]).map((n) => [n.node_id, n.label])
			)
		: {};

	function stateBadgeColor(
		state: string
	): 'success' | 'warning' | 'error' | 'info' | 'default' {
		switch (state) {
			case 'confirmed':
			case 'resolved':
				return 'success';
			case 'provisional':
			case 'asserted':
				return 'info';
			case 'contradicted':
			case 'rejected':
				return 'error';
			case 'superseded':
				return 'warning';
			default:
				return 'default';
		}
	}
</script>

<svelte:head>
	<title>{map?.title ? `${map.title} · Thinking Map` : 'Thinking Map'} · Magican</title>
</svelte:head>

<div class="tm-detail-shell">
	<header class="tm-detail-header">
		<div class="tm-detail-heading">
			<a class="tm-back" href="/thinking-maps">← Maps</a>
			<h1>{map?.title ?? (isLoading ? 'Loading…' : 'Thinking Map')}</h1>
			{#if map}
				<div class="tm-detail-meta">
					<Badge text={`rev ${map.revision}`} color="default" />
					<Badge text={map.lifecycle} color={map.lifecycle === 'active' ? 'success' : 'default'} />
					{#if actionBusy}
						<Spinner size="sm" label="Working…" />
					{/if}
				</div>
			{/if}
		</div>
		<div class="tm-detail-actions">
			<Button
				variant={attachedConversations.length > 0 ? 'primary' : 'outline'}
				size="sm"
				label={attachedConversations.length > 0
					? `Mapping ${attachedConversations.length} conversation${attachedConversations.length > 1 ? 's' : ''}`
					: '🗣 Attach conversation'}
				interactive={!!map && !historyState}
				on:click={() => void openConversationPicker()}
			/>
			<Button
				variant={isListening ? 'primary' : 'outline'}
				size="sm"
				label={listenActivating ? 'Starting…' : isListening ? 'Stop listening' : '🎙 Listen'}
				interactive={!!map && !listenActivating && !historyState}
				on:click={toggleListen}
			/>
			<Button
				variant="outline"
				size="sm"
				label={actionBusy ? 'Working…' : 'Reorganize'}
				interactive={!!map && !actionBusy && !historyState}
				on:click={runConsolidate}
			/>
		</div>
	</header>

	{#if isListening || listenActivating}
		<div class="tm-listen" role="status" aria-live="polite">
			<span class="tm-listen__dot" class:tm-listen__dot--connecting={!isListening} aria-hidden="true"
			></span>
			<span class="tm-listen__text">
				{#if !isListening}
					Starting live listening…
				{:else}
					{liveCaption || 'Listening — speak and the map builds itself.'}
				{/if}
			</span>
			<button class="tm-listen__stop" on:click={() => void stopListen()}>Stop</button>
		</div>
	{:else if listenError}
		<div class="tm-action-error" role="alert">
			<span>Couldn't listen: {listenError}</span>
			<button class="tm-action-error__x" title="Dismiss" on:click={() => (listenError = null)}
				>✕</button
			>
		</div>
	{/if}

	{#if showConversationPicker}
		<div class="tm-conversations" role="dialog" aria-label="Attach a conversation">
			<div class="tm-conversations__head">
				<input
					class="tm-conversations__filter"
					placeholder="Filter conversations…"
					bind:value={conversationFilter}
				/>
				<button
					class="tm-conversations__close"
					title="Close"
					on:click={() => (showConversationPicker = false)}>✕</button
				>
			</div>
			{#if conversationsLoading}
				<div class="tm-conversations__empty">Loading conversations…</div>
			{:else if conversationError}
				<div class="tm-conversations__empty">{conversationError}</div>
			{:else if filteredConversations.length === 0}
				<div class="tm-conversations__empty">No conversations found.</div>
			{:else}
				<ul class="tm-conversations__list">
					{#each filteredConversations as s (s.id)}
						<li>
							<button
								class="tm-conversations__row"
								disabled={attachedConversations.some((a) => a.id === s.id)}
								on:click={() => void attachConversation(s)}
							>
								<span class="tm-conversations__label">{conversationLabel(s)}</span>
								{#if s.updated_at}
									<span class="tm-conversations__when"
										>{new Date(s.updated_at).toLocaleDateString()}</span
									>
								{/if}
							</button>
						</li>
					{/each}
				</ul>
			{/if}
			<p class="tm-conversations__hint">
				New turns in an attached conversation (including a meeting bot's transcript) are
				auto-mapped onto this board until you detach or leave the page.
			</p>
		</div>
	{/if}

	{#if attachedConversations.length > 0}
		<div class="tm-conversation-chips">
			{#each attachedConversations as a (a.id)}
				<span class="tm-conversation-chip">
					<span class="tm-conversation-chip__dot" aria-hidden="true"></span>
					{a.label}
					<button
						class="tm-conversation-chip__x"
						title="Stop mapping this conversation"
						on:click={() => void detachConversation(a.id)}>✕</button
					>
				</span>
			{/each}
		</div>
	{/if}

	{#if actionError}
		<div class="tm-action-error" role="alert">
			<span>{actionError}</span>
			<button class="tm-action-error__x" title="Dismiss" on:click={() => (actionError = null)}
				>✕</button
			>
		</div>
	{/if}

	{#if loadError && !map}
		<div class="tm-error" role="alert">
			<span>Couldn't load this map: {loadError}</span>
			<Button
				variant="outline"
				size="sm"
				label="Retry"
				on:click={() => mapId && startPoll(mapId)}
			/>
		</div>
	{:else if isLoading && !map}
		<div class="tm-loading">
			<Spinner size="md" label="Loading map…" centered />
		</div>
	{:else if map}
		{@const shown = displayMap ?? map}
		<div class="tm-detail-body">
			<div class="tm-canvas-col">
				{#if historyState}
					<div class="tm-history-banner" role="status" aria-live="polite">
						<span class="tm-history-banner__text">
							⏪ Viewing history @seq {historyState.seq} · rev {historyState.revision}
							{historyState.map ? '' : '· loading…'} — read-only
						</span>
						<button class="tm-history-banner__back" on:click={() => timeLoom?.backToNow()}
							>Back to now</button
						>
					</div>
				{/if}

				<div class="tm-canvas-wrap">
					<ThinkingMapCanvas
						map={shown}
						readOnly={!!historyState}
						bind:selectedNodeId
						on:selectNode={onSelectNode}
						on:moveNode={onMoveNode}
					/>
				</div>

				{#key mapId}
					<TimeLoom
						bind:this={timeLoom}
						{mapId}
						{map}
						on:history={onHistory}
						on:restored={onRestored}
					/>
				{/key}

				{#if !historyState}
					{#if progressLine}
						<!-- What the facilitator is doing right now, narrated by the server
						     per pipeline stage. Present only while an interpret run this
						     page started is in flight. -->
						<div class="tm-interpret-progress" role="status" aria-live="polite">
							<Spinner size="sm" />
							<span class="tm-interpret-progress__label">{progressLine.label}</span>
							<span class="tm-interpret-progress__detail">{progressLine.detail}</span>
						</div>
					{/if}
					<BrainstormComposer
						{selectedNodeId}
						selectedNodeLabel={selectedNode?.label ?? ''}
						busy={actionBusy}
						{applyOps}
						{runInterpret}
						on:mutated={pollNow}
					/>
				{/if}
			</div>

			<aside class="tm-rail">
				{#if openClarifications.length > 0 && !historyState}
					<ClarificationPanel
						clarifications={openClarifications}
						{nodeLabels}
						busy={actionBusy}
						resolve={resolveClarification}
						on:mutated={pollNow}
					/>
				{/if}

				{#if firstProposal && !historyState}
					<ProposalCard proposal={firstProposal} busy={actionBusy} {decide} on:mutated={pollNow} />
				{/if}

				{#if selectedNode}
					<div class="tm-inspector">
						<div class="tm-inspector__head">
							<h2>Node</h2>
							<button
								class="tm-inspector__close"
								title="Close"
								on:click={() => (selectedNodeId = '')}>✕</button
							>
						</div>

						<div class="tm-inspector__label">{selectedNode.label}</div>

						<div class="tm-inspector__badges">
							<Badge text={selectedNode.kind} color="info" />
							<Badge
								text={selectedNode.epistemic_state}
								color={stateBadgeColor(selectedNode.epistemic_state)}
							/>
						</div>

						{#if historyState}
							<!-- Causal source reveal: where this (historical) node came from. -->
							<div class="tm-inspector__sources">
								<h3>Sources</h3>
								{#if selectedNode.source_refs.length === 0}
									<p class="tm-source-empty">No recorded sources for this node.</p>
								{:else}
									<ul class="tm-source-list">
										{#each selectedNode.source_refs as ref, i (i)}
											<li class="tm-source">
												{#if ref.utterance_id}
													<span class="tm-source__row"
														>utterance <code>{ref.utterance_id}</code></span
													>
												{/if}
												{#if ref.thread_id}
													<span class="tm-source__row">thread <code>{ref.thread_id}</code></span>
												{/if}
												{#if ref.timestamp}
													<span class="tm-source__row tm-source__when">{ref.timestamp}</span>
												{/if}
												{#if ref.quote}
													<blockquote class="tm-source__quote">“{ref.quote}”</blockquote>
												{/if}
												{#if !ref.utterance_id && !ref.thread_id && !ref.timestamp && !ref.quote}
													<span class="tm-source__row">(empty source ref)</span>
												{/if}
											</li>
										{/each}
									</ul>
								{/if}
							</div>
						{/if}

						{#if !historyState}
						<div class="tm-inspector__promote">
							{#if promotedRefOf(selectedNode, 'task')}
								<span
									class="tm-promoted-chip"
									title={`Task ${promotedRefOf(selectedNode, 'task')?.object_id}`}>→ task</span
								>
							{:else}
								<Button
									variant="outline"
									size="sm"
									label="↗ Task"
									title="Promote this node into a task"
									interactive={!actionBusy}
									on:click={() => void runPromote('task')}
								/>
							{/if}
							{#if promotedRefOf(selectedNode, 'memory')}
								<span
									class="tm-promoted-chip"
									title={`Memory candidate ${promotedRefOf(selectedNode, 'memory')?.object_id}`}
									>→ memory</span
								>
							{:else}
								<Button
									variant="outline"
									size="sm"
									label="💾 Memory"
									title="Promote this node into a reviewed memory"
									interactive={!actionBusy}
									on:click={() => void runPromote('memory')}
								/>
							{/if}
							<Button
								variant="outline"
								size="sm"
								label={askTutorBusy ? 'Opening…' : '🎓 Ask Tutor'}
								title="Ground the tutor in this node and open chat — the next @tutor run there sees this node's map context"
								interactive={!askTutorBusy}
								on:click={() => void askTutorAboutNode()}
							/>
						</div>

						{#if askTutorError}
							<div class="tm-promote-error" role="alert">
								<span>Couldn't set up the tutor: {askTutorError}</span>
								<button
									class="tm-action-error__x"
									title="Dismiss"
									on:click={() => (askTutorError = null)}>✕</button
								>
							</div>
						{/if}

						{#if promoteConfirm}
							<div class="tm-promote-confirm" role="alertdialog" aria-label="Confirm promotion">
								<span>This is AI-suggested — promote anyway?</span>
								<div class="tm-promote-confirm__actions">
									<Button
										variant="primary"
										size="sm"
										label="Promote"
										interactive={!actionBusy}
										on:click={() => {
											if (promoteConfirm) void runPromote(promoteConfirm, true);
										}}
									/>
									<Button
										variant="outline"
										size="sm"
										label="Cancel"
										on:click={() => (promoteConfirm = null)}
									/>
								</div>
							</div>
						{/if}

						{#if promoteSuccess}
							<div class="tm-promote-success" role="status">
								Promoted to {promoteSuccess.target} · <code>{promoteSuccess.objectId}</code>
							</div>
						{/if}

						{#if promoteError}
							<div class="tm-promote-error" role="alert">
								<span>Couldn't promote: {promoteError}</span>
								<button
									class="tm-action-error__x"
									title="Dismiss"
									on:click={() => (promoteError = null)}>✕</button
								>
							</div>
						{/if}
						{/if}

						<dl class="tm-inspector__facts">
							<div class="tm-fact">
								<dt>Origin</dt>
								<dd>{selectedNode.assertion_origin.replace(/_/g, ' ')}</dd>
							</div>
							<div class="tm-fact">
								<dt>Confidence</dt>
								<dd>{Math.round((selectedNode.confidence ?? 0) * 100)}%</dd>
							</div>
							{#if selectedNode.parent_id}
								<div class="tm-fact">
									<dt>Parent</dt>
									<dd>{shown.nodes[selectedNode.parent_id]?.label ?? selectedNode.parent_id}</dd>
								</div>
							{/if}
						</dl>

						{#if selectedNode.detail_markdown}
							<div class="tm-inspector__detail">
								<h3>Detail</h3>
								<pre class="tm-detail-md">{selectedNode.detail_markdown}</pre>
							</div>
						{/if}
					</div>
				{:else if historyState}
					<div class="tm-rail-empty">
						<p>
							You're viewing this map as it was at seq {historyState.seq}. Select a node to see
							what it looked like then — and where it came from (its recorded sources). Use the
							Time Loom to scrub, compare with now, or restore this point as a new branch.
						</p>
					</div>
				{:else if openClarifications.length === 0 && !firstProposal}
					<div class="tm-rail-empty">
						<p>Capture thoughts below, or use ✦ Continue / Break open to let the AI extend the map. Select a node to inspect it, and drag it to lay out the map — positions are saved.</p>
						<div class="tm-shortcuts" aria-hidden="true">
							<kbd>+ / −</kbd>
							<kbd>f · fit</kbd>
							<kbd>↑ ↓ ← →</kbd>
							<kbd>esc</kbd>
							<kbd>2× click · zoom</kbd>
						</div>
					</div>
				{/if}
			</aside>
		</div>
	{/if}
</div>

<style>
	.tm-detail-shell {
		display: flex;
		flex-direction: column;
		gap: 0.85rem;
		width: 100%;
		height: 100%;
		max-width: var(--app-content-max, 1320px);
		margin: 0 auto;
		padding: 1rem 1.45rem 1.25rem;
		box-sizing: border-box;
		min-height: 0;
	}

	.tm-detail-header {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 1rem;
		flex-wrap: wrap;
		flex-shrink: 0;
	}

	.tm-detail-heading {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
	}

	.tm-back {
		display: inline-flex;
		align-items: center;
		gap: 0.25rem;
		width: fit-content;
		padding: 0.15rem 0.5rem 0.15rem 0.35rem;
		margin-left: -0.35rem;
		border-radius: var(--radius-full, 999px);
		font-size: 0.75rem;
		color: var(--text-secondary);
		text-decoration: none;
		transition: color var(--transition-fast, 0.15s ease), background var(--transition-fast, 0.15s ease);
	}

	.tm-back:hover {
		color: var(--accent-primary);
		background: color-mix(in srgb, var(--accent-primary) 10%, transparent);
	}

	.tm-detail-heading h1 {
		margin: 0;
		font-family: var(--font-display, var(--font-primary));
		font-size: 1.3rem;
		font-weight: 700;
		letter-spacing: -0.01em;
		color: var(--text-primary);
		overflow-wrap: anywhere;
	}

	.tm-detail-meta {
		display: flex;
		align-items: center;
		gap: 0.4rem;
	}

	.tm-detail-actions {
		display: flex;
		gap: 0.5rem;
		flex-shrink: 0;
		flex-wrap: wrap;
	}

	.tm-listen {
		display: flex;
		align-items: center;
		gap: 0.6rem;
		flex-shrink: 0;
		padding: 0.5rem 0.85rem;
		border: 1px solid color-mix(in srgb, var(--accent-primary) 34%, transparent);
		border-radius: var(--radius-md, 8px);
		background: color-mix(in srgb, var(--accent-primary) 8%, transparent);
		color: var(--text-primary);
		font-size: 0.82rem;
	}

	.tm-listen__dot {
		width: 0.55rem;
		height: 0.55rem;
		flex-shrink: 0;
		border-radius: var(--radius-full, 999px);
		background: var(--accent-primary);
		animation: tm-listen-pulse 1.6s ease-in-out infinite;
	}

	.tm-listen__dot--connecting {
		background: var(--text-muted);
	}

	@keyframes tm-listen-pulse {
		0%,
		100% {
			opacity: 1;
			transform: scale(1);
		}
		50% {
			opacity: 0.45;
			transform: scale(0.8);
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.tm-listen__dot {
			animation: none;
		}
	}

	.tm-listen__text {
		flex: 1;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		color: var(--text-secondary);
	}

	.tm-listen__stop {
		flex-shrink: 0;
		padding: 0.2rem 0.7rem;
		border: 1px solid color-mix(in srgb, var(--accent-primary) 45%, transparent);
		border-radius: var(--radius-full, 999px);
		background: transparent;
		color: var(--accent-primary);
		font-size: 0.75rem;
		font-weight: 700;
		cursor: pointer;
		transition: background var(--transition-fast, 0.15s ease);
	}

	.tm-listen__stop:hover {
		background: color-mix(in srgb, var(--accent-primary) 12%, transparent);
	}

	.tm-conversations {
		flex-shrink: 0;
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
		padding: 0.75rem;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-md, 8px);
		background: var(--bg-card);
		box-shadow: var(--shadow-md);
	}

	.tm-conversations__head {
		display: flex;
		gap: 0.5rem;
		align-items: center;
	}

	.tm-conversations__filter {
		flex: 1;
		padding: 0.35rem 0.6rem;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-sm, 6px);
		background: var(--bg-soft, var(--bg-card));
		color: var(--text-primary);
		font-size: 0.8rem;
	}

	.tm-conversations__close {
		border: none;
		background: transparent;
		color: var(--text-secondary);
		cursor: pointer;
		font-size: 0.85rem;
		padding: 0.15rem 0.3rem;
	}

	.tm-conversations__list {
		list-style: none;
		margin: 0;
		padding: 0;
		max-height: 240px;
		overflow-y: auto;
		display: flex;
		flex-direction: column;
		gap: 2px;
	}

	.tm-conversations__row {
		display: flex;
		justify-content: space-between;
		align-items: center;
		gap: 0.75rem;
		width: 100%;
		padding: 0.45rem 0.6rem;
		border: none;
		border-radius: var(--radius-sm, 6px);
		background: transparent;
		color: var(--text-primary);
		font-size: 0.82rem;
		text-align: left;
		cursor: pointer;
	}

	.tm-conversations__row:hover:not(:disabled) {
		background: color-mix(in srgb, var(--accent-primary) 8%, transparent);
	}

	.tm-conversations__row:disabled {
		opacity: 0.45;
		cursor: default;
	}

	.tm-conversations__label {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.tm-conversations__when {
		flex-shrink: 0;
		color: var(--text-muted);
		font-size: 0.7rem;
	}

	.tm-conversations__empty {
		padding: 0.6rem;
		color: var(--text-secondary);
		font-size: 0.8rem;
	}

	.tm-conversations__hint {
		margin: 0;
		color: var(--text-muted);
		font-size: 0.7rem;
		line-height: 1.45;
	}

	.tm-conversation-chips {
		display: flex;
		flex-wrap: wrap;
		gap: 0.4rem;
		flex-shrink: 0;
	}

	.tm-conversation-chip {
		display: inline-flex;
		align-items: center;
		gap: 0.4rem;
		padding: 0.25rem 0.4rem 0.25rem 0.6rem;
		border: 1px solid color-mix(in srgb, var(--accent-primary) 40%, transparent);
		border-radius: var(--radius-full, 999px);
		background: color-mix(in srgb, var(--accent-primary) 8%, transparent);
		color: var(--text-primary);
		font-size: 0.75rem;
	}

	.tm-conversation-chip__dot {
		width: 0.45rem;
		height: 0.45rem;
		border-radius: var(--radius-full, 999px);
		background: var(--accent-primary);
		animation: tm-listen-pulse 1.6s ease-in-out infinite;
	}

	@media (prefers-reduced-motion: reduce) {
		.tm-conversation-chip__dot {
			animation: none;
		}
	}

	.tm-conversation-chip__x {
		border: none;
		background: transparent;
		color: var(--text-secondary);
		cursor: pointer;
		font-size: 0.75rem;
		padding: 0.05rem 0.25rem;
	}

	.tm-conversation-chip__x:hover {
		color: var(--color-error, var(--status-failed));
	}

	.tm-action-error,
	.tm-error {
		flex-shrink: 0;
		padding: 0.5rem 0.85rem;
		border: 1px solid color-mix(in srgb, var(--color-error, var(--status-failed)) 34%, transparent);
		border-radius: var(--radius-md, 8px);
		background: color-mix(in srgb, var(--color-error, var(--status-failed)) 10%, transparent);
		color: var(--color-error, var(--status-failed));
		font-size: 0.82rem;
	}

	.tm-action-error,
	.tm-error {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
	}

	.tm-action-error__x {
		border: none;
		background: transparent;
		color: inherit;
		cursor: pointer;
		font-size: 0.85rem;
		line-height: 1;
		padding: 0.1rem 0.25rem;
	}

	.tm-loading {
		padding: 2.5rem 0;
	}

	/* The facilitator's narration while an interpret run is in flight. One
	   quiet line — it sits above the composer whose buttons started the run,
	   and disappears the moment the run settles. */
	.tm-interpret-progress {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		padding: 0.35rem 0.6rem;
		border-radius: 8px;
		background: color-mix(in srgb, var(--accent-primary) 8%, transparent);
		font-size: 0.8rem;
	}

	.tm-interpret-progress__label {
		font-weight: 600;
		color: var(--accent-primary);
		text-transform: uppercase;
		font-size: 0.68rem;
		letter-spacing: 0.04em;
	}

	.tm-interpret-progress__detail {
		color: var(--text-secondary);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.tm-detail-body {
		display: grid;
		grid-template-columns: 1fr 340px;
		gap: 0.85rem;
		flex: 1;
		min-height: 0;
	}

	.tm-canvas-col {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
		min-width: 0;
		min-height: 0;
	}

	.tm-canvas-wrap {
		flex: 1;
		min-height: 340px;
		min-width: 0;
	}

	.tm-rail {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
		overflow-y: auto;
		min-height: 0;
	}

	.tm-rail-empty {
		display: flex;
		flex-direction: column;
		gap: 0.6rem;
		padding: 1rem;
		border: 1px dashed var(--border-default, var(--border-soft));
		border-radius: var(--radius-md, 8px);
		background: color-mix(in srgb, var(--accent-primary) 3%, var(--bg-soft, var(--bg-card)));
		color: var(--text-secondary);
		font-size: 0.8rem;
		line-height: 1.55;
	}

	.tm-rail-empty p {
		margin: 0;
	}

	.tm-shortcuts {
		display: flex;
		flex-wrap: wrap;
		gap: 0.3rem;
		padding-top: 0.55rem;
		border-top: 1px solid var(--border-soft);
	}

	.tm-shortcuts kbd {
		display: inline-flex;
		align-items: center;
		gap: 0.2rem;
		padding: 0.08rem 0.4rem;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-sm, 6px);
		background: var(--bg-card);
		box-shadow: var(--shadow-sm);
		color: var(--text-muted);
		font-family: var(--font-mono, monospace);
		font-size: 0.66rem;
		line-height: 1.4;
	}

	.tm-inspector {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
		padding: 0.95rem;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-md, 8px);
		background: var(--bg-card);
		box-shadow: var(--shadow-sm);
		min-height: 0;
		animation: tm-inspector-in var(--transition-base, 0.25s) var(--ease-settle, ease) both;
	}

	@keyframes tm-inspector-in {
		from {
			opacity: 0;
			transform: translateY(6px);
		}
		to {
			opacity: 1;
			transform: translateY(0);
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.tm-inspector {
			animation: none;
		}
	}

	.tm-inspector__head {
		display: flex;
		align-items: center;
		justify-content: space-between;
	}

	.tm-inspector__head h2 {
		margin: 0;
		font-size: 0.8rem;
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		color: var(--text-secondary);
	}

	.tm-inspector__close {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 1.5rem;
		height: 1.5rem;
		border: none;
		border-radius: var(--radius-full, 999px);
		background: transparent;
		color: var(--text-secondary);
		font-size: 0.85rem;
		cursor: pointer;
		line-height: 1;
		transition: color var(--transition-fast, 0.15s ease), background var(--transition-fast, 0.15s ease);
	}

	.tm-inspector__close:hover {
		color: var(--text-primary);
		background: color-mix(in srgb, var(--text-primary) 8%, transparent);
	}

	.tm-inspector__label {
		font-family: var(--font-display, var(--font-primary));
		font-size: 1rem;
		font-weight: 650;
		line-height: 1.35;
		color: var(--text-primary);
		overflow-wrap: anywhere;
	}

	.tm-inspector__badges {
		display: flex;
		flex-wrap: wrap;
		gap: 0.4rem;
	}

	.tm-inspector__promote {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.4rem;
	}

	.tm-promoted-chip {
		display: inline-flex;
		align-items: center;
		padding: 0.15rem 0.55rem;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-full, 999px);
		background: color-mix(in srgb, var(--color-success, #22a06b) 12%, transparent);
		color: var(--text-primary);
		font-size: 0.72rem;
		font-weight: 600;
		cursor: default;
	}

	.tm-promote-confirm {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
		padding: 0.6rem 0.7rem;
		border: 1px solid color-mix(in srgb, var(--color-warning, #b8860b) 45%, transparent);
		border-radius: var(--radius-sm, 6px);
		background: color-mix(in srgb, var(--color-warning, #b8860b) 10%, transparent);
		color: var(--text-primary);
		font-size: 0.8rem;
	}

	.tm-promote-confirm__actions {
		display: flex;
		gap: 0.4rem;
	}

	.tm-promote-success {
		padding: 0.45rem 0.6rem;
		border-radius: var(--radius-sm, 6px);
		background: color-mix(in srgb, var(--color-success, #22a06b) 12%, transparent);
		color: var(--text-primary);
		font-size: 0.78rem;
		overflow-wrap: anywhere;
	}

	.tm-promote-success code {
		font-family: var(--font-mono, monospace);
		font-size: 0.72rem;
	}

	.tm-promote-error {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.5rem;
		padding: 0.45rem 0.6rem;
		border-radius: var(--radius-sm, 6px);
		background: color-mix(in srgb, var(--color-error, #c0392b) 12%, transparent);
		color: var(--text-primary);
		font-size: 0.78rem;
		overflow-wrap: anywhere;
	}

	.tm-inspector__facts {
		display: flex;
		flex-direction: column;
		gap: 0.55rem;
		margin: 0;
	}

	.tm-fact {
		display: flex;
		justify-content: space-between;
		gap: 0.75rem;
		font-size: 0.8rem;
	}

	.tm-fact dt {
		color: var(--text-secondary);
		font-weight: 600;
	}

	.tm-fact dd {
		margin: 0;
		color: var(--text-primary);
		text-align: right;
		overflow-wrap: anywhere;
	}

	.tm-inspector__detail h3 {
		margin: 0 0 0.35rem;
		font-size: 0.72rem;
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		color: var(--text-secondary);
	}

	.tm-detail-md {
		margin: 0;
		padding: 0.6rem;
		border-radius: var(--radius-sm, 6px);
		background: var(--bg-soft, var(--bg-card));
		color: var(--text-primary);
		font-family: var(--font-mono, monospace);
		font-size: 0.78rem;
		line-height: 1.5;
		white-space: pre-wrap;
		overflow-wrap: anywhere;
	}

	/* ── Time Loom history mode ─────────────────────────────────────────────── */

	.tm-history-banner {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
		flex-shrink: 0;
		padding: 0.45rem 0.85rem;
		border: 1px solid color-mix(in srgb, var(--accent-primary) 40%, transparent);
		border-radius: var(--radius-md, 8px);
		background: color-mix(in srgb, var(--accent-primary) 10%, transparent);
		color: var(--text-primary);
		font-size: 0.8rem;
	}

	.tm-history-banner__text {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.tm-history-banner__back {
		flex-shrink: 0;
		padding: 0.2rem 0.7rem;
		border: 1px solid color-mix(in srgb, var(--accent-primary) 45%, transparent);
		border-radius: var(--radius-full, 999px);
		background: transparent;
		color: var(--accent-primary);
		font-size: 0.75rem;
		font-weight: 700;
		cursor: pointer;
		transition: background var(--transition-fast, 0.15s ease);
	}

	.tm-history-banner__back:hover {
		background: color-mix(in srgb, var(--accent-primary) 12%, transparent);
	}

	.tm-inspector__sources h3 {
		margin: 0 0 0.35rem;
		font-size: 0.72rem;
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		color: var(--text-secondary);
	}

	.tm-source-empty {
		margin: 0;
		color: var(--text-muted);
		font-size: 0.76rem;
	}

	.tm-source-list {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 0.45rem;
	}

	.tm-source {
		display: flex;
		flex-direction: column;
		gap: 0.2rem;
		padding: 0.45rem 0.6rem;
		border-radius: var(--radius-sm, 6px);
		background: var(--bg-soft, var(--bg-card));
		font-size: 0.74rem;
		color: var(--text-secondary);
	}

	.tm-source__row code {
		font-family: var(--font-mono, monospace);
		font-size: 0.7rem;
		color: var(--text-primary);
		overflow-wrap: anywhere;
	}

	.tm-source__when {
		color: var(--text-muted);
		font-size: 0.68rem;
	}

	.tm-source__quote {
		margin: 0.1rem 0 0;
		padding-left: 0.55rem;
		border-left: 2px solid color-mix(in srgb, var(--accent-primary) 45%, transparent);
		color: var(--text-primary);
		font-style: italic;
		overflow-wrap: anywhere;
	}

	@media (max-width: 860px) {
		.tm-detail-body {
			grid-template-columns: 1fr;
		}
	}
</style>
