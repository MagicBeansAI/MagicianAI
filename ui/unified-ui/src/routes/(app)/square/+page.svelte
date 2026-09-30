<script lang="ts">
	import { browser } from '$app/environment';
	import { goto } from '$app/navigation';
	import { page } from '$app/stores';
	import { get } from 'svelte/store';
	import { onDestroy, onMount, tick } from 'svelte';
	import {
		hitlOpenTargetFromFeedItem,
		openAttentionCenter,
		openAttentionRoute,
		openHitlPrompt,
		parseAttentionRouteIntent
	} from '$lib/attention';
	import { loadAgents, personalAgentList, type AgentSummary } from '$lib/stores/agentStore';
	import { createFeedStore } from '$lib/stores/feedStore';
	import {
		snoozeMinutesFor,
		todayStore,
		type SnoozeOption,
		type TodayStoreQuery
	} from '$lib/stores/todayStore';
	import { showError, showInfo, showSuccess } from '$lib/shared/stores/notifications';
	import { clickOutside } from '$lib/shared/clickOutside';
	import { stripMarkdownPreview } from '$lib/shared/markdownPreview';
	import { createMenuKeydown, menuFocusableItems } from '$lib/shared/menuKeydown';
	import { requestConfirmation } from '$lib/stores/confirmationStore';
	import { taskStore, type Task } from '$lib/stores/taskStore';
	import { internalTaskRoute } from '$lib/magician/tasks/taskRoutes';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import { threadStore } from '$lib/stores/threadStore';
	import type { FeedItem, FeedItemStatus, FeedItemType } from '$lib/feed/types';
	import type {
		TodayDigestBullet,
		TodayItem,
		TodaySectionId,
		TodaySectionPage,
		TodayVisibilityListItem,
		TodayVisibilitySnapshot
	} from '$lib/today/types';
	import { greetingFor } from '$lib/today/greeting';
	import {
		isInteractiveDescendantEvent,
		showTodayPrimaryAction,
		todayRowClass
	} from '$lib/today/rowInteractions';
	import { formatSpaceLabel, todaySpaceGroups } from '$lib/today/spaceGroups';
	import { todayAttentionItemId, todayHitlOpenTarget } from '$lib/today/attentionLauncher';
	import { todaySectionCursorProbeUrl } from '$lib/today/sectionCursorProbe';
	import TodayPulseBand from '$lib/today/TodayPulseBand.svelte';
	import ResurfacingBand from '$lib/today/ResurfacingBand.svelte';
	import CanonicalAttentionLane from '$lib/attention/CanonicalAttentionLane.svelte';
	import {
		countScopedOptimisticMutations,
		followUpAttentionMutationKey,
		optimisticAttentionMutationQueue
	} from '$lib/attention/optimisticAttentionMutationQueue';
	import { canonicalAttentionProjectionStore } from '$lib/stores/canonicalAttentionProjectionStore';
	import { attentionSemanticExtractionStore } from '$lib/stores/attentionSemanticExtractionStore';
	import { canonicalResurfacingChatThread } from '$lib/today/resurfacingChatContext';
	import { fetchResurfacingTodayPage, type ResurfacingCard } from '$lib/today/resurfacingQueries';
	import { collectWorthCardLookup } from '$lib/today/worthCardLookup';
	import ChannelFollowUpActions from '$lib/channel/ChannelFollowUpActions.svelte';
	import ChannelFollowUpGroupPanel from '$lib/channel/ChannelFollowUpGroupPanel.svelte';
	import AttentionLearningHealthStrip from '$lib/attention/AttentionLearningHealthStrip.svelte';
	import AttentionBanditDiagnostic from '$lib/attention/AttentionBanditDiagnostic.svelte';
	import AttentionRoutingDiagnostic from '$lib/attention/AttentionRoutingDiagnostic.svelte';
	import { verifiedAttentionVisibility } from '$lib/attention/attentionVisibility';
	import type {
		AttentionActionabilityPage,
		AttentionActionabilityTrainingStatus,
		ChannelFollowUpLearningHealth
	} from '$lib/channel/channelFollowUpLearning';
	import type { AttentionGroupingPage } from '$lib/attention/attentionGrouping';
	import type { AttentionRoutingPage } from '$lib/attention/attentionRouting';
	import type { AttentionBanditHealth } from '$lib/attention/attentionBandit';
	import type { AttentionSemanticExtractionHealth } from '$lib/attention/attentionSemanticExtraction';
	import {
		fetchChannelFollowUpsPage,
		channelActionSummary,
		channelLabelText,
		channelProviderText,
		channelLaneText,
		channelReceivedLabel,
		type ChannelFollowUp
	} from '$lib/stores/channelNeedsYouStore';
	import type { PublishedSurfaceRecord } from '$lib/types/surfaces';
	import Badge from '$lib/magician/components/generative/Badge.svelte';
	import Button from '$lib/magician/components/generative/Button.svelte';
	import Card from '$lib/magician/components/generative/Card.svelte';
	import Input from '$lib/magician/components/generative/Input.svelte';
	import Modal from '$lib/magician/components/generative/Modal.svelte';
	import LearningCandidateFeedCard from '$lib/magician/components/feed/LearningCandidateFeedCard.svelte';
	import LearningInsightFeedCard from '$lib/magician/components/feed/LearningInsightFeedCard.svelte';
	import ScrollCardPreview from '$lib/magician/dashboard/ScrollCardPreview.svelte';
	import { ensureRegistryLoaded } from '$lib/stores/themeStore';
	import Spinner from '$lib/magician/components/generative/Spinner.svelte';
	import Skeleton from '$lib/shared/components/Skeleton.svelte';
	import ServerPager from '$lib/shared/components/ServerPager.svelte';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import type { IconName } from '$lib/shared/icons/paths';
	import { fadeUp } from '$lib/motion';
	import { flipDurationMs, settleIn, settleOut } from '$lib/shared/motion';
	import { flip } from 'svelte/animate';
	import TextArea from '$lib/magician/components/generative/TextArea.svelte';
	import {
		agentLearningSourceUrl,
		learningValueLabel as sharedLearningValueLabel
	} from '$lib/feed/learningCards';
	import FleetWorld from '$lib/magician/square/FleetWorld.svelte';
	import CommandDock from '$lib/magician/square/hud/CommandDock.svelte';
	import SquareWorldChrome from '$lib/magician/square/hud/SquareWorldChrome.svelte';
	import OfficeFloor from '$lib/magician/square/office/OfficeFloor.svelte';
	import TownSquareSocial from '$lib/townSquare/TownSquareSocial.svelte';
	import { displayNameOf } from '$lib/magician/square/derive';
	import { agentTarget } from '$lib/magician/square/engine/types';
	import { isGameAudioMuted, playGameCue, setGameAudioMuted } from '$lib/magician/square/gameAudio';
	import { floatingChromeHidden } from '$lib/stores/floatingChromeStore';
	import {
		loadPublishedSurfacePage,
		subscribeToPublishedSurfaceRefresh
	} from '$lib/magician/presto/surfaces/publishedSurfaces';

	type ActivityFilter = 'all' | 'learnings' | 'outcomes' | 'failed' | 'deliveries';
	// 'social' renders the Town Square feed (lib/townSquare/TownSquareSocial.svelte)
	// as a compact tab rather than the retired /town-square page.
	type TodayTabId = TodaySectionId | 'worth_a_look' | 'social';
	type TodayTabMeta = {
		id: TodayTabId;
		title: string;
		metricLabel: string;
		summary: string;
		icon: IconName;
		groupBySpace: boolean;
		// Section-header count badge color (subset of the Badge palette).
		tone: 'warning' | 'default' | 'success' | 'primary' | 'info';
		// Rows get the emphasis treatment (primary action button + row class).
		emphasized: boolean;
	};
	type TodaySectionMeta = TodayTabMeta & { id: TodaySectionId };
	type TodaySectionView = {
		meta: TodaySectionMeta;
		items: TodayItem[];
	};
	type TodaySectionTabView = TodaySectionView & {
		count: number;
		sourceCount: number;
	};
	type TodayTabView = {
		meta: TodayTabMeta;
		items: TodayItem[];
		count: number;
		sourceCount: number;
	};
	type TodayLearnedItem = {
		id: string;
		title: string;
		summary: string | null;
		updatedAt: number | null;
	};
	type TodayVisibilityUiAction = 'dismiss' | 'snooze' | 'restore';

	// --- Fleet Civilization hero (the game deck) ---
	// The isometric world fills the visible pane (.v5-main); the copied Today
	// surface below is the scroll-body. See the fleet-civilization-game design doc.
	let heroEl: HTMLElement;
	let heroImmersed = false;
	let heroVisible = true;
	$: heroPaused = !heroVisible && !heroImmersed;
	// The game owns its window: global floating chat chrome fades out while
	// the hero is on screen and back in once it
	// scrolls away. Reset on page leave.
	$: floatingChromeHidden.set(heroVisible || heroImmersed);
	function toggleImmerse() {
		heroImmersed = !heroImmersed;
	}

	// --- hero view mode: the 2D office FLOOR is the default, the 3D CAMPUS is
	// the opt-in. The floor answers "how is my crew doing" on one screen with no
	// camera; the campus is for exploring, and asking someone to fly a camera to
	// read their own roster was the wrong default. Only one of the two is
	// mounted at a time — the campus owns a WebGL context and its own polling,
	// and paying for both while one is hidden would be a waste of a GPU.
	type HeroView = 'floor' | 'campus';
	const HERO_VIEW_KEY = 'square.heroView';
	const HERO_DOCK_KEY = 'square.commandDock';
	let heroView: HeroView = 'floor';
	let dockOpen = true;
	let audioMuted = false;
	function toggleAudio(): void {
		audioMuted = !audioMuted;
		setGameAudioMuted(audioMuted);
		if (!audioMuted) playGameCue('select');
	}
	function setHeroView(next: HeroView) {
		heroView = next;
		try {
			localStorage.setItem(HERO_VIEW_KEY, next);
		} catch {
			// private mode / storage disabled — the choice just does not persist
		}
	}
	function setDockOpen(next: boolean) {
		dockOpen = next;
		try {
			localStorage.setItem(HERO_DOCK_KEY, next ? 'open' : 'hidden');
		} catch {
			// private mode / storage disabled — the choice just does not persist
		}
	}

	$: fleetAgents = $personalAgentList.map(projectDisplayIdentity);
	let dockTarget: string | null = null;
	let dockSection: 'crew' | 'work' | 'spend' | null = null;
	let campusWorld: { focusAgent: (id: string) => void } | undefined;

	function projectDisplayIdentity(agent: AgentSummary): AgentSummary {
		return {
			...agent,
			name: displayNameOf(agent),
			persona: undefined
		};
	}

	onMount(() => {
		try {
			const stored = localStorage.getItem(HERO_VIEW_KEY);
			if (stored === 'campus' || stored === 'floor') heroView = stored;
			const dockStored = localStorage.getItem(HERO_DOCK_KEY);
			if (dockStored === 'hidden') dockOpen = false;
			if (dockStored === 'open') dockOpen = true;
			audioMuted = isGameAudioMuted();
		} catch {
			// storage disabled — fall back to the Floor default
		}
		// Size the hero to the exact VISIBLE pane (viewport - TopBar), NOT 100vh.
		const pane = (heroEl?.closest('.v5-main') ?? heroEl?.parentElement) as HTMLElement | null;
		const applyHeight = () => {
			const px = (pane ?? document.documentElement).clientHeight;
			if (heroEl && px) heroEl.style.setProperty('--hero-h', `${px}px`);
		};
		applyHeight();
		const ro = pane ? new ResizeObserver(applyHeight) : null;
		if (pane && ro) ro.observe(pane);
		// Render-pause the world when the hero scrolls off-view.
		const io = new IntersectionObserver(
			(entries) => {
				heroVisible = entries[0]?.isIntersecting ?? true;
			},
			{ threshold: 0.02 }
		);
		if (heroEl) io.observe(heroEl);
		return () => {
			ro?.disconnect();
			io.disconnect();
			// leaving /square must always restore the global chrome
			floatingChromeHidden.set(false);
		};
	});

	const TODAY_BRIEFING_ROUTE = '/briefing';
	const TODAY_BRIEFING_LIMIT = 8;
	// Single source of truth for section identity — order, icon, labels,
	// summary, space grouping, tone/emphasis, and the relief-valve target.
	// Consumed by the tab strip, the section views, the row icon, and the
	// per-section footers so identity can't drift between them. The accent
	// colors stay derived from the id-based CSS classes (`today-section-tab--*` /
	// `today-row--*`) which share the same underscores→dashes id mapping.
	// The `satisfies Record<TodaySectionId, …>` map keeps the meta exhaustive:
	// adding a section id to the union fails to compile until meta exists.
	const TODAY_SECTION_META_BY_ID = {
		needs_you: {
			title: 'Needs You',
			metricLabel: 'Needs You',
			summary: 'Decisions, approvals, failures, and blocked work.',
			icon: 'alert',
			groupBySpace: false,
			tone: 'warning',
			emphasized: true
		},
		delivered: {
			title: 'Delivered',
			metricLabel: 'Delivered',
			summary: 'Finished work with something useful to open.',
			icon: 'check',
			groupBySpace: true,
			tone: 'success',
			emphasized: false
		},
		changed: {
			title: 'Changed',
			metricLabel: 'Changed',
			summary: 'Durable memory and state changes worth knowing.',
			icon: 'info',
			groupBySpace: true,
			tone: 'primary',
			emphasized: false
		},
		active_work: {
			title: 'Active Work',
			metricLabel: 'Active',
			summary: 'Current running work, shown as status instead of logs.',
			icon: 'zap',
			groupBySpace: true,
			tone: 'info',
			emphasized: false
		},
		followups: {
			title: 'Follow-ups',
			metricLabel: 'Follow-ups',
			summary: 'Due, blocked, or stale work that needs a next action.',
			icon: 'arrow-right',
			groupBySpace: true,
			tone: 'default',
			emphasized: false
		}
	} satisfies Record<TodaySectionId, Omit<TodaySectionMeta, 'id'>>;
	// Display order for the counts strip and section stack.
	const TODAY_SECTION_ORDER: readonly TodaySectionId[] = [
		'needs_you',
		'delivered',
		'changed',
		'active_work',
		'followups'
	];
	const TODAY_SECTION_META: TodaySectionMeta[] = TODAY_SECTION_ORDER.map((id) => ({
		id,
		...TODAY_SECTION_META_BY_ID[id]
	}));
	const TODAY_WORTH_TAB_META: TodayTabMeta = {
		id: 'worth_a_look',
		title: 'Worth a look',
		metricLabel: 'Worth',
		summary: 'Proactively resurfaced memory, task, and message items.',
		icon: 'sparkle',
		groupBySpace: false,
		tone: 'info',
		emphasized: false
	};
	// Town Square lives as the Social tab: no Today count, no section pages.
	const TODAY_SOCIAL_TAB_META: TodayTabMeta = {
		id: 'social',
		title: 'Social',
		metricLabel: 'Social',
		summary: 'The shared feed of the agent swarm — posts, replies, reactions.',
		icon: 'message',
		groupBySpace: false,
		tone: 'primary',
		emphasized: false
	};
	const TODAY_TAB_ORDER: readonly TodayTabId[] = [
		'followups',
		'worth_a_look',
		'active_work',
		'delivered',
		'changed',
		'social'
	];
	const TODAY_SECTION_PAGE_SIZE = 8;
	const TODAY_DIGEST_PAGE_SIZE = 7;
	const CHANNEL_FOLLOW_UP_PAGE_SIZE = 5;
	const ACTIVITY_SEARCH_PLACEHOLDER = 'Search title, status, task, agent, memory, insights, metadata';
	const activityFeed = createFeedStore({ limit: 80 });

	let activeFilter: ActivityFilter = 'all';
	let searchQuery = '';

	/*
	 * **No task-panel state here.** `/square`'s task panel is the Fleet HUD's
	 * Quest Journal, which mounts `TasksWorkspace` and therefore the unified
	 * panel. The page-level `ExecutionPanel` this file used to carry was
	 * unreachable: every caller of its `openTaskPanel` sat inside a handler bound
	 * only to that panel's own events, and the one remaining entry — a
	 * `?selected=` parameter — is produced nowhere in the repo. Removed with
	 * Workstream C1 of the unified-task-panel migration.
	 */
	let appliedHighlightedItemId: string | null = null;
	let activityCardRefs = new Map<string, HTMLElement>();
	let highlightedActivityItemId: string | null = null;
	let activityHighlightTimeout: ReturnType<typeof setTimeout> | null = null;

	let scrollRecords: PublishedSurfaceRecord[] = [];
	let scrollsLoading = true;
	let scrollsError: string | null = null;
	let scrollRefreshUnsubscribe: (() => void) | null = null;
	let learningActionKey: string | null = null;
	let todayVisibilityActionKey: string | null = null;
	// Snooze popover — only one open at a time, so a single trigger/menu ref
	// pair is enough (the trigger is captured at click time).
	let openSnoozeItemId: string | null = null;
	let snoozeMenuTriggerEl: HTMLButtonElement | null = null;
	let snoozeMenuEl: HTMLDivElement | null = null;
	let snoozeTonightHint = 'until 6 PM';
	// "Hidden — Undo" toast for the optimistic dismiss/snooze path.
	let undoToastMessage: string | null = null;
	let undoToastTimeout: ReturnType<typeof setTimeout> | null = null;
	let editingLearningItem: FeedItem | null = null;
	let learningEditValue = '';
	let todayMounted = false;
	// Gates section-row intros: false until the first loaded paint so the
	// initial hydrate of a full board doesn't stagger-animate every row.
	let todayRowsIntroReady = false;
	let currentTodayScopeKey = '';
	let lastTodayScopeKey = '';
	let scrollRequestId = 0;
	let resurfacingCountRequestId = 0;
	let resurfacingTotal = 0;
	let resurfacingCountError: string | null = null;
	let todaySectionViews: TodaySectionView[] = [];
	let todayTabViews: TodayTabView[] = [];
	let activeTodayTab: TodayTabId = 'followups';
	let activeTodaySectionView: TodaySectionTabView | null = null;
	let todayTabUserSelected = false;
	let todayTabPageById = initialTodayTabPages();
	let activeTodayPage = 1;
	let routeTodayTab: TodayTabId | null = null;
	let routeTodayPage = 1;
	let routeTodayDigestPage = 1;
	let routeChannelFollowUpPage = 1;
	let changedDigestPage = 1;
	let appliedTodayRouteKey = '';
	let appliedTodayStoreQueryKey = '';
	let appliedTodaySectionCursorKey = '';
	let appliedChannelFollowUpPageKey = '';
	let primingTodaySectionKey: string | null = null;
	let todayScopeGeneration = 0;
	let channelFollowUpCursorGeneration = 0;
	let todaySectionCursorsById = initialTodaySectionCursors();
	let activityExpanded = false;
	// Hidden-panel disclosure — collapsed by default; restore flow unchanged.
	let hiddenPanelExpanded = false;

	$: routeSelectedItemId = (($page.url.searchParams.get('selected_item') || '').trim() || null);
	$: routeTodayTab = parseTodayTab($page.url.searchParams.get('tab'));
	$: routeTodayPage = parseTodayPage($page.url.searchParams.get('page'));
	$: routeTodayDigestPage = parseTodayPage($page.url.searchParams.get('digest_page'));
	$: routeChannelFollowUpPage = parseTodayPage($page.url.searchParams.get('message_page'));
	$: greeting = greetingFor(new Date());
	$: todayLabel = getTodayDate();
	$: activityFeedItems = $activityFeed.items.filter(isActivityFeedItem);
	$: deliveryCount = activityFeedItems.filter(
		(item) => item.item_type === 'data_delivery' || item.item_type === 'routine_result'
	).length;
	$: learningCount = activityFeedItems.filter(
		(item) => item.item_type === 'agent_learning'
	).length;
	$: outcomeCount = activityFeedItems.filter((item) => item.item_type === 'task').length;
	$: failedCount = activityFeedItems.filter((item) => item.status === 'failed').length;
	$: filteredItems = filterActivityItems(activityFeedItems, activeFilter, searchQuery);
	$: activityFilterActive = activeFilter !== 'all' || searchQuery.trim().length > 0;
	$: hiddenTodayItems = $todayStore.hiddenItems;
	$: activitySummaryLabel = activityFilterActive
		? `${filteredItems.length} of ${activityFeedItems.length} durable activity ${activityFeedItems.length === 1 ? 'item' : 'items'} shown`
		: `${activityFeedItems.length} durable activity ${activityFeedItems.length === 1 ? 'item' : 'items'}`;
	// --- Message follow-ups (classifier actionable annotations) -------------
	// Surfaced in the Follow-ups context and rolled into that count +
	// all-clear. Today requests concrete pages from the backend needs-you API.
	let channelFollowUps: ChannelFollowUp[] = [];
	let visibleLegacyChannelFollowUps: ChannelFollowUp[] = [];
	let channelFollowUpTotal = 0;
	let channelFollowUpPage = 1;
	let channelFollowUpLoadedPage = 0;
	let channelFollowUpLoading = false;
	let channelFollowUpRequestId = 0;
	let channelFollowUpLoadError: string | null = null;
	let channelFollowUpHealth: ChannelFollowUpLearningHealth | null = null;
	let channelFollowUpActionability: AttentionActionabilityPage | null = null;
	let channelFollowUpActionabilityTraining: AttentionActionabilityTrainingStatus | null = null;
	let channelFollowUpGrouping: AttentionGroupingPage | null = null;
	let channelFollowUpRouting: AttentionRoutingPage | null = null;
	let channelFollowUpBandit: AttentionBanditHealth | null = null;
	let channelFollowUpSemanticExtraction: AttentionSemanticExtractionHealth | null = null;
	let channelFollowUpSemanticRankingEnabled = false;
	let channelFollowUpTimer: ReturnType<typeof setInterval> | null = null;
	let channelFollowUpRequest: {
		page: number;
		controller: AbortController;
		promise: Promise<void>;
	} | null = null;
	let channelFollowUpCursorsByPage: Record<number, string | null> = initialCursorPages();
	let worthCardLookup: ResurfacingCard[] = [];
	let worthLookupSettled = false;

	async function loadWorthCardLookup(): Promise<void> {
		try {
			worthCardLookup = await collectWorthCardLookup((options) =>
				fetchResurfacingTodayPage(options)
			);
		} catch {
			// A failed lookup only costs the richer control set.
		} finally {
			worthLookupSettled = true;
		}
	}

	$: canonicalAttentionProjection =
		$canonicalAttentionProjectionStore.loadedScopeKey === currentTodayScopeKey
			? $canonicalAttentionProjectionStore.projection
			: null;
	$: canonicalProjectionPending =
		$canonicalAttentionProjectionStore.loadedScopeKey !== currentTodayScopeKey ||
		($canonicalAttentionProjectionStore.isLoading && canonicalAttentionProjection === null);
	$: canonicalFollowUpDiagnostics = canonicalAttentionProjection?.diagnostics ?? null;
	$: effectiveChannelFollowUpHealth =
		canonicalFollowUpDiagnostics?.health ?? channelFollowUpHealth;
	$: effectiveChannelFollowUpActionability =
		canonicalFollowUpDiagnostics?.actionability ?? channelFollowUpActionability;
	$: effectiveChannelFollowUpGrouping =
		canonicalFollowUpDiagnostics?.grouping ?? channelFollowUpGrouping;
	$: effectiveChannelFollowUpRouting =
		canonicalFollowUpDiagnostics?.routing ?? channelFollowUpRouting;
	$: effectiveChannelFollowUpBandit =
		canonicalFollowUpDiagnostics?.bandit ?? channelFollowUpBandit;
	$: effectiveChannelFollowUpSemanticExtraction =
		$attentionSemanticExtractionStore.loadedScopeKey === currentTodayScopeKey
			? $attentionSemanticExtractionStore.health
			: channelFollowUpSemanticExtraction;
	$: effectiveChannelFollowUpSemanticRankingEnabled =
		canonicalFollowUpDiagnostics?.health?.semantic_ranking_enabled === true ||
		(canonicalAttentionProjection?.status === 'succeeded' &&
			canonicalFollowUpDiagnostics?.health?.semantic_ranking_enabled !== false) ||
		channelFollowUpSemanticRankingEnabled;
	$: effectiveResurfacingTotal = Math.max(
		0,
		(canonicalAttentionProjection
			? canonicalAttentionProjection.integrity.worth_a_look_lane_total
			: canonicalProjectionPending
				? 0
				: resurfacingTotal) -
			countScopedOptimisticMutations(
				$optimisticAttentionMutationQueue.statusByKey,
				'worth_a_look',
				$scopeIdentityStore
			)
	);
	$: effectiveChannelFollowUpTotal = Math.max(
		0,
		(canonicalAttentionProjection
			? canonicalAttentionProjection.integrity.follow_up_lane_total
			: canonicalProjectionPending
				? 0
				: channelFollowUpTotal) -
			countScopedOptimisticMutations(
				$optimisticAttentionMutationQueue.statusByKey,
				'follow_up',
				$scopeIdentityStore
			)
	);
	$: visibleLegacyChannelFollowUps = channelFollowUps.filter(
		(followUp) =>
			!$optimisticAttentionMutationQueue.statusByKey.has(
				followUpAttentionMutationKey(followUp.annotation_id, $scopeIdentityStore)
			)
	);

	function initialCursorPages(): Record<number, string | null> {
		return { 1: null };
	}

	function hasCursorPage(cursors: Record<number, string | null>, page: number): boolean {
		return Object.prototype.hasOwnProperty.call(cursors, Math.max(1, Math.floor(page)));
	}

	function cursorForPage(cursors: Record<number, string | null>, page: number): string | null {
		return cursors[Math.max(1, Math.floor(page))] ?? null;
	}

	function pruneCursorPagesAfter(
		cursors: Record<number, string | null>,
		page: number
	): Record<number, string | null> {
		const maxPage = Math.max(1, Math.floor(page));
		const next: Record<number, string | null> = { 1: cursors[1] ?? null };
		for (const [rawPage, cursor] of Object.entries(cursors)) {
			const parsedPage = Number.parseInt(rawPage, 10);
			if (Number.isFinite(parsedPage) && parsedPage > 1 && parsedPage <= maxPage) {
				next[parsedPage] = cursor;
			}
		}
		return next;
	}

	function cursorPagesChanged(
		left: Record<number, string | null>,
		right: Record<number, string | null>
	): boolean {
		const keys = new Set([...Object.keys(left), ...Object.keys(right)]);
		for (const key of keys) {
			if ((left[Number(key)] ?? null) !== (right[Number(key)] ?? null)) return true;
		}
		return false;
	}

	function nearestCursorPage(cursors: Record<number, string | null>, targetPage: number): number {
		const target = Math.max(1, Math.floor(targetPage));
		let nearest = 1;
		for (const raw of Object.keys(cursors)) {
			const page = Number.parseInt(raw, 10);
			if (Number.isFinite(page) && page <= target && page > nearest) {
				nearest = page;
			}
		}
		return nearest;
	}

	function rememberChannelFollowUpCursor(page: number, cursor: string | null): void {
		const safePage = Math.max(1, Math.floor(page));
		if (hasCursorPage(channelFollowUpCursorsByPage, safePage)) {
			if (channelFollowUpCursorsByPage[safePage] === cursor) return;
		}
		const current = pruneCursorPagesAfter(channelFollowUpCursorsByPage, safePage);
		channelFollowUpCursorsByPage = {
			...current,
			[safePage]: cursor
		};
	}

	function forgetChannelFollowUpCursorsAfter(page: number): void {
		const pruned = pruneCursorPagesAfter(channelFollowUpCursorsByPage, page);
		if (cursorPagesChanged(channelFollowUpCursorsByPage, pruned)) {
			channelFollowUpCursorsByPage = pruned;
		}
	}

	function invalidateChannelFollowUpCursorsAfter(page: number): void {
		channelFollowUpCursorGeneration += 1;
		forgetChannelFollowUpCursorsAfter(page);
		appliedChannelFollowUpPageKey = '';
	}

	async function primeChannelFollowUpCursor(
		targetPage: number,
		generation = channelFollowUpCursorGeneration,
		signal?: AbortSignal
	): Promise<boolean> {
		const target = Math.max(1, Math.floor(targetPage));
		if (target <= 1) return true;
		if (generation !== channelFollowUpCursorGeneration) return false;
		if (hasCursorPage(channelFollowUpCursorsByPage, target)) return true;
		let page = nearestCursorPage(channelFollowUpCursorsByPage, target);
		let cursor = cursorForPage(channelFollowUpCursorsByPage, page);
		while (page < target) {
			if (generation !== channelFollowUpCursorGeneration) return false;
			const res = await fetchChannelFollowUpsPage(CHANNEL_FOLLOW_UP_PAGE_SIZE, cursor, {
				signal,
				includeCanonicalProjection: false
			});
			if (signal?.aborted) return false;
			if (generation !== channelFollowUpCursorGeneration) return false;
			if (!res.ok) {
				channelFollowUpLoadError = res.error ?? 'Message follow-ups unavailable.';
				return false;
			}
			channelFollowUpTotal = res.total;
			channelFollowUpHealth = res.health;
			channelFollowUpActionability = res.actionability;
			channelFollowUpActionabilityTraining = res.actionability_training;
			channelFollowUpGrouping = res.grouping;
			channelFollowUpRouting = res.routing;
			channelFollowUpBandit = res.bandit;
			channelFollowUpSemanticExtraction = res.semantic_extraction;
			channelFollowUpSemanticRankingEnabled = res.semantic_ranking_enabled;
			const nextCursor = res.next_cursor ?? null;
			if (!nextCursor) return false;
			rememberChannelFollowUpCursor(page + 1, nextCursor);
			page += 1;
			cursor = nextCursor;
		}
		return hasCursorPage(channelFollowUpCursorsByPage, target);
	}

	function channelFollowUpPageKey(page: number): string {
		const safePage = Math.max(1, Math.floor(page));
		const cursor = hasCursorPage(channelFollowUpCursorsByPage, safePage)
			? cursorForPage(channelFollowUpCursorsByPage, safePage)
			: 'pending';
		return `${safePage}:${CHANNEL_FOLLOW_UP_PAGE_SIZE}:${cursor ?? ''}`;
	}

	async function loadTodayChannelFollowUpsOwned(
		safePage: number,
		controller: AbortController
	): Promise<void> {
		const requestId = ++channelFollowUpRequestId;
		const cursorGeneration = channelFollowUpCursorGeneration;
		channelFollowUpLoading = true;
		if (safePage > 1 && !hasCursorPage(channelFollowUpCursorsByPage, safePage)) {
			const primed = await primeChannelFollowUpCursor(
				safePage,
				cursorGeneration,
				controller.signal
			);
			if (
				controller.signal.aborted
				||
				requestId !== channelFollowUpRequestId
				|| cursorGeneration !== channelFollowUpCursorGeneration
			) return;
			if (!primed) {
				channelFollowUpLoading = false;
				channelFollowUpLoadedPage = safePage;
				channelFollowUpLoadError =
					channelFollowUpLoadError ?? 'Message follow-ups page is no longer available.';
				return;
			}
		}
		const cursor = cursorForPage(channelFollowUpCursorsByPage, safePage);
		appliedChannelFollowUpPageKey = channelFollowUpPageKey(safePage);
		const res = await fetchChannelFollowUpsPage(CHANNEL_FOLLOW_UP_PAGE_SIZE, cursor, {
			signal: controller.signal,
			includeCanonicalProjection: false
		});
		if (
			controller.signal.aborted
			||
			requestId !== channelFollowUpRequestId
			|| cursorGeneration !== channelFollowUpCursorGeneration
		) return;
		channelFollowUpLoading = false;
		channelFollowUpLoadedPage = safePage;
		if (!res.ok) {
			channelFollowUpLoadError = res.error ?? 'Message follow-ups unavailable.';
			return;
		}
		channelFollowUpLoadError = null;
		channelFollowUps = res.items;
		channelFollowUpTotal = res.total;
		channelFollowUpHealth = res.health;
		channelFollowUpActionability = res.actionability;
		channelFollowUpActionabilityTraining = res.actionability_training;
		channelFollowUpGrouping = res.grouping;
		channelFollowUpRouting = res.routing;
		channelFollowUpBandit = res.bandit;
		channelFollowUpSemanticExtraction = res.semantic_extraction;
		channelFollowUpSemanticRankingEnabled = res.semantic_ranking_enabled;
		rememberChannelFollowUpCursor(safePage, res.cursor ?? cursor);
		if (res.next_cursor) {
			rememberChannelFollowUpCursor(safePage + 1, res.next_cursor);
		} else {
			forgetChannelFollowUpCursorsAfter(safePage);
		}
	}

	function loadTodayChannelFollowUps(page = channelFollowUpPage): Promise<void> {
		const safePage = Math.max(1, Math.floor(page));
		if (channelFollowUpRequest?.page === safePage) return channelFollowUpRequest.promise;
		channelFollowUpRequest?.controller.abort();
		const controller = new AbortController();
		const promise = loadTodayChannelFollowUpsOwned(safePage, controller).finally(() => {
			if (channelFollowUpRequest?.controller === controller) channelFollowUpRequest = null;
		});
		channelFollowUpRequest = { page: safePage, controller, promise };
		return promise;
	}

	function syncChannelFollowUpsPage(): void {
		const key = channelFollowUpPageKey(channelFollowUpPage);
		if (key === appliedChannelFollowUpPageKey) return;
		appliedChannelFollowUpPageKey = key;
		void loadTodayChannelFollowUps(channelFollowUpPage);
	}

	function channelFollowUpPageCount(): number {
		return Math.max(1, Math.ceil(effectiveChannelFollowUpTotal / CHANNEL_FOLLOW_UP_PAGE_SIZE));
	}

	function channelFollowUpPageStart(): number {
		if (effectiveChannelFollowUpTotal === 0) return 0;
		return (channelFollowUpPage - 1) * CHANNEL_FOLLOW_UP_PAGE_SIZE + 1;
	}

	function channelFollowUpPageEnd(): number {
		if (effectiveChannelFollowUpTotal === 0) return 0;
		return Math.min(
			effectiveChannelFollowUpTotal,
			(channelFollowUpPage - 1) * CHANNEL_FOLLOW_UP_PAGE_SIZE + visibleLegacyChannelFollowUps.length
		);
	}

	function channelFollowUpsShouldRender(): boolean {
		return (
			canonicalAttentionProjection !== null
			|| canonicalProjectionPending
			||
			channelFollowUpLoading
			|| channelFollowUpLoadError !== null
			|| channelFollowUpTotal > 0
			|| visibleLegacyChannelFollowUps.length > 0
		);
	}

	function goToChannelFollowUpPage(
		page: number,
		options: { replaceState: boolean } = { replaceState: false }
	): void {
		const nextPage = Math.max(1, Math.floor(page));
		channelFollowUpPage = nextPage;
		if (activeTodayTab === 'followups') {
			ensureTodayTabUrl(activeTodayTab, activeTodayPage, {
				replaceState: options.replaceState,
				messagePage: nextPage
			});
		}
		syncChannelFollowUpsPage();
	}

	function onTodayChannelFollowUpResolved(
		e: CustomEvent<{
			id: string;
			message: string;
		}>
	): void {
		channelFollowUps = channelFollowUps.filter((f) => f.annotation_id !== e.detail.id);
		channelFollowUpTotal = Math.max(0, channelFollowUpTotal - 1);
		invalidateChannelFollowUpCursorsAfter(channelFollowUpPage);
		void loadTodayChannelFollowUps();
	}
	function onTodayChannelFollowUpFailed(e: CustomEvent<{ error: string }>): void {
		showError(`Message action failed: ${e.detail.error}`);
	}
	function onTodayChannelFollowUpGroupChanged(): void {
		invalidateChannelFollowUpCursorsAfter(channelFollowUpPage);
		void loadTodayChannelFollowUps();
	}

	async function loadResurfacingCount(): Promise<void> {
		const rid = ++resurfacingCountRequestId;
		try {
			const page = await fetchResurfacingTodayPage({ limit: 1, offset: 0 });
			if (rid !== resurfacingCountRequestId) return;
			resurfacingTotal = page.total;
			resurfacingCountError = null;
		} catch (error) {
			if (rid !== resurfacingCountRequestId) return;
			resurfacingCountError =
				error instanceof Error ? error.message : 'Failed to load Worth a look count';
			console.warn('Failed to load resurfacing count', error);
		}
	}

	function onResurfacingCountChange(event: CustomEvent<{ total: number }>): void {
		resurfacingTotal = Math.max(0, event.detail.total);
		resurfacingCountError = null;
	}

	function onResurfacingPageChange(event: CustomEvent<{ page: number }>): void {
		const nextPage = Math.max(1, Math.floor(event.detail.page));
		todayTabPageById = {
			...todayTabPageById,
			worth_a_look: nextPage
		};
		if (activeTodayTab !== 'worth_a_look') return;
		activeTodayPage = nextPage;
		ensureTodayTabUrl('worth_a_look', nextPage, { replaceState: false });
	}

	function onCanonicalAttentionResolved(): void {
		void loadTodayChannelFollowUps();
		void loadResurfacingCount();
	}

	function onCanonicalAttentionFailed(event: CustomEvent<{ error: string }>): void {
		showError(`Attention action failed: ${event.detail.error}`);
	}

	$: todaySections = $todayStore.sections;
	$: todaySectionViews = TODAY_SECTION_META.map((meta) => ({
		meta,
		items: todaySections[meta.id]
	}));
	$: todayTabViews = todayTabOrder($todayStore.counts.needs_you)
		.map((id) => todayTabViewFor(id))
		.filter((section): section is TodayTabView => section !== null);
	$: activeTodaySectionView =
		isTodaySectionId(activeTodayTab)
			? (todayTabViews.find((section) => section.meta.id === activeTodayTab) as
					| TodaySectionTabView
					| undefined) ?? null
			: null;
	$: activeTodayPage = todayTabPageById[activeTodayTab] ?? 1;
	$: todayHasLoaded = $todayStore.lastLoadedAt !== null;
	// Structural all-clear: driven by counts.needs_you, not by matching the
	// headline copy, so backend phrasing changes can't break the state.
	// Gated on todayHasLoaded so the band never flashes during initial load.
	// All-clear only when BOTH the HITL needs-you count and message follow-ups
	// are zero.
	$: todayAllClear =
		todayHasLoaded &&
		$todayStore.counts.needs_you === 0 &&
		$todayStore.counts.followups === 0 &&
		effectiveChannelFollowUpTotal === 0 &&
		!channelFollowUpLoadError;
	$: todayHeadlineExtra = todayHeadlineDisplayText($todayStore.headline);

	$: if (browser && todayMounted) {
		routeTodayTab;
		routeTodayPage;
		routeTodayDigestPage;
		routeChannelFollowUpPage;
		applyTodayRouteSelection();
	}

	$: if (browser && todayMounted) {
		activeTodayTab;
		activeTodayPage;
		changedDigestPage;
		syncTodayStoreQuery();
	}

	$: if (browser && todayMounted && $todayStore.sectionPage) {
		const sectionPage = $todayStore.sectionPage;
		if (isTodaySectionId(sectionPage.section)) {
			const loadedPage = todayTabPageById[sectionPage.section] ?? 1;
			const nextCursor = sectionPage.next_cursor ?? null;
			const cursorKey = `${sectionPage.section}:${loadedPage}:${nextCursor ?? ''}`;
			if (cursorKey !== appliedTodaySectionCursorKey) {
				appliedTodaySectionCursorKey = cursorKey;
				if (nextCursor) {
					rememberTodaySectionCursor(sectionPage.section, loadedPage + 1, nextCursor);
				} else {
					forgetTodaySectionCursorsAfter(sectionPage.section, loadedPage);
				}
			}
		}
	}

	$: if (browser && todayMounted) {
		channelFollowUpPage;
		syncChannelFollowUpsPage();
	}

	// `todayMounted` gates this reactor because it can run at INIT with a
	// warm today store (lastLoadedAt survives stop()) — before onMount has
	// applied the deep-linked `?tab=`, which would rewrite the URL to the
	// default tab out from under e.g. /square?tab=social.
	$: if (browser && todayMounted && todayHasLoaded) {
		const activeTabAvailable = todayTabViews.some((tab) => tab.meta.id === activeTodayTab);
		if (!activeTabAvailable) {
			const fallbackTab = todayDefaultTab(todayTabViews);
			activeTodayTab = fallbackTab;
			todayTabPageById = {
				...todayTabPageById,
				[fallbackTab]: 1
			};
			todayTabUserSelected = false;
			syncTodayStoreQueryFor(fallbackTab, 1);
			ensureTodayTabUrl(fallbackTab, 1, { replaceState: true });
		} else if (!todayTabUserSelected) {
			const nextDefaultTab = todayDefaultTab(todayTabViews);
			if (activeTodayTab !== nextDefaultTab) {
				activeTodayTab = nextDefaultTab;
				todayTabPageById = {
					...todayTabPageById,
					[nextDefaultTab]: 1
				};
				syncTodayStoreQueryFor(nextDefaultTab, 1);
			}
			ensureTodayTabUrl(nextDefaultTab, todayTabPageById[nextDefaultTab] ?? 1, {
				replaceState: true
			});
		}
	}

	$: if (browser && todayHasLoaded && activeTodaySectionView) {
		const pageCount = todaySectionPageCount(activeTodaySectionView);
		if (activeTodayPage > pageCount) {
			goToTodayTabPage(activeTodaySectionView.meta.id, pageCount, { replaceState: true });
		}
	}

	$: if (
		browser
		&& todayMounted
		&& channelFollowUpLoadedPage > 0
		&& channelFollowUpPage > channelFollowUpPageCount()
	) {
		goToChannelFollowUpPage(channelFollowUpPageCount(), { replaceState: true });
	}

	$: if (
		browser
		&& todayMounted
		&& activeTodayTab === 'changed'
		&& todayDigestPageLoaded(changedDigestPage, $todayStore.loadedDigestPageKey)
		&& changedDigestPage > todayDigestPageCount()
	) {
		goToTodayDigestPage(todayDigestPageCount(), { replaceState: true });
	}

	// Flip the intro gate only AFTER the first loaded payload has painted:
	// intros read the flag when they start (during the same flush that
	// mounts the rows), and tick() resolves after that flush — so the
	// initial batch mounts silently while later insertions settle in.
	// Re-armed on scope switches (the store resets lastLoadedAt to null).
	$: if (browser && todayHasLoaded && !todayRowsIntroReady) {
		void tick().then(() => {
			todayRowsIntroReady = true;
		});
	}

	// The backend's boilerplate all-clear phrasing duplicates the band's own
	// copy — suppress just that text; any other headline still renders. This
	// string check no longer GATES the all-clear state (counts do).
	function todayHeadlineDisplayText(headline: string): string | null {
		const trimmed = (headline || '').trim();
		if (!trimmed) return null;
		return trimmed.toLowerCase().startsWith('nothing needs you') ? null : trimmed;
	}

	function initialTodayTabPages(): Record<TodayTabId, number> {
		return {
			needs_you: 1,
			followups: 1,
			worth_a_look: 1,
			active_work: 1,
			delivered: 1,
			social: 1,
			changed: 1
		};
	}

	function initialTodaySectionCursors(): Record<TodaySectionId, Record<number, string | null>> {
		return {
			needs_you: initialCursorPages(),
			followups: initialCursorPages(),
			active_work: initialCursorPages(),
			delivered: initialCursorPages(),
			changed: initialCursorPages()
		};
	}

	function hasTodaySectionCursorPage(sectionId: TodaySectionId, page: number): boolean {
		return hasCursorPage(todaySectionCursorsById[sectionId] ?? initialCursorPages(), page);
	}

	function todaySectionCursorForPage(sectionId: TodaySectionId, page: number): string | null {
		return cursorForPage(todaySectionCursorsById[sectionId] ?? initialCursorPages(), page);
	}

	function rememberTodaySectionCursor(
		sectionId: TodaySectionId,
		page: number,
		cursor: string | null
	): void {
		const safePage = Math.max(1, Math.floor(page));
		const current = todaySectionCursorsById[sectionId] ?? initialCursorPages();
		if (hasCursorPage(current, safePage) && current[safePage] === cursor) return;
		const pruned = pruneCursorPagesAfter(current, safePage);
		todaySectionCursorsById = {
			...todaySectionCursorsById,
			[sectionId]: {
				...pruned,
				[safePage]: cursor
			}
		};
	}

	function forgetTodaySectionCursorsAfter(sectionId: TodaySectionId, page: number): void {
		const current = todaySectionCursorsById[sectionId] ?? initialCursorPages();
		const pruned = pruneCursorPagesAfter(current, page);
		if (!cursorPagesChanged(current, pruned)) return;
		todaySectionCursorsById = {
			...todaySectionCursorsById,
			[sectionId]: pruned
		};
	}

	function invalidateTodaySectionCursorsAfter(sectionId: TodaySectionId, page: number): void {
		todayScopeGeneration += 1;
		forgetTodaySectionCursorsAfter(sectionId, page);
		appliedTodaySectionCursorKey = '';
		appliedTodayStoreQueryKey = '';
		primingTodaySectionKey = null;
	}

	function invalidateAllTodaySectionCursors(): void {
		todayScopeGeneration += 1;
		todaySectionCursorsById = initialTodaySectionCursors();
		appliedTodaySectionCursorKey = '';
		appliedTodayStoreQueryKey = '';
		primingTodaySectionKey = null;
	}

	async function fetchTodaySectionCursorPage(
		sectionId: TodaySectionId,
		cursor: string | null
	): Promise<TodaySectionPage | null> {
		const response = await fetch(
			todaySectionCursorProbeUrl({
				sectionId,
				cursor,
				sectionPageSize: TODAY_SECTION_PAGE_SIZE,
				digestPageSize: TODAY_DIGEST_PAGE_SIZE
			})
		);
		if (!response.ok) {
			const text = await response.text().catch(() => '');
			throw new Error(text || `Failed to load ${sectionId} page (${response.status})`);
		}
		const body = (await response.json()) as { section_page?: TodaySectionPage | null };
		return body.section_page ?? null;
	}

	async function primeTodaySectionCursor(
		sectionId: TodaySectionId,
		targetPage: number,
		generation = todayScopeGeneration,
		scopeKey = todayScopeKey()
	): Promise<boolean> {
		const target = Math.max(1, Math.floor(targetPage));
		if (target <= 1) return true;
		if (generation !== todayScopeGeneration || scopeKey !== todayScopeKey()) return false;
		if (hasTodaySectionCursorPage(sectionId, target)) return true;
		const sectionCursors = todaySectionCursorsById[sectionId] ?? initialCursorPages();
		let page = nearestCursorPage(sectionCursors, target);
		let cursor = todaySectionCursorForPage(sectionId, page);
		while (page < target) {
			if (generation !== todayScopeGeneration || scopeKey !== todayScopeKey()) return false;
			const pageInfo = await fetchTodaySectionCursorPage(sectionId, cursor);
			if (generation !== todayScopeGeneration || scopeKey !== todayScopeKey()) return false;
			const nextCursor = pageInfo?.next_cursor ?? null;
			if (!nextCursor) return false;
			rememberTodaySectionCursor(sectionId, page + 1, nextCursor);
			page += 1;
			cursor = nextCursor;
		}
		return hasTodaySectionCursorPage(sectionId, target);
	}

	function isTodaySectionId(value: TodayTabId | string | null): value is TodaySectionId {
		return !!value && !!TODAY_SECTION_META_BY_ID[value as TodaySectionId];
	}

	function todaySectionTabViewFor(sectionId: TodaySectionId): TodaySectionTabView | null {
		const section = todaySectionViews.find((view) => view.meta.id === sectionId);
		if (!section) return null;
		const sourceCount = $todayStore.counts[section.meta.id] ?? 0;
		return {
			...section,
			sourceCount,
			count: section.meta.id === 'followups' ? sourceCount + effectiveChannelFollowUpTotal : sourceCount
		};
	}

	function todayTabViewFor(tabId: TodayTabId): TodayTabView | null {
		if (tabId === 'social') {
			return { meta: TODAY_SOCIAL_TAB_META, items: [], sourceCount: 0, count: 0 };
		}
		if (tabId === 'worth_a_look') {
			return {
				meta: TODAY_WORTH_TAB_META,
				items: [],
				sourceCount: effectiveResurfacingTotal,
				count: effectiveResurfacingTotal
			};
		}
		return todaySectionTabViewFor(tabId);
	}

	function parseTodayTab(value: string | null): TodayTabId | null {
		const tab = (value || '').trim();
		if (tab === 'worth_a_look' || tab === 'social') return tab;
		return isTodaySectionId(tab) ? tab : null;
	}

	function parseTodayPage(value: string | null): number {
		const page = Number.parseInt((value || '').trim(), 10);
		return Number.isFinite(page) && page > 1 ? page : 1;
	}

	function todayRouteKey(
		sectionId: TodayTabId | null,
		page: number,
		digestPage: number,
		messagePage: number
	): string {
		return `${sectionId ?? ''}:${Math.max(1, page)}:${Math.max(1, digestPage)}:${Math.max(1, messagePage)}`;
	}

	function todayStoreQueryFor(sectionId: TodaySectionId, page: number): TodayStoreQuery {
		const safePage = Math.max(1, Math.floor(page));
		const safeDigestPage = Math.max(1, Math.floor(changedDigestPage));
		return {
			per_section: TODAY_SECTION_PAGE_SIZE,
			section: sectionId,
			limit: TODAY_SECTION_PAGE_SIZE,
			cursor: todaySectionCursorForPage(sectionId, safePage),
			digest_limit: TODAY_DIGEST_PAGE_SIZE,
			digest_offset:
				sectionId === 'changed' ? (safeDigestPage - 1) * TODAY_DIGEST_PAGE_SIZE : 0
		};
	}

	function todayDigestPageKey(page: number): string {
		const safePage = Math.max(1, Math.floor(page));
		return `${TODAY_DIGEST_PAGE_SIZE}:${(safePage - 1) * TODAY_DIGEST_PAGE_SIZE}`;
	}

	function todayDigestPageLoaded(page: number, loadedDigestPageKey: string | null): boolean {
		return loadedDigestPageKey === todayDigestPageKey(page);
	}

	function todaySectionPageKey(sectionId: TodaySectionId, page: number): string {
		const safePage = Math.max(1, Math.floor(page));
		return `${TODAY_SECTION_PAGE_SIZE}:${todaySectionCursorForPage(sectionId, safePage) ?? ''}`;
	}

	function todaySectionPageLoaded(
		sectionId: TodaySectionId,
		page: number,
		loadedSectionPageKeys: Record<TodaySectionId, string | null>
	): boolean {
		return loadedSectionPageKeys[sectionId] === todaySectionPageKey(sectionId, page);
	}

	function todayStoreQueryKey(query: TodayStoreQuery): string {
		return JSON.stringify({
			section: query.section ?? null,
			limit: query.limit ?? null,
			cursor: query.cursor ?? null,
			digest_limit: query.digest_limit ?? null,
			digest_offset: query.digest_offset ?? null
		});
	}

	function syncTodayStoreQueryFor(sectionId: TodayTabId, page: number): void {
		if (!isTodaySectionId(sectionId)) return;
		if (page > 1 && !hasTodaySectionCursorPage(sectionId, page)) {
			const primeKey = `${sectionId}:${page}`;
			if (primingTodaySectionKey === primeKey) return;
			const primeGeneration = todayScopeGeneration;
			const primeScopeKey = todayScopeKey();
			primingTodaySectionKey = primeKey;
			void primeTodaySectionCursor(sectionId, page, primeGeneration, primeScopeKey)
				.then((primed) => {
					if (
						primeGeneration !== todayScopeGeneration
						|| primeScopeKey !== todayScopeKey()
					) return;
					if (primingTodaySectionKey === primeKey) {
						primingTodaySectionKey = null;
					}
					if (!primed) {
						if (activeTodayTab === sectionId && (todayTabPageById[sectionId] ?? 1) === page) {
							goToTodayTabPage(sectionId, 1, { replaceState: true });
						}
						return;
					}
					if (activeTodayTab === sectionId && (todayTabPageById[sectionId] ?? 1) === page) {
						syncTodayStoreQueryFor(sectionId, page);
					}
				})
				.catch((error) => {
					if (
						primeGeneration !== todayScopeGeneration
						|| primeScopeKey !== todayScopeKey()
					) return;
					if (primingTodaySectionKey === primeKey) {
						primingTodaySectionKey = null;
					}
					showError(error instanceof Error ? error.message : 'Failed to load Today page');
				});
			return;
		}
		const query = todayStoreQueryFor(sectionId, page);
		const key = todayStoreQueryKey(query);
		if (key === appliedTodayStoreQueryKey) return;
		appliedTodayStoreQueryKey = key;
		todayStore.setQuery(query);
	}

	function syncTodayStoreQuery(): void {
		syncTodayStoreQueryFor(activeTodayTab, activeTodayPage);
	}

	function applyTodayRouteSelection(): void {
		const key = todayRouteKey(
			routeTodayTab,
			routeTodayPage,
			routeTodayDigestPage,
			routeChannelFollowUpPage
		);
		if (key === appliedTodayRouteKey) return;
		appliedTodayRouteKey = key;
		if (!routeTodayTab) {
			todayTabUserSelected = false;
			return;
		}
		activeTodayTab = routeTodayTab;
		todayTabPageById = {
			...todayTabPageById,
			[routeTodayTab]: routeTodayPage
		};
		if (routeTodayTab === 'changed') {
			changedDigestPage = routeTodayDigestPage;
		}
		if (routeTodayTab === 'followups') {
			channelFollowUpPage = routeChannelFollowUpPage;
		}
		todayTabUserSelected = true;
	}

	function todayTabUrl(
		sectionId: TodayTabId,
		pageNumber: number,
		extra: { digestPage?: number; messagePage?: number } = {}
	): string {
		const currentPage = get(page);
		const params = new URLSearchParams(currentPage.url.searchParams);
		params.set('tab', sectionId);
		if (pageNumber > 1) {
			params.set('page', String(pageNumber));
		} else {
			params.delete('page');
		}
		const digestPage = extra.digestPage ?? changedDigestPage;
		if (sectionId === 'changed' && digestPage > 1) {
			params.set('digest_page', String(digestPage));
		} else {
			params.delete('digest_page');
		}
		const messagePage = extra.messagePage ?? channelFollowUpPage;
		if (sectionId === 'followups' && messagePage > 1) {
			params.set('message_page', String(messagePage));
		} else {
			params.delete('message_page');
		}
		const query = params.toString();
		return query ? `${currentPage.url.pathname}?${query}` : currentPage.url.pathname;
	}

	function ensureTodayTabUrl(
		sectionId: TodayTabId,
		pageNumber: number,
		options: { replaceState: boolean; digestPage?: number; messagePage?: number } = {
			replaceState: true
		}
	): void {
		if (!browser) return;
		const currentPage = get(page);
		const nextUrl = todayTabUrl(sectionId, pageNumber, options);
		const currentUrl = currentPage.url.search + currentPage.url.hash
			? `${currentPage.url.pathname}${currentPage.url.search}${currentPage.url.hash}`
			: currentPage.url.pathname;
		if (nextUrl === currentUrl) return;
		void goto(nextUrl, { replaceState: options.replaceState, noScroll: true });
	}

	function todayTabOrder(needsYouCount: number): TodayTabId[] {
		const tabs: TodayTabId[] = [];
		if (needsYouCount > 0) {
			tabs.push('needs_you');
		}
		tabs.push(...TODAY_TAB_ORDER);
		return tabs;
	}

	function todayDefaultTab(tabs: TodayTabView[]): TodayTabId {
		return tabs.find((tab) => tab.count > 0)?.meta.id ?? TODAY_TAB_ORDER[0];
	}

	function selectTodayTab(sectionId: TodayTabId): void {
		activeTodayTab = sectionId;
		todayTabUserSelected = true;
		const page = todayTabPageById[sectionId] ?? 1;
		syncTodayStoreQueryFor(sectionId, page);
		ensureTodayTabUrl(sectionId, page, { replaceState: false });
		closeSnoozeMenu(false);
	}

	function todayTabId(sectionId: TodayTabId): string {
		return `today-tab-${sectionId.replace(/_/g, '-')}`;
	}

	function todayTabPanelId(sectionId: TodayTabId): string {
		return `today-tab-panel-${sectionId.replace(/_/g, '-')}`;
	}

	function todayTabClass(sectionId: TodayTabId): string {
		return `today-section-tab today-section-tab--${sectionId.replace(/_/g, '-')}`;
	}

	function todayTabEmptyTitle(sectionId: TodaySectionId): string {
		switch (sectionId) {
			case 'followups':
				return 'No follow-ups right now.';
			case 'active_work':
				return 'No active work is running.';
			case 'delivered':
				return 'Nothing delivered yet today.';
			case 'changed':
				return 'No durable changes yet.';
			case 'needs_you':
				return 'Nothing needs you right now.';
		}
	}

	function todayTabEmptyBody(sectionId: TodaySectionId): string {
		switch (sectionId) {
			case 'followups':
				return 'Due, blocked, or stale work will appear here when it needs a next action.';
			case 'active_work':
				return 'Running tasks and live work will appear here once they start.';
			case 'delivered':
				return 'Finished work with something useful to open will appear here.';
			case 'changed':
				return 'Memory, state, and other durable changes will appear here.';
			case 'needs_you':
				return 'Approvals, blockers, decisions, and message follow-ups are clear.';
		}
	}

	function todaySectionPaginationTotal(section: TodayTabView): number {
		return section.meta.id === 'followups' ? section.sourceCount : section.count;
	}

	function todaySectionPageCount(section: TodayTabView): number {
		return Math.max(1, Math.ceil(todaySectionPaginationTotal(section) / TODAY_SECTION_PAGE_SIZE));
	}

	function todaySectionPageStart(section: TodayTabView): number {
		const total = todaySectionPaginationTotal(section);
		if (total === 0) return 0;
		return (activeTodayPage - 1) * TODAY_SECTION_PAGE_SIZE + 1;
	}

	function todaySectionPageEnd(section: TodayTabView): number {
		const total = todaySectionPaginationTotal(section);
		if (total === 0) return 0;
		return Math.min(total, (activeTodayPage - 1) * TODAY_SECTION_PAGE_SIZE + section.items.length);
	}

	function todayDigestPageCount(): number {
		return Math.max(1, Math.ceil($todayStore.digest.total / TODAY_DIGEST_PAGE_SIZE));
	}

	function todayDigestPageStart(): number {
		if ($todayStore.digest.total === 0) return 0;
		return (changedDigestPage - 1) * TODAY_DIGEST_PAGE_SIZE + 1;
	}

	function todayDigestPageEnd(): number {
		if ($todayStore.digest.total === 0) return 0;
		return Math.min(
			$todayStore.digest.total,
			(changedDigestPage - 1) * TODAY_DIGEST_PAGE_SIZE + $todayStore.digest.bullets.length
		);
	}

	function goToTodayDigestPage(
		page: number,
		options: { replaceState: boolean } = { replaceState: false }
	): void {
		const nextPage = Math.max(1, Math.floor(page));
		changedDigestPage = nextPage;
		if (activeTodayTab === 'changed') {
			syncTodayStoreQueryFor('changed', activeTodayPage);
			ensureTodayTabUrl('changed', activeTodayPage, {
				replaceState: options.replaceState,
				digestPage: nextPage
			});
		}
	}

	function goToTodayTabPage(
		sectionId: TodaySectionId,
		page: number,
		options: { replaceState: boolean } = { replaceState: false }
	): void {
		const nextPage = Math.max(1, Math.floor(page));
		todayTabPageById = {
			...todayTabPageById,
			[sectionId]: nextPage
		};
		if (sectionId === activeTodayTab) {
			activeTodayPage = nextPage;
			syncTodayStoreQueryFor(sectionId, nextPage);
			ensureTodayTabUrl(sectionId, nextPage, { replaceState: options.replaceState });
		}
	}

	function getTodayDate(): string {
		return new Date().toLocaleDateString('en-US', {
			weekday: 'long',
			month: 'long',
			day: 'numeric'
		});
	}

	function todayScopeKey(): string {
		const scope = get(scopeIdentityStore);
		return `${scope.principal}:${scope.workspace}`;
	}

	function normalizeActivitySearch(value: string): string {
		return value.trim().toLowerCase();
	}

	function activitySearchTerms(value: string): string[] {
		return normalizeActivitySearch(value)
			.split(/\s+/)
			.map((term) => term.trim())
			.filter(Boolean);
	}

	function isActivityFeedItem(item: FeedItem): boolean {
		switch (item.item_type) {
			// `agent_learning` is the user-facing knowledge digest
			// projected by `feed/agent_learnings_projection.rs` from
			// research findings, contacts, routines, preferences,
			// skills, workflows. Replaces `learning_candidate` /
			// `learning_insight` on Today — those are internal
			// telemetry (reflection completions, evaluation runs,
			// memory-promotion review queue) and now live on /feed
			// only. See magician CHANGELOG v0.6.580.
			case 'agent_learning':
			case 'data_delivery':
			case 'routine_result':
				return true;
			case 'task':
				return isDurableTaskOutcome(item);
			default:
				return false;
		}
	}

	function isDurableTaskOutcome(item: FeedItem): boolean {
		if (item.status === 'failed') return true;
		if (item.status !== 'done') return false;
		if (item.summary?.trim()) return true;
		if (readMetadataString(item, 'completion_outcome')) return true;
		const artifactNames = readMetadataValue(item, 'completion_artifact_names');
		if (Array.isArray(artifactNames) && artifactNames.length > 0) return true;
		if (typeof artifactNames === 'string' && artifactNames.trim()) return true;
		return false;
	}

	function filterActivityItems(items: FeedItem[], filter: ActivityFilter, search: string): FeedItem[] {
		const searchTerms = activitySearchTerms(search);
		return items.filter((item) => {
			if (filter === 'learnings' && item.item_type !== 'agent_learning') return false;
			if (filter === 'outcomes' && item.item_type !== 'task') return false;
			if (filter === 'failed' && item.status !== 'failed') return false;
			if (
				filter === 'deliveries'
				&& item.item_type !== 'data_delivery'
				&& item.item_type !== 'routine_result'
			) {
				return false;
			}
			if (searchTerms.length === 0) return true;
			const haystack = activityItemSearchText(item);
			return searchTerms.every((term) => haystack.includes(term));
		});
	}

	function activityItemSearchText(item: FeedItem): string {
		return [
			item.id,
			item.title,
			item.summary || '',
			item.status,
			titleCase(item.status),
			item.item_type,
			activityTypeLabel(item.item_type),
			item.agent_id || '',
			userVisibleThreadId(item) || '',
			item.task_id || '',
			readMetadataString(item, 'memory_key') || '',
			metadataValueLabel(readMetadataValue(item, 'memory_value')) || '',
			readMetadataString(item, 'target_tier') || '',
			readMetadataString(item, 'target_scope') || '',
			readMetadataString(item, 'risk_level') || '',
			...item.actions.flatMap((action) => [
				action.id,
				action.label,
				action.action_type || '',
				boundedSearchText(action.payload)
			]),
			boundedSearchText(item.metadata)
		]
			.join(' ')
			.toLowerCase();
	}

	function userVisibleThreadId(item: FeedItem): string | null {
		const threadId = item.ui_thread_id?.trim();
		if (!threadId || threadId.startsWith('system:')) return null;
		return threadId;
	}

	function boundedSearchText(value: unknown): string {
		if (value === null || value === undefined) return '';
		if (typeof value === 'string') return value;
		if (typeof value === 'number' || typeof value === 'boolean') return String(value);
		try {
			return JSON.stringify(value).slice(0, 4000);
		} catch {
			return '';
		}
	}

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

	function titleCase(value: string | null | undefined): string {
		if (!value) return 'Unknown';
		return value.replace(/_/g, ' ').replace(/\b\w/g, (match) => match.toUpperCase());
	}

	function activityTypeIcon(type: FeedItemType, status: FeedItemStatus): IconName {
		if (status === 'failed') return 'x';
		if (status === 'needs_action') return 'alert';
		if (type === 'approval') return 'check';
		if (type === 'agent_learning') return 'sparkle';
		if (type === 'learning_candidate') return 'sparkle';
		if (type === 'learning_insight') return 'eye';
		if (type === 'agent_message') return 'message';
		if (type === 'data_delivery' || type === 'routine_result') return 'inbox';
		if (status === 'running') return 'rotate-ccw';
		return 'info';
	}

	function activityTypeLabel(type: FeedItemType): string {
		switch (type) {
			case 'routine_result':
				return 'Routine';
			case 'data_delivery':
				return 'Delivery';
			case 'agent_message':
				return 'Message';
			case 'agent_learning':
				return 'Learning';
			case 'learning_candidate':
				return 'Candidate';
			case 'learning_insight':
				return 'Telemetry';
			default:
				return titleCase(type);
		}
	}

	function statusColor(status: FeedItemStatus): 'default' | 'success' | 'warning' | 'error' | 'info' {
		switch (status) {
			case 'done':
				return 'success';
			case 'needs_action':
				return 'warning';
			case 'failed':
				return 'error';
			case 'running':
				return 'info';
			default:
				return 'default';
		}
	}

	function todaySectionClass(section: TodaySectionId): string {
		return `today-section today-section--${section.replace(/_/g, '-')}`;
	}

	function todayItemIcon(item: TodayItem): IconName {
		if (item.status === 'failed') return 'x';
		return TODAY_SECTION_META_BY_ID[item.section]?.icon ?? 'sparkle';
	}

	function todaySourceLabel(item: TodayItem): string {
		return todaySourceKindLabel(item.source_kind);
	}

	function todaySourceKindLabel(sourceKind: string): string {
		if (sourceKind === 'published_surface') return 'Briefing';
		if (sourceKind === 'routine_result') return 'Routine';
		if (sourceKind === 'memory_learning_digest') return 'Memory';
		if (sourceKind === 'memory_learning') return 'Memory';
		if (sourceKind === 'agent_message') return 'Thread';
		return titleCase(sourceKind);
	}

	function todayPrimaryActionLabel(item: TodayItem): string {
		if (item.section === 'needs_you') return 'Review';
		if (item.section === 'delivered') return 'Open';
		if (item.section === 'active_work') return 'Inspect';
		if (item.section === 'followups') return 'Open';
		return 'Open';
	}

	async function openTodayAttentionItem(item: TodayItem): Promise<void> {
		const target = todayHitlOpenTarget(item);
		if (!target) {
			if (!openAttentionRoute(item.source_url, todayAttentionItemId(item))) {
				openAttentionCenter();
			}
			return;
		}
		const result = await openHitlPrompt(target);
		if (result.status === 'error') showError(result.error);
	}

	async function openTodayItem(item: TodayItem): Promise<void> {
		void todayStore.updateVisibility(item.id, 'mark_seen').catch(() => {});
		const sourceUrl = item.source_url?.trim();
		if (todayHitlOpenTarget(item)) {
			await openTodayAttentionItem(item);
			return;
		}
		if (parseAttentionRouteIntent(sourceUrl)) {
			await openTodayAttentionItem(item);
			return;
		}
		if (item.section === 'needs_you' && !sourceUrl) {
			await openTodayAttentionItem(item);
			return;
		}
		if (sourceUrl?.startsWith('/') && !(item.task_id && isTaskSelectionRoute(sourceUrl))) {
			await goto(sourceUrl, { replaceState: false, noScroll: true });
			return;
		}
		if (item.task_id) {
			await openTaskRoute(item.task_id);
			return;
		}
		if (sourceUrl?.startsWith('/')) {
			await goto(sourceUrl, { replaceState: false, noScroll: true });
			return;
		}
		if (item.thread_id) {
			await goto(`/t/${encodeURIComponent(item.thread_id)}`, {
				replaceState: false,
				noScroll: true
			});
			return;
		}
		if (item.section === 'needs_you') {
			await openTodayAttentionItem(item);
			return;
		}
		if (item.section === 'delivered') {
			await goto('/briefing', { replaceState: false, noScroll: true });
			return;
		}
		await goto('/feed', { replaceState: false, noScroll: true });
	}

	function handleTodayRowClick(item: TodayItem, event: MouseEvent): void {
		if (todayVisibilityPending(item)) return;
		if (isInteractiveDescendantEvent(event.target, event.currentTarget)) return;
		void openTodayItem(item);
	}

	function handleTodayRowKeydown(item: TodayItem, event: KeyboardEvent): void {
		if (event.key !== 'Enter' && event.key !== ' ') return;
		if (todayVisibilityPending(item)) return;
		if (isInteractiveDescendantEvent(event.target, event.currentTarget)) return;
		event.preventDefault();
		void openTodayItem(item);
	}

	function todayVisibilityKey(itemId: string, action: TodayVisibilityUiAction): string {
		return `${itemId}:${action}`;
	}

	function todayVisibilityPending(item: TodayItem, action?: TodayVisibilityUiAction): boolean {
		if (!todayVisibilityActionKey) return false;
		if (action) return todayVisibilityActionKey === todayVisibilityKey(item.id, action);
		return todayVisibilityActionKey.startsWith(`${item.id}:`);
	}

	function hiddenTodayItemPending(item: TodayVisibilityListItem): boolean {
		return todayVisibilityActionKey === todayVisibilityKey(item.item_id, 'restore');
	}

	function todayVisibilitySnapshot(item: TodayItem): TodayVisibilitySnapshot {
		return {
			title: item.title,
			summary: item.summary ?? null,
			reason: item.reason,
			section: item.section,
			source_kind: item.source_kind,
			source_id: item.source_id,
			source_url: item.source_url ?? null,
			space_ids: item.space_ids,
			item_updated_at: item.updated_at
		};
	}

	async function updateTodayVisibility(
		item: TodayItem,
		action: 'dismiss' | 'snooze',
		options: { snoozeMinutes?: number } = {}
	): Promise<void> {
		const key = todayVisibilityKey(item.id, action);
		todayVisibilityActionKey = key;
		try {
			// Optimistic path: the store removes the row immediately and runs
			// the POST in the background (failures restore the row + toast
			// from the store), so this resolves right away.
			await todayStore.updateVisibility(item.id, action, {
				...options,
				snapshot: todayVisibilitySnapshot(item)
			});
			invalidateTodaySectionCursorsAfter(item.section, todayTabPageById[item.section] ?? 1);
			showUndoToast(action === 'dismiss' ? 'Hidden' : 'Snoozed');
		} catch (error) {
			showError(
				`Failed to ${action} Today item: ${error instanceof Error ? error.message : String(error)}`
			);
		} finally {
			if (todayVisibilityActionKey === key) {
				todayVisibilityActionKey = null;
			}
		}
	}

	async function dismissTodayItem(item: TodayItem): Promise<void> {
		await updateTodayVisibility(item, 'dismiss');
	}

	async function toggleSnoozeMenu(item: TodayItem, event: MouseEvent): Promise<void> {
		event.stopPropagation();
		if (openSnoozeItemId === item.id) {
			closeSnoozeMenu(false);
			return;
		}
		openSnoozeItemId = item.id;
		snoozeMenuTriggerEl =
			event.currentTarget instanceof HTMLButtonElement ? event.currentTarget : null;
		snoozeTonightHint = new Date().getHours() >= 18 ? 'in 3 hours' : 'until 6 PM';
		await tick();
		menuFocusableItems(snoozeMenuEl)[0]?.focus();
	}

	function closeSnoozeMenu(refocusTrigger: boolean): void {
		openSnoozeItemId = null;
		if (refocusTrigger) snoozeMenuTriggerEl?.focus();
	}

	const handleSnoozeMenuKeydown = createMenuKeydown({
		getMenuEl: () => snoozeMenuEl,
		close: closeSnoozeMenu
	});

	async function snoozeTodayItemFor(item: TodayItem, option: SnoozeOption): Promise<void> {
		closeSnoozeMenu(false);
		await updateTodayVisibility(item, 'snooze', {
			snoozeMinutes: snoozeMinutesFor(option, new Date())
		});
	}

	function showUndoToast(message: string): void {
		undoToastMessage = message;
		if (undoToastTimeout) clearTimeout(undoToastTimeout);
		undoToastTimeout = setTimeout(() => {
			undoToastMessage = null;
			undoToastTimeout = null;
		}, 5_000);
	}

	function dismissUndoToast(): void {
		if (undoToastTimeout) {
			clearTimeout(undoToastTimeout);
			undoToastTimeout = null;
		}
		undoToastMessage = null;
	}

	async function undoLastHiddenTodayItem(): Promise<void> {
		dismissUndoToast();
		try {
			// The store awaits the in-flight hide POST before issuing the
			// restore, so undo-before-POST-finishes is ordered correctly.
			const undone = await todayStore.undoLastHidden();
			if (undone) {
				invalidateAllTodaySectionCursors();
				showSuccess('Today item restored.');
			}
		} catch (error) {
			showError(
				`Failed to restore Today item: ${error instanceof Error ? error.message : String(error)}`
			);
		}
	}

	async function restoreTodayItem(item: TodayVisibilityListItem): Promise<void> {
		const key = todayVisibilityKey(item.item_id, 'restore');
		todayVisibilityActionKey = key;
		try {
			await todayStore.updateVisibility(item.item_id, 'restore');
			const section = item.record.snapshot?.section;
			const restoredSection = section ?? null;
			if (isTodaySectionId(restoredSection)) {
				invalidateTodaySectionCursorsAfter(restoredSection, 1);
			} else {
				invalidateAllTodaySectionCursors();
			}
			showSuccess('Today item restored.');
		} catch (error) {
			showError(
				`Failed to restore Today item: ${error instanceof Error ? error.message : String(error)}`
			);
		} finally {
			if (todayVisibilityActionKey === key) {
				todayVisibilityActionKey = null;
			}
		}
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

	function hiddenTodayReason(item: TodayVisibilityListItem): string | null {
		const snapshot = item.record.snapshot;
		return snapshot?.summary?.trim() || snapshot?.reason?.trim() || null;
	}

	async function openTodayDigestBullet(bullet: TodayDigestBullet): Promise<void> {
		const sourceUrl = bullet.source_url?.trim();
		if (openAttentionRoute(sourceUrl)) {
			return;
		}
		if (sourceUrl?.startsWith('/')) {
			await goto(sourceUrl, { replaceState: false, noScroll: true });
			return;
		}
		await goto('/feed', { replaceState: false, noScroll: true });
	}

	function readMetadataString(item: FeedItem, key: string): string | null {
		const record =
			typeof item.metadata === 'object' && item.metadata && !Array.isArray(item.metadata)
				? (item.metadata as Record<string, unknown>)
				: null;
		const value = record?.[key];
		return typeof value === 'string' && value.trim().length > 0 ? value : null;
	}

	function readMetadataValue(item: FeedItem, key: string): unknown {
		const record =
			typeof item.metadata === 'object' && item.metadata && !Array.isArray(item.metadata)
				? (item.metadata as Record<string, unknown>)
				: null;
		return record?.[key];
	}

	function todayMetadataRecord(item: TodayItem): Record<string, unknown> | null {
		return typeof item.metadata === 'object' && item.metadata && !Array.isArray(item.metadata)
			? (item.metadata as Record<string, unknown>)
			: null;
	}

	function todayLearnedItems(item: TodayItem): TodayLearnedItem[] {
		const learnedItems = todayMetadataRecord(item)?.learned_items;
		if (!Array.isArray(learnedItems)) return [];
		return learnedItems
			.map((entry, index): TodayLearnedItem | null => {
				if (!entry || typeof entry !== 'object' || Array.isArray(entry)) return null;
				const record = entry as Record<string, unknown>;
				const title = typeof record.title === 'string' ? record.title.trim() : '';
				const summary = typeof record.summary === 'string' ? record.summary.trim() : '';
				if (!title && !summary) return null;
				return {
					id: typeof record.id === 'string' && record.id.trim() ? record.id : `${item.id}:${index}`,
					title: title || summary,
					summary: summary && summary !== title ? summary : null,
					updatedAt: typeof record.updated_at === 'number' ? record.updated_at : null
				};
			})
			.filter((entry): entry is TodayLearnedItem => entry !== null)
			.slice(0, 4);
	}

	function metadataValueLabel(value: unknown): string | null {
		if (typeof value === 'string') return value.trim() || null;
		if (typeof value === 'number' || typeof value === 'boolean') return String(value);
		if (value && typeof value === 'object') return JSON.stringify(value);
		return null;
	}

	function learningCandidateId(item: FeedItem): string | null {
		return readMetadataString(item, 'candidate_id')
			|| (item.id.startsWith('learning_candidate:') ? item.id.slice('learning_candidate:'.length) : null);
	}

	function learningTargetLabel(item: FeedItem): string {
		const scope = readMetadataString(item, 'target_scope') || 'user';
		const tier = readMetadataString(item, 'target_tier') || 'memory';
		return `${titleCase(scope)} ${titleCase(tier)}`;
	}

	function learningValueLabel(item: FeedItem): string {
		return sharedLearningValueLabel(item);
	}

	function learningInsightId(item: FeedItem): string {
		return readMetadataString(item, 'insight_id') || item.id;
	}

	async function refreshScrolls(): Promise<void> {
		const requestId = ++scrollRequestId;
		const requestScopeKey = todayScopeKey();
		scrollsLoading = true;
		scrollsError = null;
		try {
			const page = await loadPublishedSurfacePage({
				route_target: TODAY_BRIEFING_ROUTE,
				maxItems: TODAY_BRIEFING_LIMIT
			});
			if (requestId !== scrollRequestId || requestScopeKey !== todayScopeKey()) return;
			scrollRecords = page.records;
		} catch (error) {
			if (requestId !== scrollRequestId || requestScopeKey !== todayScopeKey()) return;
			scrollsError = error instanceof Error ? error.message : 'Failed to load briefings';
		} finally {
			if (requestId === scrollRequestId && requestScopeKey === todayScopeKey()) {
				scrollsLoading = false;
			}
		}
	}

	async function clearTodaySelectionFromUrl(): Promise<void> {
		if (!browser) return;
		const params = new URLSearchParams($page.url.searchParams);
		const hadSelected = params.has('selected');
		const hadSelectedItem = params.has('selected_item');
		if (!hadSelected && !hadSelectedItem) return;
		params.delete('selected');
		params.delete('selected_item');
		const nextUrl = params.toString() ? `${$page.url.pathname}?${params.toString()}` : $page.url.pathname;
		await goto(nextUrl, { replaceState: true, noScroll: true });
	}

	function clearTodayScopeState(): void {
		channelFollowUpRequest?.controller.abort();
		channelFollowUpRequest = null;
		scrollRequestId += 1;
		resurfacingCountRequestId += 1;
		channelFollowUpRequestId += 1;
		channelFollowUpCursorGeneration += 1;
		todayScopeGeneration += 1;
		resurfacingTotal = 0;
		resurfacingCountError = null;
		channelFollowUps = [];
		channelFollowUpTotal = 0;
		channelFollowUpHealth = null;
		channelFollowUpActionability = null;
		channelFollowUpActionabilityTraining = null;
		channelFollowUpGrouping = null;
		channelFollowUpRouting = null;
		channelFollowUpBandit = null;
		channelFollowUpSemanticExtraction = null;
		channelFollowUpSemanticRankingEnabled = false;
		channelFollowUpPage = 1;
		channelFollowUpLoadedPage = 0;
		channelFollowUpLoading = false;
		channelFollowUpLoadError = null;
		channelFollowUpCursorsByPage = initialCursorPages();
		appliedChannelFollowUpPageKey = '';
		appliedTodayStoreQueryKey = '';
		appliedTodaySectionCursorKey = '';
		primingTodaySectionKey = null;
		todaySectionCursorsById = initialTodaySectionCursors();
		changedDigestPage = 1;
		appliedHighlightedItemId = routeSelectedItemId;
		highlightedActivityItemId = null;
		if (activityHighlightTimeout) {
			clearTimeout(activityHighlightTimeout);
			activityHighlightTimeout = null;
		}
		scrollRecords = [];
		scrollsLoading = false;
		scrollsError = null;
	}

	async function openBriefingCanvas(): Promise<void> {
		await goto('/briefing', { replaceState: false, noScroll: false });
	}

	async function openPublishedSurface(surfaceId: string): Promise<void> {
		await goto(`/briefing/${encodeURIComponent(surfaceId)}`, {
			replaceState: false,
			noScroll: false
		});
	}

	function taskRoute(taskId: string, lifecycle?: Task['lifecycle'] | null): string {
		if (lifecycle === 'internal') {
			return internalTaskRoute(taskId);
		}
		const params = new URLSearchParams({ filter: 'all', selected: taskId });
		return `/tasks?${params.toString()}`;
	}

	function isTaskSelectionRoute(sourceUrl: string): boolean {
		return sourceUrl.startsWith('/tasks');
	}

	async function resolveTaskRoute(taskId: string): Promise<string> {
		const localTask = get(taskStore).tasks.find((candidate) => candidate.id === taskId) ?? null;
		if (localTask) return taskRoute(taskId, localTask.lifecycle);
		const resolvedTask = await taskStore.fetchTaskRecordById(taskId);
		return taskRoute(taskId, resolvedTask?.lifecycle ?? null);
	}

	async function openTaskRoute(taskId: string): Promise<void> {
		const requestScopeKey = todayScopeKey();
		const route = await resolveTaskRoute(taskId);
		if (requestScopeKey !== todayScopeKey()) return;
		await goto(route, { replaceState: false, noScroll: true });
	}

	async function ensurePersonalAgentsLoaded(): Promise<void> {
		if (get(personalAgentList).length > 0) return;
		try {
			await loadAgents({ replace: true, clearError: true });
		} catch {
			// The Today panels handle the missing-agent case inline.
		}
	}

	async function focusActivityItem(itemId: string): Promise<void> {
		await tick();
		const node = activityCardRefs.get(itemId);
		if (!node) return;
		node.scrollIntoView({ block: 'center', behavior: 'smooth' });
		highlightedActivityItemId = itemId;
		if (activityHighlightTimeout) clearTimeout(activityHighlightTimeout);
		activityHighlightTimeout = setTimeout(() => {
			highlightedActivityItemId = null;
			activityHighlightTimeout = null;
		}, 2200);
	}

	function registerActivityCard(node: HTMLElement, itemId: string) {
		activityCardRefs.set(itemId, node);
		return {
			destroy() {
				activityCardRefs.delete(itemId);
			}
		};
	}

	async function openActivityItem(item: FeedItem): Promise<void> {
		const hitlTarget = hitlOpenTargetFromFeedItem(item);
		if (hitlTarget) {
			const result = await openHitlPrompt(hitlTarget);
			if (result.status === 'error') showError(result.error);
			return;
		}
		if (item.item_type === 'agent_learning') {
			// Route to the source provenance if the projector
			// captured one (research findings carry canonical sources
			// or the legacy source_urls
			// pointing at the execution output that produced the
			// fact). Else open /memory where the underlying tier is
			// browsable. See feed/agent_learnings_projection.rs for
			// the metadata.raw_entry shape per tier.
			const rawEntry = readMetadataValue(item, 'raw_entry');
			const sourceUrl = agentLearningSourceUrl(rawEntry);
			if (sourceUrl && sourceUrl.startsWith('/')) {
				await goto(sourceUrl, { replaceState: false, noScroll: true });
				return;
			}
			await goto('/memory', { replaceState: false, noScroll: true });
			return;
		}
		if (item.item_type === 'learning_candidate') {
			await goto('/memory', { replaceState: false, noScroll: true });
			return;
		}
		if (item.item_type === 'learning_insight') {
			const sourceTaskId = item.task_id || readMetadataString(item, 'source_task_id');
			if (sourceTaskId) {
				await openTaskRoute(sourceTaskId);
				return;
			}
			const sourceThreadId = item.ui_thread_id || readMetadataString(item, 'source_chat_session_id');
			if (sourceThreadId) {
				const params = new URLSearchParams({ selected_item: item.id });
				await goto(`/t/${encodeURIComponent(sourceThreadId)}?${params.toString()}`, {
					replaceState: false,
					noScroll: true
				});
				return;
			}
			await goto(`/feed?selected_item=${encodeURIComponent(item.id)}`, {
				replaceState: false,
				noScroll: true
			});
			return;
		}
		if (item.item_type === 'data_delivery') {
			const surfaceId = readMetadataString(item, 'surface_id');
			if (surfaceId) {
				await goto(`/briefing/${encodeURIComponent(surfaceId)}`, {
					replaceState: false,
					noScroll: true
				});
				return;
			}
			const route = readMetadataString(item, 'route');
			await goto(route?.startsWith('/') ? route : '/briefing', {
				replaceState: false,
				noScroll: true
			});
			return;
		}
		if (item.item_type === 'routine_result') {
			const surfaceId = readMetadataString(item, 'surface_id');
			if (surfaceId) {
				await goto(`/briefing/${encodeURIComponent(surfaceId)}`, {
					replaceState: false,
					noScroll: true
				});
				return;
			}
			const route = readMetadataString(item, 'route');
			if (route?.startsWith('/')) {
				await goto(route, { replaceState: false, noScroll: true });
				return;
			}
		}
		if (item.task_id) {
			await openTaskRoute(item.task_id);
			return;
		}
		if (item.item_type === 'agent_message' && item.ui_thread_id) {
			const params = new URLSearchParams({ selected_item: item.id });
			await goto(`/t/${encodeURIComponent(item.ui_thread_id)}?${params.toString()}`, {
				replaceState: false,
				noScroll: true
			});
			return;
		}
		if (item.item_type === 'routine_result') {
			await goto('/briefing', { replaceState: false, noScroll: true });
			return;
		}
		if (item.item_type === 'approval') {
			openAttentionCenter();
			return;
		}
		await goto('/tasks', { replaceState: false, noScroll: true });
	}

	async function removeActivityItem(item: FeedItem, reason: string): Promise<void> {
		if (item.item_type === 'learning_candidate') {
			const candidateId = learningCandidateId(item);
			if (!candidateId) throw new Error('This learning card is missing its candidate id.');
			await activityFeed.archiveLearningCandidate(candidateId, reason);
			return;
		}
		if (item.item_type === 'learning_insight') {
			const insightId = learningInsightId(item);
			await activityFeed.archiveLearningInsight(insightId, reason);
			return;
		}
		await activityFeed.deleteItem(item.id);
	}

	async function handleActivityItemDelete(item: FeedItem): Promise<void> {
		try {
			await removeActivityItem(item, 'Removed from Activity.');
		} catch (error) {
			console.error('[today] activity delete failed', error);
			showError(`Failed to remove activity item: ${error instanceof Error ? error.message : String(error)}`);
		}
	}

	async function handleActivityClearAll(): Promise<void> {
		const items = activityFeedItems;
		const total = items.length;
		if (total === 0) return;
		const confirmed = await requestConfirmation({
			title: `Remove all ${total} ${total === 1 ? 'item' : 'items'} from Activity?`,
			message: 'This removes only the durable Activity items. Attention requests stay untouched.',
			confirmLabel: 'Clear activity',
			destructive: true
		});
		if (!confirmed) return;
		try {
			await Promise.all(items.map((item) => removeActivityItem(item, 'Cleared from Activity.')));
		} catch (error) {
			console.error('[today] activity clear failed', error);
			showError(`Failed to clear activity: ${error instanceof Error ? error.message : String(error)}`);
		}
	}

	function openLearningEditor(item: FeedItem): void {
		editingLearningItem = item;
		learningEditValue = learningValueLabel(item);
	}

	function closeLearningEditor(): void {
		if (learningActionKey) return;
		editingLearningItem = null;
		learningEditValue = '';
	}

	async function handleLearningConfirm(item: FeedItem): Promise<void> {
		const candidateId = learningCandidateId(item);
		if (!candidateId) {
			showError('This learning card is missing its candidate id.');
			return;
		}
		const actionKey = `${candidateId}:confirm`;
		learningActionKey = actionKey;
		try {
			await activityFeed.confirmLearningCandidate(candidateId);
			showSuccess('Learning filed to memory.');
		} catch (error) {
			console.error('[today] learning confirm failed', error);
			showError(`Failed to file learning: ${error instanceof Error ? error.message : String(error)}`);
		} finally {
			if (learningActionKey === actionKey) learningActionKey = null;
		}
	}

	async function handleLearningArchive(item: FeedItem): Promise<void> {
		const candidateId = learningCandidateId(item);
		if (!candidateId) {
			showError('This learning card is missing its candidate id.');
			return;
		}
		const actionKey = `${candidateId}:archive`;
		learningActionKey = actionKey;
		try {
			await activityFeed.archiveLearningCandidate(candidateId, 'Archived from Activity.');
			showInfo('Learning archived.');
		} catch (error) {
			console.error('[today] learning archive failed', error);
			showError(`Failed to archive learning: ${error instanceof Error ? error.message : String(error)}`);
		} finally {
			if (learningActionKey === actionKey) learningActionKey = null;
		}
	}

	async function handleLearningEditConfirm(): Promise<void> {
		if (!editingLearningItem) return;
		const candidateId = learningCandidateId(editingLearningItem);
		const revisedValue = learningEditValue.trim();
		if (!candidateId) {
			showError('This learning card is missing its candidate id.');
			return;
		}
		if (!revisedValue) {
			showError('Learning text cannot be empty.');
			return;
		}
		const actionKey = `${candidateId}:edit_confirm`;
		learningActionKey = actionKey;
		try {
			await activityFeed.editConfirmLearningCandidate(candidateId, revisedValue);
			showSuccess('Edited learning filed to memory.');
			editingLearningItem = null;
			learningEditValue = '';
		} catch (error) {
			console.error('[today] learning edit-confirm failed', error);
			showError(`Failed to file edited learning: ${error instanceof Error ? error.message : String(error)}`);
		} finally {
			if (learningActionKey === actionKey) learningActionKey = null;
		}
	}

	async function handleInsightArchive(item: FeedItem): Promise<void> {
		const insightId = learningInsightId(item);
		const actionKey = `${insightId}:archive`;
		learningActionKey = actionKey;
		try {
			await activityFeed.archiveLearningInsight(insightId, 'Archived from Activity.');
			showInfo('Insight archived.');
		} catch (error) {
			console.error('[today] insight archive failed', error);
			showError(`Failed to archive insight: ${error instanceof Error ? error.message : String(error)}`);
		} finally {
			if (learningActionKey === actionKey) learningActionKey = null;
		}
	}

	async function handleInsightSaveToMemory(item: FeedItem): Promise<void> {
		const insightId = learningInsightId(item);
		const actionKey = `${insightId}:save`;
		learningActionKey = actionKey;
		try {
			await activityFeed.saveLearningInsightToMemory(insightId, item.summary || item.title);
			showSuccess('Insight queued for memory review.');
		} catch (error) {
			console.error('[today] insight save-to-memory failed', error);
			showError(`Failed to save insight: ${error instanceof Error ? error.message : String(error)}`);
		} finally {
			if (learningActionKey === actionKey) learningActionKey = null;
		}
	}

	async function handleInsightCreateTask(item: FeedItem): Promise<void> {
		const insightId = learningInsightId(item);
		const actionKey = `${insightId}:task`;
		learningActionKey = actionKey;
		try {
			await activityFeed.createLearningInsightFollowUp(insightId);
			showSuccess('Follow-up task created.');
		} catch (error) {
			console.error('[today] insight follow-up failed', error);
			showError(`Failed to create follow-up task: ${error instanceof Error ? error.message : String(error)}`);
		} finally {
			if (learningActionKey === actionKey) learningActionKey = null;
		}
	}

	function scrollMeta(record: PublishedSurfaceRecord): string {
		const bits: string[] = [];
		if (record.source_agent_id || record.metadata.producer.producer_agent_id) {
			bits.push(record.source_agent_id ?? record.metadata.producer.producer_agent_id);
		}
		if (record.metadata.ownership.task_id) {
			bits.push(record.metadata.ownership.task_id);
		}
		bits.push(formatRelative(Date.parse(record.manifest.published_at)));
		return bits.join(' · ');
	}

	function extractStructuredSummary(value: string): string | null {
		const summaryMatch = value.match(/(?:^|\n)\s*\*{0,2}Summary:\*{0,2}\s*([^\n]+)/i);
		if (!summaryMatch?.[1]) return null;
		const preview = stripMarkdownPreview(summaryMatch[1]);
		return preview.length > 0 ? preview : null;
	}

	function scrollPreview(record: PublishedSurfaceRecord): string {
		const structuredSummaryCandidates = [
			record.render?.source_output_summary,
			record.source_output_summary,
			record.render?.text_content
		];
		for (const candidate of structuredSummaryCandidates) {
			if (typeof candidate !== 'string' || candidate.trim().length === 0) continue;
			const summary = extractStructuredSummary(candidate);
			if (summary) {
				return summary;
			}
		}

		const candidates = [
			record.manifest.summary,
			record.render?.surface.summary,
			record.render?.source_output_summary,
			record.source_output_summary,
			record.render?.text_content
		];
		for (const candidate of candidates) {
			if (typeof candidate !== 'string' || candidate.trim().length === 0) continue;
			const preview = stripMarkdownPreview(candidate);
			if (preview.length > 0) {
				return preview;
			}
		}
		return 'Open this briefing for the full view.';
	}

	// No dashboard-theme override on this page. The previous version did
	// `selectedThemeId.set('editorial')` + `applyThemeToCssVariables(...)`
	// in onMount, which writes inline styles on `document.documentElement`
	// for every `--theme-color-*` variable. Those inline styles WIN over
	// the app-level `[data-theme="..."]` selectors in app.css, so the
	// user's chosen app theme was being steamrolled by the editorial
	// dashboard theme — Today rendered with a permanently warm/cream
	// surface regardless of which theme the user picked. Same
	// anti-pattern the `/llm` and `/briefing/[id]` comments warn about.
	onMount(() => {
		if (!browser) return;
		// Keep the dashboard-theme registry warmed up so ScrollCardPreview
		// tiles and any other theme-aware children that look up the
		// registry have it available — but do NOT install inline
		// overrides on documentElement.
		void ensureRegistryLoaded();

		todayMounted = true;
		lastTodayScopeKey = todayScopeKey();
		void canonicalAttentionProjectionStore.refresh(lastTodayScopeKey);
		void loadWorthCardLookup();
		applyTodayRouteSelection();
		syncTodayStoreQuery();
		todayStore.start();
		activityFeed.start();
		taskStore.start();
		threadStore.start();
		void ensurePersonalAgentsLoaded();
		syncChannelFollowUpsPage();
		void loadResurfacingCount();
		const windowWithIdle = window as typeof window & {
			requestIdleCallback?: (callback: () => void, options?: { timeout: number }) => number;
		};
		const loadSemanticDiagnostics = () => {
			if (todayMounted) {
				void attentionSemanticExtractionStore.refresh(currentTodayScopeKey);
			}
		};
		if (windowWithIdle.requestIdleCallback) {
			windowWithIdle.requestIdleCallback(loadSemanticDiagnostics, { timeout: 2_000 });
		} else {
			setTimeout(loadSemanticDiagnostics, 0);
		}
		channelFollowUpTimer = setInterval(() => {
			void loadTodayChannelFollowUps();
			void canonicalAttentionProjectionStore.refresh(currentTodayScopeKey);
			void attentionSemanticExtractionStore.refresh(currentTodayScopeKey);
		}, 20000);
		void refreshScrolls();
		scrollRefreshUnsubscribe = subscribeToPublishedSurfaceRefresh(
			{ route_target: TODAY_BRIEFING_ROUTE },
			() => {
				void refreshScrolls();
			}
		);

		return () => {
			todayMounted = false;
			todayStore.stop();
			activityFeed.stop();
			taskStore.stop();
			threadStore.stop();
			if (channelFollowUpTimer) clearInterval(channelFollowUpTimer);
			channelFollowUpRequest?.controller.abort();
			channelFollowUpRequest = null;
			attentionSemanticExtractionStore.cancel(currentTodayScopeKey);
			scrollRefreshUnsubscribe?.();
			scrollRefreshUnsubscribe = null;
		};
	});

	$: currentTodayScopeKey = `${$scopeIdentityStore.principal}:${$scopeIdentityStore.workspace}`;
	$: resurfacingChatThreadId = canonicalResurfacingChatThread($threadStore.threads);

	$: if (browser && todayMounted && currentTodayScopeKey !== lastTodayScopeKey) {
		lastTodayScopeKey = currentTodayScopeKey;
		// New scope repaints the whole board — treat it like an initial
		// load so the incoming rows don't all settle-in at once.
		todayRowsIntroReady = false;
		clearTodayScopeState();
		void clearTodaySelectionFromUrl();
		void loadTodayChannelFollowUps(1);
		void loadResurfacingCount();
		void canonicalAttentionProjectionStore.refresh(currentTodayScopeKey);
		void loadWorthCardLookup();
		void attentionSemanticExtractionStore.refresh(currentTodayScopeKey);
		void refreshScrolls();
	}

	$: if (browser && routeSelectedItemId) {
		if (routeSelectedItemId !== appliedHighlightedItemId) {
			appliedHighlightedItemId = routeSelectedItemId;
			activityExpanded = true;
			activeFilter = 'all';
			searchQuery = '';
			void focusActivityItem(routeSelectedItemId);
		}
	}

	$: if (!routeSelectedItemId && appliedHighlightedItemId) {
		appliedHighlightedItemId = null;
	}

	onDestroy(() => {
		scrollRefreshUnsubscribe?.();
		scrollRefreshUnsubscribe = null;
		if (activityHighlightTimeout) clearTimeout(activityHighlightTimeout);
		if (undoToastTimeout) clearTimeout(undoToastTimeout);
	});
</script>

{#snippet todayItemRow(item: TodayItem, emphasized: boolean)}
	<!-- Whole row = primary action (same grammar as the Tasks cards / shared
	     Card): click / Enter / Space on the row body opens the item, while
	     nested controls (Review, snooze, dismiss, menu items) keep their own
	     behavior via the interactive-descendant check. -->
	<!-- svelte-ignore a11y_no_noninteractive_element_to_interactive_role -->
	<article
		class={todayRowClass(item.section, emphasized)}
		class:today-row--snooze-open={openSnoozeItemId === item.id}
		role="button"
		tabindex="0"
		aria-label={`Open ${item.title}`}
		on:click={(event) => handleTodayRowClick(item, event)}
		on:keydown={(event) => handleTodayRowKeydown(item, event)}
	>
		<div class="today-row__marker" aria-hidden="true">
			<Icon name={todayItemIcon(item)} size={14} />
			{#if item.section === 'active_work'}
				<span class="today-row__live-dot live-pulse-dot"></span>
			{/if}
		</div>
		<div class="today-row__content">
			<div class="today-row__topline">
				<strong>{item.title}</strong>
				<span>{formatRelative(item.updated_at)}</span>
			</div>
			<p>{item.reason}</p>
			{#if item.summary}
				<p class="today-row__summary">{item.summary}</p>
			{/if}
			{#if todayLearnedItems(item).length > 0}
				<ul class="today-row__learned" aria-label="Learned memory details">
					{#each todayLearnedItems(item) as learned (learned.id)}
						<li>
							<span>{learned.title}</span>
							{#if learned.summary}
								<small>{learned.summary}</small>
							{/if}
						</li>
					{/each}
				</ul>
			{/if}
			<div class="today-row__meta">
				<Badge text={todaySourceLabel(item)} color="default" />
				<Badge text={titleCase(item.status)} color={statusColor(item.status)} />
				{#if item.agent_id}
					<Badge text={item.agent_id} color="info" />
				{/if}
				{#each item.space_ids.slice(0, 2) as spaceId (spaceId)}
					<Badge text={formatSpaceLabel(spaceId)} color="default" />
				{/each}
			</div>
		</div>
		<div class="today-row__actions" aria-label={`Actions for ${item.title}`}>
			{#if showTodayPrimaryAction(item.section)}
				<Button
					label={todayPrimaryActionLabel(item)}
					variant={emphasized ? 'primary' : 'outline'}
					size="sm"
					disabled={todayVisibilityPending(item)}
					stopPropagation
					on:click={() => void openTodayItem(item)}
				/>
			{/if}
			<div class="today-snooze">
				<button
					type="button"
					class="today-snooze__trigger ui-no-press"
					aria-label="Snooze Today item"
					title="Snooze"
					aria-haspopup="menu"
					aria-expanded={openSnoozeItemId === item.id}
					disabled={todayVisibilityPending(item)}
					on:click|stopPropagation={(event) => void toggleSnoozeMenu(item, event)}
				>
					<Icon name="moon" size={14} />
				</button>
				{#if openSnoozeItemId === item.id}
					<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
					<div
						bind:this={snoozeMenuEl}
						class="today-snooze__menu"
						role="menu"
						aria-orientation="vertical"
						aria-label="Snooze options"
						tabindex="-1"
						use:clickOutside={{ handler: () => closeSnoozeMenu(false), exclude: [snoozeMenuTriggerEl] }}
						on:keydown={handleSnoozeMenuKeydown}
					>
						<button type="button" role="menuitem" on:click={() => void snoozeTodayItemFor(item, 'tonight')}>
							Tonight <small>{snoozeTonightHint}</small>
						</button>
						<button type="button" role="menuitem" on:click={() => void snoozeTodayItemFor(item, 'tomorrow_morning')}>
							Tomorrow morning <small>8 AM</small>
						</button>
						<button type="button" role="menuitem" on:click={() => void snoozeTodayItemFor(item, 'next_week')}>
							Next week <small>Mon 8 AM</small>
						</button>
					</div>
				{/if}
			</div>
			<button
				type="button"
				class="today-snooze__trigger ui-no-press"
				aria-label="Dismiss Today item"
				title="Dismiss"
				disabled={todayVisibilityPending(item)}
				on:click|stopPropagation={() => void dismissTodayItem(item)}
			>
				<svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
					<path d="M18 6 6 18" />
					<path d="m6 6 12 12" />
				</svg>
			</button>
		</div>
	</article>
{/snippet}

{#snippet todayItemRows(items: TodayItem[], emphasized: boolean)}
	<!-- The motion wrapper (not the article in the snippet) carries
	     animate:flip because `animate:` must sit on the immediate child of
	     the keyed each. Intros stay LOCAL on purpose: rows animate when
	     they enter their own list; a whole section appearing/disappearing
	     (outer each) intentionally doesn't stagger row intros/outros.
	     out: slides right — toward "away" — matching dismiss/snooze. -->
	<div class="today-row-list">
		{#each items as item (item.id)}
			<div
				class="today-row-motion"
				animate:flip={{ duration: flipDurationMs() }}
				in:settleIn={todayRowsIntroReady ? {} : { duration: 0 }}
				out:settleOut={{ x: 24 }}
			>
				{@render todayItemRow(item, emphasized)}
			</div>
		{/each}
	</div>
{/snippet}

{#snippet todaySectionRowsSkeleton()}
	<div class="today-row-skeleton-list" aria-label="Loading Today section">
		{#each Array(4) as _, index}
			<div class="today-row-skeleton" aria-hidden="true">
				<Skeleton variant="circle" size="1.8rem" />
				<div class="today-row-skeleton__content">
					<Skeleton width={index % 2 === 0 ? '52%' : '68%'} height="0.85rem" />
					<Skeleton width="100%" height="0.72rem" />
					<Skeleton width={index % 2 === 0 ? '72%' : '58%'} height="0.72rem" />
					<div class="today-row-skeleton__badges">
						<Skeleton variant="pill" width="4.4rem" height="1.25rem" />
						<Skeleton variant="pill" width="3.8rem" height="1.25rem" />
						<Skeleton variant="pill" width="5.2rem" height="1.25rem" />
					</div>
				</div>
			</div>
		{/each}
	</div>
{/snippet}

{#snippet todaySectionPanelFallback(sectionId: TodaySectionId)}
	{@const meta = { id: sectionId, ...TODAY_SECTION_META_BY_ID[sectionId] }}
	<section class={todaySectionClass(sectionId)}>
		<div class="today-section__header">
			<div>
				<h2>{meta.title}</h2>
				<p>{meta.summary}</p>
			</div>
			<Skeleton variant="pill" width="2rem" height="1.35rem" />
		</div>
		{@render todaySectionRowsSkeleton()}
	</section>
{/snippet}

{#snippet todaySectionPanel(section: TodaySectionTabView)}
	{@const sectionTotal = todaySectionPaginationTotal(section)}
	{@const sectionPageCount = todaySectionPageCount(section)}
	{@const sectionLoaded = todaySectionPageLoaded(section.meta.id, activeTodayPage, $todayStore.loadedSectionPageKeys)}
	<section
		class={todaySectionClass(section.meta.id)}
		class:today-section-card={section.meta.id === 'delivered'
			|| section.meta.id === 'changed'
			|| section.meta.id === 'active_work'}
	>
		<div class="today-section__header">
			<div>
				<h2>{section.meta.title}</h2>
				<p>{section.meta.summary}</p>
			</div>
			<Badge text={String(section.count)} color={section.meta.tone} />
		</div>

		{#if sectionLoaded && sectionTotal > TODAY_SECTION_PAGE_SIZE}
			<ServerPager
				currentPage={activeTodayPage}
				pageCount={sectionPageCount}
				startItem={todaySectionPageStart(section)}
				endItem={todaySectionPageEnd(section)}
				totalItems={sectionTotal}
				ariaLabel={`${section.meta.title} pagination`}
				on:pagechange={(event) => goToTodayTabPage(section.meta.id, event.detail.page)}
			/>
		{/if}

		{#if !sectionLoaded}
			{@render todaySectionRowsSkeleton()}
		{:else if section.items.length === 0}
			<div class="today-tab-empty">
				<strong>{todayTabEmptyTitle(section.meta.id)}</strong>
				<p>{todayTabEmptyBody(section.meta.id)}</p>
			</div>
		{:else if section.meta.groupBySpace}
			<div class="today-space-groups">
				{#each todaySpaceGroups(section.items) as group (group.id)}
					<div class="today-space-group" class:today-space-group--unfiled={group.unfiled}>
						<div class="today-space-group__header">
							<strong>{group.label}</strong>
							<span>{group.items.length}</span>
						</div>
						{@render todayItemRows(group.items, section.meta.emphasized)}
					</div>
				{/each}
			</div>
		{:else}
			{@render todayItemRows(section.items, section.meta.emphasized)}
		{/if}

		{#if sectionLoaded && sectionTotal > TODAY_SECTION_PAGE_SIZE}
			<ServerPager
				currentPage={activeTodayPage}
				pageCount={sectionPageCount}
				startItem={todaySectionPageStart(section)}
				endItem={todaySectionPageEnd(section)}
				totalItems={sectionTotal}
				ariaLabel={`${section.meta.title} pagination`}
				on:pagechange={(event) => goToTodayTabPage(section.meta.id, event.detail.page)}
			/>
		{/if}
	</section>
{/snippet}

{#snippet todayChangedDigestPanel()}
	{@const digestLoaded = todayDigestPageLoaded(changedDigestPage, $todayStore.loadedDigestPageKey)}
	{@const digestPageCount = todayDigestPageCount()}
	<section class="today-digest" aria-label="What changed">
		<div class="today-digest__header">
			<div>
				<h2>What Changed</h2>
				<span>
					{#if $todayStore.digest.since}
						Since {formatRelative($todayStore.digest.since)}
					{:else}
						Generated {formatRelative($todayStore.digest.generated_at)}
					{/if}
				</span>
			</div>
			<Button
				label={$todayStore.isLoading ? 'Refreshing...' : 'Refresh digest'}
				variant="outline"
				size="sm"
				on:click={() => void todayStore.refreshDigest()}
			/>
		</div>
		{#if digestLoaded && $todayStore.digest.total > TODAY_DIGEST_PAGE_SIZE}
			<ServerPager
				currentPage={changedDigestPage}
				pageCount={digestPageCount}
				startItem={todayDigestPageStart()}
				endItem={todayDigestPageEnd()}
				totalItems={$todayStore.digest.total}
				ariaLabel="What Changed pagination"
				on:pagechange={(event) => goToTodayDigestPage(event.detail.page)}
			/>
		{/if}
		{#if !digestLoaded}
			{@render todaySectionRowsSkeleton()}
		{:else if $todayStore.digest.bullets.length === 0}
			<div class="today-tab-empty">
				<strong>No digest items on this page.</strong>
				<p>Durable changes will appear here as they are generated.</p>
			</div>
		{:else}
			<ul class="today-digest__list">
				{#each $todayStore.digest.bullets as bullet (bullet.id)}
					<li>
						<button
							type="button"
							class="today-digest__bullet"
							on:click={() => void openTodayDigestBullet(bullet)}
						>
							<span>{bullet.text}</span>
							<span class="today-digest__meta">
								<Badge text={todaySourceKindLabel(bullet.source_kind)} color="default" />
								{#if bullet.space_ids.length > 0}
									<Badge text={formatSpaceLabel(bullet.space_ids[0])} color="default" />
								{/if}
							</span>
						</button>
					</li>
				{/each}
			</ul>
		{/if}
		{#if digestLoaded && $todayStore.digest.total > TODAY_DIGEST_PAGE_SIZE}
			<ServerPager
				currentPage={changedDigestPage}
				pageCount={digestPageCount}
				startItem={todayDigestPageStart()}
				endItem={todayDigestPageEnd()}
				totalItems={$todayStore.digest.total}
				ariaLabel="What Changed pagination"
				on:pagechange={(event) => goToTodayDigestPage(event.detail.page)}
			/>
		{/if}
	</section>
{/snippet}

{#snippet todayChannelFollowUpsPanel()}
	{@const channelPageLoaded = channelFollowUpLoadedPage === channelFollowUpPage}
	{@const channelPageCount = channelFollowUpPageCount()}
	<section class="today-section today-section--followups today-channel-followup-section">
		<div class="today-section__header">
			<div>
				<h2>Message follow-ups</h2>
				<p>Emails &amp; messages with a reply, waiting-on, promise, or check-back signal.</p>
			</div>
			<Badge text={`${effectiveChannelFollowUpTotal}`} color="warning" />
		</div>
		<AttentionLearningHealthStrip
			health={effectiveChannelFollowUpHealth}
			actionability={effectiveChannelFollowUpActionability}
			actionabilityTraining={channelFollowUpActionabilityTraining}
			grouping={effectiveChannelFollowUpGrouping}
			routing={effectiveChannelFollowUpRouting}
			bandit={effectiveChannelFollowUpBandit}
			semanticExtraction={effectiveChannelFollowUpSemanticExtraction}
			surfaceLabel="Follow-up"
			semanticRankingEnabled={effectiveChannelFollowUpSemanticRankingEnabled}
		/>
		{#if channelPageLoaded && channelFollowUpTotal > CHANNEL_FOLLOW_UP_PAGE_SIZE}
			<ServerPager
				currentPage={channelFollowUpPage}
				pageCount={channelPageCount}
				startItem={channelFollowUpPageStart()}
				endItem={channelFollowUpPageEnd()}
				totalItems={channelFollowUpTotal}
				ariaLabel="Message follow-ups pagination"
				on:pagechange={(event) => goToChannelFollowUpPage(event.detail.page)}
			/>
		{/if}
		<div class="today-channel-followup-list">
			{#if channelFollowUpLoadError}
				<div class="today-channel-followup-row today-channel-followup-row--notice">
					<div class="today-channel-followup-main">
						<div class="today-channel-followup-subject">{channelFollowUpLoadError}</div>
					</div>
				</div>
			{/if}
			{#if !channelPageLoaded && !channelFollowUpLoadError}
				{@render todaySectionRowsSkeleton()}
			{:else if visibleLegacyChannelFollowUps.length === 0 && !channelFollowUpLoadError}
				<div class="today-tab-empty">
					<strong>No message follow-ups on this page.</strong>
					<p>Emails and messages with actionable signals will appear here.</p>
				</div>
			{:else}
				{#each visibleLegacyChannelFollowUps as f (f.annotation_id)}
					{@const actionSummary = channelActionSummary(f)}
					<div
						class="today-channel-followup-row"
						use:verifiedAttentionVisibility={{
							decision_item: f.decision_item ?? null,
							impression_policy: f.routing_page?.impression_policy ?? null,
							surface: 'follow_up'
						}}
					>
						<div class="today-channel-followup-main">
							<div class="today-channel-followup-line">
								<span class="today-channel-followup-label">{channelLabelText(f.label)}</span>
								<span class="today-channel-followup-provider"
									>{channelProviderText(f.provider)} · {channelLaneText(f.lane)}</span
								>
								{#if f.received_at}
									<span class="today-channel-followup-time">{channelReceivedLabel(f.received_at)}</span>
								{/if}
							</div>
							<div class="today-channel-followup-subject">{f.subject || '(no subject)'}</div>
							{#if f.sender}<div class="today-channel-followup-sender">{f.sender}</div>{/if}
							{#if actionSummary}<div class="today-channel-followup-reason">{actionSummary}</div>{/if}
							{#if f.summary}<div class="today-channel-followup-summary">{f.summary}</div>{/if}
							{#if f.reason}<div class="today-channel-followup-reason">{f.reason}</div>{/if}
							<AttentionRoutingDiagnostic
								item={f.decision_item ?? null}
								page={f.routing_page ?? null}
							/>
							<AttentionBanditDiagnostic decision={f.bandit_decision ?? null} />
						</div>
						<ChannelFollowUpActions
							followUp={f}
							on:resolved={onTodayChannelFollowUpResolved}
							on:failed={onTodayChannelFollowUpFailed}
						/>
						<ChannelFollowUpGroupPanel
							followUp={f}
							on:resolved={onTodayChannelFollowUpResolved}
							on:failed={onTodayChannelFollowUpFailed}
							on:groupchanged={onTodayChannelFollowUpGroupChanged}
						/>
					</div>
				{/each}
			{/if}
		</div>
		{#if channelPageLoaded && channelFollowUpTotal > CHANNEL_FOLLOW_UP_PAGE_SIZE}
			<ServerPager
				currentPage={channelFollowUpPage}
				pageCount={channelPageCount}
				startItem={channelFollowUpPageStart()}
				endItem={channelFollowUpPageEnd()}
				totalItems={channelFollowUpTotal}
				ariaLabel="Message follow-ups pagination"
				on:pagechange={(event) => goToChannelFollowUpPage(event.detail.page)}
			/>
		{/if}
	</section>
{/snippet}

{#snippet canonicalFollowUpsPanel()}
	<section class="today-section today-section--followups today-channel-followup-section">
		<div class="today-section__header">
			<div>
				<h2>Follow-ups</h2>
				<p>One exact server-ordered lane across message and resurfaced origins.</p>
			</div>
			<Badge text={`${effectiveChannelFollowUpTotal}`} color="warning" />
		</div>
		{#if canonicalAttentionProjection}
			<CanonicalAttentionLane
				projection={canonicalAttentionProjection}
				lane="follow_up"
				page={channelFollowUpPage}
				pageSize={CHANNEL_FOLLOW_UP_PAGE_SIZE}
				on:pagechange={(event) => goToChannelFollowUpPage(event.detail.page)}
				on:resolved={onCanonicalAttentionResolved}
				on:failed={onCanonicalAttentionFailed}
			/>
		{/if}
	</section>
{/snippet}

<svelte:head>
	<title>magican · Town Square</title>
</svelte:head>

<!-- Fleet Civilization — the game hero. Fills the visible pane (.v5-main),
     full-bleed; the copied Today surface below is the scroll-body. See
     docs/archive/plans/2026-07-09-fleet-civilization-game-design.md -->
<section
	class="square-hero"
	class:immersed={heroImmersed}
	class:dock-hidden={!dockOpen}
	data-game-theme="campus"
	bind:this={heroEl}
	aria-label="Crew world"
>
	<div class="square-hero__world">
		{#if heroView === 'campus'}
			<FleetWorld
				bind:this={campusWorld}
				agents={fleetAgents}
				paused={heroPaused}
				selectedTarget={dockTarget}
				on:select={(event) => (dockTarget = event.detail.target)}
			/>
		{:else}
			<OfficeFloor
				agents={fleetAgents}
				paused={heroPaused}
				on:select={(event) => {
					dockTarget = event.detail.citizenId ? agentTarget(event.detail.citizenId) : null;
				}}
			/>
		{/if}
		<SquareWorldChrome
			agents={fleetAgents}
			selectedId={dockTarget?.startsWith('agent:') ? dockTarget.slice(6) : null}
			canFly={heroView === 'campus'}
			on:select={(event) => (dockTarget = event.detail.target)}
			on:fly={(event) => campusWorld?.focusAgent(event.detail.citizenId)}
			on:dock={(event) => {
				dockTarget = null;
				dockSection = event.detail.section;
				setDockOpen(true);
			}}
		/>
		<div class="square-hero__hud">
		<button
			class="square-hero__immerse"
			type="button"
			on:click={toggleImmerse}
			title={heroImmersed ? 'Exit immerse' : 'Immerse'}
			aria-pressed={heroImmersed}
		>⛶</button>
		<div
			class="square-hero__view"
			data-game-skin="pixel"
			role="group"
			aria-label="Crew view mode"
		>
			<button
				type="button"
				on:click={() => setHeroView('floor')}
				aria-pressed={heroView === 'floor'}
				title="2D office floor — the whole crew on one screen"
			>Floor</button>
			<button
				type="button"
				on:click={() => setHeroView('campus')}
				aria-pressed={heroView === 'campus'}
				title="3D campus — the navigable world"
			>Campus</button>
			<button
				type="button"
				on:click={() => setDockOpen(!dockOpen)}
				aria-pressed={dockOpen}
				title={dockOpen ? 'Hide command dock' : 'Show command dock'}
			>Dock</button>
			<a class="square-hero__link" href="#social" title="Town Square social feed">Social</a>
			<button
				type="button"
				on:click={toggleAudio}
				aria-pressed={!audioMuted}
				title={audioMuted ? 'Unmute interface sound' : 'Mute interface sound'}
			>{audioMuted ? 'Muted' : 'Sound'}</button>
		</div>
		</div>
	</div>
	{#if dockOpen}
		<aside class="square-hero__dock" aria-label="Command dock">
			<CommandDock
				agents={fleetAgents}
				selectedTarget={dockTarget}
				openSection={dockSection}
				on:select={(event) => (dockTarget = event.detail.target)}
				on:hide={() => setDockOpen(false)}
			/>
		</aside>
	{/if}
</section>

<!-- Agent Social & Chatter directly below the game hero -->
<div class="square-social-section" id="social" aria-label="Town Square agent social feed">
	<div class="square-social-header">
		<div>
			<h2>Agent Social &amp; Chatter</h2>
			<p>Live transmissions, autonomous thought streams, and group reflections from the fleet.</p>
		</div>
	</div>
	<TownSquareSocial />
	<!-- chatThreadId={resurfacingChatThreadId} -->
</div>

<style>
	/* --- Fleet Civilization hero (the game deck) --- */
	.square-hero {
		position: relative;
		display: grid;
		grid-template-columns: minmax(0, 1fr) var(--dock-w, 26rem);
		width: 100%;
		/* THE HERO MUST FIT THE PANE ON ITS FIRST PAINT.
		   The fallback is the answer in CSS — viewport minus the shell's own
		   topbar token — not an approximation of it. A plain 100dvh overshoots
		   by exactly the bar, so the pane overflowed and /square arrived with a
		   scrollbar and the bottom of the world cut off until the JS measure
		   below landed. That measure stays, because it is exact for any future
		   bar that is not one fixed height, but nothing depends on it now. */
		height: var(--hero-h, calc(100dvh - var(--v5-topbar-h, 48px)));
		flex: 0 0 auto;
		min-height: 18rem;
		overflow: hidden;
		/* The GAME background — driven by the world's sky tokens, independent of
		   the app theme (see fleet-theme.css). */
		background: linear-gradient(
			180deg,
			var(--fleet-sky-top, #95c1dc) 0%,
			var(--fleet-sky-bottom, #cae0ed) 100%
		);
		border-bottom: 1px solid var(--border-subtle, rgba(120, 170, 210, 0.18));
	}
	.square-hero.dock-hidden {
		grid-template-columns: minmax(0, 1fr);
	}
	.square-hero.immersed {
		position: fixed;
		inset: 0;
		height: 100dvh;
		z-index: 1000;
	}
	.square-hero__world {
		position: relative;
		min-width: 0;
		min-height: 0;
		overflow: hidden;
	}
	.square-hero__dock {
		min-width: 0;
		min-height: 0;
		overflow: hidden;
		border-left: 1px solid var(--game-border, rgba(0, 0, 0, 0.25));
		background: var(--game-material-panel, var(--bg-elevated, #fff));
	}
	.square-hero__hud {
		position: absolute;
		top: 0;
		left: 0;
		right: 0;
		display: flex;
		align-items: center;
		/* The immerse control sits top-left so the world view remains unobstructed. */
		justify-content: flex-start;
		gap: 0.5rem;
		padding: 0.75rem 1rem;
		pointer-events: none;
	}
	.square-hero__immerse {
		pointer-events: auto;
		display: inline-grid;
		place-items: center;
		width: 2rem;
		height: 2rem;
		border-radius: 0.5rem;
		border: 1px solid var(--border-subtle, rgba(120, 170, 210, 0.3));
		background: var(--bg-card, rgba(10, 20, 32, 0.5));
		color: var(--text-primary, #223);
		font-size: 1rem;
		cursor: pointer;
	}
	.square-hero__immerse:hover {
		border-color: var(--accent-primary, #888);
		color: var(--accent-primary, inherit);
	}
	/* Floor | Campus. Wears the crew skin (game-chrome.css `data-game-skin`):
	   bitmap display face at its 11px grid size, square corners, hard borders,
	   and a selected state that INVERTS rather than tints — inversion has
	   exactly the contrast of body text on the same plate, in whichever of the
	   app's twenty-two themes is live. */
	.square-hero__view {
		pointer-events: auto;
		display: inline-flex;
		/* Top-RIGHT, not next to immerse: Create task, Attention, and the
		   roster own the left edge on both Floor and Campus. */
		margin-left: auto;
		border: 1px solid var(--game-border-strong, rgba(120, 120, 120, 0.6));
		background: var(--bg-elevated, var(--bg-card, #fff));
	}
	.square-hero__view button,
	.square-hero__link {
		appearance: none;
		border: 0;
		background: transparent;
		color: var(--text-primary, #223);
		padding: 0 0.6rem;
		height: 2rem;
		cursor: pointer;
		font-family: var(--game-font-display, ui-monospace, monospace);
		font-size: var(--game-display-sm, 0.6875rem);
		font-weight: 400;
		font-synthesis: none;
		display: inline-flex;
		align-items: center;
		text-decoration: none;
		letter-spacing: var(--game-display-track-sm, 1px);
		text-transform: uppercase;
	}
	.square-hero__view button + button,
	.square-hero__view button + a,
	.square-hero__view a + button {
		border-left: 1px solid var(--game-border, rgba(120, 120, 120, 0.35));
	}
	.square-hero__view button[aria-pressed='true'] {
		background: var(--text-primary, #223);
		color: var(--bg-elevated, var(--bg-card, #fff));
	}
	.square-hero__view button:focus-visible {
		outline: 2px solid var(--focus-ring, var(--accent-primary, #3978d4));
		outline-offset: -2px;
	}

	.today-page {
		width: 100%;
		max-width: var(--app-content-max, 1320px);
		margin: 0 auto;
		padding: clamp(0.75rem, 2vw, 1.25rem) clamp(0.75rem, 2vw, 1.25rem) 2.5rem;
		display: grid;
		gap: 1.25rem;
	}

	.today-header {
		display: grid;
		grid-template-columns: minmax(0, 1fr) auto;
		gap: clamp(1rem, 2vw, 1.5rem);
		align-items: start;
		padding: clamp(0.95rem, 2vw, 1.2rem);
		border: 1px solid color-mix(in srgb, var(--border-soft) 72%, transparent);
		border-radius: var(--radius-sm);
		background:
			linear-gradient(135deg, color-mix(in srgb, var(--accent-primary) 7%, transparent), transparent 48%),
			color-mix(in srgb, var(--bg-card) 86%, transparent);
		box-shadow: var(--shadow-sm);
	}

	.today-section__header {
		display: flex;
		justify-content: space-between;
		gap: 1rem;
		align-items: flex-start;
	}

	.today-header__copy h1,
	.today-section__header h2 {
		margin: 0;
		color: var(--text-primary);
	}

	.today-kicker {
		margin: 0 0 0.3rem;
		font-size: var(--text-xs);
		text-transform: uppercase;
		letter-spacing: 0.12em;
		color: var(--text-muted);
	}

	.today-header__copy {
		min-width: 0;
	}

	.today-header__copy h1 {
		font-size: clamp(2rem, 4vw, 2.6rem);
		line-height: 1.02;
		letter-spacing: 0;
	}

	.today-header__date,
	.today-header__headline,
	.today-section__header p,
	.feed-card__summary,
	.today-empty p {
		margin: 0.3rem 0 0;
		color: var(--text-secondary);
	}

	.today-header__date {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.35rem;
		font-size: var(--text-sm);
	}

	.today-header__date span {
		color: var(--text-muted);
	}

	.today-header__headline {
		max-width: 48rem;
		font-size: var(--text-md);
		line-height: 1.45;
	}

	.today-header__copy .today-all-clear {
		margin-top: 0.75rem;
	}

	.today-header__actions {
		display: flex;
		flex-wrap: wrap;
		justify-content: flex-end;
		gap: 0.75rem;
		min-width: 0;
		max-width: min(30rem, 100%);
	}

	.today-search {
		display: grid;
		gap: 0.35rem;
		flex: 1 1 17rem;
		min-width: min(17rem, 100%);
	}

	/* Today-specific sizing only. Color, font-family, and border-radius
	   are left to the Input component's own theme-aware styles so the
	   per-theme overrides in Input.svelte (retro-16bit / retro-16bit-light
	   pixel-square borders + monospace font, mario-8bit / cartoon /
	   bubbly / etc.) actually apply. Earlier this selector hard-coded
	   `border-radius: 1rem`, `font: inherit`, `border-color: ...soft...`
	   which stomped on those overrides and made the box look identical
	   across every theme. Width / padding stay overridden here because
	   the Today activity search row needs more horizontal breathing room than the
	   default tight Input sizing. */
	.today-search :global(.muij-input-field) {
		width: 100%;
		padding: 0.55rem 0.8rem;
		font-size: var(--text-sm);
	}

	.today-search :global(.muij-input-label) {
		font-size: var(--text-sm);
		color: var(--text-secondary);
	}

	.today-surface {
		display: grid;
		gap: 1rem;
	}

	.today-section-tabs {
		display: flex;
		gap: 0.5rem;
		min-width: 0;
		overflow-x: auto;
		padding: 0.1rem 0 0.35rem;
		scrollbar-width: thin;
		-webkit-overflow-scrolling: touch;
	}

	.today-section-tabs::-webkit-scrollbar {
		height: 0.4rem;
	}

	.today-section-tabs::-webkit-scrollbar-thumb {
		border-radius: var(--radius-full);
		background: color-mix(in srgb, var(--text-muted) 28%, transparent);
	}

	.today-section-tab {
		--tab-color: var(--text-secondary);
		display: inline-flex;
		flex: 0 0 auto;
		align-items: center;
		gap: 0.45rem;
		min-height: 2.35rem;
		padding: 0 0.75rem;
		border: 1px solid color-mix(in srgb, var(--tab-color) 26%, var(--border-soft));
		border-radius: var(--radius-full);
		background: color-mix(in srgb, var(--bg-card) 84%, transparent);
		color: var(--text-secondary);
		font: inherit;
		font-size: var(--text-sm);
		line-height: 1;
		white-space: nowrap;
		cursor: pointer;
	}

	.today-section-tab:hover,
	.today-section-tab:focus-visible {
		border-color: color-mix(in srgb, var(--tab-color) 45%, var(--border-soft));
		color: var(--text-primary);
	}

	.today-section-tab[aria-selected='true'] {
		background:
			linear-gradient(90deg, color-mix(in srgb, var(--tab-color) 14%, transparent), transparent 72%),
			color-mix(in srgb, var(--bg-card) 96%, transparent);
		color: var(--text-primary);
		box-shadow: inset 0 -2px 0 color-mix(in srgb, var(--tab-color) 70%, transparent);
	}

	.today-section-tab strong {
		display: grid;
		place-items: center;
		min-width: 1.35rem;
		height: 1.35rem;
		padding: 0 0.28rem;
		border-radius: var(--radius-full);
		background: color-mix(in srgb, var(--tab-color) 16%, transparent);
		color: var(--text-primary);
		font-size: var(--text-2xs);
		line-height: 1;
	}

	.today-section-tab--needs-you {
		--tab-color: var(--color-warning);
	}

	.today-section-tab--delivered {
		--tab-color: var(--color-success);
	}

	.today-section-tab--changed,
	.today-section-tab--followups {
		--tab-color: var(--accent-primary);
	}

	.today-section-tab--active-work {
		--tab-color: var(--color-info);
	}

	.today-section-tab--worth-a-look {
		--tab-color: var(--color-info);
	}

	.today-tab-panel {
		display: grid;
		gap: 1rem;
		min-width: 0;
	}

	.today-tab-empty {
		display: grid;
		gap: 0.25rem;
		padding: 0.75rem 0.2rem 0.45rem;
		color: var(--text-secondary);
	}

	.today-tab-empty strong {
		color: var(--text-primary);
		font-size: var(--text-md);
		line-height: 1.25;
	}

	.today-tab-empty p {
		margin: 0;
		font-size: var(--text-sm);
		line-height: 1.45;
	}

	.today-row-skeleton-list {
		display: grid;
		gap: 0.55rem;
	}

	.today-row-skeleton {
		display: grid;
		grid-template-columns: auto minmax(0, 1fr);
		gap: 0.75rem;
		align-items: flex-start;
		padding: 0.75rem;
		border: 1px solid color-mix(in srgb, var(--border-soft) 78%, transparent);
		border-radius: var(--radius-sm);
		background: color-mix(in srgb, var(--bg-card) 82%, transparent);
	}

	.today-row-skeleton__content {
		display: grid;
		gap: 0.42rem;
		min-width: 0;
	}

	.today-row-skeleton__badges {
		display: flex;
		flex-wrap: wrap;
		gap: 0.35rem;
		padding-top: 0.18rem;
	}

	.today-all-clear {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		min-width: 0;
		padding: 0.15rem 0.1rem 0.25rem;
		color: var(--text-secondary);
	}

	.today-all-clear__signal {
		position: relative;
		flex: 0 0 auto;
		width: 2rem;
		height: 2rem;
		display: grid;
		place-items: center;
		border-radius: var(--radius-full);
		background: color-mix(in srgb, var(--color-success) 14%, transparent);
		color: var(--color-success);
	}

	.today-all-clear__signal::after {
		content: '';
		position: absolute;
		inset: -0.28rem;
		border-radius: inherit;
		border: 1px solid color-mix(in srgb, var(--color-success) 20%, transparent);
		opacity: 0.75;
	}

	.today-all-clear > div {
		display: grid;
		gap: 0.08rem;
		min-width: 0;
	}

	.today-all-clear strong {
		color: var(--text-primary);
		font-size: var(--text-md);
		line-height: 1.2;
	}

	.today-all-clear span {
		font-size: var(--text-sm);
		line-height: 1.35;
	}

	.today-digest {
		display: grid;
		gap: 0.65rem;
		padding: 0.85rem 1rem;
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 12px;
	}

	.today-digest__header,
	.today-digest__bullet,
	.today-digest__meta {
		display: flex;
		align-items: center;
		gap: 0.6rem;
	}

	.today-digest__header {
		justify-content: space-between;
		color: var(--text-muted);
		font-size: var(--text-xs);
	}

	.today-digest__header > div {
		display: grid;
		gap: 0.2rem;
		min-width: 0;
	}

	.today-digest__header h2 {
		margin: 0;
		color: var(--text-primary);
		font-size: var(--text-md);
		line-height: 1.2;
	}

	.today-digest__list {
		display: grid;
		gap: 0.35rem;
		margin: 0;
		padding: 0;
		list-style: none;
	}

	.today-digest__bullet {
		width: 100%;
		justify-content: space-between;
		padding: 0.45rem 0;
		border: 0;
		background: transparent;
		color: var(--text-primary);
		font: inherit;
		text-align: left;
		cursor: pointer;
	}

	.today-digest__bullet > span:first-child {
		min-width: 0;
		overflow-wrap: anywhere;
	}

	.today-digest__bullet:hover > span:first-child {
		text-decoration: underline;
		text-decoration-thickness: 1px;
		text-underline-offset: 0.18em;
	}

	.today-digest__meta {
		flex: 0 0 auto;
		flex-wrap: wrap;
		justify-content: flex-end;
	}

	.today-digest__empty {
		margin: 0;
		color: var(--text-secondary);
		font-size: var(--text-sm);
		line-height: 1.45;
	}

	.today-sections {
		display: grid;
		gap: 1rem;
	}

	.today-section {
		display: grid;
		gap: 0.5rem;
		min-width: 0;
		padding-block: 0.25rem 0.35rem;
	}

	.today-section-card {
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 12px;
		padding: 0.85rem 1rem;
	}

	.today-section__header {
		display: flex;
		justify-content: space-between;
		align-items: flex-start;
		gap: 1rem;
		min-width: 0;
		padding-bottom: 0.45rem;
		border-bottom: 1px solid color-mix(in srgb, var(--border-soft) 70%, transparent);
	}

	.today-section__header h2 {
		margin: 0;
		font-size: var(--text-md);
		line-height: 1.2;
		color: var(--text-primary);
	}

	.today-space-groups {
		display: grid;
		gap: 0.8rem;
	}

	.today-space-group {
		display: grid;
		gap: 0.35rem;
		min-width: 0;
	}

	.today-space-group__header {
		display: flex;
		justify-content: space-between;
		align-items: center;
		gap: 0.7rem;
		padding: 0.15rem 0;
		color: var(--text-secondary);
		font-size: var(--text-xs);
	}

	.today-space-group__header strong {
		color: var(--text-primary);
		overflow-wrap: anywhere;
	}

	.today-space-group__header span {
		min-width: 1.45rem;
		height: 1.45rem;
		display: grid;
		place-items: center;
		border: 1px solid color-mix(in srgb, var(--border-soft) 78%, transparent);
		border-radius: var(--radius-full);
		background: color-mix(in srgb, var(--bg-card) 82%, transparent);
		font-size: var(--text-2xs);
	}

	.today-space-group--unfiled .today-space-group__header strong {
		color: var(--text-muted);
	}

	/* Message follow-ups section — a themed card so it reads
	   as a first-class Today surface, not a raw list. */
	.today-channel-followup-section {
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 12px;
		padding: 0.85rem 1rem;
	}
	.today-channel-followup-list {
		display: flex;
		flex-direction: column;
		gap: 0.15rem;
		margin-top: 0.5rem;
	}
	.today-channel-followup-row {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 0.75rem;
		padding: 0.6rem 0;
		border-top: 1px solid color-mix(in srgb, var(--border-soft) 70%, transparent);
		flex-wrap: wrap;
	}
	.today-channel-followup-row:first-child {
		border-top: 0;
	}
	.today-channel-followup-main {
		min-width: 0;
		flex: 1 1 18rem;
	}
	.today-channel-followup-line {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		flex-wrap: wrap;
		margin-bottom: 0.15rem;
	}
	.today-channel-followup-label {
		font-size: 0.72rem;
		font-weight: 600;
		color: var(--color-warning, var(--accent-primary));
	}
	.today-channel-followup-provider {
		font-size: 0.72rem;
		color: var(--text-muted);
	}
	.today-channel-followup-time {
		font-size: 0.72rem;
		color: var(--text-muted);
		font-variant-numeric: tabular-nums;
	}
	.today-channel-followup-subject {
		font-size: 0.9rem;
		font-weight: 500;
		color: var(--text-primary);
	}
	.today-channel-followup-sender,
	.today-channel-followup-reason {
		font-size: 0.78rem;
		color: var(--text-muted);
	}
	.today-channel-followup-reason {
		margin-top: 0.1rem;
		color: var(--text-secondary);
	}
	.today-channel-followup-summary {
		margin-top: 0.18rem;
		font-size: 0.82rem;
		line-height: 1.35;
		color: var(--text-primary);
	}

	.today-empty--surface {
		display: flex;
		align-items: center;
		gap: 0.85rem;
		padding: 0.75rem 0.15rem 0.4rem;
	}

	.today-empty--surface strong {
		color: var(--text-primary);
		font-size: var(--text-md);
		line-height: 1.2;
	}

	.today-empty--surface p {
		max-width: 42rem;
		line-height: 1.45;
	}

	.today-empty__signal {
		display: grid;
		grid-template-columns: repeat(3, 0.46rem);
		gap: 0.28rem;
		align-items: end;
		height: 1.6rem;
		flex: 0 0 auto;
	}

	.today-empty__signal span {
		display: block;
		width: 0.46rem;
		border-radius: var(--radius-full);
		background: color-mix(in srgb, var(--accent-primary) 42%, transparent);
	}

	.today-empty__signal span:nth-child(1) {
		height: 0.55rem;
		opacity: 0.55;
	}

	.today-empty__signal span:nth-child(2) {
		height: 1.1rem;
		opacity: 0.75;
	}

	.today-empty__signal span:nth-child(3) {
		height: 1.6rem;
	}

	.today-row-list {
		display: grid;
		gap: 0.45rem;
	}

	/* FLIP/transition wrapper around each row — no overflow clipping so
	   the snooze popover and mid-animation shadows can escape. */
	.today-row-motion {
		min-width: 0;
	}

	.today-row {
		--row-color: var(--accent-primary);
		position: relative;
		display: grid;
		grid-template-columns: auto minmax(0, 1fr) auto;
		gap: 0.8rem;
		align-items: start;
		padding: 0.72rem 0.75rem 0.72rem 0.85rem;
		border: 1px solid color-mix(in srgb, var(--border-soft) 64%, transparent);
		border-radius: var(--radius-sm);
		background:
			linear-gradient(90deg, color-mix(in srgb, var(--row-color) 6%, transparent), transparent 42%),
			color-mix(in srgb, var(--bg-card) 72%, transparent);
		min-width: 0;
		/* Whole row is the primary action (role="button"). */
		cursor: pointer;
		/* No overflow:hidden here — the snooze popover must escape the row
		   bounds; the left accent bar rounds its own corners instead. */
		transition:
			border-color 140ms ease,
			background 140ms ease,
			box-shadow 140ms ease;
	}

	.today-row:focus-visible {
		outline: 2px solid color-mix(in srgb, var(--row-color) 60%, transparent);
		outline-offset: 2px;
	}

	.today-row::before {
		content: '';
		position: absolute;
		inset: 0 auto 0 0;
		width: 0.22rem;
		border-radius: var(--radius-sm) 0 0 var(--radius-sm);
		background: color-mix(in srgb, var(--row-color) 58%, transparent);
	}

	.today-row:hover,
	.today-row:focus-within {
		border-color: color-mix(in srgb, var(--row-color) 42%, var(--border-soft));
		box-shadow: 0 0 0 3px color-mix(in srgb, var(--row-color) 9%, transparent);
	}

	.today-row--needs-you {
		--row-color: var(--color-warning);
	}

	.today-row--delivered {
		--row-color: var(--color-success);
	}

	.today-row--active-work {
		--row-color: var(--color-info);
	}

	.today-row--changed {
		--row-color: var(--accent-primary);
	}

	.today-row--emphasis {
		border-color: color-mix(in srgb, var(--row-color) 36%, var(--border-soft));
		background:
			linear-gradient(90deg, color-mix(in srgb, var(--row-color) 11%, transparent), transparent 52%),
			color-mix(in srgb, var(--bg-card) 78%, transparent);
	}

	.today-row--snooze-open {
		z-index: 40;
	}

	.today-row__marker {
		position: relative;
		width: 1.65rem;
		height: 1.65rem;
		display: grid;
		place-items: center;
		border-radius: var(--radius-full);
		border: 1px solid color-mix(in srgb, var(--row-color) 45%, transparent);
		color: var(--text-primary);
		background: color-mix(in srgb, var(--row-color) 12%, transparent);
	}

	/* "Something is live" dot on Active Work rows — sits on the marker's
	   corner in the row's own color; the shared .live-pulse-dot class in
	   app.css supplies the (reduced-motion-guarded) breathing animation. */
	.today-row__live-dot {
		position: absolute;
		top: -2px;
		right: -2px;
		width: 7px;
		height: 7px;
		border-radius: var(--radius-full);
		background: var(--row-color);
		box-shadow: 0 0 0 2px var(--bg-card);
	}

	.today-row__content {
		display: grid;
		gap: 0.28rem;
		min-width: 0;
	}

	.today-row__topline {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 0.9rem;
		min-width: 0;
	}

	.today-row__topline strong {
		color: var(--text-primary);
		overflow-wrap: anywhere;
	}

	.today-row__topline span,
	.today-row__summary {
		font-size: var(--text-xs);
		color: var(--text-muted);
	}

	.today-row__content p {
		margin: 0;
		color: var(--text-secondary);
		line-height: 1.42;
	}

	.today-row__summary {
		display: -webkit-box;
		-webkit-line-clamp: 1;
		line-clamp: 1;
		-webkit-box-orient: vertical;
		overflow: hidden;
	}

	.today-row__learned {
		display: grid;
		gap: 0.18rem;
		margin: 0;
		padding: 0.15rem 0 0 1rem;
		color: var(--text-primary);
		font-size: var(--text-xs);
	}

	.today-row__learned li {
		padding-inline-start: 0.1rem;
	}

	.today-row__learned span,
	.today-row__learned small {
		display: block;
		overflow-wrap: anywhere;
	}

	.today-row__learned small {
		margin-top: 0.12rem;
		color: var(--text-muted);
		line-height: 1.35;
	}

	.today-row__meta,
	.today-activity-actions,
	.today-activity-controls {
		display: flex;
		flex-wrap: wrap;
		gap: 0.5rem;
		align-items: center;
	}

	.today-row__actions {
		display: flex;
		align-items: center;
		justify-content: flex-end;
		gap: 0.4rem;
		flex: 0 0 auto;
		flex-wrap: wrap;
		min-width: max-content;
	}

	.today-row__actions :global(.muij-button:not(.muij-button-icon)) {
		min-width: 4.25rem;
	}

	.today-snooze {
		position: relative;
		display: inline-flex;
	}

	/* Mirrors the outline sm icon-only muij-button so the raw trigger (needed
	   for aria-haspopup/aria-expanded) sits flush with its siblings. */
	.today-snooze__trigger {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 32px;
		height: 32px;
		padding: 8px;
		background: transparent;
		color: var(--text-secondary);
		border: 1px solid var(--button-outline-border, var(--border-soft));
		border-radius: var(--radius-md);
		cursor: pointer;
		transition:
			background 0.15s,
			color 0.15s,
			border-color 0.15s;
	}

	.today-snooze__trigger:hover:not(:disabled) {
		background: var(--button-outline-hover-bg, transparent);
		border-color: var(--button-outline-border, var(--text-muted));
		color: var(--button-outline-hover-color, var(--text-primary));
	}

	.today-snooze__trigger:disabled {
		opacity: 0.5;
		cursor: default;
	}

	.today-snooze__menu {
		position: absolute;
		top: calc(100% + 0.3rem);
		right: 0;
		z-index: 30;
		min-width: 12.5rem;
		display: flex;
		flex-direction: column;
		padding: 0.25rem;
		border: 1px solid var(--border-default, #2a2a33);
		border-radius: 0.6rem;
		background: var(--bg-elevated, #1b1b22);
		box-shadow: 0 8px 24px rgba(0, 0, 0, 0.25);
	}

	.today-snooze__menu button {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		gap: 0.5rem;
		width: 100%;
		text-align: left;
		background: none;
		border: none;
		border-radius: 0.4rem;
		color: var(--text-primary, #2d3436);
		cursor: pointer;
		font-size: 0.82rem;
		padding: 0.4rem 0.5rem;
		transition: background 0.12s ease;
		white-space: nowrap;
	}

	.today-snooze__menu button:hover,
	.today-snooze__menu button:focus-visible {
		background: color-mix(in srgb, var(--text-primary, #2d3436) 8%, transparent);
		outline: none;
	}

	.today-snooze__menu small {
		color: var(--text-muted, #8f9799);
		font-size: 0.72rem;
	}

	.today-undo-toast {
		position: fixed;
		bottom: 1.5rem;
		left: 50%;
		transform: translateX(-50%);
		z-index: 60;
		display: inline-flex;
		align-items: center;
		gap: 0.75rem;
		padding: 0.55rem 1rem;
		border: 1px solid var(--border-default, #2a2a33);
		border-radius: 999px;
		background: var(--bg-elevated, #1b1b22);
		color: var(--text-primary);
		box-shadow: 0 10px 30px rgba(0, 0, 0, 0.3);
		font-size: 0.82rem;
	}

	.today-undo-toast button {
		background: none;
		border: none;
		padding: 0;
		color: var(--accent-primary);
		cursor: pointer;
		font-size: 0.82rem;
		font-weight: 600;
	}

	.today-undo-toast button:hover,
	.today-undo-toast button:focus-visible {
		text-decoration: underline;
	}

	.today-activity-actions {
		justify-content: flex-end;
	}

	.today-activity-controls {
		justify-content: flex-end;
		max-width: min(720px, 100%);
	}

	.activity-collapsed {
		padding: 0.85rem 0.9rem;
		border: 1px dashed color-mix(in srgb, var(--border-soft) 76%, transparent);
		border-radius: var(--radius-sm);
		background: color-mix(in srgb, var(--bg-soft) 42%, transparent);
	}

	.hidden-today {
		display: grid;
		gap: 0.65rem;
		padding: 0.85rem 0.9rem;
		border: 1px solid color-mix(in srgb, var(--border-soft) 76%, transparent);
		border-radius: var(--radius-sm);
		background: color-mix(in srgb, var(--bg-card) 76%, transparent);
	}

	.hidden-today__item {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 0.85rem;
		min-width: 0;
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
		font: inherit;
		font-size: var(--text-sm);
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
		font-size: var(--text-xs);
		line-height: 1.45;
	}

	.hidden-today__list {
		display: grid;
		gap: 0.55rem;
	}

	.hidden-today__item {
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
		overflow-wrap: anywhere;
	}

	.activity-collapsed p {
		margin: 0;
		color: var(--text-secondary);
		font-size: var(--text-sm);
		line-height: 1.45;
	}

	.today-scrolls,
	.today-activity {
		padding: clamp(0.85rem, 1.6vw, 1rem);
		border-radius: var(--radius-md);
		background:
			linear-gradient(180deg, color-mix(in srgb, var(--bg-card) 92%, transparent), color-mix(in srgb, var(--bg-soft) 46%, transparent)),
			var(--bg-card);
		border: 1px solid color-mix(in srgb, var(--border-soft) 70%, transparent);
		box-shadow: var(--shadow-md);
	}

	.scroll-strip {
		display: grid;
		grid-auto-flow: column;
		grid-auto-columns: minmax(260px, 320px);
		gap: 0.85rem;
		overflow-x: auto;
		padding-bottom: 0.25rem;
	}

	/* Two horizontal-rectangle skeleton placeholders for the loading
	   state — side-by-side, wider than tall so they read as preview
	   tiles, not full cards. Fits within the 1320px section column. */
	.today-scrolls-skeleton {
		display: grid;
		grid-template-columns: 1fr 1fr;
		gap: 0.85rem;
		padding: 0.5rem 0 0.25rem;
	}
	.today-scrolls-skeleton :global(.skeleton-card) {
		min-height: 6.5rem;
	}

	:global(.scroll-card) {
		display: grid;
		gap: 0.7rem;
		min-height: 12.25rem;
		border: 1px solid color-mix(in srgb, var(--border-soft) 68%, transparent);
		background: color-mix(in srgb, var(--bg-base) 94%, transparent);
		border-radius: var(--radius-md);
		/* Long titles / words must wrap inside the card. Press Start 2P
		   in Mario doesn't wrap on word boundaries the way proportional
		   fonts do, so without these the text overflows horizontally. */
		overflow: hidden;
		min-width: 0;
	}

	:global(.scroll-card) strong,
	.scroll-card__preview,
	.scroll-card__meta,
	.scroll-card__tags span {
		overflow-wrap: anywhere;
		word-break: break-word;
		min-width: 0;
	}

	/* Mario — Press Start 2P at the default scroll-card text sizes is
	   pixel-wide and runs out of the card. Drop sizes a notch and make
	   sure tag/meta items can wrap. */
	:global([data-theme^="mario-8bit"]) :global(.scroll-card) strong {
		font-size: 0.78rem;
		line-height: 1.4;
	}
	:global([data-theme^="mario-8bit"]) .scroll-card__preview {
		font-size: 0.62rem;
		line-height: 1.7;
	}
	:global([data-theme^="mario-8bit"]) .scroll-card__meta,
	:global([data-theme^="mario-8bit"]) .scroll-card__tags span {
		font-size: 0.55rem;
	}

	.feed-card {
		border: 1px solid color-mix(in srgb, var(--border-soft) 68%, transparent);
		background: color-mix(in srgb, var(--bg-base) 94%, transparent);
		border-radius: var(--radius-md);
	}

	.feed-card strong {
		color: var(--text-primary);
	}

	.scroll-card__footer,
	.scroll-card__tags,
	.feed-card__meta,
	.feed-card__approval-actions,
	.today-filters {
		display: flex;
		flex-wrap: wrap;
		gap: 0.5rem;
		align-items: center;
	}


	.scroll-card__meta,
	.scroll-card__tags span,
	.feed-card__meta span,
	.learning-edit__context span {
		font-size: var(--text-xs);
		color: var(--text-muted);
	}

	.scroll-card__preview {
		margin: 0;
		font-size: var(--text-sm);
		line-height: 1.55;
		color: var(--text-secondary);
		display: -webkit-box;
		-webkit-line-clamp: 5;
		line-clamp: 5;
		-webkit-box-orient: vertical;
		overflow: hidden;
	}

	.today-filters :global(.muij-button) {
		border-radius: var(--radius-full);
	}

	.today-activity__list {
		display: grid;
		gap: 0.85rem;
	}

	.feed-card {
		display: grid;
		grid-template-columns: auto minmax(0, 1fr);
		gap: 0.9rem;
		padding: 0.95rem;
	}

	.feed-card--highlighted {
		border-color: color-mix(in srgb, var(--accent-secondary) 55%, var(--border-soft));
		box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent-secondary) 16%, transparent);
	}

	.feed-card__icon {
		width: 2.1rem;
		height: 2.1rem;
		border-radius: var(--radius-full);
		background: color-mix(in srgb, var(--accent-primary) 12%, transparent);
		display: grid;
		place-items: center;
		color: var(--text-primary);
	}

	.feed-card__content {
		display: grid;
		gap: 0.6rem;
		min-width: 0;
	}

	.feed-card__header {
		display: flex;
		justify-content: space-between;
		gap: 0.8rem;
		align-items: flex-start;
	}

	.feed-card__summary {
		line-height: 1.45;
	}

	.learning-edit {
		display: grid;
		gap: 1rem;
	}

	.learning-edit__context,
	.learning-edit__actions {
		display: flex;
		flex-wrap: wrap;
		gap: 0.55rem;
		align-items: center;
	}

	.learning-edit__actions {
		justify-content: flex-end;
	}

	.today-loading,
	.today-empty {
		padding: 1rem 0.25rem 0.1rem;
	}

	.today-error {
		margin: 0;
		color: var(--color-error);
		font-size: var(--text-sm);
	}

	@media (max-width: 900px) {
		.today-section__header,
		.feed-card__header {
			flex-direction: column;
		}

		.today-header {
			grid-template-columns: 1fr;
		}

		.today-header__actions {
			width: 100%;
			justify-content: flex-start;
			max-width: none;
		}

		.today-sections {
			grid-template-columns: 1fr;
		}

		.today-row {
			grid-template-columns: auto minmax(0, 1fr);
		}

		.today-row__actions {
			grid-column: 2;
			justify-content: flex-start;
			flex-wrap: wrap;
			min-width: 0;
		}

		.today-digest__header,
		.today-digest__bullet {
			align-items: flex-start;
			flex-direction: column;
		}

		.today-digest__meta {
			justify-content: flex-start;
		}

		.hidden-today__item {
			flex-direction: column;
		}

		.hidden-today__item :global(.muij-button) {
			align-self: flex-start;
		}

		.scroll-strip {
			grid-auto-columns: minmax(240px, 280px);
		}

		.feed-card {
			grid-template-columns: 1fr;
		}
	}

	@media (max-width: 520px) {
		.today-row__topline {
			flex-direction: column;
			gap: 0.2rem;
		}
	}

	/* Agent Social Section directly below game */
	.square-social-section {
		width: 100%;
		max-width: 1240px;
		margin: 0 auto;
		padding: 2rem 1.5rem 4rem;
		display: flex;
		flex-direction: column;
		gap: 1.25rem;
		box-sizing: border-box;
	}

	.square-social-header h2 {
		margin: 0;
		font-family: var(--font-primary, sans-serif);
		font-size: 1.5rem;
		font-weight: 700;
		color: var(--text-primary);
	}

	.square-social-header p {
		margin: 0.25rem 0 0;
		color: var(--text-muted);
		font-size: 0.875rem;
	}
</style>
