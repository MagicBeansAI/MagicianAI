<script lang="ts">
	import { browser } from '$app/environment';
	import { goto } from '$app/navigation';
	import { page } from '$app/stores';
	import { onMount, onDestroy } from 'svelte';

	import Icon from '$lib/shared/icons/Icon.svelte';
	import ServerPager from '$lib/shared/components/ServerPager.svelte';
	import { showSuccess, showError, showInfo } from '$lib/shared/stores/notifications';
	import { todayStore } from '$lib/stores/todayStore';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import { openAttentionCenter, parseAttentionRouteIntent } from '$lib/attention';
	import type { TodayItem, TodayDigestBullet, TodayVisibilityListItem } from '$lib/today/types';
	import {
		fetchChannelFollowUpsPage,
		usefulChannelFollowUp,
		acknowledgeChannelFollowUp,
		dismissChannelFollowUp,
		dismissChannelFollowUpWithReason,
		approveChannelFollowUp,
		snoozeChannelFollowUp,
		type ChannelFollowUp
	} from '$lib/stores/channelNeedsYouStore';
	import type { ChannelFollowUpDismissReason } from '$lib/channel/channelFollowUpLearning';
	import {
		fetchResurfacingTodayPage,
		postResurfacingAction,
		type ResurfacingCard,
		type ResurfacingDismissReason
	} from '$lib/today/resurfacingQueries';
	import { greetingFor } from '$lib/today/greeting';
	import { loadPublishedSurfacePage } from '$lib/magician/presto/surfaces/publishedSurfaces';
	import ScrollCardPreview from '$lib/magician/dashboard/ScrollCardPreview.svelte';
	import type { PublishedSurfaceRecord } from '$lib/types/surfaces';

	import TodayNewspaperCard from '$lib/today/TodayNewspaperCard.svelte';
	import TodayTriageDeck, { type TriageCardData } from '$lib/today/TodayTriageDeck.svelte';
	import TodayNewspaperLedger from '$lib/today/TodayNewspaperLedger.svelte';
	import TodayRealtimeWire from '$lib/today/TodayRealtimeWire.svelte';
	import Button from '$lib/magician/components/generative/Button.svelte';
	import AppSlotRegion from '$lib/apps/AppSlotRegion.svelte';

	// Telemetry & diagnostics (imported and rendered when ?mode=debug)
	import TodayPulseBand from '$lib/today/TodayPulseBand.svelte';
	import AttentionLearningHealthStrip from '$lib/attention/AttentionLearningHealthStrip.svelte';
	import { canonicalAttentionProjectionStore } from '$lib/stores/canonicalAttentionProjectionStore';
	import type { AttentionBanditHealth } from '$lib/attention/attentionBandit';
	import type {
		AttentionActionabilityPage,
		AttentionActionabilityTrainingStatus,
		ChannelFollowUpLearningHealth
	} from '$lib/channel/channelFollowUpLearning';
	import type { AttentionGroupingPage } from '$lib/attention/attentionGrouping';
	import type { AttentionRoutingPage } from '$lib/attention/attentionRouting';
	import type { AttentionSemanticExtractionHealth } from '$lib/attention/attentionSemanticExtraction';

	// View mode: Broadsheet (newspaper multi-column) vs Morning Brief (swipe deck)
	let viewMode: 'broadsheet' | 'deck' = 'deck';

	// Learning & Attention Telemetry for Debug Mode
	let channelHealth: ChannelFollowUpLearningHealth | null = null;
	let channelActionability: AttentionActionabilityPage | null = null;
	let channelActionabilityTraining: AttentionActionabilityTrainingStatus | null = null;
	let channelGrouping: AttentionGroupingPage | null = null;
	let channelRouting: AttentionRoutingPage | null = null;
	let channelBandit: AttentionBanditHealth | null = null;
	let channelSemanticExtraction: AttentionSemanticExtractionHealth | null = null;
	let channelSemanticRankingEnabled = false;
	let worthBandit: AttentionBanditHealth | null = null;

	$: canonicalDiagnostics = $canonicalAttentionProjectionStore.projection?.diagnostics ?? null;
	$: effectiveHealth = canonicalDiagnostics?.health ?? channelHealth;
	$: effectiveActionability = canonicalDiagnostics?.actionability ?? channelActionability;
	$: effectiveActionabilityTraining = channelActionabilityTraining;
	$: effectiveGrouping = canonicalDiagnostics?.grouping ?? channelGrouping;
	$: effectiveRouting = canonicalDiagnostics?.routing ?? channelRouting;
	$: effectiveBandit = canonicalDiagnostics?.bandit ?? channelBandit ?? worthBandit;
	$: effectiveSemanticExtraction = channelSemanticExtraction;
	$: effectiveSemanticRankingEnabled =
		canonicalDiagnostics?.health?.semantic_ranking_enabled === true ||
		channelSemanticRankingEnabled;

	// Paged Dispatches (Channel follow-ups)
	let channelFollowUps: ChannelFollowUp[] = [];
	let followUpPage = 1;
	const FOLLOW_UP_LIMIT = 5;
	let followUpTotal = 0;
	let followUpLoading = false;
	let followUpCursorsByPage: Record<number, string | null> = { 1: null };

	$: followUpPageCount = Math.max(1, Math.ceil(followUpTotal / FOLLOW_UP_LIMIT));
	$: followUpStartItem = followUpTotal === 0 ? 0 : (followUpPage - 1) * FOLLOW_UP_LIMIT + 1;
	$: followUpEndItem = Math.min(followUpTotal, followUpStartItem + channelFollowUps.length - 1);

	// Paged Reading Room (Resurfacing / Worth a look)
	let worthCards: ResurfacingCard[] = [];
	let worthPage = 1;
	const WORTH_LIMIT = 5;
	let worthTotal = 0;
	let worthLoading = false;

	$: worthPageCount = Math.max(1, Math.ceil(worthTotal / WORTH_LIMIT));
	$: worthStartItem = worthTotal === 0 ? 0 : (worthPage - 1) * WORTH_LIMIT + 1;
	$: worthEndItem = Math.min(worthTotal, worthStartItem + worthCards.length - 1);

	// Special Reports / Published Briefings
	let scrollRecords: PublishedSurfaceRecord[] = [];
	let scrollsLoading = false;
	let scrollsError: string | null = null;

	// Completed Deliverables Paging (6 cards per page)
	let deliveredPage = 1;
	const DELIVERED_LIMIT = 6;
	$: deliveredItems = $todayStore.sections.delivered || [];
	$: deliveredTotal = deliveredItems.length;
	$: deliveredPageCount = Math.max(1, Math.ceil(deliveredTotal / DELIVERED_LIMIT));
	$: deliveredStartItem = deliveredTotal === 0 ? 0 : (deliveredPage - 1) * DELIVERED_LIMIT + 1;
	$: deliveredEndItem = Math.min(deliveredTotal, deliveredPage * DELIVERED_LIMIT);
	$: pagedDeliveredItems = deliveredItems.slice(
		(deliveredPage - 1) * DELIVERED_LIMIT,
		deliveredPage * DELIVERED_LIMIT
	);

	// Chronicle & Digest Paging
	let digestPage = 1;
	const DIGEST_LIMIT = 6;
	$: digestTotal = $todayStore.digest.total || 0;
	$: digestPageCount = Math.max(1, Math.ceil(digestTotal / DIGEST_LIMIT));
	$: digestStartItem = digestTotal === 0 ? 0 : (digestPage - 1) * DIGEST_LIMIT + 1;
	$: digestEndItem = Math.min(digestTotal, (digestPage - 1) * DIGEST_LIMIT + $todayStore.digest.bullets.length);
	$: pagedBullets = $todayStore.digest.bullets || [];

	let loading = true;
	let refreshTimer: ReturnType<typeof setInterval> | null = null;
	let mounted = false;

	let manualDebugToggle = false;
	$: isDebug = manualDebugToggle
		|| $page?.url?.searchParams?.get('mode') === 'debug'
		|| $page?.url?.searchParams?.get('debug') === 'true'
		|| $page?.url?.searchParams?.get('debug') === '1';

	// Greeting & formatted date
	const todayDate = new Date();
	const todayFormatted = todayDate.toLocaleDateString('en-US', {
		weekday: 'long',
		month: 'long',
		day: 'numeric',
		year: 'numeric'
	});
	$: greeting = greetingFor(todayDate);

	// Dynamic & Realistic Newspaper Volume & Issue Generation:
	// Vol. represents the publication year in Roman numerals (Inception year: 2023 = Vol. I, 2024 = Vol. II, 2025 = Vol. III, 2026 = Vol. IV).
	// No. is the continuous Day of the Year (1..365/366), matching the real-world standard of daily broadsheets.
	const INCEPTION_YEAR = 2023;

	function toRoman(num: number): string {
		const lookup: Array<[string, number]> = [
			['M', 1000], ['CM', 900], ['D', 500], ['CD', 400],
			['C', 100], ['XC', 90], ['L', 50], ['XL', 40],
			['X', 10], ['IX', 9], ['V', 5], ['IV', 4], ['I', 1]
		];
		let roman = '';
		let n = Math.max(1, Math.floor(num));
		for (const [letter, val] of lookup) {
			while (n >= val) {
				roman += letter;
				n -= val;
			}
		}
		return roman;
	}

	function getDayOfYear(date: Date): number {
		const start = new Date(date.getFullYear(), 0, 1);
		const diff = date.getTime() - start.getTime() + (start.getTimezoneOffset() - date.getTimezoneOffset()) * 60000;
		const oneDay = 86400000;
		return Math.floor(diff / oneDay) + 1;
	}

	$: volumeNumber = toRoman(Math.max(1, todayDate.getFullYear() - INCEPTION_YEAR + 1));
	$: issueNumber = getDayOfYear(todayDate);

	// Unify follow-up and worth cards for the Triage Deck
	$: triageCards = buildTriageCards(channelFollowUps, worthCards);
	$: totalTriageCount = (followUpTotal || 0) + (worthTotal || 0);
	$: morningDeckCountLabel = totalTriageCount > 50 ? '50+' : `${totalTriageCount}`;

	function formatFollowUpTriageCard(f: ChannelFollowUp): TriageCardData {
		return {
			id: `followup:${f.annotation_id}`,
			title: f.subject?.trim() || 'Untitled Message',
			category: `DISPATCH · ${f.provider?.toUpperCase() || 'CORRESPONDENCE'}`,
			summary: f.summary?.trim() || f.reason?.trim() || 'No preview available',
			sender: f.sender || undefined,
			// ⚡ posts `approve` (start a follow-up task), so it is labelled for
			// that — not for `available_actions[0]`, which needs compose/commit.
			primaryLabel: 'Do it',
			originLane: 'dispatch',
			rawFollowUp: f
		};
	}

	function formatWorthTriageCard(w: ResurfacingCard): TriageCardData {
		return {
			id: `worth:${w.candidate_id}`,
			title: w.source_title?.trim() || w.line?.trim() || 'Resurfaced Note',
			category: `READING ROOM · ${w.source_kind?.replace(/_/g, ' ').toUpperCase() || 'NOTE'}`,
			summary: w.summary?.trim() || w.why_now?.trim() || 'Resurfaced for your attention',
			primaryLabel: 'Open',
			originLane: 'reading_room',
			rawWorth: w
		};
	}

	function buildTriageCards(followUps: ChannelFollowUp[], worths: ResurfacingCard[]): TriageCardData[] {
		const result: TriageCardData[] = [];
		let fIdx = 0;
		let wIdx = 0;
		while (fIdx < followUps.length || wIdx < worths.length) {
			// 2 For Yous (Dispatches)
			for (let k = 0; k < 2 && fIdx < followUps.length; k++) {
				result.push(formatFollowUpTriageCard(followUps[fIdx++]));
			}
			// Followed by 1 Worth a Look (Reading Room)
			if (wIdx < worths.length) {
				result.push(formatWorthTriageCard(worths[wIdx++]));
			}
		}
		return result;
	}

	async function ensureFollowUpCursor(targetPage: number): Promise<string | null> {
		if (targetPage <= 1) return null;
		if (Object.prototype.hasOwnProperty.call(followUpCursorsByPage, targetPage)) {
			return followUpCursorsByPage[targetPage];
		}
		let knownPage = 1;
		for (const key of Object.keys(followUpCursorsByPage)) {
			const p = Number(key);
			if (p < targetPage && p > knownPage) {
				knownPage = p;
			}
		}
		let cursor = followUpCursorsByPage[knownPage] ?? null;
		let curr = knownPage;
		while (curr < targetPage) {
			const res = await fetchChannelFollowUpsPage(FOLLOW_UP_LIMIT, cursor, {
				includeCanonicalProjection: false
			});
			if (!res.ok || !res.has_more || !res.next_cursor) break;
			cursor = res.next_cursor;
			curr += 1;
			followUpCursorsByPage[curr] = cursor;
		}
		return followUpCursorsByPage[targetPage] ?? cursor;
	}

	async function loadFollowUps(targetPage = 1): Promise<void> {
		followUpLoading = true;
		try {
			const cursor = await ensureFollowUpCursor(targetPage);
			const res = await fetchChannelFollowUpsPage(FOLLOW_UP_LIMIT, cursor, {
				includeCanonicalProjection: false
			});
			if (res.ok) {
				channelFollowUps = res.items || [];
				followUpTotal = res.total || 0;
				channelHealth = res.health ?? null;
				channelActionability = res.actionability ?? null;
				channelActionabilityTraining = res.actionability_training ?? null;
				channelGrouping = res.grouping ?? null;
				channelRouting = res.routing ?? null;
				channelBandit = res.bandit ?? null;
				channelSemanticExtraction = res.semantic_extraction ?? null;
				channelSemanticRankingEnabled = res.semantic_ranking_enabled ?? false;
				followUpPage = targetPage;
				if (res.has_more && res.next_cursor) {
					followUpCursorsByPage[targetPage + 1] = res.next_cursor;
				}
			}
		} catch (err) {
			console.error('Failed to load dispatches page:', err);
		} finally {
			followUpLoading = false;
		}
	}

	async function loadWorthPage(targetPage = 1): Promise<void> {
		worthLoading = true;
		try {
			const offset = (targetPage - 1) * WORTH_LIMIT;
			const res = await fetchResurfacingTodayPage({ limit: WORTH_LIMIT, offset });
			worthCards = res.cards || [];
			worthTotal = res.total || 0;
			worthBandit = res.bandit ?? null;
			worthPage = targetPage;
		} catch (err) {
			console.error('Failed to load reading room page:', err);
		} finally {
			worthLoading = false;
		}
	}

	async function loadBriefings(): Promise<void> {
		scrollsLoading = true;
		scrollsError = null;
		try {
			const page = await loadPublishedSurfacePage({
				route_target: '/briefing',
				maxItems: 6
			});
			scrollRecords = page.records || [];
		} catch (err) {
			scrollsError = err instanceof Error ? err.message : 'Failed to load briefings';
			scrollRecords = [];
		} finally {
			scrollsLoading = false;
		}
	}

	function goToDigestPage(targetPage = 1): void {
		digestPage = targetPage;
		todayStore.setQuery({
			section: 'changed',
			digest_limit: DIGEST_LIMIT,
			digest_offset: (targetPage - 1) * DIGEST_LIMIT
		});
	}

	async function openTodayDigestBullet(bullet: TodayDigestBullet): Promise<void> {
		const sourceUrl = bullet.source_url?.trim();
		if (sourceUrl && parseAttentionRouteIntent(sourceUrl)) {
			openAttentionCenter();
			return;
		}
		if (sourceUrl?.startsWith('/')) {
			await goto(sourceUrl, { replaceState: false, noScroll: true });
			return;
		}
		if (sourceUrl) {
			if (typeof window !== 'undefined') {
				window.open(sourceUrl, '_blank');
				return;
			}
		}
		await goto('/feed', { replaceState: false, noScroll: true });
	}

	async function loadAllData(): Promise<void> {
		loading = true;
		try {
			const scope = $scopeIdentityStore;
			const scopeKey = `${scope.principal}:${scope.workspace}`;
			await Promise.allSettled([
				loadFollowUps(followUpPage),
				loadWorthPage(worthPage),
				loadBriefings(),
				canonicalAttentionProjectionStore.refresh(scopeKey).catch(() => {})
			]);
		} finally {
			loading = false;
		}
	}

	onMount(() => {
		if (!browser) return;
		mounted = true;
		todayStore.start();
		void loadAllData();
		refreshTimer = setInterval(() => {
			void loadAllData();
		}, 30000);
	});

	onDestroy(() => {
		mounted = false;
		todayStore.stop();
		if (refreshTimer) clearInterval(refreshTimer);
	});

	function formatRelative(value: number | null | undefined): string {
		if (!value) return 'just now';
		const diffMs = Date.now() - value;
		const minutes = Math.round(diffMs / 60_000);
		if (Math.abs(minutes) < 1) return 'just now';
		if (Math.abs(minutes) < 60) return `${Math.abs(minutes)}m ago`;
		const hours = Math.round(minutes / 60);
		if (Math.abs(hours) < 48) return `${Math.abs(hours)}h ago`;
		const days = Math.round(hours / 24);
		return `${Math.abs(days)}d ago`;
	}

	function scrollMeta(record: PublishedSurfaceRecord): string {
		const bits: string[] = [];
		if (record.source_agent_id || record.metadata?.producer?.producer_agent_id) {
			bits.push(`By ${record.source_agent_id ?? record.metadata.producer.producer_agent_id}`);
		}
		if (record.metadata?.ownership?.task_id) {
			bits.push(`Task ${record.metadata.ownership.task_id}`);
		}
		const pubTime = Date.parse(record.manifest.published_at);
		if (!isNaN(pubTime)) {
			bits.push(formatRelative(pubTime));
		}
		return bits.join(' · ');
	}

	async function openPublishedSurface(surfaceId: string): Promise<void> {
		await goto(`/briefing/${encodeURIComponent(surfaceId)}`, { replaceState: false, noScroll: false });
	}

	async function handleDeliveredOpen(item: TodayItem): Promise<void> {
		if (item.source_url) {
			if (typeof window !== 'undefined') {
				window.open(item.source_url, '_blank');
			}
			return;
		}
		if (item.task_id) {
			await goto(`/tasks?selected=${encodeURIComponent(item.task_id)}`, { replaceState: false, noScroll: false });
			return;
		}
		await goto('/briefing', { replaceState: false, noScroll: true });
	}

	async function handleDeliveredAcknowledge(item: TodayItem): Promise<void> {
		try {
			await todayStore.updateVisibility(item.id, 'dismiss');
			showInfo('Delivered item acknowledged');
		} catch (err: any) {
			showError(err?.message || 'Failed to acknowledge item');
		}
	}

	// Follow-up actions
	async function handleFollowUpAction(
		fu: ChannelFollowUp,
		kind: 'primary' | 'useful' | 'acknowledge' | 'dismiss' | 'snooze',
		reason?: string
	): Promise<void> {
		// Optimistically remove from list; the action helpers report failure as
		// `{ ok: false }` rather than throwing, so the result decides rollback.
		channelFollowUps = channelFollowUps.filter(c => c.annotation_id !== fu.annotation_id);

		const result =
			kind === 'useful'
				? await usefulChannelFollowUp(fu.annotation_id)
				: kind === 'acknowledge'
					? await acknowledgeChannelFollowUp(fu.annotation_id)
					: kind === 'dismiss'
						? reason
							? await dismissChannelFollowUpWithReason(fu.annotation_id, reason as ChannelFollowUpDismissReason)
							: await dismissChannelFollowUp(fu.annotation_id)
						: kind === 'snooze'
							? await snoozeChannelFollowUp(fu.annotation_id)
							: await approveChannelFollowUp(fu.annotation_id);

		if (!result.ok) {
			showError(result.error || 'Action failed');
			void loadFollowUps(followUpPage);
			return;
		}
		followUpTotal = Math.max(0, followUpTotal - 1);
		if (kind === 'useful') showSuccess('Marked useful — more like this will surface');
		else if (kind === 'acknowledge') showInfo('Acknowledged');
		else if (kind === 'dismiss') showInfo('Dismissed');
		else if (kind === 'snooze') showInfo('Snoozed — hidden from Today');
		else showSuccess('Task started');
	}

	// Worth-a-look actions
	const RESURFACING_DISMISS_REASONS = new Set<string>([
		'spam',
		'already_handled',
		'duplicate',
		'delegated',
		'not_relevant'
	]);

	async function handleWorthAction(
		worth: ResurfacingCard,
		kind: 'primary' | 'useful' | 'acknowledge' | 'dismiss' | 'snooze',
		reason?: string
	): Promise<void> {
		// Resurfacing has no snooze action; the card never offers one.
		if (kind === 'snooze') return;

		// Optimistically remove from list; `postResurfacingAction` reports
		// failure as `{ ok: false }` rather than throwing.
		worthCards = worthCards.filter(c => c.candidate_id !== worth.candidate_id);

		const action = kind === 'acknowledge' ? 'acknowledge' : kind === 'dismiss' ? 'dismiss' : 'open';
		const dismissReason = RESURFACING_DISMISS_REASONS.has(reason ?? '')
			? (reason as ResurfacingDismissReason)
			: undefined;
		const result = await postResurfacingAction(worth.candidate_id, action, dismissReason);

		if (!result.ok) {
			showError(result.error || 'Action failed');
			void loadWorthPage(worthPage);
			return;
		}
		worthTotal = Math.max(0, worthTotal - 1);
		if (kind === 'useful') showSuccess('Marked useful — noted for future recommendations');
		else if (kind === 'acknowledge') showInfo('Acknowledged');
		else if (kind === 'dismiss') showInfo('Dismissed');
		else await openWorthSource(worth);
	}

	async function openWorthSource(worth: ResurfacingCard): Promise<void> {
		const route = worth.source_route?.trim();
		if (route?.startsWith('/')) {
			await goto(route, { replaceState: false, noScroll: false });
			return;
		}
		const url = worth.open_url?.trim();
		if (url && typeof window !== 'undefined') {
			window.open(url, '_blank', 'noopener');
			return;
		}
		showInfo('Opened — this note has no linked source');
	}

	// Deck swipe triage event handler
	async function onDeckAction(event: CustomEvent<{ card: TriageCardData; kind: 'useful' | 'acknowledge' | 'dismiss' | 'primary' }>): Promise<void> {
		const { card, kind } = event.detail;
		if (card.rawFollowUp) {
			await handleFollowUpAction(card.rawFollowUp, kind);
		} else if (card.rawWorth) {
			await handleWorthAction(card.rawWorth, kind);
		}
	}

	function toggleDebug(): void {
		manualDebugToggle = !isDebug;
		if (typeof window !== 'undefined') {
			try {
				const url = new URL(window.location.href);
				if (isDebug) {
					url.searchParams.delete('mode');
					url.searchParams.delete('debug');
				} else {
					url.searchParams.set('mode', 'debug');
				}
				void goto(`${url.pathname}${url.search}`, { replaceState: true, noScroll: true }).catch(() => {});
			} catch {
				// No-op in headless test runners
			}
		}
	}

	// Hidden & Snoozed items drawer
	let hiddenPanelExpanded = false;
	let restoringHiddenIds = new Set<string>();

	$: hiddenTodayItems = ($todayStore.hiddenItems || []).filter((item) => item.hidden_kind === 'snoozed');

	function hiddenTodayItemPending(item: TodayVisibilityListItem): boolean {
		return restoringHiddenIds.has(item.item_id);
	}

	function titleCase(value: string | null | undefined): string {
		if (!value) return 'Unknown';
		return value.replace(/_/g, ' ').replace(/\b\w/g, (match) => match.toUpperCase());
	}

	function todaySourceKindLabel(sourceKind: string): string {
		if (sourceKind === 'published_surface') return 'Briefing';
		if (sourceKind === 'routine_result') return 'Routine';
		if (sourceKind === 'memory_learning_digest') return 'Memory';
		if (sourceKind === 'memory_learning') return 'Memory';
		if (sourceKind === 'agent_message') return 'Thread';
		if (sourceKind === 'monitor_update') return 'Monitor';
		return titleCase(sourceKind);
	}

	function formatFutureDistance(value: number | null | undefined): string {
		if (!value) return 'until later';
		const diffMs = value - Date.now();
		if (diffMs <= 0) return 'until now';
		const minutes = Math.ceil(diffMs / 60_000);
		if (minutes < 60) return `for ${minutes}m`;
		const hours = Math.ceil(minutes / 60);
		if (hours < 48) return `for ${hours}h`;
		const days = Math.ceil(hours / 24);
		return `for ${days}d`;
	}

	function hiddenTodayTitle(item: TodayVisibilityListItem): string {
		return item.record.snapshot?.title?.trim() || item.item_id;
	}

	function hiddenTodaySummary(item: TodayVisibilityListItem): string {
		const snapshot = item.record.snapshot;
		const action =
			item.hidden_kind === 'snoozed'
				? `Snoozed ${formatFutureDistance(item.record.snoozed_until)}`
				: `Dismissed ${formatRelative(item.record.dismissed_at)}`;
		const source = snapshot
			? `${todaySourceKindLabel(snapshot.source_kind)} · ${titleCase(String(snapshot.section))}`
			: 'Today item';
		return `${action} · ${source}`;
	}

	function hiddenTodayReason(item: TodayVisibilityListItem): string | null {
		const snapshot = item.record.snapshot;
		return snapshot?.summary?.trim() || snapshot?.reason?.trim() || null;
	}

	async function restoreTodayItem(item: TodayVisibilityListItem): Promise<void> {
		restoringHiddenIds.add(item.item_id);
		restoringHiddenIds = restoringHiddenIds;
		try {
			await todayStore.updateVisibility(item.item_id, 'restore');
			void loadFollowUps(followUpPage);
			void loadWorthPage(worthPage);
			showSuccess('Today item restored.');
		} catch (error) {
			showError(
				`Failed to restore Today item: ${error instanceof Error ? error.message : String(error)}`
			);
		} finally {
			restoringHiddenIds.delete(item.item_id);
			restoringHiddenIds = restoringHiddenIds;
		}
	}
</script>

<svelte:head>
	<title>Today's — Morning Edition</title>
	<link rel="preconnect" href="https://fonts.googleapis.com">
	<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin="anonymous">
	<link href="https://fonts.googleapis.com/css2?family=Cinzel:wght@600;700;800;900&family=Playfair+Display:ital,wght@0,700;0,900;1,700;1,900&family=Newsreader:ital,opsz,wght@0,6..72,400..800;1,6..72,400..800&display=swap" rel="stylesheet">
</svelte:head>

<div class="morning-page">
	<!-- Newspaper Masthead -->
	<header class="morning-masthead">
		<div class="morning-masthead__meta">
			<span class="meta-item">VOL. {volumeNumber} · NO. {issueNumber}</span>
			<span class="meta-item">{todayFormatted}</span>
			<a href="/square" class="meta-item meta-link" title="Explore Town Square preview">
				<span>SQUARE (PREVIEW)</span>
				<Icon name="arrow-right" size={10} />
			</a>
			<span class="meta-item">
				WEATHER: {$todayStore.sections.needs_you.length === 0
					? 'ALL QUIET'
					: `${$todayStore.sections.needs_you.length} URGENT`}
			</span>
		</div>

		<div class="morning-masthead__row">
			<div class="morning-masthead__title-wrap">
				<h1 class="morning-masthead__title">Today's</h1>
				<span class="morning-masthead__kicker">{greeting}. Here is your brief.</span>
			</div>

			<!-- Masthead Actions (Refresh, Square, & Debug) -->
			<div class="morning-masthead__quick-actions">
				<a href="/square" class="square-preview-pill" title="Explore Town Square preview">
					<Icon name="square" size={12} />
					<span>Square (Preview)</span>
				</a>

				<button
					type="button"
					class="icon-action-btn"
					disabled={loading}
					aria-label="Refresh morning edition"
					title={loading ? 'Refreshing…' : 'Refresh morning edition'}
					on:click={() => void loadAllData()}
				>
					<svg
						class:spin={loading}
						viewBox="0 0 24 24"
						width="13"
						height="13"
						fill="none"
						stroke="currentColor"
						stroke-width="2"
						stroke-linecap="round"
						stroke-linejoin="round"
						aria-hidden="true"
					>
						<path d="M21.5 2v6h-6M21.34 15.57a10 10 0 1 1-.57-8.38l5.67-5.67"/>
					</svg>
				</button>

				<button
					type="button"
					class="icon-action-btn"
					class:is-active={isDebug}
					aria-label="Debug"
					title={isDebug ? 'Hide developer telemetry (?mode=debug)' : 'Show developer telemetry (?mode=debug)'}
					on:click={toggleDebug}
				>
					<svg
						viewBox="0 0 24 24"
						width="13"
						height="13"
						fill="none"
						stroke="currentColor"
						stroke-width="2"
						stroke-linecap="round"
						stroke-linejoin="round"
						aria-hidden="true"
					>
						<path d="m8 2 1.88 1.88M14.12 3.88 16 2M9 7.13v-1a3.003 3.003 0 1 1 6 0v1M12 20c-3.3 0-6-2.7-6-6v-3a4 4 0 0 1 4-4h4a4 4 0 0 1 4 4v3c0 3.3-2.7 6-6 6M12 20v-9M6.53 9C4.6 8.8 3 7.1 3 5M6 13H2M3 21c0-2.1 1.7-3.9 3.8-4M20.97 5c0 2.1-1.6 3.8-3.5 4M22 13h-4M17.2 17c2.1.1 3.8 1.9 3.8 4"/>
					</svg>
				</button>
			</div>
		</div>

		<div class="morning-masthead__divider"></div>
	</header>

	<!-- Main Content Area -->
	<main class="morning-content">
		<!-- Telemetry & Bandit Diagnostics (Gated by ?mode=debug, on TOP) -->
		{#if isDebug}
			<section class="morning-telemetry-tray" aria-label="Developer Telemetry & Diagnostics">
				<header class="telemetry-tray-header">
					<div class="tray-title">
						<Icon name="zap" size={16} />
						<h3>Press Room Telemetry &amp; Learning Models</h3>
					</div>
					<div class="tray-actions">
						<span class="tray-pill">Gated by ?mode=debug</span>
						<button type="button" class="tray-toggle-btn" on:click={toggleDebug}>
							Hide
						</button>
					</div>
				</header>

				<div class="telemetry-tray-body">
					<div class="telemetry-block">
						<h4>Live System Pulse</h4>
						<TodayPulseBand />
					</div>

					<div class="telemetry-block">
						<h4>Bandit Health &amp; Routing Classifier</h4>
						<AttentionLearningHealthStrip
							health={effectiveHealth}
							actionability={effectiveActionability}
							actionabilityTraining={effectiveActionabilityTraining}
							grouping={effectiveGrouping}
							routing={effectiveRouting}
							bandit={effectiveBandit}
							semanticExtraction={effectiveSemanticExtraction}
							surfaceLabel="Attention Learning"
							semanticRankingEnabled={effectiveSemanticRankingEnabled}
							loading={followUpLoading && !effectiveHealth && !effectiveBandit}
							expanded={true}
						/>
						{#if !effectiveHealth && !effectiveBandit && !effectiveRouting}
							<div class="telemetry-fallback-note">
								<Icon name="info" size={13} />
								<span>Attention bandit and routing classifiers are calibrating for this workspace. Live weights, exploration ratios, and routing distributions will appear as items receive feedback.</span>
							</div>
						{/if}
					</div>

					<div class="telemetry-block">
						<h4>Attention Scope &amp; Projection</h4>
						<div class="telemetry-grid-stats">
							<div class="stat-pair">
								<span class="stat-label">Workspace Scope</span>
								<span class="stat-val font-mono">{$scopeIdentityStore.principal || 'default'}:{$scopeIdentityStore.workspace || 'default'}</span>
							</div>
							<div class="stat-pair">
								<span class="stat-label">Projection Status</span>
								<span class="stat-val font-mono">{$canonicalAttentionProjectionStore.isLoading ? 'loading…' : $canonicalAttentionProjectionStore.projection ? 'ready' : 'idle'}</span>
							</div>
							<div class="stat-pair">
								<span class="stat-label">Follow-Up Lane Total</span>
								<span class="stat-val font-mono">{followUpTotal} items</span>
							</div>
							<div class="stat-pair">
								<span class="stat-label">Worth-A-Look Total</span>
								<span class="stat-val font-mono">{worthTotal} items</span>
							</div>
							<div class="stat-pair">
								<span class="stat-label">Issue Volume</span>
								<span class="stat-val font-mono">Vol. {volumeNumber} · No. {issueNumber}</span>
							</div>
						</div>
					</div>

					<div class="telemetry-block">
						<h4>Deck &amp; Editorial Pipeline</h4>
						<div class="telemetry-grid-stats">
							<div class="stat-pair">
								<span class="stat-label">Urgent Decisions (Needs You)</span>
								<span class="stat-val font-mono">{$todayStore.sections.needs_you.length}</span>
							</div>
							<div class="stat-pair">
								<span class="stat-label">Triage Deck Queue</span>
								<span class="stat-val font-mono">{triageCards.length} cards</span>
							</div>
							<div class="stat-pair">
								<span class="stat-label">Completed Runs</span>
								<span class="stat-val font-mono">{$todayStore.sections.delivered?.length || 0} today</span>
							</div>
							<div class="stat-pair">
								<span class="stat-label">Layout Mode</span>
								<span class="stat-val font-mono">{viewMode === 'deck' ? 'Morning Deck (Triage Swipe)' : 'Broadsheet Columns'}</span>
							</div>
						</div>
					</div>
				</div>
			</section>
		{/if}

		<!-- Realtime Mini Feed (Live Wire) -->
		<TodayRealtimeWire />

		<!-- Above-The-Fold: Front Page Lead / Urgent Decisions -->
		{#if $todayStore.sections.needs_you.length > 0}
			{@const leadItem = $todayStore.sections.needs_you[0]}
			<section class="morning-lead-story" aria-label="Front Page Lead Story">
				<div class="lead-badge">THE LEAD STORY · URGENT DECISION</div>
				<h2 class="lead-headline">{leadItem.title}</h2>
				<p class="lead-summary">{leadItem.reason || leadItem.summary}</p>
				<div class="lead-actions">
					<button
						type="button"
						class="lead-btn lead-btn--primary"
						on:click={() => openAttentionCenter()}
					>
						Take action now <Icon name="arrow-right" size={14} />
					</button>
				</div>
			</section>
		{:else}
			<div class="morning-slate-clear">
				<span class="slate-icon">☕</span>
				<div class="slate-text">
					<strong>The Slate is Clear</strong>
					<span class="slate-sep">·</span>
					<span>No urgent decisions or blocked workflows require your immediate intervention.</span>
				</div>
			</div>
		{/if}

		<!-- § M: The Daily Index (Economics of Operations) -->
		<TodayNewspaperLedger />

		<!-- Reading Room (Broadsheet Columns vs Interleaved Triage Deck) -->
		<section class="morning-reading-room-section" aria-label="Reading Room">
			<header class="section-banner-header section-banner-header--subtle">
				<div class="section-title-wrap">
					<h2>Reading Room</h2>
				</div>
				<div class="morning-view-switcher" role="radiogroup" aria-label="Edition view">
					<button
						type="button"
						class="view-btn"
						class:active={viewMode === 'deck'}
						role="radio"
						aria-checked={viewMode === 'deck'}
						on:click={() => (viewMode = 'deck')}
					>
						<span>🃏 Morning Brief ({morningDeckCountLabel})</span>
					</button>
					<button
						type="button"
						class="view-btn"
						class:active={viewMode === 'broadsheet'}
						role="radio"
						aria-checked={viewMode === 'broadsheet'}
						on:click={() => (viewMode = 'broadsheet')}
					>
						<span>📰 Broadsheet</span>
					</button>
				</div>
			</header>

			{#if viewMode === 'deck'}
				<TodayTriageDeck
					cards={triageCards}
					debug={isDebug}
					on:action={onDeckAction}
					on:switchView={() => (viewMode = 'broadsheet')}
				/>
			{:else}
				<!-- Broadsheet Two-Column Grid: For You & Worth a look -->
				<div class="morning-broadsheet-grid">
					<!-- Left Column: For You -->
					<section class="morning-column" aria-label="For You">
						<header class="column-header">
							<div class="column-title-wrap">
								<h2>For You</h2>
							</div>
							<span class="column-count">{followUpTotal} item{followUpTotal === 1 ? '' : 's'}</span>
						</header>
						<p class="column-sub">Messages and threads needing a decision or reply.</p>

						<div class="column-cards">
							{#if followUpLoading && channelFollowUps.length === 0}
								<div class="loading-state">Printing dispatches…</div>
							{:else if channelFollowUps.length === 0}
								<div class="empty-column-card">
									<p>No dispatches waiting. Inbox is calm.</p>
								</div>
							{:else}
								{#each channelFollowUps as fu (fu.annotation_id)}
									<TodayNewspaperCard
										followUp={fu}
										debug={isDebug}
										on:action={(e) => handleFollowUpAction(fu, e.detail.kind, e.detail.reason)}
									/>
								{/each}
							{/if}
						</div>

						{#if followUpPageCount > 1}
							<div class="column-pager">
								<ServerPager
									currentPage={followUpPage}
									pageCount={followUpPageCount}
									startItem={followUpStartItem}
									endItem={followUpEndItem}
									totalItems={followUpTotal}
									disabled={followUpLoading}
									ariaLabel="For You pagination"
									on:pagechange={(e) => void loadFollowUps(e.detail.page)}
								/>
							</div>
						{/if}
					</section>

					<!-- Right Column: Worth a look -->
					<section class="morning-column" aria-label="Worth a look">
						<header class="column-header">
							<div class="column-title-wrap">
								<h2>Worth a look</h2>
							</div>
							<span class="column-count">{worthTotal} spark{worthTotal === 1 ? '' : 's'}</span>
						</header>
						<p class="column-sub">Resurfaced memory, project notes, and relevant knowledge.</p>

						<div class="column-cards">
							{#if worthLoading && worthCards.length === 0}
								<div class="loading-state">Curating the reading room…</div>
							{:else if worthCards.length === 0}
								<div class="empty-column-card">
									<p>Nothing worth a look right now. Your library is resting.</p>
								</div>
							{:else}
								{#each worthCards as worth (worth.candidate_id)}
									<TodayNewspaperCard
										{worth}
										debug={isDebug}
										on:action={(e) => handleWorthAction(worth, e.detail.kind, e.detail.reason)}
									/>
								{/each}
							{/if}
						</div>

						{#if worthPageCount > 1}
							<div class="column-pager">
								<ServerPager
									currentPage={worthPage}
									pageCount={worthPageCount}
									startItem={worthStartItem}
									endItem={worthEndItem}
									totalItems={worthTotal}
									disabled={worthLoading}
									ariaLabel="Worth a look pagination"
									on:pagechange={(e) => void loadWorthPage(e.detail.page)}
								/>
							</div>
						{/if}
					</section>
				</div>
			{/if}

			{#if hiddenTodayItems.length > 0}
				<div class="hidden-today" aria-label="Hidden Today items">
					<button
						type="button"
						class="hidden-today__toggle"
						aria-expanded={hiddenPanelExpanded}
						on:click={() => (hiddenPanelExpanded = !hiddenPanelExpanded)}
					>
						<span>
							{hiddenTodayItems.length} hidden · {hiddenPanelExpanded ? 'Hide' : 'Show'}
						</span>
						<Icon name={hiddenPanelExpanded ? 'chevron-down' : 'chevron-right'} size={14} />
					</button>
					{#if hiddenPanelExpanded}
						<p class="hidden-today__hint">Dismissed or snoozed cards can be restored here.</p>
						<div class="hidden-today__list">
							{#each hiddenTodayItems as item (item.item_id)}
								<article class="hidden-today__item">
									<div>
										<strong>{hiddenTodayTitle(item)}</strong>
										<span>{hiddenTodaySummary(item)}</span>
										{#if hiddenTodayReason(item)}
											<p>{hiddenTodayReason(item)}</p>
										{/if}
									</div>
									<Button
										label={hiddenTodayItemPending(item) ? 'Restoring...' : 'Restore'}
										variant="outline"
										size="sm"
										disabled={hiddenTodayItemPending(item)}
										on:click={() => void restoreTodayItem(item)}
									/>
								</article>
							{/each}
						</div>
					{/if}
				</div>
			{/if}
		</section>

		<!-- Custom App Panels / Region Widgets -->
		<AppSlotRegion page="/" regions={['primary', 'secondary']} ariaLabel="Morning edition app widgets" />

		<!-- § 3 Special Reports & Published Briefings -->
		<section class="morning-briefings-section" aria-label="Special Reports & Published Briefings">
				<header class="section-banner-header">
					<div class="section-title-wrap">
						<span class="section-marker">§ 3</span>
						<div>
							<h2>Special Reports &amp; Briefings</h2>
							<p class="section-sub">Curated research dossiers, project briefs, and executive syntheses.</p>
						</div>
					</div>
					<button
						type="button"
						class="editorial-link-btn"
						on:click={() => void goto('/briefing')}
					>
						<span>View all briefings</span>
						<Icon name="arrow-right" size={13} />
					</button>
				</header>

				{#if scrollsLoading && scrollRecords.length === 0}
					<div class="loading-state">Assembling published dossiers…</div>
				{:else if scrollsError}
					<div class="empty-column-card">
						<p>{scrollsError}</p>
					</div>
				{:else if scrollRecords.length === 0}
					<div class="empty-column-card">
						<p>No special reports published today. Press room is clear.</p>
					</div>
				{:else}
					<div class="briefings-grid">
						{#each scrollRecords as record (record.manifest.surface_id)}
							<div
								class="briefing-card"
								role="button"
								tabindex="0"
								on:click={() => void openPublishedSurface(record.manifest.surface_id)}
								on:keydown={(e) => {
									if (e.key === 'Enter' || e.key === ' ') {
										e.preventDefault();
										void openPublishedSurface(record.manifest.surface_id);
									}
								}}
							>
								<div class="briefing-card__meta">
									<span class="briefing-card__edition">SPECIAL EDITION</span>
									<span class="briefing-card__time">{scrollMeta(record)}</span>
								</div>
								<h3 class="briefing-card__title">{record.manifest.title}</h3>
								<div class="briefing-card__preview">
									<ScrollCardPreview {record} />
								</div>
								<div class="briefing-card__footer">
									{#if record.manifest.tags && record.manifest.tags.length > 0}
										<div class="briefing-tags">
											{#each record.manifest.tags.slice(0, 3) as tag}
												<span class="briefing-tag">#{tag}</span>
											{/each}
										</div>
									{/if}
									<span class="read-more-link">
										Read report <Icon name="arrow-right" size={12} />
									</span>
								</div>
							</div>
						{/each}
					</div>
				{/if}
			</section>

			<!-- § 4 Completed Deliverables ("What has been completed") -->
			{#if deliveredTotal > 0}
				<section class="morning-delivered-section" aria-label="Completed Deliverables">
					<header class="section-banner-header">
						<div class="section-title-wrap">
							<span class="section-marker">§ 4</span>
							<div>
								<h2>Completed Deliverables</h2>
								<p class="section-sub">Official records and signed-off deliverables ready for review.</p>
							</div>
						</div>
						<span class="column-count">{deliveredTotal} deliverable{deliveredTotal === 1 ? '' : 's'}</span>
					</header>

					<div class="delivered-grid">
						{#each pagedDeliveredItems as item (item.id)}
							<article class="delivered-card">
								<div class="delivered-card__dateline">
									<span class="dateline-tag">FILED · {(item.source_kind || 'REPORT').replace(/_/g, ' ').toUpperCase()}</span>
									<span class="dateline-time">{formatRelative(item.updated_at || item.created_at)}</span>
								</div>

								<h3 class="delivered-card__headline">{item.title}</h3>

								{#if item.summary || item.reason}
									<p class="delivered-card__prose">{item.summary || item.reason}</p>
								{/if}

								<div class="delivered-card__byline-rule"></div>

								<div class="delivered-card__footer">
									<span class="delivered-card__stamp">
										<Icon name="check" size={12} />
										<span>RESOLVED</span>
									</span>
									<div class="delivered-card__actions">
										<button
											type="button"
											class="delivered-action-link primary"
											on:click={() => void handleDeliveredOpen(item)}
										>
											<span>Inspect</span>
											<Icon name="arrow-right" size={12} />
										</button>
										<button
											type="button"
											class="delivered-action-link"
											title="Acknowledge deliverable"
											on:click={() => void handleDeliveredAcknowledge(item)}
										>
											<span>Acknowledge</span>
										</button>
									</div>
								</div>
							</article>
						{/each}
					</div>

					{#if deliveredPageCount > 1}
						<div class="section-pager">
							<ServerPager
								currentPage={deliveredPage}
								pageCount={deliveredPageCount}
								startItem={deliveredStartItem}
								endItem={deliveredEndItem}
								totalItems={deliveredTotal}
								ariaLabel="Completed deliverables pagination"
								on:pagechange={(e) => (deliveredPage = e.detail.page)}
							/>
						</div>
					{/if}
				</section>
			{/if}

			<!-- § 5 The Chronicle & Digest (Durable changes) -->
			{#if digestTotal > 0 || $todayStore.digest.bullets.length > 0}
				<section class="morning-chronicle" aria-label="The Chronicle">
					<header class="section-banner-header">
						<div class="section-title-wrap">
							<span class="section-marker">§ 5</span>
							<div>
								<h2>The Chronicle &amp; Digest</h2>
								<p class="section-sub">Automated ledger of state changes, memory saves, and task updates across your spaces. Click any dispatch to view source.</p>
							</div>
						</div>
						<button
							type="button"
							class="editorial-link-btn"
							on:click={() => void todayStore.refreshDigest()}
						>
							<Icon name="rotate-ccw" size={12} />
							<span>Refresh digest</span>
						</button>
					</header>

					<ul class="chronicle-list">
						{#each pagedBullets as bullet (bullet.id)}
							<li>
								<button
									type="button"
									class="chronicle-bullet-btn"
									title="Click to view underlying source in feed or attention center"
									on:click={() => void openTodayDigestBullet(bullet)}
								>
									<span class="bullet-dot">▪</span>
									<span class="bullet-text">{bullet.text}</span>
									<span class="bullet-source">{bullet.source_kind.replace(/_/g, ' ')}</span>
									<span class="bullet-arrow">
										<Icon name="arrow-right" size={12} />
									</span>
								</button>
							</li>
						{/each}
					</ul>

					{#if digestPageCount > 1}
						<div class="section-pager">
							<ServerPager
								currentPage={digestPage}
								pageCount={digestPageCount}
								startItem={digestStartItem}
								endItem={digestEndItem}
								totalItems={digestTotal}
								ariaLabel="The Chronicle pagination"
								on:pagechange={(e) => void goToDigestPage(e.detail.page)}
							/>
						</div>
					{/if}
				</section>
			{/if}
	</main>

	<!-- Editorial Footer -->
	<footer class="morning-footer">
		<div class="footer-divider"></div>
		<div class="footer-copy">
			<span>TODAY'S · MORNING EDITION</span>
			<span>•</span>
			<a href="/square">Square (Preview)</a>
			<span>•</span>
			{#if isDebug}
				<button type="button" class="footer-toggle" on:click={toggleDebug}>
					Hide Telemetry (?mode=debug)
				</button>
			{:else}
				<button type="button" class="footer-toggle" on:click={toggleDebug}>
					Inspect Telemetry &amp; Bandit (?mode=debug)
				</button>
			{/if}
		</div>
	</footer>
</div>

<style>
	.morning-page {
		width: 100%;
		max-width: 1240px;
		margin: 0 auto;
		padding: 0.35rem 1.5rem 3.5rem;
		display: flex;
		flex-direction: column;
		gap: 0.85rem;
		color: var(--text-primary, #18181b);
	}

	/* Masthead */
	.morning-masthead {
		display: flex;
		flex-direction: column;
		width: 100%;
		gap: 0.45rem;
	}

	.morning-masthead__meta {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 1.25rem;
		font-family: 'Cinzel', 'Playfair Display', Georgia, serif;
		font-variant: all-small-caps;
		font-size: 0.78rem;
		font-weight: 700;
		letter-spacing: 0.18em;
		color: var(--text-muted, #71717a);
		text-transform: uppercase;
		border-bottom: 1px solid color-mix(in srgb, var(--border-soft) 80%, transparent);
		padding-bottom: 0.35rem;
		width: 100%;
	}

	.morning-masthead__row {
		position: relative;
		display: flex;
		align-items: center;
		justify-content: center;
		width: 100%;
		gap: 1.25rem;
	}

	.morning-masthead__title-wrap {
		display: flex;
		flex-direction: column;
		align-items: center;
		text-align: center;
		gap: 0.2rem;
		padding: 0;
	}

	.morning-masthead__title {
		margin: 0;
		font-family: 'Playfair Display', 'Newsreader', Georgia, serif;
		font-size: clamp(2.4rem, 4.5vw, 3.4rem);
		font-weight: 900;
		letter-spacing: -0.03em;
		line-height: 1;
		color: var(--text-primary, #18181b);
		text-align: center;
	}

	.morning-masthead__kicker {
		margin: 0;
		font-family: 'Newsreader', Georgia, serif;
		font-size: 0.95rem;
		color: var(--text-secondary, #52525b);
		font-style: italic;
		letter-spacing: 0.01em;
		text-align: center;
	}

	.morning-view-switcher {
		display: inline-flex;
		align-items: center;
		padding: 0.12rem;
		border-radius: var(--radius-sm, 6px);
		background: color-mix(in srgb, var(--bg-surface) 90%, var(--border-soft));
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.1));
		height: 28px;
		box-sizing: border-box;
	}

	.view-btn {
		font-family: var(--font-primary, sans-serif);
		font-size: 0.74rem;
		font-weight: 600;
		line-height: 1;
		padding: 0.25rem 0.6rem;
		border-radius: 4px;
		border: none;
		background: transparent;
		color: var(--text-secondary, #52525b);
		cursor: pointer;
		transition: background 0.15s ease, color 0.15s ease, box-shadow 0.15s ease;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		gap: 0.25rem;
		height: 100%;
		box-sizing: border-box;
		white-space: nowrap;
	}

	.view-btn.active {
		background: var(--bg-card, #ffffff);
		color: var(--text-primary, #18181b);
		box-shadow: 0 1px 3px rgba(0, 0, 0, 0.08);
	}

	.morning-masthead__quick-actions {
		position: absolute;
		right: 0;
		top: 50%;
		transform: translateY(-50%);
		display: flex;
		align-items: center;
		gap: 0.45rem;
	}

	@media (max-width: 600px) {
		.morning-masthead__row {
			flex-direction: column;
			align-items: center;
		}

		.morning-masthead__quick-actions {
			position: static;
			transform: none;
			margin-top: 0.35rem;
		}
	}

	.icon-action-btn {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 32px;
		height: 32px;
		padding: 0;
		border-radius: var(--radius-sm, 6px);
		border: 1px solid var(--border-soft);
		background: var(--bg-card);
		color: var(--text-secondary);
		cursor: pointer;
		transition: background 0.15s ease, color 0.15s ease, border-color 0.15s ease;
		box-sizing: border-box;
	}

	.icon-action-btn:hover {
		color: var(--text-primary);
		border-color: var(--border-default);
	}

	.icon-action-btn.is-active {
		background: color-mix(in srgb, var(--accent-primary, #b45309) 12%, var(--bg-card));
		border-color: var(--accent-primary, #b45309);
		color: var(--accent-primary, #b45309);
	}

	.spin {
		animation: spin 1s linear infinite;
	}

	@keyframes spin {
		from { transform: rotate(0deg); }
		to { transform: rotate(360deg); }
	}

	.morning-masthead__divider {
		width: 100%;
		height: 3px;
		border-top: 1px solid var(--text-primary, #18181b);
		border-bottom: 1px solid var(--text-primary, #18181b);
		margin-top: 0.25rem;
	}

	/* Main Content */
	.morning-content {
		display: flex;
		flex-direction: column;
		gap: 1rem;
	}

	/* Lead Story */
	.morning-lead-story {
		padding: 1.15rem 1.4rem;
		background: color-mix(in srgb, var(--accent-primary, #b45309) 6%, var(--bg-card, #ffffff));
		border: 1px solid color-mix(in srgb, var(--accent-primary, #b45309) 35%, transparent);
		border-radius: var(--radius-sm, 6px);
	}

	.lead-badge {
		font-family: var(--font-mono, monospace);
		font-size: 0.68rem;
		font-weight: 700;
		letter-spacing: 0.08em;
		color: var(--accent-primary, #b45309);
		margin-bottom: 0.25rem;
	}

	.lead-headline {
		margin: 0 0 0.4rem;
		font-family: var(--font-display, serif);
		font-size: 1.45rem;
		font-weight: 700;
		line-height: 1.25;
		color: var(--text-primary);
	}

	.lead-summary {
		margin: 0 0 0.85rem;
		font-size: 0.92rem;
		line-height: 1.5;
		color: var(--text-secondary);
		max-width: 52rem;
	}

	.lead-actions {
		display: flex;
		gap: 0.65rem;
	}

	.lead-btn {
		display: inline-flex;
		align-items: center;
		gap: 0.45rem;
		font-size: 0.85rem;
		font-weight: 600;
		padding: 0.5rem 1.15rem;
		border-radius: var(--radius-sm, 6px);
		border: none;
		cursor: pointer;
	}

	.lead-btn--primary {
		background: var(--accent-primary, #b45309);
		color: #ffffff;
	}

	.morning-slate-clear {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		padding: 0.55rem 1rem;
		background: color-mix(in srgb, #10b981 7%, var(--bg-card, #ffffff));
		border: 1px solid color-mix(in srgb, #10b981 22%, transparent);
		border-radius: var(--radius-sm, 6px);
		font-size: 0.84rem;
		color: var(--text-secondary);
	}

	.morning-slate-clear .slate-icon {
		font-size: 1.15rem;
		line-height: 1;
	}

	.morning-slate-clear .slate-text {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: 0.4rem;
	}

	.morning-slate-clear strong {
		color: var(--text-primary);
		font-size: 0.88rem;
		font-weight: 700;
	}

	.morning-slate-clear .slate-sep {
		color: var(--text-muted);
	}

	/* Broadsheet Columns */
	.morning-broadsheet-grid {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(320px, 1fr));
		gap: 2rem;
		align-items: start;
	}

	.morning-column {
		display: flex;
		flex-direction: column;
		gap: 0.55rem;
	}

	.column-header {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		border-bottom: 1px solid var(--border-color, rgba(128, 128, 128, 0.25));
		padding-bottom: 0.25rem;
		gap: 0.5rem;
	}

	.column-title-wrap {
		display: flex;
		align-items: baseline;
		gap: 0.5rem;
	}

	.column-header h2 {
		margin: 0;
		font-family: 'Newsreader', Georgia, serif;
		font-size: 1.15rem;
		font-weight: 500;
		color: var(--text-primary, #18181b);
		letter-spacing: 0.01em;
	}

	.column-count {
		font-family: var(--font-mono, monospace);
		font-size: 0.75rem;
		color: var(--text-muted);
	}

	.column-sub {
		margin: 0 0 0.35rem;
		font-size: 0.8rem;
		font-style: italic;
		color: var(--text-muted);
	}

	.column-cards {
		display: flex;
		flex-direction: column;
		gap: 0.9rem;
		max-height: 520px;
		overflow-y: auto;
		padding-right: 0.4rem;
		scrollbar-width: thin;
		scrollbar-color: color-mix(in srgb, var(--text-muted, #71717a) 35%, transparent) transparent;
		-webkit-overflow-scrolling: touch;
	}

	.column-cards::-webkit-scrollbar {
		width: 5px;
	}

	.column-cards::-webkit-scrollbar-track {
		background: transparent;
	}

	.column-cards::-webkit-scrollbar-thumb {
		background: color-mix(in srgb, var(--text-muted, #71717a) 28%, transparent);
		border-radius: 999px;
	}

	.column-cards::-webkit-scrollbar-thumb:hover {
		background: color-mix(in srgb, var(--text-muted, #71717a) 50%, transparent);
	}

	.column-pager,
	.section-pager {
		padding-top: 0.5rem;
		border-top: 1px solid var(--border-soft, rgba(0, 0, 0, 0.08));
	}

	.empty-column-card,
	.loading-state {
		padding: 1.5rem;
		text-align: center;
		background: color-mix(in srgb, var(--bg-surface) 60%, transparent);
		border: 1px dashed var(--border-soft);
		border-radius: var(--radius-md, 8px);
		font-size: 0.88rem;
		font-style: italic;
		color: var(--text-muted);
	}

	/* Section Banner Headers (for § 3, § 4, § 5) */
	.section-banner-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		border-bottom: 2px solid var(--text-primary, #18181b);
		padding-bottom: 0.35rem;
		margin-bottom: 1rem;
		flex-wrap: wrap;
		gap: 0.5rem;
		width: 100%;
		box-sizing: border-box;
	}

	.section-banner-header--subtle {
		border-bottom: 1px solid var(--border-color, rgba(128, 128, 128, 0.2));
		padding-bottom: 0.25rem;
		margin-bottom: 0;
	}

	.section-title-wrap {
		display: flex;
		align-items: baseline;
		gap: 0.6rem;
	}

	.section-title-wrap h2 {
		margin: 0;
		font-family: 'Newsreader', Georgia, serif;
		font-size: 1.35rem;
		font-weight: 700;
	}

	.section-sub {
		margin: 0.15rem 0 0;
		font-size: 0.82rem;
		font-style: italic;
		color: var(--text-muted);
	}

	.section-marker {
		font-family: var(--font-mono, monospace);
		font-size: 0.85rem;
		font-weight: 700;
		color: var(--accent-primary, #b45309);
	}

	.editorial-link-btn {
		display: inline-flex;
		align-items: center;
		gap: 0.35rem;
		font-family: var(--font-mono, monospace);
		font-size: 0.75rem;
		font-weight: 600;
		border: none;
		background: transparent;
		color: var(--accent-primary, #b45309);
		cursor: pointer;
		padding: 0.2rem 0;
	}

	.editorial-link-btn:hover {
		text-decoration: underline;
	}

	/* § 3 Special Reports & Briefings Grid */
	.morning-briefings-section {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
	}

	.briefings-grid {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(320px, 1fr));
		gap: 1.25rem;
	}

	.briefing-card {
		display: flex;
		flex-direction: column;
		padding: 1.25rem;
		background: var(--bg-card, #ffffff);
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.08));
		border-radius: var(--radius-md, 8px);
		box-shadow: 0 1px 3px rgba(0, 0, 0, 0.04);
		cursor: pointer;
		transition: transform 0.15s ease, box-shadow 0.15s ease, border-color 0.15s ease;
	}

	.briefing-card:hover {
		transform: translateY(-2px);
		box-shadow: 0 4px 12px rgba(0, 0, 0, 0.08);
		border-color: var(--border-default, rgba(0, 0, 0, 0.16));
	}

	.briefing-card__meta {
		display: flex;
		align-items: center;
		justify-content: space-between;
		font-family: var(--font-mono, monospace);
		font-size: 0.68rem;
		font-weight: 600;
		color: var(--text-muted);
		margin-bottom: 0.4rem;
	}

	.briefing-card__edition {
		color: var(--accent-primary, #b45309);
		letter-spacing: 0.06em;
	}

	.briefing-card__title {
		margin: 0 0 0.75rem;
		font-family: 'Newsreader', Georgia, serif;
		font-size: 1.25rem;
		font-weight: 700;
		line-height: 1.25;
		color: var(--text-primary);
	}

	.briefing-card__preview {
		background: var(--bg-surface, #fafafa);
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.06));
		border-radius: 4px;
		padding: 0.65rem;
		max-height: 140px;
		overflow: hidden;
		font-size: 0.85rem;
		color: var(--text-secondary);
	}

	.briefing-card__footer {
		display: flex;
		align-items: center;
		justify-content: space-between;
		margin-top: 0.85rem;
		padding-top: 0.5rem;
		border-top: 1px dashed var(--border-soft, rgba(0, 0, 0, 0.08));
	}

	.briefing-tags {
		display: flex;
		flex-wrap: wrap;
		gap: 0.35rem;
		font-family: var(--font-mono, monospace);
		font-size: 0.7rem;
		color: var(--text-muted);
	}

	.read-more-link {
		display: inline-flex;
		align-items: center;
		gap: 0.25rem;
		font-size: 0.78rem;
		font-weight: 600;
		color: var(--accent-primary, #b45309);
	}

	/* § 4 Completed Deliverables Grid (Authentic Newspaper Cards) */
	.morning-delivered-section {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
	}

	.delivered-grid {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(320px, 1fr));
		gap: 1.25rem;
	}

	.delivered-card {
		display: flex;
		flex-direction: column;
		padding: 1.25rem 1.35rem;
		background: var(--bg-card, #ffffff);
		border: 1px solid color-mix(in srgb, var(--border-default, #e4e4e7) 70%, transparent);
		border-radius: 4px;
		box-shadow: 0 1px 3px rgba(0, 0, 0, 0.03);
		transition: border-color 0.15s ease, box-shadow 0.15s ease;
	}

	.delivered-card:hover {
		border-color: var(--border-default, #d4d4d8);
		box-shadow: 0 3px 8px rgba(0, 0, 0, 0.06);
	}

	.delivered-card__dateline {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		font-family: 'Cinzel', 'Playfair Display', Georgia, serif;
		font-variant: all-small-caps;
		font-size: 0.72rem;
		font-weight: 700;
		letter-spacing: 0.12em;
		color: var(--text-muted, #71717a);
		margin-bottom: 0.45rem;
	}

	.dateline-tag {
		color: #059669;
		font-weight: 700;
	}

	.dateline-time {
		font-family: var(--font-mono, monospace);
		font-size: 0.68rem;
		letter-spacing: 0.02em;
	}

	.delivered-card__headline {
		margin: 0 0 0.5rem;
		font-family: 'Newsreader', 'Playfair Display', Georgia, serif;
		font-size: 1.22rem;
		font-weight: 700;
		line-height: 1.25;
		color: var(--text-primary, #18181b);
	}

	.delivered-card__prose {
		margin: 0 0 0.85rem;
		font-family: var(--font-primary, sans-serif);
		font-size: 0.88rem;
		line-height: 1.55;
		color: var(--text-secondary, #52525b);
		display: -webkit-box;
		-webkit-line-clamp: 3;
		-webkit-box-orient: vertical;
		overflow: hidden;
	}

	.delivered-card__byline-rule {
		width: 100%;
		height: 1px;
		border-top: 1px dashed var(--border-soft, rgba(0, 0, 0, 0.1));
		margin-top: auto;
		margin-bottom: 0.65rem;
	}

	.delivered-card__footer {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.5rem;
	}

	.delivered-card__stamp {
		display: inline-flex;
		align-items: center;
		gap: 0.3rem;
		font-family: var(--font-mono, monospace);
		font-size: 0.68rem;
		font-weight: 700;
		color: #059669;
		letter-spacing: 0.08em;
	}

	.delivered-card__actions {
		display: flex;
		align-items: center;
		gap: 0.45rem;
	}

	.delivered-action-link {
		display: inline-flex;
		align-items: center;
		gap: 0.3rem;
		font-family: var(--font-mono, monospace);
		font-size: 0.74rem;
		font-weight: 600;
		padding: 0.28rem 0.65rem;
		border-radius: 4px;
		border: 1px solid var(--border-soft);
		background: transparent;
		color: var(--text-secondary);
		cursor: pointer;
		transition: all 0.15s ease;
	}

	.delivered-action-link:hover {
		color: var(--text-primary);
		border-color: var(--border-default);
	}

	.delivered-action-link.primary {
		background: color-mix(in srgb, #10b981 10%, var(--bg-card));
		border-color: color-mix(in srgb, #10b981 35%, transparent);
		color: #065f46;
	}

	.delivered-action-link.primary:hover {
		background: color-mix(in srgb, #10b981 20%, var(--bg-card));
	}

	/* § 5 Chronicle / Digest Section (Live interactive links) */
	.morning-chronicle {
		padding: 1.25rem 1.5rem;
		background: color-mix(in srgb, var(--bg-surface) 60%, var(--bg-card));
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-md, 8px);
	}

	.chronicle-list {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 0.45rem;
	}

	.chronicle-bullet-btn {
		display: grid;
		grid-template-columns: auto 1fr auto auto;
		align-items: baseline;
		gap: 0.75rem;
		width: 100%;
		text-align: left;
		padding: 0.5rem 0.65rem;
		border: 1px solid transparent;
		background: transparent;
		border-radius: var(--radius-sm, 6px);
		cursor: pointer;
		font: inherit;
		color: inherit;
		transition: background 0.15s ease, border-color 0.15s ease, transform 0.15s ease;
	}

	.chronicle-bullet-btn:hover {
		background: color-mix(in srgb, var(--bg-card) 85%, var(--accent-primary, #b45309));
		border-color: color-mix(in srgb, var(--accent-primary, #b45309) 25%, transparent);
	}

	.chronicle-bullet-btn:hover .bullet-arrow {
		transform: translateX(2px);
		color: var(--accent-primary, #b45309);
	}

	.bullet-dot {
		color: var(--accent-primary, #b45309);
		font-size: 0.75rem;
	}

	.bullet-text {
		color: var(--text-primary);
		font-size: 0.88rem;
		line-height: 1.5;
	}

	.bullet-source {
		font-family: var(--font-mono, monospace);
		font-size: 0.72rem;
		color: var(--text-muted);
		text-transform: uppercase;
		letter-spacing: 0.05em;
	}

	.bullet-arrow {
		display: inline-flex;
		align-items: center;
		color: var(--text-muted);
		transition: transform 0.15s ease, color 0.15s ease;
	}

	/* Telemetry Tray (Press Room Debug Mode) */
	.morning-telemetry-tray {
		padding: 1.25rem 1.5rem;
		background: var(--bg-surface, #fafafa);
		border: 1px solid var(--border-default, #e4e4e7);
		border-radius: var(--radius-md, 8px);
		box-shadow: 0 1px 3px rgba(0, 0, 0, 0.04);
	}

	.telemetry-tray-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		margin-bottom: 1rem;
		padding-bottom: 0.5rem;
		border-bottom: 1px solid var(--border-soft);
	}

	.tray-title {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		color: var(--accent-primary, #b45309);
	}

	.tray-title h3 {
		margin: 0;
		font-family: var(--font-mono, monospace);
		font-size: 0.85rem;
		font-weight: 700;
		letter-spacing: 0.04em;
		text-transform: uppercase;
		color: var(--text-primary);
	}

	.tray-actions {
		display: flex;
		align-items: center;
		gap: 0.65rem;
	}

	.tray-pill {
		font-family: var(--font-mono, monospace);
		font-size: 0.7rem;
		padding: 0.15rem 0.45rem;
		background: color-mix(in srgb, var(--accent-primary) 12%, transparent);
		color: var(--accent-primary, #b45309);
		border-radius: 4px;
	}

	.tray-toggle-btn {
		font-family: var(--font-mono, monospace);
		font-size: 0.72rem;
		border: none;
		background: transparent;
		color: var(--text-muted);
		cursor: pointer;
		text-decoration: underline;
	}

	.telemetry-tray-body {
		display: flex;
		flex-direction: column;
		gap: 1rem;
		width: 100%;
	}

	.telemetry-block {
		width: 100%;
		background: color-mix(in srgb, var(--bg-card) 60%, transparent);
		border: 1px solid color-mix(in srgb, var(--border-soft) 80%, transparent);
		border-radius: var(--radius-sm, 6px);
		padding: 1rem 1.15rem;
		box-sizing: border-box;
	}

	.telemetry-block h4 {
		margin: 0 0 0.65rem;
		font-family: var(--font-mono, monospace);
		font-size: 0.78rem;
		font-weight: 600;
		text-transform: uppercase;
		color: var(--text-muted);
		border-bottom: 1px dashed color-mix(in srgb, var(--border-soft) 80%, transparent);
		padding-bottom: 0.4rem;
	}

	.telemetry-grid-stats {
		display: grid;
		gap: 0.5rem;
		font-size: 0.8rem;
	}

	.stat-pair {
		display: flex;
		justify-content: space-between;
		align-items: center;
		padding: 0.25rem 0;
		border-bottom: 1px dotted color-mix(in srgb, var(--border-soft) 60%, transparent);
	}

	.stat-pair:last-child {
		border-bottom: none;
	}

	.stat-label {
		color: var(--text-muted);
		font-size: 0.75rem;
	}

	.stat-val {
		color: var(--text-primary);
		font-weight: 500;
	}

	.meta-link {
		color: var(--accent-primary, #b45309);
		text-decoration: none;
		display: inline-flex;
		align-items: center;
		gap: 0.3rem;
		transition: color 0.15s ease;
	}

	.meta-link:hover {
		color: var(--text-primary);
		text-decoration: underline;
	}

	.square-preview-pill {
		display: inline-flex;
		align-items: center;
		gap: 0.4rem;
		padding: 0.25rem 0.65rem;
		border: 1px solid color-mix(in srgb, var(--border-soft) 80%, transparent);
		border-radius: 999px;
		background: color-mix(in srgb, var(--bg-card) 80%, transparent);
		color: var(--text-secondary);
		font-family: var(--font-mono, monospace);
		font-size: 0.72rem;
		letter-spacing: 0.02em;
		text-decoration: none;
		transition: all 0.15s ease;
	}

	.square-preview-pill:hover {
		background: var(--bg-card);
		color: var(--text-primary);
		border-color: var(--text-primary);
	}

	.telemetry-fallback-note {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		padding: 0.75rem 1rem;
		background: var(--bg-subtle, #f8fafc);
		border: 1px dashed var(--border-color, #e2e8f0);
		border-radius: 4px;
		font-size: 0.8rem;
		color: var(--text-muted, #64748b);
		font-family: 'Newsreader', Georgia, serif;
	}

	/* Reading Room Section Container */
	.morning-reading-room-section {
		display: flex;
		flex-direction: column;
		width: 100%;
		gap: 0.2rem;
		margin-top: -0.45rem;
		box-sizing: border-box;
	}

	.morning-reading-room-section .section-title-wrap h2 {
		font-size: 1.55rem;
		font-weight: 700;
		letter-spacing: -0.01em;
	}

	/* Footer */
	.morning-footer {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 1rem;
		margin-top: 2rem;
		color: var(--text-muted);
		font-size: 0.78rem;
	}

	.footer-divider {
		width: 100%;
		height: 1px;
		background: var(--border-soft);
	}

	.footer-copy {
		display: flex;
		align-items: center;
		gap: 0.65rem;
		flex-wrap: wrap;
		justify-content: center;
		font-family: var(--font-mono, monospace);
		font-size: 0.72rem;
		letter-spacing: 0.05em;
	}

	.footer-copy a {
		color: inherit;
		text-decoration: underline;
	}

	.footer-toggle {
		font: inherit;
		border: none;
		background: transparent;
		color: inherit;
		text-decoration: underline;
		cursor: pointer;
	}

	/* Hidden & Snoozed Items Restore Drawer */
	.hidden-today {
		display: grid;
		gap: 0.65rem;
		padding: 0.85rem 1rem;
		border: 1px solid color-mix(in srgb, var(--border-soft) 76%, transparent);
		border-radius: var(--radius-sm, 6px);
		background: color-mix(in srgb, var(--bg-card) 76%, transparent);
		margin-top: 0.5rem;
	}

	.hidden-today__toggle {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.6rem;
		width: 100%;
		padding: 0;
		border: 0;
		background: transparent;
		color: var(--text-secondary);
		font-family: inherit;
		font-size: var(--text-sm, 0.875rem);
		text-align: left;
		cursor: pointer;
	}

	.hidden-today__toggle:hover,
	.hidden-today__toggle:focus-visible {
		color: var(--text-primary);
	}

	.hidden-today__hint,
	.hidden-today__item span,
	.hidden-today__item p {
		margin: 0;
		color: var(--text-muted);
		font-size: var(--text-xs, 0.75rem);
		line-height: 1.45;
	}

	.hidden-today__list {
		display: grid;
		gap: 0.55rem;
	}

	.hidden-today__item {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 0.85rem;
		min-width: 0;
		padding-top: 0.6rem;
		border-top: 1px solid color-mix(in srgb, var(--border-soft) 64%, transparent);
	}

	.hidden-today__item > div {
		display: grid;
		gap: 0.25rem;
		min-width: 0;
	}

	.hidden-today__item strong {
		color: var(--text-primary);
		font-weight: 600;
		overflow-wrap: anywhere;
	}

	@media (max-width: 640px) {
		.hidden-today__item {
			flex-direction: column;
			align-items: stretch;
		}
	}
</style>
