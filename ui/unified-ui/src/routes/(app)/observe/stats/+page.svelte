<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import ObservableSourceObservability from '$lib/observe/ObservableSourceObservability.svelte';
	import BrowserEngineObservability from '$lib/observe/BrowserEngineObservability.svelte';
	import FunnelChart from '$lib/magician/components/generative/FunnelChart.svelte';
	import PieChart from '$lib/magician/components/generative/PieChart.svelte';
	import StackedBarChart from '$lib/magician/components/generative/StackedBarChart.svelte';
	import {
		attentionLabel,
		fetchAttentionFunnelObservability,
		type AttentionFunnelObservability
	} from '$lib/stores/attentionFunnelStore';
	import { getCurrentScopeIdentity, scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import {
		fetchChannelAssistStats,
		distillQueueView,
		distillProcessedTotal,
		fetchChannelAssistSyncStatus,
		fetchChannelAssistLlmUsage,
		fetchChannelRecentDistill,
		providerLabel,
		laneLabel,
		labelText,
		opLabel,
		bindingLabel,
		senderLabel,
		latencyLabel,
		localDateTime,
		compact,
		relativeTime,
		type ChannelAssistStats,
		type ChannelAssistSyncStatus,
		type ChannelAssistLlmUsage,
		type ChannelRecentDistillEntry
	} from '$lib/stores/channelStatsStore';
	import {
		fetchResurfacingObservability,
		type ResurfacingObservability
	} from '$lib/resurfacing/observabilityQueries';

	let stats: ChannelAssistStats | null = null;
	let status: ChannelAssistSyncStatus | null = null;
	let llm: ChannelAssistLlmUsage | null = null;
	let feed: ChannelRecentDistillEntry[] = [];
	let ambientStats: AmbientStats | null = null;
	let publicContacts: PublicContactProfileListResponse | null = null;
	let publicChatStatus: PublicChatStatus | null = null;
	let attentionFunnel: AttentionFunnelObservability | null = null;
	let attentionFunnelError: string | null = null;
	let resurfacingObservability: ResurfacingObservability | null = null;
	let resurfacingObservabilityError: string | null = null;
	type AmbientRange = 'today' | '7d' | '30d';
	const ambientRangeOptions: { key: AmbientRange; label: string }[] = [
		{ key: 'today', label: 'Today' },
		{ key: '7d', label: '7d' },
		{ key: '30d', label: '30d' }
	];
	const ATTENTION_LOOKBACK_HOURS = 24 * 7;
	const ATTENTION_LOOKBACK_DAYS = Math.round(ATTENTION_LOOKBACK_HOURS / 24);
	const ATTENTION_STAGE_ORDER = [
		'ingested',
		'distilled',
		'extracted',
		'filtered',
		'routed',
		'surfaced',
		'acted',
		'dropped'
	];
	const ATTENTION_SOURCE_FAMILY_ORDER = [
		'promise',
		'comms_ingest',
		'resurfacing',
		'memory',
		'task',
		'episode',
		'calendar',
		'meeting',
		'screen_observation',
		'tab_observation'
	];
	const ATTENTION_LANE_ORDER = [
		'needs_you',
		'follow_up',
		'worth_a_look',
		'active_work',
		'delivered',
		'changed',
		'failed'
	];
	const DISTILL_STATE_KEYS = ['done', 'pending', 'skipped', 'suppressed', 'failed', 'expired'];
	const SKELETON_METRICS = Array.from({ length: 6 });
	const SKELETON_ENGINE = Array.from({ length: 3 });
	const SKELETON_BARS = Array.from({ length: 4 });
	const SKELETON_FEED = Array.from({ length: 3 });
	const SKELETON_ROWS = Array.from({ length: 6 });
	const FUNNEL_COLORS: Record<string, string> = {
		synced: 'var(--accent-primary)',
		distilled: 'var(--observe-blue, var(--accent-primary))',
		classified: 'var(--observe-green, var(--color-success))',
		needs_approval: 'var(--color-warning)',
		done: 'var(--observe-blue, var(--accent-primary))',
		pending: 'var(--accent-primary)',
		skipped: 'var(--text-muted)',
		suppressed: 'var(--observe-violet, var(--accent-secondary, var(--accent-primary)))',
		failed: 'var(--color-danger, var(--color-error, #b42318))',
		other: 'var(--text-muted)',
		unaccounted: 'var(--text-muted)'
	};
	let ambientRange: AmbientRange = 'today';
	let channelStatsLoading = true;
	let ambientStatsLoading = true;
	let publicChatLoading = true;
	let llmUsageLoading = true;
	let attentionFunnelLoading = true;
	let resurfacingObservabilityLoading = true;
	$: loading =
		channelStatsLoading || ambientStatsLoading || publicChatLoading || attentionFunnelLoading || resurfacingObservabilityLoading;
	let timer: ReturnType<typeof setInterval> | null = null;
	let ambientTimer: ReturnType<typeof setInterval> | null = null;
	let feedTimer: ReturnType<typeof setInterval> | null = null;
	let llmTimer: ReturnType<typeof setInterval> | null = null;
	let attentionFunnelTimer: ReturnType<typeof setInterval> | null = null;
	let resurfacingObservabilityTimer: ReturnType<typeof setInterval> | null = null;
	let channelStatsRequestId = 0;
	let ambientStatsRequestId = 0;
	let publicChatRequestId = 0;
	let llmUsageRequestId = 0;
	let attentionFunnelRequestId = 0;
	let resurfacingObservabilityRequestId = 0;
	let statsMounted = false;
	let resurfacingLoadedScopeKey = '';
	let webSourceObservability: { refresh: () => void } | null = null;
	let browserEngineObservability: { refresh: () => void } | null = null;

	interface AmbientByteTotals {
		batch_raw_bytes?: number;
		signal_payload_bytes?: number;
		signal_metadata_bytes?: number;
		dom_estimated_bytes?: number;
	}

	interface AmbientSignalRow {
		signal_id: string;
		ts_ms: number;
		origin: string;
		path?: string | null;
		title?: string | null;
		event_kind?: string | null;
		content_type?: string | null;
		sensitivity?: string | null;
		payload_bytes?: number;
		metadata_bytes?: number;
		dom_estimated_bytes?: number;
	}

	interface AmbientBatchRow {
		batch_id: string;
		received_at_ms: number;
		raw_signal_count: number;
		accepted_count: number;
		rejected_count: number;
		duplicate_count: number;
		batch_raw_bytes: number;
		client_queue_depth?: number | null;
		rejections_by_reason?: Record<string, number>;
	}

	interface AmbientRunRow {
		run_id: string;
		source: string;
		started_at_ms: number;
		finished_at_ms?: number | null;
		status: string;
		signals_considered: number;
		clusters_total: number;
		clusters_promotable: number;
		llm_succeeded: number;
		evidence_created: number;
		review_pending: number;
	}

	interface AmbientClusterMemory {
		attempted?: boolean;
		result?: string;
		candidate_id?: string;
		review_state?: string;
		target?: string;
		blocked_reason?: string;
		review_required?: boolean;
	}

	interface AmbientClusterRow {
		run_id: string;
		cluster_id: string;
		cluster_key: string;
		state: string;
		updated_at_ms: number;
		host: string;
		signal_count: number;
		page_count: number;
		distinct_page_count: number;
		salience?: number;
		evidence_id?: string | null;
		memory?: AmbientClusterMemory | null;
		error?: string | null;
	}

	interface AmbientStats {
		range?: {
			from_day: string;
			to_day: string;
			days: number;
			days_with_data?: number;
			capped?: boolean;
			compacted_days?: number;
			live_detail_days?: number;
		};
		summary: {
			signals: number;
			pages: number;
			distinct_pages: number;
			origins: number;
			bytes: AmbientByteTotals;
		};
		ingestion_funnel: {
			received: number;
			accepted: number;
			rejected: number;
			duplicates: number;
			rejections_by_reason?: Record<string, number>;
		};
		page_metrics: {
			total_pages: number;
			distinct_pages: number;
			origins: number;
			top_origins?: Record<string, number>;
		};
		types: {
			content_type?: Record<string, number>;
			event_kind?: Record<string, number>;
			sensitivity?: Record<string, number>;
		};
		distill_pipeline: {
			runs: number;
			clusters: number;
			evidence_created: number;
			memory_review_pending: number;
			memory_approved?: number;
			memory_rejected?: number;
			memory_indexed?: number;
		};
		llm: {
			operation: string;
			calls: number;
			input_tokens: number;
			output_tokens: number;
			cost_usd: number;
		};
		retention?: {
			last_swept_day?: string | null;
			last_swept_at_ms?: number | null;
			last_sweep_status?: string | null;
			last_sweep_deleted_files?: number;
		};
		recent_batches?: AmbientBatchRow[];
		recent_signals?: AmbientSignalRow[];
		recent_runs?: AmbientRunRow[];
		recent_clusters?: AmbientClusterRow[];
	}

	interface PublicContactSenderKey {
		workspace: string;
		channel_type: string;
		channel_address: string;
	}

	interface PublicContactResearchCounts {
		not_eligible: number;
		eligible: number;
		queued: number;
		active: number;
		completed: number;
		failed: number;
		skipped: number;
		pending: number;
	}

	interface PublicContactProfileCounts {
		total: number;
		owner_review_priority: number;
		missing_identity: number;
		missing_purpose: number;
		research: PublicContactResearchCounts;
	}

	interface PublicContactResearchResult {
		task_id: string;
		execution_id?: string | null;
		primary_user_output_id?: string | null;
		summary?: string | null;
		excerpt?: string | null;
		stored_at: number;
	}

	interface PublicContactProfile {
		sender_key: PublicContactSenderKey;
		display_name?: string | null;
		claimed_name?: string | null;
		claimed_org?: string | null;
		claimed_role?: string | null;
		purpose?: string | null;
		research_status: string;
		last_seen_at: number;
		owner_review_priority: boolean;
		research_task_id?: string | null;
		research_result?: PublicContactResearchResult | null;
	}

	interface PublicContactProfileListResponse {
		profiles: PublicContactProfile[];
		count: number;
		total_matching: number;
		total_profiles: number;
		counts: PublicContactProfileCounts;
	}

	interface PublicChatAdmissionStatus {
		paid_available: number;
		paid_in_use: number;
		max_paid_concurrent: number;
		fallback_in_use: number;
		max_fallback_concurrent: number;
		paid_calls: number;
		paid_calls_limit: number;
		paid_tokens: number;
		paid_tokens_limit: number;
		paid_cost_usd: number | null;
		paid_cost_limit_usd: number;
		fallback_reason?: string | null;
	}

	interface PublicChatQueueStatus {
		queued: number;
		max_global_queue_depth: number;
		sender_depths: number;
		worker_active: boolean;
		oldest_age_ms?: number | null;
	}

	interface PublicChatIdentityResearchStatus {
		enabled: boolean;
		queued: number;
		active: number;
		max_concurrent: number;
		queued_jobs_today: number;
		max_jobs_today: number;
		cost_usd_today: number;
		max_cost_usd_today: number;
		worker_active: boolean;
	}

	interface PublicChatStatus {
		enabled: boolean;
		source_surface?: string | null;
		source_surfaces?: string[];
		paid_operation?: string | null;
		fallback_operation?: string | null;
		admission: PublicChatAdmissionStatus;
		queue: PublicChatQueueStatus;
		identity_research?: PublicChatIdentityResearchStatus | null;
	}

	function utcDay(date: Date): string {
		return date.toISOString().slice(0, 10);
	}

	function ambientRangeQuery(range: AmbientRange): string {
		const params = new URLSearchParams({ limit: '24' });
		if (range !== 'today') {
			const now = new Date();
			const from = new Date(now);
			from.setUTCDate(now.getUTCDate() - (range === '7d' ? 6 : 29));
			params.set('from', utcDay(from));
			params.set('to', utcDay(now));
		}
		return params.toString();
	}

	async function fetchAmbientStats(range: AmbientRange = ambientRange): Promise<AmbientStats | null> {
		try {
			const res = await fetch(`/api/magician/v2/ambient/stats?${ambientRangeQuery(range)}`);
			if (!res.ok) return null;
			return (await res.json()) as AmbientStats;
		} catch {
			return null;
		}
	}

	async function fetchPublicContacts(): Promise<PublicContactProfileListResponse | null> {
		const params = new URLSearchParams({
			limit: '8'
		});
		try {
			const res = await fetch(`/api/magician/v2/chat/public-contacts?${params.toString()}`);
			if (!res.ok) return null;
			return (await res.json()) as PublicContactProfileListResponse;
		} catch {
			return null;
		}
	}

	async function fetchPublicChatStatus(): Promise<PublicChatStatus | null> {
		try {
			const res = await fetch('/api/magician/v2/chat/public-chat/status');
			if (!res.ok) return null;
			return (await res.json()) as PublicChatStatus;
		} catch {
			return null;
		}
	}

	function contactName(profile: PublicContactProfile): string {
		return (
			profile.display_name
			|| profile.claimed_name
			|| profile.claimed_org
			|| profile.sender_key.channel_address
			|| 'unknown contact'
		);
	}

	function contactDetail(profile: PublicContactProfile): string {
		const parts = [profile.claimed_role, profile.claimed_org, profile.purpose]
			.map((part) => part?.trim())
			.filter((part): part is string => Boolean(part));
		return parts.length > 0 ? parts.slice(0, 2).join(' · ') : profile.sender_key.channel_type;
	}

	function researchStatusText(status: string | null | undefined): string {
		return stateText(status);
	}

	// Live throughput — derived from aggregate deltas between polls (no backend
	// state needed). Summaries and skipped/coalesced rows count as processed;
	// expiring old queue entries is maintenance, never model throughput.
	let prevDone = 0;
	let prevProcessed = 0;
	let prevTs = 0;
	let distillSummaryRatePerMin = 0;
	let distillProcessedRatePerMin = 0;
	let sampled = false;

	function applyChannelThroughputSample(
		s: ChannelAssistStats | null,
		st: ChannelAssistSyncStatus | null
	) {
		const doneNow = s?.distill.done ?? 0;
		const processedNow = distillProcessedTotal(s);
		const now = Date.now();
		if (prevTs > 0 && now > prevTs && doneNow >= prevDone) {
			const dtMin = (now - prevTs) / 60000;
			const summaryDelta = doneNow - prevDone;
			const processedDelta = Math.max(0, processedNow - prevProcessed);
			const summaryInst = dtMin > 0 ? summaryDelta / dtMin : 0;
			const processedInst = dtMin > 0 ? processedDelta / dtMin : 0;
			distillSummaryRatePerMin = sampled
				? distillSummaryRatePerMin * 0.5 + summaryInst * 0.5
				: summaryInst;
			distillProcessedRatePerMin = sampled
				? distillProcessedRatePerMin * 0.5 + processedInst * 0.5
				: processedInst;
			sampled = true;
		}
		prevDone = doneNow;
		prevProcessed = processedNow;
		prevTs = now;
	}

	async function loadChannelStats() {
		const requestId = ++channelStatsRequestId;
		channelStatsLoading = true;
		try {
			const [s, st] = await Promise.all([
				fetchChannelAssistStats(),
				fetchChannelAssistSyncStatus()
			]);
			if (requestId !== channelStatsRequestId) return;
			applyChannelThroughputSample(s, st);
			stats = s;
			status = st;
		} finally {
			if (requestId === channelStatsRequestId) channelStatsLoading = false;
		}
	}

	async function loadAmbientPanel(range: AmbientRange = ambientRange) {
		const requestId = ++ambientStatsRequestId;
		ambientStatsLoading = true;
		try {
			const a = await fetchAmbientStats(range);
			if (requestId === ambientStatsRequestId && range === ambientRange) {
				ambientStats = a;
			}
		} finally {
			if (requestId === ambientStatsRequestId && range === ambientRange) {
				ambientStatsLoading = false;
			}
		}
	}

	async function loadPublicChatPanel() {
		const requestId = ++publicChatRequestId;
		publicChatLoading = true;
		try {
			const [pc, pcs] = await Promise.all([fetchPublicContacts(), fetchPublicChatStatus()]);
			if (requestId !== publicChatRequestId) return;
			publicContacts = pc;
			publicChatStatus = pcs;
		} finally {
			if (requestId === publicChatRequestId) publicChatLoading = false;
		}
	}

	async function loadAttentionFunnelPanel() {
		const requestId = ++attentionFunnelRequestId;
		attentionFunnelLoading = true;
		try {
			const snapshot = await fetchAttentionFunnelObservability(ATTENTION_LOOKBACK_HOURS);
			if (requestId === attentionFunnelRequestId) {
				attentionFunnel = snapshot;
				attentionFunnelError = null;
			}
		} catch (error) {
			if (requestId === attentionFunnelRequestId) {
				attentionFunnel = null;
				attentionFunnelError =
					error instanceof Error ? error.message : 'Attention funnel request failed';
			}
		} finally {
			if (requestId === attentionFunnelRequestId) attentionFunnelLoading = false;
		}
	}

	async function loadResurfacingObservabilityPanel() {
		const requestId = ++resurfacingObservabilityRequestId;
		const requestScopeKey = resurfacingScopeKey;
		resurfacingObservabilityLoading = true;
		try {
			const snapshot = await fetchResurfacingObservability();
			if (requestId !== resurfacingObservabilityRequestId || requestScopeKey !== resurfacingScopeKey) return;
			resurfacingObservability = snapshot;
			resurfacingObservabilityError = snapshot ? null : 'Worth a look metrics failed to load';
		} finally {
			if (requestId === resurfacingObservabilityRequestId && requestScopeKey === resurfacingScopeKey) {
				resurfacingObservabilityLoading = false;
			}
		}
	}

	async function loadLlmUsage() {
		const requestId = ++llmUsageRequestId;
		llmUsageLoading = true;
		try {
			const next = await fetchChannelAssistLlmUsage();
			if (requestId === llmUsageRequestId) llm = next;
		} finally {
			if (requestId === llmUsageRequestId) llmUsageLoading = false;
		}
	}

	async function load() {
		await Promise.all([
			loadChannelStats(),
			loadAmbientPanel(),
			loadPublicChatPanel(),
			loadAttentionFunnelPanel(),
			loadResurfacingObservabilityPanel()
		]);
	}

	function refreshAll() {
		void load();
		void loadLlmUsage();
		webSourceObservability?.refresh();
		browserEngineObservability?.refresh();
	}

	// The live distillation feed polls faster than the aggregate — it's a
	// cheap in-memory read and this is the "realtime" surface.
	async function loadFeed() {
		feed = await fetchChannelRecentDistill(12);
	}

	// Prefer real telemetry cost; fall back to the store's ($0 local) figure.
	$: llmCost = llm?.available ? llm.total_cost_usd : (stats?.llm.cost_usd ?? 0);
	$: resurfacingScopeKey = `${$scopeIdentityStore.principal}:${$scopeIdentityStore.workspace}`;
	$: if (statsMounted && resurfacingScopeKey !== resurfacingLoadedScopeKey) {
		resurfacingLoadedScopeKey = resurfacingScopeKey;
		resurfacingObservabilityRequestId += 1;
		resurfacingObservability = null;
		resurfacingObservabilityError = null;
		resurfacingObservabilityLoading = true;
		void loadResurfacingObservabilityPanel();
	}

	onMount(() => {
		statsMounted = true;
		resurfacingLoadedScopeKey = resurfacingScopeKey;
		void load();
		void loadLlmUsage();
		void loadFeed();
		timer = setInterval(() => {
			void loadChannelStats();
			void loadPublicChatPanel();
		}, 15000);
		ambientTimer = setInterval(() => void loadAmbientPanel(), 60000);
		llmTimer = setInterval(() => void loadLlmUsage(), 60000);
		feedTimer = setInterval(() => void loadFeed(), 4000);
		attentionFunnelTimer = setInterval(() => void loadAttentionFunnelPanel(), 60000);
		resurfacingObservabilityTimer = setInterval(() => void loadResurfacingObservabilityPanel(), 60000);
	});
	onDestroy(() => {
		statsMounted = false;
		resurfacingObservabilityRequestId += 1;
		if (timer) clearInterval(timer);
		if (ambientTimer) clearInterval(ambientTimer);
		if (feedTimer) clearInterval(feedTimer);
		if (llmTimer) clearInterval(llmTimer);
		if (attentionFunnelTimer) clearInterval(attentionFunnelTimer);
		if (resurfacingObservabilityTimer) clearInterval(resurfacingObservabilityTimer);
	});

	function pct(n: number, of: number): number {
		return of > 0 ? Math.round((n / of) * 100) : 0;
	}

	function funnelColor(key: string): string {
		return FUNNEL_COLORS[key] ?? 'var(--accent-primary)';
	}

	/** ~8/min, ~0.4/min, or — when we have no sample yet. */
	function rateLabel(perMin: number): string {
		if (!sampled) return 'measuring…';
		if (perMin <= 0) return 'idle';
		if (perMin >= 10) return `~${Math.round(perMin)}/min`;
		return `~${perMin.toFixed(1)}/min`;
	}

	/** Rough ETA to drain the pending queue at the current rate. */
	function etaLabel(pending: number, perMin: number): string {
		if (pending <= 0) return 'caught up';
		if (!sampled || perMin <= 0) return '—';
		const mins = pending / perMin;
		if (mins < 1) return '<1m left';
		if (mins < 90) return `~${Math.round(mins)}m left`;
		return `~${(mins / 60).toFixed(1)}h left`;
	}

	function byteLabel(value: number | null | undefined): string {
		const bytes = value ?? 0;
		if (bytes < 1024) return `${bytes} B`;
		const units = ['KB', 'MB', 'GB', 'TB'];
		let amount = bytes / 1024;
		let idx = 0;
		while (amount >= 1024 && idx < units.length - 1) {
			amount /= 1024;
			idx += 1;
		}
		return `${amount >= 10 ? amount.toFixed(0) : amount.toFixed(1)} ${units[idx]}`;
	}

	function stateText(state: string | null | undefined): string {
		return (state ?? 'unknown').replaceAll('_', ' ');
	}

	function ambientRangeText(range: AmbientStats['range'] | undefined): string {
		if (!range) return ambientRange === 'today' ? 'Today' : ambientRange;
		if (range.from_day === range.to_day) return range.from_day;
		return `${range.from_day} - ${range.to_day}`;
	}

	function setAmbientRange(range: AmbientRange) {
		if (ambientRange === range) return;
		ambientRange = range;
		void loadAmbientPanel(range);
	}

	function orderedCountRows(
		counts: Record<string, number> | null | undefined,
		order: string[] = []
	): { key: string; label: string; count: number }[] {
		const raw = counts ?? {};
		return [
			...order.filter((key) => key in raw),
			...Object.keys(raw).filter((key) => !order.includes(key)).sort()
		].map((key) => ({ key, label: attentionLabel(key), count: raw[key] ?? 0 }));
	}

	// Funnel stages, each as a share of synced messages.
	$: funnel = stats
		? [
				{ key: 'synced', label: 'Synced', value: stats.funnel.synced },
				{ key: 'distilled', label: 'Summarized', value: stats.funnel.distilled },
				{ key: 'classified', label: 'Classified', value: stats.funnel.classified },
				{ key: 'needs_approval', label: 'Needs you', value: stats.funnel.needs_approval }
			]
		: [];
	$: syncedTotal = stats?.funnel.synced ?? 0;
	$: pipelineFunnelRows = funnel.map((stage) => ({
		...stage,
		share: pct(stage.value, syncedTotal),
		color: funnelColor(stage.key)
	}));

	$: laneEntries = stats ? Object.entries(stats.totals.by_lane) : [];
	$: providerEntries = stats ? Object.entries(stats.totals.by_provider) : [];
	$: ambientTopOrigins = ambientStats
		? Object.entries(ambientStats.page_metrics.top_origins ?? {})
				.sort((a, b) => b[1] - a[1])
				.slice(0, 8)
		: [];
	$: ambientContentTypes = ambientStats
		? Object.entries(ambientStats.types.content_type ?? {})
				.sort((a, b) => b[1] - a[1])
				.slice(0, 6)
		: [];
	$: ambientEventKinds = ambientStats
		? Object.entries(ambientStats.types.event_kind ?? {})
				.sort((a, b) => b[1] - a[1])
				.slice(0, 8)
		: [];
	$: ambientSensitivity = ambientStats
		? Object.entries(ambientStats.types.sensitivity ?? {})
				.sort((a, b) => b[1] - a[1])
				.slice(0, 6)
		: [];
	$: ambientRejectReasons = ambientStats
		? Object.entries(ambientStats.ingestion_funnel.rejections_by_reason ?? {})
				.sort((a, b) => b[1] - a[1])
				.slice(0, 6)
		: [];
	$: ambientReceived = ambientStats?.ingestion_funnel.received ?? 0;
	$: ambientIngestRows = ambientStats
		? [
				{ label: 'Received', value: ambientStats.ingestion_funnel.received },
				{ label: 'Accepted', value: ambientStats.ingestion_funnel.accepted },
				{ label: 'Duplicates', value: ambientStats.ingestion_funnel.duplicates },
				{ label: 'Rejected', value: ambientStats.ingestion_funnel.rejected }
			]
		: [];
	$: ambientRecentSignals = ambientStats?.recent_signals ?? [];
	$: ambientRecentRuns = ambientStats?.recent_runs ?? [];
	$: ambientRecentClusters = ambientStats?.recent_clusters ?? [];
	$: ambientRangeLabel = ambientRangeText(ambientStats?.range);
	$: attentionStageRows = orderedCountRows(attentionFunnel?.by_stage, ATTENTION_STAGE_ORDER);
	$: attentionSourceFamilyRows = orderedCountRows(
		attentionFunnel?.by_source_family,
		ATTENTION_SOURCE_FAMILY_ORDER
	);
	$: attentionLaneRows = orderedCountRows(attentionFunnel?.by_lane, ATTENTION_LANE_ORDER);
	$: attentionDropRows = orderedCountRows(attentionFunnel?.by_drop_reason);
	$: attentionStageMax = attentionStageRows.reduce((m, r) => Math.max(m, r.count), 0);
	$: attentionSourceFamilyMax = attentionSourceFamilyRows.reduce(
		(m, r) => Math.max(m, r.count),
		0
	);
	$: attentionLaneMax = attentionLaneRows.reduce((m, r) => Math.max(m, r.count), 0);
	$: attentionDropMax = attentionDropRows.reduce((m, r) => Math.max(m, r.count), 0);

	// ── Live distillation progress ──
	$: distillQueue = distillQueueView(stats, status);
	$: pendingDistill = stats?.distill.by_state?.pending ?? status?.pending_distill ?? stats?.distill.pending ?? 0;
	$: distillDone = stats?.distill.done ?? 0;
	$: distillSkipped = stats?.distill.skipped ?? stats?.distill.by_state?.skipped ?? 0;
	$: distillSuppressed = stats?.distill.suppressed ?? stats?.distill.by_state?.suppressed ?? 0;
	$: distillFailed = stats?.distill.by_state?.failed ?? 0;
	$: distillOther = stats
		? Object.entries(stats.distill.by_state ?? {})
				.filter(([key]) => !DISTILL_STATE_KEYS.includes(key))
				.reduce((sum, [, count]) => sum + count, 0)
		: 0;
	$: distillAccounted =
		distillDone + pendingDistill + distillSkipped + distillSuppressed + distillFailed + distillOther + distillQueue.expired;
	$: distillTotal = Math.max(stats?.totals.messages ?? 0, distillAccounted);
	$: distillUnaccounted = Math.max(0, distillTotal - distillAccounted);
	$: distillActive = distillQueue.queued > 0;
	$: distillProcessable = distillDone + distillQueue.queued;
	$: distillStateSegments = [
		{ key: 'done', label: 'Done summaries', value: distillDone, color: funnelColor('done') },
		{ key: 'pending', label: 'Pending', value: pendingDistill, color: funnelColor('pending') },
		{ key: 'expired', label: 'Expired history work', value: distillQueue.expired, color: funnelColor('skipped') },
		{
			key: 'skipped',
			label: 'Skipped / coalesced',
			value: distillSkipped,
			color: funnelColor('skipped')
		},
		{
			key: 'suppressed',
			label: 'Suppressed',
			value: distillSuppressed,
			color: funnelColor('suppressed')
		},
		{ key: 'failed', label: 'Failed', value: distillFailed, color: funnelColor('failed') },
		...(distillOther > 0
			? [{ key: 'other', label: 'Other states', value: distillOther, color: funnelColor('other') }]
			: []),
		...(distillUnaccounted > 0
			? [
					{
						key: 'unaccounted',
						label: 'No distill state',
						value: distillUnaccounted,
						color: funnelColor('unaccounted')
					}
				]
			: [])
	];

	// ── Classification completeness ──
	// The by_label histogram only counts CLASSIFIED threads; the rest are still
	// queued (pending_classify) or not yet distilled. Account for all of them so
	// the section reconciles with the thread total.
	const CLASSIFY_LABELS = ['needs_reply', 'follow_up', 'fyi', 'no_action'] as const;
	$: classifyByLabel = stats?.classify.by_label ?? {};
	$: classifiedTotal = Object.values(classifyByLabel).reduce((a, v) => a + v, 0);
	$: pendingClassify = status?.pending_classify ?? 0;
	$: retryingClassify = stats?.classify.retrying ?? 0;
	$: failedClassify = stats?.classify.failed ?? 0;
	$: totalThreads = stats?.totals.threads ?? 0;
	// Anything neither classified nor in the classify queue hasn't been distilled
	// far enough to classify yet.
	$: notYetDistilled = Math.max(
		0,
		totalThreads - classifiedTotal - pendingClassify - retryingClassify - failedClassify
	);
	$: classificationCoverageSegments = [
		{
			key: 'classified',
			label: 'Classified',
			value: classifiedTotal,
			color: 'var(--observe-green, var(--color-success))'
		},
		{
			key: 'pending',
			label: 'Awaiting classification',
			value: pendingClassify,
			color: 'color-mix(in srgb, var(--accent-primary) 62%, transparent)'
		},
		{
			key: 'retrying',
			label: 'Retry backoff',
			value: retryingClassify,
			color: 'var(--color-warning)'
		},
		{
			key: 'failed',
			label: 'Failed',
			value: failedClassify,
			color: 'var(--color-danger, var(--color-error, #b42318))'
		},
		{
			key: 'undistilled',
			label: 'Not yet distilled',
			value: notYetDistilled,
			color: 'var(--border-default)'
		}
	];
	$: classificationLabelSegments = CLASSIFY_LABELS.map((label) => ({
		key: label,
		label: labelText(label),
		value: classifyByLabel[label] ?? 0,
		color:
			label === 'needs_reply'
				? 'var(--color-warning)'
				: label === 'follow_up'
					? 'var(--observe-violet, var(--accent-secondary, var(--accent-primary)))'
					: label === 'fyi'
						? 'var(--observe-blue, var(--accent-primary))'
						: 'var(--text-muted)'
	}));
	$: classifySummary = `${compact(classifiedTotal)} of ${compact(totalThreads)} threads classified · ${compact(pendingClassify)} awaiting · ${compact(retryingClassify)} retry backoff · ${compact(failedClassify)} failed`;
	$: followUpSourceFamilies = stats?.classify.needs_approval_by_source_family ?? {};
	$: followUpSourceRows = [
		{ key: 'promise', label: 'Promise / obligation', count: followUpSourceFamilies.promise ?? 0 },
		{
			key: 'comms_ingest',
			label: 'Comms reply/action',
			count: followUpSourceFamilies.comms_ingest ?? 0
		},
		...Object.entries(followUpSourceFamilies)
			.filter(([key]) => key !== 'promise' && key !== 'comms_ingest')
			.map(([key, count]) => ({ key, label: labelText(key), count }))
	];
	$: followUpSourceTotal = followUpSourceRows.reduce((sum, row) => sum + row.count, 0);
	$: followUpSourceSegments = followUpSourceRows.map((row) => ({
		key: row.key,
		label: row.label,
		value: row.count,
		color:
			row.key === 'promise'
				? 'var(--observe-violet, var(--accent-secondary, var(--accent-primary)))'
				: row.key === 'comms_ingest'
					? 'var(--color-warning)'
					: 'var(--observe-blue, var(--accent-primary))'
	}));
	$: runtimeHistoryDays = stats?.runtime?.history_lookback_days ?? status?.history_lookback_days;
	$: distillRuntime = stats?.runtime?.distill ?? status?.workers?.distill;
	$: classifyRuntime = stats?.runtime?.classify ?? status?.workers?.classify;
	$: runtimeLines = [
		distillRuntime ? `Distill: c${distillRuntime.concurrency}/b${distillRuntime.batch}` : null,
		classifyRuntime ? `Classify: c${classifyRuntime.concurrency}/b${classifyRuntime.batch}` : null,
		runtimeHistoryDays ? `History: ${runtimeHistoryDays}d` : null,
		distillRuntime ? `Coalescing: ${distillRuntime.coalesce_threads ? 'On' : 'Off'}` : null
	].filter(Boolean);
</script>

<svelte:head><title>Observe pipeline stats</title></svelte:head>

{#snippet attentionSkeleton()}
	<div class="channel-skeleton" aria-busy="true" aria-label="Loading attention routing stats">
		<section class="surface-card panel attention-panel">
			<h2>
				<span>Attention routing</span>
				<span class="skeleton skeleton-copy short" style="display:inline-block; margin-bottom:0; width:5rem;"></span>
			</h2>
			<div class="attention-metrics">
				{#each SKELETON_BARS as _, index}
					<div class="attention-metric">
						<span class="skeleton skeleton-label"></span>
						<strong class="skeleton skeleton-metric-value"></strong>
					</div>
				{/each}
			</div>
			<div class="attention-columns">
				{#each SKELETON_BARS as _, index}
					<div class="attention-column">
						<p class="attention-column-title"><span class="skeleton skeleton-label" style="display:inline-block"></span></p>
						{#each SKELETON_BARS.slice(0, 3) as _, i}
							<div class="funnel-row attention-row skeleton-bar-row">
								<span class="skeleton skeleton-funnel-label"></span>
								<span class="skeleton skeleton-bar-track" style={`--skeleton-width: ${Math.max(20, 90 - i * 30)}%;`}></span>
								<span class="skeleton skeleton-funnel-value"></span>
							</div>
						{/each}
					</div>
				{/each}
			</div>
		</section>
	</div>
{/snippet}

{#snippet channelAssistSkeleton()}
	<div class="channel-skeleton" aria-busy="true" aria-label="Loading mail and chat pipeline stats">

		<section class="metric-grid">
			{#each SKELETON_METRICS as _, index}
				<div class="metric skeleton-card">
					<span class="skeleton skeleton-metric-value"></span>
					<span class="skeleton skeleton-label"></span>
				</div>
			{/each}
		</section>

		<section class="engine-band skeleton-band">
			{#each SKELETON_ENGINE as _, index}
				<div class="engine-chip engine-chip--status skeleton-engine">
					<span class="skeleton skeleton-role"></span>
					<div class="engine-row">
						<span class="skeleton skeleton-row-label"></span>
						<span class="skeleton skeleton-engine-pill"></span>
					</div>
					<div class="engine-row">
						<span class="skeleton skeleton-row-label short"></span>
						<span class="skeleton skeleton-engine-pill wide"></span>
					</div>
				</div>
			{/each}
		</section>

		<section class="surface-card panel">
			<h2>Pipeline funnel</h2>
			<span class="skeleton skeleton-copy"></span>
			<div class="skeleton-funnel">
				{#each SKELETON_BARS as _, index}
					<div class="skeleton-funnel-row">
						<span class="skeleton skeleton-funnel-label"></span>
						<span
							class="skeleton skeleton-funnel-band"
							style={`--skeleton-width: ${Math.max(8, 100 - index * 24)}%;`}
						></span>
						<span class="skeleton skeleton-funnel-value"></span>
					</div>
				{/each}
			</div>
		</section>

		<div class="two-col">
			<section class="surface-card panel">
				<h2>Distillation</h2>
				<div class="skeleton-progress">
					<span class="skeleton skeleton-progress-label"></span>
					<span class="skeleton skeleton-progress-rate"></span>
				</div>
				<span class="skeleton skeleton-progress-bar"></span>
				<div class="skeleton-pie-row">
					<span class="skeleton skeleton-pie"></span>
					<div class="skeleton-legend">
						<span class="skeleton skeleton-legend-total"></span>
						{#each SKELETON_BARS as _, index}
							<span class="skeleton skeleton-legend-line"></span>
						{/each}
					</div>
				</div>
			</section>

			<section class="surface-card panel">
				<h2>Classification</h2>
				<span class="skeleton skeleton-copy"></span>
				{#each SKELETON_BARS as _, index}
					<div class="funnel-row skeleton-bar-row">
						<span class="skeleton skeleton-funnel-label"></span>
						<span class="skeleton skeleton-bar-track"></span>
						<span class="skeleton skeleton-funnel-value"></span>
					</div>
				{/each}
				<div class="section-divider"></div>
				<span class="skeleton skeleton-copy short"></span>
				{#each SKELETON_BARS.slice(0, 2) as _, index}
					<div class="funnel-row skeleton-bar-row">
						<span class="skeleton skeleton-funnel-label"></span>
						<span class="skeleton skeleton-bar-track short"></span>
						<span class="skeleton skeleton-funnel-value"></span>
					</div>
				{/each}
			</section>
		</div>

		<section class="surface-card panel">
			<h2>Live distillation</h2>
			<div class="feed">
				{#each SKELETON_FEED as _, index}
					<div class="feed-item skeleton-feed-item">
						<div class="feed-meta">
							<span class="skeleton skeleton-feed-meta"></span>
							<span class="skeleton skeleton-feed-time"></span>
						</div>
						<div class="feed-io">
							<div class="feed-col">
								<span class="skeleton skeleton-feed-tag"></span>
								<div class="feed-body skeleton-feed-body">
									<span class="skeleton skeleton-feed-title"></span>
									<span class="skeleton skeleton-feed-subtitle"></span>
								</div>
							</div>
							<div class="feed-col">
								<span class="skeleton skeleton-feed-tag"></span>
								<div class="feed-body skeleton-feed-body">
									<span class="skeleton skeleton-feed-title wide"></span>
									<span class="skeleton skeleton-feed-subtitle short"></span>
								</div>
							</div>
						</div>
					</div>
				{/each}
			</div>
		</section>

		<div class="two-col">
			<section class="surface-card panel">
				<h2>By lane</h2>
				{#each SKELETON_BARS.slice(0, 3) as _, index}
					<div class="kv">
						<span class="skeleton skeleton-kv-label"></span>
						<span class="skeleton skeleton-kv-value"></span>
					</div>
				{/each}
			</section>

			<section class="surface-card panel">
				<h2>LLM usage</h2>
				{#each SKELETON_BARS as _, index}
					<div class="kv">
						<span class="skeleton skeleton-kv-label wide"></span>
						<span class="skeleton skeleton-kv-value"></span>
					</div>
				{/each}
			</section>
		</div>

		<section class="surface-card panel">
			<h2>Accounts</h2>
			<div class="acct-table">
				{#each SKELETON_ROWS as _, index}
					<div class="acct-row skeleton-account-row">
						<span class="skeleton skeleton-account-name"></span>
						<span class="skeleton skeleton-account-badge"></span>
						<span class="skeleton skeleton-account-num"></span>
						<span class="skeleton skeleton-account-num"></span>
						<span class="skeleton skeleton-account-time"></span>
					</div>
				{/each}
			</div>
		</section>
	</div>
{/snippet}

<div class="stats-page">
	<header class="stats-head">
		<div class="stats-title-group">
			<p class="stats-kicker">Observation</p>
			<div class="stats-title-row">
				<h1>Pipelines &amp; Activity</h1>
				<p class="stats-subtitle">Ingest stages, distillation funnels, attention routing, and LLM telemetry</p>
			</div>
		</div>
		<div class="stats-actions">
			<a
				class="action-button action-button--outline action-button--sm"
				href="/observe"
				title="Back to Observe console"
			>
				<Icon name="chevron-left" size={13} />
				<span>Observe</span>
			</a>
			<a
				class="action-button action-button--outline action-button--sm"
				href="/resurfacing"
				title="View proactive resurfacing engine status and queues"
			>
				<Icon name="rotate-ccw" size={13} />
				<span>Resurfacing</span>
			</a>
			<button
				type="button"
				class="action-button action-button--outline action-button--sm"
				on:click={refreshAll}
				title="Refresh all pipeline metrics"
				aria-label="Refresh pipeline metrics"
			>
				<Icon name="rotate-ccw" size={13} class={loading ? 'spinning' : ''} />
				<span>{loading ? 'Refreshing…' : 'Refresh'}</span>
			</button>
		</div>
	</header>

	<ObservableSourceObservability bind:this={webSourceObservability} />
	<BrowserEngineObservability bind:this={browserEngineObservability} />

	{#if loading && !stats && !ambientStats && !publicContacts && !publicChatStatus && !attentionFunnel && !attentionFunnelError}
		{@render attentionSkeleton()}
		{@render channelAssistSkeleton()}
	{:else if !stats && !ambientStats && !publicContacts && !publicChatStatus && !attentionFunnel && !attentionFunnelError}
		<p class="muted">
			No stats — the observe APIs aren't responding (is the backend running with this build?).
		</p>
	{:else}
		{#if attentionFunnel}
			<section class="surface-card panel attention-panel">
				<h2>
					<span>Attention routing</span>
					<span class="badge badge-scope">last {ATTENTION_LOOKBACK_DAYS}d</span>
				</h2>
				<div class="attention-metrics">
					<div class="attention-metric">
						<span>Events</span><strong>{compact(attentionFunnel.total_events)}</strong>
					</div>
					<div class="attention-metric">
						<span>Routed</span><strong>{compact(attentionFunnel.routed_events)}</strong>
					</div>
					<div class="attention-metric">
						<span>Dropped</span><strong>{compact(attentionFunnel.dropped_events)}</strong>
					</div>
					<div class="attention-metric">
						<span>Recent</span><strong>{compact(attentionFunnel.recent_events.length)}</strong>
					</div>
				</div>

				{#if attentionFunnel.total_events === 0}
					<p class="muted small">No route events recorded for this scope.</p>
				{:else}
					<div class="attention-columns">
						<div class="attention-column">
							<p class="attention-column-title">Stages</p>
							{#each attentionStageRows as row}
								<div class="funnel-row attention-row">
									<span class="funnel-label">{row.label}</span>
									<div class="bar-track">
										<div class="bar-fill bar-distilled" style="width: {pct(row.count, attentionStageMax)}%"></div>
									</div>
									<span class="funnel-value">{compact(row.count)}</span>
								</div>
							{/each}
						</div>
						<div class="attention-column">
							<p class="attention-column-title">Source family</p>
							{#each attentionSourceFamilyRows as row}
								<div class="funnel-row attention-row">
									<span class="funnel-label">{row.label}</span>
									<div class="bar-track">
										<div class="bar-fill bar-classified" style="width: {pct(row.count, attentionSourceFamilyMax)}%"></div>
									</div>
									<span class="funnel-value">{compact(row.count)}</span>
								</div>
							{/each}
						</div>
						<div class="attention-column">
							<p class="attention-column-title">Lanes</p>
							{#each attentionLaneRows as row}
								<div class="funnel-row attention-row">
									<span class="funnel-label">{row.label}</span>
									<div class="bar-track">
										<div class="bar-fill bar-needs_approval" style="width: {pct(row.count, attentionLaneMax)}%"></div>
									</div>
									<span class="funnel-value">{compact(row.count)}</span>
								</div>
							{/each}
						</div>
						<div class="attention-column">
							<p class="attention-column-title">Drops</p>
							{#if attentionDropRows.length === 0}
								<p class="muted small">No drops.</p>
							{:else}
								{#each attentionDropRows as row}
									<div class="funnel-row attention-row">
										<span class="funnel-label">{row.label}</span>
										<div class="bar-track">
											<div class="bar-fill bar-failed" style="width: {pct(row.count, attentionDropMax)}%"></div>
										</div>
										<span class="funnel-value">{compact(row.count)}</span>
									</div>
								{/each}
							{/if}
						</div>
					</div>
				{/if}
				</section>
			{:else if attentionFunnelError}
				<section class="surface-card panel attention-panel">
					<h2>
						<span>Attention routing</span>
						<span class="badge badge-scope">last {ATTENTION_LOOKBACK_DAYS}d</span>
					</h2>
					<p class="muted small">Attention funnel stats failed to load: {attentionFunnelError}</p>
				</section>
			{:else if attentionFunnelLoading}
				{@render attentionSkeleton()}
			{/if}

		{#if stats}
		<!-- Top-line counters -->
		<section class="metric-grid" aria-label="Pipeline overview metrics">
			<div class="metric">
				<span class="metric-value">{compact(stats.totals.messages)}</span>
				<span class="metric-label">messages</span>
			</div>
			<div class="metric">
				<span class="metric-value">{compact(stats.totals.threads)}</span>
				<span class="metric-label">threads</span>
			</div>
			<div class="metric">
				<span class="metric-value">{compact(status?.pending_distill ?? stats.distill.pending)}</span>
				<span class="metric-label">pending distill</span>
			</div>
			<div class="metric">
				<span class="metric-value">{compact(pendingClassify)}</span>
				<span class="metric-label">pending classify</span>
			</div>
			<div class="metric">
				<span class="metric-value">{compact(stats.classify.needs_approval)}</span>
				<span class="metric-label">need you</span>
			</div>
			<div class="metric">
				<span class="metric-value">${llmCost.toFixed(2)}</span>
				<span class="metric-label">LLM cost</span>
			</div>
		</section>

		<!-- Engine + live status (hero). Model/provider are resolved from config
		     (stats.ops), never hardcoded. -->
		<section class="engine-band">
			<div class="engine-chip engine-chip--status">
				<span class="engine-role">Distillation</span>
				<div class="engine-row">
					<span class="engine-row-label">Model</span>
					<span class="engine-model">{bindingLabel(stats.ops?.distill)}</span>
				</div>
				<div class="engine-row">
					<span class="engine-row-label">Processing</span>
					<span class="engine-live">
						{#if distillActive}
							<span class="live-dot" class:pulsing={sampled && distillProcessedRatePerMin > 0}></span>
							<span class="live-text">{sampled && distillProcessedRatePerMin > 0 ? 'distilling' : 'queued'} · {rateLabel(distillProcessedRatePerMin)} processed · {etaLabel(distillQueue.queued, distillProcessedRatePerMin)}</span>
						{:else}
							<span class="live-dot idle"></span>
							<span class="live-text muted">idle · queue empty</span>
						{/if}
					</span>
				</div>
			</div>
			<div class="engine-chip engine-chip--status">
				<span class="engine-role">Classification</span>
				<div class="engine-row">
					<span class="engine-row-label">Model</span>
					<span class="engine-model">{bindingLabel(stats.ops?.classify)}</span>
				</div>
				<div class="engine-row">
					<span class="engine-row-label">Queued</span>
					<span class="engine-live">
						{#if pendingClassify > 0}
							<span class="live-dot pulsing classify"></span>
							<span class="live-text">{compact(pendingClassify)} queued</span>
						{:else}
							<span class="live-dot idle"></span>
							<span class="live-text muted">0 queued</span>
						{/if}
					</span>
				</div>
			</div>
			<div class="engine-chip engine-chip--runtime">
				<span class="engine-role">Runtime</span>
				{#if runtimeLines.length > 0}
					<span class="engine-model engine-model--stack">
						{#each runtimeLines as line}
							<span>{line}</span>
						{/each}
					</span>
				{:else}
					<span class="engine-model">not reported</span>
				{/if}
			</div>
		</section>

		<!-- Funnel -->
		<section class="surface-card panel">
			<h2>Pipeline funnel</h2>
			<p class="muted small">Synced messages narrow into summaries, classifications, and owner-facing items.</p>
			<FunnelChart
				data={pipelineFunnelRows}
				total={syncedTotal}
				ariaLabel="Pipeline funnel"
				formatValue={compact}
				sequential={true}
			/>
		</section>

		<div class="two-col">
			<!-- Distillation -->
			<section class="surface-card panel distillation-panel">
				<h2>Distillation <span class="muted small">{bindingLabel(stats.ops?.distill)}</span></h2>
				<div class="live-progress">
					<div class="live-progress-head">
						<span class="funnel-value">{compact(distillDone)} done · {compact(distillQueue.queued)} queued within history window</span>
						<span class="live-progress-rate" class:active={distillActive}>
							{#if distillActive}
								<span class="live-dot" class:pulsing={sampled && distillProcessedRatePerMin > 0}></span>
							{/if}
							{distillActive
								? `${rateLabel(distillProcessedRatePerMin)} processed · ${rateLabel(distillSummaryRatePerMin)} summaries · ${etaLabel(distillQueue.queued, distillProcessedRatePerMin)}`
								: 'caught up'}
						</span>
					</div>
					<div class="bar-track big">
						<div class="bar-fill bar-distilled" style="width: {pct(distillDone, distillProcessable)}%"></div>
					</div>
				</div>
				{#if distillQueue.expired > 0 || distillQueue.outsideHistory > 0}
					<p class="muted small">{compact(distillQueue.expired)} old queued items discarded outside the history window. Source records and existing summaries are retained.{distillQueue.outsideHistory > 0 ? ` ${compact(distillQueue.outsideHistory)} awaiting retirement.` : ''}</p>
				{/if}
				{#if distillQueue.retryExhausted > 0}
					<p class="muted small">{compact(distillQueue.retryExhausted)} historical failures exhausted their retries and are not queued.</p>
				{/if}
				<div class="distill-state-chart">
					<PieChart
						segments={distillStateSegments}
						size={210}
						innerRatio={0.55}
						showLegend={true}
						showLegendValues={true}
						showTotal={true}
						totalLabel="Total"
						formatValue={compact}
					/>
				</div>
			</section>

			<!-- Classification -->
			<section class="surface-card panel">
				<h2>Classification <span class="muted small">{bindingLabel(stats.ops?.classify)}</span></h2>
				<p class="muted small">{classifySummary}</p>
				<div class="classification-stack-group">
					<div class="classification-stack">
						<p class="classification-stack-title">Coverage</p>
						<StackedBarChart
							segments={classificationCoverageSegments}
							total={totalThreads}
							ariaLabel="Classification coverage"
							formatValue={compact}
						/>
					</div>
					<div class="classification-stack">
						<p class="classification-stack-title">Classified labels</p>
						<StackedBarChart
							segments={classificationLabelSegments}
							total={classifiedTotal}
							ariaLabel="Classified label mix"
							formatValue={compact}
						/>
					</div>
				</div>
				<div class="section-divider"></div>
				<div class="classification-stack">
					<p class="classification-stack-title">Follow-up source family</p>
					<StackedBarChart
						segments={followUpSourceSegments}
						total={followUpSourceTotal}
						ariaLabel="Follow-up source family"
						formatValue={compact}
					/>
				</div>
			</section>
		</div>

		<!-- Live distillation feed: realtime input → output -->
		<section class="surface-card panel">
			<h2>
				Live distillation
				<span class="muted small">input → output · updates every 4s</span>
				{#if distillActive}
					<span class="feed-live"><span class="live-dot pulsing"></span> live</span>
				{/if}
			</h2>
			{#if feed.length === 0}
				<p class="muted small">
					No recent distillations captured yet.{distillActive
						? ' The next completed message will appear here (~30s/msg on local gemma).'
						: ' The queue is idle.'}
				</p>
			{:else}
				<div class="feed">
					{#each feed as e (e.provider + e.message_id + e.at_ms)}
						<div class="feed-item">
							<div class="feed-meta">
								<span class="feed-provider">{providerLabel(e.provider)} · {e.account_alias}</span>
								<span class="feed-time">
									{#if e.received_at}<span class="feed-received">{localDateTime(e.received_at)}</span>{/if}
									{#if e.latency_ms}<span class="feed-latency">{latencyLabel(e.latency_ms)}</span>{/if}
								</span>
							</div>
							<div class="feed-io">
								<div class="feed-col">
									<span class="feed-tag">IN</span>
									<div class="feed-body">
										<div class="feed-subject">{e.subject || '(no subject)'}</div>
										<div class="feed-sender">{senderLabel(e)}</div>
									</div>
								</div>
								<div class="feed-col">
									<span class="feed-tag out">OUT</span>
									<div class="feed-body">
										<div class="feed-summary">{e.summary}</div>
										<div class="feed-intent"><span class="intent-chip">{e.intent}</span></div>
									</div>
								</div>
							</div>
						</div>
					{/each}
				</div>
				<p class="muted small">
					Input shows message metadata only — the raw body is distilled locally and
					never stored. Output is the local model's derived summary + intent.
				</p>
			{/if}
		</section>

		<div class="two-col">
			<!-- Lanes -->
			<section class="surface-card panel">
				<h2>By lane</h2>
				{#each laneEntries as [lane, count]}
					<div class="kv">
						<span class="badge badge-{lane === 'envoy' ? 'presto' : 'you'}">{laneLabel(lane)}</span>
						<strong>{compact(count)} <span class="muted small">msgs</span></strong>
					</div>
				{/each}
			</section>

			<!-- LLM usage -->
			<section class="surface-card panel">
				<h2>LLM usage <span class="muted small">(last 30d)</span></h2>
				{#if llm?.available && llm.total_calls > 0}
					<div class="acct-row acct-header llm-head">
						<span>Operation</span><span class="num">Calls</span><span class="num">In</span><span class="num">Out</span><span class="num">Cost</span>
					</div>
					{#each ['channel_ingest_distill', 'channel_classify'] as op}
						{@const u = llm.by_op[op]}
						<div class="acct-row llm-head">
							<span>{opLabel(op)}</span>
							<span class="num">{compact(u?.calls ?? 0)}</span>
							<span class="num">{compact(u?.input_tokens ?? 0)}</span>
							<span class="num">{compact(u?.output_tokens ?? 0)}</span>
							<span class="num">${(u?.cost_usd ?? 0).toFixed(2)}</span>
						</div>
					{/each}
					<div class="kv" style="margin-top:0.4rem"><span>Total cost</span><strong>${llm.total_cost_usd.toFixed(2)}</strong></div>
					<p class="muted small">From analytics telemetry (parquet), tagged per operation — In/Out are prompt/completion tokens. $0 while on a local profile; real cost shows if an op is bound to a remote profile.</p>
				{:else}
					<div class="kv"><span>Distill calls</span><strong>{compact(stats.llm.distill_calls)}</strong></div>
					<div class="kv"><span>Classify calls</span><strong>{compact(stats.llm.classify_calls)}</strong></div>
					<div class="kv"><span>Cost</span><strong>${stats.llm.cost_usd.toFixed(2)}</strong></div>
					<p class="muted small">
						{llmUsageLoading && !llm
							? 'Loading telemetry — showing store-derived counts.'
							: llm && !llm.available
							? 'Telemetry unavailable — showing store-derived counts.'
							: 'No LLM calls recorded yet (derived counts). ' + stats.llm.note}
					</p>
				{/if}
			</section>
		</div>

		<!-- Accounts -->
		<section class="surface-card panel">
			<h2>Accounts</h2>
			<div class="acct-table">
				<div class="acct-row acct-header">
					<span>Account</span><span>Lane</span><span class="num">Threads</span><span class="num">Messages</span><span>Last sync</span>
				</div>
				{#each status?.accounts ?? [] as a}
					<div class="acct-row">
						<span class="acct-name">
							{providerLabel(a.provider)} · {a.account_alias}
							{#if !a.connected}<span class="muted small"> (not connected)</span>{/if}
							{#if a.last_error}<span class="err-dot" title={a.last_error}>●</span>{/if}
						</span>
						<span><span class="badge badge-{a.lane === 'envoy' ? 'presto' : 'you'}">{laneLabel(a.lane)}</span></span>
						<span class="num">{compact(a.thread_count)}</span>
						<span class="num">{compact(a.message_count)}</span>
						<span class="muted small">{relativeTime(a.last_synced_at)}</span>
					</div>
				{/each}
			</div>
		</section>
		{:else if channelStatsLoading}
			{@render channelAssistSkeleton()}
		{:else}
			<section class="surface-card panel">
				<h2>Mail &amp; chat pipeline</h2>
				<p class="muted small">Mail stats are unavailable. Browser Tabs stats can still render independently below.</p>
			</section>
		{/if}

		{#if resurfacingObservabilityLoading && !resurfacingObservability}
			<section class="surface-card panel rollout-skeleton" aria-label="Loading Worth a look rollout metrics">
				<div class="skeleton skeleton-title"></div>
				<div class="rollout-metrics">
					{#each SKELETON_BARS as _}<div class="skeleton skeleton-metric"></div>{/each}
				</div>
			</section>
		{:else if resurfacingObservabilityError}
			<section class="surface-card panel">
				<h2>Worth a look rollout</h2>
				<p class="muted small">{resurfacingObservabilityError}</p>
			</section>
		{:else if resurfacingObservability}
			{@const repairRun = resurfacingObservability.pipeline.find((run) => run.kind === 'routing_repair')}
			{@const channelBriefs = stats?.distill.briefs}
			{@const backfill = stats?.runtime?.distill.backfill}
			<section class="section-head">
				<div>
					<h2>Worth a look rollout</h2>
					<p class="muted small">Brief migration, active routing repair, recommendations, and contextual actions.</p>
				</div>
				<div class="section-actions">
					<span class="badge badge-{backfill?.metrics.paused ? 'you' : 'presto'}">
						backfill {backfill?.metrics.paused ? 'paused' : backfill?.enabled ? 'on' : 'off'}
					</span>
				</div>
			</section>
			<div class="two-col rollout-grid">
				<section class="surface-card panel">
					<h2>Brief coverage</h2>
					<div class="rollout-metrics">
						<div class="metric"><span class="metric-value">{compact(channelBriefs?.v2 ?? 0)}</span><span class="metric-label">channel V2</span></div>
						<div class="metric"><span class="metric-value">{compact(channelBriefs?.legacy ?? 0)}</span><span class="metric-label">channel legacy</span></div>
						<div class="metric"><span class="metric-value">{compact(resurfacingObservability.briefs.with_brief)}</span><span class="metric-label">candidate briefs</span></div>
						<div class="metric"><span class="metric-value">{compact(resurfacingObservability.briefs.legacy)}</span><span class="metric-label">candidate legacy</span></div>
					</div>
					<div class="section-divider"></div>
					<div class="kv"><span>Complete</span><strong>{compact(channelBriefs?.complete ?? 0)}</strong></div>
					<div class="kv"><span>Partial</span><strong>{compact(channelBriefs?.partial ?? 0)}</strong></div>
					<div class="kv"><span>Source omits details</span><strong>{compact(channelBriefs?.source_omits_details ?? 0)}</strong></div>
					<div class="kv"><span>Surfaced comm cards</span><strong>{compact(resurfacingObservability.briefs.comm_surfaced)}</strong></div>
				</section>

				<section class="surface-card panel">
					<h2>Migration &amp; routing repair</h2>
					<div class="rollout-metrics">
						<div class="metric"><span class="metric-value">{compact(backfill?.backlog.ready ?? 0)}</span><span class="metric-label">ready</span></div>
						<div class="metric"><span class="metric-value">{compact(backfill?.backlog.cooling ?? 0)}</span><span class="metric-label">cooling</span></div>
						<div class="metric"><span class="metric-value">{compact(backfill?.metrics.distilled ?? 0)}</span><span class="metric-label">repaired briefs</span></div>
						<div class="metric"><span class="metric-value">{compact(resurfacingObservability.sizes.routing_repairs)}</span><span class="metric-label">route receipts</span></div>
					</div>
					<div class="section-divider"></div>
					<div class="kv"><span>Repair passes</span><strong>{compact(repairRun?.total ?? 0)}</strong></div>
					<div class="kv"><span>Accepted in Worth</span><strong>{compact(resurfacingObservability.routing_repair.by_outcome.accepted_worth_a_look ?? 0)}</strong></div>
					<div class="kv"><span>Rerouted / withheld</span><strong>{compact(resurfacingObservability.routing_repair.by_outcome.rerouted_or_withheld ?? 0)}</strong></div>
					<div class="kv"><span>Repair failures</span><strong class:danger-text={(repairRun?.failures ?? 0) > 0}>{compact(repairRun?.failures ?? 0)}</strong></div>
					<div class="kv"><span>Backfill failures</span><strong class:danger-text={(backfill?.metrics.failed ?? 0) > 0}>{compact(backfill?.metrics.failed ?? 0)}</strong></div>
					<div class="kv"><span>Pressure yields</span><strong>{compact((backfill?.metrics.yielded_pending ?? 0) + (backfill?.metrics.yielded_dispatch_pressure ?? 0))}</strong></div>
				</section>

				<section class="surface-card panel">
					<h2>Recommendations</h2>
					<div class="rollout-metrics">
						<div class="metric"><span class="metric-value">{compact(resurfacingObservability.recommendations.shown)}</span><span class="metric-label">shown</span></div>
						<div class="metric"><span class="metric-value">{compact(resurfacingObservability.recommendations.selected)}</span><span class="metric-label">selected</span></div>
						<div class="metric"><span class="metric-value">{pct(resurfacingObservability.recommendations.selected, resurfacingObservability.recommendations.shown)}%</span><span class="metric-label">acceptance</span></div>
						<div class="metric"><span class="metric-value">{pct(resurfacingObservability.recommendations.completed, resurfacingObservability.recommendations.selected)}%</span><span class="metric-label">completion</span></div>
					</div>
				</section>

				<section class="surface-card panel">
					<h2>Contextual actions</h2>
					<div class="rollout-metrics">
						<div class="metric"><span class="metric-value">{compact(resurfacingObservability.actions.started)}</span><span class="metric-label">started</span></div>
						<div class="metric"><span class="metric-value">{compact(resurfacingObservability.actions.completed)}</span><span class="metric-label">completed</span></div>
						<div class="metric"><span class="metric-value">{pct(resurfacingObservability.actions.completed, resurfacingObservability.actions.started)}%</span><span class="metric-label">conversion</span></div>
						<div class="metric"><span class="metric-value danger-text">{compact(resurfacingObservability.actions.failed)}</span><span class="metric-label">failed</span></div>
					</div>
					{#each Object.entries(resurfacingObservability.actions.errors).slice(0, 4) as [errorClass, count]}
						<div class="kv"><span>{stateText(errorClass)}</span><strong>{compact(count)}</strong></div>
					{/each}
				</section>
			</div>
		{/if}

		{#if publicContacts || publicChatStatus}
			<section class="section-head">
				<div>
					<h2>Public chat contacts</h2>
					<p class="muted small">
						{compact(publicContacts?.total_profiles ?? 0)} profiles · {compact(publicContacts?.counts.owner_review_priority ?? 0)} owner review
						{#if publicChatStatus?.enabled}
							· queue {compact(publicChatStatus.queue.queued)}
						{/if}
					</p>
				</div>
				<div class="section-actions">
					{#if publicChatStatus?.source_surfaces?.length}
						{#each publicChatStatus.source_surfaces as surface}
							<span class="badge badge-presto">{surface}</span>
						{/each}
					{:else}
						<span class="badge badge-presto">{publicChatStatus?.source_surface ?? 'public-chat'}</span>
					{/if}
				</div>
			</section>

			<section class="metric-grid">
				<div class="metric">
					<span class="metric-value">{compact(publicContacts?.counts.total ?? 0)}</span>
					<span class="metric-label">contacts</span>
				</div>
				<div class="metric">
					<span class="metric-value">{compact(publicContacts?.counts.owner_review_priority ?? 0)}</span>
					<span class="metric-label">owner review</span>
				</div>
				<div class="metric">
					<span class="metric-value">{compact(publicContacts?.counts.missing_identity ?? 0)}</span>
					<span class="metric-label">missing identity</span>
				</div>
				<div class="metric">
					<span class="metric-value">{compact(publicContacts?.counts.research.pending ?? publicChatStatus?.identity_research?.queued ?? 0)}</span>
					<span class="metric-label">research pending</span>
				</div>
				<div class="metric">
					<span class="metric-value">{compact(publicChatStatus?.queue.queued ?? 0)}</span>
					<span class="metric-label">chat queued</span>
				</div>
				<div class="metric">
					<span class="metric-value">{publicChatStatus?.admission.fallback_reason ? 'on' : 'off'}</span>
					<span class="metric-label">fallback</span>
				</div>
			</section>

			<div class="two-col">
				{#if publicChatStatus?.enabled}
					<section class="surface-card panel">
						<h2>Admission budget</h2>
						<div class="kv"><span>Paid calls</span><strong>{compact(publicChatStatus.admission.paid_calls)} / {compact(publicChatStatus.admission.paid_calls_limit)}</strong></div>
						<div class="kv"><span>Paid tokens</span><strong>{compact(publicChatStatus.admission.paid_tokens)} / {compact(publicChatStatus.admission.paid_tokens_limit)}</strong></div>
						<div class="kv"><span>Paid cost</span><strong>{publicChatStatus.admission.paid_cost_usd === null ? 'Not reported' : `$${publicChatStatus.admission.paid_cost_usd.toFixed(2)}`} / ${publicChatStatus.admission.paid_cost_limit_usd.toFixed(2)}</strong></div>
						<div class="kv"><span>Paid capacity</span><strong>{compact(publicChatStatus.admission.paid_in_use)} / {compact(publicChatStatus.admission.max_paid_concurrent)}</strong></div>
						<div class="kv"><span>Fallback capacity</span><strong>{compact(publicChatStatus.admission.fallback_in_use)} / {compact(publicChatStatus.admission.max_fallback_concurrent)}</strong></div>
						<div class="kv"><span>Queue</span><strong>{compact(publicChatStatus.queue.queued)} / {compact(publicChatStatus.queue.max_global_queue_depth)}</strong></div>
					</section>
				{/if}

				{#if publicContacts}
					<section class="surface-card panel">
						<h2>Research lifecycle</h2>
						<div class="kv"><span>Eligible</span><strong>{compact(publicContacts.counts.research.eligible)}</strong></div>
						<div class="kv"><span>Queued</span><strong>{compact(publicContacts.counts.research.queued)}</strong></div>
						<div class="kv"><span>Active</span><strong>{compact(publicContacts.counts.research.active)}</strong></div>
						<div class="kv"><span>Completed</span><strong>{compact(publicContacts.counts.research.completed)}</strong></div>
						<div class="kv"><span>Skipped</span><strong>{compact(publicContacts.counts.research.skipped)}</strong></div>
						<div class="kv"><span>Not eligible</span><strong>{compact(publicContacts.counts.research.not_eligible)}</strong></div>
					</section>

					<section class="surface-card panel">
						<h2>Newest profiles</h2>
						{#if publicContacts.profiles.length === 0}
							<p class="muted small">No public-contact profiles yet.</p>
						{:else}
							<div class="contact-table">
								<div class="contact-row contact-header"><span>Contact</span><span>Status</span><span>Last seen</span></div>
								{#each publicContacts.profiles as profile}
									<div class="contact-row">
										<span class="contact-main">
											<span class="truncate" title={contactName(profile)}>{contactName(profile)}</span>
											<span class="muted small truncate" title={contactDetail(profile)}>{contactDetail(profile)}</span>
										</span>
										<span>
											<span class="badge badge-{profile.owner_review_priority ? 'presto' : 'you'}">{researchStatusText(profile.research_status)}</span>
											{#if profile.research_result?.excerpt}
												<span class="muted small"> · result</span>
											{/if}
										</span>
										<span class="muted small">{relativeTime(profile.last_seen_at)}</span>
									</div>
								{/each}
							</div>
						{/if}
					</section>
				{/if}
			</div>
		{/if}

		{#if ambientStats}
			<section class="section-head">
				<div>
					<h2>Browser tabs</h2>
					<p class="muted small">
						{ambientRangeLabel}
						{#if ambientStats.range?.days_with_data !== undefined}
							· {compact(ambientStats.range.days_with_data ?? 0)} data day{(ambientStats.range.days_with_data ?? 0) === 1 ? '' : 's'}
						{/if}
						{#if ambientStats.range?.compacted_days}
							· {compact(ambientStats.range.compacted_days)} compacted
						{/if}
					</p>
				</div>
				<div class="section-actions">
					<div class="segmented" aria-label="Browser tabs stats range">
						{#each ambientRangeOptions as option}
							<button
								type="button"
								class:active={ambientRange === option.key}
								on:click={() => setAmbientRange(option.key)}
							>
								{option.label}
							</button>
						{/each}
					</div>
					<span class="badge badge-you">ambient_distill</span>
				</div>
			</section>

			<section class="metric-grid">
				<div class="metric">
					<span class="metric-value">{compact(ambientStats.summary.signals)}</span>
					<span class="metric-label">signals</span>
				</div>
				<div class="metric">
					<span class="metric-value">{compact(ambientStats.summary.distinct_pages)}</span>
					<span class="metric-label">distinct pages</span>
				</div>
				<div class="metric">
					<span class="metric-value">{compact(ambientStats.summary.origins)}</span>
					<span class="metric-label">origins</span>
				</div>
				<div class="metric">
					<span class="metric-value">{byteLabel(ambientStats.summary.bytes.signal_metadata_bytes)}</span>
					<span class="metric-label">retained metadata</span>
				</div>
				<div class="metric">
					<span class="metric-value">${ambientStats.llm.cost_usd.toFixed(2)}</span>
					<span class="metric-label">LLM cost</span>
				</div>
			</section>

			<div class="two-col">
				<section class="surface-card panel">
					<h2>Ingestion funnel</h2>
					{#each ambientIngestRows as row}
						<div class="funnel-row">
							<span class="funnel-label">{row.label}</span>
							<div class="bar-track">
								<div class="bar-fill bar-distilled" style="width: {pct(row.value, ambientReceived)}%"></div>
							</div>
							<span class="funnel-value">{compact(row.value)}<span class="muted small"> · {pct(row.value, ambientReceived)}%</span></span>
						</div>
					{/each}
					{#if ambientRejectReasons.length > 0}
						<div class="mini-list">
							{#each ambientRejectReasons as [reason, count]}
								<div class="kv"><span>{stateText(reason)}</span><strong>{compact(count)}</strong></div>
							{/each}
						</div>
					{/if}
				</section>

				<section class="surface-card panel">
					<h2>Bytes</h2>
					<div class="kv"><span>Raw requests</span><strong>{byteLabel(ambientStats.summary.bytes.batch_raw_bytes)}</strong></div>
					<div class="kv"><span>Accepted payloads</span><strong>{byteLabel(ambientStats.summary.bytes.signal_payload_bytes)}</strong></div>
					<div class="kv"><span>Retained metadata</span><strong>{byteLabel(ambientStats.summary.bytes.signal_metadata_bytes)}</strong></div>
					<div class="kv"><span>Estimated DOM</span><strong>{byteLabel(ambientStats.summary.bytes.dom_estimated_bytes)}</strong></div>
					{#if ambientStats.retention?.last_swept_at_ms}
						<div class="kv"><span>Retention sweep</span><strong>{relativeTime(ambientStats.retention.last_swept_at_ms)}</strong></div>
					{/if}
					<p class="muted small">Estimated DOM bytes are measurement only; raw HTML is not stored.</p>
				</section>
			</div>

			<div class="two-col">
				<section class="surface-card panel">
					<h2>Page and type breakdown</h2>
					<div class="kv"><span>Total pages</span><strong>{compact(ambientStats.page_metrics.total_pages)}</strong></div>
					<div class="kv"><span>Distinct pages</span><strong>{compact(ambientStats.page_metrics.distinct_pages)}</strong></div>
					<div class="kv"><span>Origins</span><strong>{compact(ambientStats.page_metrics.origins)}</strong></div>
					<div class="split-lists">
						<div>
							<p class="muted small">Content type</p>
							{#each ambientContentTypes as [label, count]}
								<div class="kv compact-kv"><span>{label}</span><strong>{compact(count)}</strong></div>
							{/each}
						</div>
						<div>
							<p class="muted small">Event kind</p>
							{#each ambientEventKinds as [label, count]}
								<div class="kv compact-kv"><span>{stateText(label)}</span><strong>{compact(count)}</strong></div>
							{/each}
						</div>
					</div>
				</section>

				<section class="surface-card panel">
					<h2>Distill and memory</h2>
					<div class="kv"><span>Runs</span><strong>{compact(ambientStats.distill_pipeline.runs)}</strong></div>
					<div class="kv"><span>Cluster journal rows</span><strong>{compact(ambientStats.distill_pipeline.clusters)}</strong></div>
					<div class="kv"><span>Evidence created</span><strong>{compact(ambientStats.distill_pipeline.evidence_created)}</strong></div>
					<div class="kv"><span>Memory review pending</span><strong>{compact(ambientStats.distill_pipeline.memory_review_pending)}</strong></div>
					<div class="kv"><span>Memory approved</span><strong>{compact(ambientStats.distill_pipeline.memory_approved ?? 0)}</strong></div>
					<div class="kv"><span>Memory indexed</span><strong>{compact(ambientStats.distill_pipeline.memory_indexed ?? 0)}</strong></div>
					<div class="kv"><span>Memory rejected</span><strong>{compact(ambientStats.distill_pipeline.memory_rejected ?? 0)}</strong></div>
					<div class="kv"><span>LLM calls</span><strong>{compact(ambientStats.llm.calls)}</strong></div>
					<div class="kv"><span>Tokens</span><strong>{compact(ambientStats.llm.input_tokens)} in · {compact(ambientStats.llm.output_tokens)} out</strong></div>
					<div class="kv"><span>Cost</span><strong>${ambientStats.llm.cost_usd.toFixed(2)}</strong></div>
					{#if ambientSensitivity.length > 0}
						<p class="muted small">Sensitivity</p>
						{#each ambientSensitivity as [label, count]}
							<div class="kv compact-kv"><span>{stateText(label)}</span><strong>{compact(count)}</strong></div>
						{/each}
					{/if}
				</section>
			</div>

			<section class="surface-card panel">
				<h2>Top origins</h2>
				{#if ambientTopOrigins.length === 0}
					<p class="muted small">No tab signals in this range.</p>
				{:else}
					<div class="origin-grid">
						{#each ambientTopOrigins as [origin, count]}
							<div class="origin-row">
								<span>{origin}</span>
								<strong>{compact(count)}</strong>
							</div>
						{/each}
					</div>
				{/if}
			</section>

			<div class="two-col">
				<section class="surface-card panel">
					<h2>Recent signals</h2>
					{#if ambientRecentSignals.length === 0}
						<p class="muted small">No recent signals in the ledger.</p>
					{:else}
						<div class="ambient-table">
							<div class="ambient-row ambient-header"><span>Page</span><span>Kind</span><span>Bytes</span><span>Seen</span></div>
							{#each ambientRecentSignals.slice(0, 10) as row}
								<div class="ambient-row">
									<span class="truncate" title={(row.origin ?? '') + (row.path ?? '')}>{row.title || row.path || row.origin}</span>
									<span>{stateText(row.event_kind)}</span>
									<span class="num">{byteLabel(row.metadata_bytes)}</span>
									<span class="muted small">{relativeTime(row.ts_ms)}</span>
								</div>
							{/each}
						</div>
					{/if}
				</section>

				<section class="surface-card panel">
					<h2>Recent clusters</h2>
					{#if ambientRecentClusters.length === 0}
						<p class="muted small">No cluster journal rows yet.</p>
					{:else}
						<div class="ambient-table cluster-table">
							<div class="ambient-row ambient-header"><span>Host</span><span>State</span><span>Pages</span><span>Updated</span></div>
							{#each ambientRecentClusters.slice(0, 10) as row}
								<div class="ambient-row">
									<span class="truncate" title={row.cluster_key}>{row.host}</span>
									<span title={row.memory?.candidate_id || row.memory?.blocked_reason || row.error || row.state}>
										{stateText(row.state)}
										{#if row.memory?.candidate_id}
											<span class="muted small"> · {row.memory.review_state ?? 'pending'}</span>
										{/if}
									</span>
									<span class="num">{compact(row.distinct_page_count)}</span>
									<span class="muted small">{relativeTime(row.updated_at_ms)}</span>
								</div>
							{/each}
						</div>
					{/if}
				</section>
			</div>

			<section class="surface-card panel">
				<h2>Recent distill runs</h2>
				{#if ambientRecentRuns.length === 0}
					<p class="muted small">No distill runs recorded yet.</p>
				{:else}
					<div class="ambient-run-table">
						<div class="ambient-run-row ambient-header"><span>Run</span><span>Source</span><span>Status</span><span>Signals</span><span>Clusters</span><span>LLM</span><span>Memory</span><span>Started</span></div>
						{#each ambientRecentRuns.slice(0, 8) as row}
							<div class="ambient-run-row">
								<span class="truncate" title={row.run_id}>{row.run_id}</span>
								<span>{row.source}</span>
								<span>{stateText(row.status)}</span>
								<span class="num">{compact(row.signals_considered)}</span>
								<span class="num">{compact(row.clusters_promotable)} / {compact(row.clusters_total)}</span>
								<span class="num">{compact(row.llm_succeeded)}</span>
								<span class="num">{compact(row.review_pending)}</span>
								<span class="muted small">{relativeTime(row.started_at_ms)}</span>
							</div>
						{/each}
					</div>
				{/if}
			</section>
		{:else if ambientStatsLoading}
			<section class="surface-card panel">
				<h2>Browser tabs</h2>
				<p class="muted small">Loading browser tab stats…</p>
			</section>
		{:else}
			<section class="surface-card panel">
				<h2>Browser tabs</h2>
				<p class="muted small">Browser tab stats are unavailable. The backend may not include `/ambient/stats` yet.</p>
			</section>
		{/if}
	{/if}
</div>

<style>
	.stats-page {
		--observe-red: var(--accent-primary);
		--observe-green: var(--color-success);
		--observe-blue: var(--color-info);
		--observe-violet: var(--accent-secondary);

		width: 100%;
		max-width: var(--app-content-max, 1320px);
		margin: 0 auto;
		padding: 1.25rem 1.25rem 2rem;
		display: flex;
		flex-direction: column;
		gap: 1rem;
		color: var(--text-primary);
		font-family: var(--font-primary);
		box-sizing: border-box;
		min-width: 0;
		overflow-x: hidden;
	}
	.stats-head {
		display: flex;
		align-items: flex-end;
		justify-content: space-between;
		gap: 1rem;
		min-width: 0;
	}
	.stats-title-group {
		display: flex;
		flex-direction: column;
		min-width: 0;
	}
	.stats-kicker {
		margin: 0;
		color: var(--accent-primary);
		font-size: 0.72rem;
		font-weight: 700;
		letter-spacing: 0.06em;
		text-transform: uppercase;
	}
	.stats-title-row {
		display: flex;
		flex-wrap: wrap;
		align-items: baseline;
		gap: 0.35rem 0.75rem;
		min-width: 0;
	}
	.stats-title-row h1 {
		margin: 0;
		font-family: var(--font-display, var(--font-primary));
		font-size: clamp(1.55rem, 2vw, 2rem);
		line-height: 1.05;
		color: var(--text-primary);
	}
	.stats-subtitle {
		margin: 0;
		color: var(--text-secondary);
		font-size: 0.92rem;
	}
	.stats-actions {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		justify-content: flex-end;
		gap: 0.45rem;
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
	:global(.spinning) {
		animation: spin 900ms linear infinite;
	}
	@keyframes spin {
		to {
			transform: rotate(-360deg);
		}
	}
	@media (max-width: 768px) {
		.stats-head {
			align-items: flex-start;
			flex-direction: column;
			gap: 0.75rem;
		}
	}

	/* Card surface (scoped — the observe page's .surface-card is not global) */
	.surface-card {
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
	}
	.panel {
		padding: 1rem 1.15rem;
		min-width: 0;
		box-sizing: border-box;
	}
	.panel h2 {
		margin: 0 0 0.6rem;
		font-size: 0.95rem;
		font-weight: 600;
		color: var(--text-primary);
	}
	.section-head {
		display: flex;
		align-items: flex-end;
		justify-content: space-between;
		gap: 1rem;
		margin-top: 0.7rem;
		padding-top: 1rem;
		border-top: 1px solid var(--border-soft);
	}
	.section-head h2 {
		margin: 0;
		font-size: 1.05rem;
		color: var(--text-primary);
	}
	.section-head p {
		margin: 0.15rem 0 0;
	}
	.section-actions {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		justify-content: flex-end;
		gap: 0.5rem;
	}
	.segmented {
		display: inline-flex;
		align-items: center;
		padding: 0.15rem;
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		background: var(--bg-card);
	}
	.segmented button {
		border: 0;
		background: transparent;
		color: var(--text-secondary);
		border-radius: 6px;
		padding: 0.22rem 0.45rem;
		font-size: 0.76rem;
		cursor: pointer;
	}
	.segmented button.active {
		background: var(--bg-soft);
		color: var(--text-primary);
		box-shadow: inset 0 0 0 1px var(--border-soft);
	}

	.metric-grid {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(7.5rem, 1fr));
		gap: 0.7rem;
	}
	.metric {
		position: relative;
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 0.25rem;
		padding: 0.85rem 0.75rem;
		border-radius: 9px;
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		box-shadow: var(--shadow-sm, 0 1px 2px rgba(0, 0, 0, 0.04));
		transition: border-color 0.15s ease, transform 0.15s ease, box-shadow 0.15s ease;
	}
	.metric:hover {
		border-color: color-mix(in srgb, var(--accent-primary) 35%, var(--border-soft));
		transform: translateY(-1px);
	}
	.metric-value {
		font-family: var(--font-mono, monospace);
		font-size: 1.45rem;
		font-weight: 800;
		line-height: 1.1;
		color: var(--text-primary);
		font-variant-numeric: tabular-nums;
	}
	.metric-label {
		font-size: 0.72rem;
		font-weight: 650;
		text-transform: uppercase;
		letter-spacing: 0.04em;
		color: var(--text-muted);
		text-align: center;
	}

	.two-col {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(16rem, 1fr));
		gap: 1rem;
	}
	.rollout-grid {
		align-items: stretch;
	}
	.rollout-metrics {
		display: grid;
		grid-template-columns: repeat(4, minmax(0, 1fr));
		gap: 0;
		margin: 0.45rem 0 0.7rem;
	}
	.rollout-metrics .metric {
		min-width: 0;
		padding: 0.45rem 0.35rem;
		border: 0;
		border-right: 1px solid var(--border-soft);
		border-radius: 0;
		background: transparent;
	}
	.rollout-metrics .metric:last-child {
		border-right: 0;
	}
	.rollout-metrics .metric-value {
		font-size: 1.15rem;
	}
	.rollout-metrics .metric-label {
		text-align: center;
		overflow-wrap: anywhere;
	}
	.danger-text {
		color: var(--color-danger, var(--color-error, #b42318));
	}
	.rollout-skeleton .skeleton-title {
		width: 12rem;
	}
	@media (max-width: 640px) {
		.rollout-metrics {
			grid-template-columns: repeat(2, minmax(0, 1fr));
		}
		.rollout-metrics .metric:nth-child(2) {
			border-right: 0;
		}
		.rollout-metrics .metric:nth-child(-n + 2) {
			border-bottom: 1px solid var(--border-soft);
		}
	}
	.muted {
		color: var(--text-muted);
	}
	.small {
		font-size: 0.78rem;
	}
	.section-divider {
		height: 1px;
		margin: 0.7rem 0 0.45rem;
		background: var(--border-soft);
	}
	.channel-skeleton {
		display: flex;
		flex-direction: column;
		gap: 1rem;
	}
	.skeleton {
		position: relative;
		display: block;
		overflow: hidden;
		border-radius: 6px;
		background: color-mix(in srgb, var(--bg-soft) 82%, var(--text-muted) 10%);
	}
	.skeleton::after {
		content: '';
		position: absolute;
		inset: 0;
		transform: translateX(-100%);
		background: linear-gradient(
			90deg,
			transparent,
			color-mix(in srgb, var(--bg-card) 68%, transparent),
			transparent
		);
		animation: skeletonShimmer 1.45s ease-in-out infinite;
	}
	@keyframes skeletonShimmer {
		100% {
			transform: translateX(100%);
		}
	}
	.skeleton-card {
		min-height: 5.1rem;
	}
	.skeleton-metric-value {
		width: 4.4rem;
		height: 1.55rem;
	}
	.skeleton-label {
		width: 5.2rem;
		height: 0.58rem;
	}
	.skeleton-band {
		align-items: stretch;
	}
	.skeleton-engine {
		min-height: 7.4rem;
	}
	.skeleton-role {
		width: 6rem;
		height: 0.7rem;
	}
	.skeleton-row-label {
		width: 4.2rem;
		height: 0.65rem;
	}
	.skeleton-row-label.short {
		width: 3.4rem;
	}
	.skeleton-engine-pill {
		width: min(13rem, 100%);
		height: 1.35rem;
		border-radius: 0.4rem;
	}
	.skeleton-engine-pill.wide {
		width: min(17rem, 100%);
	}
	.skeleton-copy {
		width: min(28rem, 82%);
		height: 0.75rem;
		margin-bottom: 0.85rem;
	}
	.skeleton-copy.short {
		width: 10rem;
		margin: 0.2rem 0 0.55rem;
	}
	.skeleton-funnel {
		display: grid;
		gap: 0.5rem;
		margin-top: 0.55rem;
	}
	.skeleton-funnel-row {
		display: grid;
		grid-template-columns: 7rem minmax(0, 1fr) 3.2rem;
		align-items: center;
		gap: 0.75rem;
	}
	.skeleton-funnel-label {
		width: 5.4rem;
		height: 0.78rem;
	}
	.skeleton-funnel-band {
		width: var(--skeleton-width, 100%);
		height: 1.35rem;
		justify-self: center;
		border-radius: 8px;
	}
	.skeleton-funnel-value {
		width: 3rem;
		height: 0.78rem;
	}
	.skeleton-progress {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
		margin-bottom: 0.42rem;
	}
	.skeleton-progress-label {
		width: 8.5rem;
		height: 0.8rem;
	}
	.skeleton-progress-rate {
		width: 11rem;
		height: 0.75rem;
	}
	.skeleton-progress-bar {
		width: 100%;
		height: 0.9rem;
		border-radius: 999px;
	}
	.skeleton-pie-row {
		display: grid;
		grid-template-columns: minmax(0, 12rem) minmax(10rem, 1fr);
		align-items: center;
		justify-content: center;
		gap: 1.15rem;
		margin: 1rem auto 0.2rem;
		max-width: 32rem;
	}
	.skeleton-pie {
		width: 11rem;
		height: 11rem;
		border-radius: 999px;
		justify-self: end;
	}
	.skeleton-legend {
		display: grid;
		gap: 0.52rem;
		min-width: 0;
	}
	.skeleton-legend-total {
		width: 11rem;
		height: 1rem;
		margin-bottom: 0.2rem;
	}
	.skeleton-legend-line {
		width: 12.5rem;
		max-width: 100%;
		height: 0.78rem;
	}
	.skeleton-bar-row {
		padding: 0.38rem 0;
	}
	.skeleton-bar-track {
		width: 100%;
		height: 0.65rem;
		border-radius: 999px;
	}
	.skeleton-bar-track.short {
		width: 72%;
	}
	.skeleton-feed-item {
		pointer-events: none;
	}
	.skeleton-feed-meta {
		width: 11rem;
		height: 0.72rem;
	}
	.skeleton-feed-time {
		width: 7rem;
		height: 0.72rem;
	}
	.skeleton-feed-tag {
		width: 2rem;
		height: 1rem;
		border-radius: 0.3rem;
		flex: none;
	}
	.skeleton-feed-body {
		display: grid;
		gap: 0.35rem;
		width: 100%;
	}
	.skeleton-feed-title {
		width: 72%;
		height: 0.82rem;
	}
	.skeleton-feed-title.wide {
		width: 92%;
	}
	.skeleton-feed-subtitle {
		width: 54%;
		height: 0.72rem;
	}
	.skeleton-feed-subtitle.short {
		width: 34%;
	}
	.skeleton-kv-label {
		width: 6rem;
		height: 0.82rem;
	}
	.skeleton-kv-label.wide {
		width: 9rem;
	}
	.skeleton-kv-value {
		width: 3.4rem;
		height: 0.82rem;
	}
	.skeleton-account-row {
		pointer-events: none;
	}
	.skeleton-account-name {
		width: min(14rem, 100%);
		height: 0.82rem;
	}
	.skeleton-account-badge {
		width: 3.8rem;
		height: 1.05rem;
		border-radius: 999px;
	}
	.skeleton-account-num {
		width: 2.4rem;
		height: 0.82rem;
		justify-self: end;
	}
	.skeleton-account-time {
		width: 5.2rem;
		height: 0.82rem;
	}
	.attention-panel {
		display: grid;
		gap: 0.85rem;
	}
	.attention-panel h2 {
		margin: 0;
		font-size: 0.95rem;
		font-weight: 600;
		color: var(--text-primary);
		display: flex;
		align-items: center;
		gap: 0.5rem;
		line-height: 1.4;
	}
	.badge-scope {
		display: inline-flex;
		align-items: center;
		padding: 0.12rem 0.5rem;
		border-radius: 999px;
		background: color-mix(in srgb, var(--bg-soft) 85%, var(--border-soft));
		color: var(--text-muted);
		font-size: 0.72rem;
		font-weight: 500;
		line-height: 1.2;
	}
	.attention-metrics {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(7rem, 1fr));
		gap: 0.55rem;
	}
	.attention-metric {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.6rem;
		padding: 0.55rem 0.65rem;
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		background: var(--bg-soft);
	}
	.attention-metric span {
		font-size: 0.74rem;
		color: var(--text-muted);
	}
	.attention-metric strong {
		font-size: 1rem;
		font-variant-numeric: tabular-nums;
		color: var(--text-primary);
	}
	.attention-columns {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(15rem, 1fr));
		gap: 0.9rem;
		align-items: start;
	}
	.attention-column {
		min-width: 0;
	}
	.attention-column-title {
		margin: 0 0 0.35rem;
		font-size: 0.75rem;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		color: var(--text-muted);
	}
	.attention-row {
		grid-template-columns: minmax(6.5rem, 8rem) minmax(4rem, 1fr) auto;
		gap: 0.55rem;
	}
	.distillation-panel {
		display: flex;
		flex-direction: column;
		min-height: 100%;
	}
	.distill-state-chart {
		display: flex;
		flex: 1 1 auto;
		align-items: center;
		justify-content: center;
		min-height: 15rem;
		margin: 0.85rem 0 0;
	}
	.distill-state-chart :global(.muij-pie-chart) {
		display: grid;
		place-items: center;
		width: 100%;
	}
	.distill-state-chart :global(.muij-pie-body) {
		display: grid;
		grid-template-columns: minmax(0, 210px) minmax(12rem, max-content);
		align-items: center;
		justify-items: center;
		place-content: center;
		column-gap: 1.25rem;
		width: fit-content;
		max-width: 34rem;
		margin-inline: auto;
	}
	.distill-state-chart :global(.muij-pie-svg) {
		width: min(210px, 100%);
		justify-self: end;
	}
	.distill-state-chart :global(.muij-pie-legend) {
		flex: 0 1 auto;
		min-width: 12rem;
		width: max-content;
		max-width: min(18rem, 100%);
		align-self: center;
		justify-self: start;
	}
	.classification-stack-group {
		display: grid;
		gap: 0.9rem;
		margin-top: 0.75rem;
	}
	.classification-stack {
		display: grid;
		gap: 0.45rem;
	}
	.classification-stack-title {
		margin: 0;
		font-size: 0.75rem;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		color: var(--text-muted);
	}
	.funnel-row {
		display: grid;
		grid-template-columns: 6.5rem 1fr auto;
		align-items: center;
		gap: 0.7rem;
		padding: 0.32rem 0;
	}
	.funnel-label {
		font-size: 0.85rem;
		color: var(--text-secondary);
	}
	.bar-track {
		height: 0.65rem;
		background: var(--bg-soft);
		border: 1px solid var(--border-soft);
		border-radius: 999px;
		overflow: hidden;
	}
	.bar-fill {
		height: 100%;
		border-radius: 999px;
		background: var(--accent-primary);
		transition: width 0.4s ease;
		min-width: 2px;
	}
	.bar-distilled {
		background: var(--observe-blue, var(--accent-primary));
	}
	.bar-classified {
		background: var(--observe-green, var(--color-success));
	}
	.bar-needs_approval {
		background: var(--color-warning);
	}
	/* Classification bar tones */
	:global(.bar-label-needs_reply) {
		background: var(--color-warning);
	}
	:global(.bar-label-follow_up) {
		background: var(--observe-violet, var(--accent-secondary, var(--accent-primary)));
	}
	:global(.bar-label-fyi) {
		background: var(--observe-blue, var(--accent-primary));
	}
	:global(.bar-label-no_action) {
		background: var(--text-muted);
	}
	:global(.bar-pending) {
		background: color-mix(in srgb, var(--accent-primary) 55%, transparent);
	}
	:global(.bar-failed) {
		background: var(--color-danger, var(--color-error, #b42318));
	}
	:global(.bar-undistilled) {
		background: var(--border-default);
	}
	.bar-track.big {
		height: 0.9rem;
	}

	/* Engine + live status band */
	.engine-band {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(18rem, 1fr));
		gap: 0.7rem;
	}
	.engine-chip {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: 0.5rem 0.65rem;
		padding: 0.65rem 0.9rem;
		border-radius: 8px;
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
	}
	.engine-chip--status {
		display: grid;
		align-items: start;
		gap: 0.45rem;
	}
	.engine-role {
		font-size: 0.72rem;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		color: var(--text-muted);
	}
	.engine-row {
		display: grid;
		grid-template-columns: 5.4rem minmax(0, 1fr);
		align-items: center;
		gap: 0.6rem;
		width: 100%;
	}
	.engine-row-label {
		font-size: 0.74rem;
		color: var(--text-muted);
	}
	.engine-model {
		font-family: var(--font-mono, monospace);
		font-size: 0.82rem;
		color: var(--text-primary);
		background: var(--bg-soft);
		border: 1px solid var(--border-soft);
		border-radius: 0.4rem;
		padding: 0.1rem 0.4rem;
		max-width: 100%;
		overflow-wrap: anywhere;
	}
	.engine-chip--runtime .engine-model {
		white-space: normal;
		line-height: 1.35;
	}
	.engine-model--stack {
		display: grid;
		gap: 0.08rem;
	}
	.engine-live {
		display: inline-flex;
		align-items: center;
		gap: 0.35rem;
		margin-left: auto;
		font-size: 0.78rem;
		color: var(--text-secondary);
	}
	.engine-row .engine-live {
		margin-left: 0;
		min-width: 0;
	}
	.live-text {
		font-variant-numeric: tabular-nums;
	}
	.live-dot {
		width: 0.5rem;
		height: 0.5rem;
		border-radius: 999px;
		background: var(--color-success, #34d399);
		flex: none;
	}
	.live-dot.classify {
		background: var(--observe-violet, var(--accent-secondary, var(--accent-primary)));
	}
	.live-dot.idle {
		background: var(--border-default);
	}
	.live-dot.pulsing {
		animation: livePulse 1.4s ease-in-out infinite;
	}
	@keyframes livePulse {
		0%,
		100% {
			opacity: 1;
			box-shadow: 0 0 0 0 color-mix(in srgb, var(--color-success) 55%, transparent);
		}
		50% {
			opacity: 0.55;
			box-shadow: 0 0 0 4px transparent;
		}
	}

	/* Live distillation progress (inside the Distillation panel) */
	.live-progress {
		margin-bottom: 0.6rem;
	}
	.live-progress-head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.5rem;
		margin-bottom: 0.35rem;
	}
	.live-progress-rate {
		display: inline-flex;
		align-items: center;
		gap: 0.35rem;
		font-size: 0.78rem;
		color: var(--text-muted);
		font-variant-numeric: tabular-nums;
	}
	.live-progress-rate.active {
		color: var(--text-secondary);
	}

	/* Live distillation feed */
	.feed-live {
		display: inline-flex;
		align-items: center;
		gap: 0.3rem;
		font-size: 0.72rem;
		font-weight: 500;
		color: var(--color-success, #34d399);
		margin-left: 0.4rem;
		vertical-align: middle;
	}
	.feed {
		display: flex;
		flex-direction: column;
		gap: 0.55rem;
		margin-bottom: 0.5rem;
	}
	.feed-item {
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		background: var(--bg-soft);
		padding: 0.55rem 0.7rem;
	}
	.feed-meta {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.5rem;
		margin-bottom: 0.4rem;
	}
	.feed-provider {
		font-size: 0.72rem;
		font-weight: 600;
		color: var(--text-secondary);
	}
	.feed-time {
		display: inline-flex;
		align-items: center;
		gap: 0.4rem;
		font-size: 0.72rem;
		color: var(--text-muted);
		font-variant-numeric: tabular-nums;
	}
	.feed-received {
		color: var(--text-secondary);
		font-weight: 500;
	}
	.feed-latency {
		padding: 0.02rem 0.35rem;
		border-radius: 0.35rem;
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		color: var(--text-secondary);
	}
	.feed-io {
		display: grid;
		grid-template-columns: 1fr 1fr;
		gap: 0.5rem;
		align-items: stretch;
	}
	.feed-col {
		display: flex;
		gap: 0.45rem;
		min-width: 0;
		padding: 0.45rem 0.55rem;
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		background: var(--bg-card);
	}
	.feed-tag {
		flex: none;
		font-size: 0.6rem;
		font-weight: 700;
		letter-spacing: 0.05em;
		padding: 0.08rem 0.3rem;
		border-radius: 0.3rem;
		height: fit-content;
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		color: var(--text-muted);
	}
	.feed-tag.out {
		background: var(--observe-green-soft, var(--color-success-soft, var(--bg-card)));
		color: var(--color-success, var(--accent-primary));
		border-color: color-mix(in srgb, var(--color-success) 35%, transparent);
	}
	.feed-body {
		min-width: 0;
	}
	.feed-subject {
		font-size: 0.82rem;
		color: var(--text-primary);
		font-weight: 500;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.feed-sender {
		font-size: 0.74rem;
		color: var(--text-muted);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		margin-top: 0.1rem;
	}
	.feed-summary {
		font-size: 0.82rem;
		color: var(--text-primary);
		line-height: 1.35;
	}
	.feed-intent {
		margin-top: 0.25rem;
	}
	.intent-chip {
		display: inline-block;
		font-size: 0.68rem;
		padding: 0.05rem 0.4rem;
		border-radius: 0.35rem;
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		color: var(--text-secondary);
	}
	@media (max-width: 640px) {
		.feed-io {
			grid-template-columns: 1fr;
		}
		.skeleton-progress {
			align-items: flex-start;
			flex-direction: column;
		}
		.skeleton-progress-rate {
			width: min(11rem, 100%);
		}
		.skeleton-pie-row {
			grid-template-columns: 1fr;
			justify-items: center;
			row-gap: 0.75rem;
		}
		.skeleton-pie {
			justify-self: center;
		}
		.skeleton-legend {
			justify-items: center;
		}
		.skeleton-funnel-row,
		.skeleton-bar-row {
			grid-template-columns: 5.8rem minmax(0, 1fr) 2.6rem;
			gap: 0.5rem;
		}
		.distill-state-chart :global(.muij-pie-body) {
			grid-template-columns: 1fr;
			justify-items: center;
			row-gap: 0.75rem;
		}
		.distill-state-chart :global(.muij-pie-svg),
		.distill-state-chart :global(.muij-pie-legend) {
			justify-self: center;
		}
	}
	@media (prefers-reduced-motion: reduce) {
		.skeleton::after {
			animation: none;
		}
	}
	.funnel-value {
		font-size: 0.82rem;
		white-space: nowrap;
		color: var(--text-secondary);
		font-variant-numeric: tabular-nums;
	}

	.kv {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
		padding: 0.3rem 0;
		font-size: 0.9rem;
		color: var(--text-secondary);
	}
	.kv strong {
		color: var(--text-primary);
		font-variant-numeric: tabular-nums;
	}
	.compact-kv {
		padding: 0.18rem 0;
		font-size: 0.82rem;
	}
	.mini-list {
		margin-top: 0.5rem;
		padding-top: 0.4rem;
		border-top: 1px solid var(--border-soft);
	}
	.split-lists {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(10rem, 1fr));
		gap: 0.75rem;
		margin-top: 0.55rem;
	}
	.split-lists p {
		margin: 0 0 0.25rem;
	}
	.origin-grid {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(14rem, 1fr));
		gap: 0.45rem;
	}
	.origin-row {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
		min-width: 0;
		padding: 0.45rem 0.55rem;
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		background: var(--bg-soft);
		font-size: 0.84rem;
	}
	.origin-row span {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		color: var(--text-primary);
	}
	.origin-row strong {
		font-variant-numeric: tabular-nums;
		color: var(--text-secondary);
	}
	.ambient-table,
	.ambient-run-table,
	.contact-table {
		display: flex;
		flex-direction: column;
		min-width: 0;
		overflow-x: auto;
		scrollbar-width: thin;
		-webkit-overflow-scrolling: touch;
	}
	.ambient-row,
	.ambient-run-row,
	.contact-row {
		display: grid;
		gap: 0.5rem;
		align-items: center;
		padding: 0.38rem 0;
		border-top: 1px solid var(--border-soft);
		font-size: 0.82rem;
		color: var(--text-secondary);
		min-width: 0;
	}
	.ambient-row {
		grid-template-columns: minmax(0, 1.6fr) 0.75fr 0.65fr 0.85fr;
	}
	.ambient-run-row {
		grid-template-columns: minmax(0, 1.3fr) 0.55fr 0.75fr 0.65fr 0.8fr 0.55fr 0.65fr 0.8fr;
	}
	.contact-row {
		grid-template-columns: minmax(0, 1.35fr) 0.8fr 0.75fr;
	}
	.contact-main {
		display: grid;
		gap: 0.12rem;
		min-width: 0;
	}
	.ambient-header,
	.contact-header {
		border-top: none;
		color: var(--text-muted);
		font-size: 0.68rem;
		text-transform: uppercase;
		letter-spacing: 0.04em;
	}
	.truncate {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		color: var(--text-primary);
	}

	.badge {
		font-size: 0.7rem;
		padding: 0.08rem 0.45rem;
		border-radius: 0.35rem;
		border: 1px solid transparent;
		white-space: nowrap;
	}
	.badge-you {
		background: var(--bg-soft);
		color: var(--text-secondary);
		border-color: var(--border-soft);
	}
	.badge-presto {
		background: var(--color-info-soft);
		color: var(--color-info);
		border-color: color-mix(in srgb, var(--color-info) 40%, transparent);
	}

	.acct-table {
		display: flex;
		flex-direction: column;
		min-width: 0;
		overflow-x: auto;
		scrollbar-width: thin;
		-webkit-overflow-scrolling: touch;
	}
	.acct-row {
		display: grid;
		grid-template-columns: 2fr 0.8fr 0.8fr 1fr 1fr;
		gap: 0.5rem;
		align-items: center;
		padding: 0.4rem 0;
		border-top: 1px solid var(--border-soft);
		font-size: 0.85rem;
		color: var(--text-secondary);
	}
	.acct-name {
		color: var(--text-primary);
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.acct-header {
		border-top: none;
		color: var(--text-muted);
		font-size: 0.7rem;
		text-transform: uppercase;
		letter-spacing: 0.04em;
	}
	.llm-head {
		grid-template-columns: 2fr 0.8fr 1fr 1fr 1fr;
	}
	.num {
		text-align: right;
		font-variant-numeric: tabular-nums;
	}
	.err-dot {
		color: var(--color-error);
		margin-left: 0.25rem;
	}
	@media (max-width: 780px) {
		.stats-page {
			padding: 1rem;
		}
		.section-head {
			align-items: flex-start;
			flex-direction: column;
		}
		.section-actions {
			justify-content: flex-start;
		}
		.acct-row {
			min-width: 42rem;
		}
		.ambient-run-row {
			min-width: 46rem;
		}
		.ambient-row {
			grid-template-columns: minmax(0, 1.4fr) 0.8fr 0.7fr 0.8fr;
		}
	}
</style>
