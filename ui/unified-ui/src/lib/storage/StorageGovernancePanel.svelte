<script lang="ts">
	import { onMount } from 'svelte';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';

	import { requestConfirmation } from '$lib/stores/confirmationStore';
	import { showError, showSuccess } from '$lib/shared/stores/notifications';
	import {
		applyAttentionLearningRetention,
		applyStorageRetention,
		clearCompactionMetrics,
		compactDatabases,
		compactParquet,
		fetchStorageSnapshot,
		fetchMaintenanceStatus,
		type DatabaseMaintenanceStatus,
		maintainAppStore,
		type AppStoreMaintenanceOperation,
		type AppStoreMaintenanceReport,
		formatStorageBytes,
		optimizeAttentionLearning,
		previewAttentionLearningRetention,
		purgeFeedOrphans,
		reclaimAttentionLearningSpace,
		type AttentionLearningMaintenanceReport,
		type CompactionMetricsSnapshot,
		type DuckDbTarget,
		type StorageEntry,
		type StorageMaintenanceReport,
		type StorageActivationReport,
		type StorageSnapshot
	} from './storageGovernance';

	export let loadMaintenance = fetchMaintenanceStatus;
	let maintenance: DatabaseMaintenanceStatus[] = [];
	let maintenanceError: string | null = null;
	export let loadSnapshot: () => Promise<StorageSnapshot> = fetchStorageSnapshot;
	export let maintainAppStoreAction = maintainAppStore;
	export let loadActivation: () => Promise<StorageActivationReport | null> = async () => null;
	export let compactDatabaseAction: (
		targets: DuckDbTarget[]
	) => Promise<StorageMaintenanceReport> = compactDatabases;
	export let compactParquetAction: () => Promise<StorageMaintenanceReport> = compactParquet;
	export let retentionAction: (days?: number) => Promise<StorageMaintenanceReport> =
		applyStorageRetention;
	export let purgeFeedAction: () => Promise<number> = purgeFeedOrphans;
	export let clearMetricsAction: () => Promise<CompactionMetricsSnapshot> = clearCompactionMetrics;
	export let optimizeAttentionAction: () => Promise<AttentionLearningMaintenanceReport> =
		optimizeAttentionLearning;
	export let previewAttentionRetentionAction: (
		days: number
	) => Promise<AttentionLearningMaintenanceReport> = previewAttentionLearningRetention;
	export let applyAttentionRetentionAction: (
		days: number
	) => Promise<AttentionLearningMaintenanceReport> = applyAttentionLearningRetention;
	export let reclaimAttentionAction: () => Promise<AttentionLearningMaintenanceReport> =
		reclaimAttentionLearningSpace;

	let snapshot: StorageSnapshot | null = null;
	let activation: StorageActivationReport | null = null;
	let loading = true;
	let error: string | null = null;
	let activeAction: string | null = null;
	let lastResult: string | null = null;
	let appMaintenanceReport: AppStoreMaintenanceReport | null = null;
	let attentionRetentionDays = 90;
	let attentionRetentionPreview: AttentionLearningMaintenanceReport | null = null;

	$: databases = snapshot?.entries.filter((entry) => entry.kind === 'duck_db') ?? [];
	$: telemetry = snapshot?.entries.filter((entry) => entry.kind === 'parquet') ?? [];
	$: protectedStores =
		snapshot?.entries.filter(
			(entry) =>
				entry.id !== 'compaction_metrics' &&
				entry.id !== 'attention_learning_sqlite' &&
				entry.id !== 'app_store_sqlite' &&
				(entry.kind === 'sqlite' || entry.kind === 'journal' || entry.kind === 'directory')
		) ?? [];
	$: metrics = snapshot?.compaction_metrics ?? null;
	$: attentionStore = snapshot?.entries.find((entry) => entry.id === 'attention_learning_sqlite') ?? null;
	$: appStore = snapshot?.entries.find((entry) => entry.id === 'app_store_sqlite') ?? null;
	$: currentAppReport = appMaintenanceReport?.principal === snapshot?.principal && appMaintenanceReport?.workspace === snapshot?.workspace
		? appMaintenanceReport : null;
	$: appSizeReport = currentAppReport && Date.parse(currentAppReport.completed_at) >= (snapshot?.generated_at_ms ?? 0)
		? currentAppReport : null;
	$: attentionPreviewRows = attentionRetentionPreview?.retention
		? Object.values(attentionRetentionPreview.retention.affected_rows).reduce(
				(sum, count) => sum + count,
				0
		  )
		: 0;
	$: attentionPreviewCurrent =
		attentionRetentionPreview?.operation === 'retention_preview'
		&& attentionRetentionPreview.retention_days === attentionRetentionDays
		&& attentionRetentionPreview.principal === snapshot?.principal
		&& attentionRetentionPreview.workspace === snapshot?.workspace;
	$: largest = [...(snapshot?.entries ?? [])].sort((a, b) => (b.size_bytes + b.wal_bytes) - (a.size_bytes + a.wal_bytes))[0];
	$: totalWalBytes = snapshot?.entries.reduce((sum, entry) => sum + entry.wal_bytes, 0) ?? 0;
	$: partialInventoryCount = snapshot?.entries.filter((entry) => !entry.inventory_complete).length ?? 0;

	onMount(() => {
        void refresh();
        let disposed = false;
        let generation = 0;
        let inFlight = false;
        let scopeKey = '';
        async function pollMaintenance() {
            if (disposed || inFlight || document.visibilityState === 'hidden') return;
            inFlight = true;
            const requested = generation;
            try {
                const result = await loadMaintenance();
                if (!disposed && requested === generation) { maintenance = result; maintenanceError = null; }
            } catch {
                if (!disposed && requested === generation) {
                    maintenance = []; maintenanceError = 'Maintenance status is unavailable. Retrying automatically.';
                }
            } finally {
                inFlight = false;
                if (!disposed && requested !== generation) void pollMaintenance();
            }
        }
        const unsubscribe = scopeIdentityStore.subscribe((scope) => {
            const next = `${scope.principal}/${scope.workspace}/${scope.isResolved}`;
            if (next === scopeKey) return;
            scopeKey = next; generation += 1; maintenance = []; maintenanceError = null;
            void pollMaintenance();
        });
        const timer = setInterval(() => void pollMaintenance(), 10000);
        document.addEventListener('visibilitychange', pollMaintenance);
        return () => {
            disposed = true; unsubscribe(); clearInterval(timer);
            document.removeEventListener('visibilitychange', pollMaintenance);
        };
	});

	async function refresh(): Promise<void> {
		loading = snapshot === null;
		error = null;
		try {
			snapshot = await loadSnapshot();
			activation = await loadActivation();
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Storage inventory could not be loaded';
		} finally {
			loading = false;
		}
	}

	function databaseTarget(entry: StorageEntry): DuckDbTarget | null {
		if (entry.id === 'analytics_duckdb') return 'analytics';
		if (entry.id === 'channel_assist_duckdb') return 'channel_assist';
		if (entry.id === 'ui_threads_duckdb') return 'ui_threads';
		if (entry.id === 'social_sqlite') return 'social';
		return null;
	}

	async function confirmAction(
		title: string,
		message: string,
		confirmLabel: string,
		destructive = false
	): Promise<boolean> {
		return requestConfirmation({ title, message, confirmLabel, destructive });
	}

	async function perform<T>(
		id: string,
		operation: () => Promise<T>,
		success: (result: T) => string
	): Promise<void> {
		if (activeAction) return;
		activeAction = id;
		error = null;
		try {
			const result = await operation();
			lastResult = success(result);
			showSuccess(lastResult);
			await refresh();
		} catch (cause) {
			const message = cause instanceof Error ? cause.message : 'Storage maintenance failed';
			error = message;
			showError(message);
		} finally {
			activeAction = null;
		}
	}

	async function maintainApps(operation: AppStoreMaintenanceOperation): Promise<void> {
		if (!appStore?.actions.some((action) => action.id === `app_store_${operation}`)) return;
		if (operation !== 'verify' && !await confirmAction(
			operation === 'reclaim' ? 'Reclaim App database space?' : 'Optimize App database?',
			'This coordinates active app database operations in this workspace and preserves all app records.',
			operation === 'reclaim' ? 'Reclaim space' : 'Optimize'
		)) return;
		await perform(`app-store:${operation}`, async () => {
			const result = await maintainAppStoreAction(operation);
			if (result.principal !== snapshot?.principal || result.workspace !== snapshot?.workspace || result.operation !== operation) {
				throw new Error('App maintenance response does not match the selected workspace and operation');
			}
			if (!result.encrypted || !result.integrity_ok) throw new Error('App database integrity verification did not pass');
			appMaintenanceReport = result;
			return result;
		}, (result) => result.checkpoint_busy
			? 'App integrity passed; WAL checkpoint remains busy.'
			: `App database ${operation === 'verify' ? 'integrity verified' : operation === 'optimize' ? 'optimized' : 'compacted'} · records preserved`);
	}

	async function compactOne(entry: StorageEntry): Promise<void> {
		const target = databaseTarget(entry);
		if (!target) return;
		const confirmed = await confirmAction(
			`Compact ${entry.label}?`,
			'Writes a verified fresh copy, keeps a rollback backup during the swap, and does not delete logical rows.',
			'Compact'
		);
		if (!confirmed) return;
		await perform(
			`db:${target}`,
			() => compactDatabaseAction([target]),
			(result) => {
				const report = typeof result === 'number' ? null : result.duckdb[0];
				return report
					? `${entry.label} compacted · ${formatStorageBytes(report.bytes_reclaimed)} reclaimed`
					: `${entry.label} compacted`;
			}
		);
	}

	async function compactAllDatabases(): Promise<void> {
		const targets = databases.map(databaseTarget).filter((target): target is DuckDbTarget => !!target);
		if (targets.length === 0) return;
		const confirmed = await confirmAction(
			'Compact all owned databases?',
			'Each store is locked, copied, fully verified, and atomically swapped independently. Mail rows are not truncated.',
			'Compact all'
		);
		if (!confirmed) return;
		await perform(
			'db:all',
			() => compactDatabaseAction(targets),
			(result) => {
				if (typeof result === 'number') return 'Databases compacted';
				const reclaimed = result.duckdb.reduce((sum, report) => sum + report.bytes_reclaimed, 0);
				return `${result.duckdb.length} databases compacted · ${formatStorageBytes(reclaimed)} reclaimed`;
			}
		);
	}

	async function compactCompletedPartitions(): Promise<void> {
		const confirmed = await confirmAction(
			'Compact analytics now?',
			'Completed batch-owned partitions are verified before raw files are pruned. Canonical LLM facts keep immutable revisions while a rolling prefix reduces active-day read fan-out.',
			'Compact analytics'
		);
		if (!confirmed) return;
		await perform('parquet', compactParquetAction, (result) => {
			const partitions =
				Number(result.parquet?.partitions_compacted ?? 0) +
				Number(result.canonical_llm?.partitions_compacted ?? 0);
			const files =
				Number(result.parquet?.raw_files_compacted ?? 0) +
				Number(result.canonical_llm?.raw_files_compacted ?? 0);
			return `${partitions} partitions compacted · ${files} source files folded`;
		});
	}

	async function clearMetrics(): Promise<void> {
		if (!metrics || (metrics.event_count === 0 && metrics.storage_bytes === 0)) return;
		const confirmed = await confirmAction(
			'Clear compaction metrics?',
			`Deletes only ${metrics.event_count.toLocaleString()} content-free history records (${formatStorageBytes(metrics.storage_bytes)}). Databases, Parquet files, manifests, and recovery state are untouched.`,
			'Clear metrics',
			true
		);
		if (!confirmed) return;
		await perform('metrics:clear', clearMetricsAction, () => 'Compaction metrics cleared');
	}

	async function applyRetention(): Promise<void> {
		const confirmed = await confirmAction(
			'Apply 90-day telemetry retention?',
			'Removes expired observability partitions, including memory diagnostics and tool-call lineage. It never touches mail, tasks, memory facts, or recovery journals.',
			'Apply retention',
			true
		);
		if (!confirmed) return;
		await perform('retention', () => retentionAction(90), (result) => {
			if (typeof result === 'number') return 'Telemetry retention applied';
			const partitions = Number(result.retention?.partitions_removed ?? 0);
			const bytes = Number(result.retention?.bytes_removed ?? 0);
			return `${partitions} expired partitions removed · ${formatStorageBytes(bytes)} released`;
		});
	}

	async function purgeFeed(): Promise<void> {
		const confirmed = await confirmAction(
			'Purge stale feed projections?',
			'Only rows proven orphaned against the authoritative task list are removed. The operation refuses to run if that list is unavailable.',
			'Purge stale rows',
			true
		);
		if (!confirmed) return;
		await perform('feed', purgeFeedAction, (result) => `${Number(result)} stale feed rows removed`);
	}

	async function optimizeAttention(): Promise<void> {
		await perform('attention:optimize', optimizeAttentionAction, (result) => {
			const analyzed = result.optimize?.analyzed_tables.length ?? 0;
			return `Attention learning optimized · ${analyzed} hot tables analyzed`;
		});
	}

	async function previewAttentionRetention(): Promise<void> {
		attentionRetentionPreview = null;
		await perform(
			'attention:preview',
			() => previewAttentionRetentionAction(attentionRetentionDays),
			(result) => {
				attentionRetentionPreview = result;
				const rows = result.retention
					? Object.values(result.retention.affected_rows).reduce((sum, count) => sum + count, 0)
					: 0;
				return `${rows.toLocaleString()} old attention rows eligible for scoped cleanup`;
			}
		);
	}

	async function applyAttentionRetentionPreview(): Promise<void> {
		if (!attentionPreviewCurrent || !attentionRetentionPreview?.retention) return;
		const confirmed = await confirmAction(
			`Clean attention history older than ${attentionRetentionDays} days?`,
			`Removes up to ${attentionPreviewRows.toLocaleString()} eligible analytical rows only from ${attentionRetentionPreview.principal}/${attentionRetentionPreview.workspace}. Current models, active evidence, and referenced decisions remain protected.`,
			'Clean old history',
			true
		);
		if (!confirmed) return;
		await perform(
			'attention:apply',
			() => applyAttentionRetentionAction(attentionRetentionDays),
			(result) => {
				const rows = result.retention
					? Object.values(result.retention.affected_rows).reduce((sum, count) => sum + count, 0)
					: 0;
				attentionRetentionPreview = null;
				return `${rows.toLocaleString()} eligible attention-history rows cleaned`;
			}
		);
	}

	async function reclaimAttentionSpace(): Promise<void> {
		const confirmed = await confirmAction(
			'Reclaim attention-learning disk space?',
			'Pauses attention-learning writes while it creates and verifies a complete SQLite rebuild. Reads stay online until a brief final drain, then the verified file is atomically installed with rollback protection. No logical rows are deleted.',
			'Reclaim disk space'
		);
		if (!confirmed) return;
		await perform('attention:reclaim', reclaimAttentionAction, (result) => {
			const reclaimed = result.reclaim?.bytes_reclaimed ?? 0;
			return `Attention database rebuilt · ${formatStorageBytes(reclaimed)} reclaimed`;
		});
	}

	function safetyLabel(value: StorageEntry['safety_class']): string {
		return value.replace('_', ' ');
	}

	function kindLabel(value: StorageEntry['kind']): string {
		if (value === 'duck_db') return 'DuckDB';
		if (value === 'sqlite') return 'SQLite';
		if (value === 'parquet') return 'Parquet';
		if (value === 'directory') return 'Directory';
		return 'Journal';
	}

	function formatCount(value: number | null): string {
		return value == null ? '—' : new Intl.NumberFormat().format(value);
	}

	function formatWhen(value: number | null): string {
		return value == null
			? 'Never'
			: new Intl.DateTimeFormat(undefined, {
				month: 'short',
				day: 'numeric',
				hour: 'numeric',
				minute: '2-digit'
			}).format(new Date(value));
	}

	function metricKindLabel(value: string): string {
		if (value === 'duck_db') return 'Database';
		if (value === 'canonical_llm') return 'Canonical LLM';
		return 'Parquet';
	}
</script>

<section class="storage-shell" aria-busy={loading || !!activeAction}>
	<header class="storage-hero">
		<div class="hero-copy">
			<span class="eyebrow">Storage governance</span>
			<h1>See what grows. Keep what matters.</h1>
			<p>
				A scope-aware view of backend-owned databases, analytics partitions, and recovery state —
				with guarded maintenance that understands each store’s role.
			</p>
			{#if snapshot}
				<div class="scope-chip"><span>{snapshot.principal}</span><i>/</i><span>{snapshot.workspace}</span></div>
			{/if}
		</div>
		<div class="hero-total">
			<span>Tracked footprint</span>
			<strong>{snapshot ? formatStorageBytes(snapshot.total_size_bytes) : '—'}</strong>
			<small>{snapshot ? `${snapshot.entries.reduce((sum, entry) => sum + entry.file_count, 0).toLocaleString()} files · ${formatStorageBytes(snapshot.total_allocated_bytes)} disk blocks · ${formatStorageBytes(totalWalBytes)} WAL${partialInventoryCount > 0 ? ` · ${partialInventoryCount} partial ${partialInventoryCount === 1 ? 'inventory' : 'inventories'}` : ''}` : 'Loading inventory'}</small>
		</div>
	</header>

	<div class="toolbar">
		<div>
			{#if largest}<span>Largest now</span><strong>{largest.label} · {formatStorageBytes(largest.size_bytes + largest.wal_bytes)}</strong>{/if}
		</div>
		<div class="toolbar-actions">
			<button class="button quiet" type="button" disabled={!!activeAction || loading} on:click={refresh}>Refresh</button>
			<button class="button" type="button" disabled={!!activeAction || databases.length === 0} on:click={compactAllDatabases}>
				{activeAction === 'db:all' ? 'Verifying…' : 'Compact databases'}
			</button>
		</div>
	</div>

	{#if error}<div class="notice error" role="alert">{error}</div>{/if}
	{#if lastResult}<div class="notice success" role="status">{lastResult}</div>{/if}

	<section id="activation" class="section-block" aria-labelledby="activation-title">
		<div class="section-heading">
			<div><span>Track B</span><h2 id="activation-title">Remote activation</h2></div>
			<p>
				Inventory and status are qualified. Cutover stays blocked until every Task 19
				precondition holds (Gates 1–3, recent backup, remote health). Switching Runtime
				provider does not move data.
			</p>
		</div>
		{#if activation}
			<p>
				Gates: 1 {activation.gates.gate1_closed ? 'closed' : 'open'} · 2
				{activation.gates.gate2_closed ? 'closed' : 'open'} · 3
				{activation.gates.gate3_closed ? 'closed' : 'open'}. Canonical source remains local.
			</p>
			{#if activation.blocking.length}
				<p role="status">Blocked: {activation.blocking.join(', ')}</p>
			{/if}
			<button class="button" type="button" disabled aria-disabled="true">
				Cut over to remote (not qualified)
			</button>
		{:else}
			<p role="status">Activation status is unavailable. Use <code>magician storage status</code>.</p>
		{/if}
	</section>

    <section class="section-block" aria-labelledby="maintenance-title">
        <div class="section-heading">
            <div><h2 id="maintenance-title">Automatic maintenance</h2></div>
            <p>Storage optimization runs while the service stays online. Related requests may briefly wait.</p>
        </div>
        <div aria-live="polite">
            {#if maintenanceError}<p>{maintenanceError}</p>{/if}
            {#each maintenance as entry (entry.database)}
                <p><strong>{entry.database === 'channel_assist' ? 'Comms intelligence' : 'Feed'} · {entry.state}</strong><br />
                {entry.message}
                {#if entry.last_success_at_ms}<br />Last completed {new Date(entry.last_success_at_ms).toLocaleString()} · {formatStorageBytes(entry.bytes_reclaimed)} reclaimed{/if}</p>
            {/each}
        </div>
    </section>

	{#if loading && !snapshot}
		<div class="loading-grid" aria-label="Loading storage inventory">
			{#each Array(6) as _}<div class="skeleton"></div>{/each}
		</div>
	{:else if snapshot}
		<section class="section-block" aria-labelledby="databases-title">
			<div class="section-heading">
				<div><span>Live stores</span><h2 id="databases-title">Databases</h2></div>
				<p>Compaction changes the physical layout, never the logical records.</p>
			</div>
			<div class="database-grid">
				{#each databases as entry (entry.id)}
					<article class="database-card">
						<div class="card-top">
							<div class="store-mark">{entry.label.slice(0, 1)}</div>
							<div><span class={`safety ${entry.safety_class}`}>{safetyLabel(entry.safety_class)}</span><h3>{entry.label}</h3></div>
						</div>
						<div class="primary-metric"><strong>{formatStorageBytes(entry.size_bytes)}</strong><span>{kindLabel(entry.kind)}</span></div>
						<dl class="mini-metrics">
							<div><dt>WAL</dt><dd>{formatStorageBytes(entry.wal_bytes)}</dd></div>
							<div><dt>Rows</dt><dd>{formatCount(entry.row_count)}</dd></div>
							<div><dt>Disk blocks</dt><dd>{formatStorageBytes(entry.allocated_bytes)}</dd></div>
						</dl>
						<p class="policy">{entry.policy}</p>
						<div class="card-footer">
							<code>{entry.relative_path}</code>
							{#if databaseTarget(entry)}
								<button class="text-action" type="button" disabled={!!activeAction} on:click={() => compactOne(entry)}>
									{activeAction === `db:${databaseTarget(entry)}` ? 'Verifying…' : 'Compact'}
								</button>
							{:else if entry.id === 'feed_duckdb'}
								<button class="text-action" type="button" disabled={!!activeAction} on:click={purgeFeed}>
									{activeAction === 'feed' ? 'Purging…' : 'Purge stale'}
								</button>
							{/if}
						</div>
					</article>
				{/each}
			</div>
		</section>

		{#if appStore}
			<section class="section-block" aria-labelledby="app-storage-title">
				<div class="section-heading"><div><span>Workspace apps</span><h2 id="app-storage-title">App database</h2></div></div>
				<article class="attention-card">
					<strong>{formatStorageBytes(appSizeReport?.database_bytes_after ?? appStore.size_bytes)} · SQLCipher encrypted</strong>
					<p>{appStore.policy}</p>
					<code>scopes/{snapshot?.principal}/{snapshot?.workspace}/{appStore.relative_path}</code>
					<p>Location relative to the runtime data root.</p>
                    <p><a href="/apps">Manage older app data</a> — choose an app and preview a date-based cleanup.</p>
					<p>{formatStorageBytes(appSizeReport?.wal_bytes_after ?? appStore.wal_bytes)} WAL{#if appSizeReport || appStore.shm_bytes !== undefined} · {formatStorageBytes(appSizeReport?.shm_bytes_after ?? appStore.shm_bytes ?? 0)} shared memory{:else} · shared-memory size unavailable{/if}</p>
					{#if currentAppReport}
						<p role="status">Integrity verified at {new Date(currentAppReport.completed_at).toLocaleString()} · {currentAppReport.free_pages} free pages{#if currentAppReport.checkpoint_busy} · WAL checkpoint still busy{/if}</p>
					{:else}
						<p>Integrity has not been checked in this session.</p>
					{/if}
					<div class="card-footer">
						{#each ['verify', 'optimize', 'reclaim'] as operation}
							{@const action = appStore.actions.find((item) => item.id === `app_store_${operation}`)}
							{#if action}<button type="button" class="text-action" disabled={!!activeAction || appStore.file_count === 0} on:click={() => maintainApps(operation as AppStoreMaintenanceOperation)}>{activeAction === `app-store:${operation}` ? 'Working…' : action.label}</button>{/if}
						{/each}
					</div>
				</article>
			</section>
		{/if}

		{#if attentionStore}
			<section class="section-block attention-maintenance" aria-labelledby="attention-storage-title">
				<div class="section-heading">
					<div><span>Learning ledger</span><h2 id="attention-storage-title">Attention learning</h2></div>
					<p>Optimize online, preview scope-owned history cleanup, or reclaim physical space with a controlled drain.</p>
				</div>
				<div class="attention-card">
					<div class="attention-summary">
						<div class="store-mark">A</div>
						<div>
							<strong>{formatStorageBytes(attentionStore.size_bytes)}</strong>
							<small>{formatStorageBytes(attentionStore.wal_bytes)} WAL · shared physical SQLite file</small>
							<p>{attentionStore.policy}</p>
						</div>
					</div>
					<div class="attention-actions">
						<div class="attention-action-block">
							<div><strong>Optimize now</strong><small>Refresh query plans; preserve all rows and file size.</small></div>
							<button class="button" type="button" disabled={!!activeAction} on:click={optimizeAttention}>
								{activeAction === 'attention:optimize' ? 'Optimizing…' : 'Optimize now'}
							</button>
						</div>
						<div class="attention-action-block retention-control">
							<div>
								<strong>Clean old history</strong>
								<small>Only {snapshot.principal}/{snapshot.workspace}; preview is mandatory.</small>
							</div>
							<label>
								<span>Keep</span>
								<select bind:value={attentionRetentionDays} on:change={() => (attentionRetentionPreview = null)} disabled={!!activeAction}>
									<option value={30}>30 days</option>
									<option value={90}>90 days</option>
									<option value={180}>180 days</option>
									<option value={365}>1 year</option>
								</select>
							</label>
							<button class="button quiet" type="button" disabled={!!activeAction} on:click={previewAttentionRetention}>
								{activeAction === 'attention:preview' ? 'Previewing…' : 'Preview cleanup'}
							</button>
							<button class="button danger" type="button" disabled={!!activeAction || !attentionPreviewCurrent} on:click={applyAttentionRetentionPreview}>
								{activeAction === 'attention:apply' ? 'Cleaning…' : 'Apply cleanup'}
							</button>
						</div>
						{#if attentionPreviewCurrent && attentionRetentionPreview?.retention}
							<div class="attention-preview" role="status">
								<strong>{attentionPreviewRows.toLocaleString()} eligible rows</strong>
								<span>Cutoff {formatWhen(attentionRetentionPreview.retention.cutoff_at)}</span>
								<small>Logical cleanup does not shrink the SQLite file until disk space is reclaimed.</small>
							</div>
						{/if}
						<div class="attention-action-block">
							<div><strong>Reclaim disk space</strong><small>Writes pause for the rebuild; reads drain only for the verified atomic swap.</small></div>
							<button class="button" type="button" disabled={!!activeAction} on:click={reclaimAttentionSpace}>
								{activeAction === 'attention:reclaim' ? 'Rebuilding and verifying…' : 'Reclaim disk space'}
							</button>
						</div>
					</div>
				</div>
			</section>
		{/if}

		<section class="section-block" aria-labelledby="telemetry-title">
			<div class="section-heading with-actions">
				<div><span>Partitioned history</span><h2 id="telemetry-title">Analytics lake</h2></div>
				<div class="section-actions">
					<button class="button quiet" type="button" disabled={!!activeAction} on:click={compactCompletedPartitions}>
						{activeAction === 'parquet' ? 'Compacting…' : 'Compact analytics'}
					</button>
					<button class="button danger" type="button" disabled={!!activeAction} on:click={applyRetention}>
						{activeAction === 'retention' ? 'Applying…' : 'Apply 90-day retention'}
					</button>
				</div>
			</div>
			<div class="lake-table" role="table" aria-label="Analytics storage">
				<div class="lake-row lake-head" role="row"><span>Dataset</span><span>Size</span><span>Files</span><span>Range</span><span>Policy</span></div>
				{#each telemetry as entry (entry.id)}
					<details class="lake-row-wrap">
						<summary class="lake-row" role="row">
							<span class="dataset-name"><i class={`dot ${entry.safety_class}`}></i><strong>{entry.label}</strong><small>{entry.relative_path}</small></span>
							<span data-label="Size">{formatStorageBytes(entry.size_bytes)}</span>
							<span data-label="Files">{entry.file_count.toLocaleString()}</span>
							<span data-label="Range">{entry.oldest_partition ?? '—'}<small>{entry.newest_partition && entry.newest_partition !== entry.oldest_partition ? ` → ${entry.newest_partition}` : ''}</small></span>
							<span data-label="Policy">{entry.retention_days ? `${entry.retention_days} days` : 'governed'}</span>
						</summary>
						<div class="lake-detail">{entry.policy}</div>
					</details>
				{/each}
			</div>
		</section>

		{#if metrics}
			<section class="section-block compaction-insights" aria-labelledby="compaction-metrics-title">
				<div class="section-heading with-actions">
					<div>
						<span>Measured maintenance</span>
						<h2 id="compaction-metrics-title">Compaction impact</h2>
					</div>
					<div class="metrics-heading-actions">
						<small>Last run · {formatWhen(metrics.last_compaction_at_ms)}</small>
						<button
							class="button danger"
							type="button"
							disabled={!!activeAction || (metrics.event_count === 0 && metrics.storage_bytes === 0)}
							on:click={clearMetrics}
						>
							{activeAction === 'metrics:clear' ? 'Clearing…' : 'Clear metrics'}
						</button>
					</div>
				</div>
				{#if metrics.warning}
					<div class="notice error" role="status">{metrics.warning}</div>
				{/if}
				<div class="impact-grid">
					<div class="impact-card primary">
						<span>Reclaimed · retained history</span>
						<strong>{formatStorageBytes(metrics.total_bytes_reclaimed)}</strong>
						<small>Verified apparent file bytes released</small>
					</div>
					<div class="impact-card">
						<span>Files · retained history</span>
						<strong>{metrics.total_files_compacted.toLocaleString()}</strong>
						<small>Sources folded into governed generations</small>
					</div>
					<div class="impact-card">
						<span>Reads avoided · retained</span>
						<strong>{metrics.total_query_files_avoided.toLocaleString()}</strong>
						<small>Read fan-out removed by canonical prefixes</small>
					</div>
					<div class="impact-card ledger-cost">
						<span>Metrics footprint</span>
						<strong>{formatStorageBytes(metrics.storage_bytes)}</strong>
						<small>{metrics.event_count.toLocaleString()} / {metrics.retained_event_limit.toLocaleString()} retained events · {formatStorageBytes(metrics.allocated_bytes)} disk blocks</small>
					</div>
				</div>

				<p class="metrics-context">
					Canonical facts remain immutable for recovery, so their main win is fewer files per query.
					The ledger records that separately from physical space reclaimed; it has written
					{formatStorageBytes(metrics.total_compacted_bytes_written)} of verified compacted objects.
				</p>

				{#if metrics.areas.length > 0}
					<div class="metrics-table" role="table" aria-label="Compaction metrics by storage area">
						<div class="metrics-row metrics-head" role="row">
							<span>Area</span><span>Runs</span><span>Files</span><span>Reads avoided</span><span>Reclaimed</span>
						</div>
						{#each metrics.areas as area (`${area.kind}:${area.area}`)}
							<div class="metrics-row" role="row">
								<span class="metric-area"><strong>{area.area.replaceAll('_', ' ')}</strong><small>{metricKindLabel(area.kind)} · {area.partitions_compacted.toLocaleString()} partitions</small></span>
								<span data-label="Runs">{area.runs.toLocaleString()}</span>
								<span data-label="Files">{area.files_compacted.toLocaleString()}</span>
								<span data-label="Reads avoided">{area.query_files_avoided.toLocaleString()}</span>
								<span data-label="Reclaimed"><strong>{formatStorageBytes(area.bytes_reclaimed)}</strong><small>{formatWhen(area.last_compacted_at_ms)}</small></span>
							</div>
						{/each}
					</div>
				{:else}
					<div class="metrics-empty">No effective compaction has been recorded for this scope yet.</div>
				{/if}

				{#if metrics.recent_events.length > 0}
					<details class="recent-runs">
						<summary>Recent runs <span>{metrics.recent_events.length} shown</span></summary>
						<div class="recent-list">
							{#each metrics.recent_events as event (event.id)}
								<div>
									<span class={`run-kind ${event.kind}`}>{metricKindLabel(event.kind)}</span>
									<p><strong>{event.area.replaceAll('_', ' ')}</strong><small>{event.trigger.replaceAll('_', ' ')} · {formatWhen(event.completed_at_ms)}</small></p>
									<b>{event.files_compacted.toLocaleString()} files · {formatStorageBytes(event.bytes_reclaimed)} · {event.duration_ms.toLocaleString()} ms</b>
								</div>
							{/each}
						</div>
					</details>
				{/if}
			</section>
		{/if}

		<div class="bottom-grid">
			<section class="section-block compact" aria-labelledby="protected-title">
				<div class="section-heading"><div><span>Hands off by default</span><h2 id="protected-title">Protected state</h2></div></div>
				<div class="protected-list">
					{#each protectedStores as entry (entry.id)}
						<div><span class={`dot ${entry.safety_class}`}></span><div><strong>{entry.label}</strong><small>{kindLabel(entry.kind)}{entry.inventory_complete ? '' : ' · Partial inventory'}</small><p>{entry.policy}</p></div><b>{formatStorageBytes(entry.size_bytes)}</b></div>
					{/each}
				</div>
			</section>
			<section class="section-block compact safeguards" aria-labelledby="safeguards-title">
				<div class="section-heading"><div><span>Before anything disappears</span><h2 id="safeguards-title">Safety contract</h2></div></div>
				<ol>{#each snapshot.safeguards as safeguard}<li>{safeguard}</li>{/each}</ol>
			</section>
		</div>
	{/if}
</section>

<style>
	.storage-shell{box-sizing:border-box;color:var(--text-primary);display:flex;flex-direction:column;gap:1.15rem;margin:0 auto;max-width:var(--app-content-max,1320px);padding:1.35rem 1.45rem 5rem;width:100%}
	.storage-hero{align-items:stretch;background:linear-gradient(135deg,color-mix(in srgb,var(--bg-card) 94%,var(--accent-primary) 6%),var(--bg-card));border:1px solid var(--border-soft);border-radius:12px;display:grid;gap:1rem;grid-template-columns:minmax(0,1fr) minmax(13rem,.3fr);overflow:hidden;padding:1.3rem;position:relative}
	.storage-hero:after{background:radial-gradient(circle,color-mix(in srgb,var(--accent-primary) 20%,transparent),transparent 70%);content:"";height:18rem;pointer-events:none;position:absolute;right:-6rem;top:-10rem;width:18rem}
	.eyebrow,.section-heading span{color:var(--text-secondary);font-size:.72rem;font-weight:750;letter-spacing:.06em;text-transform:uppercase}
	.hero-copy h1{font-family:var(--font-display,var(--font-primary));font-size:clamp(1.9rem,4vw,3.25rem);letter-spacing:-.035em;line-height:1.02;margin:.35rem 0 .7rem;max-width:18ch}
	.hero-copy p{color:var(--text-secondary);font-size:.94rem;line-height:1.55;margin:0;max-width:68ch}
	.scope-chip{align-items:center;background:var(--bg-soft);border:1px solid var(--border-soft);border-radius:999px;display:inline-flex;font-size:.76rem;gap:.45rem;margin-top:1rem;padding:.34rem .65rem}.scope-chip i{color:var(--text-tertiary);font-style:normal}
	.hero-total{align-items:flex-end;border-left:1px solid var(--border-soft);display:flex;flex-direction:column;justify-content:center;padding:1rem;position:relative;z-index:1}.hero-total span,.hero-total small{color:var(--text-secondary);font-size:.78rem}.hero-total strong{font-family:var(--font-display,var(--font-primary));font-size:clamp(2rem,5vw,3.8rem);letter-spacing:-.05em;line-height:1;margin:.35rem 0}
	.toolbar{align-items:center;background:var(--bg-card);border:1px solid var(--border-soft);border-radius:10px;display:flex;gap:1rem;justify-content:space-between;padding:.72rem .8rem}.toolbar>div:first-child{display:flex;flex-direction:column}.toolbar span{color:var(--text-secondary);font-size:.7rem}.toolbar strong{font-size:.85rem}.toolbar-actions,.section-actions{display:flex;flex-wrap:wrap;gap:.5rem}
	.button,.text-action{appearance:none;border:0;cursor:pointer;font:inherit}.button{background:var(--accent-primary);border:1px solid color-mix(in srgb,var(--accent-primary) 70%,var(--border-soft));border-radius:7px;color:var(--accent-contrast,#fff);font-size:.78rem;font-weight:650;padding:.5rem .72rem}.button.quiet{background:var(--bg-soft);border-color:var(--border-soft);color:var(--text-primary)}.button.danger{background:color-mix(in srgb,var(--danger,#c2410c) 12%,var(--bg-card));border-color:color-mix(in srgb,var(--danger,#c2410c) 35%,var(--border-soft));color:var(--text-primary)}button:disabled{cursor:not-allowed;opacity:.52}
	.notice{border-radius:8px;font-size:.84rem;padding:.7rem .85rem}.notice.error{background:color-mix(in srgb,var(--danger,#c2410c) 10%,var(--bg-card));border:1px solid color-mix(in srgb,var(--danger,#c2410c) 30%,var(--border-soft))}.notice.success{background:color-mix(in srgb,var(--success,#238636) 10%,var(--bg-card));border:1px solid color-mix(in srgb,var(--success,#238636) 30%,var(--border-soft))}
	.loading-grid,.database-grid{display:grid;gap:.8rem;grid-template-columns:repeat(auto-fit,minmax(15rem,1fr))}.skeleton{animation:pulse 1.5s ease-in-out infinite;background:var(--bg-soft);border:1px solid var(--border-soft);border-radius:10px;height:14rem}@keyframes pulse{50%{opacity:.55}}
	.section-block{background:var(--bg-card);border:1px solid var(--border-soft);border-radius:12px;padding:1rem}.section-block.compact{padding:.9rem}.section-heading{align-items:flex-end;display:flex;gap:1rem;justify-content:space-between;margin-bottom:.85rem}.section-heading h2{font-size:1.12rem;margin:.18rem 0 0}.section-heading p{color:var(--text-secondary);font-size:.78rem;margin:0;max-width:34rem;text-align:right}.section-heading.with-actions{align-items:center}
	.database-card{background:color-mix(in srgb,var(--bg-card) 90%,var(--bg-soft) 10%);border:1px solid var(--border-soft);border-radius:10px;display:flex;flex-direction:column;gap:.8rem;min-width:0;padding:.85rem}.card-top{align-items:center;display:flex;gap:.7rem}.store-mark{align-items:center;background:color-mix(in srgb,var(--accent-primary) 13%,var(--bg-soft));border:1px solid color-mix(in srgb,var(--accent-primary) 28%,var(--border-soft));border-radius:8px;color:var(--accent-primary);display:flex;font-weight:800;height:2.25rem;justify-content:center;width:2.25rem}.card-top h3{font-size:.98rem;margin:.12rem 0 0}.safety{color:var(--text-secondary);font-size:.65rem;text-transform:uppercase}.safety.lifecycle_managed{color:var(--warning,#b7791f)}.safety.authoritative{color:var(--accent-primary)}
	.primary-metric{align-items:baseline;display:flex;gap:.5rem}.primary-metric strong{font-family:var(--font-display,var(--font-primary));font-size:1.8rem;letter-spacing:-.035em}.primary-metric span{color:var(--text-secondary);font-size:.72rem}.mini-metrics{display:grid;gap:.45rem;grid-template-columns:repeat(3,1fr);margin:0}.mini-metrics>div{border-top:1px solid var(--border-soft);padding-top:.45rem}.mini-metrics dt{color:var(--text-secondary);font-size:.65rem}.mini-metrics dd{font-size:.77rem;margin:.12rem 0 0}.policy{color:var(--text-secondary);font-size:.76rem;line-height:1.45;margin:0;min-height:2.2rem}.card-footer{align-items:center;border-top:1px solid var(--border-soft);display:flex;gap:.5rem;justify-content:space-between;padding-top:.65rem}.card-footer code{color:var(--text-tertiary);font-size:.63rem;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}.text-action{background:transparent;color:var(--accent-primary);font-size:.74rem;font-weight:700;white-space:nowrap}
	.attention-card{background:color-mix(in srgb,var(--accent-primary) 4%,var(--bg-soft));border:1px solid color-mix(in srgb,var(--accent-primary) 18%,var(--border-soft));border-radius:10px;display:grid;gap:1rem;grid-template-columns:minmax(13rem,.42fr) minmax(0,1fr);padding:.9rem}.attention-summary{align-items:flex-start;display:flex;gap:.75rem}.attention-summary>div:last-child{display:flex;flex-direction:column;min-width:0}.attention-summary strong{font-family:var(--font-display,var(--font-primary));font-size:1.65rem;letter-spacing:-.03em}.attention-summary small{color:var(--text-secondary);font-size:.67rem;margin-top:.15rem}.attention-summary p{color:var(--text-secondary);font-size:.73rem;line-height:1.45;margin:.7rem 0 0}.attention-actions{display:flex;flex-direction:column}.attention-action-block{align-items:center;border-top:1px solid var(--border-soft);display:flex;gap:.7rem;justify-content:space-between;padding:.7rem 0}.attention-action-block:first-child{border-top:0;padding-top:0}.attention-action-block:last-child{padding-bottom:0}.attention-action-block>div{display:flex;flex:1;flex-direction:column;min-width:11rem}.attention-action-block strong{font-size:.8rem}.attention-action-block small{color:var(--text-secondary);font-size:.67rem;line-height:1.35;margin-top:.12rem}.retention-control{flex-wrap:wrap}.retention-control label{align-items:center;display:flex;font-size:.7rem;gap:.35rem}.retention-control select{background:var(--bg-card);border:1px solid var(--border-soft);border-radius:6px;color:var(--text-primary);font:inherit;padding:.4rem .45rem}.attention-preview{background:var(--bg-card);border:1px solid var(--border-soft);border-radius:8px;display:grid;gap:.18rem;margin:.1rem 0 .15rem;padding:.55rem .65rem}.attention-preview strong{font-size:.76rem}.attention-preview span,.attention-preview small{color:var(--text-secondary);font-size:.65rem}.attention-preview small{line-height:1.35}
	.lake-table{border:1px solid var(--border-soft);border-radius:9px;overflow:hidden}.lake-row{align-items:center;display:grid;gap:.7rem;grid-template-columns:minmax(13rem,1.7fr) .65fr .55fr .9fr .65fr;padding:.65rem .75rem}.lake-head{background:var(--bg-soft);color:var(--text-secondary);font-size:.68rem;font-weight:700;text-transform:uppercase}.lake-row-wrap{border-top:1px solid var(--border-soft)}.lake-row-wrap:first-of-type{border-top:0}.lake-row-wrap summary{cursor:pointer;font-size:.78rem;list-style:none}.lake-row-wrap summary::-webkit-details-marker{display:none}.dataset-name{align-items:center;display:grid;gap:.15rem;grid-template-columns:auto 1fr}.dataset-name .dot{grid-row:1/3}.dataset-name small{color:var(--text-tertiary);font-size:.62rem;grid-column:2;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}.dot{background:var(--text-tertiary);border-radius:50%;display:inline-block;height:.48rem;width:.48rem}.dot.observability{background:var(--accent-primary)}.dot.restricted{background:var(--warning,#b7791f)}.dot.authoritative{background:var(--success,#238636)}.lake-row>span>small{color:var(--text-secondary);font-size:.68rem}.lake-detail{background:color-mix(in srgb,var(--bg-soft) 65%,transparent);color:var(--text-secondary);font-size:.76rem;line-height:1.45;padding:.7rem .9rem}
	.metrics-heading-actions{align-items:center;display:flex;gap:.75rem}.metrics-heading-actions small{color:var(--text-secondary);font-size:.72rem}.impact-grid{display:grid;gap:.7rem;grid-template-columns:repeat(4,minmax(0,1fr))}.impact-card{background:var(--bg-soft);border:1px solid var(--border-soft);border-radius:9px;display:flex;flex-direction:column;min-height:6.4rem;padding:.8rem}.impact-card.primary{background:color-mix(in srgb,var(--accent-primary) 8%,var(--bg-soft));border-color:color-mix(in srgb,var(--accent-primary) 25%,var(--border-soft))}.impact-card span{color:var(--text-secondary);font-size:.68rem;font-weight:700;text-transform:uppercase}.impact-card strong{font-family:var(--font-display,var(--font-primary));font-size:1.65rem;letter-spacing:-.03em;margin:.45rem 0 auto}.impact-card small{color:var(--text-secondary);font-size:.65rem;line-height:1.35}.metrics-context{border-left:2px solid color-mix(in srgb,var(--accent-primary) 45%,var(--border-soft));color:var(--text-secondary);font-size:.73rem;line-height:1.5;margin:.8rem 0;padding:.18rem 0 .18rem .7rem}.metrics-table{border:1px solid var(--border-soft);border-radius:9px;overflow:hidden}.metrics-row{align-items:center;border-top:1px solid var(--border-soft);display:grid;font-size:.76rem;gap:.6rem;grid-template-columns:minmax(12rem,1.6fr) .45fr .55fr .75fr .9fr;padding:.62rem .72rem}.metrics-row:first-child{border-top:0}.metrics-head{background:var(--bg-soft);color:var(--text-secondary);font-size:.66rem;font-weight:700;text-transform:uppercase}.metric-area,.metrics-row>span:last-child{display:flex;flex-direction:column;gap:.12rem}.metric-area strong{text-transform:capitalize}.metric-area small,.metrics-row>span:last-child small{color:var(--text-secondary);font-size:.62rem}.metrics-empty{background:var(--bg-soft);border:1px dashed var(--border-soft);border-radius:9px;color:var(--text-secondary);font-size:.78rem;padding:1rem;text-align:center}.recent-runs{border-top:1px solid var(--border-soft);margin-top:.85rem;padding-top:.75rem}.recent-runs summary{cursor:pointer;font-size:.76rem;font-weight:700;list-style:none}.recent-runs summary span{color:var(--text-secondary);font-size:.65rem;font-weight:500;margin-left:.4rem}.recent-list{display:flex;flex-direction:column;margin-top:.55rem}.recent-list>div{align-items:center;border-top:1px solid var(--border-soft);display:grid;gap:.65rem;grid-template-columns:5.6rem 1fr auto;padding:.55rem 0}.recent-list>div:first-child{border-top:0}.recent-list p{display:flex;flex-direction:column;margin:0;text-transform:capitalize}.recent-list p strong{font-size:.75rem}.recent-list p small{color:var(--text-secondary);font-size:.62rem}.recent-list b{font-size:.68rem;font-weight:600}.run-kind{background:var(--bg-soft);border:1px solid var(--border-soft);border-radius:999px;font-size:.59rem;font-weight:700;padding:.22rem .42rem;text-align:center}.run-kind.canonical_llm{background:color-mix(in srgb,var(--accent-primary) 8%,var(--bg-soft));color:var(--accent-primary)}
	.bottom-grid{display:grid;gap:1.15rem;grid-template-columns:1.1fr .9fr}.protected-list{display:flex;flex-direction:column}.protected-list>div{align-items:flex-start;border-top:1px solid var(--border-soft);display:grid;gap:.65rem;grid-template-columns:auto 1fr auto;padding:.65rem 0}.protected-list>div:first-child{border-top:0}.protected-list strong{font-size:.8rem}.protected-list small{color:var(--text-tertiary);display:block;font-size:.64rem;margin-top:.1rem}.protected-list p{color:var(--text-secondary);font-size:.72rem;line-height:1.4;margin:.15rem 0 0}.protected-list b{font-size:.75rem}.safeguards ol{counter-reset:item;display:flex;flex-direction:column;gap:.6rem;list-style:none;margin:0;padding:0}.safeguards li{color:var(--text-secondary);counter-increment:item;display:grid;font-size:.75rem;gap:.6rem;grid-template-columns:1.35rem 1fr;line-height:1.4}.safeguards li:before{align-items:center;background:var(--bg-soft);border:1px solid var(--border-soft);border-radius:50%;color:var(--text-primary);content:counter(item);display:flex;font-size:.62rem;height:1.25rem;justify-content:center;width:1.25rem}
	@media(max-width:900px){.impact-grid{grid-template-columns:1fr 1fr}}
	@media(max-width:800px){.storage-shell{padding:1rem .8rem 4rem}.storage-hero,.attention-card{grid-template-columns:1fr}.hero-total{align-items:flex-start;border-left:0;border-top:1px solid var(--border-soft);padding:.8rem 0 0}.toolbar,.section-heading,.section-heading.with-actions{align-items:flex-start;flex-direction:column}.section-heading p{text-align:left}.bottom-grid{grid-template-columns:1fr}.lake-head,.metrics-head{display:none}.lake-row,.metrics-row{grid-template-columns:1fr 1fr;padding:.75rem}.dataset-name,.metric-area{grid-column:1/-1}.lake-row>span:not(.dataset-name):before,.metrics-row>span:not(.metric-area):before{color:var(--text-secondary);content:attr(data-label);display:block;font-size:.6rem;text-transform:uppercase}.section-actions,.toolbar-actions,.metrics-heading-actions{align-items:stretch;width:100%}.section-actions .button,.toolbar-actions .button,.metrics-heading-actions .button{flex:1}.mini-metrics{grid-template-columns:1fr 1fr}.mini-metrics>div:last-child{grid-column:1/-1}.recent-list>div{grid-template-columns:5rem 1fr}.recent-list b{grid-column:2}.attention-action-block{align-items:stretch}.attention-action-block>.button{align-self:flex-start}}
	@media(max-width:520px){.impact-grid{grid-template-columns:1fr}.impact-card{min-height:5.4rem}}
</style>
