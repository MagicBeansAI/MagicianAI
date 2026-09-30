<script lang="ts">
	import { browser } from '$app/environment';
	import { onMount } from 'svelte';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import ServerPager from '$lib/shared/components/ServerPager.svelte';
	import { formatRelativeTime } from '$lib/shared/formatRelativeTime';
	import { showError, showSuccess } from '$lib/shared/stores/notifications';
	import { timedFetch } from '$lib/shared/fetch';
	import {
		loadHarnessRuntimeStatus,
		setHarnessRuntimeEnabled,
		type HarnessRuntimeStatus
	} from '$lib/stores/agentStore';
	import { v2Events, type ConnectionStatus } from '$lib/realtime/v2-websocket';

	interface HarnessAnomaly {
		signature: string;
		agent_id: string;
		goal_id: string;
		kind: string;
		summary: string;
		detail?: string;
		status: string;
		occurrences: number;
		first_seen?: string;
		last_seen: string;
		last_fix_dispatch_at?: string;
		fix_task_id?: string;
	}

	interface CompanyLoopTarget {
		agentId: string;
		goalId: string;
		label: string;
		role: string;
	}

	interface CompanyLoopRunResult {
		key: string;
		label: string;
		agent_id: string;
		goal_id: string;
		status: string;
		detail: string;
		execution_id?: string;
		queue_position?: number;
	}

	type BadgeColor = 'default' | 'success' | 'warning' | 'error' | 'info';
	type StatusFilter = 'all' | 'open' | 'fix_dispatched' | 'resolved' | 'dismissed';
	type ViewMode = 'table' | 'cards';

	const companyLoopTargets: CompanyLoopTarget[] = [
		{
			agentId: 'cmo',
			goalId: 'harness:cmo:content-calendar',
			label: 'Content Calendar',
			role: 'Chief Marketing Officer'
		},
		{
			agentId: 'cro',
			goalId: 'harness:cro:weekly-gtm-review',
			label: 'Weekly GTM Review',
			role: 'Chief Revenue Officer'
		},
		{
			agentId: 'cpo',
			goalId: 'harness:cpo:backlog-grooming',
			label: 'Backlog Grooming',
			role: 'Chief Product Officer'
		},
		{
			agentId: 'cto',
			goalId: 'harness:cto:morning-engineering-standup',
			label: 'Engineering Standup',
			role: 'Chief Technology Officer'
		},
		{
			agentId: 'ceo',
			goalId: 'harness:ceo:morning-briefing',
			label: 'Executive Briefing',
			role: 'Chief Executive Officer'
		}
	];

	let anomalies: HarnessAnomaly[] = [];
	let loading = true;
	let error = '';
	let lastRefreshedAt: number | null = null;
	let connectionStatus: ConnectionStatus = 'disconnected';

	// Harness Runtime
	let harnessRuntime: HarnessRuntimeStatus | null = null;
	let harnessRuntimeLoading = true;
	let harnessRuntimeMutating = false;
	let harnessRuntimeError: string | null = null;

	// Company Loop Run State
	let companyLoopRunning = false;
	let companyLoopError = '';
	let companyLoopResults: CompanyLoopRunResult[] = [];
	let currentStageIndex = -1;

	// Anomalies Filters & Paging
	let searchQuery = '';
	let statusFilter: StatusFilter = 'all';
	let viewMode: ViewMode = 'table';
	let expandedSignatures = new Set<string>();

	const PAGE_SIZE_OPTIONS = [10, 25, 50];
	let pageSize = PAGE_SIZE_OPTIONS[0];
	let currentPage = 1;

	$: harnessEnabled = harnessRuntime?.enabled === true;
	$: harnessStatusLabel = harnessRuntimeLoading
		? 'Loading'
		: !harnessRuntime
			? 'Unavailable'
			: harnessEnabled
				? 'Cadence On'
				: 'Cadence Off';
	$: harnessStatusColor = (harnessRuntimeLoading
		? 'default'
		: !harnessRuntime
			? 'error'
			: harnessEnabled
				? 'success'
				: 'warning') as BadgeColor;

	// Metrics
	$: totalAnomaliesCount = anomalies.length;
	$: openAnomaliesCount = anomalies.filter((a) => a.status.toLowerCase() === 'open').length;
	$: fixDispatchedCount = anomalies.filter((a) => a.status.toLowerCase() === 'fix_dispatched').length;
	$: resolvedCount = anomalies.filter(
		(a) => a.status.toLowerCase() === 'resolved' || a.status.toLowerCase() === 'dismissed'
	).length;
	$: uniqueAgentsAffected = new Set(anomalies.map((a) => a.agent_id)).size;
	$: totalOccurrences = anomalies.reduce((acc, a) => acc + (a.occurrences || 1), 0);

	// Filtering
	let lastFilterKey = '';
	$: currentFilterKey = `${searchQuery}::${statusFilter}::${pageSize}`;
	$: if (currentFilterKey !== lastFilterKey) {
		lastFilterKey = currentFilterKey;
		currentPage = 1;
	}

	$: filteredAnomalies = anomalies.filter((a) => {
		if (statusFilter !== 'all' && a.status.toLowerCase() !== statusFilter) {
			return false;
		}
		if (!searchQuery.trim()) return true;
		const q = searchQuery.trim().toLowerCase();
		return [a.signature, a.agent_id, a.goal_id, a.kind, a.summary, a.detail, a.status]
			.filter(Boolean)
			.join(' ')
			.toLowerCase()
			.includes(q);
	});

	// Pagination
	$: totalFilteredItems = filteredAnomalies.length;
	$: pageCount = Math.max(1, Math.ceil(totalFilteredItems / pageSize));
	$: if (currentPage > pageCount) currentPage = Math.max(1, pageCount);
	$: startItem = totalFilteredItems === 0 ? 0 : (currentPage - 1) * pageSize + 1;
	$: endItem = totalFilteredItems === 0 ? 0 : Math.min(totalFilteredItems, (currentPage - 1) * pageSize + pageSize);
	$: pagedAnomalies = filteredAnomalies.slice(
		(currentPage - 1) * pageSize,
		currentPage * pageSize
	);

	function runKey(target: CompanyLoopTarget): string {
		return `${target.agentId}:${target.goalId}`;
	}

	function readString(payload: Record<string, unknown>, key: string): string {
		const value = payload[key];
		return typeof value === 'string' ? value : '';
	}

	function readNumber(payload: Record<string, unknown>, key: string): number | undefined {
		const value = payload[key];
		return typeof value === 'number' && Number.isFinite(value) ? value : undefined;
	}

	async function readApiError(response: Response): Promise<string> {
		try {
			const payload = (await response.json()) as Record<string, unknown>;
			const message = readString(payload, 'message') || readString(payload, 'error');
			const details = payload.details;
			if (message) return message;
			if (details && typeof details === 'object') {
				const reason = readString(details as Record<string, unknown>, 'reason');
				if (reason) return reason;
			}
		} catch {
			// Best-effort error body parsing.
		}
		return `HTTP ${response.status}`;
	}

	function replaceRunResult(next: CompanyLoopRunResult): void {
		companyLoopResults = companyLoopResults.map((result) =>
			result.key === next.key ? next : result
		);
	}

	function formatTimestamp(isoString: string | undefined): string {
		if (!isoString) return 'never';
		const ms = Date.parse(isoString);
		if (!Number.isFinite(ms)) return isoString;
		const rel = formatRelativeTime(ms);
		return rel ? `${rel} ago` : 'just now';
	}

	function formatAnomalyKind(kind: string): string {
		return kind.replace(/_/g, ' ');
	}

	function anomalyStatusBadgeClass(status: string): string {
		switch (status.toLowerCase()) {
			case 'open':
				return 'harness-badge harness-badge-error';
			case 'fix_dispatched':
				return 'harness-badge harness-badge-info';
			case 'resolved':
				return 'harness-badge harness-badge-success';
			case 'dismissed':
				return 'harness-badge harness-badge-default';
			default:
				return 'harness-badge harness-badge-default';
		}
	}

	function anomalyKindBadgeClass(kind: string): string {
		switch (kind.toLowerCase()) {
			case 'cycle_failed':
			case 'sandbox_denied':
				return 'kind-badge kind-badge--error';
			case 'stuck_hitl':
			case 'tool_unavailable':
				return 'kind-badge kind-badge--warn';
			case 'no_progress':
			case 'cycle_dropped':
			case 'roster_drift':
				return 'kind-badge kind-badge--info';
			default:
				return 'kind-badge';
		}
	}

	function resultStatusBadgeClass(status: string): string {
		switch (status.toLowerCase()) {
			case 'accepted':
			case 'success':
			case 'completed':
				return 'harness-badge harness-badge-success';
			case 'dispatching':
			case 'running':
			case 'queued':
				return 'harness-badge harness-badge-info';
			case 'failed':
			case 'error':
				return 'harness-badge harness-badge-error';
			default:
				return 'harness-badge harness-badge-default';
		}
	}

	function toggleSignatureExpand(signature: string): void {
		const next = new Set(expandedSignatures);
		if (next.has(signature)) next.delete(signature);
		else next.add(signature);
		expandedSignatures = next;
	}

	async function refreshRuntimeStatus(): Promise<void> {
		harnessRuntimeLoading = true;
		harnessRuntimeError = null;
		try {
			harnessRuntime = await loadHarnessRuntimeStatus();
		} catch (e) {
			harnessRuntimeError = e instanceof Error ? e.message : 'Failed to load harness runtime';
		} finally {
			harnessRuntimeLoading = false;
		}
	}

	async function toggleHarnessRuntime(): Promise<void> {
		if (!harnessRuntime || harnessRuntimeLoading || harnessRuntimeMutating) return;
		const nextEnabled = !harnessRuntime.enabled;
		harnessRuntimeMutating = true;
		harnessRuntimeError = null;
		try {
			const result = await setHarnessRuntimeEnabled(nextEnabled);
			harnessRuntime = result;
			const warnings = result.warnings ?? [];
			if (warnings.length > 0) {
				const msg = `Harness saved, but cleanup incomplete: ${warnings.join('; ')}`;
				harnessRuntimeError = msg;
				showError(msg);
			} else {
				showSuccess(`Company loop cadence switched ${result.enabled ? 'ON' : 'OFF'}`);
			}
		} catch (e) {
			const msg = e instanceof Error ? e.message : 'Failed to toggle harness runtime';
			harnessRuntimeError = msg;
			showError(msg);
		} finally {
			harnessRuntimeMutating = false;
		}
	}

	async function triggerHarnessTarget(target: CompanyLoopTarget): Promise<CompanyLoopRunResult> {
		const response = await timedFetch(
			`/api/magician/v2/agents/${encodeURIComponent(target.agentId)}/trigger`,
			{
				method: 'POST',
				headers: { 'Content-Type': 'application/json' },
				body: JSON.stringify({
					goal_id: target.goalId,
					trigger: 'manual.harness.company_loop'
				})
			}
		);

		if (!response.ok) {
			throw new Error(await readApiError(response));
		}

		const payload = (await response.json()) as Record<string, unknown>;
		const executionId = readString(payload, 'execution_id') || undefined;
		const queuePosition = readNumber(payload, 'queue_position');
		let detail = 'Accepted by harness admission';
		if (typeof queuePosition === 'number') {
			detail = `Queued at position ${queuePosition}`;
		} else if (executionId) {
			detail = `Started execution ${executionId}`;
		}

		return {
			key: runKey(target),
			label: target.label,
			agent_id: readString(payload, 'agent_id') || target.agentId,
			goal_id: readString(payload, 'goal_id') || target.goalId,
			status: readString(payload, 'status') || 'accepted',
			detail,
			execution_id: executionId,
			queue_position: queuePosition
		};
	}

	async function runCompanyLoop(): Promise<void> {
		if (companyLoopRunning) return;
		companyLoopRunning = true;
		companyLoopError = '';
		currentStageIndex = 0;
		companyLoopResults = companyLoopTargets.map((target) => ({
			key: runKey(target),
			label: target.label,
			agent_id: target.agentId,
			goal_id: target.goalId,
			status: 'pending',
			detail: 'Waiting to dispatch'
		}));

		for (let i = 0; i < companyLoopTargets.length; i++) {
			const target = companyLoopTargets[i];
			currentStageIndex = i;
			replaceRunResult({
				key: runKey(target),
				label: target.label,
				agent_id: target.agentId,
				goal_id: target.goalId,
				status: 'dispatching',
				detail: 'Sending manual trigger'
			});

			try {
				const result = await triggerHarnessTarget(target);
				replaceRunResult(result);
			} catch (e) {
				const msg = e instanceof Error ? e.message : 'Failed to trigger';
				replaceRunResult({
					key: runKey(target),
					label: target.label,
					agent_id: target.agentId,
					goal_id: target.goalId,
					status: 'failed',
					detail: msg
				});
				companyLoopError = `Stage ${target.agentId.toUpperCase()} failed: ${msg}`;
			}
		}

		companyLoopRunning = false;
		currentStageIndex = -1;
		if (!companyLoopError) {
			showSuccess('Company loop sequence dispatched successfully');
		} else {
			showError(companyLoopError);
		}
		await refreshAnomalies();
	}

	async function refreshAnomalies(): Promise<void> {
		loading = true;
		error = '';
		try {
			const r = await timedFetch('/api/magician/v2/harness/anomalies');
			if (!r.ok) throw new Error(`HTTP ${r.status}`);
			const data = (await r.json()) as { anomalies?: HarnessAnomaly[] };
			anomalies = Array.isArray(data?.anomalies) ? data.anomalies : [];
			lastRefreshedAt = Date.now();
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load anomalies';
			showError(error);
		} finally {
			loading = false;
		}
	}

	async function refreshAll(): Promise<void> {
		await Promise.all([refreshAnomalies(), refreshRuntimeStatus()]);
	}

	onMount(() => {
		if (!browser) return;
		const unsubConn = v2Events.connectionStatus.subscribe((s) => (connectionStatus = s));
		void refreshAll();
		return () => {
			unsubConn();
		};
	});
</script>

<svelte:head>
	<title>Harness & Company Loop — Operations</title>
</svelte:head>

<div class="harness-page">
	<!-- Top Command Masthead -->
	<header class="harness-masthead">
		<div class="harness-masthead__identity">
			<div class="harness-masthead__badge-row">
				<a href="/crew" class="back-link">
					<Icon name="chevron-left" size={14} />
					<span>Back to Crew</span>
				</a>
				<span class="harness-badge harness-badge-outline">Autonomous Harness</span>
				<span class="pulse-indicator">
					<span class="pulse-dot pulse-dot--{connectionStatus}"></span>
					{connectionStatus}
				</span>
				{#if lastRefreshedAt}
					<span class="updated-time">Updated {formatTimestamp(new Date(lastRefreshedAt).toISOString())}</span>
				{/if}
			</div>
			<h1>Harness & Company Loop</h1>
			<p>Monitor autonomous harness execution, trigger executive cadence sequences, and audit detected anomalies.</p>
		</div>

		<div class="harness-masthead__actions">
			<button
				class="crew-button crew-button-secondary"
				type="button"
				disabled={loading || harnessRuntimeLoading}
				title="Refresh anomalies and runtime status"
				on:click={refreshAll}
			>
				<Icon name="rotate-ccw" size={14} />
				<span>{loading ? 'Refreshing...' : 'Refresh'}</span>
			</button>
			<a href="/crew" class="crew-button crew-button-outline">
				<Icon name="sparkle" size={14} />
				<span>View Full Fleet</span>
			</a>
		</div>
	</header>

	{#if error}
		<div class="harness-alert" role="alert">
			<Icon name="alert" size={16} />
			<span>{error}</span>
		</div>
	{/if}

	<!-- SECTION 1: Autonomous Cadence & Company Loop Console -->
	<section class="company-loop-console" aria-label="Company Loop Cadence Console">
		<header class="console-header">
			<div class="console-title-group">
				<div class="console-icon" class:console-icon--active={harnessEnabled} aria-hidden="true">
					<Icon name="sparkle" size={20} />
				</div>
				<div>
					<div class="console-heading-row">
						<h2>Autonomous Company Loop</h2>
						<span class={`harness-badge harness-badge-${harnessStatusColor}`}>{harnessStatusLabel}</span>
						{#if harnessEnabled}
							<span class="cadence-live-pill">
								<span class="pulse-dot pulse-dot--connected"></span>
								Active Cadence
							</span>
						{/if}
					</div>
					<p class="console-desc">
						Sequential autonomous cadence (CMO → CRO → CPO → CTO → CEO). Orchestrates standing goals, steward cycles, and company routines.
					</p>
				</div>
			</div>

			<div class="console-actions">
				<div class="runtime-toggle-group">
					<span class="toggle-state-text">
						{#if harnessRuntimeLoading || harnessRuntimeMutating}
							Updating...
						{:else if harnessEnabled}
							Autonomous Run: ON
						{:else}
							Autonomous Run: OFF
						{/if}
					</span>
					<button
						class="crew-switch"
						class:crew-switch-enabled={harnessEnabled}
						type="button"
						role="switch"
						aria-checked={harnessEnabled}
						aria-label={harnessEnabled ? 'Turn off company loop cadence' : 'Turn on company loop cadence'}
						title={harnessEnabled ? 'Turn off company loop cadence' : 'Turn on company loop cadence'}
						disabled={!harnessRuntime || harnessRuntimeLoading || harnessRuntimeMutating}
						on:click={toggleHarnessRuntime}
					>
						<span aria-hidden="true"></span>
					</button>
				</div>

				<button
					type="button"
					class="crew-button crew-button-primary"
					disabled={companyLoopRunning}
					on:click={runCompanyLoop}
				>
					<Icon name="flag" size={14} />
					<span>{companyLoopRunning ? 'Running Cadence...' : 'Run Company Loop'}</span>
				</button>
			</div>
		</header>

		{#if harnessRuntimeError || harnessRuntime?.config_error}
			<div class="harness-warning-banner" role="alert">
				<Icon name="alert" size={14} />
				<span>{harnessRuntimeError || harnessRuntime?.config_error}</span>
			</div>
		{/if}

		<!-- Stepper Sequence of the 5 Executives -->
		<div class="cadence-stepper" role="region" aria-label="Cadence Sequence Pipeline">
			{#each companyLoopTargets as target, idx (target.agentId)}
				{@const result = companyLoopResults.find((r) => r.agent_id === target.agentId)}
				{@const isCurrent = currentStageIndex === idx}
				<div
					class="stepper-step"
					class:stepper-step--current={isCurrent}
					class:stepper-step--failed={result?.status === 'failed'}
					class:stepper-step--done={result?.status === 'accepted' || result?.status === 'completed'}
				>
					<div class="step-badge">
						<span class="step-num">{idx + 1}</span>
						{#if isCurrent}
							<span class="step-live-dot"></span>
						{/if}
					</div>
					<div class="step-info">
						<div class="step-title-row">
							<a href={`/crew/${encodeURIComponent(target.agentId)}`} class="step-agent-link">
								{target.agentId.toUpperCase()}
							</a>
							{#if result}
								<span class={resultStatusBadgeClass(result.status)}>{result.status}</span>
							{/if}
						</div>
						<span class="step-role">{target.role}</span>
						<span class="step-goal crew-mono">{target.label}</span>
					</div>
				</div>
				{#if idx < companyLoopTargets.length - 1}
					<div class="stepper-arrow" aria-hidden="true">→</div>
				{/if}
			{/each}
		</div>

		<!-- Optional Detailed Results Table if recently run -->
		{#if companyLoopResults.length > 0}
			<div class="results-container">
				<div class="results-header">
					<h4>Latest Cadence Execution Run</h4>
					{#if companyLoopError}
						<span class="text-error">{companyLoopError}</span>
					{/if}
				</div>
				<div class="results-table-wrap">
					<table class="harness-table">
						<thead>
							<tr>
								<th>Stage / Agent</th>
								<th>Goal</th>
								<th>Status</th>
								<th>Execution ID / Queue</th>
								<th>Detail</th>
							</tr>
						</thead>
						<tbody>
							{#each companyLoopResults as res (res.key)}
								<tr class:row-failed={res.status === 'failed'}>
									<td>
										<strong>{res.agent_id.toUpperCase()}</strong>
										<span class="cell-sub">{res.label}</span>
									</td>
									<td>
										<span class="crew-mono">{res.goal_id}</span>
									</td>
									<td>
										<span class={resultStatusBadgeClass(res.status)}>{res.status}</span>
									</td>
									<td>
										{#if res.execution_id}
											<span class="crew-mono">{res.execution_id}</span>
										{:else if typeof res.queue_position === 'number'}
											<span>Queue #{res.queue_position}</span>
										{:else}
											<span class="text-muted">-</span>
										{/if}
									</td>
									<td>{res.detail}</td>
								</tr>
							{/each}
						</tbody>
					</table>
				</div>
			</div>
		{/if}
	</section>

	<!-- SECTION 2: Anomalies KPI Ribbon -->
	<section class="harness-kpi-ribbon" aria-label="Anomaly metrics">
		<div class="harness-kpi-card">
			<span class="kpi-label">Total Anomalies</span>
			<div class="kpi-value-row">
				<strong>{totalAnomaliesCount}</strong>
				<span class="kpi-tag">{uniqueAgentsAffected} agents affected</span>
			</div>
			<span class="kpi-subtext">{totalOccurrences} total incident occurrence{totalOccurrences === 1 ? '' : 's'}</span>
		</div>

		<button
			type="button"
			class="harness-kpi-card"
			class:harness-kpi-card--active={statusFilter === 'open'}
			on:click={() => (statusFilter = statusFilter === 'open' ? 'all' : 'open')}
		>
			<span class="kpi-label">Open Issues</span>
			<div class="kpi-value-row">
				<strong class:text-error={openAnomaliesCount > 0}>{openAnomaliesCount}</strong>
				{#if openAnomaliesCount > 0}
					<span class="kpi-warn-badge">Requires investigation</span>
				{/if}
			</div>
			<span class="kpi-subtext">{openAnomaliesCount === 0 ? 'All clean' : 'Awaiting autofix or manual triage'}</span>
		</button>

		<button
			type="button"
			class="harness-kpi-card"
			class:harness-kpi-card--active={statusFilter === 'fix_dispatched'}
			on:click={() => (statusFilter = statusFilter === 'fix_dispatched' ? 'all' : 'fix_dispatched')}
		>
			<span class="kpi-label">Fix Dispatched</span>
			<div class="kpi-value-row">
				<strong class:text-blue={fixDispatchedCount > 0}>{fixDispatchedCount}</strong>
				{#if fixDispatchedCount > 0}
					<span class="kpi-blue-badge">Repair in flight</span>
				{/if}
			</div>
			<span class="kpi-subtext">{fixDispatchedCount > 0 ? 'Autofix agent running' : 'None in progress'}</span>
		</button>

		<button
			type="button"
			class="harness-kpi-card"
			class:harness-kpi-card--active={statusFilter === 'resolved'}
			on:click={() => (statusFilter = statusFilter === 'resolved' ? 'all' : 'resolved')}
		>
			<span class="kpi-label">Resolved / Dismissed</span>
			<div class="kpi-value-row">
				<strong class="text-success">{resolvedCount}</strong>
			</div>
			<span class="kpi-subtext">Past anomalies closed out</span>
		</button>
	</section>

	<!-- SECTION 3: Detected Anomalies Roster -->
	<section class="anomalies-section" aria-label="Detected harness anomalies">
		<div class="anomalies-toolbar">
			<div class="anomalies-search-box">
				<Icon name="search" size={15} />
				<input
					type="search"
					bind:value={searchQuery}
					placeholder="Search by agent, anomaly kind, goal, or summary..."
					aria-label="Search anomalies"
				/>
				{#if searchQuery}
					<button class="search-clear" type="button" on:click={() => (searchQuery = '')}>×</button>
				{/if}
			</div>

			<div class="anomalies-filter-chips" role="group" aria-label="Filter anomalies by status">
				<button
					type="button"
					class="filter-chip"
					class:filter-chip--active={statusFilter === 'all'}
					on:click={() => (statusFilter = 'all')}
				>
					All ({totalAnomaliesCount})
				</button>
				<button
					type="button"
					class="filter-chip"
					class:filter-chip--active={statusFilter === 'open'}
					on:click={() => (statusFilter = 'open')}
				>
					Open ({openAnomaliesCount})
				</button>
				<button
					type="button"
					class="filter-chip"
					class:filter-chip--active={statusFilter === 'fix_dispatched'}
					on:click={() => (statusFilter = 'fix_dispatched')}
				>
					Fix Dispatched ({fixDispatchedCount})
				</button>
				<button
					type="button"
					class="filter-chip"
					class:filter-chip--active={statusFilter === 'resolved'}
					on:click={() => (statusFilter = 'resolved')}
				>
					Resolved ({resolvedCount})
				</button>
			</div>

			<div class="view-switcher" role="group" aria-label="View mode">
				<button
					type="button"
					class="view-toggle-btn"
					class:view-toggle-btn--active={viewMode === 'table'}
					title="Table view"
					on:click={() => (viewMode = 'table')}
				>
					<Icon name="file-text" size={15} />
				</button>
				<button
					type="button"
					class="view-toggle-btn"
					class:view-toggle-btn--active={viewMode === 'cards'}
					title="Card grid view"
					on:click={() => (viewMode = 'cards')}
				>
					<Icon name="square" size={15} />
				</button>
			</div>
		</div>

		<!-- Content: Table or Cards or Empty -->
		{#if totalFilteredItems === 0}
			<div class="harness-empty">
				<div class="empty-icon-circle" aria-hidden="true">
					<Icon name="sparkle" size={28} />
				</div>
				<h3>{totalAnomaliesCount === 0 ? 'No anomalies detected' : 'No matching anomalies found'}</h3>
				<p>
					{totalAnomaliesCount === 0
						? 'The autonomous harness has not flagged any operational cycle drops, sandbox violations, or tool stalls.'
						: 'Try clearing the search query or switching the status filter.'}
				</p>
				{#if totalAnomaliesCount > 0}
					<button
						class="crew-button crew-button-secondary"
						type="button"
						on:click={() => {
							searchQuery = '';
							statusFilter = 'all';
						}}
					>
						Reset filters
					</button>
				{/if}
			</div>

		{:else if viewMode === 'table'}
			<div class="harness-table-container">
				<table class="harness-table">
					<colgroup>
						<col style="width: 14%;" />
						<col style="width: 16%;" />
						<col style="width: 12%;" />
						<col style="width: 38%;" />
						<col style="width: 10%;" />
						<col style="width: 10%;" />
					</colgroup>
					<thead>
						<tr>
							<th>Agent</th>
							<th>Kind / Status</th>
							<th>Occurrences</th>
							<th>Summary & Detail</th>
							<th>Last Seen</th>
							<th>Action</th>
						</tr>
					</thead>
					<tbody>
						{#each pagedAnomalies as a (a.signature)}
							{@const isExpanded = expandedSignatures.has(a.signature)}
							<tr>
								<td>
									<div class="agent-cell">
										<a href={`/crew/${encodeURIComponent(a.agent_id)}`} class="agent-link">
											{a.agent_id}
										</a>
										<span class="cell-sub crew-mono">{a.goal_id}</span>
									</div>
								</td>
								<td>
									<div class="kind-status-cell">
										<span class={anomalyKindBadgeClass(a.kind)}>{formatAnomalyKind(a.kind)}</span>
										<span class={anomalyStatusBadgeClass(a.status)}>{a.status}</span>
									</div>
								</td>
								<td>
									<span class="occurrence-pill">×{a.occurrences}</span>
								</td>
								<td>
									<div class="summary-cell">
										<p class="summary-text">{a.summary}</p>
										{#if a.detail}
											<button
												type="button"
												class="expand-detail-btn"
												aria-expanded={isExpanded}
												on:click={() => toggleSignatureExpand(a.signature)}
											>
												<Icon name={isExpanded ? 'chevron-up' : 'chevron-down'} size={12} />
												<span>{isExpanded ? 'Hide details' : 'Show details'}</span>
											</button>
											{#if isExpanded}
												<pre class="detail-block crew-mono">{a.detail}</pre>
											{/if}
										{/if}
										{#if a.fix_task_id}
											<div class="fix-task-note">
												<span>Fix Task:</span>
												<span class="crew-mono">{a.fix_task_id}</span>
											</div>
										{/if}
									</div>
								</td>
								<td>
									<span class="timestamp-text">{formatTimestamp(a.last_seen)}</span>
								</td>
								<td>
									<a href={`/crew/${encodeURIComponent(a.agent_id)}`} class="crew-button crew-button-outline crew-button-sm">
										Agent →
									</a>
								</td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>

		{:else}
			<!-- CARDS GRID VIEW -->
			<div class="anomalies-card-grid">
				{#each pagedAnomalies as a (a.signature)}
					{@const isExpanded = expandedSignatures.has(a.signature)}
					<article class="anomaly-card" class:anomaly-card--open={a.status.toLowerCase() === 'open'}>
						<header class="anomaly-card__header">
							<div class="card-title-row">
								<a href={`/crew/${encodeURIComponent(a.agent_id)}`} class="card-agent-name">
									{a.agent_id}
								</a>
								<span class="occurrence-pill">×{a.occurrences}</span>
							</div>
							<div class="card-badges">
								<span class={anomalyKindBadgeClass(a.kind)}>{formatAnomalyKind(a.kind)}</span>
								<span class={anomalyStatusBadgeClass(a.status)}>{a.status}</span>
							</div>
						</header>

						<p class="card-summary">{a.summary}</p>

						{#if a.detail}
							<button
								type="button"
								class="expand-detail-btn"
								aria-expanded={isExpanded}
								on:click={() => toggleSignatureExpand(a.signature)}
							>
								<Icon name={isExpanded ? 'chevron-up' : 'chevron-down'} size={12} />
								<span>{isExpanded ? 'Hide details' : 'Show details'}</span>
							</button>
							{#if isExpanded}
								<pre class="detail-block crew-mono">{a.detail}</pre>
							{/if}
						{/if}

						<div class="card-meta">
							<span class="crew-mono card-goal">{a.goal_id}</span>
							<span class="card-time">Last seen {formatTimestamp(a.last_seen)}</span>
						</div>

						<footer class="card-footer">
							<a href={`/crew/${encodeURIComponent(a.agent_id)}`} class="crew-button crew-button-secondary crew-button-sm">
								Inspect Agent →
							</a>
						</footer>
					</article>
				{/each}
			</div>
		{/if}

		<!-- Pagination Footer -->
		{#if totalFilteredItems > 0}
			<div class="harness-pagination-bar">
				<div class="harness-pagination-bar__per-page">
					<label for="harness-page-size-select">Per page:</label>
					<select
						id="harness-page-size-select"
						bind:value={pageSize}
						aria-label="Items per page"
					>
						{#each PAGE_SIZE_OPTIONS as size}
							<option value={size}>{size}</option>
						{/each}
					</select>
				</div>
				<ServerPager
					{currentPage}
					{pageCount}
					{startItem}
					{endItem}
					totalItems={totalFilteredItems}
					ariaLabel="Harness anomalies pagination"
					on:pagechange={(event) => (currentPage = event.detail.page)}
				/>
			</div>
		{/if}
	</section>
</div>

<style>
	/* Canonical Magician Full-Width Shell */
	.harness-page {
		box-sizing: border-box;
		display: flex;
		flex-direction: column;
		gap: 1.5rem;
		margin: 0 auto;
		max-width: var(--app-content-max, 1360px);
		padding: 1.5rem 1.5rem 5rem;
		width: 100%;
	}

	/* Top Masthead */
	.harness-masthead {
		align-items: flex-start;
		background: linear-gradient(180deg, var(--bg-card) 0%, var(--bg-soft) 100%);
		border: 1px solid var(--border-soft);
		border-radius: 12px;
		display: flex;
		flex-wrap: wrap;
		gap: 1.25rem;
		justify-content: space-between;
		padding: 1.25rem 1.5rem;
	}

	.harness-masthead__identity {
		display: flex;
		flex-direction: column;
		gap: 0.35rem;
		max-width: 52rem;
	}

	.harness-masthead__badge-row {
		align-items: center;
		display: flex;
		flex-wrap: wrap;
		gap: 0.55rem;
		margin-bottom: 0.2rem;
	}

	.back-link {
		align-items: center;
		color: var(--accent-primary);
		display: inline-flex;
		font-size: 0.78rem;
		font-weight: 600;
		gap: 0.2rem;
		text-decoration: none;
	}

	.back-link:hover {
		text-decoration: underline;
	}

	.pulse-indicator {
		align-items: center;
		color: var(--text-secondary);
		display: inline-flex;
		font-size: 0.76rem;
		font-weight: 500;
		gap: 0.35rem;
		text-transform: capitalize;
	}

	.pulse-dot {
		border-radius: 50%;
		display: inline-block;
		height: 7px;
		width: 7px;
	}

	.pulse-dot--connected {
		background: var(--accent-success, #10b981);
		box-shadow: 0 0 0 2px color-mix(in srgb, var(--accent-success, #10b981) 30%, transparent);
	}

	.pulse-dot--connecting {
		animation: pulse-glow 1.5s infinite;
		background: var(--accent-warning, #f59e0b);
	}

	.pulse-dot--disconnected {
		background: var(--accent-error, #ef4444);
	}

	@keyframes pulse-glow {
		0%, 100% { opacity: 0.4; }
		50% { opacity: 1; }
	}

	.updated-time {
		color: var(--text-muted);
		font-size: 0.74rem;
	}

	.harness-masthead__identity h1 {
		color: var(--text-primary);
		font-size: 1.5rem;
		font-weight: 700;
		letter-spacing: -0.015em;
		margin: 0;
	}

	.harness-masthead__identity p {
		color: var(--text-secondary);
		font-size: 0.88rem;
		line-height: 1.45;
		margin: 0;
	}

	.harness-masthead__actions {
		align-items: center;
		display: flex;
		flex-wrap: wrap;
		gap: 0.5rem;
	}

	/* Alert */
	.harness-alert {
		align-items: center;
		background: color-mix(in srgb, var(--accent-error, #ef4444) 10%, transparent);
		border: 1px solid var(--accent-error, #ef4444);
		border-radius: 8px;
		color: var(--accent-error, #ef4444);
		display: flex;
		font-size: 0.88rem;
		font-weight: 500;
		gap: 0.6rem;
		padding: 0.75rem 1rem;
	}

	/* Autonomous Cadence Console */
	.company-loop-console {
		background: linear-gradient(135deg, color-mix(in srgb, var(--accent-primary) 6%, var(--bg-card)) 0%, var(--bg-card) 100%);
		border: 1px solid color-mix(in srgb, var(--accent-primary) 28%, var(--border-soft));
		border-radius: 12px;
		display: flex;
		flex-direction: column;
		gap: 1.25rem;
		padding: 1.25rem 1.5rem;
	}

	.console-header {
		align-items: flex-start;
		display: flex;
		flex-wrap: wrap;
		gap: 1.25rem;
		justify-content: space-between;
	}

	.console-title-group {
		align-items: flex-start;
		display: flex;
		flex: 1;
		gap: 1rem;
		min-width: 0;
	}

	.console-icon {
		align-items: center;
		background: color-mix(in srgb, var(--accent-primary) 12%, var(--bg-soft));
		border: 1px solid color-mix(in srgb, var(--accent-primary) 24%, var(--border-soft));
		border-radius: 10px;
		color: var(--accent-primary);
		display: inline-flex;
		flex-shrink: 0;
		height: 2.75rem;
		justify-content: center;
		margin-top: 0.1rem;
		width: 2.75rem;
	}

	.console-icon--active {
		background: color-mix(in srgb, var(--accent-success, #10b981) 16%, var(--bg-soft));
		border-color: color-mix(in srgb, var(--accent-success, #10b981) 32%, var(--border-soft));
		color: var(--accent-success, #10b981);
	}

	.console-heading-row {
		align-items: center;
		display: flex;
		flex-wrap: wrap;
		gap: 0.55rem;
	}

	.console-heading-row h2 {
		color: var(--text-primary);
		font-size: 1.2rem;
		font-weight: 700;
		letter-spacing: -0.01em;
		margin: 0;
	}

	.cadence-live-pill {
		align-items: center;
		background: color-mix(in srgb, var(--accent-success, #10b981) 12%, transparent);
		border-radius: 9999px;
		color: var(--accent-success, #10b981);
		display: inline-flex;
		font-size: 0.72rem;
		font-weight: 600;
		gap: 0.35rem;
		padding: 0.2rem 0.55rem;
	}

	.console-desc {
		color: var(--text-secondary);
		font-size: 0.84rem;
		line-height: 1.4;
		margin: 0.25rem 0 0;
		max-width: 48rem;
	}

	.console-actions {
		align-items: center;
		display: flex;
		flex-shrink: 0;
		flex-wrap: wrap;
		gap: 0.75rem;
	}

	.runtime-toggle-group {
		align-items: center;
		background: var(--bg-soft);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		display: inline-flex;
		gap: 0.6rem;
		padding: 0.35rem 0.65rem;
	}

	.toggle-state-text {
		color: var(--text-primary);
		font-size: 0.8125rem;
		font-weight: 600;
		white-space: nowrap;
	}

	.harness-warning-banner {
		align-items: center;
		background: color-mix(in srgb, var(--accent-warning, #f59e0b) 12%, transparent);
		border: 1px solid color-mix(in srgb, var(--accent-warning, #f59e0b) 30%, transparent);
		border-radius: 6px;
		color: var(--accent-warning, #f59e0b);
		display: flex;
		font-size: 0.8rem;
		gap: 0.5rem;
		padding: 0.45rem 0.75rem;
	}

	/* Stepper Pipeline */
	.cadence-stepper {
		align-items: center;
		display: flex;
		flex-wrap: wrap;
		gap: 0.6rem;
		margin-top: 0.5rem;
		padding-top: 0.75rem;
		border-top: 1px solid var(--border-subtle, rgba(255, 255, 255, 0.08));
	}

	.stepper-step {
		align-items: center;
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		display: flex;
		flex: 1;
		gap: 0.65rem;
		min-width: 170px;
		padding: 0.6rem 0.75rem;
		transition: border-color 0.15s, background-color 0.15s;
	}

	.stepper-step--current {
		border-color: var(--accent-primary);
		box-shadow: 0 0 0 1px var(--accent-primary);
	}

	.stepper-step--done {
		border-color: color-mix(in srgb, var(--accent-success, #10b981) 40%, var(--border-soft));
	}

	.stepper-step--failed {
		border-color: var(--accent-error, #ef4444);
	}

	.step-badge {
		align-items: center;
		background: var(--bg-soft);
		border-radius: 6px;
		display: flex;
		font-size: 0.76rem;
		font-weight: 700;
		height: 26px;
		justify-content: center;
		position: relative;
		width: 26px;
	}

	.step-live-dot {
		animation: pulse-glow 1s infinite;
		background: var(--accent-primary);
		border-radius: 50%;
		height: 6px;
		position: absolute;
		right: -2px;
		top: -2px;
		width: 6px;
	}

	.step-info {
		display: flex;
		flex-direction: column;
		gap: 0.1rem;
		min-width: 0;
	}

	.step-title-row {
		align-items: center;
		display: flex;
		gap: 0.4rem;
	}

	.step-agent-link {
		color: var(--text-primary);
		font-size: 0.85rem;
		font-weight: 700;
		text-decoration: none;
	}

	.step-agent-link:hover {
		color: var(--accent-primary);
		text-decoration: underline;
	}

	.step-role {
		color: var(--text-secondary);
		font-size: 0.72rem;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.step-goal {
		color: var(--text-muted);
		font-size: 0.68rem;
	}

	.stepper-arrow {
		color: var(--text-muted);
		font-size: 1rem;
		font-weight: 700;
	}

	/* Run Results */
	.results-container {
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		display: flex;
		flex-direction: column;
		gap: 0.6rem;
		margin-top: 0.5rem;
		padding: 0.85rem 1rem;
	}

	.results-header {
		align-items: center;
		display: flex;
		justify-content: space-between;
	}

	.results-header h4 {
		color: var(--text-primary);
		font-size: 0.88rem;
		font-weight: 600;
		margin: 0;
	}

	.results-table-wrap {
		overflow-x: auto;
	}

	/* KPI Ribbon */
	.harness-kpi-ribbon {
		display: grid;
		gap: 0.75rem;
		grid-template-columns: repeat(auto-fit, minmax(200px, 1fr));
		width: 100%;
	}

	.harness-kpi-card {
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 10px;
		cursor: pointer;
		display: flex;
		flex-direction: column;
		gap: 0.3rem;
		padding: 0.85rem 1rem;
		text-align: left;
		transition: border-color 0.15s, background-color 0.15s, transform 0.1s;
	}

	.harness-kpi-card:hover {
		border-color: var(--accent-primary);
		transform: translateY(-1px);
	}

	.harness-kpi-card--active {
		border-color: var(--accent-primary);
		box-shadow: 0 0 0 1px var(--accent-primary);
	}

	.kpi-label {
		color: var(--text-secondary);
		font-size: 0.74rem;
		font-weight: 600;
		letter-spacing: 0.04em;
		text-transform: uppercase;
	}

	.kpi-value-row {
		align-items: baseline;
		display: flex;
		gap: 0.5rem;
	}

	.kpi-value-row strong {
		font-size: 1.65rem;
		font-weight: 700;
		line-height: 1.1;
	}

	.kpi-tag {
		background: var(--bg-soft);
		border-radius: 4px;
		color: var(--text-secondary);
		font-size: 0.72rem;
		padding: 0.15rem 0.4rem;
	}

	.kpi-subtext {
		color: var(--text-muted);
		font-size: 0.78rem;
	}

	.kpi-warn-badge {
		background: color-mix(in srgb, var(--accent-warning, #f59e0b) 15%, transparent);
		border-radius: 9999px;
		color: var(--accent-warning, #f59e0b);
		font-size: 0.7rem;
		font-weight: 600;
		padding: 0.15rem 0.5rem;
	}

	.kpi-blue-badge {
		background: color-mix(in srgb, var(--accent-primary) 15%, transparent);
		border-radius: 9999px;
		color: var(--accent-primary);
		font-size: 0.7rem;
		font-weight: 600;
		padding: 0.15rem 0.5rem;
	}

	/* Anomalies Section */
	.anomalies-section {
		display: flex;
		flex-direction: column;
		gap: 1rem;
	}

	.anomalies-toolbar {
		align-items: center;
		display: flex;
		flex-wrap: wrap;
		gap: 0.75rem;
		justify-content: space-between;
	}

	.anomalies-search-box {
		align-items: center;
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		display: flex;
		gap: 0.5rem;
		min-width: 280px;
		padding: 0.45rem 0.75rem;
		position: relative;
	}

	.anomalies-search-box input {
		background: transparent;
		border: none;
		color: var(--text-primary);
		font-size: 0.85rem;
		outline: none;
		width: 100%;
	}

	.search-clear {
		background: transparent;
		border: none;
		color: var(--text-muted);
		cursor: pointer;
		font-size: 1rem;
		padding: 0;
	}

	.anomalies-filter-chips {
		display: flex;
		flex-wrap: wrap;
		gap: 0.4rem;
	}

	.filter-chip {
		align-items: center;
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 9999px;
		color: var(--text-secondary);
		cursor: pointer;
		display: inline-flex;
		font-size: 0.78rem;
		font-weight: 500;
		gap: 0.35rem;
		padding: 0.3rem 0.7rem;
		transition: border-color 0.15s, background-color 0.15s, color 0.15s;
	}

	.filter-chip:hover {
		border-color: var(--accent-primary);
		color: var(--text-primary);
	}

	.filter-chip--active {
		background: color-mix(in srgb, var(--accent-primary) 15%, transparent);
		border-color: var(--accent-primary);
		color: var(--accent-primary);
		font-weight: 600;
	}

	.view-switcher {
		align-items: center;
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		display: inline-flex;
		padding: 0.15rem;
	}

	.view-toggle-btn {
		align-items: center;
		background: transparent;
		border: none;
		border-radius: 6px;
		color: var(--text-secondary);
		cursor: pointer;
		display: inline-flex;
		height: 28px;
		justify-content: center;
		padding: 0;
		width: 28px;
	}

	.view-toggle-btn--active {
		background: var(--bg-soft);
		color: var(--text-primary);
	}

	/* Table Layout */
	.harness-table-container {
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 10px;
		overflow-x: auto;
	}

	.harness-table {
		border-collapse: collapse;
		font-size: 0.85rem;
		text-align: left;
		width: 100%;
	}

	.harness-table th {
		background: var(--bg-soft);
		border-bottom: 1px solid var(--border-soft);
		color: var(--text-secondary);
		font-size: 0.72rem;
		font-weight: 600;
		letter-spacing: 0.04em;
		padding: 0.65rem 0.85rem;
		text-transform: uppercase;
	}

	.harness-table td {
		border-bottom: 1px solid var(--border-soft);
		color: var(--text-primary);
		padding: 0.75rem 0.85rem;
		vertical-align: top;
	}

	.harness-table tr:last-child td {
		border-bottom: none;
	}

	.agent-cell {
		display: flex;
		flex-direction: column;
		gap: 0.15rem;
	}

	.agent-link {
		color: var(--text-primary);
		font-weight: 700;
		text-decoration: none;
	}

	.agent-link:hover {
		color: var(--accent-primary);
		text-decoration: underline;
	}

	.cell-sub {
		color: var(--text-muted);
		font-size: 0.72rem;
	}

	.kind-status-cell {
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		gap: 0.35rem;
	}

	.kind-badge {
		background: var(--bg-soft);
		border: 1px solid var(--border-soft);
		border-radius: 4px;
		color: var(--text-secondary);
		font-size: 0.72rem;
		font-weight: 600;
		padding: 0.15rem 0.4rem;
		text-transform: capitalize;
	}

	.kind-badge--error {
		background: color-mix(in srgb, var(--accent-error, #ef4444) 12%, transparent);
		border-color: color-mix(in srgb, var(--accent-error, #ef4444) 30%, transparent);
		color: var(--accent-error, #ef4444);
	}

	.kind-badge--warn {
		background: color-mix(in srgb, var(--accent-warning, #f59e0b) 12%, transparent);
		border-color: color-mix(in srgb, var(--accent-warning, #f59e0b) 30%, transparent);
		color: var(--accent-warning, #f59e0b);
	}

	.kind-badge--info {
		background: color-mix(in srgb, var(--accent-primary) 12%, transparent);
		border-color: color-mix(in srgb, var(--accent-primary) 30%, transparent);
		color: var(--accent-primary);
	}

	.occurrence-pill {
		background: var(--bg-soft);
		border-radius: 9999px;
		color: var(--text-secondary);
		font-size: 0.74rem;
		font-weight: 700;
		padding: 0.15rem 0.5rem;
	}

	.summary-cell {
		display: flex;
		flex-direction: column;
		gap: 0.35rem;
	}

	.summary-text {
		color: var(--text-primary);
		font-size: 0.85rem;
		line-height: 1.4;
		margin: 0;
	}

	.expand-detail-btn {
		align-items: center;
		background: transparent;
		border: none;
		color: var(--accent-primary);
		cursor: pointer;
		display: inline-flex;
		font-size: 0.74rem;
		font-weight: 600;
		gap: 0.25rem;
		padding: 0;
	}

	.expand-detail-btn:hover {
		text-decoration: underline;
	}

	.detail-block {
		background: var(--bg-soft);
		border: 1px solid var(--border-soft);
		border-radius: 6px;
		font-size: 0.74rem;
		margin: 0.35rem 0 0;
		max-height: 140px;
		overflow-y: auto;
		padding: 0.5rem;
		white-space: pre-wrap;
		word-break: break-all;
	}

	.fix-task-note {
		align-items: center;
		color: var(--accent-primary);
		display: inline-flex;
		font-size: 0.72rem;
		gap: 0.35rem;
		margin-top: 0.2rem;
	}

	.timestamp-text {
		color: var(--text-muted);
		font-size: 0.78rem;
		white-space: nowrap;
	}

	.row-failed td {
		background: color-mix(in srgb, var(--accent-error, #ef4444) 6%, transparent);
	}

	/* Card Grid */
	.anomalies-card-grid {
		display: grid;
		gap: 1rem;
		grid-template-columns: repeat(auto-fill, minmax(320px, 1fr));
	}

	.anomaly-card {
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 10px;
		display: flex;
		flex-direction: column;
		gap: 0.65rem;
		padding: 1rem;
		transition: border-color 0.15s;
	}

	.anomaly-card:hover {
		border-color: var(--accent-primary);
	}

	.anomaly-card--open {
		border-left: 3px solid var(--accent-error, #ef4444);
	}

	.anomaly-card__header {
		display: flex;
		flex-direction: column;
		gap: 0.4rem;
	}

	.card-title-row {
		align-items: center;
		display: flex;
		justify-content: space-between;
	}

	.card-agent-name {
		color: var(--text-primary);
		font-size: 1rem;
		font-weight: 700;
		text-decoration: none;
	}

	.card-agent-name:hover {
		color: var(--accent-primary);
		text-decoration: underline;
	}

	.card-badges {
		display: flex;
		flex-wrap: wrap;
		gap: 0.35rem;
	}

	.card-summary {
		color: var(--text-secondary);
		font-size: 0.84rem;
		line-height: 1.4;
		margin: 0;
	}

	.card-meta {
		align-items: center;
		display: flex;
		font-size: 0.72rem;
		justify-content: space-between;
		margin-top: auto;
		padding-top: 0.4rem;
	}

	.card-goal {
		color: var(--text-muted);
	}

	.card-time {
		color: var(--text-muted);
	}

	.card-footer {
		border-top: 1px solid var(--border-soft);
		display: flex;
		justify-content: flex-end;
		padding-top: 0.6rem;
	}

	/* Empty State */
	.harness-empty {
		align-items: center;
		background: var(--bg-card);
		border: 1px dashed var(--border-soft);
		border-radius: 12px;
		display: flex;
		flex-direction: column;
		gap: 0.65rem;
		justify-content: center;
		padding: 3.5rem 1.5rem;
		text-align: center;
	}

	.empty-icon-circle {
		align-items: center;
		background: color-mix(in srgb, var(--accent-success, #10b981) 12%, var(--bg-soft));
		border-radius: 50%;
		color: var(--accent-success, #10b981);
		display: flex;
		height: 3.5rem;
		justify-content: center;
		width: 3.5rem;
	}

	.harness-empty h3 {
		color: var(--text-primary);
		font-size: 1.15rem;
		font-weight: 600;
		margin: 0;
	}

	.harness-empty p {
		color: var(--text-secondary);
		font-size: 0.88rem;
		margin: 0;
		max-width: 30rem;
	}

	/* Pagination Bar */
	.harness-pagination-bar {
		align-items: center;
		border-top: 1px solid var(--border-subtle, rgba(255, 255, 255, 0.08));
		display: flex;
		flex-wrap: wrap;
		gap: 0.75rem;
		justify-content: space-between;
		margin-top: 1rem;
		padding-top: 0.75rem;
	}

	.harness-pagination-bar__per-page {
		align-items: center;
		color: var(--text-muted, #94a3b8);
		display: inline-flex;
		gap: 0.4rem;
		font-size: 0.8125rem;
	}

	.harness-pagination-bar__per-page select {
		background: var(--surface-bg-subtle, rgba(255, 255, 255, 0.04));
		border: 1px solid var(--border-subtle, rgba(255, 255, 255, 0.12));
		border-radius: 4px;
		color: var(--text-primary, #f1f5f9);
		cursor: pointer;
		font-size: 0.8125rem;
		padding: 0.2rem 0.4rem;
	}

	/* Common Primitive Styles */
	.crew-button {
		align-items: center;
		border: 1px solid transparent;
		border-radius: 7px;
		cursor: pointer;
		display: inline-flex;
		font-family: inherit;
		font-size: 0.85rem;
		font-weight: 600;
		gap: 0.45rem;
		line-height: 1;
		padding: 0.5rem 0.85rem;
		text-decoration: none;
		transition: background-color 0.15s, border-color 0.15s, color 0.15s;
		white-space: nowrap;
	}

	.crew-button-primary {
		background: var(--accent-primary, #38bdf8);
		border-color: var(--accent-primary, #38bdf8);
		color: #ffffff;
	}

	.crew-button-primary:hover:not(:disabled) {
		filter: brightness(1.08);
	}

	.crew-button-secondary {
		background: var(--bg-card);
		border-color: var(--border-soft);
		color: var(--text-primary);
	}

	.crew-button-secondary:hover:not(:disabled) {
		background: var(--bg-soft);
		border-color: var(--accent-primary);
	}

	.crew-button-outline {
		background: transparent;
		border-color: var(--border-soft);
		color: var(--text-secondary);
	}

	.crew-button-outline:hover:not(:disabled) {
		background: var(--bg-soft);
		border-color: var(--text-secondary);
		color: var(--text-primary);
	}

	.crew-button-sm {
		font-size: 0.76rem;
		padding: 0.35rem 0.6rem;
	}

	.crew-button:disabled {
		cursor: not-allowed;
		opacity: 0.55;
	}

	.harness-badge {
		align-items: center;
		border-radius: 4px;
		display: inline-flex;
		font-size: 0.74rem;
		font-weight: 600;
		gap: 0.3rem;
		line-height: 1;
		padding: 0.25rem 0.45rem;
		white-space: nowrap;
	}

	.harness-badge-default {
		background: var(--bg-soft);
		color: var(--text-secondary);
	}

	.harness-badge-info {
		background: color-mix(in srgb, var(--accent-primary) 15%, transparent);
		color: var(--accent-primary);
	}

	.harness-badge-success {
		background: color-mix(in srgb, var(--accent-success, #10b981) 15%, transparent);
		color: var(--accent-success, #10b981);
	}

	.harness-badge-warning {
		background: color-mix(in srgb, var(--accent-warning, #f59e0b) 18%, transparent);
		color: var(--accent-warning, #f59e0b);
	}

	.harness-badge-error {
		background: color-mix(in srgb, var(--accent-error, #ef4444) 18%, transparent);
		color: var(--accent-error, #ef4444);
	}

	.harness-badge-outline {
		border: 1px solid var(--border-soft);
		color: var(--text-secondary);
	}

	/* Switch */
	.crew-switch {
		align-items: center;
		background: var(--bg-soft);
		border: 1px solid var(--border-soft);
		border-radius: 9999px;
		cursor: pointer;
		display: inline-flex;
		height: 22px;
		padding: 2px;
		position: relative;
		transition: background 0.15s, border-color 0.15s;
		width: 40px;
	}

	.crew-switch span {
		background: var(--text-secondary);
		border-radius: 50%;
		display: block;
		height: 16px;
		transform: translateX(0);
		transition: transform 0.15s, background 0.15s;
		width: 16px;
	}

	.crew-switch-enabled {
		background: var(--accent-success, #10b981);
		border-color: var(--accent-success, #10b981);
	}

	.crew-switch-enabled span {
		background: #ffffff;
		transform: translateX(18px);
	}

	.crew-mono {
		font-family: var(--font-mono, monospace);
		font-size: 0.76rem;
	}

	.text-error {
		color: var(--accent-error, #ef4444);
	}

	.text-success {
		color: var(--accent-success, #10b981);
	}

	.text-blue {
		color: var(--accent-primary);
	}

	.text-muted {
		color: var(--text-muted);
	}

	@media (max-width: 900px) {
		.harness-masthead,
		.console-header {
			flex-direction: column;
		}

		.harness-kpi-ribbon {
			grid-template-columns: repeat(2, 1fr);
		}

		.cadence-stepper {
			flex-direction: column;
			align-items: stretch;
		}

		.stepper-arrow {
			transform: rotate(90deg);
			align-self: center;
		}

		.anomalies-toolbar {
			flex-direction: column;
			align-items: stretch;
		}

		.anomalies-search-box {
			width: 100%;
			min-width: 0;
		}
	}

	@media (max-width: 600px) {
		.harness-page {
			padding: 1rem 0.75rem 4rem;
		}

		.harness-kpi-ribbon {
			grid-template-columns: 1fr;
		}
	}
</style>
