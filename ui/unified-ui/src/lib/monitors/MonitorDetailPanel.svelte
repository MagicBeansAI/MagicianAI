<!--
  Monitor detail (Phase 4, plan §5.3): four focused tabs over the Phase 1-3
  read seams — Latest (latest material update or baseline), Updates (durable
  update ledger), Runs (every accepted run incl. unchanged/degraded), and
  Settings (objective, sources, cadence, strictness, notifications,
  revision). Lifecycle actions ride the same endpoints as the HTTP surface:
  run-now, pause/resume, delete. Deep links land on the EXACT update via
  `initialUpdateId` (`/tasks?type=monitors&selected=…&update=…`).

  Execution-level inspection stays on the shared task detail surface — a
  monitor IS a task, so "Open task view" links to /tasks?selected={id}
  rather than rebuilding a second task-detail implementation.
-->
<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import { browser } from '$app/environment';
	import Badge from '$lib/magician/components/native/Badge.svelte';
	import Button from '$lib/magician/components/native/Button.svelte';
	import EmptyState from '$lib/magician/components/native/EmptyState.svelte';
	import Skeleton from '$lib/magician/components/native/Skeleton.svelte';
	import Tabs from '$lib/magician/components/native/Tabs.svelte';
	import { requestConfirmation } from '$lib/stores/confirmationStore';
	import type {
		MonitorDetailV1,
		MonitorFeedbackVerdict,
		MonitorRunResultV1,
		MonitorUpdateDetailV1
	} from '$lib/types/monitor';
	import {
		deleteMonitor,
		getMonitor,
		getMonitorFeedback,
		getMonitorRuns,
		getMonitorUpdates,
		pauseMonitor,
		resumeMonitor,
		runMonitorNow,
		submitMonitorUpdateFeedback
	} from './api';
	import {
		beginVerdict,
		feedbackStateFromRecords,
		feedbackVerdictLabel,
		isFeedbackInFlight,
		isMaterialUpdate,
		rollbackVerdict,
		settleVerdict,
		verdictOf,
		type FeedbackStateMap
	} from './feedbackState';
	import {
		cadenceSummary,
		matchModeLabel,
		notificationPolicyLabel,
		runStatusBadge
	} from './specForm';

	export let taskId: string;
	/** Exact update to reveal (deep link / notification open). */
	export let initialUpdateId: string | null = null;

	const dispatch = createEventDispatcher<{
		back: void;
		deleted: { taskId: string };
		edit: { detail: MonitorDetailV1 };
		changed: void;
	}>();

	const HISTORY_LIMIT = 50;
	const TAB_LABELS = [
		{ label: 'Latest' },
		{ label: 'Updates' },
		{ label: 'Runs' },
		{ label: 'Settings' }
	];

	let detail: MonitorDetailV1 | null = null;
	let updates: MonitorUpdateDetailV1[] = [];
	let runs: MonitorRunResultV1[] = [];
	/** Per-update verdict state (stored + optimistic), keyed by update_id. */
	let feedback: FeedbackStateMap = {};
	let isLoading = true;
	let loadError: string | null = null;
	let actionError: string | null = null;
	let actionBusy: string | null = null;
	let activeTab = 0;
	let highlightedUpdateId: string | null = null;
	let loadedTaskId: string | null = null;

	$: paused = detail ? scheduleIsPaused(detail) : false;
	$: latestUpdate = updates.length > 0 ? updates[0] : null;
	// Client-only load (static SPA build; no server fetch), re-fires when the
	// selected monitor changes.
	$: if (browser && taskId !== loadedTaskId) void load();

	function scheduleIsPaused(monitorDetail: MonitorDetailV1): boolean {
		const schedule = monitorDetail.schedule;
		return !!schedule && (schedule as { paused?: boolean | null }).paused === true;
	}

	async function load(): Promise<void> {
		// Stale-response guard: capture the task id THIS load is for. The
		// old `loadedTaskId` guard was set at load START, so when the prop
		// changed mid-flight the newer load overwrote `loadedTaskId` and an
		// OLDER Promise.all result then passed `loadedTaskId === taskId`
		// (both now the new id) and clobbered state with stale data. Every
		// write below is gated on `thisTaskId === taskId` instead.
		const thisTaskId = taskId;
		loadedTaskId = thisTaskId;
		isLoading = true;
		loadError = null;
		actionError = null;
		try {
			const [detailResult, updatesResult, runsResult, feedbackResult] = await Promise.all([
				getMonitor(thisTaskId),
				getMonitorUpdates(thisTaskId, HISTORY_LIMIT),
				getMonitorRuns(thisTaskId, HISTORY_LIMIT),
				// Partial tolerance: stored verdicts are decoration on the
				// Updates ledger — a feedback read failure must not block
				// the whole detail surface.
				getMonitorFeedback(thisTaskId, HISTORY_LIMIT).catch(() => null)
			]);
			if (thisTaskId !== taskId) return;
			detail = detailResult;
			updates = updatesResult.items;
			runs = runsResult.items;
			feedback = feedbackResult ? feedbackStateFromRecords(feedbackResult.items) : {};
			if (initialUpdateId) {
				highlightedUpdateId = initialUpdateId;
				if (updates.some((update) => update.update_id === initialUpdateId)) {
					activeTab = 1; // Updates tab — the exact update is highlighted there.
				}
				initialUpdateId = null;
			}
		} catch (err) {
			if (thisTaskId !== taskId) return;
			loadError = err instanceof Error ? err.message : String(err);
			detail = null;
		} finally {
			if (thisTaskId === taskId) isLoading = false;
		}
	}

	async function runAction(name: string, action: () => Promise<unknown>): Promise<void> {
		if (actionBusy) return;
		actionBusy = name;
		actionError = null;
		try {
			await action();
			await load();
			dispatch('changed');
		} catch (err) {
			actionError = err instanceof Error ? err.message : String(err);
		} finally {
			actionBusy = null;
		}
	}

	async function handleDelete(): Promise<void> {
		if (!detail) return;
		const confirmed = await requestConfirmation({
			title: 'Delete monitor',
			message: `Delete "${detail.title}"? Its run history is archived with the task.`,
			confirmLabel: 'Delete',
			destructive: true
		});
		if (!confirmed) return;
		actionBusy = 'delete';
		actionError = null;
		try {
			await deleteMonitor(taskId);
			dispatch('deleted', { taskId });
		} catch (err) {
			actionError = err instanceof Error ? err.message : String(err);
		} finally {
			actionBusy = null;
		}
	}

	/**
	 * Useful / Not relevant on one material update (plan §10). Optimistic:
	 * the clicked verdict shows immediately, settles from the POST response
	 * (identical for the idempotent `recorded:false` replay), and rolls back
	 * on error into the shared action-error banner.
	 */
	async function submitFeedback(
		update: MonitorUpdateDetailV1,
		verdict: MonitorFeedbackVerdict
	): Promise<void> {
		const updateId = update.update_id;
		if (isFeedbackInFlight(feedback, updateId)) return;
		const thisTaskId = taskId;
		const previous = feedback[updateId];
		feedback = beginVerdict(feedback, updateId, verdict);
		actionError = null;
		try {
			const response = await submitMonitorUpdateFeedback(thisTaskId, updateId, { verdict });
			if (thisTaskId !== taskId) return;
			feedback = settleVerdict(feedback, updateId, response);
		} catch (err) {
			if (thisTaskId !== taskId) return;
			feedback = rollbackVerdict(feedback, updateId, previous);
			actionError = err instanceof Error ? err.message : String(err);
		}
	}

	function formatTimestamp(iso: string | null | undefined): string {
		if (!iso) return '—';
		const parsed = Date.parse(iso);
		if (!Number.isFinite(parsed)) return iso;
		return new Date(parsed).toLocaleString();
	}

	function sourceOutcomeSummary(run: MonitorRunResultV1): string {
		const failing = run.source_outcomes.filter((outcome) => outcome.status !== 'ok');
		if (failing.length === 0) return `${run.source_outcomes.length} source(s) ok`;
		return failing
			.map((outcome) => `${outcome.source}: ${outcome.status}`)
			.join(' · ');
	}

</script>

<section class="mon-detail" aria-label="Monitor detail">
	<header class="mon-detail__head">
		<div class="mon-detail__head-main">
			<Button
				variant="outline"
				size="sm"
				label="← Monitors"
				ariaLabel="Back to monitors list"
				on:click={() => dispatch('back')}
			/>
			{#if detail}
				<h2 class="mon-detail__title">{detail.title}</h2>
				<Badge
					text={paused ? 'Paused' : 'Active'}
					color={paused ? 'warning' : 'success'}
				/>
				<span class="mon-detail__rev">rev {detail.monitor_revision}</span>
			{/if}
		</div>
		{#if detail}
			<div class="mon-detail__actions">
				<Button
					variant="secondary"
					size="sm"
					label={actionBusy === 'run' ? 'Starting…' : 'Run now'}
					interactive={actionBusy === null}
					on:click={() => void runAction('run', () => runMonitorNow(taskId))}
				/>
				{#if paused}
					<Button
						variant="secondary"
						size="sm"
						label={actionBusy === 'resume' ? 'Resuming…' : 'Resume'}
						interactive={actionBusy === null}
						on:click={() => void runAction('resume', () => resumeMonitor(taskId))}
					/>
				{:else if detail.schedule}
					<Button
						variant="secondary"
						size="sm"
						label={actionBusy === 'pause' ? 'Pausing…' : 'Pause'}
						interactive={actionBusy === null}
						on:click={() => void runAction('pause', () => pauseMonitor(taskId))}
					/>
				{/if}
				<Button
					variant="secondary"
					size="sm"
					label="Edit"
					interactive={actionBusy === null}
					on:click={() => detail && dispatch('edit', { detail })}
				/>
				<Button
					variant="outline"
					size="sm"
					label={actionBusy === 'delete' ? 'Deleting…' : 'Delete'}
					interactive={actionBusy === null}
					on:click={() => void handleDelete()}
				/>
			</div>
		{/if}
	</header>

	{#if actionError}
		<div class="mon-detail__error" role="alert">
			<span>Action failed: {actionError}</span>
		</div>
	{/if}

	{#if isLoading}
		<div class="mon-detail__skeleton" aria-hidden="true">
			<Skeleton variant="rect" height="2.2rem" />
			<Skeleton variant="rect" height="8rem" />
			<Skeleton variant="rect" height="8rem" />
		</div>
	{:else if loadError}
		<div class="mon-detail__error" role="alert">
			<span>Couldn't load this monitor: {loadError}</span>
			<Button variant="outline" size="sm" label="Retry" on:click={() => void load()} />
		</div>
	{:else if detail}
		<p class="mon-detail__meta">
			{cadenceSummary(detail.schedule)} · {detail.state.schedule_fire_count} fire(s) ·
			updated {formatTimestamp(detail.updated_at)}
		</p>

		<Tabs tabs={TAB_LABELS} bind:activeIndex={activeTab} idBase={`monitor-${taskId}`}>
			{#if activeTab === 0}
				<!-- Latest: the newest update record (material change or baseline). -->
				{#if latestUpdate}
					<article class="mon-update mon-update--latest">
						<header class="mon-update__head">
							<Badge
								text={runStatusBadge(latestUpdate.status).text}
								color={runStatusBadge(latestUpdate.status).color}
							/>
							<span class="mon-update__time">{formatTimestamp(latestUpdate.occurred_at)}</span>
							{#if verdictOf(feedback, latestUpdate.update_id)}
								<Badge
									text={`Marked ${feedbackVerdictLabel(verdictOf(feedback, latestUpdate.update_id) ?? 'useful').toLowerCase()}`}
									color={verdictOf(feedback, latestUpdate.update_id) === 'useful'
										? 'success'
										: 'warning'}
								/>
							{/if}
						</header>
						<h3 class="mon-update__headline">{latestUpdate.headline}</h3>
						<p class="mon-update__summary">{latestUpdate.summary}</p>
						{#each latestUpdate.findings as finding (finding.stable_key + finding.content_fingerprint)}
							<div class="mon-finding">
								<span class="mon-finding__title">{finding.title}</span>
								<span class="mon-finding__why">{finding.why_it_matters}</span>
								{#if finding.canonical_url}
									<a
										class="mon-finding__link"
										href={finding.canonical_url}
										target="_blank"
										rel="noopener noreferrer">{finding.canonical_url}</a
									>
								{/if}
							</div>
						{/each}
						{#if isMaterialUpdate(latestUpdate)}
							{@const latestUpdateRef = latestUpdate}
							<div
								class="mon-update__actions"
								role="group"
								aria-label="Feedback and actions for this update"
							>
								<button
									type="button"
									class="mon-feedback-chip"
									class:mon-feedback-chip--useful={verdictOf(feedback, latestUpdate.update_id) ===
										'useful'}
									aria-pressed={verdictOf(feedback, latestUpdate.update_id) === 'useful'}
									aria-label="Mark this update as useful"
									disabled={isFeedbackInFlight(feedback, latestUpdate.update_id)}
									on:click={() => void submitFeedback(latestUpdateRef, 'useful')}
								>
									Useful
								</button>
								<button
									type="button"
									class="mon-feedback-chip"
									class:mon-feedback-chip--not-relevant={verdictOf(
										feedback,
										latestUpdate.update_id
									) === 'not_relevant'}
									aria-pressed={verdictOf(feedback, latestUpdate.update_id) === 'not_relevant'}
									aria-label="Mark this update as not relevant"
									disabled={isFeedbackInFlight(feedback, latestUpdate.update_id)}
									on:click={() => void submitFeedback(latestUpdateRef, 'not_relevant')}
								>
									Not relevant
								</button>
								<Button
									variant="outline"
									size="sm"
									label="Edit monitor"
									interactive={actionBusy === null}
									on:click={() => detail && dispatch('edit', { detail })}
								/>
								{#if !paused && detail.schedule}
									<Button
										variant="outline"
										size="sm"
										label={actionBusy === 'pause' ? 'Pausing…' : 'Pause monitor'}
										interactive={actionBusy === null}
										on:click={() => void runAction('pause', () => pauseMonitor(taskId))}
									/>
								{/if}
							</div>
						{/if}
					</article>
				{:else}
					<EmptyState
						icon="◉"
						title="No updates yet"
						description="The first run records a quiet baseline; material changes show up here."
					/>
				{/if}
			{:else if activeTab === 1}
				<!-- Updates: server-recorded material-change history. -->
				{#if updates.length === 0}
					<EmptyState
						icon="◉"
						title="No recorded updates"
						description="Material changes (and baselines) land here as the monitor runs."
					/>
				{:else}
					<ol class="mon-update-list">
						{#each updates as update (update.update_id)}
							<li
								class="mon-update"
								class:mon-update--highlighted={update.update_id === highlightedUpdateId}
							>
								<header class="mon-update__head">
									<Badge
										text={runStatusBadge(update.status).text}
										color={runStatusBadge(update.status).color}
									/>
									<span class="mon-update__time">{formatTimestamp(update.occurred_at)}</span>
									{#if !update.notification.emitted}
										<Badge text="Not notified" color="default" />
									{/if}
									{#if verdictOf(feedback, update.update_id)}
										<Badge
											text={`Marked ${feedbackVerdictLabel(verdictOf(feedback, update.update_id) ?? 'useful').toLowerCase()}`}
											color={verdictOf(feedback, update.update_id) === 'useful'
												? 'success'
												: 'warning'}
										/>
									{/if}
								</header>
								<h3 class="mon-update__headline">{update.headline}</h3>
								<p class="mon-update__summary">{update.summary}</p>
								{#if isMaterialUpdate(update)}
									<!-- Plan §10: every MATERIAL update offers the four actions. -->
									<div
										class="mon-update__actions"
										role="group"
										aria-label="Feedback and actions for this update"
									>
										<button
											type="button"
											class="mon-feedback-chip"
											class:mon-feedback-chip--useful={verdictOf(feedback, update.update_id) ===
												'useful'}
											aria-pressed={verdictOf(feedback, update.update_id) === 'useful'}
											aria-label="Mark this update as useful"
											disabled={isFeedbackInFlight(feedback, update.update_id)}
											on:click={() => void submitFeedback(update, 'useful')}
										>
											Useful
										</button>
										<button
											type="button"
											class="mon-feedback-chip"
											class:mon-feedback-chip--not-relevant={verdictOf(
												feedback,
												update.update_id
											) === 'not_relevant'}
											aria-pressed={verdictOf(feedback, update.update_id) === 'not_relevant'}
											aria-label="Mark this update as not relevant"
											disabled={isFeedbackInFlight(feedback, update.update_id)}
											on:click={() => void submitFeedback(update, 'not_relevant')}
										>
											Not relevant
										</button>
										<Button
											variant="outline"
											size="sm"
											label="Edit monitor"
											interactive={actionBusy === null}
											on:click={() => detail && dispatch('edit', { detail })}
										/>
										{#if !paused && detail.schedule}
											<Button
												variant="outline"
												size="sm"
												label={actionBusy === 'pause' ? 'Pausing…' : 'Pause monitor'}
												interactive={actionBusy === null}
												on:click={() => void runAction('pause', () => pauseMonitor(taskId))}
											/>
										{/if}
									</div>
								{/if}
							</li>
						{/each}
					</ol>
				{/if}
			{:else if activeTab === 2}
				<!-- Runs: every accepted run, incl. unchanged and degraded. -->
				{#if runs.length === 0}
					<EmptyState
						icon="▷"
						title="No runs yet"
						description="Run now, or wait for the schedule to fire."
					/>
				{:else}
					<ol class="mon-run-list">
						{#each runs as run (run.execution_id)}
							<li class="mon-run">
								<Badge
									text={runStatusBadge(run.status).text}
									color={runStatusBadge(run.status).color}
								/>
								<div class="mon-run__body">
									<span class="mon-run__time">{formatTimestamp(run.completed_at)}</span>
									<span class="mon-run__counts">
										{run.counts.scanned} scanned · {run.counts.new} new ·
										{run.counts.updated} updated · {run.counts.unchanged} unchanged
									</span>
									<span class="mon-run__sources">{sourceOutcomeSummary(run)}</span>
									{#if run.access_problem}
										<span class="mon-run__access" role="alert">
											Access problem — {run.access_problem.source}: {run.access_problem.message}
										</span>
									{/if}
								</div>
							</li>
						{/each}
					</ol>
				{/if}
			{:else}
				<!-- Settings: the typed contract, read-only (Edit opens the composer). -->
				<dl class="mon-settings">
					<div class="mon-settings__row">
						<dt>Objective</dt>
						<dd>{detail.spec.objective}</dd>
					</div>
					<div class="mon-settings__row">
						<dt>URLs</dt>
						<dd>{detail.spec.sources.urls.join(', ') || '—'}</dd>
					</div>
					<div class="mon-settings__row">
						<dt>Domains</dt>
						<dd>{detail.spec.sources.domains.join(', ') || '—'}</dd>
					</div>
					<div class="mon-settings__row">
						<dt>Signed-in sources</dt>
						<dd>{detail.spec.sources.authenticated_sources.join(', ') || '—'}</dd>
					</div>
					<div class="mon-settings__row">
						<dt>Search phrases</dt>
						<dd>{detail.spec.query_seeds.join(', ') || '—'}</dd>
					</div>
					<div class="mon-settings__row">
						<dt>Include rules</dt>
						<dd>{detail.spec.include_rules.join(', ') || '—'}</dd>
					</div>
					<div class="mon-settings__row">
						<dt>Exclude rules</dt>
						<dd>{detail.spec.exclude_rules.join(', ') || '—'}</dd>
					</div>
					<div class="mon-settings__row">
						<dt>Matching</dt>
						<dd>{matchModeLabel(detail.spec.match_mode)}</dd>
					</div>
					<div class="mon-settings__row">
						<dt>Notifications</dt>
						<dd>
							{notificationPolicyLabel(detail.spec.notification_policy)}
							{#if detail.spec.notify_initial_baseline}
								· baseline notifies
							{/if}
						</dd>
					</div>
					<div class="mon-settings__row">
						<dt>Schedule</dt>
						<dd>{cadenceSummary(detail.schedule)}</dd>
					</div>
					<div class="mon-settings__row">
						<dt>Revision</dt>
						<dd>{detail.monitor_revision}</dd>
					</div>
					<div class="mon-settings__row">
						<dt>Task</dt>
						<dd>
							<a class="mon-settings__task-link" href={`/tasks?selected=${encodeURIComponent(taskId)}`}>
								Open task view
							</a>
						</dd>
					</div>
				</dl>
			{/if}
		</Tabs>
	{/if}
</section>

<style>
	.mon-detail {
		display: flex;
		flex-direction: column;
		gap: 0.8rem;
	}

	.mon-detail__head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
		flex-wrap: wrap;
	}

	.mon-detail__head-main {
		display: flex;
		align-items: center;
		gap: 0.6rem;
		min-width: 0;
		flex-wrap: wrap;
	}

	.mon-detail__title {
		margin: 0;
		font-family: var(--font-display, var(--font-primary));
		font-size: 1.1rem;
		font-weight: 700;
		color: var(--text-primary);
		overflow-wrap: anywhere;
	}

	.mon-detail__rev {
		padding: 0.05rem 0.4rem;
		border-radius: var(--radius-full, 999px);
		background: color-mix(in srgb, var(--text-primary) 6%, transparent);
		font-family: var(--font-mono, monospace);
		font-size: 0.68rem;
		color: var(--text-muted);
	}

	.mon-detail__actions {
		display: flex;
		gap: 0.4rem;
		flex-wrap: wrap;
	}

	.mon-detail__meta {
		margin: 0;
		font-size: 0.78rem;
		color: var(--text-secondary);
	}

	.mon-detail__skeleton {
		display: flex;
		flex-direction: column;
		gap: 0.6rem;
	}

	.mon-detail__error {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
		padding: 0.6rem 0.85rem;
		border: 1px solid color-mix(in srgb, var(--color-error, var(--status-failed)) 34%, transparent);
		border-radius: var(--radius-md, 8px);
		background: color-mix(in srgb, var(--color-error, var(--status-failed)) 10%, transparent);
		color: var(--color-error, var(--status-failed));
		font-size: 0.82rem;
	}

	.mon-update-list,
	.mon-run-list {
		margin: 0;
		padding: 0;
		list-style: none;
		display: flex;
		flex-direction: column;
		gap: 0.6rem;
	}

	.mon-update {
		display: flex;
		flex-direction: column;
		gap: 0.35rem;
		padding: 0.7rem 0.8rem;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-md, 8px);
		background: var(--bg-card);
	}

	.mon-update--highlighted {
		border-color: color-mix(in srgb, var(--accent-primary, currentColor) 55%, var(--border-soft));
		box-shadow: 0 0 0 2px
			color-mix(in srgb, var(--accent-primary, currentColor) 25%, transparent);
	}

	.mon-update__head {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		flex-wrap: wrap;
	}

	.mon-update__time {
		font-size: 0.74rem;
		color: var(--text-muted);
	}

	.mon-update__headline {
		margin: 0;
		font-size: 0.95rem;
		font-weight: 650;
		color: var(--text-primary);
		overflow-wrap: anywhere;
	}

	.mon-update__summary {
		margin: 0;
		font-size: 0.83rem;
		line-height: 1.45;
		color: var(--text-secondary);
		overflow-wrap: anywhere;
	}

	.mon-update__actions {
		display: flex;
		align-items: center;
		gap: 0.4rem;
		flex-wrap: wrap;
		margin-top: 0.2rem;
	}

	/* Verdict toggle chips (aria-pressed carries the state; all tokens). */
	.mon-feedback-chip {
		display: inline-flex;
		align-items: center;
		min-height: 1.75rem;
		padding: 0.25rem 0.625rem;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-full, 999px);
		background: color-mix(in srgb, var(--bg-card) 88%, transparent);
		color: var(--text-secondary);
		font-family: var(--font-primary);
		font-size: 0.75rem;
		font-weight: 600;
		cursor: pointer;
	}

	.mon-feedback-chip:disabled {
		cursor: default;
		opacity: 0.5;
	}

	.mon-feedback-chip:not(:disabled):hover {
		border-color: color-mix(in srgb, var(--accent-primary, currentColor) 42%, var(--border-soft));
		color: var(--text-primary);
	}

	.mon-feedback-chip:focus-visible {
		outline: 2px solid color-mix(in srgb, var(--accent-primary, currentColor) 55%, transparent);
		outline-offset: 1px;
	}

	.mon-feedback-chip--useful {
		border-color: color-mix(in srgb, var(--color-success, var(--status-completed)) 40%, transparent);
		background: color-mix(in srgb, var(--color-success, var(--status-completed)) 12%, transparent);
		color: var(--color-success, var(--status-completed));
	}

	.mon-feedback-chip--not-relevant {
		border-color: color-mix(in srgb, var(--color-warning, var(--status-attention)) 40%, transparent);
		background: color-mix(in srgb, var(--color-warning, var(--status-attention)) 12%, transparent);
		color: var(--color-warning, var(--status-attention));
	}

	.mon-finding {
		display: flex;
		flex-direction: column;
		gap: 0.15rem;
		padding: 0.45rem 0.6rem;
		border-left: 2px solid var(--border-soft);
	}

	.mon-finding__title {
		font-size: 0.83rem;
		font-weight: 600;
		color: var(--text-primary);
		overflow-wrap: anywhere;
	}

	.mon-finding__why {
		font-size: 0.76rem;
		color: var(--text-secondary);
	}

	.mon-finding__link {
		font-size: 0.74rem;
		color: var(--accent-primary, var(--text-secondary));
		overflow-wrap: anywhere;
	}

	.mon-run {
		display: flex;
		align-items: flex-start;
		gap: 0.6rem;
		padding: 0.55rem 0.7rem;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-md, 8px);
		background: var(--bg-card);
	}

	.mon-run__body {
		display: flex;
		flex-direction: column;
		gap: 0.15rem;
		min-width: 0;
	}

	.mon-run__time {
		font-size: 0.78rem;
		font-weight: 600;
		color: var(--text-primary);
	}

	.mon-run__counts,
	.mon-run__sources {
		font-size: 0.74rem;
		color: var(--text-secondary);
		overflow-wrap: anywhere;
	}

	.mon-run__access {
		font-size: 0.76rem;
		color: var(--color-warning, var(--status-attention));
	}

	.mon-settings {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
		margin: 0;
	}

	.mon-settings__row {
		display: grid;
		grid-template-columns: 9rem 1fr;
		gap: 0.6rem;
	}

	.mon-settings__row dt {
		font-size: 0.72rem;
		font-weight: 700;
		letter-spacing: 0.04em;
		text-transform: uppercase;
		color: var(--text-muted);
	}

	.mon-settings__row dd {
		margin: 0;
		font-size: 0.84rem;
		color: var(--text-primary);
		overflow-wrap: anywhere;
	}

	.mon-settings__task-link {
		color: var(--accent-primary, var(--text-secondary));
		font-weight: 600;
		text-decoration: none;
	}

	.mon-settings__task-link:hover,
	.mon-settings__task-link:focus-visible {
		text-decoration: underline;
	}
</style>
