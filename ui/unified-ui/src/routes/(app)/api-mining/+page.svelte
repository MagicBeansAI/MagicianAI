<script lang="ts">
	import { onMount } from 'svelte';
	import { showError, showSuccess } from '$lib/shared/stores/notifications';
	// Pack/registry browsing was moved to /skills (which already lists
	// embedded compiled packs as `layer: built-in` alongside skill-loaded
	// packs). The Tools page now focuses on API mining + traffic
	// monitoring; the Forge surface module is gone.
	import SwaggerModal from '$lib/magician/components/SwaggerModal.svelte';
	import ServerPager from '$lib/shared/components/ServerPager.svelte';
	import { timedFetch } from '$lib/shared/fetch';
	import {
		applyRelevanceFilter,
		DEFAULT_RELEVANCE,
		recipeRows,
		recipeReplayUncertainty,
		recipeReplayTone,
		recipeRunTone,
		staleRecipeDetailIds,
		submitRecipeReplay,
		type Relevance
	} from '$lib/magician/api-mining/overview';

	type OriginDecision = 'allowed' | 'blocked';

	type NoisyOriginEntry = {
		origin_key: string;
		origin_url: string;
		trace_count: number;
		capability_count: number;
		decision?: OriginDecision | null;
	};

	type NoisyOriginsResponse = {
		threshold: number;
		origins: NoisyOriginEntry[];
	};

	type OriginEntry = {
		origin_key: string;
		origin_url: string;
		capability_count: number;
		trace_count: number;
		capabilities: CapabilitySummary[];
		updated_at: number;
	};

	type CapabilitySummary = {
		origin_key?: string;
		origin_url?: string;
		id: string;
		name: string;
		method: string;
		url_template: string;
		confidence: string;
		side_effects: string;
		sample_count: number;
		updated_at: number;
		graphql_operation?: string;
		replay_success_count?: number;
		replay_failure_count?: number;
		relevance: Relevance;
		used_by_recipe_ids?: string[];
		hidden?: boolean;
	};

	type SiteGroup = {
		site: string;
		origins: OriginEntry[];
		capabilities: CapabilitySummary[];
		telemetry_hidden: number;
	};

	type AuthStatus = {
		has_auth: boolean;
		is_stale: boolean;
		has_cookies: boolean;
		has_headers: boolean;
		has_storage: boolean;
	};

	type AuthRefreshPhase =
		| 'starting'
		| 'waiting_for_auth'
		| 'captured'
		| 'captured_unverified'
		| 'verifying'
		| 'verified'
		| 'verification_failed'
		| 'timed_out'
		| 'failed';

	type AuthRefreshStatus = {
		refresh_id: string;
		origin_key: string;
		origin_url: string;
		phase: AuthRefreshPhase;
		message: string;
		started_at_ms: number;
		updated_at_ms: number;
		terminal: boolean;
		verification_status?: number;
	};

	type ReplayResponse = {
		status: number;
		headers: Record<string, string> | null;
		body: string | null;
		elapsed_ms: number;
		auth_was_stale: boolean;
		confidence_after: string;
		error: string | null;
	};

	type LearnedApiOriginView = {
		origin: OriginEntry;
		filteredCapabilities: CapabilitySummary[];
		visibleCapabilities: CapabilitySummary[];
		page: number;
		totalPages: number;
		pageStart: number;
		pageEnd: number;
	};

	// --- Learned APIs state ---
	const LEARNED_APIS_PAGE_SIZE_OPTIONS = [10, 25, 50, 100] as const;
	let authStatuses: Record<string, AuthStatus> = {};
	let replayingCapabilityIds = new Set<string>();
	let replayResults: Record<string, ReplayResponse> = {};
	let apisTabLoaded = false;
	let refreshingAuthOriginKeys = new Set<string>();
	let authRefreshStatuses: Record<string, AuthRefreshStatus> = {};
	let learnedApisQuery = '';
	let learnedApisPageSize = 25;
	let learnedApisPages: Record<string, number> = {};
	let previousLearnedApisQuery = learnedApisQuery;
	let previousLearnedApisPageSize = learnedApisPageSize;
	let noisyOrigins: NoisyOriginEntry[] = [];
	let noisyOriginsThreshold = 0;
	let noisyOriginsLoading = false;
	let noisyOriginsError: string | null = null;
	let noisyTabLoaded = false;
	let noisyOriginsActing = new Set<string>();

	// --- Swagger modal state ---
	let swaggerOpen = false;
	let swaggerOriginKey = '';
	let swaggerOriginUrl = '';

	// --- Edit & Replay modal state ---
	let editReplayOpen = false;
	let editReplayOriginKey = '';
	let editReplayCapabilityId = '';
	let editReplayUrl = '';
	let editReplayHeaders = '';
	let editReplayBody = '';
	let editReplayLoading = false;
	let editReplayResult: ReplayResponse | null = null;

	// --- Tab config ---
	const apiMiningTabs = [
		{ label: 'Recipes' },
		{ label: 'Learned APIs' },
		{ label: 'Activity' },
		{ label: 'Auth' }
	];
	let activeTab = 0;

	type RecipeMetadata = {
		id: string;
		version: number;
		template: string;
		inputs: Array<{
			name: string;
			schema: 'string' | 'number' | 'boolean';
			source: 'task_text' | 'browser_typed';
		}>;
		maturity: string;
		origins: string[];
		step_count: number;
		replay_stats: { successful_replays: number; failed_replays: number };
		has_write_steps: boolean;
		last_replayed_at_ms?: number | null;
	};
	type RecipeRunRecord = {
		ts_ms: number;
		recipe_id: string;
		execution_id: string;
		version: number;
		rail_ended: string;
		failure_class?: string;
		auth_heals: number;
		transport_downgrades: number;
		duration_ms: number;
		recompiled_to_version?: number;
	};
	type OverviewResponse = {
		recipes: RecipeMetadata[];
		sites: SiteGroup[];
		counters: {
			router: RouterMetricsSnapshot;
			passive_validation: PassiveValidationMetricsSnapshot;
			recipe: RecipeMetricsSnapshot;
			projection: ProjectionMetricsSnapshot;
			registry_health: RegistryHealthSnapshot;
		};
		auth: Record<string, AuthStatus>;
		grants: number;
		truncation?: {
			truncated: boolean;
			omitted_recipes: number;
			omitted_sites: number;
			omitted_origins: number;
			omitted_capabilities: number;
		};
	};
	type RecipeStepDetail = {
		id: string;
		origin: string;
		method: string;
		url_template: string;
		capability_id?: string | null;
		side_effects: string;
		verify_with?: string | null;
		browser_fallback?: unknown;
	};
	type RecipeVersionDetail = {
		version: number;
		maturity: string;
		compiled_at_ms: number;
		steps: RecipeStepDetail[];
		answer_spec: Array<{ field: string; step_id: string; extractor: unknown }>;
	};
	type RecipeDetail = {
		id: string;
		current_version: number;
		shape: { inputs: RecipeMetadata['inputs'] };
		versions: RecipeVersionDetail[];
	};
	type ReplayGrant = {
		id: string;
		recipe_id?: string;
		step_id?: string;
		origin: string;
		granted_at_ms: number;
		revoked_at_ms?: number | null;
	};
	type WorkflowMetadata = {
		id: string;
		origin_key: string;
		name: string;
		step_count: number;
		maturity: string;
		last_compiled_at_ms: number;
	};
	type SiteWorkflow = WorkflowMetadata & { origin_url: string };
	let overview: OverviewResponse | null = null;
	let overviewLoading = false;
	let overviewError: string | null = null;
	let overviewLoaded = false;
	let recipeRuns: Record<string, RecipeRunRecord[]> = {};
	let recipeRunsLoading = new Set<string>();
	let shownRelevance = new Set<Relevance>(DEFAULT_RELEVANCE);
	let showNoisyReview = false;
	let expandedCapabilityIds = new Set<string>();
	let expandedRecipeIds = new Set<string>();
	let expandedSiteIds = new Set<string>();
	let siteWorkflows: Record<string, SiteWorkflow[]> = {};
	let siteWorkflowsLoading = new Set<string>();
	let siteWorkflowsErrors: Record<string, string> = {};
	let recipeDetails: Record<string, RecipeDetail> = {};
	let recipeDetailLoading = new Set<string>();
	let recipeInputValues: Record<string, Record<string, string>> = {};
	let recipeReplayResults: Record<string, { status: number; body: unknown }> = {};
	let recipeReplayLoading = new Set<string>();
	let replayGrants: ReplayGrant[] = [];
	let grantsLoaded = false;
	const DASHBOARD_RELEVANCE: Relevance[] = [
		'answer_bearing',
		'dependency',
		'first_party_api',
		'third_party_api',
		'unclassified'
	];
	type ApiMiningSettings = {
		effective: boolean;
		process_enabled: boolean;
		scope_override?: boolean | null;
		set_by: 'config' | 'scope' | 'default';
	};
	let apiMiningSettings: ApiMiningSettings | null = null;
	let apiMiningSettingsLoading = false;
	let purgeConfirmationOpen = false;
	let purgeConfirmation = '';
	$: sortedRecipes = recipeRows(overview?.recipes ?? []);
	$: telemetryHidden = (overview?.sites ?? []).reduce(
		(sum, site) => sum + site.telemetry_hidden,
		0
	);
	$: overviewOrigins = uniqueOrigins(overview?.sites ?? []);

	function uniqueOrigins(sites: SiteGroup[]): OriginEntry[] {
		const origins = new Map<string, OriginEntry>();
		for (const site of sites) {
			for (const origin of site.origins) origins.set(origin.origin_key, origin);
		}
		return [...origins.values()].sort((left, right) =>
			left.origin_url.localeCompare(right.origin_url)
		);
	}

	async function toggleSiteDetails(site: SiteGroup): Promise<void> {
		const siteId = site.site;
		const next = new Set(expandedSiteIds);
		if (next.has(siteId)) {
			next.delete(siteId);
			expandedSiteIds = next;
			return;
		}
		next.add(siteId);
		expandedSiteIds = next;
		if (siteWorkflows[siteId] || siteWorkflowsLoading.has(siteId)) return;

		siteWorkflowsLoading = new Set([...siteWorkflowsLoading, siteId]);
		const errors = { ...siteWorkflowsErrors };
		delete errors[siteId];
		siteWorkflowsErrors = errors;
		try {
			const perOrigin = await Promise.all(
				site.origins.map(async (origin) => {
					const response = await timedFetch(
						`/api/magician/v2/api-mining/workflows/${encodeURIComponent(origin.origin_key)}`
					);
					if (!response.ok) {
						throw new Error(`${origin.origin_url}: HTTP ${response.status}`);
					}
					const workflows = (await response.json()) as WorkflowMetadata[];
					return workflows.map((workflow) => ({ ...workflow, origin_url: origin.origin_url }));
				})
			);
			siteWorkflows = {
				...siteWorkflows,
				[siteId]: perOrigin
					.flat()
					.sort((left, right) => right.last_compiled_at_ms - left.last_compiled_at_ms)
			};
		} catch (error) {
			siteWorkflowsErrors = {
				...siteWorkflowsErrors,
				[siteId]: error instanceof Error ? error.message : String(error)
			};
		} finally {
			const loading = new Set(siteWorkflowsLoading);
			loading.delete(siteId);
			siteWorkflowsLoading = loading;
		}
	}

	function overviewUrl(): string {
		const relevance = DASHBOARD_RELEVANCE.join(',');
		return `/api/magician/v2/api-mining/overview?relevance=${encodeURIComponent(relevance)}`;
	}

	function applyOverview(next: OverviewResponse): void {
		const staleIds = new Set(staleRecipeDetailIds(next.recipes, recipeDetails));
		recipeDetails = Object.fromEntries(Object.entries(recipeDetails).filter(([id]) => !staleIds.has(id)));
		recipeInputValues = Object.fromEntries(Object.entries(recipeInputValues).filter(([id]) => !staleIds.has(id)));
		expandedRecipeIds = new Set([...expandedRecipeIds].filter((id) => !staleIds.has(id)));
		overview = next;
		authStatuses = next.auth ?? {};
		routerSnapshot = next.counters?.router ?? null;
		passiveValidationSnapshot = next.counters?.passive_validation ?? null;
		recipeSnapshot = next.counters?.recipe ?? null;
		projectionSnapshot = next.counters?.projection ?? null;
		registryHealthSnapshot = next.counters?.registry_health ?? null;
	}

	async function fetchOverview(): Promise<void> {
		overviewLoading = true;
		overviewError = null;
		try {
			const response = await timedFetch(overviewUrl());
			if (!response.ok) throw new Error(`HTTP ${response.status}`);
			applyOverview((await response.json()) as OverviewResponse);
		} catch (error) {
			overviewError = error instanceof Error ? error.message : String(error);
		} finally {
			overviewLoading = false;
		}
	}

	function setRelevanceShown(relevance: Relevance, shown: boolean): void {
		if (!shown && shownRelevance.size === 1 && shownRelevance.has(relevance)) {
			showError('Keep at least one relevance filter selected');
			return;
		}
		const next = new Set(shownRelevance);
		if (shown) next.add(relevance);
		else next.delete(relevance);
		shownRelevance = next;
	}

	function relevanceLabel(relevance: Relevance): string {
		return relevance.replaceAll('_', ' ').replace(/\b\w/g, (value) => value.toUpperCase());
	}

	function relevanceColor(relevance: Relevance): 'default' | 'success' | 'warning' | 'error' | 'info' {
		if (relevance === 'answer_bearing') return 'success';
		if (relevance === 'dependency' || relevance === 'first_party_api') return 'info';
		if (relevance === 'telemetry') return 'error';
		return 'warning';
	}

	function recipeLabel(recipeId: string): string {
		return overview?.recipes.find((recipe) => recipe.id === recipeId)?.template ?? recipeId;
	}

	function overviewCapability(capabilityId?: string | null): CapabilitySummary | undefined {
		if (!capabilityId) return undefined;
		return overview?.sites
			.flatMap((site) => site.capabilities)
			.find((capability) => capability.id === capabilityId);
	}

	async function fetchApiMiningSettings(): Promise<void> {
		try {
			const response = await timedFetch('/api/magician/v2/api-mining/settings');
			if (!response.ok) throw new Error(`HTTP ${response.status}`);
			apiMiningSettings = (await response.json()) as ApiMiningSettings;
		} catch (error) {
			showError(`Could not load API mining settings: ${error instanceof Error ? error.message : String(error)}`);
		}
	}

	async function setApiMiningEnabled(enabled: boolean): Promise<void> {
		apiMiningSettingsLoading = true;
		try {
			const response = await timedFetch('/api/magician/v2/api-mining/settings', {
				method: 'PUT',
				headers: { 'Content-Type': 'application/json' },
				body: JSON.stringify({ enabled, layer: 'scope' })
			});
			if (!response.ok) throw new Error(`HTTP ${response.status}`);
			const updated = (await response.json()) as ApiMiningSettings;
			apiMiningSettings = updated;
			if (enabled && !updated.effective) {
				showError('API mining remains off', 'The process-level configuration is the active ceiling.');
			} else {
				showSuccess(`API mining ${enabled ? 'enabled' : 'disabled'} for this workspace`);
			}
		} catch (error) {
			showError(`Could not update API mining: ${error instanceof Error ? error.message : String(error)}`);
		} finally {
			apiMiningSettingsLoading = false;
		}
	}

	function openDisableAndPurgeConfirmation(): void {
		purgeConfirmation = '';
		purgeConfirmationOpen = true;
	}

	async function disableAndPurgeApiMining(): Promise<void> {
		if (purgeConfirmation !== 'delete learned data') return;
		apiMiningSettingsLoading = true;
		try {
			const response = await timedFetch('/api/magician/v2/api-mining/settings/disable-and-purge', {
				method: 'POST',
				headers: { 'Content-Type': 'application/json' },
				body: JSON.stringify({ confirm: purgeConfirmation })
			});
			if (!response.ok) throw new Error(`HTTP ${response.status}`);
			await Promise.all([fetchApiMiningSettings(), fetchOverview()]);
			purgeConfirmationOpen = false;
			purgeConfirmation = '';
			showSuccess('API mining was disabled and learned data was deleted');
		} catch (error) {
			showError(`Could not delete learned data: ${error instanceof Error ? error.message : String(error)}`);
		} finally {
			apiMiningSettingsLoading = false;
		}
	}

	function ensureOverviewLoaded(_node: HTMLElement): void {
		if (overviewLoaded) return;
		overviewLoaded = true;
		void fetchOverview();
		void fetchApiMiningSettings();
	}

	async function fetchRecipeRuns(recipeId: string): Promise<void> {
		if (recipeRunsLoading.has(recipeId)) return;
		recipeRunsLoading = new Set([...recipeRunsLoading, recipeId]);
		try {
			const response = await timedFetch(
				`/api/magician/v2/api-mining/recipes/${encodeURIComponent(recipeId)}/runs?limit=5`
			);
			if (!response.ok) throw new Error(`HTTP ${response.status}`);
			const body = (await response.json()) as { runs?: RecipeRunRecord[] };
			recipeRuns = { ...recipeRuns, [recipeId]: body.runs ?? [] };
		} catch (error) {
			showError(`Could not load recipe history: ${error instanceof Error ? error.message : String(error)}`);
		} finally {
			const next = new Set(recipeRunsLoading);
			next.delete(recipeId);
			recipeRunsLoading = next;
		}
	}

	function formatRecipeTime(timestamp?: number | null): string {
		return timestamp ? new Date(timestamp).toLocaleString() : 'never';
	}

	function currentRecipeVersion(detail: RecipeDetail | undefined): RecipeVersionDetail | undefined {
		return detail?.versions.find((version) => version.version === detail.current_version);
	}

	function recipeCapabilityIds(detail: RecipeDetail | undefined): Set<string> {
		return new Set(
			(currentRecipeVersion(detail)?.steps ?? [])
				.map((step) => step.capability_id)
				.filter((value): value is string => Boolean(value))
		);
	}

	function projectionsForCapabilityIds(capabilityIds: Set<string>): ProjectionIndexEntry[] {
		return projections.filter((projection) => capabilityIds.has(projection.capability_id));
	}

	async function fetchReplayGrants(): Promise<void> {
		const response = await timedFetch('/api/magician/v2/api-mining/replay-grants');
		if (!response.ok) throw new Error(`HTTP ${response.status}`);
		replayGrants = (await response.json()) as ReplayGrant[];
		grantsLoaded = true;
	}

	async function toggleRecipeDetails(recipe: RecipeMetadata): Promise<void> {
		const next = new Set(expandedRecipeIds);
		if (next.has(recipe.id)) {
			next.delete(recipe.id);
			expandedRecipeIds = next;
			return;
		}
		next.add(recipe.id);
		expandedRecipeIds = next;
		if (!recipeInputValues[recipe.id]) {
			recipeInputValues = {
				...recipeInputValues,
				[recipe.id]: Object.fromEntries(
					recipe.inputs.map((input) => [input.name, ''])
				)
			};
		}
		if (recipeDetails[recipe.id]) return;
		recipeDetailLoading = new Set([...recipeDetailLoading, recipe.id]);
		try {
			const detailResponse = await timedFetch(
				`/api/magician/v2/api-mining/recipes/${encodeURIComponent(recipe.id)}`
			);
			if (!detailResponse.ok) throw new Error(`HTTP ${detailResponse.status}`);
			const detail = (await detailResponse.json()) as RecipeDetail;
			const listedVersion = overview?.recipes.find((item) => item.id === recipe.id)?.version;
			if (listedVersion !== undefined && detail.current_version < listedVersion) {
				throw new Error('Recipe changed while loading. Close and reopen its details.');
			}
			recipeDetails = {
				...recipeDetails,
				[recipe.id]: detail
			};
			recipeInputValues = { ...recipeInputValues, [recipe.id]: Object.fromEntries(detail.shape.inputs.map((input) => [input.name, ''])) };
			await Promise.all([
				projectionsTabLoaded ? Promise.resolve() : fetchProjections(),
				grantsLoaded ? Promise.resolve() : fetchReplayGrants()
			]);
			projectionsTabLoaded = true;
		} catch (error) {
			showError(`Could not load recipe details: ${error instanceof Error ? error.message : String(error)}`);
		} finally {
			const loading = new Set(recipeDetailLoading);
			loading.delete(recipe.id);
			recipeDetailLoading = loading;
		}
	}

	function setRecipeInput(recipeId: string, name: string, value: string): void {
		recipeInputValues = {
			...recipeInputValues,
			[recipeId]: { ...(recipeInputValues[recipeId] ?? {}), [name]: value }
		};
	}

	async function runRecipeNow(recipeId: string): Promise<void> {
		if (recipeReplayLoading.has(recipeId)) return;
		if (recipeReplayUncertainty(recipeReplayResults[recipeId]?.body)) {
			showError('Write outcome uncertain', 'Check the destination before starting another write.');
			return;
		}
		recipeReplayLoading = new Set([...recipeReplayLoading, recipeId]);
		try {
			const detail = recipeDetails[recipeId];
			const version = currentRecipeVersion(detail);
			if (!detail || !version) throw new Error('Load recipe details before replaying');
			const response = await submitRecipeReplay({
				id: recipeId, version: detail.current_version,
				inputs: recipeInputValues[recipeId] ?? {},
				hasWriteSteps: version.steps.some((step) => step.side_effects === 'write')
			}, timedFetch);
			const body = response.body;
			recipeReplayResults = { ...recipeReplayResults, [recipeId]: { status: response.status, body } };
			const result = body as {
				success?: boolean;
				pending_approval?: unknown;
				error?: string;
				message?: string;
				state_persistence_warning?: string;
				failure?: { detail?: string };
				fallback?: { detail?: string };
			} | null;
			const uncertainty = recipeReplayUncertainty(body);
			if (response.status === 409 && result?.error === 'stale_recipe_skill') {
				const { [recipeId]: staleDetail, ...remainingDetails } = recipeDetails;
				recipeDetails = remainingDetails;
				const { [recipeId]: staleInputs, ...remainingInputs } = recipeInputValues;
				recipeInputValues = remainingInputs;
				expandedRecipeIds = new Set([...expandedRecipeIds].filter((id) => id !== recipeId));
				await fetchOverview();
				showError('Recipe changed', 'Open the updated recipe and review its inputs before replaying.');
			} else if (uncertainty) {
				showError('Write outcome uncertain', uncertainty);
			} else if (response.status === 409 && result?.pending_approval) {
				showError('Write approval required', 'Approve this recipe during a normal task run before using direct replay.');
			} else if (recipeReplayTone(response.status, body) !== 'success') {
				throw new Error(result?.failure?.detail || result?.fallback?.detail || result?.message || result?.error || `HTTP ${response.status}`);
			} else {
				showSuccess('Recipe replay completed', result?.state_persistence_warning);
				await Promise.all([fetchOverview(), fetchRecipeRuns(recipeId)]);
			}
		} catch (error) {
			showError(`Recipe replay failed: ${error instanceof Error ? error.message : String(error)}`);
		} finally {
			const next = new Set(recipeReplayLoading);
			next.delete(recipeId);
			recipeReplayLoading = next;
		}
	}

	async function revokeRecipeGrant(grantId: string): Promise<void> {
		const response = await timedFetch(
			`/api/magician/v2/api-mining/replay-grants/${encodeURIComponent(grantId)}`,
			{ method: 'DELETE' }
		);
		if (!response.ok) {
			showError(`Could not revoke grant: HTTP ${response.status}`);
			return;
		}
		await Promise.all([fetchReplayGrants(), fetchOverview()]);
		showSuccess('Replay grant revoked');
	}

	function acknowledgeRecipeOutcome(recipeId: string): void {
		const { [recipeId]: acknowledged, ...remaining } = recipeReplayResults;
		recipeReplayResults = remaining;
	}

	async function toggleCapabilityDetails(capabilityId: string): Promise<void> {
		const next = new Set(expandedCapabilityIds);
		if (next.has(capabilityId)) next.delete(capabilityId);
		else {
			next.add(capabilityId);
			if (!projectionsTabLoaded) {
				await fetchProjections();
				projectionsTabLoaded = true;
			}
		}
		expandedCapabilityIds = next;
	}

	// --- Cached resource detail state ---
	// Projections are loaded only when a recipe/capability drawer opens.
	type ProjectionIndexEntry = {
		id: string;
		capability_id: string;
		origin: string;
		resource_label: string;
		lifecycle: 'pending' | 'approved' | 'live' | 'invalidated';
		last_ingested_at: number | null;
		row_count: number;
	};
	let projections: ProjectionIndexEntry[] = [];
	let projectionsLoading = false;
	let projectionsError: string | null = null;
	let projectionsActing: Set<string> = new Set();
	let projectionsTabLoaded = false;

	async function fetchProjections(): Promise<void> {
		projectionsLoading = true;
		projectionsError = null;
		try {
			const resp = await timedFetch('/api/magician/v2/api-mining/projections');
			if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
			const data = (await resp.json()) as { projections: ProjectionIndexEntry[] };
			projections = data.projections ?? [];
		} catch (e) {
			projectionsError = e instanceof Error ? e.message : String(e);
		} finally {
			projectionsLoading = false;
		}
	}

	async function runProjectionAction(id: string, action: 'approve' | 'purge-rows'): Promise<void> {
		if (projectionsActing.has(id)) return;
		projectionsActing = new Set([...projectionsActing, id]);
		try {
			const path =
				action === 'approve'
					? `/api/magician/v2/api-mining/projections/${id}/approve`
					: `/api/magician/v2/api-mining/projections/${id}/purge-rows`;
			const resp = await timedFetch(path, {
				method: 'POST',
			});
			if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
			await fetchProjections();
		} catch (e) {
			projectionsError = e instanceof Error ? e.message : String(e);
		} finally {
			const next = new Set(projectionsActing);
			next.delete(id);
			projectionsActing = next;
		}
	}

	// --- Routing Activity tile state ---
	// Polls API-mining metrics endpoints to surface live counters.
	// Counters reset on process restart; this is operational telemetry,
	// not a durable history.
	type RouterMetricsSnapshot = {
		replayed: number;
		refused_navigate: number;
		pass_through_router_disabled: number;
		pass_through_no_binding: number;
		pass_through_no_capability: number;
		pass_through_low_confidence: number;
		pass_through_not_replayable: number;
		pass_through_session_not_ready: number;
		pass_through_router_panic: number;
		pass_through_other: number;
	};
	type ProjectionMetricsSnapshot = {
		pending_created: number;
		approved: number;
		rows_ingested: number;
		rows_served_from_store: number;
		stale_served: number;
		schema_converged: number;
		migration_rejected: number;
		rows_purged: number;
	};
	type PassiveValidationMetricsSnapshot = {
		observed_xhr_fetch: number;
		matched_capabilities: number;
		validations_fired: number;
		validations_passed: number;
		validations_failed: number;
		skipped_non_xhr_fetch: number;
		skipped_not_first_party: number;
		skipped_no_match: number;
		skipped_policy: number;
		skipped_missing_response: number;
		errors: number;
		promoted_to_validated: number;
		promoted_to_trusted: number;
		budget_exhausted: number;
	};
	type RecipeMetricsSnapshot = {
		lookup_hit: number;
		lookup_miss: number;
		replay_started: number;
		replay_succeeded: number;
		replay_failed_auth: number;
		replay_failed_anti_bot: number;
		replay_failed_schema_drift: number;
		replay_failed_http: number;
		replay_failed_network: number;
		replay_failed_policy: number;
		replay_failed_input: number;
		fallback_handoffs: number;
		grants_created: number;
		grants_used: number;
		approvals_denied: number;
		auth_heals: number;
	};
	type OriginTakeoverReadiness = {
		origin_key: string;
		origin_url: string;
		indexed_capability_count: number;
		loadable_capability_count: number;
		replayable_capability_count: number;
		action_bindings_count: number;
		takeover_ready_bindings_count: number;
		origin_policy_decision?: string | null;
		auto_replay_allowed?: boolean | null;
		replay_mode?: string | null;
		inactive_reasons: string[];
	};
	type RegistryHealthWarning = {
		code: string;
		message: string;
	};
	type RegistryHealthSnapshot = {
		index_version: string;
		expected_index_version: string;
		version_mismatch: boolean;
		last_rebuilt: number;
		checked_at: number;
		origin_count: number;
		indexed_capability_count: number;
		loadable_capability_count: number;
		stale_index_count: number;
		unindexed_capability_count: number;
		candidates_by_confidence: Record<string, number>;
		action_bindings_count: number;
		takeover_ready_bindings_count: number;
		replayable_capability_count: number;
		stale_origins: string[];
		unindexed_origins: string[];
		origin_readiness: OriginTakeoverReadiness[];
		warnings?: RegistryHealthWarning[];
	};
	let routerSnapshot: RouterMetricsSnapshot | null = null;
	let projectionSnapshot: ProjectionMetricsSnapshot | null = null;
	let passiveValidationSnapshot: PassiveValidationMetricsSnapshot | null = null;
	let recipeSnapshot: RecipeMetricsSnapshot | null = null;
	let registryHealthSnapshot: RegistryHealthSnapshot | null = null;
	let metricsLoading = false;
	let metricsError: string | null = null;
	let metricsTimer: ReturnType<typeof setInterval> | null = null;

	async function fetchMetrics(): Promise<void> {
		metricsLoading = true;
		metricsError = null;
		try {
			const [routerRes, projectionRes, passiveRes, recipeRes, healthRes] = await Promise.all([
				timedFetch('/api/magician/v2/api-mining/router-metrics'),
				timedFetch('/api/magician/v2/api-mining/projection-metrics'),
				timedFetch('/api/magician/v2/api-mining/passive-validation-metrics'),
				timedFetch('/api/magician/v2/api-mining/recipe-metrics'),
				timedFetch('/api/magician/v2/api-mining/registry-health')
			]);
			for (const response of [routerRes, projectionRes, passiveRes, recipeRes, healthRes]) {
				if (!response.ok) throw new Error(`metrics HTTP ${response.status}`);
			}
			routerSnapshot = (await routerRes.json()) as RouterMetricsSnapshot;
			projectionSnapshot = (await projectionRes.json()) as ProjectionMetricsSnapshot;
			passiveValidationSnapshot = (await passiveRes.json()) as PassiveValidationMetricsSnapshot;
			recipeSnapshot = (await recipeRes.json()) as RecipeMetricsSnapshot;
			registryHealthSnapshot = (await healthRes.json()) as RegistryHealthSnapshot;
		} catch (e) {
			metricsError = e instanceof Error ? e.message : String(e);
		} finally {
			metricsLoading = false;
		}
	}

	function ensureMetricsLoaded(node: HTMLElement): { destroy(): void } {
		// Fetch immediately on tab activation, then poll every 5s while
		// the tab stays mounted. Destroyed on tab change so the poll
		// doesn't run forever.
		void fetchMetrics();
		metricsTimer = setInterval(() => void fetchMetrics(), 5000);
		return {
			destroy() {
				if (metricsTimer) {
					clearInterval(metricsTimer);
					metricsTimer = null;
				}
			}
		};
	}

	// --- Learned APIs derived display data ---
	$: learnedApiOrigins = (overview?.sites ?? [])
		.map((site) => {
			const capabilities = applyRelevanceFilter(site.capabilities, shownRelevance);
			return {
				origin_key: site.site,
				origin_url: site.site,
				capability_count: capabilities.length,
				trace_count: site.origins.reduce((sum, origin) => sum + origin.trace_count, 0),
				capabilities,
				updated_at: Math.max(0, ...site.origins.map((origin) => origin.updated_at))
			};
		})
		.filter((site) => site.capabilities.length > 0);
	$: learnedApiTotalCapabilities = learnedApiOrigins.reduce(
		(sum, origin) => sum + (origin.capability_count ?? origin.capabilities.length),
		0
	);
	$: learnedApiSearchTokens = normalizeSearchTokens(learnedApisQuery);
	$: if (
		learnedApisQuery !== previousLearnedApisQuery ||
		learnedApisPageSize !== previousLearnedApisPageSize
	) {
		previousLearnedApisQuery = learnedApisQuery;
		previousLearnedApisPageSize = learnedApisPageSize;
		learnedApisPages = {};
	}
	$: learnedApiOriginViews = buildLearnedApiOriginViews(
		learnedApiOrigins,
		learnedApiSearchTokens,
		learnedApisPageSize,
		learnedApisPages
	);
	$: learnedApiVisibleOriginCount = learnedApiOriginViews.length;
	$: learnedApiFilteredTotalCapabilities = learnedApiOriginViews.reduce(
		(sum, view) => sum + view.filteredCapabilities.length,
		0
	);

	// --- Computed props for Edit & Replay result display ---
	$: editReplayStatusText = editReplayResult?.error
		? 'Error'
		: `${editReplayResult?.status ?? 0}`;

	$: editReplayStatusColor = ((): 'default' | 'success' | 'warning' | 'error' | 'info' => {
		if (!editReplayResult) return 'default';
		if (editReplayResult.error) return 'error';
		if (editReplayResult.status < 300) return 'success';
		if (editReplayResult.status < 500) return 'warning';
		return 'error';
	})();

	$: editReplayFormattedBody = (() => {
		if (!editReplayResult?.body) return '';
		try {
			return JSON.stringify(JSON.parse(editReplayResult.body), null, 2);
		} catch {
			return editReplayResult.body;
		}
	})();

	$: editReplayBodyLanguage = (() => {
		if (!editReplayResult?.body) return 'text';
		try {
			JSON.parse(editReplayResult.body);
			return 'json';
		} catch {
			return 'text';
		}
	})();

	// --- Svelte action: lazy-load Learned APIs tab on first mount ---
	function ensureApisLoaded(_node: HTMLElement) {
		if (!apisTabLoaded) {
			apisTabLoaded = true;
			if (!overviewLoaded) {
				overviewLoaded = true;
				void fetchOverview();
			}
		}
	}

	function normalizeSearchTokens(value: string): string[] {
		return value
			.trim()
			.toLowerCase()
			.split(/\s+/)
			.map((token) => token.trim())
			.filter(Boolean);
	}

	function capabilitySearchText(origin: OriginEntry, capability: CapabilitySummary): string {
		return [
			origin.origin_key,
			origin.origin_url,
			capability.id,
			capability.name,
			capability.method,
			capability.url_template,
			capability.confidence,
			capability.side_effects,
			capability.sample_count,
			capability.graphql_operation,
			capability.replay_success_count,
			capability.replay_failure_count
		]
			.filter((value) => value !== undefined && value !== null)
			.join(' ')
			.toLowerCase();
	}

	function originSearchText(origin: OriginEntry): string {
		return [origin.origin_key, origin.origin_url, origin.capability_count, origin.trace_count]
			.filter((value) => value !== undefined && value !== null)
			.join(' ')
			.toLowerCase();
	}

	function matchesSearch(text: string, tokens: string[]): boolean {
		return tokens.every((token) => text.includes(token));
	}

	function filteredCapabilitiesForOrigin(
		origin: OriginEntry,
		tokens: string[]
	): CapabilitySummary[] {
		if (tokens.length === 0) return origin.capabilities;
		if (matchesSearch(originSearchText(origin), tokens)) return origin.capabilities;
		return origin.capabilities.filter((capability) =>
			matchesSearch(capabilitySearchText(origin, capability), tokens)
		);
	}

	function buildLearnedApiOriginViews(
		origins: OriginEntry[],
		tokens: string[],
		pageSize: number,
		pages: Record<string, number>
	): LearnedApiOriginView[] {
		const views: LearnedApiOriginView[] = [];
		for (const origin of origins) {
			const filteredCapabilities = filteredCapabilitiesForOrigin(origin, tokens);
			if (tokens.length > 0 && filteredCapabilities.length === 0) continue;

			const totalPages = Math.max(1, Math.ceil(filteredCapabilities.length / pageSize));
			const requestedPage = pages[origin.origin_key] ?? 0;
			const page = Math.max(0, Math.min(totalPages - 1, requestedPage));
			const start = page * pageSize;
			const visibleCapabilities = filteredCapabilities.slice(start, start + pageSize);
			views.push({
				origin,
				filteredCapabilities,
				visibleCapabilities,
				page,
				totalPages,
				pageStart: filteredCapabilities.length === 0 ? 0 : start + 1,
				pageEnd: Math.min(filteredCapabilities.length, start + visibleCapabilities.length)
			});
		}
		return views;
	}

	function setLearnedApisPage(originKey: string, page: number): void {
		learnedApisPages = { ...learnedApisPages, [originKey]: Math.max(0, page) };
	}

	function setLearnedApisPagerPage(originKey: string, page: number): void {
		setLearnedApisPage(originKey, page - 1);
	}

	function handleLearnedApisPageSizeChange(event: Event): void {
		const value = Number((event.currentTarget as HTMLSelectElement).value);
		learnedApisPageSize = LEARNED_APIS_PAGE_SIZE_OPTIONS.includes(
			value as (typeof LEARNED_APIS_PAGE_SIZE_OPTIONS)[number]
		)
			? value
			: 25;
	}

	function methodColor(method: string): 'default' | 'success' | 'warning' | 'error' | 'info' {
		switch (method.toUpperCase()) {
			case 'GET':
				return 'success';
			case 'POST':
				return 'info';
			case 'PUT':
			case 'PATCH':
				return 'warning';
			case 'DELETE':
				return 'error';
			default:
				return 'default';
		}
	}

	function confidenceColor(confidence: string): 'default' | 'success' | 'warning' | 'error' | 'info' {
		switch (confidence.toLowerCase()) {
			case 'trusted':
				return 'success';
			case 'validated':
				return 'info';
			case 'candidate':
				return 'warning';
			case 'observed':
			case 'draft':
				return 'default';
			default:
				return 'default';
		}
	}

	function replayStatusColor(result: ReplayResponse): 'default' | 'success' | 'warning' | 'error' | 'info' {
		if (result.error) return 'error';
		if (result.status < 300) return 'success';
		if (result.status < 500) return 'warning';
		return 'error';
	}

	function replayStatusText(result: ReplayResponse): string {
		return result.error ? 'Error' : `${result.status}`;
	}

	function formatReplayBody(body: string | null): string {
		if (!body) return '';
		try {
			return JSON.stringify(JSON.parse(body), null, 2);
		} catch {
			return body;
		}
	}

	function replayBodyLanguage(body: string | null): string {
		if (!body) return 'text';
		try {
			JSON.parse(body);
			return 'json';
		} catch {
			return 'text';
		}
	}

	function authBadges(
		auth: AuthStatus | undefined
	): Array<{
		text: string;
		color: 'default' | 'success' | 'warning' | 'error' | 'info';
		title?: string;
	}> {
		if (!auth) return [{ text: 'Auth unknown', color: 'default' }];
		const badges: Array<{
			text: string;
			color: 'default' | 'success' | 'warning' | 'error' | 'info';
			title?: string;
		}> = [
			auth.has_auth
				? {
						text: auth.is_stale ? 'Auth stale' : 'Auth ready',
						color: auth.is_stale ? 'warning' : 'success',
						title: auth.is_stale
							? 'Use Refresh Auth to start a secure CDP session that captures and verifies fresh authentication.'
							: 'Captured auth is available for replay.'
					}
				: { text: 'No auth', color: 'default' }
		];
		if (auth.has_cookies) badges.push({ text: 'Cookies', color: 'info' });
		if (auth.has_headers) badges.push({ text: 'Headers', color: 'info' });
		if (auth.has_storage) badges.push({ text: 'Storage', color: 'info' });
		return badges;
	}

	function authRefreshLabel(phase: AuthRefreshPhase): string {
		switch (phase) {
			case 'starting':
				return 'Starting secure browser';
			case 'waiting_for_auth':
				return 'Waiting for authenticated request';
			case 'captured':
				return 'Authentication captured';
			case 'captured_unverified':
				return 'Authentication refreshed';
			case 'verifying':
				return 'Verifying authentication';
			case 'verified':
				return 'Authentication verified';
			case 'verification_failed':
				return 'Verification failed';
			case 'timed_out':
				return 'Authentication refresh timed out';
			case 'failed':
				return 'Authentication refresh failed';
		}
	}

	function authRefreshButtonLabel(originKey: string, hasAuth: boolean): string {
		const status = authRefreshStatuses[originKey];
		if (!status || status.terminal) return hasAuth ? 'Refresh Auth' : 'Capture Auth';
		if (status.phase === 'starting') return 'Starting...';
		if (status.phase === 'waiting_for_auth') return 'Waiting...';
		if (status.phase === 'captured' || status.phase === 'verifying') return 'Verifying...';
		return 'Refreshing...';
	}

	function setAuthRefreshStatus(status: AuthRefreshStatus): void {
		authRefreshStatuses = { ...authRefreshStatuses, [status.origin_key]: status };
	}

	async function refreshOriginAuthBadge(originKey: string): Promise<void> {
		try {
			const response = await timedFetch(
				`/api/magician/v2/api-mining/auth-status/${encodeURIComponent(originKey)}`
			);
			if (!response.ok) return;
			const status = (await response.json()) as AuthStatus;
			authStatuses = { ...authStatuses, [originKey]: status };
		} catch {
			// The refresh result remains visible even if the badge refresh is unavailable.
		}
	}

	function openSwagger(origin: OriginEntry): void {
		swaggerOriginKey = origin.origin_key;
		swaggerOriginUrl = origin.origin_url;
		swaggerOpen = true;
	}

	async function handleRefreshAuth(origin: OriginEntry): Promise<void> {
		if (refreshingAuthOriginKeys.has(origin.origin_key)) return;
		refreshingAuthOriginKeys = new Set(refreshingAuthOriginKeys).add(origin.origin_key);
		try {
			const response = await timedFetch(
				`/api/magician/v2/api-mining/origins/${encodeURIComponent(origin.origin_key)}/refresh-auth`,
				{ method: 'POST' }
			);
			if (!response.ok) {
				const detail = await response.text();
				throw new Error(detail || `HTTP ${response.status}`);
			}
			let status = (await response.json()) as AuthRefreshStatus;
			setAuthRefreshStatus(status);

			while (!status.terminal) {
				await new Promise((resolve) => setTimeout(resolve, 700));
				const statusResponse = await timedFetch(
					`/api/magician/v2/api-mining/origins/${encodeURIComponent(origin.origin_key)}/refresh-auth/${encodeURIComponent(status.refresh_id)}`
				);
				if (!statusResponse.ok) throw new Error(`Status polling failed with HTTP ${statusResponse.status}`);
				status = (await statusResponse.json()) as AuthRefreshStatus;
				setAuthRefreshStatus(status);
			}

			await refreshOriginAuthBadge(origin.origin_key);
			if (status.phase === 'verified') {
				showSuccess('Authentication refreshed', status.message);
			} else if (status.phase === 'captured_unverified') {
				showSuccess('Authentication captured', status.message);
			} else {
				showError(authRefreshLabel(status.phase), status.message);
			}
		} catch (error) {
			showError('Authentication refresh failed', error instanceof Error ? error.message : 'Unknown error');
		} finally {
			const next = new Set(refreshingAuthOriginKeys);
			next.delete(origin.origin_key);
			refreshingAuthOriginKeys = next;
		}
	}

	async function fetchNoisyOrigins(): Promise<void> {
		noisyOriginsLoading = true;
		noisyOriginsError = null;
		try {
			const resp = await timedFetch('/api/magician/v2/api-mining/noisy-origins');
			if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
			const data: NoisyOriginsResponse = await resp.json();
			noisyOrigins = data.origins ?? [];
			noisyOriginsThreshold = data.threshold ?? 0;
		} catch (err) {
			noisyOriginsError = err instanceof Error ? err.message : String(err);
		} finally {
			noisyOriginsLoading = false;
		}
	}

	function noisyDecisionSummary(decision?: OriginDecision | null): string {
		if (decision === 'allowed') {
			return 'Reviewed and allowed for continued capture and mining.';
		}
		if (decision === 'blocked') {
			return 'Future capture and mining are blocked until you change the decision.';
		}
		return 'High-frequency origin pending operator review.';
	}

	function originHostLabel(originUrl: string): string {
		try {
			return new URL(originUrl).host || originUrl;
		} catch {
			return originUrl;
		}
	}

	async function runNoisyOriginAction(
		originKey: string,
		originUrl: string,
		action: 'allow' | 'block' | 'purge' | 'block-and-purge'
	): Promise<void> {
		noisyOriginsActing = new Set([...noisyOriginsActing, originKey]);
		try {
			const resp = await timedFetch(`/api/magician/v2/api-mining/origins/${originKey}/${action}`, {
				method: 'POST',
				headers: { 'Content-Type': 'application/json' },
				body: JSON.stringify({ origin_url: originUrl })
			});
			if (!resp.ok) {
				const errorText = await resp.text();
				throw new Error(errorText || `HTTP ${resp.status}`);
			}
			if (action === 'allow') {
				showSuccess('Origin allowed', 'Future capture and mining remain enabled for this origin.');
			} else if (action === 'block') {
				showSuccess('Origin blocked', 'Future capture and mining are disabled for this origin.');
			} else if (action === 'purge') {
				showSuccess('Origin purged', 'Existing mined capabilities, traces, and captured auth were removed.');
			} else {
				showSuccess('Origin blocked and purged', 'Future capture is blocked and existing data was removed.');
			}
			await Promise.all([fetchNoisyOrigins(), fetchOverview()]);
		} catch (err) {
			const message = err instanceof Error ? err.message : String(err);
			showError('Noisy origin action failed', message);
		} finally {
			const next = new Set(noisyOriginsActing);
			next.delete(originKey);
			noisyOriginsActing = next;
		}
	}

	async function handleReplay(originKey: string, capabilityId: string): Promise<void> {
		replayingCapabilityIds = new Set([...replayingCapabilityIds, capabilityId]);
		try {
			const resp = await timedFetch(
				`/api/magician/v2/api-mining/replay/${originKey}/${capabilityId}`,
				{ method: 'POST', headers: { 'Content-Type': 'application/json' }, body: '{}' }
			);
			replayResults = { ...replayResults, [capabilityId]: await resp.json() };
		} catch (err) {
			replayResults = {
				...replayResults,
				[capabilityId]: {
					status: 0,
					headers: null,
					body: null,
					elapsed_ms: 0,
					auth_was_stale: false,
					confidence_after: '',
					error: String(err)
				}
			};
		} finally {
			const next = new Set(replayingCapabilityIds);
			next.delete(capabilityId);
			replayingCapabilityIds = next;
		}
	}

	// --- Edit & Replay handlers ---
	async function openEditReplay(originKey: string, capabilityId: string): Promise<void> {
		editReplayOriginKey = originKey;
		editReplayCapabilityId = capabilityId;
		editReplayResult = null;
		editReplayLoading = false;

		try {
			const resp = await timedFetch(
				`/api/magician/v2/api-mining/capabilities/${originKey}/${capabilityId}`
			);
			if (resp.ok) {
				const cap = await resp.json();
				editReplayUrl = cap.url_template || '';
				editReplayHeaders = cap.headers_template
					? JSON.stringify(cap.headers_template, null, 2)
					: '{}';
				editReplayBody = cap.body_template || '';
			}
		} catch {
			editReplayUrl = '';
			editReplayHeaders = '{}';
			editReplayBody = '';
		}

		editReplayOpen = true;
	}

	async function submitEditReplay(): Promise<void> {
		editReplayLoading = true;
		try {
			let headersObj: Record<string, string> = {};
			try {
				headersObj = JSON.parse(editReplayHeaders);
			} catch {
				// invalid JSON, use empty object
			}

			const resp = await timedFetch(
				`/api/magician/v2/api-mining/replay/${editReplayOriginKey}/${editReplayCapabilityId}`,
				{
					method: 'POST',
					headers: { 'Content-Type': 'application/json' },
					body: JSON.stringify({
						parameter_overrides: {},
						headers_overrides: headersObj,
						body_override: editReplayBody || null
					})
				}
			);
			editReplayResult = await resp.json();
		} catch (err) {
			editReplayResult = {
				status: 0,
				headers: null,
				body: null,
				elapsed_ms: 0,
				auth_was_stale: false,
				confidence_after: '',
				error: String(err)
			};
		} finally {
			editReplayLoading = false;
		}
	}

	function handleEditReplayBackdropClick(event: MouseEvent): void {
		if (event.target === event.currentTarget) {
			editReplayOpen = false;
		}
	}

	function handlePurgeConfirmationBackdropClick(event: MouseEvent): void {
		if (event.target === event.currentTarget && !apiMiningSettingsLoading) {
			purgeConfirmationOpen = false;
		}
	}

	onMount(() => {
		// Tabs are lazy: each section owns its first bounded request.
	});
</script>

<svelte:head>
	<title>API Mining &middot; Magican</title>
</svelte:head>

<div class="api-mining-page">
	<section class="api-hero">
		<div class="api-hero-copy">
			<p class="api-overline">API Mining</p>
			<h1>Learned API Operations</h1>
			<p>
			Review browserless task recipes, relevant learned APIs, activity, and replay authentication.
			</p>
		</div>
		<div class="api-chip-row" aria-label="API mining summary">
			<span class="api-chip api-chip--info">{overview?.recipes.length ?? 0} recipes</span>
			<span class="api-chip api-chip--info">{learnedApiOrigins.length} learned origins</span>
			<span class="api-chip api-chip--info">{learnedApiTotalCapabilities} capabilities</span>
			<span class="api-chip api-chip--default">{telemetryHidden} telemetry calls hidden</span>
			<span class="api-chip api-chip--default">{projections.length} projections</span>
			{#if overview?.truncation?.truncated}
				<span class="api-chip api-chip--warning">Showing a bounded overview</span>
			{/if}
		</div>
	</section>

	<section class="api-section api-section-header" use:ensureOverviewLoaded>
		<div>
			<p class="api-overline">Workspace control</p>
			<h2>{apiMiningSettings ? `API mining is ${apiMiningSettings.effective ? 'on' : 'off'}` : 'Loading API mining state'}</h2>
			<p>
				{#if !apiMiningSettings}
					Checking the workspace and process-level privacy controls.
				{:else if apiMiningSettings.effective === false}
					No traffic is captured, no browser session material is learned, and repeated tasks use the browser.
				{:else}
					Magician learns answer-bearing APIs from its own browser runs and replays repeated tasks without a browser.
				{/if}
			</p>
			<p class="routing-caption">Set by: {apiMiningSettings?.set_by ?? 'loading'}. Turning it off keeps existing learned data until you delete it.</p>
		</div>
		<div class="api-section-actions">
			<button
				class="api-button api-button-primary"
				type="button"
				disabled={apiMiningSettingsLoading || !apiMiningSettings || (!apiMiningSettings.process_enabled && !apiMiningSettings.effective)}
				on:click={() => setApiMiningEnabled(!(apiMiningSettings?.effective ?? true))}
			>
				{apiMiningSettingsLoading ? 'Updating...' : !apiMiningSettings?.process_enabled && apiMiningSettings?.effective === false ? 'Disabled by process config' : apiMiningSettings?.effective === false ? 'Turn on' : 'Turn off'}
			</button>
			<button
				class="api-button api-button-warning"
				type="button"
				disabled={apiMiningSettingsLoading}
				on:click={openDisableAndPurgeConfirmation}
			>
				Turn off and delete learned data
			</button>
		</div>
	</section>

	<nav class="api-tabs" aria-label="API mining sections">
		{#each apiMiningTabs as tab, index (tab.label)}
			<button
				id={`api-mining-tab-${index}`}
				class="api-tab"
				class:api-tab-active={activeTab === index}
				type="button"
				aria-pressed={activeTab === index}
				on:click={() => (activeTab = index)}
			>
				{tab.label}
			</button>
		{/each}
	</nav>

	<section
		id={`api-mining-panel-${activeTab}`}
		class="api-panel"
		aria-labelledby={`api-mining-tab-${activeTab}`}
	>
		{#if activeTab === 0}
			<div class="api-tab-stack" use:ensureOverviewLoaded>
				<section class="api-section api-section-header">
					<div>
						<p class="api-overline">Browserless task paths</p>
						<h2>Task Recipes</h2>
						<p>Repeated task shapes learned from successful browser runs, with their latest replay rail.</p>
					</div>
					<div class="api-section-actions">
						<span class="api-chip api-chip--info">{sortedRecipes.length} recipes</span>
						<span class="api-chip">{overview?.grants ?? 0} active grants</span>
						<button class="api-button api-button-secondary" type="button" disabled={overviewLoading} on:click={fetchOverview}>
							{overviewLoading ? 'Refreshing...' : 'Refresh'}
						</button>
					</div>
				</section>
				{#if overviewError}
					<div class="api-alert api-alert--error" role="alert">Failed to load recipes: {overviewError}</div>
				{:else if overviewLoading && sortedRecipes.length === 0}
					<div class="api-alert api-alert--info">Loading learned task recipes...</div>
				{:else if sortedRecipes.length === 0}
					<section class="api-empty">
						<h3>No task recipes yet</h3>
						<p>A successful browser-backed task with answer-bearing API traffic can teach the first recipe.</p>
					</section>
				{:else}
					<div class="api-card-list">
						{#each sortedRecipes as recipe (recipe.id)}
							{@const detail = recipeDetails[recipe.id]}
							{@const currentVersion = currentRecipeVersion(detail)}
							{@const cachedRows = projectionsForCapabilityIds(recipeCapabilityIds(detail))}
							{@const grants = replayGrants.filter((grant) => grant.recipe_id === recipe.id && !grant.revoked_at_ms)}
							<section class="api-section projection-card">
								<header class="api-item-header">
									<div>
										<h3>{recipe.template}</h3>
										<p>{recipe.origins.join(', ') || 'No origin recorded'}</p>
									</div>
									<span class={`api-chip api-chip--${confidenceColor(recipe.maturity)}`}>{recipe.maturity}</span>
								</header>
								<div class="api-chip-row">
									<span class="api-chip api-chip--info">{recipe.step_count} step{recipe.step_count === 1 ? '' : 's'}</span>
									<span class="api-chip">{recipe.inputs.length} input{recipe.inputs.length === 1 ? '' : 's'}</span>
									<span class="api-chip api-chip--success">{recipe.replay_stats.successful_replays} succeeded</span>
									<span class="api-chip api-chip--warning">{recipe.replay_stats.failed_replays} failed</span>
									{#if recipe.has_write_steps}<span class="api-chip api-chip--warning">write approval required</span>{/if}
								</div>
								<p class="api-mono">Last replay: {formatRecipeTime(recipe.last_replayed_at_ms)}</p>
								<div class="api-action-row">
									<button class="api-button" type="button" disabled={recipeDetailLoading.has(recipe.id)} on:click={() => toggleRecipeDetails(recipe)}>
										{recipeDetailLoading.has(recipe.id) ? 'Loading...' : expandedRecipeIds.has(recipe.id) ? 'Hide details' : 'View details'}
									</button>
									<button class="api-button api-button-secondary" type="button" disabled={recipeRunsLoading.has(recipe.id)} on:click={() => fetchRecipeRuns(recipe.id)}>
										{recipeRunsLoading.has(recipe.id) ? 'Loading...' : recipeRuns[recipe.id] ? 'Refresh runs' : 'Show last runs'}
									</button>
								</div>
								{#if recipeRuns[recipe.id]}
									{#if recipeRuns[recipe.id].length === 0}
										<p class="routing-caption">No run history has been recorded yet.</p>
									{:else}
										<div class="routing-badges">
											{#each recipeRuns[recipe.id] as run (`${run.execution_id}-${run.ts_ms}`)}
												<span class={`api-chip api-chip--${recipeRunTone(run)}`} title={run.failure_class ?? 'successful'}>
													{run.rail_ended} · v{run.version} · {run.duration_ms}ms · {formatRecipeTime(run.ts_ms)}
												</span>
											{/each}
										</div>
									{/if}
								{/if}
								{#if expandedRecipeIds.has(recipe.id)}
									<section class="api-detail-drawer">
										{#if recipeDetailLoading.has(recipe.id) && !detail}
											<p class="routing-caption">Loading recipe definition...</p>
										{:else if currentVersion}
											<h4>Version {currentVersion.version} steps</h4>
											<div class="learned-apis-table-wrap">
												<table class="learned-apis-table">
													<thead><tr><th>Step</th><th>Method</th><th>Origin</th><th>Relevance</th><th>Side effects</th><th>Fallback</th></tr></thead>
													<tbody>
														{#each currentVersion.steps as step (step.id)}
															{@const capability = overviewCapability(step.capability_id)}
															<tr><td>{step.id}</td><td>{step.method}</td><td>{originHostLabel(step.origin)}</td><td>{capability ? relevanceLabel(capability.relevance) : 'Unclassified'}</td><td>{step.side_effects}</td><td>{step.browser_fallback ? 'available' : 'none'}</td></tr>
														{/each}
													</tbody>
												</table>
											</div>
											<p class="routing-caption">Answer fields: {currentVersion.answer_spec.map((field) => `${field.field} from ${field.step_id}`).join(', ') || 'none'}</p>
											<p class="routing-caption">Versions: {detail.versions.map((version) => `v${version.version} ${version.maturity}`).join(' · ')}</p>

											<h4>Run now</h4>
											<div class="api-form-stack">
												{#each detail.shape.inputs as input (input.name)}
													<label class="api-field" for={`recipe-${recipe.id}-${input.name}`}>
														<span>{input.name} ({input.schema})</span>
														<input id={`recipe-${recipe.id}-${input.name}`} value={recipeInputValues[recipe.id]?.[input.name] ?? ''} on:input={(event) => setRecipeInput(recipe.id, input.name, event.currentTarget.value)} />
													</label>
												{/each}
												<button class="api-button api-button-primary" type="button" disabled={recipeReplayLoading.has(recipe.id) || Boolean(recipeReplayUncertainty(recipeReplayResults[recipe.id]?.body))} on:click={() => runRecipeNow(recipe.id)}>
													{recipeReplayLoading.has(recipe.id) ? 'Running...' : 'Run recipe'}
												</button>
											</div>
											{#if recipeReplayResults[recipe.id]}
												<div class={`api-alert api-alert--${recipeReplayTone(recipeReplayResults[recipe.id].status, recipeReplayResults[recipe.id].body)}`}>
													{recipeReplayResults[recipe.id].status ? `HTTP ${recipeReplayResults[recipe.id].status}` : 'Response unavailable'}<pre class="api-code"><code>{JSON.stringify(recipeReplayResults[recipe.id].body, null, 2)}</code></pre>
													{#if recipeReplayUncertainty(recipeReplayResults[recipe.id].body)}
														<p>{recipeReplayUncertainty(recipeReplayResults[recipe.id].body)}</p>
														<button class="api-button api-button-secondary" type="button" on:click={() => acknowledgeRecipeOutcome(recipe.id)}>Destination checked — clear result</button>
													{/if}
												</div>
											{/if}

											<h4>Write grants</h4>
											{#if grants.length === 0}<p class="routing-caption">No active durable grants.</p>{/if}
											{#each grants as grant (grant.id)}
												<div class="api-action-row"><span class="api-chip api-chip--warning">{grant.step_id ?? 'capability'} · {originHostLabel(grant.origin)}</span><button class="api-button api-button-compact" type="button" on:click={() => revokeRecipeGrant(grant.id)}>Revoke</button></div>
											{/each}

											<h4>Cached rows</h4>
											{#if projectionsLoading}
												<p class="routing-caption">Loading cached resources...</p>
											{:else if projectionsError}
												<div class="api-alert api-alert--error" role="alert">Could not load cached resources: {projectionsError}</div>
											{:else if cachedRows.length === 0}
												<p class="routing-caption">No projections are linked to this recipe.</p>
											{:else}
												{#each cachedRows as projection (projection.id)}
													<div class="api-item-header"><span>{projection.resource_label} · {projection.row_count} rows · {projection.lifecycle}</span><div class="api-action-row">{#if projection.lifecycle === 'pending'}<button class="api-button api-button-compact" type="button" disabled={projectionsActing.has(projection.id)} on:click={() => runProjectionAction(projection.id, 'approve')}>Approve</button>{/if}<button class="api-button api-button-compact" type="button" disabled={projectionsActing.has(projection.id)} on:click={() => runProjectionAction(projection.id, 'purge-rows')}>Purge rows</button></div></div>
												{/each}
											{/if}
										{/if}
									</section>
								{/if}
							</section>
						{/each}
					</div>
				{/if}
			</div>
		{:else if activeTab === 1}
			<div class="learned-apis-tab" use:ensureApisLoaded>
				<section class="api-section learned-apis-summary">
					<div class="learned-apis-summary-copy">
						<h2>Learned APIs</h2>
						<p>Discovered capabilities grouped by the site that taught them, with relevance and recipe usage.</p>
					</div>
					<div class="learned-apis-stats" aria-label="Learned API summary">
						<span class="api-chip api-chip--info">
							{learnedApiVisibleOriginCount} origin{learnedApiVisibleOriginCount !== 1 ? 's' : ''}
						</span>
						<span class="api-chip api-chip--info">
							{learnedApiFilteredTotalCapabilities}
							{#if learnedApiSearchTokens.length > 0}
								of {learnedApiTotalCapabilities}
							{/if}
							capabilit{learnedApiFilteredTotalCapabilities !== 1 ? 'ies' : 'y'}
						</span>
						<button
							class="api-button api-button-secondary"
							type="button"
							disabled={overviewLoading}
							on:click={fetchOverview}
						>
							{overviewLoading ? 'Refreshing...' : 'Refresh'}
						</button>
					</div>
					<div class="learned-apis-controls">
						{#each ['answer_bearing', 'dependency', 'first_party_api', 'third_party_api', 'unclassified'] as relevance (relevance)}
							<label class="api-chip" class:api-chip--info={shownRelevance.has(relevance as Relevance)}>
								<input type="checkbox" checked={shownRelevance.has(relevance as Relevance)} on:change={(event) => setRelevanceShown(relevance as Relevance, event.currentTarget.checked)} />
								{relevanceLabel(relevance as Relevance)}
							</label>
						{/each}
						<button class="api-button api-button-compact" type="button" on:click={() => { showNoisyReview = !showNoisyReview; if (showNoisyReview && !noisyTabLoaded) { noisyTabLoaded = true; void fetchNoisyOrigins(); } }}>
							{telemetryHidden} telemetry calls hidden
						</button>
						<label class="api-field api-field-inline" for="learned-apis-search">
							<span>Search</span>
							<input
								id="learned-apis-search"
								type="search"
								bind:value={learnedApisQuery}
								placeholder="Origin, URL, method, confidence"
							/>
						</label>
						<label class="api-field api-field-inline api-field-page-size" for="learned-apis-page-size">
							<span>Rows</span>
							<select
								id="learned-apis-page-size"
								value={learnedApisPageSize}
								on:change={handleLearnedApisPageSizeChange}
							>
								{#each LEARNED_APIS_PAGE_SIZE_OPTIONS as option (option)}
									<option value={option}>{option}</option>
								{/each}
							</select>
						</label>
						{#if learnedApisQuery.trim()}
							<button class="api-button api-button-compact" type="button" on:click={() => (learnedApisQuery = '')}>
								Clear
							</button>
						{/if}
					</div>
				</section>

				{#if showNoisyReview}
					<section class="api-section api-detail-drawer">
						<header class="api-item-header"><div><h3>Noise review</h3><p>High-frequency origins and explicit capture decisions. Telemetry stays excluded from recipes.</p></div><button class="api-button api-button-compact" type="button" on:click={() => (showNoisyReview = false)}>Close</button></header>
						{#if noisyOriginsError}<div class="api-alert api-alert--error">{noisyOriginsError}</div>{/if}
						{#if noisyOriginsLoading}<p class="routing-caption">Loading noisy origins...</p>{/if}
						{#if !noisyOriginsLoading && noisyOrigins.length === 0}<p class="routing-caption">No origins exceed the {noisyOriginsThreshold}-trace review threshold.</p>{/if}
						{#each noisyOrigins as origin (origin.origin_key)}
							<div class="api-item-header"><div><strong>{origin.origin_url}</strong><p>{origin.trace_count} traces · {noisyDecisionSummary(origin.decision)}</p></div><div class="api-action-row"><button class="api-button api-button-compact" type="button" disabled={noisyOriginsActing.has(origin.origin_key)} on:click={() => runNoisyOriginAction(origin.origin_key, origin.origin_url, 'allow')}>Allow</button><button class="api-button api-button-compact" type="button" disabled={noisyOriginsActing.has(origin.origin_key)} on:click={() => runNoisyOriginAction(origin.origin_key, origin.origin_url, 'block')}>Block</button></div></div>
						{/each}
					</section>
				{/if}

				{#if overviewLoading && learnedApiOrigins.length === 0}
					<section class="api-empty">
						<h3>Loading learned APIs</h3>
						<p>Fetching the task-centric API mining overview.</p>
					</section>
				{:else if overviewError}
					<div class="api-alert api-alert--error" role="alert">{overviewError}</div>
				{:else if learnedApiOrigins.length === 0}
					<section class="api-empty">
						<h3>No APIs discovered yet</h3>
						<p>Browse a website to start learning.</p>
					</section>
				{:else if learnedApiOriginViews.length === 0}
					<section class="api-empty">
						<h3>No matching APIs</h3>
						<p>Clear the search or use a broader origin, method, URL, confidence, or side-effect term.</p>
					</section>
				{:else}
					<div class="learned-apis-origin-list">
						{#each learnedApiOriginViews as view (view.origin.origin_key)}
							{@const origin = view.origin}
							{@const site = overview?.sites.find((candidate) => candidate.site === origin.origin_key)}
							{@const primaryOrigin = site?.origins[0]}
							<section class="api-section learned-apis-origin">
								<header class="learned-apis-origin-header">
									<div>
										<h3>{origin.origin_url}</h3>
										<p>
											{#if learnedApiSearchTokens.length > 0}
												{view.filteredCapabilities.length} of {origin.capabilities.length} matching capabilities /
											{:else}
												{origin.capability_count} capabilit{origin.capability_count !== 1 ? 'ies' : 'y'} /
											{/if}
											{origin.trace_count} trace{origin.trace_count !== 1 ? 's' : ''}
										</p>
									</div>
									<div class="learned-apis-origin-actions">
										{#if site}
											<button class="api-button" type="button" on:click={() => toggleSiteDetails(site)}>
												{expandedSiteIds.has(site.site) ? 'Hide site details' : 'Site details'}
											</button>
										{/if}
										{#if primaryOrigin}<button class="api-button" type="button" on:click={() => openSwagger(primaryOrigin)}>
											View OpenAPI
										</button>{/if}
									</div>
								</header>
								<div class="learned-apis-badges">
									<span class="api-chip">{origin.capability_count} capabilit{origin.capability_count !== 1 ? 'ies' : 'y'}</span>
									{#if learnedApiSearchTokens.length > 0}
										<span class="api-chip api-chip--info">{view.filteredCapabilities.length} matching</span>
									{/if}
									<span class="api-chip">{origin.trace_count} trace{origin.trace_count !== 1 ? 's' : ''}</span>
								</div>

								{#if site && expandedSiteIds.has(site.site)}
									<section class="api-detail-drawer">
										<h4>Learned workflows</h4>
										<p class="routing-caption">Read-only multi-step API paths compiled for this site.</p>
										{#if siteWorkflowsLoading.has(site.site)}
											<p class="routing-caption">Loading workflows...</p>
										{:else if siteWorkflowsErrors[site.site]}
											<div class="api-alert api-alert--error" role="alert">Could not load workflows: {siteWorkflowsErrors[site.site]}</div>
										{:else if (siteWorkflows[site.site] ?? []).length === 0}
											<p class="routing-caption">No multi-step workflows have been compiled for this site yet.</p>
										{:else}
											{#each siteWorkflows[site.site] as workflow (`${workflow.origin_key}-${workflow.id}`)}
												<div class="api-item-header">
													<div><strong>{workflow.name}</strong><p>{workflow.origin_url} · compiled {formatRecipeTime(workflow.last_compiled_at_ms)}</p></div>
													<div class="api-chip-row"><span class={`api-chip api-chip--${confidenceColor(workflow.maturity)}`}>{workflow.maturity}</span><span class="api-chip">{workflow.step_count} step{workflow.step_count === 1 ? '' : 's'}</span></div>
												</div>
											{/each}
										{/if}
									</section>
								{/if}

								{#if view.filteredCapabilities.length === 0}
									<div class="api-empty api-empty-inline">
										<h4>No capabilities</h4>
										<p>This origin has no capability rows yet.</p>
									</div>
								{:else}
									<div class="learned-apis-pagination learned-apis-pagination-top">
										<ServerPager
											currentPage={view.page + 1}
											pageCount={view.totalPages}
											startItem={view.pageStart}
											endItem={view.pageEnd}
											totalItems={view.filteredCapabilities.length}
											ariaLabel={`${origin.origin_url} capabilities pagination`}
											on:pagechange={(event) => setLearnedApisPagerPage(origin.origin_key, event.detail.page)}
										/>
									</div>
									<div class="learned-apis-table-wrap">
										<table class="learned-apis-table">
											<thead>
												<tr>
													<th>Method</th>
												<th>URL Template</th>
												<th>Confidence</th>
												<th>Side Effects</th>
												<th>Samples</th>
													<th>Actions</th>
												</tr>
											</thead>
											<tbody>
											{#each view.visibleCapabilities as capability (capability.id)}
												{@const result = replayResults[capability.id]}
												{@const capabilityRows = projections.filter((projection) => projection.capability_id === capability.id)}
												<tr>
														<td>
															<span class={`api-chip api-chip--${methodColor(capability.method)}`}>{capability.method}</span>
														</td>
													<td class="learned-apis-url">
														{capability.url_template}
														<div class="api-chip-row"><span class={`api-chip api-chip--${relevanceColor(capability.relevance)}`}>{relevanceLabel(capability.relevance)}</span>{#each capability.used_by_recipe_ids ?? [] as recipeId (recipeId)}<span class="api-chip" title={recipeId}>used by {recipeLabel(recipeId)}</span>{/each}</div>
													</td>
														<td>
															<span class={`api-chip api-chip--${confidenceColor(capability.confidence)}`}>{capability.confidence}</span>
														</td>
														<td>{capability.side_effects}</td>
														<td>{capability.sample_count}</td>
														<td>
															<div class="learned-apis-row-actions">
																<button
																	class="api-button api-button-compact"
																	type="button"
																	disabled={replayingCapabilityIds.has(capability.id)}
															on:click={() => handleReplay(capability.origin_key ?? origin.origin_key, capability.id)}
																>
																	{replayingCapabilityIds.has(capability.id) ? 'Replaying...' : 'Replay'}
																</button>
																<button
																	class="api-button api-button-compact"
																	type="button"
															on:click={() => openEditReplay(capability.origin_key ?? origin.origin_key, capability.id)}
														>
															Edit & Replay
														</button>
														<button class="api-button api-button-compact" type="button" on:click={() => toggleCapabilityDetails(capability.id)}>{expandedCapabilityIds.has(capability.id) ? 'Hide details' : 'Details'}</button>
															</div>
														</td>
													</tr>
													{#if result}
														<tr class="learned-apis-result-row">
															<td colspan="6">
																<section class="learned-apis-replay">
																	<header>
																		<h4>Replay: {capability.method} {capability.url_template}</h4>
																		<div class="learned-apis-badges">
																			<span class={`api-chip api-chip--${replayStatusColor(result)}`}>{replayStatusText(result)}</span>
																			<span class="api-chip">{result.elapsed_ms}ms</span>
																		</div>
																	</header>
																	{#if result.auth_was_stale}
																		<div class="api-alert api-alert--warning">Auth credentials may be stale (401/403 response)</div>
																	{/if}
																	{#if result.error}
																		<div class="api-alert api-alert--error" role="alert">{result.error}</div>
																	{/if}
																	{#if result.body}
																		<pre class={`api-code api-code-${replayBodyLanguage(result.body)}`}><code>{formatReplayBody(result.body)}</code></pre>
																	{/if}
																</section>
															</td>
														</tr>
												{/if}
												{#if expandedCapabilityIds.has(capability.id)}
													<tr class="learned-apis-result-row">
														<td colspan="6">
															<section class="api-detail-drawer">
																<h4>Cached rows</h4>
																{#if projectionsLoading}
																	<p class="routing-caption">Loading cached resources...</p>
																{:else if projectionsError}
																	<div class="api-alert api-alert--error" role="alert">Could not load cached resources: {projectionsError}</div>
																{:else if capabilityRows.length === 0}
																	<p class="routing-caption">No learned resource projection is linked to this capability.</p>
																{:else}
																	{#each capabilityRows as projection (projection.id)}
																		<div class="api-item-header"><span>{projection.resource_label} · {projection.row_count} rows · {projection.lifecycle}</span><div class="api-action-row">{#if projection.lifecycle === 'pending'}<button class="api-button api-button-compact" type="button" disabled={projectionsActing.has(projection.id)} on:click={() => runProjectionAction(projection.id, 'approve')}>Approve</button>{/if}<button class="api-button api-button-compact" type="button" disabled={projectionsActing.has(projection.id)} on:click={() => runProjectionAction(projection.id, 'purge-rows')}>Purge rows</button></div></div>
																	{/each}
																{/if}
															</section>
														</td>
													</tr>
												{/if}
											{/each}
											</tbody>
										</table>
									</div>
									<div class="learned-apis-pagination">
										<ServerPager
											currentPage={view.page + 1}
											pageCount={view.totalPages}
											startItem={view.pageStart}
											endItem={view.pageEnd}
											totalItems={view.filteredCapabilities.length}
											ariaLabel={`${origin.origin_url} capabilities pagination`}
											on:pagechange={(event) => setLearnedApisPagerPage(origin.origin_key, event.detail.page)}
										/>
									</div>
								{/if}
							</section>
						{/each}
					</div>
				{/if}
			</div>
		{:else if activeTab === 2}
			<!-- Routing Activity tile: live counters from /router-metrics + /projection-metrics. -->
			<div use:ensureMetricsLoaded class="routing-activity">
				<section class="api-section api-section-header">
					<div>
						<p class="api-overline">Live counters</p>
						<h2>Routing Activity</h2>
						<p>Operational API-mining counters from the active process. They reset when the backend restarts.</p>
					</div>
					<button class="api-button api-button-secondary" type="button" disabled={metricsLoading} on:click={fetchMetrics}>
						{metricsLoading ? 'Refreshing...' : 'Refresh'}
					</button>
				</section>
				{#if metricsError}
					<div class="api-alert api-alert--error" role="alert">Failed to load metrics: {metricsError}</div>
				{/if}
				{#if metricsLoading && !routerSnapshot}
					<div class="api-alert api-alert--info">Loading metrics...</div>
				{/if}
				{#if routerSnapshot}
					<section class="routing-card">
						<h4>Router outcomes</h4>
						<p class="routing-caption">Counts since process start. Reset on backend restart.</p>
						<div class="routing-badges">
							<span class="routing-badge routing-badge-success">replayed: {routerSnapshot.replayed}</span>
							<span class="routing-badge routing-badge-warning">refused: {routerSnapshot.refused_navigate}</span>
							<span class="routing-badge">pass_through_disabled: {routerSnapshot.pass_through_router_disabled}</span>
							<span class="routing-badge">pass_through_no_binding: {routerSnapshot.pass_through_no_binding}</span>
							<span class="routing-badge">pass_through_no_capability: {routerSnapshot.pass_through_no_capability}</span>
							<span class="routing-badge">pass_through_low_conf: {routerSnapshot.pass_through_low_confidence}</span>
							<span class="routing-badge">pass_through_not_replayable: {routerSnapshot.pass_through_not_replayable}</span>
							<span class="routing-badge">pass_through_session_not_ready: {routerSnapshot.pass_through_session_not_ready}</span>
							<span class="routing-badge routing-badge-error">pass_through_panic: {routerSnapshot.pass_through_router_panic}</span>
							<span class="routing-badge">pass_through_other: {routerSnapshot.pass_through_other}</span>
						</div>
					</section>
				{/if}
				{#if passiveValidationSnapshot}
					<section class="routing-card">
						<h4>Passive validation</h4>
						<p class="routing-caption">Background XHR/Fetch comparisons. Counts since process start.</p>
						<div class="routing-badges">
							<span class="routing-badge">observed_xhr_fetch: {passiveValidationSnapshot.observed_xhr_fetch}</span>
							<span class="routing-badge">matched: {passiveValidationSnapshot.matched_capabilities}</span>
							<span class="routing-badge">fired: {passiveValidationSnapshot.validations_fired}</span>
							<span class="routing-badge routing-badge-success">passed: {passiveValidationSnapshot.validations_passed}</span>
							<span class="routing-badge routing-badge-warning">failed: {passiveValidationSnapshot.validations_failed}</span>
							<span class="routing-badge">skipped_no_match: {passiveValidationSnapshot.skipped_no_match}</span>
							<span class="routing-badge">skipped_non_xhr_fetch: {passiveValidationSnapshot.skipped_non_xhr_fetch}</span>
							<span class="routing-badge routing-badge-warning">skipped_not_first_party: {passiveValidationSnapshot.skipped_not_first_party}</span>
							<span class="routing-badge routing-badge-warning">skipped_policy: {passiveValidationSnapshot.skipped_policy}</span>
							<span class="routing-badge">skipped_missing_response: {passiveValidationSnapshot.skipped_missing_response}</span>
							<span class="routing-badge routing-badge-success">promoted_validated: {passiveValidationSnapshot.promoted_to_validated}</span>
							<span class="routing-badge routing-badge-success">promoted_trusted: {passiveValidationSnapshot.promoted_to_trusted}</span>
							<span class="routing-badge routing-badge-warning">budget_exhausted: {passiveValidationSnapshot.budget_exhausted}</span>
							<span class="routing-badge routing-badge-error">errors: {passiveValidationSnapshot.errors}</span>
						</div>
					</section>
				{/if}
				{#if recipeSnapshot}
					<section class="routing-card">
						<h4>Task recipe rail</h4>
						<p class="routing-caption">Task-start lookup, browserless replay, recovery, approval, and auth-heal counters.</p>
						<div class="routing-badges">
							<span class="routing-badge routing-badge-success">lookup_hit: {recipeSnapshot.lookup_hit}</span>
							<span class="routing-badge">lookup_miss: {recipeSnapshot.lookup_miss}</span>
							<span class="routing-badge routing-badge-success">replay_succeeded: {recipeSnapshot.replay_succeeded}</span>
							<span class="routing-badge routing-badge-warning">fallback_handoffs: {recipeSnapshot.fallback_handoffs}</span>
							<span class="routing-badge routing-badge-warning">schema_drift: {recipeSnapshot.replay_failed_schema_drift}</span>
							<span class="routing-badge routing-badge-error">policy_failures: {recipeSnapshot.replay_failed_policy}</span>
							<span class="routing-badge">grants_used: {recipeSnapshot.grants_used}</span>
							<span class="routing-badge routing-badge-success">auth_heals: {recipeSnapshot.auth_heals}</span>
						</div>
					</section>
				{/if}
				{#if registryHealthSnapshot}
					<section class="routing-card">
						<h4>Registry health</h4>
						<p class="routing-caption">Index integrity and takeover-readiness for the active scope.</p>
						<div class="routing-badges">
							<span class="routing-badge">origins: {registryHealthSnapshot.origin_count}</span>
							<span class="routing-badge">indexed: {registryHealthSnapshot.indexed_capability_count}</span>
							<span class="routing-badge routing-badge-success">loadable: {registryHealthSnapshot.loadable_capability_count}</span>
							<span class="routing-badge routing-badge-warning">stale_index: {registryHealthSnapshot.stale_index_count}</span>
							<span class="routing-badge routing-badge-warning">unindexed: {registryHealthSnapshot.unindexed_capability_count}</span>
							<span class="routing-badge">replayable: {registryHealthSnapshot.replayable_capability_count}</span>
							<span class="routing-badge">action_bindings: {registryHealthSnapshot.action_bindings_count}</span>
							<span class="routing-badge routing-badge-success">takeover_ready: {registryHealthSnapshot.takeover_ready_bindings_count}</span>
						</div>
						{#if registryHealthSnapshot.version_mismatch || registryHealthSnapshot.stale_index_count > 0 || registryHealthSnapshot.unindexed_capability_count > 0}
							<p class="routing-caption">
								Registry drift detected. The backend repairs drift when the registry opens; refresh after restart or router reload to confirm.
							</p>
						{/if}
						{#if registryHealthSnapshot.warnings?.length}
							<div class="routing-warning-stack">
								{#each registryHealthSnapshot.warnings as warning (warning.code)}
									<p class="routing-warning" title={warning.code}>{warning.message}</p>
								{/each}
							</div>
						{/if}
						{#if registryHealthSnapshot.origin_readiness.length > 0}
							<div class="routing-badges">
								{#each registryHealthSnapshot.origin_readiness.slice(0, 6) as origin (origin.origin_key)}
									<span
										class="routing-badge"
										class:routing-badge-success={origin.inactive_reasons.length === 0}
										class:routing-badge-warning={origin.inactive_reasons.length > 0}
										title={[
											origin.replay_mode ? `mode: ${origin.replay_mode}` : null,
											...origin.inactive_reasons
										]
											.filter(Boolean)
											.join(', ') || 'ready'}
									>
										{originHostLabel(origin.origin_url)}: {origin.takeover_ready_bindings_count}/{origin.action_bindings_count} ready
									</span>
								{/each}
							</div>
						{/if}
					</section>
				{/if}
				{#if projectionSnapshot}
					<section class="routing-card">
						<h4>Projection pipeline</h4>
						<p class="routing-caption">Per-scope projection store activity. Counts since process start.</p>
						<div class="routing-badges">
							<span class="routing-badge">pending_created: {projectionSnapshot.pending_created}</span>
							<span class="routing-badge routing-badge-success">approved: {projectionSnapshot.approved}</span>
							<span class="routing-badge">rows_ingested: {projectionSnapshot.rows_ingested}</span>
							<span class="routing-badge routing-badge-success">rows_served_from_store: {projectionSnapshot.rows_served_from_store}</span>
							<span class="routing-badge routing-badge-warning">stale_served: {projectionSnapshot.stale_served}</span>
							<span class="routing-badge">schema_converged: {projectionSnapshot.schema_converged}</span>
							<span class="routing-badge routing-badge-error">migration_rejected: {projectionSnapshot.migration_rejected}</span>
							<span class="routing-badge">rows_purged: {projectionSnapshot.rows_purged}</span>
						</div>
					</section>
				{/if}
				<p class="routing-caption">Auto-refreshes every 5s while this tab is visible.</p>
			</div>
		{:else if activeTab === 3}
			<div class="api-tab-stack" use:ensureApisLoaded>
				<section class="api-section api-section-header">
					<div>
						<p class="api-overline">Captured session material</p>
						<h2>Authentication</h2>
						<p>Credential values stay encrypted and never appear here. Refresh opens a bounded secure browser session and verifies a safe request when policy permits.</p>
					</div>
					<div class="api-section-actions">
						<span class="api-chip api-chip--info">{overviewOrigins.length} origin{overviewOrigins.length === 1 ? '' : 's'}</span>
						<button
							class="api-button api-button-secondary"
							type="button"
							disabled={overviewLoading}
							on:click={fetchOverview}
						>
							{overviewLoading ? 'Refreshing...' : 'Refresh'}
						</button>
					</div>
				</section>
				{#if overviewError}
					<div class="api-alert api-alert--error" role="alert">{overviewError}</div>
				{:else if overviewLoading && overviewOrigins.length === 0}
					<div class="api-alert api-alert--info">Loading authentication status...</div>
				{:else if overviewOrigins.length === 0}
					<div class="api-empty">
						<h3>No learned origins yet</h3>
						<p>Complete a browser-backed task before capturing replay authentication.</p>
					</div>
				{:else}
					<div class="api-card-list">
						{#each overviewOrigins as origin (origin.origin_key)}
							{@const auth = authStatuses[origin.origin_key]}
							{@const refresh = authRefreshStatuses[origin.origin_key]}
							<section class="api-section learned-apis-origin">
								<header class="api-item-header">
									<div>
										<h3>{origin.origin_url}</h3>
										<p>{origin.capability_count} capabilities · {origin.trace_count} traces</p>
									</div>
									<button class="api-button api-button-warning" type="button" disabled={refreshingAuthOriginKeys.has(origin.origin_key)} on:click={() => handleRefreshAuth(origin)}>{authRefreshButtonLabel(origin.origin_key, auth?.has_auth ?? false)}</button>
								</header>
								<div class="api-chip-row">
									{#each authBadges(auth) as badge (`${origin.origin_key}-${badge.text}`)}<span class={`api-chip api-chip--${badge.color}`} title={badge.title ?? badge.text}>{badge.text}</span>{/each}
								</div>
								{#if refresh}<div class:auth-refresh-status--error={refresh.phase === 'failed' || refresh.phase === 'timed_out' || refresh.phase === 'verification_failed'} class:auth-refresh-status--success={refresh.phase === 'verified' || refresh.phase === 'captured_unverified'} class="auth-refresh-status" role="status" aria-live="polite">{#if !refresh.terminal}<span class="auth-refresh-spinner" aria-hidden="true"></span>{/if}<div><strong>{authRefreshLabel(refresh.phase)}</strong><p>{refresh.message}</p></div>{#if refresh.verification_status}<span class="api-chip">HTTP {refresh.verification_status}</span>{/if}</div>{/if}
							</section>
						{/each}
					</div>
				{/if}
			</div>
		{/if}
	</section>

	{#if purgeConfirmationOpen}
		<div class="api-modal-backdrop" role="presentation" on:click={handlePurgeConfirmationBackdropClick}>
			<div
				class="api-modal api-modal-confirm"
				role="dialog"
				aria-modal="true"
				aria-labelledby="purge-api-mining-title"
			>
				<header class="api-modal-header">
					<div>
						<h2 id="purge-api-mining-title">Turn off and delete learned data?</h2>
						<p>This permanently deletes recipes, grants, projections, captured auth, traces, capabilities, sequences, and workflows for this workspace.</p>
					</div>
					<button class="api-icon-button" type="button" aria-label="Close confirmation" disabled={apiMiningSettingsLoading} on:click={() => (purgeConfirmationOpen = false)}>x</button>
				</header>
				<label class="api-field" for="purge-api-mining-confirmation">
					<span>Type <strong>delete learned data</strong> to confirm</span>
					<input id="purge-api-mining-confirmation" autocomplete="off" bind:value={purgeConfirmation} />
				</label>
				<div class="api-form-actions">
					<button class="api-button" type="button" disabled={apiMiningSettingsLoading} on:click={() => (purgeConfirmationOpen = false)}>Cancel</button>
					<button class="api-button api-button-warning" type="button" disabled={apiMiningSettingsLoading || purgeConfirmation !== 'delete learned data'} on:click={disableAndPurgeApiMining}>
						{apiMiningSettingsLoading ? 'Deleting...' : 'Turn off and delete'}
					</button>
				</div>
			</div>
		</div>
	{/if}

	<SwaggerModal
		bind:open={swaggerOpen}
		originKey={swaggerOriginKey}
		originUrl={swaggerOriginUrl}
		on:close={() => (swaggerOpen = false)}
	/>

	{#if editReplayOpen}
		<div class="api-modal-backdrop" role="presentation" on:click={handleEditReplayBackdropClick}>
			<div
				class="api-modal"
				role="dialog"
				aria-modal="true"
				aria-labelledby="edit-replay-title"
				tabindex="-1"
			>
				<header class="api-modal-header">
					<div>
						<h2 id="edit-replay-title">Edit & Replay</h2>
						<p>Override request fields for a single replay attempt.</p>
					</div>
					<button class="api-icon-button" type="button" aria-label="Close edit replay" on:click={() => (editReplayOpen = false)}>
						x
					</button>
				</header>
				<div class="api-form-stack">
					<label class="api-field" for="edit-replay-url">
						<span>URL Template</span>
						<input id="edit-replay-url" value={editReplayUrl} readonly />
					</label>
					<label class="api-field" for="edit-replay-headers">
						<span>Headers (JSON)</span>
						<textarea id="edit-replay-headers" rows="4" bind:value={editReplayHeaders}></textarea>
					</label>
					<label class="api-field" for="edit-replay-body">
						<span>Body</span>
						<textarea id="edit-replay-body" rows="6" bind:value={editReplayBody}></textarea>
					</label>
					<div class="api-form-actions">
						<button
							class="api-button api-button-primary"
							type="button"
							disabled={editReplayLoading}
							on:click={submitEditReplay}
						>
							{editReplayLoading ? 'Sending...' : 'Send'}
						</button>
					</div>
				</div>

				{#if editReplayResult}
					<section class="api-replay-result">
						<div class="api-chip-row">
							<span class={`api-chip api-chip--${editReplayStatusColor}`}>{editReplayStatusText}</span>
							<span class="api-chip">{editReplayResult.elapsed_ms}ms</span>
						</div>
						{#if editReplayResult.auth_was_stale}
							<div class="api-alert api-alert--warning">Auth credentials may be stale (401/403 response)</div>
						{/if}
						{#if editReplayResult.error}
							<div class="api-alert api-alert--error" role="alert">{editReplayResult.error}</div>
						{/if}
						{#if editReplayResult.body}
							<pre class={`api-code api-code-${editReplayBodyLanguage}`}><code>{editReplayFormattedBody}</code></pre>
						{/if}
					</section>
				{/if}
			</div>
		</div>
	{/if}
</div>

<style>
	.api-mining-page {
		box-sizing: border-box;
		color: var(--text-primary);
		display: flex;
		flex-direction: column;
		gap: 1rem;
		margin: 0 auto;
		max-width: var(--app-content-max, 1320px);
		padding: 1.35rem 1.45rem 5rem;
		width: 100%;
	}

	.api-hero,
	.api-tabs,
	.api-panel,
	.api-section,
	.api-empty,
	.routing-card,
	.learned-apis-replay {
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
	}

	.api-hero {
		align-items: flex-start;
		display: flex;
		gap: 1rem;
		justify-content: space-between;
		padding: 1rem;
		background:
			radial-gradient(circle at top right, color-mix(in srgb, var(--accent-primary) 12%, transparent), transparent 42%),
			linear-gradient(180deg, color-mix(in srgb, var(--bg-card) 94%, var(--bg-soft) 6%), var(--bg-card));
	}

	.api-hero-copy {
		max-width: 68rem;
	}

	.api-overline,
	.api-hero h1,
	.api-hero p,
	.api-section h2,
	.api-section h3,
	.api-section h4,
	.api-section p,
	.api-empty h3,
	.api-empty h4,
	.api-empty p,
	.routing-card h4,
	.routing-card p,
	.api-modal h2,
	.api-modal p,
	.api-mono {
		letter-spacing: 0;
		margin: 0;
	}

	.api-overline {
		color: var(--text-secondary);
		font-size: 0.74rem;
		font-weight: 760;
		text-transform: uppercase;
	}

	.api-hero h1 {
		font-family: var(--font-display, var(--font-primary));
		font-size: 2.25rem;
		font-weight: 700;
		line-height: 1.1;
		margin: 0.25rem 0 0.55rem;
	}

	.api-hero p,
	.api-section p,
	.api-empty p,
	.routing-caption,
	.api-modal p {
		color: var(--text-secondary);
		font-size: 0.9rem;
		line-height: 1.5;
	}

	.api-section h2 {
		font-size: 1.08rem;
		line-height: 1.25;
	}

	.api-section h3,
	.routing-card h4 {
		color: var(--text-primary);
		font-size: 0.98rem;
		line-height: 1.28;
		overflow-wrap: anywhere;
	}

	.api-section h4 {
		color: var(--text-primary);
		font-size: 0.9rem;
		line-height: 1.28;
	}

	.api-tabs {
		display: flex;
		flex-wrap: wrap;
		gap: 0.4rem;
		padding: 0.45rem;
	}

	.api-tab {
		background: transparent;
		border: 1px solid transparent;
		border-radius: 6px;
		color: var(--text-secondary);
		cursor: pointer;
		font: inherit;
		font-size: 0.84rem;
		font-weight: 680;
		min-height: 2.1rem;
		padding: 0.45rem 0.7rem;
	}

	.api-tab:hover,
	.api-tab-active {
		background: color-mix(in srgb, var(--accent-primary) 12%, var(--bg-soft));
		border-color: color-mix(in srgb, var(--accent-primary) 28%, var(--border-soft));
		color: var(--text-primary);
	}

	.api-panel {
		padding: 1rem;
	}

	.api-tab-stack,
	.learned-apis-tab,
	.routing-activity,
	.api-card-list,
	.learned-apis-origin-list {
		display: grid;
		gap: 1rem;
	}

	.api-section,
	.api-empty,
	.learned-apis-replay,
	.routing-card {
		display: grid;
		gap: 0.75rem;
		padding: 0.9rem;
	}

	.api-section-header,
	.learned-apis-origin-header,
	.learned-apis-replay header,
	.api-item-header,
	.api-modal-header {
		align-items: flex-start;
		display: flex;
		gap: 0.75rem;
		justify-content: space-between;
	}

	.learned-apis-summary {
		align-items: start;
		grid-template-columns: minmax(0, 1fr) auto;
	}

	.api-section-actions,
	.learned-apis-stats {
		align-items: center;
		display: flex;
		flex-wrap: wrap;
		gap: 0.55rem;
		justify-content: flex-end;
	}

	.learned-apis-controls,
	.learned-apis-pagination {
		align-items: center;
		display: flex;
		flex-wrap: wrap;
		gap: 0.55rem;
	}

	.learned-apis-controls {
		border-top: 1px solid var(--border-soft);
		grid-column: 1 / -1;
		padding-top: 0.85rem;
	}

	.api-detail-drawer {
		background: color-mix(in srgb, var(--bg-card) 88%, var(--bg-soft) 12%);
		border: 1px solid var(--border-soft);
		border-radius: 7px;
		display: grid;
		gap: 0.8rem;
		margin-top: 0.75rem;
		padding: 0.85rem;
	}

	.learned-apis-controls .api-chip {
		cursor: pointer;
	}

	.learned-apis-pagination {
		justify-content: flex-end;
	}

	.api-chip-row,
	.learned-apis-badges,
	.learned-apis-origin-actions,
	.learned-apis-row-actions,
	.api-action-row,
	.routing-badges {
		align-items: center;
		display: flex;
		flex-wrap: wrap;
		gap: 0.45rem;
	}

	.api-chip,
	.routing-badge {
		align-items: center;
		background: color-mix(in srgb, var(--bg-card) 88%, var(--bg-soft) 12%);
		border: 1px solid var(--border-soft);
		border-radius: 999px;
		color: var(--text-secondary);
		display: inline-flex;
		font-size: 0.72rem;
		font-weight: 740;
		line-height: 1.2;
		max-width: 100%;
		min-height: 1.55rem;
		padding: 0.25rem 0.52rem;
		white-space: normal;
		word-break: normal;
		overflow-wrap: normal;
	}

	.api-chip--success,
	.routing-badge-success {
		background: color-mix(in srgb, var(--success, var(--color-success, #12805c)) 14%, transparent);
		border-color: color-mix(in srgb, var(--success, var(--color-success, #12805c)) 34%, var(--border-soft));
		color: color-mix(in srgb, var(--success, var(--color-success, #12805c)) 76%, var(--text-primary));
	}

	.api-chip--warning,
	.routing-badge-warning {
		background: color-mix(in srgb, var(--warning, var(--color-warning, #b7791f)) 14%, transparent);
		border-color: color-mix(in srgb, var(--warning, var(--color-warning, #b7791f)) 34%, var(--border-soft));
		color: color-mix(in srgb, var(--warning, var(--color-warning, #b7791f)) 82%, var(--text-primary));
	}

	.api-chip--error,
	.routing-badge-error {
		background: color-mix(in srgb, var(--danger, var(--color-error, #c2410c)) 14%, transparent);
		border-color: color-mix(in srgb, var(--danger, var(--color-error, #c2410c)) 34%, var(--border-soft));
		color: color-mix(in srgb, var(--danger, var(--color-error, #c2410c)) 82%, var(--text-primary));
	}

	.api-chip--info {
		background: color-mix(in srgb, var(--accent-primary) 12%, transparent);
		border-color: color-mix(in srgb, var(--accent-primary) 28%, var(--border-soft));
		color: color-mix(in srgb, var(--accent-primary) 76%, var(--text-primary));
	}

	.api-button {
		align-items: center;
		background: color-mix(in srgb, var(--bg-card) 92%, var(--bg-soft) 8%);
		border: 1px solid var(--border-soft);
		border-radius: 6px;
		color: var(--text-primary);
		cursor: pointer;
		display: inline-flex;
		font: inherit;
		font-size: 0.82rem;
		font-weight: 680;
		justify-content: center;
		line-height: 1.2;
		min-height: 2.15rem;
		padding: 0.5rem 0.75rem;
		white-space: nowrap;
	}

	.api-button:hover:not(:disabled) {
		background: color-mix(in srgb, var(--accent-primary) 10%, var(--bg-card));
		border-color: color-mix(in srgb, var(--accent-primary) 28%, var(--border-soft));
	}

	.api-button-primary {
		background: var(--accent-primary);
		border-color: var(--accent-primary);
		color: var(--accent-on-primary, var(--button-primary-color, #fff));
	}

	.api-button-secondary {
		background: color-mix(in srgb, var(--accent-primary) 12%, var(--bg-soft));
		border-color: color-mix(in srgb, var(--accent-primary) 28%, var(--border-soft));
		color: var(--text-primary);
	}

	.api-button-warning {
		border-color: color-mix(in srgb, var(--warning, var(--color-warning, #b7791f)) 42%, var(--border-soft));
		color: color-mix(in srgb, var(--warning, var(--color-warning, #b7791f)) 82%, var(--text-primary));
	}

	.api-button-compact {
		font-size: 0.76rem;
		min-height: 1.85rem;
		padding: 0.36rem 0.55rem;
	}

	.api-button:disabled {
		cursor: not-allowed;
		opacity: 0.58;
	}

	.learned-apis-table-wrap {
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		overflow: hidden;
		width: 100%;
	}

	.learned-apis-table {
		border-collapse: collapse;
		font-size: 0.79rem;
		table-layout: fixed;
		width: 100%;
	}

	.learned-apis-table th,
	.learned-apis-table td {
		border-bottom: 1px solid var(--border-soft);
		overflow-wrap: anywhere;
		padding: 0.58rem 0.65rem;
		text-align: left;
		vertical-align: top;
	}

	.learned-apis-table th {
		background: color-mix(in srgb, var(--bg-card) 90%, var(--bg-soft) 10%);
		color: var(--text-secondary);
		font-size: 0.69rem;
		font-weight: 760;
		line-height: 1.2;
		text-transform: uppercase;
		white-space: nowrap;
	}

	.learned-apis-table tr:last-child td {
		border-bottom: 0;
	}

	.learned-apis-result-row td {
		background: color-mix(in srgb, var(--bg-card) 88%, var(--bg-soft) 12%);
		padding: 0.7rem;
	}

	.learned-apis-table th:nth-child(1),
	.learned-apis-table td:nth-child(1) {
		width: 76px;
	}

	.learned-apis-table th:nth-child(3),
	.learned-apis-table td:nth-child(3) {
		width: 105px;
	}

	.learned-apis-table th:nth-child(4),
	.learned-apis-table td:nth-child(4) {
		width: 116px;
	}

	.learned-apis-table th:nth-child(5),
	.learned-apis-table td:nth-child(5) {
		width: 70px;
	}

	.learned-apis-table th:nth-child(6),
	.learned-apis-table td:nth-child(6) {
		width: 158px;
	}

	.learned-apis-url,
	.api-mono {
		color: var(--text-primary);
		font-family: var(--font-mono, ui-monospace, SFMono-Regular, Menlo, monospace);
		overflow-wrap: anywhere;
	}

	.learned-apis-replay,
	.api-empty-inline {
		background: color-mix(in srgb, var(--bg-card) 90%, var(--bg-soft) 10%);
		box-shadow: none;
	}

	.api-empty {
		align-items: flex-start;
		background: color-mix(in srgb, var(--bg-card) 90%, var(--bg-soft) 10%);
	}

	.api-empty-inline {
		border-style: dashed;
	}

	.auth-refresh-status {
		align-items: center;
		background: color-mix(in srgb, var(--accent-primary) 8%, var(--bg-card));
		border: 1px solid color-mix(in srgb, var(--accent-primary) 24%, var(--border-soft));
		border-radius: 6px;
		color: var(--text-primary);
		display: grid;
		gap: 0.7rem;
		grid-template-columns: auto minmax(0, 1fr) auto;
		padding: 0.65rem 0.75rem;
	}

	.auth-refresh-status strong {
		display: block;
		font-size: 0.8rem;
		line-height: 1.3;
	}

	.auth-refresh-status p {
		color: var(--text-secondary);
		font-size: 0.76rem;
		line-height: 1.4;
		margin: 0.15rem 0 0;
	}

	.auth-refresh-status--success {
		background: color-mix(in srgb, var(--success, var(--color-success, #12805c)) 9%, var(--bg-card));
		border-color: color-mix(in srgb, var(--success, var(--color-success, #12805c)) 28%, var(--border-soft));
	}

	.auth-refresh-status--error {
		background: color-mix(in srgb, var(--danger, var(--color-error, #c2410c)) 8%, var(--bg-card));
		border-color: color-mix(in srgb, var(--danger, var(--color-error, #c2410c)) 28%, var(--border-soft));
	}

	.auth-refresh-spinner {
		animation: auth-refresh-spin 0.8s linear infinite;
		border: 2px solid color-mix(in srgb, var(--accent-primary) 24%, transparent);
		border-radius: 50%;
		border-top-color: var(--accent-primary);
		display: block;
		height: 1rem;
		width: 1rem;
	}

	@keyframes auth-refresh-spin {
		to {
			transform: rotate(360deg);
		}
	}

	.api-alert {
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		color: var(--text-primary);
		font-size: 0.84rem;
		line-height: 1.45;
		padding: 0.72rem 0.82rem;
	}

	.api-alert--info {
		background: color-mix(in srgb, var(--accent-primary) 10%, var(--bg-card));
		border-color: color-mix(in srgb, var(--accent-primary) 28%, var(--border-soft));
	}

	.api-alert--success {
		background: color-mix(in srgb, var(--success, var(--color-success, #12805c)) 11%, var(--bg-card));
		border-color: color-mix(in srgb, var(--success, var(--color-success, #12805c)) 30%, var(--border-soft));
	}

	.api-alert--warning {
		background: color-mix(in srgb, var(--warning, var(--color-warning, #b7791f)) 12%, var(--bg-card));
		border-color: color-mix(in srgb, var(--warning, var(--color-warning, #b7791f)) 32%, var(--border-soft));
	}

	.api-alert--error {
		background: color-mix(in srgb, var(--danger, var(--color-error, #c2410c)) 10%, var(--bg-card));
		border-color: color-mix(in srgb, var(--danger, var(--color-error, #c2410c)) 32%, var(--border-soft));
	}

	.routing-card {
		background: color-mix(in srgb, var(--bg-card) 94%, var(--bg-soft) 6%);
	}

	.routing-badges {
		align-items: flex-start;
	}

	.routing-warning-stack {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
		margin-top: 0.25rem;
	}

	.routing-warning {
		background: color-mix(in srgb, var(--warning, var(--color-warning, #b7791f)) 12%, var(--bg-card));
		border: 1px solid color-mix(in srgb, var(--warning, var(--color-warning, #b7791f)) 34%, var(--border-soft));
		border-radius: 8px;
		color: var(--text-primary);
		font-size: 0.82rem;
		line-height: 1.45;
		margin: 0;
		padding: 0.62rem 0.75rem;
	}

	.api-code {
		background: color-mix(in srgb, var(--bg-card) 82%, var(--bg-soft) 18%);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		color: var(--text-primary);
		font-family: var(--font-mono, ui-monospace, SFMono-Regular, Menlo, monospace);
		font-size: 0.78rem;
		line-height: 1.45;
		margin: 0;
		max-height: 300px;
		overflow: auto;
		padding: 0.75rem;
		white-space: pre-wrap;
	}

	.api-modal-backdrop {
		align-items: flex-start;
		background: color-mix(in srgb, var(--bg-base, #000) 44%, transparent);
		display: flex;
		inset: 0;
		justify-content: center;
		overflow: auto;
		padding: 3rem 1rem;
		position: fixed;
		z-index: 1000;
	}

	.api-modal {
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		box-shadow: var(--shadow-lg, 0 18px 48px rgb(0 0 0 / 0.22));
		color: var(--text-primary);
		display: grid;
		gap: 1rem;
		max-width: 760px;
		padding: 1rem;
		width: min(100%, 760px);
	}

	.api-modal-confirm {
		max-width: 560px;
	}

	.api-modal-header {
		border-bottom: 1px solid var(--border-soft);
		padding-bottom: 0.75rem;
	}

	.api-icon-button {
		align-items: center;
		background: transparent;
		border: 1px solid var(--border-soft);
		border-radius: 6px;
		color: var(--text-primary);
		cursor: pointer;
		display: inline-flex;
		font: inherit;
		font-size: 0.9rem;
		font-weight: 760;
		height: 2rem;
		justify-content: center;
		line-height: 1;
		width: 2rem;
	}

	.api-form-stack,
	.api-replay-result {
		display: grid;
		gap: 0.75rem;
	}

	.api-field {
		color: var(--text-primary);
		display: grid;
		font-size: 0.82rem;
		font-weight: 680;
		gap: 0.35rem;
	}

	.api-field input,
	.api-field select,
	.api-field textarea {
		background: color-mix(in srgb, var(--bg-card) 92%, var(--bg-soft) 8%);
		border: 1px solid var(--border-soft);
		border-radius: 6px;
		color: var(--text-primary);
		font: inherit;
		font-size: 0.88rem;
		min-height: 2.25rem;
		padding: 0.55rem 0.65rem;
		width: 100%;
	}

	.api-field-inline {
		min-width: min(22rem, 100%);
	}

	.api-field-page-size {
		min-width: 6.5rem;
	}

	.api-field textarea {
		font-family: var(--font-mono, ui-monospace, SFMono-Regular, Menlo, monospace);
		line-height: 1.45;
		resize: vertical;
	}

	.api-field input[readonly] {
		color: var(--text-secondary);
	}

	.api-form-actions {
		display: flex;
		justify-content: flex-end;
	}

	@media (max-width: 880px) {
		.api-hero,
		.api-section-header,
		.learned-apis-origin-header,
		.learned-apis-replay header,
		.api-item-header {
			flex-direction: column;
		}

		.learned-apis-summary {
			grid-template-columns: minmax(0, 1fr);
		}

		.api-section-actions,
		.learned-apis-controls,
		.learned-apis-stats {
			align-items: flex-start;
			justify-content: flex-start;
		}
	}

	@media (max-width: 760px) {
		.api-mining-page {
			padding: 0.85rem 0.75rem 4.5rem;
		}

		.api-hero h1 {
			font-size: 1.85rem;
		}

		.api-tabs,
		.api-panel {
			padding: 0.65rem;
		}

		.api-tab,
		.api-button,
		.api-section-actions,
		.api-section-actions .api-button,
		.learned-apis-controls,
		.learned-apis-controls .api-field,
		.learned-apis-controls .api-button,
		.learned-apis-pagination,
		.learned-apis-origin-actions,
		.learned-apis-origin-actions .api-button,
		.learned-apis-row-actions,
		.learned-apis-row-actions .api-button {
			width: 100%;
		}

		.learned-apis-table thead {
			display: none;
		}

		.learned-apis-table,
		.learned-apis-table tbody,
		.learned-apis-table tr,
		.learned-apis-table td {
			display: block;
			width: 100%;
		}

		.learned-apis-table tr {
			border-bottom: 1px solid var(--border-soft);
			padding: 0.65rem;
		}

		.learned-apis-table tr:last-child {
			border-bottom: 0;
		}

		.learned-apis-table td {
			border-bottom: 0;
			padding: 0.25rem 0;
		}

		.api-modal-backdrop {
			padding: 1rem 0.75rem;
		}
	}
</style>
