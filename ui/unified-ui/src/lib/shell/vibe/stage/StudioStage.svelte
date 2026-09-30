<script lang="ts">
	/**
	 * Stage — the cockpit's right column. Segmented Preview | Code | Diff |
	 * Tests, a live status-chip bar (cost / context / model / retry / queue /
	 * compaction), the Build/Discuss mode switch, and Apply-all/Deploy actions.
	 * The Diff tab is THE one diff review home: the Code tab shows repo STATE
	 * (changed-file stats + full-repo browse) and routes review clicks there.
	 */
	import { createEventDispatcher } from 'svelte';
	import { get } from 'svelte/store';
	import Checkbox from '$lib/magician/components/generative/Checkbox.svelte';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import type { IconName } from '$lib/shared/icons/paths';
	import VibePreviewPanel from '$lib/shell/VibePreviewPanel.svelte';
	import StageDiff from '$lib/shell/vibe/stage/StageDiff.svelte';
	import {
		runCheck,
		fetchProjectFiles,
		fetchProjectFile,
		fetchProjectScreenshots,
		scopedImageUrl,
		type CheckResult,
		type CheckSpec,
		type ProjectFile,
		type Screenshot
	} from '$lib/shell/vibe/stage/checks';
	import { vibeStudioStore, type StageTab, type DeviceFrame } from '$lib/stores/vibeStudioStore';
	import {
		vibeDevProjectStore,
		type VibeDevDeployTarget,
		type VibeDevDeployment,
		type VibeDevProject
	} from '$lib/stores/vibeDevProjectStore';
	import type { RunMeta, SpineCard } from '$lib/shell/vibe/conversation/spineModel';
	import type { HitlRequest, HitlSource } from '$lib/hitl/types';
	import type { VibeRow, VibeChangedFile } from '$lib/stores/vibeHitlStore';

	/** First-run state (no run in view AND no runs exist yet): the tab strip and
	 *  run-only controls are hidden and a single narrative card explains what
	 *  will appear here. The CLI Agent remains available in the status bar. */
	export let firstRun = false;
	export let projectId: string | null = null;
	export let activeProject: VibeDevProject | null = null;
	export let pinnedUrl: string | null = null;
	export let previewAvailable = true;
	export let editMode = false;
	export let checks: CheckSpec[] = [];
	export let meta: RunMeta | null = null;
	export let changedFiles: VibeChangedFile[] = [];
	export let testCards: SpineCard[] = [];
	// StageDiff wiring
	export let orderedDiffRows: VibeRow[] = [];
	export let orderedOtherRows: VibeRow[] = [];
	export let bulkApplying = false;
	export let actingKeys: Set<string> = new Set();
	/** Optional external diff focus (spine diff-card review): feeds the same
	 *  path-keyed scroll mechanism as the Code tab's openFileInDiff. */
	export let focusDiffPath: string | null = null;

	const dispatch = createEventDispatcher<{
		approveAll: void;
		toggleAutoApply: { value: boolean };
		applyDiff: { request: HitlRequest };
		rejectDiff: { request: HitlRequest };
		applyFile: { request: HitlRequest; path: string };
		rejectFile: { request: HitlRequest; path: string };
		respond: { row: VibeRow };
		toggleEdit: void;
		attemptFix: { result: CheckResult };
		/** First-run example-prompt chip clicked — the parent fills the composer
		 *  and focuses it. */
		seedPrompt: { text: string };
	}>();

	// First-run example prompts — clicking one fills the composer and focuses
	// it (the parent binds the text into the prompt field). Kept grammatically
	// parallel: all imperative "Build …".
	const FIRST_RUN_PROMPTS = [
		'Build a habit tracker with streaks',
		'Build a landing page with pricing tiers',
		'Build a pomodoro timer with sounds'
	];

	// Self-heal checks (S1)
	let checkResults: CheckResult[] = [];
	let runningKind: string | null = null;
	let checkError = '';

	async function runChecks(kind: string): Promise<void> {
		if (!projectId || runningKind) return;
		runningKind = kind;
		checkError = '';
		const res = await runCheck(projectId, kind);
		runningKind = null;
		if ('error' in res) {
			checkError = res.error;
			return;
		}
		if (kind === 'all') {
			checkResults = res.results;
		} else {
			const byKind = new Map(checkResults.map((r) => [r.kind, r]));
			for (const r of res.results) byKind.set(r.kind, r);
			checkResults = Array.from(byKind.values());
		}
		if (res.note) checkError = res.note;
	}

	$: studio = $vibeStudioStore;
	$: stageTab = previewAvailable ? studio.stageTab : studio.stageTab === 'preview' ? 'code' : studio.stageTab;
	$: deviceFrame = studio.deviceFrame;

	const DEVICE_WIDTH: Record<DeviceFrame, string> = {
		desktop: '100%',
		tablet: '834px',
		phone: '390px'
	};

	const DEVICES: DeviceFrame[] = ['desktop', 'tablet', 'phone'];
	const DEVICE_ICON: Record<DeviceFrame, IconName> = {
		desktop: 'monitor',
		tablet: 'tablet',
		phone: 'smartphone'
	};
	const TABS: { id: StageTab; label: string }[] = [
		{ id: 'preview', label: 'Preview' },
		{ id: 'code', label: 'Code' },
		{ id: 'diff', label: 'Diff' },
		{ id: 'visual', label: 'Visual' },
		{ id: 'tests', label: 'Tests' }
	];
	$: visibleTabs = TABS.filter((tab) => tab.id !== 'preview' || previewAvailable);

	function onTabKeydown(event: KeyboardEvent): void {
		const ids = visibleTabs.map((t) => t.id);
		const current = ids.indexOf(stageTab);
		let next = current;
		if (event.key === 'ArrowRight' || event.key === 'ArrowDown') next = (current + 1) % ids.length;
		else if (event.key === 'ArrowLeft' || event.key === 'ArrowUp') next = (current - 1 + ids.length) % ids.length;
		else if (event.key === 'Home') next = 0;
		else if (event.key === 'End') next = ids.length - 1;
		else return;
		event.preventDefault();
		vibeStudioStore.setStageTab(ids[next]);
	}

	// One diff home: clicking a changed file in the Code tab routes to the Diff
	// tab, scrolled to the change set containing that file (StageDiff's
	// scrollToPath hook). Cleared once the user leaves the Diff tab so a later
	// manual visit doesn't re-scroll to a stale file.
	let diffFocusPath: string | null = null;
	function openFileInDiff(path: string): void {
		diffFocusPath = path;
		vibeStudioStore.setStageTab('diff');
	}
	$: if (focusDiffPath) diffFocusPath = focusDiffPath;
	$: if (stageTab !== 'diff' && diffFocusPath) diffFocusPath = null;
	$: changedTotals = changedFiles.reduce(
		(acc, f) => ({ additions: acc.additions + f.additions, deletions: acc.deletions + f.deletions }),
		{ additions: 0, deletions: 0 }
	);

	// #5 — full-repo browse in the Code tab (not just the changed-file set).
	let browseMode = false;
	let repoFiles: string[] = [];
	let repoTruncated = false;
	let browseLoadingFiles = false;
	let fileFilter = '';
	let browsePath: string | null = null;
	let browseContent: ProjectFile | null = null;
	let browseLoadingContent = false;
	let lastBrowseProject: string | null = null;

	$: filteredRepoFiles = (
		fileFilter.trim()
			? repoFiles.filter((f) => f.toLowerCase().includes(fileFilter.trim().toLowerCase()))
			: repoFiles
	).slice(0, 500);

	async function loadRepoFiles(): Promise<void> {
		if (!projectId) {
			repoFiles = [];
			repoTruncated = false;
			return;
		}
		browseLoadingFiles = true;
		const res = await fetchProjectFiles(projectId);
		browseLoadingFiles = false;
		repoFiles = res?.files ?? [];
		repoTruncated = res?.truncated ?? false;
	}
	function toggleBrowse(): void {
		browseMode = !browseMode;
		if (browseMode && repoFiles.length === 0) void loadRepoFiles();
	}
	async function openRepoFile(path: string): Promise<void> {
		if (!projectId) return;
		browsePath = path;
		browseContent = null;
		browseLoadingContent = true;
		const file = await fetchProjectFile(projectId, path);
		if (browsePath === path) {
			browseContent = file;
			browseLoadingContent = false;
		}
	}
	function fmtBytes(n?: number): string {
		if (n == null) return '';
		if (n < 1024) return `${n} B`;
		if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
		return `${(n / (1024 * 1024)).toFixed(1)} MB`;
	}
	// Reset the browse cache when the run/project switches so stale files never show.
	$: if (projectId !== lastBrowseProject) {
		lastBrowseProject = projectId;
		repoFiles = [];
		repoTruncated = false;
		browsePath = null;
		browseContent = null;
		if (browseMode) void loadRepoFiles();
	}

	// #25 — visual-correction screenshot history (before/after card).
	let shots: Screenshot[] = [];
	let shotsLoading = false;
	let shotsLoadedFor = '';
	$: if (stageTab === 'visual' && projectId && shotsLoadedFor !== projectId) {
		shotsLoadedFor = projectId;
		void loadShots();
	}
	async function loadShots(): Promise<void> {
		if (!projectId) {
			shots = [];
			return;
		}
		shotsLoading = true;
		shots = await fetchProjectScreenshots(projectId);
		shotsLoading = false;
	}
	function fmtShotTime(ms: number): string {
		try {
			return new Date(ms).toLocaleString([], {
				month: 'short',
				day: 'numeric',
				hour: '2-digit',
				minute: '2-digit'
			});
		} catch {
			return '';
		}
	}

	function fmtCost(c: number | null): string {
		if (c == null) return '—';
		return c < 1 ? `$${c.toFixed(3)}` : `$${c.toFixed(2)}`;
	}
	function fmtTokens(t: number | null): string {
		if (t == null) return '';
		if (t >= 1000) return `${(t / 1000).toFixed(1)}k tok`;
		return `${t} tok`;
	}

	let deployError = '';
	let deployErrorProjectId: string | null = null;
	let deployingProjectId: string | null = null;
	let selectedDeployTargetId = '';
	$: latestDeployment = latestProjectDeployment(activeProject);
	$: publishedUrl = activeProject?.published_url ?? latestDeployment?.public_url ?? null;
	$: deployTargets = activeProject?.deploy_targets ?? [];
	$: defaultDeployTarget =
		deployTargets.find((target) => target.is_default) ?? deployTargets[0] ?? null;
	$: if (!projectId || deployTargets.length === 0) {
		selectedDeployTargetId = '';
	} else if (!deployTargets.some((target) => target.id === selectedDeployTargetId)) {
		selectedDeployTargetId = defaultDeployTarget?.id ?? '';
	}
	$: selectedDeployTarget =
		deployTargets.find((target) => target.id === selectedDeployTargetId) ??
		defaultDeployTarget ??
		null;
	$: deploying = Boolean(projectId && deployingProjectId === projectId);
	$: visibleDeployError = deployErrorProjectId === projectId ? deployError : '';

	function latestProjectDeployment(project: VibeDevProject | null): VibeDevDeployment | null {
		const deployments = project?.deployments ?? [];
		return deployments.length > 0 ? deployments[0] : null;
	}

	function fmtArtifactBytes(n: number | null | undefined): string {
		if (n == null || Number.isNaN(n)) return '';
		if (n < 1024) return `${n} B`;
		if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
		return `${(n / (1024 * 1024)).toFixed(1)} MB`;
	}

	function deploymentChipLabel(deployment: VibeDevDeployment | null): string {
		if (!deployment) return 'Not published';
		const target = deployment.target_label;
		if (deployment.status === 'succeeded') return `Published · ${target}`;
		if (deployment.status === 'running') return `Publishing · ${target}`;
		if (deployment.status === 'failed') return `Publish failed · ${target}`;
		return `${deployment.status} · ${target}`;
	}

	function deploymentChipTitle(deployment: VibeDevDeployment | null): string {
		if (!deployment) return 'No deployment has been recorded for this project.';
		const parts = [
			deployment.public_url,
			deployment.output_dir ? `Output: ${deployment.output_dir}` : '',
			deployment.artifact_files != null ? `${deployment.artifact_files} files` : '',
			deployment.artifact_bytes != null ? fmtArtifactBytes(deployment.artifact_bytes) : '',
			deployment.error
		].filter(Boolean);
		return parts.join('\n');
	}

	function deployTargetTitle(target: VibeDevDeployTarget | null): string {
		if (!target) return 'No deploy target is configured.';
		return `${target.label}\nProvider: ${target.provider}\nKind: ${target.kind}`;
	}

	async function deployActiveProject(): Promise<void> {
		const targetProjectId = projectId;
		if (!targetProjectId || deployingProjectId === targetProjectId) return;
		deployingProjectId = targetProjectId;
		deployError = '';
		deployErrorProjectId = null;
		const result = await vibeDevProjectStore.deployProject(
			targetProjectId,
			selectedDeployTarget?.id ?? null
		);
		if (deployingProjectId === targetProjectId) {
			deployingProjectId = null;
		}
		if (!result) {
			deployErrorProjectId = targetProjectId;
			deployError = get(vibeDevProjectStore).error || 'Could not publish site.';
			return;
		}
		if (result.deployment.status === 'failed') {
			deployErrorProjectId = targetProjectId;
			deployError = result.deployment.error || 'Publish failed.';
		}
	}
</script>

<section class="stage" aria-label="Build stage">
	{#if firstRun}
		<!-- First-run narrative: no run exists yet, so run-specific chrome is
		     dead weight. The persistent CLI Agent entry point remains below. -->
		<div class="stage__firstrun">
			<div class="stage__firstrun-card">
				<span class="stage__firstrun-icon" aria-hidden="true"><Icon name="monitor" size={22} /></span>
				<h2 class="stage__firstrun-title">Your build will appear here</h2>
				<p class="stage__firstrun-sub">Preview, code, diffs, and tests — live, as it's built.</p>
				<div class="stage__firstrun-chips">
					{#each FIRST_RUN_PROMPTS as example (example)}
						<button
							type="button"
							class="stage__firstrun-chip"
							on:click={() => dispatch('seedPrompt', { text: example })}
						>{example}</button>
					{/each}
				</div>
			</div>
		</div>
	{:else}
	<div class="stage__tabs" role="tablist" aria-label="Stage view">
		{#each visibleTabs as tab (tab.id)}
			<button
				type="button"
				role="tab"
				id={`stage-tab-${tab.id}`}
				aria-selected={stageTab === tab.id}
				aria-controls="stage-panel"
				tabindex={stageTab === tab.id ? 0 : -1}
				class="stage__tab"
				class:active={stageTab === tab.id}
				on:keydown={onTabKeydown}
				on:click={() => vibeStudioStore.setStageTab(tab.id)}
			>
				{tab.label}
				{#if tab.id === 'diff' && orderedDiffRows.length > 0}
					<span class="stage__tab-badge">{orderedDiffRows.length}</span>
				{/if}
			</button>
		{/each}
		<div class="stage__tabs-spacer"></div>
		<!-- Build / Discuss / Autopilot moved to the composer (next to send) so the
		     submit intent is obvious; the Stage keeps the visual-self-correct toggle. -->
		{#if studio.mode !== 'discuss'}
			<div
				class="vsc-toggle"
				title="Auto: on a vision-capable profile, the agent screenshots the preview, critiques it as an image, and patches what looks wrong — but ONLY for visual changes (it skips config/backend/tests), max 3 passes. Off: never."
			>
				<Icon name="eye" size={13} />
				<Checkbox
					label="Visual self-correct"
					checked={studio.visualSelfCorrect}
					on:change={(event) => vibeStudioStore.setVisualSelfCorrect(event.detail.checked)}
				/>
			</div>
		{/if}
	</div>

	<div class="stage__body" id="stage-panel" role="tabpanel" aria-labelledby={`stage-tab-${stageTab}`} tabindex="0">
		{#if stageTab === 'preview' && previewAvailable}
			<div class="stage__preview-toolbar">
				<button
					type="button"
					class="edit-toggle"
					class:active={editMode}
					aria-pressed={editMode}
					title="Click an element to edit it"
					on:click={() => dispatch('toggleEdit')}
				><Icon name="pencil" size={12} /> {editMode ? 'Editing' : 'Edit'}</button>
				<span class="stage__toolbar-spacer"></span>
				<div class="device-toggle" role="group" aria-label="Preview device">
					{#each DEVICES as device}
						<button
							type="button"
							class:active={deviceFrame === device}
							on:click={() => vibeStudioStore.setDeviceFrame(device)}
							aria-label={device}
						><Icon name={DEVICE_ICON[device]} size={13} /></button>
					{/each}
				</div>
			</div>
			<div class="stage__device-wrap">
				<div class="stage__device" style={`max-width:${DEVICE_WIDTH[deviceFrame]}`}>
					<VibePreviewPanel {projectId} {pinnedUrl} />
				</div>
			</div>
		{:else if stageTab === 'code'}
			<div class="code-tab">
				<aside class="code-tab__tree" aria-label={browseMode ? 'Repo files' : 'Changed files'}>
					<div class="code-tab__tree-head">
						<button
							type="button"
							class="code-tab__browse-toggle"
							class:active={browseMode}
							disabled={!projectId}
							title={browseMode ? 'Back to changed files' : 'Browse the whole repo'}
							on:click={toggleBrowse}
						>
							{browseMode ? '← Changes' : 'Browse repo'}
						</button>
					</div>
					{#if browseMode}
						<input
							class="code-tab__filter"
							type="text"
							placeholder="Filter files…"
							bind:value={fileFilter}
							aria-label="Filter repo files"
						/>
						{#if browseLoadingFiles}
							<p class="code-tab__empty">Loading…</p>
						{:else if filteredRepoFiles.length === 0}
							<p class="code-tab__empty">{repoFiles.length === 0 ? 'No files.' : 'No match.'}</p>
						{:else}
							{#each filteredRepoFiles as f (f)}
								<button
									type="button"
									class="code-tab__file"
									class:active={browsePath === f}
									on:click={() => void openRepoFile(f)}
								>
									<span class="code-tab__path">{f}</span>
								</button>
							{/each}
							{#if repoTruncated}
								<p class="code-tab__empty">…list truncated</p>
							{/if}
						{/if}
					{:else if changedFiles.length === 0}
						<p class="code-tab__empty">No changed files yet.</p>
					{:else}
						{#each changedFiles as file (file.path)}
							<button
								type="button"
								class="code-tab__file"
								title="Review this file in the Diff tab"
								aria-label={`Review ${file.path} in the Diff tab`}
								on:click={() => openFileInDiff(file.path)}
							>
								<span class="code-tab__path">{file.path}</span>
								<span class="code-tab__stat">+{file.additions} −{file.deletions}</span>
							</button>
						{/each}
					{/if}
				</aside>
				<div class="code-tab__view">
					{#if browseMode}
						{#if browseLoadingContent}
							<p class="code-tab__empty">Loading…</p>
						{:else if browseContent?.binary}
							<p class="code-tab__empty">{browsePath} — binary file ({fmtBytes(browseContent.size)}), not shown.</p>
						{:else if browseContent?.too_large}
							<p class="code-tab__empty">{browsePath} — too large to preview ({fmtBytes(browseContent.size)}).</p>
						{:else if browseContent && browseContent.content != null}
							<div class="code-tab__file-head">{browsePath}</div>
							<pre class="code-tab__content">{browseContent.content}</pre>
						{:else if browsePath}
							<p class="code-tab__empty">Could not read {browsePath}.</p>
						{:else}
							<p class="code-tab__empty">Select a file to view it.</p>
						{/if}
					{:else if changedFiles.length > 0}
						<!-- State, not review: the Code tab reports WHAT changed; the
						     Diff tab is the one place to inspect and apply it. -->
						<div class="code-tab__state">
							<p class="code-tab__state-line">
								{changedFiles.length} file{changedFiles.length === 1 ? '' : 's'} changed ·
								+{changedTotals.additions} −{changedTotals.deletions}
							</p>
							<p class="code-tab__state-hint">
								Click a file on the left, or open the Diff tab to review and apply the changes.
							</p>
							<button
								type="button"
								class="vbtn vbtn--primary"
								aria-label="Open the Diff tab to review changes"
								on:click={() => vibeStudioStore.setStageTab('diff')}
							>Review in Diff tab →</button>
						</div>
					{:else}
						<p class="code-tab__empty">No changes staged yet — use Browse repo to read any file.</p>
					{/if}
				</div>
			</div>
		{:else if stageTab === 'diff'}
			<StageDiff
				{orderedDiffRows}
				{orderedOtherRows}
				autoApplyCodeProposals={studio.autoApplyCodeProposals}
				{bulkApplying}
				{actingKeys}
				scrollToPath={diffFocusPath}
				on:approveAll
				on:toggleAutoApply
				on:applyDiff
				on:rejectDiff
				on:applyFile
				on:rejectFile
				on:respond
			/>
		{:else if stageTab === 'visual'}
			<div class="visual-tab">
				<div class="visual-tab__bar">
					<span class="visual-tab__title">Before / after</span>
					<button
						type="button"
						class="vbtn"
						disabled={!projectId || shotsLoading}
						on:click={() => void loadShots()}>↻ Refresh</button
					>
				</div>
				{#if shotsLoading && shots.length === 0}
					<p class="code-tab__empty">Loading…</p>
				{:else if shots.length === 0}
					<p class="code-tab__empty">
						No screenshots yet — the visual self-correction loop captures them during a run.
					</p>
				{:else}
					<div class="visual-tab__pair">
						{#if shots[1]}
							<figure class="visual-shot">
								<img
									src={scopedImageUrl(shots[1].outputs_path)}
									alt={shots[1].label ?? 'previous screenshot'}
									loading="lazy"
								/>
								<figcaption>Before · {fmtShotTime(shots[1].created_at_ms)}</figcaption>
							</figure>
						{/if}
						<figure class="visual-shot">
							<img
								src={scopedImageUrl(shots[0].outputs_path)}
								alt={shots[0].label ?? 'latest screenshot'}
								loading="lazy"
							/>
							<figcaption>{shots[1] ? 'After' : 'Latest'} · {fmtShotTime(shots[0].created_at_ms)}</figcaption>
						</figure>
					</div>
					{#if shots.length > 2}
						<div class="visual-tab__strip">
							{#each shots.slice(2) as shot (shot.id)}
								<img
									class="visual-thumb"
									src={scopedImageUrl(shot.outputs_path)}
									alt={shot.label ?? 'screenshot'}
									title={fmtShotTime(shot.created_at_ms)}
									loading="lazy"
								/>
							{/each}
						</div>
					{/if}
				{/if}
			</div>
		{:else if stageTab === 'tests'}
			<div class="tests-tab">
				<div class="checks-bar">
					{#if checks.length > 0}
						<button type="button" class="vbtn vbtn--primary" disabled={Boolean(runningKind) || !projectId} on:click={() => void runChecks('all')}>
							{runningKind === 'all' ? 'Running…' : 'Run checks'}
						</button>
						{#each checks as check (check.kind)}
							<button type="button" class="vbtn" disabled={Boolean(runningKind) || !projectId} on:click={() => void runChecks(check.kind)} title={check.display}>
								{runningKind === check.kind ? '…' : check.kind}
							</button>
						{/each}
					{:else}
						<span class="checks-bar__hint">No checks detected for this project.</span>
					{/if}
				</div>

				{#if checkError}<p class="checks-error">{checkError}</p>{/if}

				{#each checkResults as result (result.kind)}
					<article class="check-card" class:is-failed={!result.ok} class:is-done={result.ok}>
						<header class="check-card__head">
							<span class="check-card__glyph">{result.ok ? '✓' : '✗'}</span>
							<span class="check-card__title">{result.kind}</span>
							<code class="check-card__cmd">{result.command}</code>
							{#if !result.ok}
								<button type="button" class="vbtn vbtn--primary check-card__fix" on:click={() => dispatch('attemptFix', { result })}>Attempt fix</button>
							{/if}
						</header>
						{#if result.output_tail && !result.ok}
							<pre class="check-card__out">{result.output_tail}</pre>
						{/if}
					</article>
				{/each}

				{#if testCards.length > 0}
					<h3 class="tests-tab__section">Live test events</h3>
					{#each testCards as card (card.id)}
						<article class="test-card is-{card.status}">
							<header>
								<span class="test-card__glyph">{card.status === 'failed' ? '✗' : card.status === 'done' ? '✓' : '…'}</span>
								<span class="test-card__title">{card.title}</span>
							</header>
							{#if card.result}<pre>{card.result}</pre>{/if}
						</article>
					{/each}
				{/if}

				{#if checkResults.length === 0 && testCards.length === 0 && checks.length > 0}
					<p class="code-tab__empty">Run a check to verify the build, or let the coding agent run tests as it works.</p>
				{/if}
			</div>
		{/if}
	</div>
	{/if}

	<div class="stage__statusbar" class:stage__statusbar--first-run={firstRun}>
		<div class="chips" aria-label="Run telemetry" aria-live="polite">
			{#if !firstRun}
			{#if meta && studio.costBudgetUsd != null}
				{@const spent = meta?.costTotal ?? 0}
				{@const budget = studio.costBudgetUsd}
				{@const pct = budget > 0 ? Math.min(100, (spent / budget) * 100) : 0}
				<span
					class="chip chip--budget"
					class:chip--warn={pct >= 80 && pct < 100}
					class:chip--over={pct >= 100}
					title={`Run cost vs budget (${Math.round(pct)}%)`}
				>
					{fmtCost(spent)} / {fmtCost(budget)}
					<span class="budget-bar"><span class="budget-bar__fill" style={`width:${pct}%`}></span></span>
				</span>
			{:else if meta}
				<span class="chip chip--cost" title="Run cost">{fmtCost(meta?.costTotal ?? null)}</span>
			{/if}
			{#if meta?.contextPercent != null}
				<span class="chip" title="Context window used">{Math.round(meta.contextPercent)}% ctx</span>
			{/if}
			{#if meta?.tokens != null}<span class="chip">{fmtTokens(meta.tokens)}</span>{/if}
			{#if meta?.model}<span class="chip">{meta.model}</span>{/if}
			{#if meta?.compacting}<span class="chip chip--warn">compacting…</span>{/if}
			{#if meta?.retry?.active}
				<span class="chip chip--warn">retrying {meta.retry.attempt}/{meta.retry.max || '?'}</span>
			{/if}
			{#if meta?.queue && meta.queue.steering + meta.queue.followUp > 0}
				<span class="chip">{meta.queue.steering + meta.queue.followUp} queued</span>
			{/if}
			{/if}
		</div>
		<div class="stage__ship">
			<button
				type="button"
				class="vbtn"
				class:active={studio.terminalOpen}
				aria-pressed={studio.terminalOpen}
				on:click={() => vibeStudioStore.toggleTerminal()}
				title="Open the CLI Agent full-screen"
			>⌥ CLI Agent</button>
			{#if !firstRun}
			<!-- Status-bar shortcut for the Diff tab's Apply-all — ONE action, ONE
			     name, ONE handler path (`approveAll`), same disabled logic. -->
			<button
				type="button"
				class="vbtn vbtn--primary"
				disabled={orderedDiffRows.length === 0 || bulkApplying}
				title="Apply every staged change set to the repo (same as the Diff tab's Apply all changes)"
				on:click={() => dispatch('approveAll')}
			>{bulkApplying ? 'Applying…' : 'Apply all changes'}</button>
			<span
				class="chip"
				class:chip--warn={latestDeployment?.status === 'failed' || Boolean(visibleDeployError)}
				title={visibleDeployError || deploymentChipTitle(latestDeployment)}
			>{visibleDeployError || deploymentChipLabel(latestDeployment)}</span>
			{#if publishedUrl}
				<a class="vbtn" href={publishedUrl} target="_blank" rel="noreferrer">Open live ↗</a>
			{/if}
			{#if deployTargets.length > 0}
				<label class="deploy-target" title={deployTargetTitle(selectedDeployTarget)}>
					<span class="deploy-target__label">Target</span>
					<select
						class="deploy-target__select"
						bind:value={selectedDeployTargetId}
						disabled={!projectId || deploying}
						aria-label="Deploy target"
					>
						{#each deployTargets as target (target.id)}
							<option value={target.id}>{target.label}</option>
						{/each}
					</select>
				</label>
			{:else}
				<span class="chip chip--warn" title="No deploy target is enabled in magician-config.yaml">No deploy target</span>
			{/if}
			<button
				type="button"
				class="vbtn"
				disabled={!projectId || deploying || !selectedDeployTarget}
				title="Build the static site and publish it through the selected VibeDev deploy target."
				on:click={() => void deployActiveProject()}
			>{deploying ? 'Publishing…' : 'Publish ↗'}</button>
			{/if}
		</div>
	</div>
</section>

<style>
	.stage {
		display: flex;
		flex-direction: column;
		min-height: 0;
		height: 100%;
		background: var(--vibe-surface);
		border-left: 1px solid var(--vibe-border);
	}
	/* First-run narrative card — the whole column, centered. */
	.stage__firstrun {
		flex: 1;
		min-height: 0;
		display: grid;
		place-items: center;
		padding: 1.5rem;
	}
	.stage__firstrun-card {
		max-width: 22rem;
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 0.55rem;
		text-align: center;
	}
	.stage__firstrun-icon {
		display: inline-grid;
		place-items: center;
		width: 3rem;
		height: 3rem;
		border-radius: 999px;
		border: 1px solid var(--vibe-border);
		background: color-mix(in srgb, var(--vibe-accent) 7%, var(--vibe-surface));
		color: var(--vibe-accent);
	}
	.stage__firstrun-title {
		margin: 0.2rem 0 0;
		font-family: var(--font-display, inherit);
		font-size: 1rem;
		font-weight: 650;
		color: var(--vibe-text);
	}
	.stage__firstrun-sub {
		margin: 0;
		font-size: 0.84rem;
		line-height: 1.5;
		color: var(--vibe-text-muted);
	}
	.stage__firstrun-chips {
		display: flex;
		flex-wrap: wrap;
		justify-content: center;
		gap: 0.4rem;
		margin-top: 0.5rem;
	}
	.stage__firstrun-chip {
		border: 1px solid var(--vibe-border);
		border-radius: var(--radius-full, 999px);
		background: var(--vibe-surface);
		color: var(--vibe-text-muted);
		font: inherit;
		font-size: 0.78rem;
		font-weight: 600;
		padding: 0.3rem 0.8rem;
		cursor: pointer;
	}
	.stage__firstrun-chip:hover {
		color: var(--vibe-accent);
		border-color: var(--vibe-accent);
	}

	.stage__tabs {
		display: flex;
		align-items: center;
		gap: 0.25rem;
		padding: 0.4rem 0.6rem;
		border-bottom: 1px solid var(--vibe-border);
	}
	.stage__tab {
		display: inline-flex;
		align-items: center;
		gap: 0.35rem;
		border: 0;
		border-radius: var(--radius-sm, 10px);
		background: transparent;
		color: var(--vibe-text-muted);
		font: inherit;
		font-size: 0.82rem;
		font-weight: 600;
		padding: 0.35rem 0.7rem;
		cursor: pointer;
	}
	.stage__tab:hover {
		color: var(--vibe-text);
		background: color-mix(in srgb, var(--vibe-page-surface) 60%, transparent);
	}
	.stage__tab.active {
		color: var(--vibe-accent);
		background: color-mix(in srgb, var(--vibe-accent) 10%, transparent);
	}
	.stage__tab-badge {
		display: inline-grid;
		place-items: center;
		min-width: 1.05rem;
		height: 1.05rem;
		border-radius: 999px;
		background: var(--vibe-accent);
		color: var(--button-primary-color, #fff);
		font-size: var(--text-2xs);
		font-weight: 700;
	}
	.stage__tabs-spacer {
		flex: 1;
	}

	.device-toggle {
		display: inline-flex;
		border: 1px solid var(--vibe-border);
		border-radius: var(--radius-full, 999px);
		padding: 0.12rem;
		gap: 0.1rem;
	}
	.device-toggle button {
		display: inline-grid;
		place-items: center;
		border: 0;
		border-radius: var(--radius-full, 999px);
		background: transparent;
		color: var(--vibe-text-muted);
		font: inherit;
		font-size: 0.74rem;
		font-weight: 600;
		padding: 0.2rem 0.5rem;
		cursor: pointer;
	}
	.device-toggle button.active {
		background: var(--vibe-accent);
		color: var(--button-primary-color, #fff);
	}

	.vsc-toggle {
		display: inline-flex;
		align-items: center;
		gap: 0.35rem;
		margin-left: 0.5rem;
		color: var(--vibe-text-muted);
		font-size: 0.74rem;
		font-weight: 600;
		white-space: nowrap;
	}

	:global(.vsc-toggle .muij-checkbox) {
		font-size: inherit;
		font-weight: inherit;
		color: inherit;
	}

	.stage__body {
		flex: 1;
		min-height: 0;
		display: flex;
		flex-direction: column;
		overflow: hidden;
	}

	.stage__preview-toolbar {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		padding: 0.45rem 0.6rem 0;
	}
	.stage__toolbar-spacer {
		flex: 1;
	}
	.edit-toggle {
		display: inline-flex;
		align-items: center;
		gap: 0.3rem;
		border: 1px solid var(--vibe-border);
		border-radius: var(--radius-full, 999px);
		background: var(--vibe-surface);
		color: var(--vibe-text-muted);
		font: inherit;
		font-size: 0.74rem;
		font-weight: 600;
		padding: 0.2rem 0.7rem;
		cursor: pointer;
	}
	.edit-toggle:hover {
		color: var(--vibe-text);
		border-color: var(--vibe-accent);
	}
	.edit-toggle.active {
		background: var(--vibe-accent);
		border-color: transparent;
		color: var(--button-primary-color, #fff);
	}
	.stage__device-wrap {
		flex: 1;
		min-height: 0;
		overflow: auto;
		display: flex;
		justify-content: center;
		padding: 0.6rem;
	}
	.stage__device {
		width: 100%;
		transition: max-width 0.28s var(--ease-settle, ease);
	}

	.code-tab {
		flex: 1;
		min-height: 0;
		display: grid;
		grid-template-columns: minmax(11rem, 16rem) 1fr;
	}
	.code-tab__tree {
		border-right: 1px solid var(--vibe-border);
		overflow: auto;
		padding: 0.5rem;
		display: flex;
		flex-direction: column;
		gap: 0.15rem;
	}
	.code-tab__tree-head {
		display: flex;
		margin-bottom: 0.2rem;
	}
	.code-tab__browse-toggle {
		flex: 1;
		border: 1px solid var(--vibe-border);
		border-radius: var(--radius-sm, 10px);
		background: transparent;
		color: var(--vibe-text-muted);
		font: inherit;
		font-size: 0.72rem;
		padding: 0.28rem 0.45rem;
		cursor: pointer;
		text-align: center;
	}
	.code-tab__browse-toggle:hover:not(:disabled) {
		color: var(--vibe-text);
		border-color: var(--vibe-accent);
	}
	.code-tab__browse-toggle.active {
		color: var(--vibe-accent);
		border-color: color-mix(in srgb, var(--vibe-accent) 55%, transparent);
		background: color-mix(in srgb, var(--vibe-accent) 8%, transparent);
	}
	.code-tab__browse-toggle:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}
	.code-tab__filter {
		border: 1px solid var(--vibe-border);
		border-radius: var(--radius-sm, 10px);
		background: var(--vibe-surface);
		color: var(--vibe-text);
		font: inherit;
		font-size: 0.74rem;
		padding: 0.28rem 0.45rem;
		margin-bottom: 0.2rem;
	}
	.code-tab__file-head {
		font-family: var(--font-mono, monospace);
		font-size: 0.74rem;
		color: var(--vibe-text-muted);
		padding-bottom: 0.4rem;
		margin-bottom: 0.4rem;
		border-bottom: 1px solid var(--vibe-border);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.code-tab__content {
		margin: 0;
		font-family: var(--font-mono, monospace);
		font-size: 0.74rem;
		line-height: 1.5;
		white-space: pre;
		overflow: auto;
		color: var(--vibe-text);
		tab-size: 4;
	}
	.visual-tab {
		flex: 1;
		min-height: 0;
		display: flex;
		flex-direction: column;
		gap: 0.6rem;
		padding: 0.6rem;
		overflow: auto;
	}
	.visual-tab__bar {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.5rem;
	}
	.visual-tab__title {
		font-size: 0.82rem;
		font-weight: 600;
		color: var(--vibe-text);
	}
	.visual-tab__pair {
		display: grid;
		grid-template-columns: 1fr 1fr;
		gap: 0.6rem;
	}
	.visual-shot {
		margin: 0;
		display: flex;
		flex-direction: column;
		gap: 0.3rem;
		min-width: 0;
	}
	.visual-shot img {
		width: 100%;
		height: auto;
		border: 1px solid var(--vibe-border);
		border-radius: var(--radius-sm, 10px);
		background: var(--vibe-surface);
	}
	.visual-shot figcaption {
		font-size: var(--text-2xs);
		color: var(--vibe-text-muted);
	}
	.visual-tab__strip {
		display: flex;
		flex-wrap: wrap;
		gap: 0.4rem;
		border-top: 1px solid var(--vibe-border);
		padding-top: 0.5rem;
	}
	.visual-thumb {
		width: 7rem;
		height: auto;
		border: 1px solid var(--vibe-border);
		border-radius: var(--radius-sm, 8px);
	}
	.code-tab__file {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.5rem;
		border: 0;
		border-radius: var(--radius-sm, 10px);
		background: transparent;
		color: var(--vibe-text);
		font: inherit;
		font-size: 0.76rem;
		padding: 0.32rem 0.45rem;
		cursor: pointer;
		text-align: left;
	}
	.code-tab__file:hover {
		background: color-mix(in srgb, var(--vibe-page-surface) 60%, transparent);
	}
	.code-tab__file.active {
		background: color-mix(in srgb, var(--vibe-accent) 12%, transparent);
		color: var(--vibe-accent);
	}
	.code-tab__path {
		font-family: var(--font-mono, monospace);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.code-tab__stat {
		font-family: var(--font-mono, monospace);
		font-size: var(--text-2xs);
		color: var(--vibe-text-muted);
		flex-shrink: 0;
	}
	.code-tab__view {
		min-width: 0;
		overflow: auto;
		padding: 0.6rem;
	}
	.code-tab__empty {
		margin: auto;
		color: var(--vibe-text-muted);
		font-size: 0.86rem;
		text-align: center;
		padding: 2rem 1rem;
	}
	/* Changed-files STATE summary (review lives in the Diff tab). */
	.code-tab__state {
		margin: auto;
		max-width: 22rem;
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 0.55rem;
		text-align: center;
		padding: 2rem 1rem;
	}
	.code-tab__state-line {
		margin: 0;
		font-family: var(--font-mono, monospace);
		font-size: 0.82rem;
		font-weight: 600;
		color: var(--vibe-text);
	}
	.code-tab__state-hint {
		margin: 0;
		font-size: 0.8rem;
		line-height: 1.45;
		color: var(--vibe-text-muted);
	}

	.tests-tab {
		flex: 1;
		min-height: 0;
		overflow: auto;
		padding: 0.7rem;
		display: flex;
		flex-direction: column;
		gap: 0.6rem;
	}
	.test-card {
		border: 1px solid var(--vibe-border);
		border-radius: var(--radius-md, 18px);
		padding: 0.7rem 0.8rem;
		background: var(--vibe-surface);
	}
	.test-card.is-failed {
		border-color: color-mix(in srgb, var(--vibe-error) 45%, var(--vibe-border));
		background: color-mix(in srgb, var(--vibe-error) 6%, var(--vibe-surface));
	}
	.test-card.is-done {
		border-color: color-mix(in srgb, var(--vibe-success) 45%, var(--vibe-border));
		background: color-mix(in srgb, var(--vibe-success) 6%, var(--vibe-surface));
	}
	.test-card header {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		font-weight: 600;
		font-size: 0.86rem;
	}
	.test-card pre {
		margin: 0.5rem 0 0;
		max-height: 16rem;
		overflow: auto;
		font-family: var(--font-mono, monospace);
		font-size: 0.74rem;
		white-space: pre-wrap;
		overflow-wrap: anywhere;
		color: var(--vibe-text-muted);
	}

	.checks-bar {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.4rem;
	}
	.checks-bar__hint {
		font-size: 0.82rem;
		color: var(--vibe-text-muted);
	}
	.checks-error {
		margin: 0;
		font-size: 0.78rem;
		color: var(--vibe-warning);
	}
	.tests-tab__section {
		margin: 0.4rem 0 0;
		font-family: var(--font-display, inherit);
		font-size: 0.82rem;
		font-weight: 600;
		color: var(--vibe-text-muted);
	}
	.check-card {
		border: 1px solid var(--vibe-border);
		border-radius: var(--radius-md, 18px);
		padding: 0.6rem 0.75rem;
		background: var(--vibe-surface);
	}
	.check-card.is-failed {
		border-color: color-mix(in srgb, var(--vibe-error) 45%, var(--vibe-border));
		background: color-mix(in srgb, var(--vibe-error) 6%, var(--vibe-surface));
	}
	.check-card.is-done {
		border-color: color-mix(in srgb, var(--vibe-success) 45%, var(--vibe-border));
		background: color-mix(in srgb, var(--vibe-success) 6%, var(--vibe-surface));
	}
	.check-card__head {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		flex-wrap: wrap;
	}
	.check-card__glyph {
		font-weight: 700;
	}
	.is-done .check-card__glyph {
		color: var(--vibe-success);
	}
	.is-failed .check-card__glyph {
		color: var(--vibe-error);
	}
	.check-card__title {
		font-weight: 600;
		font-size: 0.84rem;
		text-transform: capitalize;
	}
	.check-card__cmd {
		font-family: var(--font-mono, monospace);
		font-size: var(--text-2xs);
		color: var(--vibe-text-muted);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		max-width: 16rem;
	}
	.check-card__fix {
		margin-left: auto;
	}
	.check-card__out {
		margin: 0.5rem 0 0;
		max-height: 18rem;
		overflow: auto;
		font-family: var(--font-mono, monospace);
		font-size: 0.72rem;
		white-space: pre-wrap;
		overflow-wrap: anywhere;
		color: var(--vibe-text-muted);
	}

	.stage__statusbar {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		padding: 0.5rem 0.7rem;
		border-top: 1px solid var(--vibe-border);
		background: color-mix(in srgb, var(--vibe-page-surface) 40%, var(--vibe-surface));
	}
	.stage__statusbar--first-run {
		justify-content: flex-end;
	}
	.chips {
		display: flex;
		flex-wrap: wrap;
		gap: 0.35rem;
		flex: 1;
		min-width: 0;
	}
	.chip {
		font-family: var(--font-mono, monospace);
		font-size: var(--text-2xs);
		font-weight: 600;
		color: var(--vibe-text-muted);
		border: 1px solid var(--vibe-border);
		border-radius: var(--radius-full, 999px);
		padding: 0.1rem 0.5rem;
	}
	.chip--cost {
		color: var(--vibe-text);
	}
	.chip--warn {
		color: var(--vibe-warning);
		border-color: color-mix(in srgb, var(--vibe-warning) 40%, transparent);
	}
	.chip--over {
		color: var(--vibe-error);
		border-color: color-mix(in srgb, var(--vibe-error) 45%, transparent);
	}
	.chip--budget {
		display: inline-flex;
		align-items: center;
		gap: 0.4rem;
		color: var(--vibe-text);
	}
	.budget-bar {
		width: 3rem;
		height: 0.3rem;
		border-radius: 999px;
		background: color-mix(in srgb, var(--vibe-text-muted) 22%, transparent);
		overflow: hidden;
	}
	.budget-bar__fill {
		display: block;
		height: 100%;
		border-radius: 999px;
		background: var(--vibe-success);
	}
	.chip--warn .budget-bar__fill {
		background: var(--vibe-warning);
	}
	.chip--over .budget-bar__fill {
		background: var(--vibe-error);
	}
	.stage__ship {
		display: inline-flex;
		align-items: center;
		justify-content: flex-end;
		flex-wrap: wrap;
		gap: 0.45rem;
		flex-shrink: 0;
	}
	.deploy-target {
		min-height: 2rem;
		display: inline-flex;
		align-items: center;
		gap: 0.35rem;
		border: 1px solid var(--vibe-border-strong);
		border-radius: var(--radius-sm, 10px);
		background: var(--vibe-surface);
		padding: 0 0.45rem;
	}
	.deploy-target__label {
		font-size: 0.68rem;
		font-weight: 700;
		color: var(--vibe-text-muted);
		text-transform: uppercase;
	}
	.deploy-target__select {
		max-width: 10rem;
		border: 0;
		background: transparent;
		color: var(--vibe-text);
		font: inherit;
		font-size: 0.76rem;
		font-weight: 650;
		outline: none;
	}
	.deploy-target__select:disabled {
		opacity: 0.55;
	}

	.vbtn {
		min-height: 2rem;
		border: 1px solid var(--vibe-border-strong);
		border-radius: var(--radius-sm, 10px);
		background: var(--vibe-surface);
		color: var(--vibe-text);
		font: inherit;
		font-size: 0.76rem;
		font-weight: 600;
		padding: 0.32rem 0.7rem;
		cursor: pointer;
	}
	.vbtn--primary {
		border-color: color-mix(in srgb, var(--vibe-accent) 72%, transparent);
		background: var(--vibe-accent);
		color: var(--button-primary-color, #fff);
	}
	.vbtn.active {
		border-color: color-mix(in srgb, var(--vibe-accent) 55%, transparent);
		background: color-mix(in srgb, var(--vibe-accent) 12%, transparent);
		color: var(--vibe-accent);
	}
	.vbtn:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}

	@media (max-width: 900px) {
		.code-tab {
			grid-template-columns: 1fr;
		}
		.code-tab__tree {
			max-height: 9rem;
			border-right: 0;
			border-bottom: 1px solid var(--vibe-border);
		}
	}
	@media (prefers-reduced-motion: reduce) {
		.stage__device {
			transition: none;
		}
	}
</style>
