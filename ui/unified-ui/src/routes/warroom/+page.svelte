<script lang="ts">
	/**
	 * `/warroom` — OPS DECK. The agent, center of the room, instrumented.
	 *
	 * Rebuilt 2026-07-27 from the demo-era warroom, whose peripheral panels
	 * synthesized "vitals", "throughput", "outputs" and "executions" out of
	 * ticker event names so the deck felt alive. THE LAW OF THIS DECK IS
	 * THE OPPOSITE: no motion without information. Every panel binds a real
	 * source, and when a source is down the deck says so instead of acting.
	 *
	 *   SystemBar  ← /health poll · todayPulseStore (live DuckDB SQL)
	 *   NeedsYou   ← attentionStore (feed lanes) → canonical HITL respond
	 *   CoreStage  ← chatStore (real send) · pulse pacing arcs · event EMA
	 *   InFlight   ← taskStore tasks + executingTask (REAL progress/steps)
	 *   EventTape  ← /api/magician/v3/events NDJSON stream (the only panel
	 *                the old deck fed honestly — kept, tightened)
	 *
	 * The deck itself is a state machine: NOMINAL (phosphor), ATTENTION
	 * (amber — a human input is pending; the whole room shifts), FAULT
	 * (red — health probe failed or an error burst is on the tape). Mode
	 * is derived in `deck.ts` from those measurements and nothing else.
	 */
	import { browser } from '$app/environment';
	import { onDestroy, onMount } from 'svelte';
	import { get } from 'svelte/store';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import {
		chatStore,
		getMessageText,
		normalizeMessages,
		type ChatMessage
	} from '$lib/stores/chatStore';
	import { todayPulseStore } from '$lib/stores/todayPulseStore';
	import { taskStore } from '$lib/stores/taskStore';
	import { attentionStore } from '$lib/stores/attentionStore';
	import { hitlRequestFromFeedItem } from '$lib/hitl/adapters';
	import type { HitlRequest } from '$lib/hitl/types';
	import type { FeedItem } from '$lib/feed/types';
	import {
		breathePeriodSeconds,
		deriveDeckMode,
		isLiveArrival,
		nextEventRate,
		normalizeEventFrame,
		pacingRatio,
		recentErrorCount,
		type TapeRow
	} from './deck';
	import ThemeSwitcher from '$lib/shared/components/ThemeSwitcher.svelte';
	import SystemBar from './SystemBar.svelte';
	import NeedsYou from './NeedsYou.svelte';
	import TextChat from './TextChat.svelte';
	import { createRunGraph, ingestRow, seedTaskRun, syncTasks } from './runGraph';
	import { extractPlanStepsFromExecutionPanel, extractPlanStepsFromPlanGraph } from '$lib/stores/taskStore';
	import HistoryDrawer from '$lib/shell/HistoryDrawer.svelte';
	import ConfirmationModalHost from '$lib/magician/components/ConfirmationModalHost.svelte';
	import {
		historyDrawerOpen,
		historyDrawerInitialTab,
		historyDrawerThreadFilter,
		openHistoryDrawer
	} from '$lib/shell/shellState';
	import { stripSpeechTags } from '$lib/media/tts/speechTags';
	import { voiceTranscriptStore } from '$lib/media/voice/realtimeVoiceClient';
	import CoreStage from './CoreStage.svelte';
	import InFlight from './InFlight.svelte';
	import EventTape from './EventTape.svelte';

	const TAPE_MAX = 60;
	const HITL_MAX = 12;
	const HEALTH_POLL_MS = 15_000;
	/**
	 * Backfill cap. The stream replays history on connect and, with no
	 * `limit`, defaults to the whole retention buffer — measured at 4,001
	 * frames. Parsing those synchronously inside the read loop blocks the main
	 * thread long enough to hang the tab, and it happens again on EVERY
	 * reconnect. The tape shows 7 lines over a 60-bucket histogram, so 80 is
	 * already more history than the deck can display.
	 */
	const BACKFILL_LIMIT = 80;
	/**
	 * A frame older than this is replayed history, not live traffic. Counting
	 * replay toward the rate is what made the deck read "65.8/s" while the
	 * uplink was in fact idle — an instrument reporting motion that was not
	 * happening, which is the exact failure this deck exists to avoid.
	 */
	const LIVE_WINDOW_MS = 5_000;

	// ── clock / heartbeat ────────────────────────────────────────────────
	let nowMs = Date.now();
	let clock = '--:--:--';
	let heartbeat: ReturnType<typeof setInterval> | null = null;

	// ── uplink (event stream) ────────────────────────────────────────────
	let tape: TapeRow[] = [];
	let nextRowId = 1;
	let totalEvents = 0;
	let lastEventAt = 0;
	let connection: 'connecting' | 'open' | 'closed' | 'error' = 'connecting';
	let uplink: AbortController | null = null;
	let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
	let reconnectDelayMs = 1_000;
	/** Arrivals since the last heartbeat tick — feeds the EMA. */
	let arrivalsSinceTick = 0;
	let eventsPerSecond = 0;
	/** 60 one-second buckets, most recent LAST. Real counts only. */
	let histogram: number[] = new Array(60).fill(0);

	// COALESCING. A busy runtime pushes 60+ frames/second down the uplink.
	// Assigning reactive state per frame re-renders the whole deck at that
	// rate and pegs the renderer (observed: the page became unscreenshottable
	// at ~65 events/s). Frames land in these NON-REACTIVE buffers and flush
	// to the DOM at a fixed 4 Hz, which is far above human reading speed and
	// costs a bounded amount of work no matter how hard the stream runs.
	const FLUSH_MS = 250;
	let pendingRows: TapeRow[] = [];
	let pendingBucket = 0;
	let pendingFlash = false;
	let flushTimer: ReturnType<typeof setInterval> | null = null;

	// ── health ───────────────────────────────────────────────────────────
	let health: { magician: boolean; magicutor: boolean; tauri: boolean } | null = null;
	let healthTimer: ReturnType<typeof setInterval> | null = null;

	// ── run graph: the stage's living execution tree ─────────────────────
	const runGraph = createRunGraph();
	let graphFocusTaskId: string | null = null;
	let graphFront = false;

	function toggleGraphFocus(taskId: string, meta?: { title?: string; status?: string }): void {
		if (graphFocusTaskId === taskId) {
			graphFocusTaskId = null;
			graphFront = false;
			return;
		}
		graphFocusTaskId = taskId;
		void seedFocusedRun(taskId, meta);
	}

	/** The ⌕ on a task row: spotlight AND bring the run graph to the front. */
	function openGraphFor(taskId: string, meta?: { title?: string; status?: string }): void {
		graphFocusTaskId = taskId;
		void seedFocusedRun(taskId, meta);
		graphFront = true;
	}

	/**
	 * Make ANY task inspectable, not just ones whose events flowed live: the
	 * moment a task is selected its node takes the OUTCOME colour (green
	 * completed / red failed / amber cancelled), and the execution-panel
	 * endpoint (execution_id omitted → latest run) supplies the plan steps,
	 * which grow the branch — status-coloured and labelled under focus.
	 */
	async function seedFocusedRun(
		taskId: string,
		meta?: { title?: string; status?: string }
	): Promise<void> {
		// Internal tasks never appear in the user task store — the row's own
		// metadata is the label/status source of truth there.
		const task = taskState.tasks?.find((t) => t.id === taskId);
		const label = (meta?.title || task?.title || task?.description || taskId).slice(0, 22);
		const status = meta?.status || task?.status || 'unknown';
		seedTaskRun(runGraph, { id: taskId, label, status }, [], Date.now());
		try {
			// Two sources, best first: the execution panel carries PER-STEP
			// STATUSES but only for panel-tracked runs; the task's PLAN GRAPH
			// carries the full step/tool/dependency structure for any planned
			// task (statuses unknown → steps render dim, which is honest: it
			// is the plan's shape, not its outcome). Single-shot agentic runs
			// have neither, and their one outcome node is the truth.
			const panelRes = await fetch(
				`/api/magician/v3/tasks/${encodeURIComponent(taskId)}/execution-panel`,
				{ signal: AbortSignal.timeout(10_000) }
			);
			let steps = panelRes.ok
				? extractPlanStepsFromExecutionPanel(await panelRes.json().catch(() => null))
				: [];
			if (steps.length === 0) {
				const planRes = await fetch(
					`/api/magician/v3/tasks/${encodeURIComponent(taskId)}/plan`,
					{ signal: AbortSignal.timeout(10_000) }
				);
				if (planRes.ok) {
					const body = await planRes.json().catch(() => null);
					steps = extractPlanStepsFromPlanGraph(body?.plan?.plan_graph);
				}
			}
			// The user may have moved on while the fetches ran.
			if (graphFocusTaskId !== taskId || steps.length === 0) return;
			seedTaskRun(runGraph, { id: taskId, label, status }, steps, Date.now());
		} catch {
			// The outcome node already stands; steps are best-effort.
		}
	}

	// ── stores ───────────────────────────────────────────────────────────
	$: scope = $scopeIdentityStore;
	$: scopeHeaders = {
	} as Record<string, string>;
	$: pulse = $todayPulseStore;
	$: snapshot = pulse.snapshot;
	$: taskState = $taskStore;
	// Live tasks get graph clusters even before their first event arrives.
	$: if (browser && taskState.tasks) syncTasks(runGraph, taskState.tasks, Date.now());
	$: attention = $attentionStore;
	$: chatState = $chatStore;

	// HITL queue: feed lanes → canonical requests. `failed`/`running` lanes
	// stay off this rail — they are history and status, not "waiting on you".
	$: hitlQueue = dedupeById(
		[...attention.requests, ...attention.approvals, ...attention.escalations]
			.map((item: FeedItem) => hitlRequestFromFeedItem(item))
			.filter((r): r is HitlRequest => r !== null)
	)
		.sort((a, b) => (b.at ?? 0) - (a.at ?? 0))
		.slice(0, HITL_MAX);

	function dedupeById(list: HitlRequest[]): HitlRequest[] {
		const seen = new Set<string>();
		return list.filter((r) => (seen.has(r.id) ? false : (seen.add(r.id), true)));
	}

	// ── deck mode: derived from measurements, nothing else ─────────────
	$: mode = deriveDeckMode({
		healthOk: health === null ? null : health.magician,
		pendingHitl: hitlQueue.length,
		recentErrors: recentErrorCount(tape, nowMs)
	});

	$: breathePeriod = breathePeriodSeconds(eventsPerSecond);

	// Pacing arcs — today against yesterday's full total (that is what the
	// pulse queries measure; the caption in CoreStage says so).
	$: arcs = {
		spend: snapshot ? pacingRatio(snapshot.llm.spendToday, snapshot.llm.spendYesterday) : null,
		calls: snapshot ? pacingRatio(snapshot.llm.callsToday, snapshot.llm.callsYesterday) : null,
		tasks: snapshot
			? pacingRatio(snapshot.tasks.completedToday, snapshot.tasks.completedYesterday)
			: null
	};

	// ── chat ─────────────────────────────────────────────────────────────
	$: activeSessionId = chatState.activeSessionId;
	$: bubbles = buildBubbles(chatState.messages);
	// Finished voice turns join the text rail — the centre-stage caption
	// decays, and this is where the words land. Appended after the typed
	// bubbles: turns during a live call are always newer than the loaded
	// history, and the backend persists them into the session for next load.
	$: voiceBubbles = $voiceTranscriptStore.turns
		.filter((t) => t.done && t.text.trim().length > 0)
		.slice(-14)
		.map((t) => ({
			id: `vox-${t.id}`,
			role: (t.speaker === 'user' ? 'user' : 'assistant') as 'user' | 'assistant',
			text: t.text.trim(),
			voice: true
		}));
	$: railBubbles = [...bubbles, ...voiceBubbles];
	// Thread, then session name -- a raw session UUID identifies nothing a
	// human can act on. Scope moved to the system bar, where it belongs to the
	// whole deck rather than to this one channel.
	$: activeSession = chatState.sessions?.find((s) => s.id === activeSessionId) ?? null;
	// UUID thread ids get shortened — `#f7b33632` reads as a tag,
	// `#f7b33632-c1d3-47f6-…` reads as debris.
	$: threadTag = activeSession
		? activeSession.ui_thread_id.length > 16
			? activeSession.ui_thread_id.slice(0, 8)
			: activeSession.ui_thread_id
		: '';
	$: sessionLabel = activeSession
		? `#${threadTag} · ${activeSession.title?.trim() || 'untitled session'}`
		: '';

	function buildBubbles(
		messages: typeof chatState.messages
	): Array<{ id: string; role: 'user' | 'assistant'; text: string }> {
		if (!messages || messages.length === 0) return [];
		const out: Array<{ id: string; role: 'user' | 'assistant'; text: string }> = [];
		for (const m of normalizeMessages(messages)) {
			// ChatMessage carries `direction`, not `role`.
			if (m.direction !== 'user' && m.direction !== 'assistant') continue;
			// Voice-originated replies carry `<speech>` delivery markup. It is
			// TTS instruction, not prose -- rendering it raw leaks angle
			// brackets into the transcript. `stripSpeechTags` is the canonical
			// parser, so this cannot drift from how other surfaces read it.
			const text = stripSpeechTags(getMessageText(m.content)).trim();
			if (!text) continue;
			out.push({ id: m.id, role: m.direction, text });
		}
		return out.slice(-40);
	}

	/**
	 * Chat session load: BOUNDED RETRY, never a reactive subscription.
	 *
	 * `loadActiveSession` stamps a scope token on entry and, if that token has
	 * gone stale on return, bails with `return null` WITHOUT committing the
	 * session or clearing `isLoading` — leaving the store at
	 * `isLoading: true, error: null`, which renders as a channel stuck on
	 * "connecting". A single mount-time call can lose that race against scope
	 * hydration, so it needs a retry.
	 *
	 * But the retry MUST NOT be driven off `scopeIdentityStore`: on success
	 * `loadActiveSession` itself calls `scopeIdentityStore.observe(...)`, so a
	 * reactive block keyed on that store re-triggers the very load that wrote
	 * to it. That is a feedback loop, and it pinned the main thread hard enough
	 * to make the whole page unresponsive — measured, not theorised.
	 *
	 * A bounded counter cannot loop no matter what the callee writes.
	 */
	const CHAT_RETRY_LIMIT = 4;
	const CHAT_RETRY_DELAY_MS = 1_500;
	let chatAttempts = 0;
	let chatRetryTimer: ReturnType<typeof setTimeout> | null = null;

	function loadChatSession(): void {
		if (!browser) return;
		if (chatAttempts >= CHAT_RETRY_LIMIT) return;
		chatAttempts += 1;
		void chatStore
			.loadActiveSession('general')
			.then((session) => {
				// A null return is the silent stale-token bail; retry it.
				if (!session && chatAttempts < CHAT_RETRY_LIMIT) {
					chatRetryTimer = setTimeout(loadChatSession, CHAT_RETRY_DELAY_MS);
				}
			})
			.catch((err) => {
				console.warn('[warroom] chat session load failed:', err);
			});
	}

	function sendCommand(text: string): void {
		if (!activeSessionId) return;
		void chatStore.sendMessage(activeSessionId, text).catch((err) => {
			console.warn('[warroom] send failed:', err);
		});
	}

	async function sendAmbientDictation(sessionId: string, text: string): Promise<{
		id: string;
		text: string;
		speechSegments?: ChatMessage['speech_segments'];
	} | null> {
		const reply = await chatStore.sendMessage(sessionId, text, null, [], {
			sourceSurface: 'web_ambient_dictation',
			voiceOrigin: true
		});
		if (!reply) return null;
		const spokenText = stripSpeechTags(getMessageText(reply.content)).trim();
		if (!spokenText && !reply.speech_segments?.length) return null;
		return {
			id: reply.id,
			text: spokenText,
			speechSegments: reply.speech_segments
		};
	}

	// ── uplink loop ──────────────────────────────────────────────────────
	async function connectUplink(): Promise<void> {
		if (!browser) return;
		uplink?.abort();
		const ctrl = new AbortController();
		uplink = ctrl;
		connection = 'connecting';
		try {
			const params = new URLSearchParams({
				limit: String(BACKFILL_LIMIT)
			});
			const response = await fetch(`/api/magician/v3/events?${params.toString()}`, {
				signal: ctrl.signal,
				headers: { Accept: 'application/x-ndjson' }
			});
			if (!response.ok || !response.body) throw new Error(`uplink HTTP ${response.status}`);
			connection = 'open';
			const openedAt = Date.now();
			const reader = response.body.getReader();
			const decoder = new TextDecoder();
			let buffer = '';
			for (;;) {
				const { done, value } = await reader.read();
				if (ctrl !== uplink) return;
				if (done) break;
				buffer += decoder.decode(value, { stream: true });
				let nl: number;
				while ((nl = buffer.indexOf('\n')) !== -1) {
					const line = buffer.slice(0, nl).trim();
					buffer = buffer.slice(nl + 1);
					if (line) ingest(line);
				}
				// Hard cap on the reassembly buffer. Without it, a stream that
				// stops emitting newlines (a malformed frame, a keep-alive
				// without a terminator) grows this string without bound for as
				// long as the page is open — memory climbs, GC thrashes, and
				// the tab dies "after a while" with no error anywhere.
				if (buffer.length > 1_000_000) buffer = '';
			}
			// Only treat the attempt as healthy if it actually stayed up. A
			// stream that closes immediately after backfill must back off,
			// not reconnect every second and replay history each time.
			if (Date.now() - openedAt > 10_000) reconnectDelayMs = 1_000;
			connection = 'closed';
		} catch (err) {
			if (ctrl !== uplink || ctrl.signal.aborted) return;
			connection = 'error';
			void err;
		}
		scheduleReconnect();
	}

	function scheduleReconnect(): void {
		if (!browser || reconnectTimer) return;
		reconnectTimer = setTimeout(() => {
			reconnectTimer = null;
			reconnectDelayMs = Math.min(reconnectDelayMs * 2, 15_000);
			void connectUplink();
		}, reconnectDelayMs);
	}

	/** Hot path: buffer only. No reactive assignment here — see FLUSH_MS. */
	function ingest(line: string): void {
		const row = normalizeEventFrame(line, nextRowId);
		if (!row) return;
		nextRowId += 1;

		// Replayed history belongs on the tape (it is real, and an empty tape
		// on connect is unhelpful) but must NOT drive the rate, histogram or
		// core flash. Those three report LIVE activity; feeding replay into
		// them makes an idle system look busy.
		if (isLiveArrival(row.ts, Date.now(), LIVE_WINDOW_MS)) {
			arrivalsSinceTick += 1;
			pendingBucket += 1;
			pendingFlash = true;
		}

		// Bounded ring: shift once at the cap instead of re-slicing the whole
		// buffer on every push past it (that was O(n²) across a backfill).
		pendingRows.push(row);
		if (pendingRows.length > TAPE_MAX) pendingRows.shift();
	}

	/** 4 Hz: one reactive write per burst instead of one per frame. */
	function flushUplink(): void {
		if (pendingRows.length === 0 && pendingBucket === 0) return;
		if (pendingRows.length > 0) {
			// Feed the run graph BEFORE the reverse below mutates the array.
			// Plain object, no reactivity: the stage canvas polls it per frame.
			const feedNow = Date.now();
			for (const row of pendingRows) ingestRow(runGraph, row, feedNow);
			// pendingRows is oldest-first; the tape renders newest-first.
			tape = [...pendingRows.reverse(), ...tape].slice(0, TAPE_MAX);
			totalEvents += pendingRows.length;
			pendingRows = [];
		}
		if (pendingBucket > 0) {
			const next = [...histogram];
			next[next.length - 1] += pendingBucket;
			histogram = next;
			pendingBucket = 0;
		}
		if (pendingFlash) {
			// One flash per flush window, so the core pulses legibly at 4 Hz
			// rather than strobing at the stream's rate.
			lastEventAt = Date.now();
			pendingFlash = false;
		}
	}

	// ── health probe ─────────────────────────────────────────────────────
	async function probeHealth(): Promise<void> {
		try {
			// `/health` is proxied at the ROOT, not under `/api/magician` — the
			// proxy forwards that prefix without rewriting, so
			// `/api/magician/health` 404s and the deck sat in a permanent FAULT
			// reporting a backend that was in fact healthy.
			const res = await fetch('/health', { signal: AbortSignal.timeout(5_000) });
			if (!res.ok) throw new Error(String(res.status));
			const body = (await res.json()) as Record<string, unknown>;
			health = {
				magician: body.magician === 'healthy' || body.status === 'ok',
				magicutor: body.magicutor_status === 'healthy',
				tauri: body.tauri_status === 'healthy'
			};
		} catch {
			health = { magician: false, magicutor: false, tauri: false };
		}
	}

	onMount(() => {
		// Deck typefaces load ASYNCHRONOUSLY. A render-blocking <link> in
		// <head> hangs `domcontentloaded` on the CDN when the network is slow
		// or absent — a command deck must never wait on a third-party host.
		// The page renders instantly on the fallback stacks and upgrades
		// in place when (if) the fonts arrive.
		if (!document.getElementById('warroom-fonts')) {
			const link = document.createElement('link');
			link.id = 'warroom-fonts';
			link.rel = 'stylesheet';
			link.href =
				'https://fonts.googleapis.com/css2?family=Chakra+Petch:wght@500;600;700&family=IBM+Plex+Mono:wght@400;500;600;700&display=swap';
			document.head.appendChild(link);
		}
		todayPulseStore.start();
		taskStore.start();
		void taskStore.loadTasks();
		attentionStore.start();
		loadChatSession();
		void connectUplink();
		void probeHealth();
		healthTimer = setInterval(() => void probeHealth(), HEALTH_POLL_MS);
		flushTimer = setInterval(flushUplink, FLUSH_MS);
		heartbeat = setInterval(() => {
			nowMs = Date.now();
			clock = new Date(nowMs).toLocaleTimeString('en-GB', { hour12: false });
			eventsPerSecond = nextEventRate(eventsPerSecond, 1_000, arrivalsSinceTick);
			arrivalsSinceTick = 0;
			histogram = [...histogram.slice(1), 0];
		}, 1_000);
	});

	onDestroy(() => {
		uplink?.abort();
		uplink = null;
		if (reconnectTimer) clearTimeout(reconnectTimer);
		if (chatRetryTimer) clearTimeout(chatRetryTimer);
		if (heartbeat) clearInterval(heartbeat);
		if (healthTimer) clearInterval(healthTimer);
		if (flushTimer) clearInterval(flushTimer);
		todayPulseStore.stop();
		taskStore.stop();
		attentionStore.stop();
	});
</script>

<svelte:head>
	<title>OPS DECK</title>
</svelte:head>

<div class="deck" data-mode={mode}>
	<SystemBar
		{mode}
		{clock}
		{health}
		spendToday={snapshot?.llm.spendToday ?? null}
		spendYesterday={snapshot?.llm.spendYesterday ?? null}
		callsToday={snapshot?.llm.callsToday ?? null}
		memoriesToday={snapshot?.memoriesToday ?? null}
		{eventsPerSecond}
		{scope}
	/>
	<TextChat
		bubbles={railBubbles}
		onOpenHistory={() => openHistoryDrawer()}
		{sessionLabel}
		canSend={Boolean(activeSessionId)}
		sending={chatState.isSendingMessage}
		chatError={activeSessionId ? null : chatState.error}
		onSend={sendCommand}
	/>
	<CoreStage
		{breathePeriod}
		{lastEventAt}
		threadId={activeSessionId}
		onDictationSend={sendAmbientDictation}
		graph={runGraph}
		{graphFocusTaskId}
		{graphFront}
		onGraphFront={(front) => (graphFront = front)}
	/>
	<NeedsYou
		requests={hitlQueue}
		headers={scopeHeaders}
		{nowMs}
		onResolved={() => void attentionStore.refresh()}
	/>
	<InFlight
		executing={taskState.executingTask}
		tasks={taskState.tasks}
		completedToday={snapshot?.tasks.completedToday ?? 0}
		{nowMs}
		selectedTaskId={graphFocusTaskId}
		onSelectTask={toggleGraphFocus}
		onOpenGraph={openGraphFor}
	/>
	<EventTape rows={tape} {histogram} {connection} {totalEvents} {nowMs} />
</div>

<!-- Theme control, floated exactly as `/` floats its own: a FIXED wrapper
     that is a SIBLING of the deck, never a descendant. The deck is
     `position: fixed; overflow: hidden`, so a switcher nested inside it has
     nowhere to open into — its dropdown is clipped away and the control
     reads as dead. Fixed positioning takes the panel out of the deck's
     clipping and stacking entirely, which is why the landing page's
     `.landing-theme-corner` has always worked. -->
<div class="deck-theme-corner">
	<ThemeSwitcher iconOnly />
</div>

<!-- The history drawer normally lives in (app)/+layout.svelte; the deck is
     outside that group, so it mounts here — same pattern as /hud. Session
     selection switches the deck's chat IN PLACE via onSelectSession instead
     of navigating to the thread's chat page. ConfirmationModalHost hosts the
     drawer's destructive confirmations. -->
<HistoryDrawer
	bind:open={$historyDrawerOpen}
	threadFilter={$historyDrawerThreadFilter}
	initialTab={$historyDrawerInitialTab}
	onSelectSession={(session) => {
		void chatStore.openSession(session.id);
	}}
/>
<ConfirmationModalHost />

<style>
	.deck {
		/* THE DECK DOES NOT OWN A PALETTE. It reads the operator's theme, which
		   is the layout's documented promise ("whichever theme the operator has
		   on stays applied"). An earlier cut darkened `--bg-base` toward black
		   to force an instrument look; that broke light themes and overrode a
		   choice that is the operator's to make.
		   Every value below resolves from a per-theme token, so the deck is a
		   HUD in Jarvis-dark and an equally legible HUD in a paper-light
		   theme — the geometry carries the identity, not the colours. */
		--deck-glow: var(--accent-primary, #38e1c9);
		--deck-bg: var(--bg-base, #05080b);
		--deck-panel: var(--bg-surface, var(--deck-bg));
		--deck-text: var(--text-primary, #dbe7e5);
		--deck-dim: var(--text-secondary, color-mix(in srgb, var(--deck-text) 55%, transparent));
		/* Rules take their weight from the theme's own border, warmed toward
		   the accent so the HUD framing still reads as instrument etching. */
		--deck-line: color-mix(in srgb, var(--border-default, currentColor) 70%, var(--deck-glow));
		--sev-ok: var(--color-success, #3ddc97);
		--sev-warn: var(--color-warning, #ffb454);
		--sev-err: var(--color-error, #ff5c5c);
		--sev-hitl: var(--color-warning, #ffd166);
		/* Theme typefaces win when the theme defines them (retro-16bit and
		   longhand carry their own); the deck's stack is the fallback. */
		--font-display: var(--theme-font-display, 'Chakra Petch', 'Avenir Next Condensed', sans-serif);
		--font-data: var(--theme-font-mono, 'IBM Plex Mono', ui-monospace, monospace);
		--font-body: var(--theme-font-body, system-ui, sans-serif);

		position: fixed;
		inset: 0;
		/* Contain the deck's stacking so nothing outside can interleave. */
		isolation: isolate;
		display: grid;
		grid-template:
			'sysbar sysbar sysbar' 54px
			'chat stage needs' auto
			'chat stage flight' minmax(0, 1fr)
			'tape tape tape' 40px
			/ 304px 1fr 300px;
		background:
			radial-gradient(1100px 520px at 50% 30%, color-mix(in srgb, var(--deck-glow) 5%, transparent), transparent 70%),
			var(--deck-bg);
		color: var(--deck-text);
		overflow: hidden;
	}

	/* The room reacts to truth: amber when a human is being waited on,
	   red when the uplink itself is the problem. */
	.deck[data-mode='attention'] { --deck-glow: var(--color-warning, #ffb454); }
	.deck[data-mode='fault'] { --deck-glow: var(--color-error, #ff5c5c); }
	/* NO full-page `filter` animation here. `filter` on a viewport-sized fixed
	   element forces the whole page into a new compositing layer and
	   re-rasterises every pixel for the duration — and `mode` can flip as
	   often as once a second (it derives from a 1 Hz error-window count), so
	   this fired continuously. The mode change is already unmistakable: the
	   accent recolours every rule, LED and arc on the deck. */

	/* The scanline film is GONE. It was a viewport-sized ~300-stripe gradient
	   on its own compositing layer, repainted with every deck repaint, and it
	   carried no information — which makes it exactly the kind of decoration
	   this deck's own rule forbids. Removing it costs nothing and removes a
	   full-screen layer from every frame. */

	.deck-theme-corner {
		position: fixed;
		top: 11px;
		right: 16px;
		z-index: 60;
	}

	/* Reduced motion: the deck's animations are all *ambient* — breathing,
	   spin, scanlines, mode blink. None of them carry information that the
	   numbers and colours do not already carry, so an operator who asks for
	   stillness loses nothing. The one thing kept is the arc transition,
	   which is a value change rather than decoration. */
	@media (prefers-reduced-motion: reduce) {
		.deck :global(*),
		.deck :global(*::before),
		.deck :global(*::after) {
			animation: none !important;
		}
		.deck { animation: none !important; }
	}

	@media (max-width: 1180px) {
		.deck {
			grid-template:
				'sysbar sysbar' 54px
				'needs flight' minmax(140px, auto)
				'stage stage' 1fr
				'tape tape' 120px
				/ 1fr 1fr;
			position: relative;
			min-height: 100dvh;
			overflow: auto;
		}
	}
</style>
