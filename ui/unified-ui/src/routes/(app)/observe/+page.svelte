<script lang="ts">
	// Observe console — one route, four panes:
	//   Now      upcoming, recent, and the Listen / Join / Watch start forms
	//   Sources  continuous sources, mail, calendar, tabs, startup catch-up
	//   Audio    meeting and listening profiles
	//   Notes    published task notes
	// Live captures sit above the panes. The Meetings index redirects here.
	//
	// Rails: media_rails/meeting/* (audio) + media_rails/screen_observe.rs
	// (frames). Doc: docs/components/magician/screen-capture-and-ask.md +
	// docs/components/magician/realtime-media-rails.md
	import { browser } from '$app/environment';
	import { onDestroy, onMount, tick } from 'svelte';
	import { goto, replaceState } from '$app/navigation';
	import { page } from '$app/stores';
	import RecordingDot from '$lib/shared/components/RecordingDot.svelte';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import SurfaceAudioProfileControl from '$lib/media/SurfaceAudioProfileControl.svelte';
	import ObservableSourcesPanel from '$lib/observe/ObservableSourcesPanel.svelte';
	import ObserveCatchUpPanel from '$lib/observe/ObserveCatchUpPanel.svelte';
	import ObserveNotesPanel from '$lib/observe/ObserveNotesPanel.svelte';
	import AppSlotRegion from '$lib/apps/AppSlotRegion.svelte';
	import {
		observeRetreatedSections,
		OBSERVE_RETREAT_PAGE
	} from '$lib/observe/observeRetreat';
	import { mediaPreferencesStore } from '$lib/media/preferences';
	import {
		matchActiveSessionsToUpcoming,
		resolveMeetingListenMetadata,
		upcomingMeetingKey
	} from '$lib/observe/activeMeetingMatch';
	import { loadAgents } from '$lib/stores/agentStore';
	import { ASSISTANT_FALLBACK_NAME } from '$lib/presentationIdentity';
	import { showError, showSuccess } from '$lib/shared/stores/notifications';
	import {
		meetingSessionLabel,
		meetingsActive,
		pollMeetingsActiveNow,
		requestFastMeetingsPolling
	} from '$lib/stores/meetingsStore';
	import type {
		ActiveMeetingSession,
		RecentMeetingThread,
		UpcomingMeeting
	} from '$lib/stores/meetingsStore';
	import {
		observeStatus,
		pollObserveStatusNow,
		requestFastObservePolling
	} from '$lib/stores/observeStore';
	import {
		enrollAmbientBrowser,
		fetchAmbientStatus,
		fetchRecentMeetings,
		fetchRecentWatchSessions,
		fetchUpcomingMeetings,
		joinMeeting,
		retargetScreenObservation,
		saveAmbientConfig,
		setMeetingPaused,
		startMeetingListen,
		startScreenObservation,
		stopMeetingSession,
		stopScreenObservation,
		type AmbientStatus,
		type WatchSession
	} from '$lib/observe/api';
	import {
		fetchObserveAccounts,
		fetchObserveStatus,
		saveObserveConfig,
		type ObserveAccounts,
		type ObserveConfig,
		type ObserveProducer
	} from '$lib/stores/observeConnectorStore';
	import {
		fetchChannelAssistChannels,
		hasVerificationCodesPurpose,
		saveChannelAssistChannels,
		setChannelVerificationCodes,
		supportsVerificationCodes,
		channelProviderLabel,
		laneLabel,
		type ChannelAssistChannel,
		type ChannelAssistChannelsView
	} from '$lib/stores/channelAssistStore';

	// ── channel-assist (Mail/Chat Assist toggle) state ───────────────
	let channelAssistChannels: ChannelAssistChannel[] = [];
	let channelAssistHistoryLookbackDays = 7;
	let channelAssistChannelsBusy = false;
	let channelAssistChannelsDirty = false;
	let channelAssistChannelsError = '';
	// Message follow-ups now live natively on /today + /attention, so
	// the Observe card was removed to avoid duplication.

	function applyChannelAssistChannels(view: ChannelAssistChannelsView) {
		channelAssistChannelsError = view.ok ? '' : (view.error ?? 'Mail channels unavailable.');
		channelAssistChannels = view.channels;
		channelAssistHistoryLookbackDays = view.history_lookback_days;
		channelAssistChannelsDirty = false;
	}

	async function loadChannelAssistChannels() {
		applyChannelAssistChannels(await fetchChannelAssistChannels());
	}
	let verificationPurposeBusy = '';
	/**
	 * Secure HITL P6: the verification-code purpose is a separate grant on a
	 * configured account, written immediately (it is not part of the
	 * enablement list the Save button posts).
	 */
	async function toggleVerificationCodes(idx: number) {
		const account = channelAssistChannels[idx];
		const granted = !hasVerificationCodesPurpose(account);
		// A grant can only be GIVEN on an observed account, but it must always be
		// withdrawable: gating the control behind `enabled` meant turning
		// observation off left the code grant checked and un-uncheckable.
		if (!account.enabled && granted) return;
		verificationPurposeBusy = `${account.provider}:${account.account_alias}`;
		const res = await setChannelVerificationCodes(account.provider, account.account_alias, granted);
		verificationPurposeBusy = '';
		if (!res.ok) {
			channelAssistChannelsError = res.error;
			return;
		}
		channelAssistChannels[idx] = {
			...account,
			purposes: granted
				? [...(account.purposes ?? []).filter((p) => p !== 'verification_codes'), 'verification_codes']
				: (account.purposes ?? []).filter((p) => p !== 'verification_codes')
		};
		channelAssistChannels = channelAssistChannels;
	}
	function toggleChannelAssistChannel(idx: number) {
		channelAssistChannels[idx] = {
			...channelAssistChannels[idx],
			enabled: !channelAssistChannels[idx].enabled
		};
		channelAssistChannels = channelAssistChannels;
		channelAssistChannelsDirty = true;
	}
	async function saveChannelAssistChannelsNow() {
		channelAssistChannelsBusy = true;
		channelAssistChannelsError = '';
		const res = await saveChannelAssistChannels(
			channelAssistChannels.map((c) => ({
				provider: c.provider,
				account_alias: c.account_alias,
				lane: c.lane,
				enabled: c.enabled
			})),
			channelAssistHistoryLookbackDays
		);
		channelAssistChannelsBusy = false;
		if (res.ok) {
			channelAssistChannels = res.view.channels;
			channelAssistHistoryLookbackDays = res.view.history_lookback_days;
			channelAssistChannelsDirty = false;
		} else {
			channelAssistChannelsError = res.error;
		}
	}
	$: channelAssistChannelsEnabledCount = channelAssistChannels.filter((c) => c.enabled).length;

	// ── meetings state ────────────────────────────────────────────────
	// Active sessions come from the SHARED poller (the same one behind the
	// TopBar dot) — this page holds a fast-cadence lease instead of running
	// its own interval against `/meetings/active`.
	$: active = $meetingsActive ?? [];
	let recentMeetings: RecentMeetingThread[] = [];
	let loading = true;
	let loadError: string | null = null;

	// Calendar context (own fetch lane — a slow gws CLI never stalls the lists)
	let upcoming: UpcomingMeeting[] = [];
	let upcomingError: string | null = null;
	let upcomingLoading = true;
	let upcomingRefreshing = false;

	// The attendee bot joins under the PERSONAL agent's name — resolve it so
	// every label matches the tile that actually appears in the meeting.
	let agentName = ASSISTANT_FALLBACK_NAME;

	let listenTitle = '';
	let listenUrl = '';
	let listenMic = false;
	type StartVerb = 'listen' | 'join' | 'watch';
	let startVerb: StartVerb | null = null;
	let joinUrl = '';
	let joinTitle = '';
	let actionError: string | null = null;
	let actionBusy: 'listen' | 'join' | 'observe' | null = null;
	let stoppingIds = new Set<string>();
	let pausingIds = new Set<string>();
	let activeUpcomingSessions = new Map<string, ActiveMeetingSession>();
	let upcomingTitleBySessionId = new Map<string, string>();

	// ── observation state ─────────────────────────────────────────────
	// Observation status from the SHARED poller (same one behind the TopBar
	// dot); mutations call pollObserveStatusNow() instead of fetching here.
	$: observation = $observeStatus;
	let recentWatch: WatchSession[] = [];
	let purpose = '';
	let watchFor = '';
	let retargetCondition = '';
	let observeBusy = false;
	let observeDeep: 'off' | 'on' = 'off';
	let activeDeepObservation: 'off' | 'on' = 'off';
	// Optional audio capture alongside the screen. System rides the Screen
	// Recording grant the watch already needs; mic asks for its own grant.
	// The STT model is chosen HERE (not the composer's browser STT) because the
	// rail captures server-side system audio the Web Speech API can't see.
	let observeAudio: 'none' | 'system' | 'mic' | 'both' = 'none';
	let observableSourceCount = 0;

	// ── ambient tab observation (WEG Phase 2) ─────────────────────────
	// Opt-in, user-controlled capture of normal browsing. This card is the
	// consent + status surface; the paired browser extension does the capture
	// (P2.2) and only when `enabled` here. Private/incognito windows are always
	// excluded; `denylist` origins are never captured.
	let ambientStatus: AmbientStatus | null = null;
	let ambientDenylistText = '';
	let ambientBusy = false;
	// Collector-token pairing (P2.1b): a token issued here is pasted into the
	// browser extension's pairing field so its uploads resolve to THIS scope.
	let ambientPairToken: string | null = null;
	let tabsDetails = false;

	// ── email/calendar observe connectors (WEG Phase 4) ───────────────
	// One generic per-producer state map; the two cards below render over it.
	interface ObserveEditState {
		config: ObserveConfig | null;
		selected: Set<string>;
		frequency: string;
		time: string;
		suppressSensitive: boolean;
		busy: boolean;
		error: string | null;
	}
	function freshObserveState(): ObserveEditState {
		return {
			config: null,
			selected: new Set(),
			frequency: 'daily',
			time: '07:00',
			suppressSensitive: true,
			busy: false,
			error: null
		};
	}
	let observeAccounts: ObserveAccounts = { email_accounts: [], calendar_accounts: [] };
	let observe: Record<ObserveProducer, ObserveEditState> = {
		email: freshObserveState(),
		calendar: freshObserveState()
	};

	type BadgeTone = 'default' | 'success' | 'warning' | 'error' | 'info';

	function badgeClass(tone: BadgeTone = 'default'): string {
		return `status-badge status-badge--${tone}`;
	}

	function checkedFrom(event: Event): boolean {
		return (event.currentTarget as HTMLInputElement).checked;
	}

	function applyCalendarObserveStatus(calendarStatus: ObserveConfig | null) {
		// Email folded into the unified "Mail & chat" channel card (U4); only the
		// calendar producer keeps its own observe card (it stays on the digest).
		const st = observe.calendar;
		st.config = calendarStatus;
		st.error = null;
		if (calendarStatus) {
			st.selected = new Set(calendarStatus.accounts ?? []);
			st.frequency = calendarStatus.frequency || 'daily';
			st.time = calendarStatus.time || '07:00';
			st.suppressSensitive = calendarStatus.suppress_sensitive ?? true;
		}
		observe = observe; // nudge reactivity (Set/nested mutation)
	}

	async function loadObserveConnectors() {
		const accountsPromise = fetchObserveAccounts().then((accounts) => {
			observeAccounts = accounts;
		});
		const calendarPromise = fetchObserveStatus('calendar')
			.then(applyCalendarObserveStatus)
			.catch(() => {
				const st = observe.calendar;
				st.error = 'Calendar observe unavailable.';
				observe = observe;
			});
		const channelPromise = fetchChannelAssistChannels().then(applyChannelAssistChannels);
		await Promise.allSettled([accountsPromise, calendarPromise, channelPromise]);
	}
	function toggleObserveAccount(producer: ObserveProducer, name: string) {
		const sel = observe[producer].selected;
		if (sel.has(name)) sel.delete(name);
		else sel.add(name);
		observe = observe;
	}
	async function saveObserve(producer: ObserveProducer, enabled: boolean) {
		const st = observe[producer];
		st.busy = true;
		st.error = null;
		observe = observe;
		try {
			const res = await saveObserveConfig(producer, {
				enabled,
				accounts: Array.from(st.selected),
				frequency: st.frequency,
				time: st.time,
				suppress_sensitive: st.suppressSensitive
			});
			if (res.ok) st.config = res.config;
			else st.error = res.error;
		} finally {
			st.busy = false;
			observe = observe;
		}
	}

	$: observing = observation?.status === 'observing';
	$: activeDeepObservation = observation?.deep_observation ? 'on' : 'off';
	$: anyActive = active.length > 0 || observing;
	$: activeUpcomingSessions = matchActiveSessionsToUpcoming(upcoming, active);
	$: upcomingTitleBySessionId = new Map(
		upcoming.flatMap((event) => {
			const session = activeUpcomingSessions.get(upcomingMeetingKey(event));
			return session ? [[session.session_id, event.title] as const] : [];
		})
	);
	$: liveUpcomingCount = upcoming.filter((event) => event.live_now).length;
	$: enabledObserveSources = [
		ambientStatus?.enabled,
		channelAssistChannelsEnabledCount > 0,
		observe.calendar.config?.enabled
	].filter(Boolean).length + observableSourceCount;
	$: activeCaptureCount = active.length + (observing ? 1 : 0);

	// ── the /observe retreat (gate M4) ────────────────────────────────
	// Which slot regions on this page currently render a ready app widget. The
	// region owner reports it; nothing here assumes a pinned default rendered.
	let filledSlotRegions: string[] = [];
	$: retreatedSections = observeRetreatedSections(OBSERVE_RETREAT_PAGE, filledSlotRegions);
	// The meetings app's own console, when it declared one. Section-placed
	// navigation belongs beside this page's other destinations, not in a tab.

	// ── unified recent list (🎙 meetings · 👁 observations) ───────────
	interface RecentItem {
		key: string;
		kind: 'meeting' | 'observation';
		title: string;
		meta: string;
		updated_at: number;
		open: () => void;
	}
	// When the pinned Recent-meetings widget renders, its rows are already on
	// this page and the intermingled list keeps only the observations. The
	// meetings half returns the instant the widget stops rendering.
	$: recentMeetingsRetreated = retreatedSections.has('recent_meetings');
	$: allRecentItems = (
		[
			...recentMeetings.map(
				(r): RecentItem => ({
					key: `m:${r.thread_id}`,
					kind: 'meeting',
					title: r.title ?? r.thread_id,
					meta: `${r.thread_id} · ${formatWhen(r.updated_at)}`,
					updated_at: r.updated_at,
					open: () => openDetails(r.thread_id)
				})
			),
			...recentWatch.map(
				(s): RecentItem => ({
					key: `o:${s.id}`,
					kind: 'observation',
					title: s.title ?? s.id,
					meta: formatWhen(s.updated_at ?? 0),
					updated_at: s.updated_at ?? 0,
					open: () => openThread('screen-watch')
				})
			)
		] as RecentItem[]
	)
		.sort((a, b) => b.updated_at - a.updated_at)
		.slice(0, 30);
	// The hero counter stays on the unfiltered set. The retreat moves where the
	// meeting rows are rendered; it must never make this page under-report how
	// much was captured.
	$: recentCaptureCount = allRecentItems.length;
	$: recentItems = recentMeetingsRetreated
		? allRecentItems.filter((item) => item.kind !== 'meeting')
		: allRecentItems;

	// Slow lane: the listings that change rarely (recent threads, calendar).
	// The live Active section rides the shared pollers' fast leases instead.
	let slowTimer: ReturnType<typeof setInterval> | null = null;
	const SLOW_POLL_MS = 30_000;

	type ObservePane = 'now' | 'sources' | 'audio' | 'notes';
	let pane: ObservePane = 'now';
	let paneLock: ObservePane | null = null;
	let refreshing = false;

	function paneFromUrl(value: string | null): ObservePane | null {
		if (value === 'now' || value === 'sources' || value === 'audio' || value === 'notes') {
			return value;
		}
		return null;
	}

	$: urlPane = paneFromUrl($page?.url?.searchParams?.get('pane') ?? null) ?? 'now';
	$: if (paneLock) {
		if (urlPane === paneLock) paneLock = null;
	} else if (urlPane !== pane) {
		pane = urlPane;
	}

	$: captureStatusLine = anyActive
		? `${activeCaptureCount} capture${activeCaptureCount === 1 ? '' : 's'} live`
		: liveUpcomingCount > 0
			? `${liveUpcomingCount} meeting${liveUpcomingCount === 1 ? '' : 's'} live — nothing capturing`
			: 'Quiet — nothing capturing';

	function setPane(next: ObservePane, anchor?: string) {
		paneLock = next;
		pane = next;
		if (!browser) return;
		try {
			const url = new URL(window.location.href);
			url.searchParams.set('pane', next);
			if (anchor) url.hash = anchor;
			else if (next === 'notes') url.hash = 'observe-notes';
			else url.hash = '';
			replaceState(url.toString(), {});
		} catch {
			// The address bar is a convenience. Pane state already changed.
		}
		if (anchor) {
			void tick().then(() => {
				document.getElementById(anchor)?.scrollIntoView({ block: 'start', inline: 'nearest' });
			});
		}
	}

	function toggleStart(verb: StartVerb) {
		startVerb = startVerb === verb ? null : verb;
	}

	async function refreshConsole(): Promise<void> {
		if (refreshing) return;
		refreshing = true;
		try {
			pollMeetingsActiveNow();
			pollObserveStatusNow();
			await Promise.all([
				refresh(),
				loadUpcoming(true),
				loadRecentWatch(),
				loadAmbientStatus(),
				loadObserveConnectors()
			]);
		} finally {
			refreshing = false;
		}
	}

	// ── meetings actions ──────────────────────────────────────────────
	/// Recent meeting threads (the `active` list comes from meetingsStore).
	async function refresh(): Promise<void> {
		try {
			recentMeetings = await fetchRecentMeetings();
			loadError = null;
		} catch (err) {
			loadError = err instanceof Error ? err.message : 'failed to load meetings';
		} finally {
			loading = false;
		}
	}

	function slowPoll(): void {
		void refresh();
		void loadUpcoming();
		void loadRecentWatch();
	}

	/// Calendar events for the next ~12h (server caches the gws round-trip).
	async function loadUpcoming(forceRefresh = false): Promise<void> {
		if (forceRefresh) {
			if (upcomingRefreshing) return;
			upcomingRefreshing = true;
		}
		try {
			const data = await fetchUpcomingMeetings(forceRefresh);
			upcoming = data.events;
			// Per-account failures stay visible even when others delivered.
			const errs = data.errors;
			upcomingError = errs.length
				? errs.map((e) => `${e.account ?? '?'}: ${e.error ?? 'failed'}`).join(' · ')
				: null;
		} catch (err) {
			upcomingError = err instanceof Error ? err.message : 'failed to load calendar';
		} finally {
			upcomingLoading = false;
			if (forceRefresh) upcomingRefreshing = false;
		}
	}

	async function startListen(): Promise<void> {
		actionBusy = 'listen';
		actionError = null;
		try {
			const metadata = resolveMeetingListenMetadata(listenTitle, listenUrl, upcoming);
			await startMeetingListen({
				title: metadata.title,
				url: metadata.url,
				mic: listenMic
			});
			if (metadata.inferredEvent) {
				showSuccess('Listening to scheduled meeting', metadata.inferredEvent.title);
			}
			listenTitle = '';
			listenUrl = '';
			startVerb = null;
			pollMeetingsActiveNow();
			await refresh();
		} catch (err) {
			actionError = err instanceof Error ? err.message : 'failed to start listener';
		} finally {
			actionBusy = null;
		}
	}

	async function startJoin(): Promise<void> {
		if (!joinUrl.trim()) {
			actionError = 'A meeting URL is required to join.';
			return;
		}
		actionBusy = 'join';
		actionError = null;
		try {
			await joinMeeting({
				url: joinUrl.trim(),
				title: joinTitle.trim() || null
			});
			joinUrl = '';
			joinTitle = '';
			startVerb = null;
			pollMeetingsActiveNow();
			await refresh();
		} catch (err) {
			actionError = err instanceof Error ? err.message : 'failed to join meeting';
		} finally {
			actionBusy = null;
		}
	}

	/// One-click start from a calendar event: title + link prefill the request.
	async function listenToEvent(ev: UpcomingMeeting): Promise<void> {
		listenTitle = ev.title;
		listenUrl = ev.meet_url ?? '';
		await startListen();
	}

	async function joinEvent(ev: UpcomingMeeting): Promise<void> {
		if (!ev.meet_url) return;
		joinUrl = ev.meet_url;
		joinTitle = ev.title;
		await startJoin();
	}

	/// Join the meeting yourself, as a human — just open the link in a new tab
	/// (distinct from "Send bot", which dispatches the agent attendee).
	function openMeetLink(url: string | null | undefined): void {
		if (!url) return;
		window.open(url, '_blank', 'noopener');
	}

	async function togglePause(s: ActiveMeetingSession): Promise<void> {
		pausingIds = new Set(pausingIds).add(s.session_id);
		try {
			await setMeetingPaused(s.session_id, Boolean(s.paused));
		} catch (err) {
			actionError = err instanceof Error ? err.message : 'failed to pause/resume';
		} finally {
			const next = new Set(pausingIds);
			next.delete(s.session_id);
			pausingIds = next;
			pollMeetingsActiveNow();
		}
	}

	async function stopSession(sessionId: string): Promise<void> {
		stoppingIds = new Set(stoppingIds).add(sessionId);
		try {
			await stopMeetingSession(sessionId);
		} catch (err) {
			actionError = err instanceof Error ? err.message : 'failed to stop session';
		} finally {
			const next = new Set(stoppingIds);
			next.delete(sessionId);
			stoppingIds = next;
			pollMeetingsActiveNow();
			await refresh();
		}
	}

	// ── observation actions ───────────────────────────────────────────
	async function loadRecentWatch(): Promise<void> {
		try {
			recentWatch = await fetchRecentWatchSessions();
		} catch {
			/* best-effort */
		}
	}

	async function startObservation(): Promise<void> {
		actionBusy = 'observe';
		observeBusy = true;
		actionError = null;
		try {
			const condition = watchFor.trim();
			const body: Parameters<typeof startScreenObservation>[0] = {
				purpose: purpose.trim() || undefined,
				mode: condition ? 'watch' : 'notes',
				deep_observation: observeDeep === 'on'
			};
			if (condition) body.watch_for = condition;
			if (observeAudio !== 'none') {
				body.audio = observeAudio;
			}
			await startScreenObservation(body);
			purpose = '';
			watchFor = '';
			startVerb = null;
			pollObserveStatusNow();
			await loadRecentWatch();
		} catch (err) {
			actionError = err instanceof Error ? err.message : String(err);
		} finally {
			actionBusy = null;
			observeBusy = false;
		}
	}

	function setActiveDeepObservation(event: Event): void {
		const select = event.currentTarget as HTMLSelectElement;
		void retarget({ deep_observation: select.value === 'on' });
	}

	async function stopObservation(): Promise<void> {
		observeBusy = true;
		actionError = null;
		try {
			await stopScreenObservation();
			pollObserveStatusNow();
			await loadRecentWatch();
		} catch (err) {
			actionError = err instanceof Error ? err.message : String(err);
		} finally {
			observeBusy = false;
		}
	}

	async function retarget(body: Record<string, unknown>): Promise<void> {
		observeBusy = true;
		actionError = null;
		try {
			await retargetScreenObservation(body);
			pollObserveStatusNow();
			retargetCondition = '';
		} catch (err) {
			actionError = err instanceof Error ? err.message : String(err);
		} finally {
			observeBusy = false;
		}
	}

	// ── ambient tab observation actions ──────────────────────────────
	async function loadAmbientStatus(): Promise<void> {
		try {
			ambientStatus = await fetchAmbientStatus();
			ambientDenylistText = (ambientStatus?.denylist ?? []).join('\n');
		} catch {
			/* non-fatal — card shows as unavailable */
		}
	}

	async function saveAmbient(enabled: boolean): Promise<void> {
		ambientBusy = true;
		actionError = null;
		try {
			const denylist = ambientDenylistText
				.split(/[\n,]/)
				.map((d) => d.trim())
				.filter(Boolean);
			await saveAmbientConfig(enabled, denylist);
			await loadAmbientStatus();
		} catch (err) {
			actionError = err instanceof Error ? err.message : String(err);
		} finally {
			ambientBusy = false;
		}
	}

	// Issue a collector token bound to this scope; the user pastes it into the
	// browser extension so its uploads can't claim a different principal.
	async function pairBrowser(): Promise<void> {
		ambientBusy = true;
		actionError = null;
		ambientPairToken = null;
		try {
			ambientPairToken = await enrollAmbientBrowser();
			tabsDetails = true;
			await loadAmbientStatus();
		} catch (err) {
			actionError = err instanceof Error ? err.message : String(err);
		} finally {
			ambientBusy = false;
		}
	}

	// ── shared helpers ────────────────────────────────────────────────
	function openThread(threadId: string | null): void {
		if (threadId) goto(`/t/${encodeURIComponent(threadId)}`);
	}

	function openDetails(threadId: string | null): void {
		if (threadId) goto(`/meetings/${encodeURIComponent(threadId)}`);
	}

	function activeSessionLabel(session: ActiveMeetingSession): string {
		return (
			session.title ??
			upcomingTitleBySessionId.get(session.session_id) ??
			meetingSessionLabel(session)
		);
	}

	function formatWhen(ms: number): string {
		try {
			return new Date(ms).toLocaleString(undefined, {
				month: 'short',
				day: 'numeric',
				hour: '2-digit',
				minute: '2-digit'
			});
		} catch {
			return '';
		}
	}

	/// "09:30 – 10:00" from the event's RFC3339 bounds.
	function formatEventWindow(ev: UpcomingMeeting): string {
		const fmt = (s: string | null): string => {
			if (!s) return '';
			try {
				return new Date(s).toLocaleTimeString(undefined, {
					hour: '2-digit',
					minute: '2-digit'
				});
			} catch {
				return '';
			}
		};
		const start = fmt(ev.start);
		const end = fmt(ev.end);
		return end ? `${start} – ${end}` : start;
	}

	function elapsedLabel(startedMs?: number): string {
		if (!startedMs) return '';
		const mins = Math.max(0, Math.round((Date.now() - startedMs) / 60000));
		return mins < 60 ? `${mins}m` : `${Math.floor(mins / 60)}h ${mins % 60}m`;
	}

	function compactNumber(value: number | null | undefined): string {
		return new Intl.NumberFormat(undefined, { notation: 'compact' }).format(value ?? 0);
	}

	function byteLabel(value: number | null | undefined): string {
		const bytes = value ?? 0;
		if (bytes < 1024) return `${bytes} B`;
		const units = ['KB', 'MB', 'GB'];
		let amount = bytes / 1024;
		let idx = 0;
		while (amount >= 1024 && idx < units.length - 1) {
			amount /= 1024;
			idx += 1;
		}
		return `${amount >= 10 ? amount.toFixed(0) : amount.toFixed(1)} ${units[idx]}`;
	}

	// Fast-cadence leases on the shared pollers while this page is open —
	// released on destroy so background pages drop back to the idle rate.
	let releaseFastMeetings: (() => void) | null = null;
	let releaseFastObserve: (() => void) | null = null;

	onMount(() => {
		const hash = window.location.hash.replace('#', '');
		const explicitPane = paneFromUrl(new URL(window.location.href).searchParams.get('pane'));
		if (!explicitPane && hash === 'observe-notes') setPane('notes');
		releaseFastMeetings = requestFastMeetingsPolling();
		releaseFastObserve = requestFastObservePolling();
		slowPoll();
		slowTimer = setInterval(slowPoll, SLOW_POLL_MS);
		void loadAmbientStatus();
		void loadObserveConnectors();
		// Best-effort: a fetch failure keeps the backend-matching fallback.
		void (async () => {
			try {
				const agents = await loadAgents();
				const personal =
					agents.find((a) => a.is_primary) ?? agents.find((a) => a.kind === 'Personal');
				const name = personal?.name?.trim() || personal?.aliases?.[0]?.trim();
				if (name) agentName = name;
			} catch {
				/* keep fallback */
			}
		})();
	});
	onDestroy(() => {
		if (slowTimer) clearInterval(slowTimer);
		releaseFastMeetings?.();
		releaseFastObserve?.();
	});
</script>

<div class="observe-page">
	<header class="observe-command">
		<div class="observe-command__copy">
			<p class="observe-kicker">Observation</p>
			<div class="observe-command__title-row">
				<h1>Observe</h1>
				<p class="observe-status">{captureStatusLine}</p>
			</div>
		</div>
		<div class="observe-command__actions">
			<a
				class="action-button action-button--outline action-button--sm"
				href="/resurfacing"
				title="View proactive resurfacing engine status and queues"
			>
				<Icon name="rotate-ccw" size={13} />
				<span>Resurfacing</span>
			</a>
			<a
				class="action-button action-button--outline action-button--sm"
				href="/observe/stats"
				title="View observation pipeline stages and activity"
			>
				<Icon name="git-branch" size={13} />
				<span>Pipelines</span>
			</a>
			<button
				type="button"
				class="action-button action-button--outline action-button--sm"
				disabled={refreshing}
				aria-label="Refresh observation"
				title="Refresh meetings, calendar, screen watches, tabs, and channels"
				on:click={() => void refreshConsole()}
			>
				<Icon name="rotate-ccw" size={13} />
				<span>{refreshing ? 'Refreshing…' : 'Refresh'}</span>
			</button>
		</div>
	</header>

	{#if loadError}
		<div class="banner banner--error">Backend unreachable: {loadError}</div>
	{/if}
	{#if actionError}
		<div class="banner banner--error">
			{actionError}
			<button type="button" class="banner-dismiss" on:click={() => (actionError = null)}>×</button>
		</div>
	{/if}

	<section class="observe-kpis" aria-label="Observation status">
		<button
			type="button"
			class="observe-kpi observe-kpi--now"
			class:observe-kpi--active={pane === 'now'}
			aria-pressed={pane === 'now'}
			on:click={() => setPane('now', anyActive ? 'observe-active' : undefined)}
		>
			<div class="observe-kpi__top">
				<span class="observe-kpi__icon-badge" class:observe-kpi__icon-badge--live={anyActive} aria-hidden="true">
					<Icon name="zap" size={15} />
				</span>
				<span class="observe-kpi__label">Now &amp; Live</span>
				{#if anyActive}
					<span class="observe-kpi__live-pill">LIVE</span>
				{/if}
			</div>
			<div class="observe-kpi__body">
				<strong class="observe-kpi__metric">{activeCaptureCount}</strong>
				<span class="observe-kpi__sub">
					{anyActive
						? `${activeCaptureCount} live capture${activeCaptureCount === 1 ? '' : 's'}`
						: liveUpcomingCount > 0
							? `${liveUpcomingCount} live meeting${liveUpcomingCount === 1 ? '' : 's'}`
							: 'Live captures & meetings'}
				</span>
			</div>
			<span class="observe-kpi__action" aria-hidden="true">Open live deck →</span>
		</button>
		<button
			type="button"
			class="observe-kpi observe-kpi--sources"
			class:observe-kpi--active={pane === 'sources'}
			aria-pressed={pane === 'sources'}
			on:click={() => setPane('sources')}
		>
			<div class="observe-kpi__top">
				<span class="observe-kpi__icon-badge" aria-hidden="true">
					<Icon name="sliders" size={15} />
				</span>
				<span class="observe-kpi__label">Sources on</span>
			</div>
			<div class="observe-kpi__body">
				<strong class="observe-kpi__metric">{enabledObserveSources}</strong>
				<span class="observe-kpi__sub">Channels, tabs &amp; feeds</span>
			</div>
			<span class="observe-kpi__action" aria-hidden="true">Configure sources →</span>
		</button>
		<button
			type="button"
			class="observe-kpi observe-kpi--audio"
			class:observe-kpi--active={pane === 'audio'}
			aria-pressed={pane === 'audio'}
			on:click={() => setPane('audio')}
		>
			<div class="observe-kpi__top">
				<span class="observe-kpi__icon-badge" aria-hidden="true">
					<Icon name="mic" size={15} />
				</span>
				<span class="observe-kpi__label">Audio Profiles</span>
			</div>
			<div class="observe-kpi__body">
				<strong class="observe-kpi__metric">2</strong>
				<span class="observe-kpi__sub">Meeting &amp; listening STT</span>
			</div>
			<span class="observe-kpi__action" aria-hidden="true">Configure audio →</span>
		</button>
		<button
			type="button"
			class="observe-kpi observe-kpi--notes"
			class:observe-kpi--active={pane === 'notes'}
			aria-pressed={pane === 'notes'}
			on:click={() => setPane('notes')}
		>
			<div class="observe-kpi__top">
				<span class="observe-kpi__icon-badge" aria-hidden="true">
					<Icon name="file-text" size={15} />
				</span>
				<span class="observe-kpi__label">Notes &amp; Recents</span>
			</div>
			<div class="observe-kpi__body">
				<strong class="observe-kpi__metric">{recentCaptureCount}</strong>
				<span class="observe-kpi__sub">Observations &amp; journals</span>
			</div>
			<span class="observe-kpi__action" aria-hidden="true">Browse notes →</span>
		</button>
	</section>

	{#if anyActive}
		{@render activeCaptures(true)}
	{/if}
	{#if !anyActive}
		{@render activeCaptures(false)}
	{/if}

	<div class="observe-tabs" role="tablist" aria-label="Observe sections">
		<button
			type="button"
			role="tab"
			class="observe-tab"
			class:observe-tab--active={pane === 'now'}
			aria-selected={pane === 'now'}
			on:click={() => setPane('now')}
		>
			<Icon name="zap" size={15} />
			<span>Now</span>
			{#if activeCaptureCount > 0}<span class="tab-count">{activeCaptureCount}</span>{/if}
		</button>
		<button
			type="button"
			role="tab"
			class="observe-tab"
			class:observe-tab--active={pane === 'sources'}
			aria-selected={pane === 'sources'}
			on:click={() => setPane('sources')}
		>
			<Icon name="sliders" size={15} />
			<span>Sources</span>
			<span class="tab-count">{enabledObserveSources}</span>
		</button>
		<button
			type="button"
			role="tab"
			class="observe-tab"
			class:observe-tab--active={pane === 'audio'}
			aria-selected={pane === 'audio'}
			on:click={() => setPane('audio')}
		>
			<Icon name="mic" size={15} />
			<span>Audio</span>
		</button>
		<button
			type="button"
			role="tab"
			class="observe-tab"
			class:observe-tab--active={pane === 'notes'}
			aria-selected={pane === 'notes'}
			on:click={() => setPane('notes')}
		>
			<Icon name="file-text" size={15} />
			<span>Notes</span>
		</button>
	</div>

	<div class="observe-pane" role="tabpanel" hidden={pane !== 'now'}>
		<section class="start-panel" aria-label="Start a capture">
			<div class="surface-card start-card-container">
				<div class="start-card-header">
					<div class="card-title-row">
						<span class="card-icon"><Icon name="play" size={15} /></span>
						<h2 class="section-head-title">Capture launchpad</h2>
					</div>
					<p class="hint">Instant triggers to listen, join as agent, or watch screen</p>
				</div>
				<div class="start-row" role="group" aria-label="Start a capture">
					<button
						type="button"
						class="start-verb"
						class:start-verb--active={startVerb === 'listen'}
						aria-expanded={startVerb === 'listen'}
						aria-label="Listen"
						on:click={() => toggleStart('listen')}
					>
						<span class="start-verb__icon" aria-hidden="true"><Icon name="mic" size={15} /></span>
						<span class="start-verb__text-group">
							<span class="start-verb__title">Listen</span>
							<span class="start-verb__meta" aria-hidden="true">Audio &amp; mic capture</span>
						</span>
					</button>
					<button
						type="button"
						class="start-verb"
						class:start-verb--active={startVerb === 'join'}
						aria-expanded={startVerb === 'join'}
						aria-label={`Join as ${agentName}`}
						on:click={() => toggleStart('join')}
					>
						<span class="start-verb__icon" aria-hidden="true"><Icon name="message" size={15} /></span>
						<span class="start-verb__text-group">
							<span class="start-verb__title">Join as {agentName}</span>
							<span class="start-verb__meta" aria-hidden="true">Agent attendee</span>
						</span>
					</button>
					<button
						type="button"
						class="start-verb"
						class:start-verb--active={startVerb === 'watch'}
						aria-expanded={startVerb === 'watch'}
						aria-label={observing ? 'Watching' : 'Watch screen'}
						title={observing ? 'An observation is already running' : 'Watch the screen'}
						on:click={() => toggleStart('watch')}
					>
						<span class="start-verb__icon" aria-hidden="true"><Icon name="monitor" size={15} /></span>
						<span class="start-verb__text-group">
							<span class="start-verb__title">{observing ? 'Watching' : 'Watch screen'}</span>
							<span class="start-verb__meta" aria-hidden="true">Vision journal &amp; alerts</span>
						</span>
					</button>
				</div>
			</div>

			{#if startVerb === 'listen'}
				<div class="surface-card start-form">
					<div class="start-card">
						<div class="card-head">
							<div class="card-title-row">
								<span class="card-icon"><Icon name="clock" size={15} /></span>
								<h2>Listen to meeting</h2>
							</div>
						</div>
						<p class="hint">
							Captures this Mac’s system audio — never joins the call, no bot tile.
							Needs Screen Recording (plus Microphone for the optional “You” track).
						</p>
						<input class="native-input" placeholder="Meeting title" bind:value={listenTitle} />
						<input class="native-input" placeholder="Meeting URL" bind:value={listenUrl} />
						<div class="mic-toggle">
							<label class="native-checkbox">
								<input
									class="native-checkbox-input"
									type="checkbox"
									checked={listenMic}
									on:change={(event) => (listenMic = checkedFrom(event))}
								/>
								<span>Also capture my microphone as the “You” track</span>
							</label>
						</div>
						{#if listenMic}
							<p class="hint hint--warn">
								Reads the mic device directly — the meeting app's mute does
								<strong>not</strong> stop it, and background voices in your room
								will be transcribed too.
							</p>
						{/if}
						<button
							class="action-button action-button--primary"
							disabled={actionBusy !== null}
							on:click={() => void startListen()}
						>
							<Icon name="play" size={14} />
							<span>{actionBusy === 'listen' ? 'Starting…' : 'Start listening'}</span>
						</button>
					</div>
				</div>
			{:else if startVerb === 'join'}
				<div class="surface-card start-form">
					<div class="start-card">
						<div class="card-head">
							<div class="card-title-row">
								<span class="card-icon"><Icon name="message" size={15} /></span>
								<h2>Join as {agentName}</h2>
							</div>
						</div>
						<p class="hint">
							{agentName} joins the meeting as a visible participant, transcribes it,
							and speaks when addressed (“Hey {agentName}…”). Google Meet only for now.
						</p>
						<input class="native-input" placeholder="Meeting URL" bind:value={joinUrl} />
						<input class="native-input" placeholder="Meeting title" bind:value={joinTitle} />
						<button
							class="action-button action-button--primary"
							disabled={actionBusy !== null || !joinUrl.trim()}
							on:click={() => void startJoin()}
						>
							<Icon name="arrow-right" size={14} />
							<span>{actionBusy === 'join' ? 'Joining…' : `Join as ${agentName}`}</span>
						</button>
					</div>
				</div>
			{:else if startVerb === 'watch'}
				<div class="surface-card start-form">
					<div class="start-card">
						<div class="card-head">
							<div class="card-title-row">
								<span class="card-icon"><Icon name="monitor" size={15} /></span>
								<h2>Watch screen</h2>
							</div>
						</div>
						<p class="hint">
							Analyzes only <em>changed</em> frames; the narration is what's kept.
							{watchFor.trim()
								? '⚠ Watch mode: alerts when the condition matches, then stops.'
								: 'Notes mode (default): a silent work journal — never interrupts.'}
						</p>
						<input class="native-input" placeholder="What are you working on?" bind:value={purpose} />
						<input class="native-input" placeholder="Alert me when…" bind:value={watchFor} />
						<div class="audio-controls">
							<label class="field">
								<span class="field-label">Deep read</span>
								<select bind:value={observeDeep}>
									<option value="off">Off</option>
									<option value="on">Stable screen · 20s / 5m</option>
								</select>
							</label>
							<label class="field">
								<span class="field-label">Also hear</span>
								<select bind:value={observeAudio}>
									<option value="none">Nothing (screen only)</option>
									<option value="system">System audio</option>
									<option value="mic">Microphone</option>
									<option value="both">System + Mic</option>
								</select>
							</label>
						</div>
						{#if observeAudio === 'mic' || observeAudio === 'both'}
							<p class="hint hint--warn">
								Reads the mic device directly — background voices in your room are
								transcribed too. <strong>Microphone</strong> permission required.
							</p>
						{:else if observeAudio === 'system'}
							<p class="hint">
								System audio uses the Screen Recording permission you already grant
								for watching — no extra prompt.
							</p>
						{/if}
						<button
							class="action-button action-button--primary"
							disabled={actionBusy !== null || observing}
							title={observing ? 'An observation is already running (see Active)' : ''}
							on:click={() => void startObservation()}
						>
							<Icon name="eye" size={14} />
							<span>{actionBusy === 'observe' ? 'Starting…' : 'Start observing'}</span>
						</button>
					</div>
				</div>
			{/if}
		</section>

		<div class="surface-card observe-section-card" id="observe-upcoming">
			<section class="section-panel">
				<h2 class="section-head">
					<span><Icon name="calendar" size={16} /> Upcoming</span>
					<button
						type="button"
						class="action-button action-button--outline action-button--sm"
						class:action-button--icon={!upcomingRefreshing}
						aria-label="Re-read calendar"
						title="Re-read calendar"
						disabled={upcomingRefreshing}
						on:click={() => void loadUpcoming(true)}
					>
						{#if upcomingRefreshing}
							<span>Refreshing…</span>
						{:else}
							<Icon name="rotate-ccw" size={14} />
						{/if}
					</button>
				</h2>
				{#if upcomingLoading}
					<div class="loading-inline">
						<span class="loading-spinner" aria-hidden="true"></span>
						<span>Reading calendar</span>
					</div>
				{:else if upcoming.length === 0}
					{#if upcomingError}
						<div class="empty-state">
							<span class="empty-state-icon" aria-hidden="true">!</span>
							<h3>Calendar unavailable</h3>
							<p>{upcomingError}</p>
						</div>
					{:else}
						<div class="empty-state">
							<span class="empty-state-icon" aria-hidden="true">○</span>
							<h3>No meetings</h3>
							<p>No calendar meetings in the next 12 hours.</p>
						</div>
					{/if}
				{:else}
					<ul class="upcoming-list">
						{#each upcoming as ev (upcomingMeetingKey(ev))}
							{@const activeSession = activeUpcomingSessions.get(upcomingMeetingKey(ev)) ?? null}
							<li class="upcoming-row" class:upcoming-row--live={ev.live_now}>
								<div class="upcoming-info">
									<span class="upcoming-title">
										{ev.title}
										{#if ev.live_now}<span class={badgeClass('error')}>now</span>{/if}
									</span>
									<span class="upcoming-meta">
										{formatEventWindow(ev)}{#if ev.account}&nbsp;· {ev.account}{/if}{#if ev.meet_url}&nbsp;· {ev.meet_url}{/if}
									</span>
								</div>
								<div class="upcoming-actions">
									{#if activeSession}
										<span class={badgeClass(activeSession.paused ? 'warning' : 'success')}>
											{activeSession.paused
												? 'Paused'
												: activeSession.mode === 'attendee'
													? `Joined as ${agentName}`
													: 'Listening now'}
										</span>
										{#if activeSession.thread_id}
											<button
												class="action-button action-button--outline action-button--sm"
												on:click={() => openThread(activeSession.thread_id)}
											>
												<Icon name="message" size={14} />
												<span>Open transcript</span>
											</button>
										{/if}
									{:else}
										{#if ev.meet_url}
											<button
												class="action-button action-button--primary action-button--sm"
												title="Join the meeting yourself"
												on:click={() => openMeetLink(ev.meet_url)}
											>
												<Icon name="arrow-up-right" size={14} />
												<span>Join (Me)</span>
											</button>
										{/if}
										<button
											class="action-button action-button--outline action-button--sm"
											disabled={actionBusy !== null}
											on:click={() => void listenToEvent(ev)}
										>
											Listen
										</button>
										<button
											class="action-button action-button--outline action-button--sm"
											disabled={actionBusy !== null || !ev.meet_url}
											title={ev.meet_url ? 'Send the agent to join' : 'No Meet link on this event'}
											on:click={() => void joinEvent(ev)}
										>
											Send bot
										</button>
									{/if}
								</div>
							</li>
						{/each}
					</ul>
					{#if upcomingError}
						<p class="upcoming-warn">Some calendars failed — {upcomingError}</p>
					{/if}
				{/if}
			</section>
		</div>

		<!-- ONE batched region owner for this page (gate M4). `capture` and
		     `history` are the slots the meetings package pins as workspace
		     defaults; adding them here is what lets those widgets stand on the
		     page at all. They ride the existing owner rather than three separate
		     ones so the page keeps its single slot-resolve + render batch. -->
		<AppSlotRegion
			page={OBSERVE_RETREAT_PAGE}
			regions={['capture', 'reviews', 'history']}
			ariaLabel="Observe app widgets"
			on:filled={(event) => (filledSlotRegions = event.detail.regions)}
		/>

		<div class="surface-card observe-section-card" id="observe-recent">
			<section class="section-panel">
				<h2 class="section-head">
					<span><Icon name="clock" size={16} /> Recent</span>
					{#if recentMeetingsRetreated}
						<small class="section-note">Meetings are listed by the Meetings app widget above.</small>
					{/if}
				</h2>
				{#if recentItems.length === 0}
					<div class="empty-state">
						<span class="empty-state-icon" aria-hidden="true">○</span>
						<h3>{recentMeetingsRetreated ? 'No observations yet' : 'Nothing captured yet'}</h3>
						<p>
							{recentMeetingsRetreated
								? 'Screen observations appear here once captured.'
								: 'Meetings and observations appear here once captured.'}
						</p>
					</div>
				{:else}
					<ul class="recent-list">
						{#each recentItems as item (item.key)}
							<li>
								<button class="recent-row" on:click={item.open}>
									<span class="kind-chip" class:kind-chip--observe={item.kind === 'observation'} aria-hidden="true">
										<Icon name={item.kind === 'meeting' ? 'message' : 'eye'} size={14} />
									</span>
									<span class="recent-title">{item.title}</span>
									<span class="recent-meta">{item.meta}</span>
								</button>
							</li>
						{/each}
					</ul>
				{/if}
			</section>
		</div>
	</div>

	<div class="observe-pane" role="tabpanel" hidden={pane !== 'sources'}>
		<ObservableSourcesPanel
			on:countchange={(event) => {
				const enabled = event.detail.enabled;
				observableSourceCount =
					typeof enabled === 'number' && Number.isFinite(enabled) ? enabled : 0;
			}}
		/>

		<div class="account-grid">
			<div class="surface-card account-card">
				<div class="start-card">
					<div class="card-head">
						<div class="card-title-row">
							<span class="card-icon"><Icon name="message" size={15} /></span>
							<h2>Mail &amp; chat</h2>
						</div>
						<span class={badgeClass(channelAssistChannelsEnabledCount > 0 ? 'success' : 'default')}>
							{channelAssistChannelsEnabledCount > 0 ? `${channelAssistChannelsEnabledCount} ON` : 'OFF'}
						</span>
					</div>
					<p class="hint">
						The one place you pick which inboxes your assistant follows — your Gmail, your
						WhatsApp, and Presto's own inboxes (the <strong>Presto</strong> lane). Each
						account feeds <strong>both</strong> your work evidence and mail/chat assist
						(follow-ups, drafts). Full content is read but only a <strong>local</strong>
						summary is kept — never raw bodies, never a remote model. Sensitive mail is
						suppressed.
					</p>
					<label class="field">
						<span class="field-label">Accounts to follow</span>
						{#if channelAssistChannels.length === 0 && !channelAssistChannelsError}
							<p class="hint">No channels discovered yet.</p>
						{/if}
						{#each channelAssistChannels as c, i}
							<div class="acct-row">
								<label class="native-checkbox">
									<input
										class="native-checkbox-input"
										type="checkbox"
										checked={c.enabled}
										disabled={!c.connected}
										on:change={() => toggleChannelAssistChannel(i)}
									/>
									<span>
										{channelProviderLabel(c.provider, c)} · {c.display}
										<span class={badgeClass(c.lane === 'envoy' ? 'info' : 'default')}
											>{laneLabel(c.lane)}</span
										>
										{#if !c.connected}<span class="hint"> — not connected</span>{/if}
										{#if c.message_count > 0}<span class="hint"> · {c.message_count} synced</span>{/if}
									</span>
								</label>
								{#if supportsVerificationCodes(c)}
									<label class="native-checkbox acct-purpose" title="Let the backend read a one-time login code from this account while a verification request is open. Observation alone never grants this.">
										<input
											class="native-checkbox-input"
											type="checkbox"
											checked={hasVerificationCodesPurpose(c)}
											disabled={verificationPurposeBusy === `${c.provider}:${c.account_alias}` ||
												(!c.enabled && !hasVerificationCodesPurpose(c))}
											aria-label={`Use ${c.display} for verification codes`}
											on:change={() => void toggleVerificationCodes(i)}
										/>
										<span class="hint">Use for verification codes</span>
									</label>
								{/if}
							</div>
						{/each}
					</label>
					{#if channelAssistChannelsError}<p class="hint hint--warn">{channelAssistChannelsError}</p>{/if}
					<button
						class="action-button action-button--primary"
						disabled={channelAssistChannelsBusy || !channelAssistChannelsDirty}
						on:click={() => void saveChannelAssistChannelsNow()}
					>
						<Icon name="check" size={14} />
						<span>{channelAssistChannelsBusy ? 'Saving…' : 'Save channels'}</span>
					</button>
				</div>
			</div>

			<div class="surface-card account-card">
				<div class="start-card">
					<div class="card-head">
						<div class="card-title-row">
							<span class="card-icon"><Icon name="calendar" size={15} /></span>
							<h2>Observe calendar</h2>
						</div>
						{#if observe.calendar.config}
							<span class={badgeClass(observe.calendar.config.enabled ? 'success' : 'default')}>
								{observe.calendar.config.enabled ? 'ON' : 'OFF'}
							</span>
						{/if}
					</div>
					<p class="hint">
						Builds work evidence from calendars — event titles, attendees, and cadence
						on a schedule. Pick your own calendars (the <strong>You</strong> lane) and,
						if you like, Presto's own calendar (the <strong>Presto</strong> lane). Only
						the accounts you pick are read.
					</p>
					{#if observe.calendar.config?.last_sync_at}
						<p class="hint">
							{observe.calendar.config.total_synced} synced · last {formatWhen(
								new Date(observe.calendar.config.last_sync_at).getTime()
							)}.
						</p>
					{/if}
					<label class="field">
						<span class="field-label">Accounts to observe</span>
						{#if observeAccounts.calendar_accounts.length === 0}
							<p class="hint">No connected calendar accounts found.</p>
						{/if}
						{#each observeAccounts.calendar_accounts as a}
							<div class="acct-row">
								<label class="native-checkbox">
									<input
										class="native-checkbox-input"
										type="checkbox"
										checked={observe.calendar.selected.has(a.name)}
										disabled={!a.connected}
										on:change={() => toggleObserveAccount('calendar', a.name)}
									/>
									<span>
										{a.name}{a.email ? ` (${a.email})` : ''}
										<span class={badgeClass(a.lane === 'envoy' ? 'info' : 'default')}
											>{laneLabel(a.lane)}</span
										>
										{#if !a.connected}<span class="hint"> — not connected</span>{/if}
									</span>
								</label>
							</div>
						{/each}
					</label>
					<label class="field">
						<span class="field-label">Frequency</span>
						<select bind:value={observe.calendar.frequency}>
							<option value="daily">Daily</option>
							<option value="twice-daily">Twice daily</option>
							<option value="hourly">Hourly</option>
						</select>
					</label>
					{#if observe.calendar.frequency !== 'hourly'}
						<label class="field">
							<span class="field-label">Time</span>
							<input class="time-input" type="time" bind:value={observe.calendar.time} />
						</label>
					{/if}
					{#if observe.calendar.error}<p class="hint hint--warn">{observe.calendar.error}</p>{/if}
					<button
						class="action-button action-button--primary"
						disabled={observe.calendar.busy}
						on:click={() =>
							void saveObserve('calendar', !(observe.calendar.config?.enabled ?? false))}
					>
						<Icon name={observe.calendar.config?.enabled ? 'square' : 'play'} size={14} />
						<span>
							{observe.calendar.busy
								? 'Saving…'
								: observe.calendar.config?.enabled
									? 'Pause calendar observation'
									: 'Enable calendar observation'}
						</span>
					</button>
					{#if observe.calendar.config?.enabled}
						<button
							class="action-button action-button--outline"
							disabled={observe.calendar.busy}
							on:click={() => void saveObserve('calendar', true)}
						>
							<Icon name="check" size={14} />
							<span>Save settings</span>
						</button>
					{/if}
				</div>
			</div>
		</div>

		<section class="surface-card tabs-status" aria-labelledby="tabs-observe-title">
			<div class="tabs-status__row">
				<div class="tabs-status__copy">
					<div class="card-title-row">
						<span class="card-icon"><Icon name="eye" size={15} /></span>
						<h2 id="tabs-observe-title">Observe tabs</h2>
						{#if ambientStatus}
							<span class={badgeClass(ambientStatus.enabled ? 'success' : 'default')}>
								{ambientStatus.enabled ? 'ON' : 'OFF'}
							</span>
						{/if}
					</div>
					<p class="hint">
						{#if ambientStatus}
							{compactNumber(ambientStatus.total_signals)} signal{ambientStatus.total_signals === 1 ? '' : 's'}
							captured{ambientStatus.last_signal_at
								? ` · last ${formatWhen(new Date(ambientStatus.last_signal_at).getTime())}`
								: ''}{ambientStatus.total_rejected
								? ` · ${compactNumber(ambientStatus.total_rejected)} filtered`
								: ''}.
						{:else}
							Browser tab evidence is unavailable.
						{/if}
					</p>
				</div>
				<div class="tabs-status__actions">
					<button
						type="button"
						class="action-button action-button--primary action-button--sm"
						disabled={ambientBusy}
						on:click={() => void saveAmbient(!(ambientStatus?.enabled ?? false))}
					>
						<Icon name={ambientStatus?.enabled ? 'square' : 'play'} size={14} />
						<span>
							{ambientBusy
								? 'Saving…'
								: ambientStatus?.enabled
									? 'Pause'
									: 'Enable'}
						</span>
					</button>
					<button
						type="button"
						class="action-button action-button--secondary action-button--sm"
						disabled={ambientBusy}
						on:click={() => void pairBrowser()}
					>
						<Icon name="settings" size={14} />
						<span>{ambientStatus?.paired ? 'Pair another browser' : 'Pair browser'}</span>
					</button>
					<button
						type="button"
						class="action-button action-button--outline action-button--sm"
						aria-expanded={tabsDetails}
						on:click={() => (tabsDetails = !tabsDetails)}
					>
						<span>{tabsDetails ? 'Hide details' : 'Details'}</span>
					</button>
				</div>
			</div>
			{#if ambientPairToken}
				<label class="field">
					<span class="field-label">
						Pairing token — paste into the extension popup (shown once; binds that
						browser's uploads to this scope)
					</span>
					<textarea class="native-textarea" rows="2" readonly>{ambientPairToken}</textarea>
				</label>
			{/if}
			{#if tabsDetails}
				<div class="tabs-status__details">
					<p class="hint">
						Builds work evidence from your normal browsing — metadata-first (page
						title, origin, structure), never full HTML or screenshots.
						Private/incognito windows are always excluded; capture only runs while
						this is on and the browser extension is paired.
					</p>
					{#if ambientStatus}
						<div class="ambient-metrics" aria-label="Tab observation metrics">
							<div class="ambient-metric">
								<span class="ambient-metric-value">{compactNumber(ambientStatus.accepted_today)}</span>
								<span class="ambient-metric-label">Today</span>
							</div>
							<div class="ambient-metric">
								<span class="ambient-metric-value">{compactNumber(ambientStatus.distinct_pages_today)}</span>
								<span class="ambient-metric-label">Pages</span>
							</div>
							<div class="ambient-metric">
								<span class="ambient-metric-value">{compactNumber(ambientStatus.origins_today)}</span>
								<span class="ambient-metric-label">Origins</span>
							</div>
							<div class="ambient-metric">
								<span class="ambient-metric-value">{byteLabel(ambientStatus.bytes_today?.signal_metadata_bytes)}</span>
								<span class="ambient-metric-label">Kept</span>
							</div>
							<div class="ambient-metric">
								<span class="ambient-metric-value">{compactNumber(ambientStatus.buffered_signals)}</span>
								<span class="ambient-metric-label">Buffered</span>
							</div>
							<div class="ambient-metric">
								<span class="ambient-metric-value">
									{ambientStatus.worker?.running
										? 'Running'
										: ambientStatus.worker?.last_run_status ?? 'Idle'}
								</span>
								<span class="ambient-metric-label">Distill</span>
							</div>
							<div class="ambient-metric">
								<span class="ambient-metric-value">{compactNumber(ambientStatus.pending?.clusters_due)}</span>
								<span class="ambient-metric-label">Pending</span>
							</div>
							<div class="ambient-metric">
								<span class="ambient-metric-value">{compactNumber(ambientStatus.pending?.review_pending)}</span>
								<span class="ambient-metric-label">Review</span>
							</div>
							<div class="ambient-metric">
								<span class="ambient-metric-value">
									{compactNumber(ambientStatus.llm_today?.calls)} · ${(
										ambientStatus.llm_today?.cost_usd ?? 0
									).toFixed(2)}
								</span>
								<span class="ambient-metric-label">LLM</span>
							</div>
						</div>
					{/if}
					<label class="field">
						<span class="field-label">Denylist (one origin per line — never captured)</span>
						<textarea
							class="native-textarea"
							rows={2}
							placeholder="bank.com&#10;mail.example.com"
							bind:value={ambientDenylistText}
						></textarea>
					</label>
					{#if ambientStatus?.enabled}
						<button
							type="button"
							class="action-button action-button--outline"
							disabled={ambientBusy}
							on:click={() => void saveAmbient(true)}
						>
							<Icon name="check" size={14} />
							<span>Save denylist</span>
						</button>
					{/if}
				</div>
			{/if}
		</section>

		<ObserveCatchUpPanel
			on:policychange={(event) => (channelAssistHistoryLookbackDays = event.detail.lookback_days)}
		/>
	</div>

	<div class="observe-pane" role="tabpanel" hidden={pane !== 'audio'}>
		<div class="surface-card audio-pane-card">
			<div class="audio-pane-header">
				<div class="card-title-row">
					<span class="card-icon"><Icon name="mic" size={16} /></span>
					<h2>Audio capture &amp; transcription profiles</h2>
				</div>
				<p class="hint">
					Configure speech-to-text engines, voice detection stages, and audio routing for meeting recording and passive listening.
				</p>
			</div>
			<section class="audio-surface-bar" aria-label="Observation audio profiles">
				<SurfaceAudioProfileControl surface="meeting" compact={true} showStages={true} />
				<SurfaceAudioProfileControl surface="listening" compact={true} showStages={true} />
			</section>
		</div>
	</div>

	<div class="observe-pane" role="tabpanel" hidden={pane !== 'notes'}>
		<ObserveNotesPanel />
	</div>

	{#snippet activeCaptures(promoted: boolean)}
		{#if anyActive || (loading && !anyActive) || (!observing && observation?.status === 'stopped' && observation.latest_summary)}
			<div
				id="observe-active"
				class="surface-card observe-section-card observe-active-section"
				class:observe-active-section--promoted={promoted}
				aria-live="polite"
			>
				<section class="section-panel">
					{#if anyActive || (loading && !anyActive)}
						<h2 class="section-head">
							<span><Icon name="zap" size={16} /> Active</span>
							{#if anyActive}<RecordingDot />{/if}
						</h2>
					{/if}
					{#if loading && !anyActive}
						<div class="loading-inline">
							<span class="loading-spinner" aria-hidden="true"></span>
							<span>Loading active captures</span>
						</div>
					{:else if anyActive}
						<div class="active-grid">
							{#each active as s (s.session_id)}
								<div class="session-card">
									<div class="session-head">
										<span class={badgeClass(s.mode === 'attendee' ? 'info' : 'default')}>
											{s.mode === 'attendee' ? `Joined as ${agentName}` : 'Passive listening'}
										</span>
										{#if s.paused}
											<span class="status"><span class={badgeClass('warning')}>paused</span></span>
										{:else if s.status.trim().toLowerCase() !== 'listening'}
											<span class="status">{s.status}</span>
										{/if}
									</div>
									<div class="session-title" title={activeSessionLabel(s)}>{activeSessionLabel(s)}</div>
									{#if (s.title || s.url) && s.thread_id && s.thread_id.trim() !== activeSessionLabel(s).trim()}
										<div class="session-thread">{s.thread_id}</div>
									{/if}
									<div class="sources">
										<span class="source-chip">System audio</span>
										{#if s.mode === 'passive' && s.mic}<span class="source-chip">Microphone</span>{/if}
										{#if s.mode === 'attendee'}<span class="source-chip">BlackHole mic (speech)</span>{/if}
									</div>
									<div class="session-actions">
										<button
											class="action-button action-button--outline"
											disabled={!s.thread_id}
											on:click={() => openThread(s.thread_id)}
										>
											<Icon name="message" size={14} />
											<span>Open transcript</span>
										</button>
										<button
											class="action-button action-button--outline"
											disabled={pausingIds.has(s.session_id)}
											on:click={() => void togglePause(s)}
										>
											<span>
												{pausingIds.has(s.session_id)
													? '…'
													: s.paused
														? s.mode === 'attendee'
															? `Unmute ${agentName}`
															: 'Resume'
														: s.mode === 'attendee'
															? `Mute ${agentName}`
															: 'Pause'}
											</span>
										</button>
										<button
											class="action-button action-button--outline danger-action"
											disabled={stoppingIds.has(s.session_id)}
											on:click={() => void stopSession(s.session_id)}
										>
											<Icon name="square" size={14} />
											<span>
												{stoppingIds.has(s.session_id)
													? 'Stopping…'
													: s.mode === 'attendee'
														? 'Leave meeting'
														: 'Stop listening'}
											</span>
										</button>
									</div>
								</div>
							{/each}

							{#if observing && observation}
								<div class="session-card">
									<div class="session-head">
										<span class={badgeClass('info')}>
											{observation.mode === 'watch' ? 'Watching for' : 'Watching notes'}
										</span>
										<span class="status">{elapsedLabel(observation.started_at_ms)}</span>
									</div>
									<div class="session-title" title={observation.purpose}>{observation.purpose}</div>
									{#if observation.watch_for}
										<div class="session-thread">alert when: {observation.watch_for}</div>
									{/if}
									<div class="sources">
										<span class="source-chip">{observation.note_count ?? 0} notes</span>
										<span class="source-chip">{observation.alert_count ?? 0} alerts</span>
										{#if observation.deep_observation}
											<span class="source-chip">{observation.deep_note_count ?? 0} deep reads</span>
										{/if}
										{#if observation.audio_source && observation.audio_source !== 'none'}
											<span class="source-chip">🔊 {observation.audio_source} · {observation.stt_provider ?? observation.audio_profile ?? 'resolving'}</span>
											<span class="source-chip">{observation.transcript_count ?? 0} heard</span>
										{/if}
									</div>
									<div class="retarget-row">
										<label class="field field--compact">
											<span class="field-label">Deep read</span>
											<select
												bind:value={activeDeepObservation}
												disabled={observeBusy}
												on:change={setActiveDeepObservation}
											>
												<option value="off">Off</option>
												<option value="on">Stable screen · 20s / 5m</option>
											</select>
										</label>
										<input
											placeholder="Alert me when… (retargets this session)"
											bind:value={retargetCondition}
										/>
										<button
											class="action-button action-button--outline action-button--sm"
											disabled={observeBusy || !retargetCondition.trim()}
											on:click={() => void retarget({ watch_for: retargetCondition.trim() })}
										>
											Set alert
										</button>
										{#if observation.mode === 'watch'}
											<button
												class="action-button action-button--outline action-button--sm"
												disabled={observeBusy}
												on:click={() => void retarget({ mode: 'notes' })}
											>
												To notes
											</button>
										{/if}
									</div>
									<div class="session-actions">
										<button
											class="action-button action-button--outline"
											on:click={() => openThread('screen-watch')}
										>
											<Icon name="message" size={14} />
											<span>Open narration</span>
										</button>
										<button
											class="action-button action-button--outline danger-action"
											disabled={observeBusy}
											on:click={() => void stopObservation()}
										>
											<Icon name="square" size={14} />
											<span>{observeBusy ? '…' : 'Stop observing'}</span>
										</button>
									</div>
								</div>
							{/if}
						</div>
					{/if}
					{#if !observing && observation?.status === 'stopped' && observation.latest_summary}
						<div class="last-summary">
							<div class="last-summary-head">
								<span>Last observation — summary</span>
								<button
									class="action-button action-button--outline action-button--sm"
									on:click={() => openThread('screen-watch')}
								>
									<Icon name="message" size={14} />
									<span>Open thread</span>
								</button>
							</div>
							<pre>{observation.latest_summary}</pre>
						</div>
					{/if}
				</section>
			</div>
		{/if}
	{/snippet}
</div>
<style>
	.observe-page {
		--observe-red: var(--accent-primary);
		--observe-green: var(--color-success);
		--observe-blue: var(--color-info);
		--observe-violet: var(--accent-secondary);
		width: 100%;
		max-width: var(--app-content-max, 1320px);
		margin: 0 auto;
		padding: 1rem 1rem 2rem;
		box-sizing: border-box;
		overflow-x: hidden;
		min-width: 0;
		display: flex;
		flex-direction: column;
		gap: 0.85rem;
		color: var(--text-primary);
		font-family: var(--font-primary);
	}

	.surface-card {
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		box-shadow: var(--shadow-sm);
		box-sizing: border-box;
	}

	.observe-command {
		display: flex;
		align-items: flex-end;
		justify-content: space-between;
		gap: 1rem;
		min-width: 0;
	}

	.observe-command__copy {
		min-width: 0;
	}

	.observe-kicker {
		margin: 0;
		color: var(--accent-primary);
		font-size: 0.72rem;
		font-weight: 700;
		letter-spacing: 0.06em;
		text-transform: uppercase;
	}

	.observe-command__title-row {
		display: flex;
		flex-wrap: wrap;
		align-items: baseline;
		gap: 0.35rem 0.75rem;
	}

	.observe-command h1 {
		margin: 0;
		font-family: var(--font-display, var(--font-primary));
		font-size: clamp(1.55rem, 2vw, 2rem);
		line-height: 1.05;
	}

	.observe-status {
		margin: 0;
		color: var(--text-secondary);
		font-size: 0.92rem;
	}

	.observe-command__actions {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		justify-content: flex-end;
		gap: 0.45rem;
	}

	.observe-kpis {
		display: grid;
		grid-template-columns: repeat(4, minmax(0, 1fr));
		gap: 0.65rem;
	}

	@media (max-width: 860px) {
		.observe-kpis {
			grid-template-columns: repeat(2, minmax(0, 1fr));
		}
	}

	@media (max-width: 480px) {
		.observe-kpis {
			grid-template-columns: 1fr;
		}
	}

	.observe-kpi {
		position: relative;
		display: flex;
		min-width: 0;
		flex-direction: column;
		align-items: stretch;
		justify-content: space-between;
		gap: 0.45rem;
		padding: 0.8rem 0.95rem;
		border: 1px solid var(--border-soft);
		border-radius: 9px;
		background: var(--bg-card);
		box-shadow: var(--shadow-sm);
		color: inherit;
		font: inherit;
		text-align: left;
		cursor: pointer;
		overflow: hidden;
		transition: transform 0.16s cubic-bezier(0.16, 1, 0.3, 1),
			box-shadow 0.16s ease,
			border-color 0.16s ease,
			background-color 0.16s ease;
	}

	.observe-kpi::before {
		content: '';
		position: absolute;
		top: 0;
		left: 0;
		right: 0;
		height: 3px;
		background: transparent;
		transition: background 0.16s ease;
	}

	.observe-kpi:hover {
		transform: translateY(-2px);
		border-color: color-mix(in srgb, var(--accent-primary) 50%, var(--border-soft));
		box-shadow: 0 4px 14px -2px rgba(0, 0, 0, 0.08), 0 2px 6px -1px rgba(0, 0, 0, 0.04);
	}

	.observe-kpi:hover .observe-kpi__action {
		color: var(--accent-primary);
		transform: translateX(2px);
	}

	.observe-kpi--active {
		border-color: var(--accent-primary);
		background: color-mix(in srgb, var(--accent-primary) 4%, var(--bg-card));
		box-shadow: 0 0 0 1px var(--accent-primary), var(--shadow-sm);
	}

	.observe-kpi--active::before {
		background: var(--accent-primary);
	}

	.observe-kpi__top {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		width: 100%;
		min-width: 0;
	}

	.observe-kpi__icon-badge {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 24px;
		height: 24px;
		border-radius: 6px;
		flex-shrink: 0;
		background: var(--bg-soft);
		color: var(--text-secondary);
	}

	.observe-kpi--now .observe-kpi__icon-badge {
		background: color-mix(in srgb, #10b981 12%, transparent);
		color: #10b981;
	}

	.observe-kpi--sources .observe-kpi__icon-badge {
		background: color-mix(in srgb, #0ea5e9 12%, transparent);
		color: #0ea5e9;
	}

	.observe-kpi--audio .observe-kpi__icon-badge {
		background: color-mix(in srgb, #8b5cf6 14%, transparent);
		color: #8b5cf6;
	}

	.observe-kpi--notes .observe-kpi__icon-badge {
		background: color-mix(in srgb, #f59e0b 14%, transparent);
		color: #f59e0b;
	}

	.observe-kpi__icon-badge--live {
		background: color-mix(in srgb, #ef4444 14%, transparent) !important;
		color: #ef4444 !important;
		animation: pulse-badge 2s infinite ease-in-out;
	}

	@keyframes pulse-badge {
		0%, 100% { opacity: 1; transform: scale(1); }
		50% { opacity: 0.85; transform: scale(1.04); }
	}

	.observe-kpi__label {
		color: var(--text-secondary);
		font-size: var(--text-2xs);
		font-weight: 700;
		letter-spacing: 0.05em;
		text-transform: uppercase;
		flex-grow: 1;
		min-width: 0;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.observe-kpi__live-pill {
		display: inline-flex;
		align-items: center;
		padding: 0.1rem 0.35rem;
		border-radius: 999px;
		background: color-mix(in srgb, #ef4444 15%, transparent);
		color: #ef4444;
		font-size: 0.65rem;
		font-weight: 800;
		letter-spacing: 0.04em;
	}

	.observe-kpi__body {
		display: flex;
		flex-direction: column;
		gap: 0.15rem;
		min-width: 0;
	}

	.observe-kpi__metric {
		font-family: var(--font-mono);
		font-size: 1.35rem;
		font-weight: 800;
		line-height: 1;
		color: var(--text-primary);
	}

	.observe-kpi__sub {
		color: var(--text-muted);
		font-size: 0.76rem;
		font-weight: 550;
		line-height: 1.25;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.observe-kpi__action {
		display: flex;
		align-items: center;
		gap: 0.2rem;
		font-size: 0.72rem;
		font-weight: 650;
		color: var(--text-muted);
		margin-top: 0.15rem;
		transition: color 0.15s ease, transform 0.15s ease;
	}

	.observe-kpi--active .observe-kpi__action {
		color: var(--accent-primary);
		font-weight: 700;
	}

	.observe-tabs {
		position: absolute;
		width: 1px;
		height: 1px;
		padding: 0;
		margin: -1px;
		overflow: hidden;
		clip: rect(0, 0, 0, 0);
		white-space: nowrap;
		border: 0;
		pointer-events: none;
	}

	.observe-tab {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		gap: 0.5rem;
		min-height: 34px;
		padding: 0.45rem 1.05rem;
		border: 1px solid transparent;
		border-radius: var(--radius-sm, 7px);
		background: transparent;
		color: var(--text-secondary);
		font-family: inherit;
		font-size: 0.88rem;
		font-weight: 650;
		line-height: 1.25;
		white-space: nowrap;
		cursor: pointer;
		user-select: none;
		box-sizing: border-box;
		transition: color 0.15s ease, background 0.15s ease, border-color 0.15s ease, box-shadow 0.15s ease;
	}

	.observe-tab:hover {
		color: var(--text-primary);
		background: color-mix(in srgb, var(--bg-card) 75%, transparent);
	}

	.observe-tab--active {
		color: var(--text-primary);
		background: var(--bg-card);
		border-color: var(--border-soft);
		font-weight: 750;
		box-shadow: var(--shadow-sm, 0 1px 3px rgba(0, 0, 0, 0.08));
	}

	.observe-tab--active :global(svg) {
		color: var(--accent-primary);
	}

	.tab-count {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		min-width: 1.35rem;
		padding: 0.1rem 0.45rem;
		border-radius: 999px;
		background: color-mix(in srgb, var(--text-secondary) 15%, transparent);
		color: var(--text-secondary);
		font-family: var(--font-mono, monospace);
		font-size: 0.72rem;
		font-weight: 750;
		line-height: 1;
	}

	.observe-tab--active .tab-count {
		background: color-mix(in srgb, var(--accent-primary) 18%, transparent);
		color: var(--accent-primary);
	}

	.observe-pane {
		display: flex;
		min-width: 0;
		flex-direction: column;
		gap: 0.85rem;
	}

	.observe-pane[hidden] {
		display: none;
	}

	.start-panel {
		display: flex;
		flex-direction: column;
		gap: 0.65rem;
	}

	.start-card-container {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
		padding: 0.9rem 1rem;
	}

	.start-card-header {
		display: flex;
		flex-direction: column;
		gap: 0.2rem;
	}

	.start-card-header h2 {
		margin: 0;
		font-size: 0.95rem;
		font-weight: 700;
	}

	.start-row {
		display: grid;
		grid-template-columns: repeat(3, minmax(0, 1fr));
		gap: 0.65rem;
	}

	@media (max-width: 680px) {
		.start-row {
			grid-template-columns: 1fr;
		}
	}

	.start-verb {
		display: flex;
		align-items: center;
		gap: 0.65rem;
		padding: 0.65rem 0.85rem;
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		background: var(--bg-soft);
		color: var(--text-primary);
		font: inherit;
		cursor: pointer;
		text-align: left;
		transition: all 0.15s ease;
	}

	.start-verb:hover {
		border-color: color-mix(in srgb, var(--accent-primary) 40%, var(--border-soft));
		background: color-mix(in srgb, var(--accent-primary) 5%, var(--bg-card));
		transform: translateY(-1px);
	}

	.start-verb--active {
		border-color: var(--accent-primary);
		background: color-mix(in srgb, var(--accent-primary) 10%, var(--bg-card));
		box-shadow: 0 0 0 1px var(--accent-primary);
	}

	.start-verb__icon {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 30px;
		height: 30px;
		border-radius: 6px;
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		color: var(--accent-primary);
		flex-shrink: 0;
	}

	.start-verb--active .start-verb__icon {
		background: var(--accent-primary);
		color: #ffffff;
		border-color: var(--accent-primary);
	}

	.start-verb__text-group {
		display: flex;
		flex-direction: column;
		gap: 0.1rem;
		min-width: 0;
	}

	.start-verb__title {
		font-size: 0.84rem;
		font-weight: 700;
		color: var(--text-primary);
		line-height: 1.2;
	}

	.start-verb__meta {
		font-size: 0.72rem;
		color: var(--text-muted);
		font-weight: 500;
		line-height: 1.2;
	}

	.start-form {
		padding: 0.9rem 1rem;
	}

	.audio-pane-card {
		display: flex;
		flex-direction: column;
		gap: 1rem;
		padding: 1rem 1.15rem;
	}

	.audio-pane-header {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
	}

	.audio-pane-header h2 {
		margin: 0;
		font-size: 1rem;
		line-height: 1.2;
	}

	.account-grid {
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		gap: 0.85rem;
	}

	.account-card {
		padding: 0.9rem 1rem;
	}

	.tabs-status {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
		padding: 0.9rem 1rem;
	}

	.tabs-status__row,
	.tabs-status__actions {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.55rem;
	}

	.tabs-status__row {
		justify-content: space-between;
	}

	.tabs-status__copy {
		min-width: 0;
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
	}

	.tabs-status h2,
	.start-form h2,
	.account-card h2 {
		margin: 0;
		font-size: 1rem;
		line-height: 1.2;
	}

	.tabs-status__details {
		display: flex;
		flex-direction: column;
		gap: 0.7rem;
	}

	#observe-upcoming,
	#observe-recent,
	#observe-active {
		scroll-margin-top: 0.75rem;
	}

	.audio-surface-bar {
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		display: grid;
		gap: 1rem;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		padding: 0.8rem;
	}

	.observe-section-card {
		position: relative;
		padding: 1rem;
	}

	.observe-active-section--promoted {
		padding: 0.72rem;
	}

	.observe-active-section--promoted .section-head {
		margin-bottom: 0.45rem;
	}

	.observe-active-section--promoted .active-grid {
		grid-template-columns: repeat(auto-fit, minmax(280px, 1fr));
		gap: 0.65rem;
	}

	.observe-active-section--promoted .session-card {
		display: grid;
		grid-template-columns: minmax(0, 1fr) auto;
		grid-template-areas:
			'head actions'
			'title actions'
			'thread actions'
			'sources actions'
			'retarget retarget';
		align-items: center;
		column-gap: 0.75rem;
		row-gap: 0.3rem;
		padding: 0.68rem 0.75rem;
	}

	.observe-active-section--promoted .session-head {
		grid-area: head;
		justify-content: flex-start;
		flex-wrap: nowrap;
		min-width: 0;
	}

	.observe-active-section--promoted .session-head .status {
		flex: 0 0 auto;
		min-width: max-content;
		max-width: 100%;
		overflow-wrap: normal;
		white-space: nowrap;
	}

	.observe-active-section--promoted .session-head > .status-badge {
		padding-left: 0;
	}

	.observe-active-section--promoted .session-title {
		grid-area: title;
	}

	.observe-active-section--promoted .session-thread {
		grid-area: thread;
	}

	.observe-active-section--promoted .sources {
		grid-area: sources;
		min-width: 0;
	}

	.observe-active-section--promoted .session-actions {
		grid-area: actions;
		align-self: center;
		justify-content: flex-end;
		gap: 0.35rem;
		margin-top: 0;
	}

	.observe-active-section--promoted .session-actions .action-button {
		min-height: 1.85rem;
		padding: 0.3rem 0.62rem;
		font-size: var(--text-xs);
	}

	.observe-active-section--promoted .retarget-row {
		grid-area: retarget;
		gap: 0.3rem;
		margin-top: 0.15rem;
	}

	.observe-active-section--promoted .retarget-row input {
		padding: 0.3rem 0.5rem;
	}


	.danger-action {
		color: var(--color-error);
		border-color: color-mix(in srgb, var(--color-error) 42%, var(--border-soft));
		background: color-mix(in srgb, var(--color-error) 8%, transparent);
	}

	.danger-action:hover:not(:disabled) {
		border-color: var(--color-error);
		color: var(--color-error);
	}

	.start-card {
		height: 100%;
		display: flex;
		flex-direction: column;
		gap: 0.7rem;
	}

	.start-card h2 {
		margin: 0;
		font-size: 1rem;
		line-height: 1.2;
	}

	/* Bottom-align the start buttons so the cards read symmetrically even
	   when one card's body (mic toggle / warning) is taller. */
	.start-card > .action-button {
		margin-top: auto;
		align-self: flex-start;
	}

	.ambient-metrics {
		display: grid;
		grid-template-columns: repeat(4, minmax(0, 1fr));
		gap: 0.45rem;
	}

	.ambient-metric {
		min-width: 0;
		padding: 0.48rem 0.55rem;
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		background: var(--bg-soft);
	}

	.ambient-metric-value,
	.ambient-metric-label {
		display: block;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.ambient-metric-value {
		color: var(--text-primary);
		font-size: var(--text-sm);
		font-weight: 750;
		line-height: 1.2;
	}

	.ambient-metric-label {
		margin-top: 0.16rem;
		color: var(--text-muted);
		font-size: var(--text-2xs);
		line-height: 1.2;
	}

	.status-badge {
		display: inline-flex;
		align-items: center;
		max-width: 100%;
		padding: 0.12rem 0.48rem;
		border-radius: 999px;
		background: var(--bg-soft);
		color: var(--text-secondary);
		font-size: var(--text-2xs);
		font-weight: 700;
		line-height: 1.35;
		white-space: nowrap;
	}

	.status-badge--success {
		background: var(--color-success-soft);
		color: var(--color-success);
	}

	.status-badge--warning {
		background: var(--color-warning-soft);
		color: color-mix(in srgb, var(--color-warning) 62%, var(--text-primary));
	}

	.status-badge--error {
		background: var(--color-error-soft);
		color: var(--color-error);
	}

	.status-badge--info {
		background: var(--color-info-soft);
		color: var(--color-info);
	}

	:global(.action-button) {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		gap: 0.38rem;
		max-width: 100%;
		min-height: 2.1rem;
		padding: 0.42rem 0.82rem;
		border: 1px solid transparent;
		border-radius: 8px;
		font: inherit;
		font-size: var(--text-sm);
		font-weight: 700;
		line-height: 1.2;
		cursor: pointer;
		text-decoration: none;
		transition:
			background 0.15s ease,
			border-color 0.15s ease,
			color 0.15s ease,
			opacity 0.15s ease,
			transform 0.15s ease;
	}

	:global(.action-button):hover:not(:disabled) {
		transform: translateY(-1px);
	}

	:global(.action-button):disabled {
		cursor: default;
		opacity: 0.56;
	}

	:global(.action-button--primary) {
		background: var(--button-primary-bg, var(--accent-primary));
		color: var(--button-primary-color, var(--text-on-accent));
		box-shadow: var(--button-primary-shadow, none);
	}

	:global(.action-button--primary:hover:not(:disabled)) {
		box-shadow: var(--button-primary-shadow-hover, var(--button-primary-shadow, none));
	}

	:global(.action-button--secondary) {
		background: var(--button-secondary-bg, var(--bg-soft));
		border-color: var(--button-secondary-border, var(--border-soft));
		color: var(--button-secondary-color, var(--text-primary));
	}

	:global(.action-button--secondary:hover:not(:disabled)) {
		background: var(--button-secondary-hover-bg, var(--bg-surface));
	}

	:global(.action-button--outline) {
		background: transparent;
		border-color: var(--button-outline-border, var(--border-soft));
		color: var(--text-secondary);
	}

	:global(.action-button--outline:hover:not(:disabled)) {
		background: var(--button-outline-hover-bg, color-mix(in srgb, var(--accent-primary) 8%, transparent));
		color: var(--button-outline-hover-color, var(--text-primary));
	}

	:global(.action-button--sm) {
		min-height: 1.78rem;
		padding: 0.28rem 0.58rem;
		font-size: var(--text-2xs);
	}

	:global(.action-button--icon) {
		width: 2rem;
		min-width: 2rem;
		padding: 0;
	}

	.native-input,
	.native-textarea {
		width: 100%;
		box-sizing: border-box;
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		background: var(--bg-base);
		color: var(--text-primary);
		font: inherit;
		font-size: var(--text-sm);
		line-height: 1.35;
	}

	.native-input {
		min-height: 2rem;
		padding: 0.42rem 0.62rem;
	}

	.native-textarea {
		min-height: 4.5rem;
		padding: 0.55rem 0.62rem;
		resize: vertical;
	}

	.native-input::placeholder,
	.native-textarea::placeholder {
		color: var(--text-faint);
	}

	.native-input:focus,
	.native-textarea:focus,
	.field select:focus,
	.time-input:focus,
	.retarget-row input:focus {
		border-color: var(--input-focus-border, var(--accent-primary));
		box-shadow: 0 0 0 3px var(--accent-primary-soft);
		outline: none;
	}

	.native-checkbox {
		display: inline-flex;
		align-items: flex-start;
		gap: 0.5rem;
		max-width: 100%;
		color: inherit;
		font: inherit;
		line-height: 1.35;
	}

	.native-checkbox span {
		min-width: 0;
		overflow-wrap: anywhere;
	}

	.native-checkbox-input {
		appearance: none;
		width: 1rem;
		height: 1rem;
		margin: 0.08rem 0 0;
		display: inline-grid;
		place-content: center;
		flex: 0 0 auto;
		border: 1.5px solid var(--border-default, var(--border-soft));
		border-radius: 4px;
		background: var(--bg-base);
		color: var(--text-on-accent);
		cursor: pointer;
		transition:
			background 0.12s ease,
			border-color 0.12s ease,
			box-shadow 0.12s ease;
	}

	.native-checkbox-input::before {
		content: '';
		width: 0.32rem;
		height: 0.56rem;
		border-right: 2px solid currentColor;
		border-bottom: 2px solid currentColor;
		transform: rotate(42deg) scale(0);
		transform-origin: center;
		transition: transform 0.12s ease;
	}

	.native-checkbox-input:checked {
		background: var(--accent-primary);
		border-color: var(--accent-primary);
		box-shadow: 0 0 0 3px var(--accent-primary-soft);
	}

	.native-checkbox-input:checked::before {
		transform: rotate(42deg) scale(1);
	}

	.native-checkbox-input:disabled {
		cursor: not-allowed;
		opacity: 0.5;
	}

	.hint {
		margin: 0;
		font-size: 0.8rem;
		color: var(--text-muted);
		line-height: 1.45;
	}

	.hint--warn {
		color: color-mix(in srgb, var(--color-warning) 62%, var(--text-primary));
	}

	.mic-toggle {
		display: flex;
		align-items: center;
		font-size: 0.82rem;
		color: var(--text-muted);
	}

	.audio-controls {
		display: flex;
		flex-wrap: wrap;
		gap: 0.5rem;
	}

	.field {
		display: flex;
		flex-direction: column;
		gap: 0.2rem;
		flex: 1;
		min-width: 130px;
	}

	.field--compact {
		flex: 0 1 220px;
		min-width: 180px;
	}

	.field-label {
		font-size: var(--text-2xs);
		font-weight: 700;
		letter-spacing: 0.05em;
		text-transform: uppercase;
		color: var(--text-muted);
	}

	/* email/calendar observe cards: account checkbox rows + time input */
	.acct-row {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: 0.25rem 1rem;
		margin: 0.2rem 0;
		font-size: 0.85rem;
		font-weight: 400;
		text-transform: none;
		letter-spacing: 0;
	}
	.acct-purpose {
		margin-left: 1.6rem;
	}

	.time-input {
		padding: 0.4rem 0.55rem;
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		background: var(--bg-base);
		color: var(--text-primary);
		max-width: 9rem;
		font: inherit;
		font-size: var(--text-sm);
	}

	.field select {
		padding: 0.4rem 0.55rem;
		border-radius: 8px;
		border: 1px solid var(--border-soft);
		background: var(--bg-base);
		color: var(--text-primary);
		font-size: var(--text-sm);
		font-family: inherit;
		cursor: pointer;
	}

	.field select:disabled {
		cursor: default;
		opacity: 0.78;
	}

	/* Card header row: title on the left, the read-only STT chip on the right. */
	.card-head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.5rem;
	}

	.card-title-row {
		display: inline-flex;
		align-items: center;
		gap: 0.5rem;
		min-width: 0;
	}

	.card-icon {
		display: inline-flex;
		width: 1.75rem;
		height: 1.75rem;
		align-items: center;
		justify-content: center;
		flex: 0 0 auto;
		border-radius: 8px;
		background: color-mix(in srgb, var(--accent-primary) 11%, transparent);
		color: var(--accent-primary);
	}

	.section-head {
		margin: 0 0 0.6rem;
		font-size: 1.05rem;
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.45rem;
	}

	.section-head span {
		display: inline-flex;
		align-items: center;
		gap: 0.45rem;
	}

	.section-note {
		color: var(--text-muted);
		font-size: 0.78rem;
		font-weight: 400;
		text-align: right;
	}

	.upcoming-list {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 0.4rem;
		/* Schedule data must stay COMPLETE but bounded: the section scrolls
		   after ~5 rows instead of growing the page on a dense day (backend
		   window is +12h × 25 events per calendar account). */
		max-height: 19rem;
		overflow: auto;
		overscroll-behavior: contain;
		padding-right: 0.25rem;
	}

	.upcoming-row {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
		padding: 0.55rem 0.75rem;
		border-radius: 8px;
		border: 1px solid var(--border-soft);
		background: var(--bg-soft);
	}

	.upcoming-row--live {
		border-color: var(--accent-primary);
	}

	.upcoming-info {
		display: flex;
		flex-direction: column;
		gap: 0.12rem;
		min-width: 0;
	}

	.upcoming-title {
		font-size: 0.88rem;
		font-weight: 600;
		display: flex;
		align-items: center;
		gap: 0.4rem;
		min-width: 0;
		overflow-wrap: anywhere;
	}

	.upcoming-meta {
		font-size: var(--text-2xs);
		color: var(--text-muted);
		overflow-wrap: anywhere;
		word-break: break-word;
	}

	.upcoming-actions {
		display: flex;
		align-items: center;
		gap: 0.4rem;
		flex-shrink: 0;
	}

	.upcoming-actions > .status-badge {
		white-space: nowrap;
	}

	.upcoming-warn {
		margin: 0.45rem 0 0;
		font-size: var(--text-2xs);
		color: color-mix(in srgb, var(--color-warning) 62%, var(--text-primary));
	}

	.active-grid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(320px, 1fr));
		gap: 1rem;
	}

	.session-card {
		background: var(--bg-soft);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		padding: 0.9rem;
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
	}

	.session-head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.5rem;
	}

	.status {
		font-size: var(--text-2xs);
		color: var(--text-muted);
		display: inline-flex;
		align-items: center;
		gap: 0.35rem;
	}

	.session-title {
		font-weight: 600;
		font-size: 0.92rem;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.session-thread {
		font-family: var(--font-mono, monospace);
		font-size: var(--text-2xs);
		color: var(--text-muted);
		overflow-wrap: anywhere;
	}

	.sources {
		display: flex;
		flex-wrap: wrap;
		gap: 0.35rem;
	}

	.source-chip {
		font-size: var(--text-2xs);
		padding: 0.12rem 0.45rem;
		border-radius: 999px;
		border: 1px solid var(--border-soft);
		color: var(--text-muted);
	}

	.session-actions {
		display: flex;
		gap: 0.5rem;
		margin-top: 0.2rem;
		flex-wrap: wrap;
	}

	.retarget-row {
		display: flex;
		gap: 0.4rem;
		flex-wrap: wrap;
	}

	.retarget-row .field {
		margin: 0;
	}

	.retarget-row input {
		flex: 1;
		min-width: 170px;
		padding: 0.4rem 0.6rem;
		border-radius: 8px;
		border: 1px solid var(--border-soft);
		background: var(--bg-base);
		color: var(--text-primary);
		font: inherit;
		font-size: var(--text-sm);
	}

	.last-summary {
		margin-top: 0.8rem;
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		background: var(--bg-soft);
		padding: 0.6rem 0.75rem;
	}

	.last-summary-head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.5rem;
		font-size: var(--text-2xs);
		text-transform: uppercase;
		letter-spacing: 0.04em;
		color: var(--text-muted);
		margin-bottom: 0.3rem;
	}

	.last-summary pre {
		margin: 0;
		white-space: pre-wrap;
		font-size: 0.8rem;
		font-family: inherit;
	}

	.loading-inline {
		display: inline-flex;
		align-items: center;
		gap: 0.5rem;
		color: var(--text-muted);
		font-size: var(--text-sm);
	}

	.loading-spinner {
		width: 1rem;
		height: 1rem;
		border: 2px solid var(--border-soft);
		border-top-color: var(--accent-primary);
		border-radius: 999px;
		animation: observe-spin 0.8s linear infinite;
	}

	.empty-state {
		display: flex;
		min-height: 7rem;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: 0.35rem;
		padding: 1rem;
		border: 1px dashed var(--border-soft);
		border-radius: 8px;
		background: var(--bg-soft);
		text-align: center;
		color: var(--text-secondary);
	}

	.empty-state-icon {
		display: inline-flex;
		width: 1.8rem;
		height: 1.8rem;
		align-items: center;
		justify-content: center;
		border-radius: 999px;
		background: var(--accent-primary-soft);
		color: var(--accent-primary);
		font-weight: 800;
	}

	.empty-state h3 {
		margin: 0;
		color: var(--text-primary);
		font-size: var(--text-md);
		line-height: 1.2;
	}

	.empty-state p {
		margin: 0;
		max-width: 42rem;
		color: var(--text-muted);
		font-size: var(--text-sm);
		line-height: 1.45;
	}

	@keyframes observe-spin {
		to {
			transform: rotate(360deg);
		}
	}

	.recent-list {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 0.35rem;
		/* History: ~6 rows visible, the rest reachable by scrolling within
		   the section (merged cap raised to 30 — full history lives in the
		   threads themselves). */
		max-height: 17rem;
		overflow: auto;
		overscroll-behavior: contain;
		padding-right: 0.25rem;
	}

	.recent-row {
		width: 100%;
		display: flex;
		align-items: center;
		gap: 0.55rem;
		text-align: left;
		background: var(--bg-soft);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		padding: 0.5rem 0.75rem;
		cursor: pointer;
		color: var(--text-primary);
		font-size: 0.85rem;
	}

	.recent-row:hover {
		border-color: var(--accent-primary);
	}

	.kind-chip {
		flex-shrink: 0;
		font-size: 0.8rem;
	}

	.recent-title {
		font-weight: 600;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.recent-meta {
		margin-left: auto;
		flex-shrink: 0;
		font-size: var(--text-2xs);
		color: var(--text-muted);
	}

	@media (max-width: 900px) {
		.observe-kpis {
			grid-template-columns: repeat(2, minmax(0, 1fr));
		}

		.account-grid,
		.audio-surface-bar {
			grid-template-columns: 1fr;
		}

		.observe-command {
			align-items: flex-start;
			flex-direction: column;
		}
	}

	@media (max-width: 620px) {
		.observe-page {
			padding: 0.75rem 0.75rem 1.5rem;
			gap: 1rem;
		}

		.observe-kpis {
			grid-template-columns: repeat(2, minmax(0, 1fr));
		}

		.active-grid {
			grid-template-columns: 1fr;
		}

		.upcoming-row,
		.recent-row {
			align-items: flex-start;
		}

		.upcoming-row {
			flex-direction: column;
			width: 100%;
			box-sizing: border-box;
		}

		.upcoming-info {
			width: 100%;
		}

		.upcoming-actions {
			flex-wrap: wrap;
		}

		.upcoming-actions,
		.session-actions {
			width: 100%;
		}

		.recent-row {
			display: grid;
			grid-template-columns: auto minmax(0, 1fr);
		}

		.recent-title {
			white-space: normal;
			overflow-wrap: anywhere;
		}

		.recent-meta {
			grid-column: 2;
			margin-left: 0;
		}
	}
</style>
