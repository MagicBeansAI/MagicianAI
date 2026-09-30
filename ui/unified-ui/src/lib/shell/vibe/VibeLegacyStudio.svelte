<script lang="ts">
	import { PRODUCT_NAME } from '$lib/presentationIdentity';
	import { derived, get } from 'svelte/store';
	import { browser } from '$app/environment';
	import { goto } from '$app/navigation';
	import { page } from '$app/stores';
	import { onDestroy, onMount } from 'svelte';
	import VibeComposer from '$lib/shell/VibeComposer.svelte';
	import VibeReviewPanel from '$lib/shell/VibeReviewPanel.svelte';
	import WorkbenchColumn from '$lib/shell/WorkbenchColumn.svelte';
	import VibePreviewPanel from '$lib/shell/VibePreviewPanel.svelte';
	import {
		ensurePendingHitlBridge,
		pendingHitlEntries,
		type HitlPendingEntry
	} from '$lib/stores/pendingHitlStore';
	import { hitlRequestFromCanonicalEvent, postHitlResponse } from '$lib/hitl/adapters';
	import { respondToHitl } from '$lib/hitl/respondToHitl';
	import type { HitlRequest, HitlResolveOutcome } from '$lib/hitl/types';
	import { appendCurrentScopeQuery, scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import { codingProfileStore } from '$lib/stores/codingProfileStore';
	import { chatStore } from '$lib/stores/chatStore';
	import type { UploadedAttachment } from '$lib/stores/chatStore';
	import { taskStore, type Task } from '$lib/stores/taskStore';
	import { threadStore } from '$lib/stores/threadStore';
	import { vibeDevProjectStore, type VibeDevProject } from '$lib/stores/vibeDevProjectStore';
	import { startVibeDevRun } from '$lib/shell/vibe/conversation/submit';
	import { timedFetch, LONG_FETCH_TIMEOUT_MS } from '$lib/shared/fetch';
	import { createBackoff } from '$lib/realtime/backoff';
	import { showError, showSuccess } from '$lib/shared/stores/notifications';
	import { requestConfirmation } from '$lib/stores/confirmationStore';

	type VibeRow = {
		key: string;
		entry: HitlPendingEntry;
		request: HitlRequest;
	};

	type CodingActivityStatus = 'running' | 'waiting' | 'done' | 'failed';
	type CodingLogTone = CodingActivityStatus | 'info';
	type VibeDevSurface = 'agents' | 'workbench';
	type VibeComposerMode = 'fresh' | 'follow_up';
	type VibeWorkspaceTab = 'preview' | 'logs' | 'tests';
	type VibeComposerActiveRun = {
		label: string;
		title: string;
		meta: string[];
		blocked?: boolean;
	};

	type CodingActivityRow = {
		key: string;
		label: string;
		detail: string | null;
		status: CodingActivityStatus;
		profileLabel: string | null;
		taskId: string | null;
		updatedAt: number;
	};
	type CodingLogRow = {
		key: string;
		sourceKey: string;
		eventType: string;
		label: string;
		detail: string | null;
		tone: CodingLogTone;
		profileLabel: string | null;
		taskId: string | null;
		updatedAt: number;
	};
	type ProjectedCodingLogItem = {
		id: string;
		kind: string;
		source: string;
		event_type?: string | null;
		label: string;
		detail?: string | null;
		timestamp_ms: number;
		task_id?: string | null;
		profile_label?: string | null;
		metadata?: Record<string, unknown> | null;
	};
	type ProjectedCodingLogSources = {
		event_log?: boolean;
		coding_events?: boolean;
		command_summaries?: boolean;
		pty_sessions?: boolean;
		pty_snippets?: boolean;
		dev_server_urls?: boolean;
		test_output?: boolean;
	};
	type ProjectedCodingLogResponse = {
		task_id: string;
		generated_at_ms?: number;
		sources?: ProjectedCodingLogSources;
		items?: ProjectedCodingLogItem[];
	};
	type VibeChangedFile = {
		path: string;
		additions: number;
		deletions: number;
		active: boolean;
	};
	type DirectoryEntry = {
		name: string;
		path: string;
	};
	type DirectoryListResponse = {
		path: string;
		parent?: string | null;
		home?: string | null;
		root?: string;
		current_dir?: string | null;
		entries?: DirectoryEntry[];
		truncated?: boolean;
		error?: string;
	};

	const AUTO_APPLY_STORAGE_KEY = 'magician.vibedev.autoApplyCodeProposals';
	const VIBEDEV_THREAD_ID = 'vibedev';
	const VIBEDEV_THREAD_NAME = 'VibeDev';
	const VIBEDEV_PARENT_TASK_PREFIX = 'Parent task:';
	const VIBEDEV_PROJECT_PREFIX = 'VibeDev project:';
	const CODING_STREAM_CONNECT_TIMEOUT_MS = 12_000;

	const rows = derived(pendingHitlEntries, ($entries) =>
		$entries
			.map((entry): VibeRow | null => {
				const request = entry.raw ? hitlRequestFromCanonicalEvent(entry.raw) : null;
				if (!request) return null;
				return {
					key:
						request.schema.proposal_id ??
						request.schema.transaction_id ??
						request.identifiers.correlation_id ??
						request.id,
					entry,
					request
				};
			})
			.filter((row): row is VibeRow => row !== null)
			.sort((a, b) => b.entry.at - a.entry.at)
	);

	$: diffRows = $rows.filter((row) => row.request.input_type === 'diff_approval');
	$: otherRows = $rows.filter((row) => row.request.input_type !== 'diff_approval');

	let actingKeys = new Set<string>();
	let bulkApplying = false;
	let statusText = '';
	let errorText = '';
	let codingConnection: AbortController | null = null;
	let codingReconnectTimer: ReturnType<typeof setTimeout> | null = null;
	let codingScopeKey = '';
	let codingStreamState: 'idle' | 'connecting' | 'live' | 'closed' | 'error' = 'idle';
	let codingStreamMessage = '';
	let codingRows: CodingActivityRow[] = [];
	let codingLogRows: CodingLogRow[] = [];
	let projectedCodingLogRows: CodingLogRow[] = [];
	let projectedCodingLogSources: ProjectedCodingLogSources = {};
	let projectedCodingLogKey = '';
	let projectedCodingLogLoading = false;
	let projectedCodingLogError = '';
	let projectedCodingLogAbort: AbortController | null = null;
	let codingLogSerial = 0;
	const codingByKey = new Map<string, CodingActivityRow>();
	const codingReconnectBackoff = createBackoff({ initialMs: 2_000, maxMs: 60_000 });
	let vibePrompt = '';
	let dispatchingCodingTask = false;
	let autoApplyCodeProposals = false;
	let autoApplyInFlight = false;
	let autoApplyAttemptedKeys = new Set<string>();
	let reviewPanelCollapsed = true;
	let reviewPanelOpen = true;
	let reviewPendingCount = 0;
	let reviewAutoOpenSignature = '';
	let vibeDevThreadScopeKey = '';
	let vibeDevProjectEnsurePromise: Promise<string | null> | null = null;
	let vibeDevSessionId: string | null = null;
	let vibeDevThreadReady = false;
	let vibeDevThreadLoading = false;
	let vibeDevThreadError = '';
	let activeVibeProject: VibeDevProject | null = null;
	let activeVibeProjectId: string | null = null;
	let projectNameDraft = '';
	let projectNameEditKey = '';
	let projectRenameOpen = false;
	let projectCreateOpen = false;
	let projectCreateNameDraft = '';
	let projectCreateRepoDraft = '';
	let projectRepoPickerOpen = false;
	let projectRepoPickerLoading = false;
	let projectRepoPickerError = '';
	let projectRepoPickerPath = '';
	let projectRepoPickerParent: string | null = null;
	let projectRepoPickerHome: string | null = null;
	let projectRepoPickerRoot = '/';
	let projectRepoPickerEntries: DirectoryEntry[] = [];
	let projectRepoPickerTruncated = false;
	let projectSettingsOpen = false;
	let projectPreviewUrlDraft = '';
	let projectActionMenuOpen = false;
	let projectActionInFlight = false;
	let vibeStagedAttachments: UploadedAttachment[] = [];
	let vibeAttachmentUploading = false;
	let vibeAttachmentBatchVersion = 0;
	let vibeVoiceComposerPrefix: string | null = null;
	let activeFetchedVibeTask: Task | null = null;
	let activeTaskFetchKey = '';
	let activeTaskFetchInFlight = false;
	let activeVibeTaskIdOverride: string | null | undefined = undefined;
	let routeVibeProjectId: string | null = null;
	let routeVibeTaskId: string | null = null;
	let activeVibeTaskId: string | null = null;
	let workspaceTab: VibeWorkspaceTab = 'preview';

	$: scope = $scopeIdentityStore;
	$: selectedCodingProfile = $codingProfileStore.profiles.find(
		(profile) => profile.id === $codingProfileStore.selected
	);
	$: selectedCodingProfileSupportsImages =
		selectedCodingProfile?.supports_user_image_inputs === true;
	$: activeVibeProject =
		$vibeDevProjectStore.projects.find(
			(project) => project.project_id === $vibeDevProjectStore.activeProjectId
		) ??
		$vibeDevProjectStore.projects.find((project) => project.chat_session_status === 'active') ??
		$vibeDevProjectStore.projects.find((project) => !project.archived) ??
		null;
	$: if (activeVibeProject?.chat_session_id && vibeDevSessionId !== activeVibeProject.chat_session_id) {
		vibeDevSessionId = activeVibeProject.chat_session_id;
		vibeDevThreadReady = true;
		vibeDevThreadError = '';
	}
	$: if ($vibeDevProjectStore.error) {
		vibeDevThreadError = $vibeDevProjectStore.error;
	}
	$: if ((activeVibeProject?.project_id ?? '') !== projectNameEditKey) {
		projectNameEditKey = activeVibeProject?.project_id ?? '';
		projectNameDraft = activeVibeProject?.name ?? '';
		projectRenameOpen = false;
		projectCreateOpen = false;
		projectRepoPickerOpen = false;
		projectSettingsOpen = false;
		projectPreviewUrlDraft = activeVibeProject?.preview_url ?? '';
		projectActionMenuOpen = false;
	}
	$: if (!projectRenameOpen && activeVibeProject && projectNameDraft !== activeVibeProject.name) {
		projectNameDraft = activeVibeProject.name;
	}
	$: proposalDiffRows = diffRows.filter((row) => isProposalDiff(row.request));
	$: activeVibeProjectId = activeVibeProject?.project_id ?? null;
	$: vibeDevTasks = buildVisibleVibeDevTasks(
		$taskStore.tasks,
		activeFetchedVibeTask,
		activeVibeProjectId
	);
	$: allVibeDevTasks = [
		...$taskStore.tasks.filter((task) => isVibeDevTaskForProject(task, activeVibeProjectId)),
		...(activeFetchedVibeTask && isVibeDevTaskForProject(activeFetchedVibeTask, activeVibeProjectId)
			? [activeFetchedVibeTask]
			: [])
	];
	$: routeVibeProjectId = normalizeTaskIdParam($page.url.searchParams.get('project'));
	$: routeVibeTaskId = normalizeTaskIdParam($page.url.searchParams.get('task'));
	$: if (activeVibeTaskIdOverride !== undefined && routeVibeTaskId === activeVibeTaskIdOverride) {
		activeVibeTaskIdOverride = undefined;
	}
	$: activeVibeTaskId =
		activeVibeTaskIdOverride !== undefined ? activeVibeTaskIdOverride : routeVibeTaskId;
	$: activeVibeTaskFromStore = activeVibeTaskId
		? $taskStore.tasks.find((task) => task.id === activeVibeTaskId) ?? null
		: null;
	$: activeVibeTask =
		activeVibeTaskFromStore ??
		(activeFetchedVibeTask?.id === activeVibeTaskId ? activeFetchedVibeTask : null);
	$: activeVibeTaskLoading = Boolean(
		activeVibeTaskId && !activeVibeTask && ($taskStore.isLoading || activeTaskFetchInFlight)
	);
	$: activeVibeTaskMissing = Boolean(
		activeVibeTaskId && !activeVibeTask && !activeVibeTaskLoading
	);
	$: activeRunChainIds = buildVibeRunChainIds(activeVibeTask, allVibeDevTasks);
	$: orderedDiffRows = sortRowsForActiveRun(diffRows, activeRunChainIds);
	$: orderedOtherRows = sortRowsForActiveRun(otherRows, activeRunChainIds);
	$: activeRunDiffRows = filterRowsForActiveRun(orderedDiffRows, activeRunChainIds);
	$: visibleChangedFiles = buildChangedFiles(
		activeRunDiffRows.length > 0 ? activeRunDiffRows : orderedDiffRows,
		activeRunChainIds
	);
	$: activeActivityRows = filterActivityForActiveRun(codingRows, activeRunChainIds);
	$: projectedRunLogTaskIds = runLogTaskIdsForFetch(activeRunChainIds, activeVibeTaskId);
	$: combinedCodingLogRows = mergeCodingLogRows(projectedCodingLogRows, codingLogRows);
	$: activeLogRows = filterLogsForActiveRun(combinedCodingLogRows, activeRunChainIds);
	$: projectedLogSourceLabel = projectedLogSourceText(projectedCodingLogSources);
	$: runCounts = buildRunCounts(vibeDevTasks);
	$: vibeComposerMode = (activeVibeTask ? 'follow_up' : 'fresh') as VibeComposerMode;
	$: vibePromptPlaceholder = composerPlaceholder(vibeComposerMode, activeVibeTask);
	$: vibeSubmitBlocker = composerSubmitBlocker();
	$: vibeSubmitDisabled = Boolean(vibeSubmitBlocker);
	$: vibeActiveRun = buildVibeActiveRunSummary(
		activeVibeTask,
		activeVibeTaskLoading,
		activeVibeTaskMissing,
		activeVibeTaskId,
		activeRunChainIds,
		Boolean(vibeSubmitBlocker && vibePrompt.trim().length > 0)
	);
	$: vibeDevSessionHref = vibeDevSessionId
		? `/t/${VIBEDEV_THREAD_ID}/chat?session=${encodeURIComponent(vibeDevSessionId)}`
		: `/t/${VIBEDEV_THREAD_ID}/chat`;
	$: reviewPendingCount = diffRows.length + otherRows.length;
	$: reviewAttentionSignature = `${activeSurface}:${autoApplyCodeProposals ? 'auto' : 'manual'}:${reviewPendingCount}`;
	$: reviewShouldAutoOpen =
		activeSurface === 'agents' && (!autoApplyCodeProposals || reviewPendingCount > 0);
	$: reviewPanelOpen = activeSurface === 'agents' && !reviewPanelCollapsed;
	$: activeSurface =
		$page.url.searchParams.get('tab') === 'workbench' ? 'workbench' : 'agents';
	$: codingActivityEmptyText =
		codingStreamState === 'error' && codingStreamMessage
			? codingStreamMessage
			: 'No coding activity in the last 6 hours.';
	$: maybeConnectCodingStream(scope?.principal, scope?.workspace);
	$: maybeFetchProjectedRunLogs(scope?.principal, scope?.workspace, projectedRunLogTaskIds);
	$: maybeEnsureVibeDevThreadSession(scope?.principal, scope?.workspace);
	$: maybeFetchActiveVibeTask(activeVibeTaskId, activeVibeTaskFromStore);
	$: if (
		browser &&
		reviewShouldAutoOpen &&
		reviewAutoOpenSignature !== reviewAttentionSignature
	) {
		reviewPanelCollapsed = false;
		reviewAutoOpenSignature = reviewAttentionSignature;
	}
	$: if (
		browser &&
		!reviewShouldAutoOpen &&
		reviewAutoOpenSignature !== reviewAttentionSignature
	) {
		reviewAutoOpenSignature = reviewAttentionSignature;
	}
	$: if (
		browser &&
		activeSurface === 'agents' &&
		autoApplyCodeProposals &&
		proposalDiffRows.length > 0
	) {
		void autoApplyEligibleDiffs(proposalDiffRows);
	}

	onMount(() => {
		autoApplyCodeProposals = loadAutoApplyPreference();
		threadStore.start();
		taskStore.start();
		void taskStore.loadTasks();
		ensurePendingHitlBridge();
		void codingProfileStore.load();
	});

	onDestroy(() => {
		disconnectCodingStream();
		projectedCodingLogAbort?.abort();
		threadStore.stop();
		taskStore.stop();
	});

	function scopeHeaders(): HeadersInit {
		const scope = get(scopeIdentityStore);
		const headers: Record<string, string> = {};
		return headers;
	}

	function loadAutoApplyPreference(): boolean {
		// Default OFF (U5), matching vibeStudioStore — manual review unless the
		// user opted into auto-apply (same localStorage key on both surfaces).
		if (!browser) return false;
		try {
			const value = localStorage.getItem(AUTO_APPLY_STORAGE_KEY);
			if (value === 'false') return false;
			if (value === 'true') return true;
		} catch {
			// localStorage unavailable — keep the default.
		}
		return false;
	}

	function persistAutoApplyPreference(value: boolean): void {
		if (!browser) return;
		try {
			localStorage.setItem(AUTO_APPLY_STORAGE_KEY, value ? 'true' : 'false');
		} catch {
			// Storage can be unavailable; the in-memory toggle still works.
		}
	}

	function toggleAutoApplyCodeProposals(value: boolean): void {
		autoApplyCodeProposals = value;
		persistAutoApplyPreference(autoApplyCodeProposals);
		if (autoApplyCodeProposals) {
			autoApplyAttemptedKeys = new Set();
		}
	}

	function switchSurface(surface: VibeDevSurface): void {
		const params = new URLSearchParams();
		if (surface === 'workbench') params.set('tab', 'workbench');
		if (activeVibeProjectId) params.set('project', activeVibeProjectId);
		if (surface === 'agents' && activeVibeTaskId) params.set('task', activeVibeTaskId);
		const query = params.toString();
		const target = query ? `/vibe?${query}` : '/vibe';
		void goto(target, { keepFocus: true, noScroll: true });
	}

	function maybeEnsureVibeDevThreadSession(
		principal: string | undefined | null,
		workspace: string | undefined | null
	): void {
		if (!browser || !principal || !workspace) return;
		const routeProjectId = routeVibeProjectId ?? '';
		const key = `${principal}::${workspace}::${routeProjectId}`;
		if (key === vibeDevThreadScopeKey) return;
		vibeDevThreadScopeKey = key;
		vibeDevThreadReady = false;
		vibeDevSessionId = null;
		void ensureVibeDevThreadSession();
	}

	async function ensureVibeDevThreadSession(): Promise<string | null> {
		if (!browser) return null;
		if (
			vibeDevThreadReady &&
			activeVibeProject?.chat_session_id &&
			activeVibeProject.chat_session_status === 'active'
		) {
			return activeVibeProject.chat_session_id;
		}
		if (vibeDevProjectEnsurePromise) return vibeDevProjectEnsurePromise;
		vibeDevProjectEnsurePromise = (async () => {
			vibeDevThreadLoading = true;
			vibeDevThreadError = '';
			try {
				const thread = await threadStore.createThread(VIBEDEV_THREAD_NAME, VIBEDEV_THREAD_ID);
				if (!thread) throw new Error('Could not create #vibedev thread');

				await vibeDevProjectStore.load();
				let state = get(vibeDevProjectStore);
				let project: VibeDevProject | null =
					(routeVibeProjectId
						? state.projects.find((candidate) => candidate.project_id === routeVibeProjectId)
						: null) ??
					state.projects.find((candidate) => candidate.project_id === state.activeProjectId) ??
					state.projects.find(
						(candidate) => candidate.chat_session_status === 'active' && !candidate.archived
					) ??
					state.projects.find((candidate) => !candidate.archived) ??
					null;

				if (!project) {
					projectCreateOpen = true;
					projectCreateNameDraft = projectCreateNameDraft || defaultProjectName();
					throw new Error('Create a VibeDev project and choose its repo folder first.');
				} else if (project.archived || project.chat_session_status !== 'active') {
					project = await vibeDevProjectStore.activateProject(project.project_id);
				}
				if (!project?.chat_session_id) {
					throw new Error('Could not prepare a VibeDev project session');
				}

				vibeDevSessionId = project.chat_session_id;
				vibeDevThreadReady = true;
				await chatStore.loadSessions(VIBEDEV_THREAD_ID);
				return project.chat_session_id;
			} catch (error) {
				const message = error instanceof Error ? error.message : String(error);
				vibeDevThreadError = message;
				vibeDevThreadReady = false;
				return null;
			} finally {
				vibeDevThreadLoading = false;
				vibeDevProjectEnsurePromise = null;
			}
		})();
		return vibeDevProjectEnsurePromise;
	}

	function normalizeTaskIdParam(value: string | null): string | null {
		const trimmed = value?.trim() ?? '';
		return trimmed.length > 0 ? trimmed : null;
	}

	function defaultProjectName(): string {
		const count = $vibeDevProjectStore.projects.length + 1;
		return count <= 1 ? 'VibeDev Project' : `VibeDev Project ${count}`;
	}

	function vibeAgentsPath(projectId: string | null, taskId?: string | null): string {
		const params = new URLSearchParams();
		if (projectId) params.set('project', projectId);
		if (taskId) params.set('task', taskId);
		const query = params.toString();
		return query ? `/vibe?${query}` : '/vibe';
	}

	function beginProjectCreate(): void {
		if (projectActionInFlight || $vibeDevProjectStore.isLoading) return;
		projectActionMenuOpen = false;
		projectRenameOpen = false;
		projectSettingsOpen = false;
		projectCreateNameDraft = defaultProjectName();
		projectCreateRepoDraft = '';
		projectRepoPickerOpen = false;
		projectRepoPickerError = '';
		projectCreateOpen = true;
	}

	function cancelProjectCreate(): void {
		projectCreateOpen = false;
		projectCreateNameDraft = '';
		projectCreateRepoDraft = '';
		projectRepoPickerOpen = false;
		projectRepoPickerError = '';
		projectActionMenuOpen = false;
	}

	function resetProjectCreateRepoDraft(): void {
		projectCreateRepoDraft = '';
		projectRepoPickerError = '';
	}

	function projectCreateRepoDisplayPath(): string {
		const value = projectCreateRepoDraft.trim();
		if (value === '~' || value.startsWith('~/') || value.startsWith('$HOME') || value.startsWith('${HOME}')) {
			return value;
		}
		if (value.startsWith('/')) {
			return projectRepoPathLabel(value);
		}
		return value && value !== '.' ? `workdirs/home/${value}` : 'workdirs/home';
	}

	function projectWorkspaceAbsolutePath(): string {
		return get(vibeDevProjectStore).workspaceAbsolutePath?.trim() ?? '';
	}

	function projectRepoPickerHomePath(): string | null {
		return projectRepoPickerHome?.trim() || null;
	}

	function cleanAbsolutePath(value: string): string {
		return value.replace(/\/+$/, '') || '/';
	}

	function joinWorkspacePath(root: string, relative: string): string {
		const cleanRoot = cleanAbsolutePath(root);
		const cleanRelative = relative.replace(/^\/+/, '').replace(/\/+$/, '');
		return cleanRelative ? `${cleanRoot}/${cleanRelative}` : cleanRoot;
	}

	function projectRepoDraftAbsolutePath(): string | null {
		const root = projectWorkspaceAbsolutePath();
		if (!root) return null;
		const value = projectCreateRepoDraft.trim();
		if (!value || value === '.') return root;
		if (value === '~') return projectRepoPickerHomePath();
		if (value.startsWith('~/')) {
			const home = projectRepoPickerHomePath();
			return home ? `${cleanAbsolutePath(home)}/${value.slice(2)}` : null;
		}
		if (value === '$HOME' || value === '${HOME}') return projectRepoPickerHomePath();
		if (value.startsWith('$HOME/')) {
			const home = projectRepoPickerHomePath();
			return home ? `${cleanAbsolutePath(home)}/${value.slice(6)}` : null;
		}
		if (value.startsWith('${HOME}/')) {
			const home = projectRepoPickerHomePath();
			return home ? `${cleanAbsolutePath(home)}/${value.slice(8)}` : null;
		}
		if (value.startsWith('/')) return value;
		return joinWorkspacePath(root, value);
	}

	function isPathInsideProjectWorkspace(path: string): boolean {
		const root = projectWorkspaceAbsolutePath();
		if (!root || !path) return false;
		const cleanRoot = cleanAbsolutePath(root);
		const cleanPath = cleanAbsolutePath(path);
		return cleanPath === cleanRoot || cleanPath.startsWith(`${cleanRoot}/`);
	}

	function isPathInsideProjectHome(path: string): boolean {
		const home = projectRepoPickerHomePath();
		if (!home || !path) return false;
		const cleanHome = cleanAbsolutePath(home);
		const cleanPath = cleanAbsolutePath(path);
		return cleanPath === cleanHome || cleanPath.startsWith(`${cleanHome}/`);
	}

	function repoAbsolutePathToDraft(path: string): string | null {
		const cleanPath = cleanAbsolutePath(path);
		if (isPathInsideProjectWorkspace(path)) {
			const root = cleanAbsolutePath(projectWorkspaceAbsolutePath());
			const relative = cleanPath.slice(root.length).replace(/^\/+/, '');
			return relative || '.';
		}
		if (isPathInsideProjectHome(path)) {
			return cleanPath;
		}
		return cleanPath.startsWith('/') ? cleanPath : null;
	}

	function projectRepoPathLabel(path: string): string {
		if (isPathInsideProjectWorkspace(path)) {
			const relative = repoAbsolutePathToDraft(path);
			return relative === '.' ? 'workdirs/home' : `workdirs/home/${relative}`;
		}
		const home = projectRepoPickerHomePath();
		if (home) {
			const cleanHome = cleanAbsolutePath(home);
			const cleanPath = cleanAbsolutePath(path);
			if (cleanPath === cleanHome) return '~';
			if (cleanPath.startsWith(`${cleanHome}/`)) return `~/${cleanPath.slice(cleanHome.length + 1)}`;
		}
		return path;
	}

	function projectRepoPickerParentPath(): string | null {
		return projectRepoPickerParent?.trim() || null;
	}

	async function openProjectRepoPicker(): Promise<void> {
		const root = projectWorkspaceAbsolutePath();
		if (!root) {
			showError('Workspace folder unavailable', 'Reload VibeDev projects before browsing repo folders.');
			return;
		}
		projectRepoPickerOpen = true;
		await loadProjectRepoDirectory(projectRepoDraftAbsolutePath() ?? root);
	}

	async function loadProjectRepoDirectory(path?: string | null): Promise<void> {
		const root = projectWorkspaceAbsolutePath();
		const requested = path?.trim() || root;
		if (!requested) return;
		projectRepoPickerLoading = true;
		projectRepoPickerError = '';
		try {
			const params = appendCurrentScopeQuery();
			params.set('path', requested);
			const response = await timedFetch(
				`/api/magician/v2/filesystem/directories?${params.toString()}`
			);
			const payload = (await response.json().catch(() => null)) as DirectoryListResponse | null;
			if (!response.ok || !payload) {
				throw new Error(payload?.error || `server returned ${response.status}`);
			}
			projectRepoPickerHome = payload.home ?? projectRepoPickerHome;
			projectRepoPickerRoot = payload.root || projectRepoPickerRoot || '/';
			projectRepoPickerPath = payload.path;
			projectRepoPickerParent = payload.parent ?? null;
			projectRepoPickerEntries = payload.entries ?? [];
			projectRepoPickerTruncated = Boolean(payload.truncated);
		} catch (error) {
			projectRepoPickerError =
				error instanceof Error ? error.message : 'Failed to list VibeDev workspace folders.';
		} finally {
			projectRepoPickerLoading = false;
		}
	}

	function chooseProjectRepoDirectory(path: string): void {
		const draft = repoAbsolutePathToDraft(path);
		if (!draft) {
			projectRepoPickerError = 'Choose an existing folder.';
			return;
		}
		projectCreateRepoDraft = draft;
		projectRepoPickerOpen = false;
		projectRepoPickerError = '';
	}

	async function createNewVibeProject(): Promise<void> {
		const name = projectCreateNameDraft.trim();
		if (!name) {
			showError('Project name is required', 'Enter a name before creating the project.');
			return;
		}
		const repoPath = projectCreateRepoDraft.trim() || '.';
		projectActionInFlight = true;
		let project: VibeDevProject | null = null;
		try {
			project = await vibeDevProjectStore.createProject({
				name,
				repo_path: repoPath
			});
		} finally {
			projectActionInFlight = false;
		}
		if (!project) {
			showError('Could not create VibeDev project', $vibeDevProjectStore.error ?? 'Project creation failed.');
			return;
		}
		projectCreateOpen = false;
		projectCreateNameDraft = '';
		projectCreateRepoDraft = '';
		projectRepoPickerOpen = false;
		projectRepoPickerError = '';
		vibeStagedAttachments = [];
		vibeAttachmentBatchVersion += 1;
		activeFetchedVibeTask = null;
		activeTaskFetchKey = '';
		activeTaskFetchInFlight = false;
		activeVibeTaskIdOverride = null;
		vibeDevSessionId = project.chat_session_id;
		vibeDevThreadReady = true;
		await chatStore.loadSessions(VIBEDEV_THREAD_ID);
		void goto(vibeAgentsPath(project.project_id), { keepFocus: true, noScroll: true });
	}

	async function selectVibeProject(projectId: string): Promise<void> {
		if (!projectId || projectId === activeVibeProject?.project_id) return;
		projectActionMenuOpen = false;
		const project = await vibeDevProjectStore.activateProject(projectId);
		if (!project) {
			showError('Could not switch VibeDev project', $vibeDevProjectStore.error ?? 'Project activation failed.');
			return;
		}
		vibeStagedAttachments = [];
		vibeAttachmentBatchVersion += 1;
		activeFetchedVibeTask = null;
		activeTaskFetchKey = '';
		activeTaskFetchInFlight = false;
		activeVibeTaskIdOverride = project.active_root_task_id ?? null;
		vibeDevSessionId = project.chat_session_id;
		vibeDevThreadReady = true;
		await chatStore.loadSessions(VIBEDEV_THREAD_ID);
		void goto(vibeAgentsPath(project.project_id, project.active_root_task_id ?? null), {
			keepFocus: true,
			noScroll: true
		});
	}

	function resetVibeProjectContext(): void {
		vibeStagedAttachments = [];
		vibeAttachmentBatchVersion += 1;
		activeFetchedVibeTask = null;
		activeTaskFetchKey = '';
		activeTaskFetchInFlight = false;
		activeVibeTaskIdOverride = null;
		vibeDevSessionId = null;
		vibeDevThreadReady = false;
		vibeDevThreadScopeKey = '';
	}

	function beginProjectRename(): void {
		if (!activeVibeProject || projectActionInFlight || $vibeDevProjectStore.isLoading) return;
		projectNameDraft = activeVibeProject.name;
		projectCreateOpen = false;
		projectRenameOpen = true;
		projectSettingsOpen = false;
		projectActionMenuOpen = false;
	}

	function cancelProjectRename(): void {
		projectNameDraft = activeVibeProject?.name ?? '';
		projectRenameOpen = false;
		projectActionMenuOpen = false;
	}

	function beginProjectSettings(): void {
		if (!activeVibeProject || projectActionInFlight || $vibeDevProjectStore.isLoading) return;
		projectCreateOpen = false;
		projectRenameOpen = false;
		projectSettingsOpen = true;
		projectPreviewUrlDraft = activeVibeProject.preview_url ?? '';
		projectActionMenuOpen = false;
	}

	function cancelProjectSettings(): void {
		projectSettingsOpen = false;
		projectPreviewUrlDraft = activeVibeProject?.preview_url ?? '';
		projectActionMenuOpen = false;
	}

	function resetProjectSettingsDrafts(): void {
		projectPreviewUrlDraft = '';
	}

	function projectRepoPath(project: VibeDevProject | null): string {
		const value = project?.repo_path?.trim();
		return value ? value : '.';
	}

	function projectRepoLabel(project: VibeDevProject | null): string {
		return project?.repo_display_path?.trim() || `workdirs/home${projectRepoPath(project) === '.' ? '' : `/${projectRepoPath(project)}`}`;
	}

	function projectRepoTitle(project: VibeDevProject | null): string {
		return project?.repo_absolute_path?.trim() || projectRepoLabel(project);
	}

	async function saveProjectSettings(): Promise<void> {
		const project = activeVibeProject;
		if (!project || projectActionInFlight) return;
		const previewUrl = projectPreviewUrlDraft.trim();
		if (previewUrl === (project.preview_url ?? '')) {
			projectSettingsOpen = false;
			return;
		}
		projectActionInFlight = true;
		try {
			const updated = await vibeDevProjectStore.updateProject(project.project_id, {
				preview_url: previewUrl
			});
			if (!updated) {
				showError('Could not save project settings', $vibeDevProjectStore.error ?? 'Settings update failed.');
				return;
			}
			projectPreviewUrlDraft = updated.preview_url ?? '';
			projectSettingsOpen = false;
			showSuccess('VibeDev project settings saved', updated.name);
		} finally {
			projectActionInFlight = false;
		}
	}

	async function renameActiveVibeProject(): Promise<void> {
		const project = activeVibeProject;
		if (!project || projectActionInFlight) return;
		const name = projectNameDraft.trim();
		if (!name) {
			showError('Project name is required', 'Enter a name before saving.');
			return;
		}
		if (name === project.name) {
			projectNameDraft = project.name;
			projectRenameOpen = false;
			return;
		}
		projectActionInFlight = true;
		try {
			const updated = await vibeDevProjectStore.renameProject(project.project_id, name);
			if (!updated) {
				showError('Could not rename project', $vibeDevProjectStore.error ?? 'Rename failed.');
				return;
			}
			projectNameDraft = updated.name;
			projectRenameOpen = false;
			await chatStore.loadSessions(VIBEDEV_THREAD_ID);
			showSuccess('VibeDev project renamed', updated.name);
		} finally {
			projectActionInFlight = false;
		}
	}

	async function archiveActiveVibeProject(): Promise<void> {
		const project = activeVibeProject;
		if (!project || projectActionInFlight) return;
		projectActionMenuOpen = false;
		const confirmed = await requestConfirmation({
			title: 'Archive VibeDev project?',
			message: `Archive "${project.name}" and its backing chat session. You can restore it by selecting the archived project again.`,
			confirmLabel: 'Archive'
		});
		if (!confirmed) return;
		projectActionInFlight = true;
		try {
			const archived = await vibeDevProjectStore.archiveProject(project.project_id);
			if (!archived) {
				showError('Could not archive project', $vibeDevProjectStore.error ?? 'Archive failed.');
				return;
			}
			resetVibeProjectContext();
			const nextProject = get(vibeDevProjectStore).projects.find(
				(candidate) => !candidate.archived && candidate.project_id !== project.project_id
			);
			if (nextProject) {
				await selectVibeProject(nextProject.project_id);
			} else {
				void ensureVibeDevThreadSession();
				void goto('/vibe', { keepFocus: true, noScroll: true });
			}
			showSuccess('VibeDev project archived', project.name);
		} finally {
			projectActionInFlight = false;
		}
	}

	async function unarchiveActiveVibeProject(): Promise<void> {
		const project = activeVibeProject;
		if (!project || projectActionInFlight) return;
		projectActionMenuOpen = false;
		projectActionInFlight = true;
		try {
			const restored = await vibeDevProjectStore.activateProject(project.project_id);
			if (!restored) {
				showError('Could not unarchive project', $vibeDevProjectStore.error ?? 'Unarchive failed.');
				return;
			}
			activeVibeTaskIdOverride = restored.active_root_task_id ?? null;
			vibeDevSessionId = restored.chat_session_id;
			vibeDevThreadReady = true;
			await chatStore.loadSessions(VIBEDEV_THREAD_ID);
			void goto(vibeAgentsPath(restored.project_id, restored.active_root_task_id ?? null), {
				keepFocus: true,
				noScroll: true
			});
			showSuccess('VibeDev project unarchived', restored.name);
		} finally {
			projectActionInFlight = false;
		}
	}

	async function deleteActiveVibeProject(): Promise<void> {
		const project = activeVibeProject;
		if (!project || projectActionInFlight) return;
		projectActionMenuOpen = false;
		const confirmed = await requestConfirmation({
			title: 'Delete VibeDev project?',
			message: `Permanently delete "${project.name}" and its backing chat session. This removes the session messages and uploaded attachment references for that project.`,
			confirmLabel: 'Delete',
			destructive: true
		});
		if (!confirmed) return;
		projectActionInFlight = true;
		try {
			const deleted = await vibeDevProjectStore.deleteProject(project.project_id);
			if (!deleted) {
				showError('Could not delete project', $vibeDevProjectStore.error ?? 'Delete failed.');
				return;
			}
			resetVibeProjectContext();
			await chatStore.loadSessions(VIBEDEV_THREAD_ID);
			const nextProject = get(vibeDevProjectStore).projects.find((candidate) => !candidate.archived);
			if (nextProject) {
				await selectVibeProject(nextProject.project_id);
			} else {
				void ensureVibeDevThreadSession();
				void goto('/vibe', { keepFocus: true, noScroll: true });
			}
			showSuccess('VibeDev project deleted', project.name);
		} finally {
			projectActionInFlight = false;
		}
	}

	function selectVibeRun(taskId: string): void {
		activeVibeTaskIdOverride = taskId;
		void goto(vibeAgentsPath(activeVibeProjectId, taskId), { keepFocus: true, noScroll: true });
	}

	function startFreshRun(): void {
		activeFetchedVibeTask = null;
		activeTaskFetchKey = '';
		activeTaskFetchInFlight = false;
		activeVibeTaskIdOverride = null;
		void goto(vibeAgentsPath(activeVibeProjectId), { keepFocus: true, noScroll: true });
	}

	function maybeFetchActiveVibeTask(taskId: string | null, storeTask: Task | null): void {
		if (!browser || !taskId) return;
		if (storeTask) {
			if (activeFetchedVibeTask?.id === taskId) activeFetchedVibeTask = null;
			activeTaskFetchKey = '';
			activeTaskFetchInFlight = false;
			return;
		}
		if (activeFetchedVibeTask?.id === taskId || activeTaskFetchKey === taskId) return;
		activeTaskFetchKey = taskId;
		activeTaskFetchInFlight = true;
		void (async () => {
			const fetched = await taskStore.fetchTaskRecordById(taskId);
			if (activeTaskFetchKey !== taskId) return;
			activeFetchedVibeTask = fetched;
			activeTaskFetchInFlight = false;
		})();
	}

	function parentTaskIdFromDescription(description: string | undefined): string | null {
		if (!description) return null;
		const lines = description.split('\n');
		for (const line of lines) {
			const trimmed = line.trim();
			if (!trimmed.startsWith(VIBEDEV_PARENT_TASK_PREFIX)) continue;
			const value = trimmed.slice(VIBEDEV_PARENT_TASK_PREFIX.length).trim();
			return value.length > 0 ? value : null;
		}
		return null;
	}

	function projectIdFromDescription(description: string | undefined): string | null {
		if (!description) return null;
		const lines = description.split('\n');
		for (const line of lines) {
			const trimmed = line.trim();
			if (!trimmed.startsWith(VIBEDEV_PROJECT_PREFIX)) continue;
			const value = trimmed.slice(VIBEDEV_PROJECT_PREFIX.length).trim();
			return value.length > 0 ? value : null;
		}
		return null;
	}

	function buildVibeRunChainIds(activeTask: Task | null, tasks: Task[]): Set<string> {
		const ids = new Set<string>();
		if (!activeTask) return ids;
		const byId = new Map(tasks.map((task) => [task.id, task]));
		const childrenByParent = new Map<string, string[]>();
		for (const task of tasks) {
			const parentId = parentTaskIdFromDescription(task.description);
			if (!parentId) continue;
			const children = childrenByParent.get(parentId) ?? [];
			children.push(task.id);
			childrenByParent.set(parentId, children);
		}

		const visit = (taskId: string, depth: number) => {
			if (!taskId || ids.has(taskId) || depth > 24) return;
			ids.add(taskId);
			const task = byId.get(taskId);
			const parentId = parentTaskIdFromDescription(task?.description);
			if (parentId) visit(parentId, depth + 1);
			for (const childId of childrenByParent.get(taskId) ?? []) {
				visit(childId, depth + 1);
			}
		};

		visit(activeTask.id, 0);
		return ids;
	}

	function rowTaskId(row: VibeRow): string | null {
		return row.request.scope.task_id ?? null;
	}

	function sortRowsForActiveRun(rowsToSort: VibeRow[], chainIds: Set<string>): VibeRow[] {
		if (chainIds.size === 0) return rowsToSort;
		return [...rowsToSort].sort((a, b) => {
			const aActive = rowTaskId(a) ? chainIds.has(rowTaskId(a) as string) : false;
			const bActive = rowTaskId(b) ? chainIds.has(rowTaskId(b) as string) : false;
			if (aActive !== bActive) return aActive ? -1 : 1;
			return b.entry.at - a.entry.at;
		});
	}

	function filterRowsForActiveRun(rowsToFilter: VibeRow[], chainIds: Set<string>): VibeRow[] {
		if (chainIds.size === 0) return rowsToFilter;
		return rowsToFilter.filter((row) => {
			const taskId = rowTaskId(row);
			return taskId ? chainIds.has(taskId) : false;
		});
	}

	function filterActivityForActiveRun(
		rowsToFilter: CodingActivityRow[],
		chainIds: Set<string>
	): CodingActivityRow[] {
		if (chainIds.size === 0) return rowsToFilter;
		const scopedRows = rowsToFilter.filter((row) => row.taskId && chainIds.has(row.taskId));
		return scopedRows.length > 0 ? scopedRows : rowsToFilter;
	}

	function filterLogsForActiveRun(
		rowsToFilter: CodingLogRow[],
		chainIds: Set<string>
	): CodingLogRow[] {
		if (chainIds.size === 0) return rowsToFilter;
		return rowsToFilter.filter((row) => row.taskId && chainIds.has(row.taskId));
	}

	function runLogTaskIdsForFetch(chainIds: Set<string>, activeTaskId: string | null): string[] {
		if (chainIds.size > 0) return Array.from(chainIds).slice(0, 8);
		return activeTaskId ? [activeTaskId] : [];
	}

	function mergeCodingLogRows(projectedRows: CodingLogRow[], liveRows: CodingLogRow[]): CodingLogRow[] {
		const byIdentity = new Map<string, CodingLogRow>();
		for (const row of [...projectedRows, ...liveRows]) {
			const identity = [
				row.eventType,
				row.taskId ?? '',
				row.updatedAt,
				row.detail ?? '',
				row.label
			].join('::');
			if (!byIdentity.has(identity)) byIdentity.set(identity, row);
		}
		return Array.from(byIdentity.values())
			.sort((a, b) => b.updatedAt - a.updatedAt)
			.slice(0, 180);
	}

	function buildChangedFiles(rowsToScan: VibeRow[], chainIds: Set<string>): VibeChangedFile[] {
		const byPath = new Map<string, VibeChangedFile>();
		for (const row of rowsToScan) {
			const taskId = rowTaskId(row);
			const active = Boolean(taskId && chainIds.has(taskId));
			for (const file of row.request.schema.files ?? []) {
				if (!file.path) continue;
				const existing = byPath.get(file.path);
				if (existing) {
					existing.additions += file.additions;
					existing.deletions += file.deletions;
					existing.active = existing.active || active;
				} else {
					byPath.set(file.path, {
						path: file.path,
						additions: file.additions,
						deletions: file.deletions,
						active
					});
				}
			}
		}
		return Array.from(byPath.values()).sort((a, b) => {
			if (a.active !== b.active) return a.active ? -1 : 1;
			return a.path.localeCompare(b.path);
		});
	}

	function buildRunCounts(tasks: Task[]): {
		total: number;
		active: number;
		done: number;
		failed: number;
	} {
		return tasks.reduce(
			(counts, task) => {
				counts.total += 1;
				if (
					task.synthesisPending ||
					task.status === 'pending' ||
					task.status === 'planning' ||
					task.status === 'running'
				) {
					counts.active += 1;
				} else if (task.status === 'completed') {
					counts.done += 1;
				} else if (task.status === 'failed' || task.status === 'cancelled') {
					counts.failed += 1;
				}
				return counts;
			},
			{ total: 0, active: 0, done: 0, failed: 0 }
		);
	}

	function taskAcceptsFollowUp(task: Task): boolean {
		if (task.synthesisPending) return false;
		return task.status === 'completed' || task.status === 'failed' || task.status === 'cancelled';
	}

	function codingLogEmptyText(): string {
		if (projectedCodingLogLoading && activeRunChainIds.size > 0) {
			return 'Loading selected-run log…';
		}
		if (projectedCodingLogError) return projectedCodingLogError;
		if (activeRunChainIds.size > 0) return 'No coding log events for the selected run yet.';
		if (codingStreamState === 'error' && codingStreamMessage) return codingStreamMessage;
		return 'No coding log events in the last 6 hours.';
	}

	function projectedLogSourceText(sources: ProjectedCodingLogSources): string | null {
		const labels = [
			sources.coding_events ? 'coding events' : null,
			sources.command_summaries ? 'tool summaries' : null,
			sources.pty_snippets ? 'terminal snippets' : null,
			sources.dev_server_urls ? 'dev URLs' : null,
			sources.test_output ? 'test output' : null
		].filter((label): label is string => Boolean(label));
		return labels.length > 0 ? labels.join(' + ') : null;
	}

	function composerPlaceholder(mode: VibeComposerMode, task: Task | null): string {
		if (mode === 'follow_up' && task) {
			if (task.status === 'failed') return 'Ask your coding assistant to retry, fix, or narrow this failed run…';
			return 'Ask for the next change on this run…';
		}
		return 'Ask your coding assistant to build, fix, refactor, or explain code…';
	}

	function composerSubmitBlocker(): string | null {
		if (dispatchingCodingTask) return 'Dispatching coding task.';
		if (vibeDevThreadLoading || $vibeDevProjectStore.isLoading) return 'Preparing VibeDev project.';
		if (!activeVibeProject) return 'Preparing VibeDev project.';
		if (vibeAttachmentUploading) return 'Uploading attachments.';
		if (vibePrompt.trim().length === 0) return 'Enter a coding request.';
		if ($taskStore.executingTask) return 'Another task is executing.';
		if (activeVibeTaskLoading) return 'Loading selected run.';
		if (activeVibeTaskMissing) return 'Selected run was not found. Start fresh.';
		if (activeVibeTask && !taskAcceptsFollowUp(activeVibeTask)) {
			if (activeVibeTask.synthesisPending) return 'Selected run is still synthesizing.';
			return 'Selected run is still active. Finish or resolve it first.';
		}
		return null;
	}

	function rowKey(request: HitlRequest): string {
		return (
			request.schema.proposal_id ??
			request.schema.transaction_id ??
			request.identifiers.correlation_id ??
			request.id
		);
	}

	function outcomeError(outcome: HitlResolveOutcome): string | null {
		if (outcome.ok) return null;
		if ('cancelled' in outcome) return 'cancelled';
		return outcome.message || `HTTP ${outcome.status}`;
	}

	function setActing(key: string, active: boolean): void {
		const next = new Set(actingKeys);
		if (active) {
			next.add(key);
		} else {
			next.delete(key);
		}
		actingKeys = next;
	}

	function isProposalDiff(request: HitlRequest): boolean {
		return (
			request.input_type === 'diff_approval' &&
			(request.schema.approval_source === 'proposal' || Boolean(request.schema.proposal_id))
		);
	}

	async function autoApplyEligibleDiffs(rowsToApply: VibeRow[]): Promise<void> {
		if (autoApplyInFlight) return;
		autoApplyInFlight = true;
		try {
			let applied = 0;
			for (const row of rowsToApply) {
				const key = rowKey(row.request);
				if (autoApplyAttemptedKeys.has(key) || actingKeys.has(key) || bulkApplying) continue;
				autoApplyAttemptedKeys.add(key);
				const ok = await resolveDiff(row.request, 'apply');
				if (ok) applied += 1;
			}
			if (applied > 0) {
				statusText = `Auto-applied ${applied} code proposal${applied === 1 ? '' : 's'}.`;
			}
		} finally {
			autoApplyInFlight = false;
		}
	}

	async function resolveDiff(
		request: HitlRequest,
		selectedId: 'apply' | 'reject',
		selectedPaths?: string[]
	): Promise<boolean> {
		const key = rowKey(request);
		errorText = '';
		statusText = '';
		setActing(key, true);
		try {
			const outcome = await postHitlResponse(
				request,
				{ type: 'choice', selected_id: selectedId },
				scopeHeaders(),
				{ selectedPaths }
			);
			const err = outcomeError(outcome);
			if (err) {
				errorText = err;
				return false;
			}
			statusText = selectedId === 'apply' ? 'Applied.' : 'Rejected.';
			return true;
		} finally {
			setActing(key, false);
		}
	}

	async function applyDiffFile(request: HitlRequest, path: string): Promise<void> {
		await resolveDiff(request, 'apply', [path]);
	}

	async function rejectDiffFile(request: HitlRequest, path: string): Promise<void> {
		const remaining = (request.schema.files ?? [])
			.map((file) => file.path)
			.filter((candidate) => candidate && candidate !== path);
		if (remaining.length === 0) {
			await resolveDiff(request, 'reject');
			return;
		}
		await resolveDiff(request, 'apply', remaining);
	}

	async function approveAll(): Promise<void> {
		if (bulkApplying || diffRows.length === 0) return;
		bulkApplying = true;
		errorText = '';
		statusText = '';
		let applied = 0;
		const failures: string[] = [];
		for (const row of diffRows) {
			const key = rowKey(row.request);
			setActing(key, true);
			const outcome = await postHitlResponse(
				row.request,
				{ type: 'choice', selected_id: 'apply' },
				scopeHeaders()
			);
			const err = outcomeError(outcome);
			if (err) {
				failures.push(`${key}: ${err}`);
			} else {
				applied += 1;
			}
			setActing(key, false);
		}
		bulkApplying = false;
		if (failures.length > 0) {
			errorText = failures.slice(0, 3).join(' · ');
		}
		statusText =
			failures.length === 0
				? `Applied ${applied} change set${applied === 1 ? '' : 's'}.`
				: `Applied ${applied}; ${failures.length} failed.`;
	}

	async function respond(row: VibeRow): Promise<void> {
		errorText = '';
		statusText = '';
		const outcome = await respondToHitl(row.request, scopeHeaders());
		const err = outcomeError(outcome);
		if (err && err !== 'cancelled') errorText = err;
	}

	function relativeTime(ms: number): string {
		const diff = Date.now() - ms;
		const minutes = Math.round(diff / 60_000);
		if (minutes < 1) return 'just now';
		if (minutes < 60) return `${minutes}m ago`;
		const hours = Math.round(minutes / 60);
		if (hours < 48) return `${hours}h ago`;
		const days = Math.round(hours / 24);
		return `${days}d ago`;
	}

	function selectCodingProfile(id: string): void {
		if (id) codingProfileStore.select(id);
	}

	function isImageFile(file: File): boolean {
		if (file.type.startsWith('image/')) return true;
		return /\.(png|jpe?g|gif|webp|avif|bmp|svg)$/i.test(file.name);
	}

	function filterVibeAttachmentFiles(files: File[]): File[] {
		if (selectedCodingProfileSupportsImages) return files;
		const blockedImages = files.filter(isImageFile);
		if (blockedImages.length > 0) {
			showError(
				'Images need a vision-capable coding profile',
				`Skipped ${blockedImages.length} image${blockedImages.length === 1 ? '' : 's'}.`
			);
		}
		return files.filter((file) => !isImageFile(file));
	}

	function removeVibeAttachment(attachmentId: string): void {
		vibeStagedAttachments = vibeStagedAttachments.filter(
			(attachment) => attachment.attachment_id !== attachmentId
		);
	}

	async function handleVibeAttachmentFiles(event: CustomEvent<{ files: File[] }>): Promise<void> {
		await uploadVibeFilesAsAttachments(event.detail.files);
	}

	async function uploadVibeFilesAsAttachments(files: File[]): Promise<void> {
		const acceptedFiles = filterVibeAttachmentFiles(files);
		if (acceptedFiles.length === 0 || vibeAttachmentUploading) return;
		const sessionId = await ensureVibeDevThreadSession();
		if (!sessionId) {
			showError(
				'Could not prepare #vibedev attachments',
				vibeDevThreadError || 'No active #vibedev session.'
			);
			return;
		}
		const batchVersion = ++vibeAttachmentBatchVersion;
		vibeAttachmentUploading = true;
		try {
			for (const file of acceptedFiles) {
				try {
					const uploaded = await chatStore.uploadAttachment(sessionId, file);
					if (batchVersion !== vibeAttachmentBatchVersion) break;
					vibeStagedAttachments = [...vibeStagedAttachments, { ...uploaded, label: file.name }];
				} catch (error) {
					showError(
						'Attachment upload failed',
						error instanceof Error ? `${file.name}: ${error.message}` : `Could not upload ${file.name}.`
					);
				}
			}
		} finally {
			if (batchVersion === vibeAttachmentBatchVersion) {
				vibeAttachmentUploading = false;
			}
		}
	}

	function handleVibeMicCapture(event: CustomEvent<{ file: File; durationMs: number }>): void {
		void uploadVibeFilesAsAttachments([event.detail.file]);
	}

	function handleVibeMicTranscribeDelta(
		event: CustomEvent<{ transcript: string }>
	): void {
		const delta = event.detail.transcript;
		if (vibeVoiceComposerPrefix === null) {
			vibeVoiceComposerPrefix = vibePrompt;
		}
		vibePrompt =
			vibeVoiceComposerPrefix.length > 0
				? `${vibeVoiceComposerPrefix.trim()} ${delta}`
				: delta;
	}

	function handleVibeMicTranscribe(
		event: CustomEvent<{ transcript: string; durationMs: number }>
	): void {
		const transcript = event.detail.transcript.trim();
		const prefix = vibeVoiceComposerPrefix ?? vibePrompt;
		vibeVoiceComposerPrefix = null;
		if (!transcript) return;
		vibePrompt = prefix.length > 0 ? `${prefix.trim()} ${transcript}` : transcript;
	}

	async function submitVibePrompt(): Promise<void> {
		const prompt = vibePrompt.trim();
		if (vibeSubmitBlocker) return;
		const parentTask = activeVibeTask;
		dispatchingCodingTask = true;
		errorText = '';
		statusText = '';
		try {
			const sessionId = await ensureVibeDevThreadSession();
			if (!sessionId) {
				throw new Error(vibeDevThreadError || 'Could not prepare #vibedev session');
			}
			const project = activeVibeProject;
			if (!project?.project_id) {
				throw new Error('Could not prepare a VibeDev project');
			}
			// One governed request: the server composes the description, admits the
			// run idempotently, creates the task, pins the project pointer and
			// dispatches (VibeDevRunService::start_build) — the three client calls
			// this replaced. This cockpit has no Discuss/Autopilot switch and no
			// budget or visual-self-correct controls, so only the auto-apply toggle
			// rides along; the server merges the parent continuation ref itself.
			const { taskId, isFollowUp } = await startVibeDevRun(
				prompt,
				{
					project,
					parentTask,
					profile: selectedCodingProfile
						? { id: selectedCodingProfile.id, label: selectedCodingProfile.label }
						: null,
					stagedAttachments: vibeStagedAttachments,
					sessionId,
					referenceTaskIds: [],
					// The old client path created ordinary user-visible tasks, and
					// this cockpit's run rail reads the task store — an Internal
					// run would vanish from it.
					saveAsTask: true
				},
				{ mode: 'build', autoApply: autoApplyCodeProposals }
			);
			vibePrompt = '';
			vibeStagedAttachments = [];
			vibeAttachmentBatchVersion += 1;
			statusText = isFollowUp ? 'Follow-up coding task started.' : 'Coding task started.';
			void goto(vibeAgentsPath(project.project_id, taskId), { keepFocus: true, noScroll: true });
			showSuccess(isFollowUp ? 'VibeDev follow-up started' : 'VibeDev task started', taskId);
		} catch (error) {
			const message = error instanceof Error ? error.message : String(error);
			errorText = message;
			showError('Could not start VibeDev task', message);
		} finally {
			dispatchingCodingTask = false;
		}
	}

	function maybeConnectCodingStream(
		principal: string | undefined | null,
		workspace: string | undefined | null
	): void {
		if (!browser || !principal || !workspace) return;
		const key = `${principal}::${workspace}`;
		if (key === codingScopeKey) return;
		codingScopeKey = key;
		void connectCodingStream(principal, workspace);
	}

	async function connectCodingStream(principal: string, workspace: string): Promise<void> {
		disconnectCodingStream();
		codingByKey.clear();
		codingRows = [];
		codingLogRows = [];
		codingLogSerial = 0;
		const controller = new AbortController();
		codingConnection = controller;
		codingStreamState = 'connecting';
		codingStreamMessage = '';
		let connectTimedOut = false;
		const connectWatchdog = setTimeout(() => {
			connectTimedOut = true;
			controller.abort();
		}, CODING_STREAM_CONNECT_TIMEOUT_MS);
		try {
			const params = new URLSearchParams();
			params.set('event_type', 'coding.');
			params.set('limit', '80');
			params.set('since', String(Date.now() - 6 * 60 * 60 * 1000));
			const response = await timedFetch(`/api/magician/v3/events?${params.toString()}`, {
				signal: controller.signal,
				timeoutMs: LONG_FETCH_TIMEOUT_MS
			});
			if (controller !== codingConnection) return;
			clearTimeout(connectWatchdog);
			if (!response.ok || !response.body) {
				codingStreamState = 'error';
				codingStreamMessage = `${response.status} ${response.statusText}`;
				scheduleCodingStreamReconnect(principal, workspace);
				return;
			}
			codingStreamState = 'live';
			codingReconnectBackoff.reset();
			const reader = response.body.getReader();
			const decoder = new TextDecoder('utf-8');
			let buffer = '';
			while (true) {
				const { done, value } = await reader.read();
				if (controller !== codingConnection) return;
				if (done) {
					codingStreamState = 'closed';
					scheduleCodingStreamReconnect(principal, workspace);
					return;
				}
				codingReconnectBackoff.reset();
				buffer += decoder.decode(value, { stream: true });
				let nl: number;
				while ((nl = buffer.indexOf('\n')) !== -1) {
					const line = buffer.slice(0, nl);
					buffer = buffer.slice(nl + 1);
					if (line.trim()) ingestCodingLine(line);
				}
			}
		} catch (error) {
			if (controller !== codingConnection) return;
			if (controller.signal.aborted) {
				clearTimeout(connectWatchdog);
				if (connectTimedOut) {
					codingStreamState = 'error';
					codingStreamMessage = 'Coding event stream did not connect in time.';
					scheduleCodingStreamReconnect(principal, workspace);
				} else {
					codingStreamState = 'closed';
				}
				return;
			}
			clearTimeout(connectWatchdog);
			codingStreamState = 'error';
			codingStreamMessage = error instanceof Error ? error.message : String(error);
			scheduleCodingStreamReconnect(principal, workspace);
		} finally {
			clearTimeout(connectWatchdog);
		}
	}

	function scheduleCodingStreamReconnect(principal: string, workspace: string): void {
		if (!browser) return;
		if (codingReconnectTimer) {
			clearTimeout(codingReconnectTimer);
			codingReconnectTimer = null;
		}
		const scopeKey = `${principal}::${workspace}`;
		const delay = codingReconnectBackoff.nextMs();
		codingReconnectTimer = setTimeout(() => {
			codingReconnectTimer = null;
			if (codingScopeKey !== scopeKey) return;
			void connectCodingStream(principal, workspace);
		}, delay);
	}

	function disconnectCodingStream(): void {
		if (codingReconnectTimer) {
			clearTimeout(codingReconnectTimer);
			codingReconnectTimer = null;
		}
		if (codingConnection) {
			codingConnection.abort();
			codingConnection = null;
		}
	}

	function maybeFetchProjectedRunLogs(
		principal: string | undefined | null,
		workspace: string | undefined | null,
		taskIds: string[]
	): void {
		if (!browser || !principal || !workspace) return;
		if (taskIds.length === 0) {
			projectedCodingLogKey = '';
			projectedCodingLogRows = [];
			projectedCodingLogSources = {};
			projectedCodingLogError = '';
			projectedCodingLogLoading = false;
			projectedCodingLogAbort?.abort();
			projectedCodingLogAbort = null;
			return;
		}
		const key = `${principal}::${workspace}::${taskIds.join(',')}`;
		if (key === projectedCodingLogKey) return;
		projectedCodingLogKey = key;
		void fetchProjectedRunLogs(principal, workspace, taskIds, key);
	}

	async function fetchProjectedRunLogs(
		principal: string,
		workspace: string,
		taskIds: string[],
		key: string
	): Promise<void> {
		projectedCodingLogAbort?.abort();
		const controller = new AbortController();
		projectedCodingLogAbort = controller;
		projectedCodingLogLoading = true;
		projectedCodingLogError = '';
		try {
			const responses = await Promise.all(
				taskIds.map(async (taskId) => {
					const response = await timedFetch(
						`/api/magician/v2/vibedev/runs/${encodeURIComponent(taskId)}/logs?limit=120`,
						{
							signal: controller.signal,
							timeoutMs: 15_000
						}
					);
					const payload = (await response.json().catch(() => null)) as
						| ProjectedCodingLogResponse
						| { error?: string; message?: string }
						| null;
					if (!response.ok) {
						const message =
							(payload as { message?: string; error?: string } | null)?.message ||
							(payload as { error?: string } | null)?.error ||
							`server returned ${response.status}`;
						throw new Error(message);
					}
					return payload as ProjectedCodingLogResponse;
				})
			);
			if (controller.signal.aborted || key !== projectedCodingLogKey) return;
			const sourceSummary: ProjectedCodingLogSources = {};
			const rows: CodingLogRow[] = [];
			const seenProjectionItemIds = new Set<string>();
			for (const payload of responses) {
				Object.assign(sourceSummary, mergeProjectedLogSources(sourceSummary, payload.sources));
				for (const item of payload.items ?? []) {
					const itemKey = `${item.source}:${item.id}`;
					if (seenProjectionItemIds.has(itemKey)) continue;
					seenProjectionItemIds.add(itemKey);
					rows.push(projectedLogItemToRow(item, payload.task_id));
				}
			}
			projectedCodingLogSources = sourceSummary;
			projectedCodingLogRows = rows
				.sort((a, b) => b.updatedAt - a.updatedAt)
				.slice(0, 160);
		} catch (error) {
			if (controller.signal.aborted || key !== projectedCodingLogKey) return;
			projectedCodingLogError =
				error instanceof Error ? error.message : 'Could not load selected-run logs.';
			projectedCodingLogRows = [];
			projectedCodingLogSources = {};
		} finally {
			if (key === projectedCodingLogKey && projectedCodingLogAbort === controller) {
				projectedCodingLogLoading = false;
				projectedCodingLogAbort = null;
			}
		}
	}

	function mergeProjectedLogSources(
		current: ProjectedCodingLogSources,
		next: ProjectedCodingLogSources | undefined
	): ProjectedCodingLogSources {
		if (!next) return current;
		return {
			event_log: Boolean(current.event_log || next.event_log),
			coding_events: Boolean(current.coding_events || next.coding_events),
			command_summaries: Boolean(current.command_summaries || next.command_summaries),
			pty_sessions: Boolean(current.pty_sessions || next.pty_sessions),
			pty_snippets: Boolean(current.pty_snippets || next.pty_snippets),
			dev_server_urls: Boolean(current.dev_server_urls || next.dev_server_urls),
			test_output: Boolean(current.test_output || next.test_output)
		};
	}

	function projectedLogItemToRow(item: ProjectedCodingLogItem, fallbackTaskId: string): CodingLogRow {
		return {
			key: `projection:${item.id}`,
			sourceKey: item.source,
			eventType: item.event_type || item.kind,
			label: item.label,
			detail: item.detail ?? null,
			tone: projectedLogTone(item),
			profileLabel: item.profile_label ?? null,
			taskId: item.task_id ?? fallbackTaskId,
			updatedAt: item.timestamp_ms || Date.now()
		};
	}

	function projectedLogTone(item: ProjectedCodingLogItem): CodingLogTone {
		if (item.kind === 'run_started' || item.kind === 'command_summary') return 'running';
		if (item.kind === 'approval_requested') return 'waiting';
		if (item.kind === 'run_completed') return 'done';
		if (item.kind === 'run_failed') return 'failed';
		return 'info';
	}

	function ingestCodingLine(line: string): void {
		let parsed: Record<string, unknown>;
		try {
			parsed = JSON.parse(line);
		} catch {
			return;
		}
		const event = unwrapCodingEvent(parsed);
		if (!event) return;
		const shadowId = (event.payload.shadow_workspace_id as string | undefined) ?? 'coding';
		const key = `coding:${shadowId}`;
		const existing = codingByKey.get(key);
		const profile = event.payload.coding_profile as Record<string, unknown> | undefined;
		const profileLabel =
			(typeof profile?.label === 'string' && profile.label) ||
			(typeof profile?.id === 'string' && profile.id) ||
			null;
		const taskId = (event.payload.task_id as string | undefined) ?? existing?.taskId ?? null;
		appendCodingLog(event, key, taskId, profileLabel ?? existing?.profileLabel ?? null);
		const update = (patch: Partial<CodingActivityRow>) => {
			const next: CodingActivityRow = {
				key,
				label: existing?.label ?? 'Coding',
				detail: existing?.detail ?? null,
				status: existing?.status ?? 'running',
				profileLabel: profileLabel ?? existing?.profileLabel ?? null,
				taskId,
				updatedAt: event.ts,
				...patch
			};
			codingByKey.set(key, next);
			codingRows = Array.from(codingByKey.values())
				.sort((a, b) => b.updatedAt - a.updatedAt)
				.slice(0, 12);
		};

		if (event.eventType === 'coding.started') {
			update({
				label: `Coding with ${profileLabel ?? 'coding agent'}`,
				detail: (event.payload.prompt_preview as string | undefined) ?? null,
				status: 'running'
			});
			return;
		}
		if (event.eventType === 'coding.message') {
			const delta = (event.payload.delta as string | undefined) ?? '';
			if (!delta) return;
			update({
				label: existing?.label ?? `Coding with ${profileLabel ?? 'coding agent'}`,
				detail: trimActivity(`${existing?.detail ?? ''} ${delta}`, 180),
				status: 'running'
			});
			return;
		}
		if (event.eventType === 'coding.tool.started') {
			const tool = (event.payload.tool_name as string | undefined) ?? 'coding agent tool';
			update({ detail: `Running ${tool}`, status: 'running' });
			return;
		}
		if (event.eventType === 'coding.approval_requested') {
			const fileCount = event.payload.file_count as number | undefined;
			update({
				label: 'Coding proposal ready',
				detail:
					typeof fileCount === 'number'
						? `${fileCount} file${fileCount === 1 ? '' : 's'}`
						: existing?.detail ?? null,
				status: 'waiting'
			});
			return;
		}
		if (event.eventType === 'coding.completed') {
			const pendingApproval = event.payload.pending_approval === true;
			const noChange = event.payload.no_change === true;
			update({
				label: pendingApproval
					? 'Coding proposal ready'
					: noChange
						? 'Coding finished with no changes'
						: 'Coding finished',
				detail:
					(event.payload.assistant_text as string | undefined) ??
					existing?.detail ??
					null,
				status: pendingApproval ? 'waiting' : 'done'
			});
			return;
		}
		if (event.eventType === 'coding.failed') {
			update({
				label: 'Coding failed',
				detail: (event.payload.error as string | undefined) ?? 'Coding agent task failed',
				status: 'failed'
			});
		}
	}

	function appendCodingLog(
		event: { eventType: string; payload: Record<string, unknown>; ts: number },
		sourceKey: string,
		taskId: string | null,
		profileLabel: string | null
	): void {
		const row = codingLogRowFromEvent(event, sourceKey, taskId, profileLabel);
		if (!row) return;
		codingLogRows = [row, ...codingLogRows].slice(0, 80);
	}

	function codingLogRowFromEvent(
		event: { eventType: string; payload: Record<string, unknown>; ts: number },
		sourceKey: string,
		taskId: string | null,
		profileLabel: string | null
	): CodingLogRow | null {
		const base = {
			key: `${event.ts}:${++codingLogSerial}:${event.eventType}`,
			sourceKey,
			eventType: event.eventType,
			profileLabel,
			taskId,
			updatedAt: event.ts
		};

		if (event.eventType === 'coding.started') {
			return {
				...base,
				label: `Coding agent run started${profileLabel ? ` with ${profileLabel}` : ''}`,
				detail: (event.payload.prompt_preview as string | undefined) ?? null,
				tone: 'running'
			};
		}
		if (event.eventType === 'coding.message') {
			const detail =
				(event.payload.delta as string | undefined) ??
				(event.payload.assistant_text as string | undefined) ??
				null;
			if (!detail?.trim()) return null;
			return {
				...base,
				label: 'Coding agent message',
				detail,
				tone: 'info'
			};
		}
		if (event.eventType === 'coding.tool.started') {
			const tool = (event.payload.tool_name as string | undefined) ?? 'tool';
			return {
				...base,
				label: 'Tool started',
				detail: tool,
				tone: 'running'
			};
		}
		if (event.eventType === 'coding.approval_requested') {
			const fileCount = event.payload.file_count as number | undefined;
			return {
				...base,
				label: 'Changes ready for review',
				detail:
					typeof fileCount === 'number'
						? `${fileCount} file${fileCount === 1 ? '' : 's'} in the proposal`
						: null,
				tone: 'waiting'
			};
		}
		if (event.eventType === 'coding.completed') {
			const pendingApproval = event.payload.pending_approval === true;
			const noChange = event.payload.no_change === true;
			return {
				...base,
				label: pendingApproval
					? 'Run completed with pending review'
					: noChange
						? 'Run completed with no changes'
						: 'Run completed',
				detail: (event.payload.assistant_text as string | undefined) ?? null,
				tone: pendingApproval ? 'waiting' : 'done'
			};
		}
		if (event.eventType === 'coding.failed') {
			return {
				...base,
				label: 'Run failed',
				detail: (event.payload.error as string | undefined) ?? 'Coding agent task failed',
				tone: 'failed'
			};
		}
		return {
			...base,
			label: event.eventType.replace(/^coding\./, 'Coding '),
			detail: null,
			tone: 'info'
		};
	}

	function unwrapCodingEvent(
		parsed: Record<string, unknown>
	): { eventType: string; payload: Record<string, unknown>; ts: number } | null {
		const outerType = String(parsed.event_type ?? '');
		const data = parsed.data as Record<string, unknown> | undefined;
		let eventType = outerType;
		let payload: Record<string, unknown> = data ?? {};
		if (outerType === 'AgentEvent' && data) {
			const inner = data.event as Record<string, unknown> | undefined;
			if (!inner || typeof inner.event_type !== 'string') return null;
			eventType = inner.event_type;
			payload = (inner.payload as Record<string, unknown>) ?? {};
		}
		if (!eventType.startsWith('coding.')) return null;
		return { eventType, payload, ts: extractEventTimestamp(parsed, payload) };
	}

	function extractEventTimestamp(
		parsed: Record<string, unknown>,
		payload: Record<string, unknown>
	): number {
		const candidates = [
			parsed.timestamp_ms,
			(parsed.data as Record<string, unknown> | undefined)?.timestamp_ms,
			payload.timestamp_ms
		];
		for (const candidate of candidates) {
			if (typeof candidate === 'number' && Number.isFinite(candidate)) return candidate;
		}
		return Date.now();
	}

	function trimActivity(value: string, max: number): string {
		const trimmed = value.replace(/\s+/g, ' ').trim();
		if (trimmed.length <= max) return trimmed;
		return `${trimmed.slice(0, max - 1)}…`;
	}

	function isVibeDevTask(task: Task): boolean {
		if (task.uiThreadId === VIBEDEV_THREAD_ID) return true;
		return task.tags.some((tag) => tag.name.toLowerCase() === 'vibedev');
	}

	function isVibeDevTaskForProject(task: Task, projectId: string | null): boolean {
		if (!isVibeDevTask(task)) return false;
		const taskProjectId = projectIdFromDescription(task.description);
		if (!taskProjectId) return true;
		return Boolean(projectId && taskProjectId === projectId);
	}

	function buildVisibleVibeDevTasks(
		tasks: Task[],
		fetchedTask: Task | null,
		projectId: string | null
	): Task[] {
		const byId = new Map<string, Task>();
		for (const task of tasks) {
			if (isVibeDevTaskForProject(task, projectId)) byId.set(task.id, task);
		}
		if (fetchedTask && isVibeDevTaskForProject(fetchedTask, projectId)) {
			byId.set(fetchedTask.id, fetchedTask);
		}
		const sorted = Array.from(byId.values()).sort((a, b) => taskUpdatedAtMs(b) - taskUpdatedAtMs(a));
		const visible = sorted.slice(0, 8);
		if (!fetchedTask || !isVibeDevTaskForProject(fetchedTask, projectId)) return visible;
		if (visible.some((task) => task.id === fetchedTask.id)) return visible;
		return [fetchedTask, ...visible.slice(0, 7)];
	}

	function taskUpdatedAtMs(task: Task): number {
		const updated = Date.parse(task.updatedAt);
		if (Number.isFinite(updated)) return updated;
		const created = Date.parse(task.createdAt);
		return Number.isFinite(created) ? created : 0;
	}

	function taskStatusLabel(task: Task): string {
		if (task.synthesisPending) return 'synthesizing';
		return task.status;
	}

	function taskStatusDetail(task: Task): string {
		const detail =
			task.completionSummary ||
			task.completionOutcome ||
			task.errorMessage ||
			task.currentSubstepTitle ||
			task.currentStepTitle ||
			task.description;
		return trimActivity(detail || 'No summary yet.', 180);
	}

	function buildVibeActiveRunSummary(
		task: Task | null,
		loading: boolean,
		missing: boolean,
		taskId: string | null,
		chainIds: Set<string>,
		blocked: boolean
	): VibeComposerActiveRun | null {
		if (task) {
			const meta = [
				taskStatusLabel(task),
				task.id,
				relativeTime(taskUpdatedAtMs(task)),
				chainIds.size > 1 ? `${chainIds.size} linked runs` : null
			].filter((item): item is string => Boolean(item));
			return {
				label: 'Follow-up context',
				title: task.title,
				meta,
				blocked
			};
		}
		if (loading) {
			return {
				label: 'Selected run',
				title: 'Loading selected run...',
				meta: taskId ? [taskId] : [],
				blocked
			};
		}
		if (missing) {
			return {
				label: 'Selected run',
				title: 'Selected run was not found',
				meta: taskId ? [taskId] : [],
				blocked
			};
		}
		return null;
	}
</script>

<svelte:head>
	<title>VibeDev · {PRODUCT_NAME}</title>
</svelte:head>

<div class="vibe-page presto-gaui-page" class:vibe-page--workbench={activeSurface === 'workbench'}>
	<header class="vibe-head">
		<div class="vibe-head__actions">
			<div class="vibe-tabs" role="tablist" aria-label="VibeDev surfaces">
				<button
					type="button"
					role="tab"
					class:vibe-tab--active={activeSurface === 'agents'}
					aria-selected={activeSurface === 'agents'}
					on:click={() => switchSurface('agents')}
				>Agents</button>
				<button
					type="button"
					role="tab"
					class:vibe-tab--active={activeSurface === 'workbench'}
					aria-selected={activeSurface === 'workbench'}
					on:click={() => switchSurface('workbench')}
				>Workbench</button>
			</div>
		</div>
	</header>

	{#if statusText || errorText}
		<div class="vibe-status" class:vibe-status--error={!!errorText}>
			{errorText || statusText}
		</div>
	{/if}

	{#if activeSurface === 'workbench'}
		<section class="vibe-workbench" aria-label="Developer workbench">
			<WorkbenchColumn threadId={null} placement="main" />
		</section>
	{:else}
		<div
			class="vibe-agents-shell"
			class:vibe-agents-shell--review-open={reviewPanelOpen}
			class:vibe-agents-shell--review-collapsed={!reviewPanelOpen}
		>
			<main class="vibe-code-workspace" aria-label="VibeDev coding workspace">
				<aside class="vibe-code-rail" aria-label="VibeDev runs and files">
					<section class="vibe-code-panel vibe-project-panel" aria-labelledby="vibe-project-heading">
						<div class="vibe-code-panel__head">
							<div>
								<h2 id="vibe-project-heading">
									Projects <span class="vibe-heading-count">({$vibeDevProjectStore.projects.length})</span>
								</h2>
								{#if activeVibeProject}
									<p>{activeVibeProject.chat_session_status}</p>
								{/if}
							</div>
							<div class="vibe-project-menu">
								<button
									type="button"
									class="vibe-mini-action vibe-project-menu__trigger"
									aria-haspopup="menu"
									aria-expanded={projectActionMenuOpen}
									disabled={projectActionInFlight}
									on:click={() => (projectActionMenuOpen = !projectActionMenuOpen)}
								>
									Actions
								</button>
								{#if projectActionMenuOpen}
									<div class="vibe-project-menu__popover" role="menu" aria-label="Project actions">
										<button
											type="button"
											role="menuitem"
											disabled={$vibeDevProjectStore.isLoading || projectActionInFlight}
											on:click={beginProjectCreate}
										>
											New project
										</button>
										<button
											type="button"
											role="menuitem"
											disabled={
												!activeVibeProject ||
												projectActionInFlight ||
												$vibeDevProjectStore.isLoading
											}
											on:click={beginProjectRename}
										>
											Edit name
										</button>
										<button
											type="button"
											role="menuitem"
											disabled={
												!activeVibeProject ||
												projectActionInFlight ||
												$vibeDevProjectStore.isLoading
											}
											on:click={beginProjectSettings}
										>
											Preview settings
										</button>
										<div class="vibe-project-menu__divider" aria-hidden="true"></div>
										{#if activeVibeProject?.archived}
											<button
												type="button"
												role="menuitem"
												disabled={projectActionInFlight || $vibeDevProjectStore.isLoading}
												on:click={() => void unarchiveActiveVibeProject()}
											>
												Unarchive
											</button>
										{:else}
											<button
												type="button"
												role="menuitem"
												disabled={
													!activeVibeProject ||
													projectActionInFlight ||
													$vibeDevProjectStore.isLoading
												}
												on:click={() => void archiveActiveVibeProject()}
											>
												Archive
											</button>
										{/if}
										<button
											type="button"
											role="menuitem"
											class="vibe-project-menu__danger"
											disabled={
												!activeVibeProject ||
												projectActionInFlight ||
												$vibeDevProjectStore.isLoading
											}
											on:click={() => void deleteActiveVibeProject()}
										>
											Delete
										</button>
									</div>
								{/if}
							</div>
						</div>
						{#if projectCreateOpen}
							<form
								class="vibe-project-settings vibe-project-create"
								on:submit|preventDefault={() => void createNewVibeProject()}
							>
								<label>
									<span>Name</span>
									<input
										type="text"
										bind:value={projectCreateNameDraft}
										placeholder="VibeDev Project"
										disabled={projectActionInFlight || $vibeDevProjectStore.isLoading}
										maxlength="96"
									/>
								</label>
								<div class="vibe-project-settings__row">
									<label>
										<span>Repo folder</span>
										<input
											type="text"
											bind:value={projectCreateRepoDraft}
											placeholder=". or my-app"
											disabled={projectActionInFlight || $vibeDevProjectStore.isLoading}
										/>
									</label>
									<button
										type="button"
										class="vibe-mini-action"
										disabled={
											projectActionInFlight ||
											$vibeDevProjectStore.isLoading ||
											!$vibeDevProjectStore.workspaceAbsolutePath
										}
										on:click={() => void openProjectRepoPicker()}
									>
										Browse
									</button>
								</div>
								<p class="vibe-project-settings__hint">
									{projectCreateRepoDisplayPath()}
								</p>
								{#if projectRepoPickerOpen}
									<div class="vibe-project-dir-picker" role="dialog" aria-label="Choose VibeDev repo folder">
										<div class="vibe-project-dir-picker__head">
											<span title={projectRepoPickerPath}>
												{projectRepoPathLabel(projectRepoPickerPath)}
											</span>
											<button
												type="button"
												aria-label="Close repo folder picker"
												on:click={() => (projectRepoPickerOpen = false)}
											>×</button>
										</div>
										<div class="vibe-project-dir-picker__actions">
											<button
												type="button"
												disabled={!projectRepoPickerParentPath() || projectRepoPickerLoading}
												on:click={() => void loadProjectRepoDirectory(projectRepoPickerParentPath())}
											>
												Parent
											</button>
											<button
												type="button"
												disabled={projectRepoPickerLoading}
												on:click={() => void loadProjectRepoDirectory(projectWorkspaceAbsolutePath())}
											>
												Workspace
											</button>
											<button
												type="button"
												disabled={!projectRepoPickerHomePath() || projectRepoPickerLoading}
												on:click={() => void loadProjectRepoDirectory(projectRepoPickerHomePath())}
											>
												Home
											</button>
											<button
												type="button"
												disabled={projectRepoPickerLoading}
												on:click={() => void loadProjectRepoDirectory(projectRepoPickerRoot)}
											>
												Root
											</button>
											<button
												type="button"
												disabled={!projectRepoPickerPath || projectRepoPickerLoading}
												on:click={() => chooseProjectRepoDirectory(projectRepoPickerPath)}
											>
												Use this
											</button>
										</div>
										{#if projectRepoPickerError}
											<div class="vibe-project-dir-picker__empty vibe-project-dir-picker__error">
												{projectRepoPickerError}
											</div>
										{:else if projectRepoPickerLoading}
											<div class="vibe-project-dir-picker__empty">Loading folders…</div>
										{:else if projectRepoPickerEntries.length === 0}
											<div class="vibe-project-dir-picker__empty">No child folders.</div>
										{:else}
											<div class="vibe-project-dir-picker__list">
												{#each projectRepoPickerEntries as entry (entry.path)}
													<button
														type="button"
														title={entry.path}
														on:click={() => void loadProjectRepoDirectory(entry.path)}
														on:dblclick={() => chooseProjectRepoDirectory(entry.path)}
													>
														<span>{entry.name}</span>
													</button>
												{/each}
											</div>
											{#if projectRepoPickerTruncated}
												<div class="vibe-project-dir-picker__empty">Showing first 240 folders.</div>
											{/if}
										{/if}
									</div>
								{/if}
								<div class="vibe-project-settings__actions">
									<button
										type="button"
										class="vibe-mini-action"
										disabled={projectActionInFlight}
										on:click={resetProjectCreateRepoDraft}
									>
										Use workspace
									</button>
									<button
										type="button"
										class="vibe-mini-action"
										disabled={projectActionInFlight}
										on:click={cancelProjectCreate}
									>
										Cancel
									</button>
									<button
										type="submit"
										class="vibe-mini-action"
										disabled={
											projectActionInFlight ||
											$vibeDevProjectStore.isLoading ||
											!projectCreateNameDraft.trim()
										}
									>
										Create
									</button>
								</div>
							</form>
						{/if}
						{#if $vibeDevProjectStore.projects.length === 0 && !projectCreateOpen}
							<div class="vibe-empty">
								{$vibeDevProjectStore.isLoading ? 'Preparing projects…' : 'No VibeDev projects yet.'}
							</div>
						{:else}
							{#if activeVibeProject}
								{#if projectRenameOpen}
									<form
										class="vibe-project-picker vibe-project-picker--editing"
										on:submit|preventDefault={() => void renameActiveVibeProject()}
									>
										<input
											type="text"
											class="vibe-project-name-input"
											aria-label="Project name"
											bind:value={projectNameDraft}
											disabled={projectActionInFlight || $vibeDevProjectStore.isLoading}
											maxlength="96"
										/>
										<button
											type="submit"
											class="vibe-mini-action"
											disabled={
												projectActionInFlight ||
												$vibeDevProjectStore.isLoading ||
												!projectNameDraft.trim() ||
												projectNameDraft.trim() === activeVibeProject.name
											}
										>
											Save
										</button>
										<button
											type="button"
											class="vibe-mini-action"
											disabled={projectActionInFlight}
											on:click={cancelProjectRename}
										>
											Cancel
										</button>
									</form>
								{:else}
									<div class="vibe-project-picker">
										<select
											class="vibe-project-select"
											aria-label="VibeDev project"
											value={activeVibeProject.project_id}
											disabled={$vibeDevProjectStore.isLoading || projectActionInFlight}
											on:change={(event) =>
												void selectVibeProject((event.currentTarget as HTMLSelectElement).value)}
										>
											{#each $vibeDevProjectStore.projects as project (project.project_id)}
												<option value={project.project_id}>
													{project.archived ? `${project.name} (archived)` : project.name}
												</option>
											{/each}
										</select>
									</div>
								{/if}
								<div class="vibe-project-meta">
									<a href={vibeDevSessionHref}>Session</a>
									<span>{activeVibeProject.chat_session_id}</span>
									<strong>Repo</strong>
									<span title={projectRepoTitle(activeVibeProject)}>{projectRepoLabel(activeVibeProject)}</span>
									{#if activeVibeProject.preview_url}
										<strong>Preview</strong>
										<span>{activeVibeProject.preview_url}</span>
									{/if}
								</div>
								{#if projectSettingsOpen}
									<form
										class="vibe-project-settings"
										on:submit|preventDefault={() => void saveProjectSettings()}
									>
										<label>
											<span>Preview URL</span>
											<input
												type="url"
												bind:value={projectPreviewUrlDraft}
												placeholder="Auto-discover"
												disabled={projectActionInFlight || $vibeDevProjectStore.isLoading}
											/>
										</label>
										<div class="vibe-project-settings__actions">
											<button
												type="button"
												class="vibe-mini-action"
												disabled={projectActionInFlight}
												on:click={resetProjectSettingsDrafts}
											>
												Auto
											</button>
											<button
												type="button"
												class="vibe-mini-action"
												disabled={projectActionInFlight}
												on:click={cancelProjectSettings}
											>
												Cancel
											</button>
											<button
												type="submit"
												class="vibe-mini-action"
												disabled={projectActionInFlight || $vibeDevProjectStore.isLoading}
											>
												Save
											</button>
										</div>
									</form>
								{/if}
							{/if}
						{/if}
						{#if $vibeDevProjectStore.error}
							<div class="vibe-project-error">{$vibeDevProjectStore.error}</div>
						{/if}
					</section>

					<section class="vibe-code-panel vibe-runs-panel" aria-labelledby="vibe-runs-heading">
						<div class="vibe-code-panel__head">
							<div>
								<h2 id="vibe-runs-heading">Runs</h2>
								<p>
									{runCounts.total} total · {runCounts.active} active · {runCounts.done} done
								</p>
							</div>
							<button type="button" class="vibe-mini-action" on:click={startFreshRun}>
								New run
							</button>
						</div>
						{#if vibeDevTasks.length === 0}
							<div class="vibe-empty">
								{$taskStore.isLoading ? 'Loading #vibedev runs…' : 'No #vibedev tasks yet.'}
							</div>
						{:else}
							<div class="vibe-run-list vibe-run-list--rail">
								{#each vibeDevTasks as task (task.id)}
									<article
										class="vibe-run-card vibe-run-row--{task.status}"
										class:vibe-run-card--active={task.id === activeVibeTask?.id}
										class:vibe-run-card--linked={activeRunChainIds.has(task.id) && task.id !== activeVibeTask?.id}
									>
										<button
											type="button"
											class="vibe-run-select"
											on:click={() => selectVibeRun(task.id)}
											aria-pressed={task.id === activeVibeTask?.id}
										>
											<div>
												<div class="vibe-run-title">{task.title}</div>
												<div class="vibe-run-detail">{taskStatusDetail(task)}</div>
											</div>
											<div class="vibe-run-meta">
												{#if task.id === activeVibeTask?.id}
													<span>selected</span>
												{:else if activeRunChainIds.has(task.id)}
													<span>linked</span>
												{/if}
												<span class="vibe-run-status">{taskStatusLabel(task)}</span>
												<span>{relativeTime(taskUpdatedAtMs(task))}</span>
											</div>
										</button>
										<a
											class="vibe-run-open"
											href={`/tasks?selected=${encodeURIComponent(task.id)}`}
										>Open task</a>
									</article>
								{/each}
							</div>
						{/if}
					</section>

					<section class="vibe-code-panel vibe-files-panel" aria-labelledby="vibe-files-heading">
						<div class="vibe-code-panel__head">
							<div>
								<h2 id="vibe-files-heading">Files</h2>
								<p>{visibleChangedFiles.length} pending change file{visibleChangedFiles.length === 1 ? '' : 's'}</p>
							</div>
						</div>
						{#if visibleChangedFiles.length === 0}
							<div class="vibe-empty">No pending changed files.</div>
						{:else}
							<div class="vibe-file-list">
								{#each visibleChangedFiles as file (file.path)}
									<div class="vibe-file-row" class:vibe-file-row--active={file.active}>
										<span class="vibe-file-row__path">{file.path}</span>
										<span class="vibe-file-row__stat">+{file.additions} / -{file.deletions}</span>
									</div>
								{/each}
							</div>
						{/if}
					</section>
				</aside>

				<section class="vibe-code-main" aria-label="VibeDev request and responses">
					<VibeComposer
						bind:value={vibePrompt}
						mode={vibeComposerMode}
						placeholder={vibePromptPlaceholder}
						profiles={$codingProfileStore.profiles}
						blockedProfiles={$codingProfileStore.blocked}
						selectedProfileId={$codingProfileStore.selected}
						selectedProfile={selectedCodingProfile ?? null}
						profileError={$codingProfileStore.error}
						supportsImages={selectedCodingProfileSupportsImages}
						stagedAttachments={vibeStagedAttachments}
						submitDisabled={vibeSubmitDisabled}
						submitBlocker={vibeSubmitBlocker}
						submitting={dispatchingCodingTask}
						uploading={vibeAttachmentUploading}
						preparingSession={vibeDevThreadLoading || $vibeDevProjectStore.isLoading}
						activeRun={vibeActiveRun}
						on:submit={() => void submitVibePrompt()}
						on:selectProfile={(event) => selectCodingProfile(event.detail.id)}
						on:attachFiles={(event) => void handleVibeAttachmentFiles(event)}
						on:removeAttachment={(event) => removeVibeAttachment(event.detail.attachmentId)}
						on:newRun={startFreshRun}
						on:micCapture={handleVibeMicCapture}
						on:micTranscribe={handleVibeMicTranscribe}
						on:micTranscribeDelta={handleVibeMicTranscribeDelta}
					/>

					<section class="vibe-code-panel vibe-session-panel" aria-labelledby="vibe-session-heading">
						<div class="vibe-code-panel__head">
							<div>
								<h2 id="vibe-session-heading">
									{activeVibeTask ? 'Active run' : 'New coding run'}
								</h2>
								<p>
									{activeVibeTask
										? `${taskStatusLabel(activeVibeTask)} · ${relativeTime(taskUpdatedAtMs(activeVibeTask))}`
										: activeVibeProject
											? `${activeVibeProject.name} · ready for the next request`
											: 'Preparing VibeDev project'}
								</p>
							</div>
							<a class="vibe-mini-action vibe-mini-action--link" href={vibeDevSessionHref}>
								#vibedev
							</a>
						</div>
						<div class="vibe-session-stats">
							<div>
								<strong>{activeRunChainIds.size}</strong>
								<span>{activeRunChainIds.size === 1 ? 'linked run' : 'linked runs'}</span>
							</div>
							<div>
								<strong>{activeActivityRows.length}</strong>
								<span>responses</span>
							</div>
							<div>
								<strong>{visibleChangedFiles.length}</strong>
								<span>files</span>
							</div>
							<div>
								<strong>{reviewPendingCount}</strong>
								<span>decisions</span>
							</div>
						</div>
					</section>

					<section class="vibe-code-panel" aria-labelledby="vibe-activity-heading">
						<div class="vibe-code-panel__head">
							<div>
								<h2 id="vibe-activity-heading">Agent responses</h2>
								<p>{activeActivityRows.length} recent event{activeActivityRows.length === 1 ? '' : 's'}</p>
							</div>
						</div>
						{#if activeActivityRows.length === 0}
							<div class="vibe-empty">
								{codingActivityEmptyText}
							</div>
						{:else}
							<div class="vibe-activity-list vibe-activity-list--wide">
								{#each activeActivityRows as row (row.key)}
									<article class="vibe-activity-row vibe-activity-row--{row.status}">
										<div class="vibe-activity-status" aria-hidden="true"></div>
										<div>
											<div class="vibe-activity-title">{row.label}</div>
											<div class="vibe-activity-meta">
												{#if row.profileLabel}
													<span>{row.profileLabel}</span>
												{/if}
												{#if row.taskId}
													<span>{row.taskId}</span>
												{/if}
												<span>{relativeTime(row.updatedAt)}</span>
											</div>
											{#if row.detail}
												<div class="vibe-activity-detail">{row.detail}</div>
											{/if}
										</div>
									</article>
								{/each}
							</div>
						{/if}
					</section>
				</section>

				<aside class="vibe-code-inspector" aria-label="VibeDev output surfaces">
					<div class="vibe-inspector-tabs" role="tablist" aria-label="VibeDev output tabs">
						<button
							type="button"
							role="tab"
							class:vibe-inspector-tab--active={workspaceTab === 'preview'}
							aria-selected={workspaceTab === 'preview'}
							on:click={() => (workspaceTab = 'preview')}
						>Preview</button>
						<button
							type="button"
							role="tab"
							class:vibe-inspector-tab--active={workspaceTab === 'logs'}
							aria-selected={workspaceTab === 'logs'}
							on:click={() => (workspaceTab = 'logs')}
						>Logs</button>
						<button
							type="button"
							role="tab"
							class:vibe-inspector-tab--active={workspaceTab === 'tests'}
							aria-selected={workspaceTab === 'tests'}
							on:click={() => (workspaceTab = 'tests')}
						>Tests</button>
					</div>
					<div class="vibe-inspector-body">
						{#if workspaceTab === 'preview'}
							<VibePreviewPanel projectId={activeVibeProject?.project_id} pinnedUrl={activeVibeProject?.preview_url ?? null} />
						{:else if workspaceTab === 'logs'}
							<section class="vibe-code-panel vibe-logs-panel" aria-labelledby="vibe-logs-heading">
								<div class="vibe-code-panel__head">
									<div>
										<h2 id="vibe-logs-heading">Run logs</h2>
										<p>
											{#if projectedCodingLogLoading && activeRunChainIds.size > 0}
												Loading projection…
											{:else}
												{activeRunChainIds.size > 0
													? `${activeLogRows.length} selected-run event${activeLogRows.length === 1 ? '' : 's'}`
													: `${activeLogRows.length} recent event${activeLogRows.length === 1 ? '' : 's'}`}
												{#if projectedLogSourceLabel}
													· {projectedLogSourceLabel}
												{/if}
											{/if}
										</p>
									</div>
								</div>
								{#if activeLogRows.length === 0}
									<div class="vibe-empty">{codingLogEmptyText()}</div>
								{:else}
									<div class="vibe-log-list" role="log" aria-label="Read-only VibeDev run log">
										{#each activeLogRows as row (row.key)}
											<article class="vibe-log-row vibe-log-row--{row.tone}">
												<div class="vibe-log-row__dot" aria-hidden="true"></div>
												<div class="vibe-log-row__body">
													<div class="vibe-log-row__head">
														<span>{row.label}</span>
														<time datetime={new Date(row.updatedAt).toISOString()}>
															{relativeTime(row.updatedAt)}
														</time>
													</div>
													<div class="vibe-log-row__meta">
														<span>{row.eventType}</span>
														{#if row.profileLabel}
															<span>{row.profileLabel}</span>
														{/if}
														{#if row.taskId}
															<span>{row.taskId}</span>
														{/if}
													</div>
													{#if row.detail}
														<p>{trimActivity(row.detail, 320)}</p>
													{/if}
												</div>
											</article>
										{/each}
									</div>
								{/if}
							</section>
						{:else}
							<section class="vibe-code-panel vibe-tests-panel" aria-labelledby="vibe-tests-heading">
								<div class="vibe-code-panel__head">
									<div>
										<h2 id="vibe-tests-heading">Tests</h2>
										<p>0 structured runs</p>
									</div>
								</div>
								<div class="vibe-empty">No structured test result for this run.</div>
							</section>
						{/if}
					</div>
				</aside>
			</main>

			<VibeReviewPanel
				open={reviewPanelOpen}
				{diffRows}
				{otherRows}
				{orderedDiffRows}
				{orderedOtherRows}
				{autoApplyCodeProposals}
				{bulkApplying}
				{actingKeys}
				on:hide={() => (reviewPanelCollapsed = true)}
				on:show={() => (reviewPanelCollapsed = false)}
				on:approveAll={() => void approveAll()}
				on:toggleAutoApply={(event) => toggleAutoApplyCodeProposals(event.detail.value)}
				on:applyDiff={(event) => void resolveDiff(event.detail.request, 'apply')}
				on:rejectDiff={(event) => void resolveDiff(event.detail.request, 'reject')}
				on:applyFile={(event) =>
					void applyDiffFile(event.detail.request, event.detail.path)}
				on:rejectFile={(event) =>
					void rejectDiffFile(event.detail.request, event.detail.path)}
				on:respond={(event) => void respond(event.detail.row)}
			/>
		</div>
	{/if}
</div>

<style>
	.vibe-page {
		--vibe-surface: var(--bg-card, #ffffff);
		--vibe-page-surface: var(--bg-surface, var(--bg-page, #f8f6f2));
		--vibe-border: var(--border-soft, #e5e2dc);
		--vibe-border-strong: var(--border-strong, var(--border-soft, #d8d2c8));
		--vibe-text: var(--text-primary, #2d2a26);
		--vibe-text-muted: var(--text-secondary, #6b6258);
		--vibe-accent: var(--accent-primary, #c2502a);
		--vibe-success: var(--color-success, #2f8f5b);
		--vibe-warning: var(--color-warning, #b7791f);
		--vibe-error: var(--color-error, #d23a3a);
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
		padding-top: 0.45rem;
		color: var(--vibe-text);
	}

	.vibe-page--workbench {
		max-width: none;
	}

	@media (min-width: 900px) {
		.vibe-page--workbench {
			width: min(calc(100vw - 1.5rem), 100%);
			max-width: none;
			margin-inline: auto;
			padding-inline: 0.75rem;
		}
	}

	.vibe-head {
		display: flex;
		align-items: center;
		justify-content: center;
		gap: 1rem;
		padding: 0;
		border-bottom: 0;
	}

	.vibe-head__actions {
		display: flex;
		align-items: center;
		gap: 0.65rem;
		flex-wrap: wrap;
		justify-content: center;
		width: 100%;
	}

	.vibe-tabs {
		display: inline-flex;
		align-items: center;
		gap: 0.15rem;
		padding: 0.15rem;
		border: 1px solid var(--vibe-border);
		border-radius: 7px;
		background: color-mix(in srgb, var(--vibe-page-surface) 72%, var(--vibe-surface));
	}

	.vibe-tabs button {
		min-height: 1.75rem;
		border: 0;
		border-radius: 5px;
		background: transparent;
		color: var(--vibe-text-muted);
		font: inherit;
		font-size: 0.72rem;
		font-weight: 800;
		padding: 0.28rem 0.52rem;
		cursor: pointer;
	}

	.vibe-tabs button:hover {
		color: var(--vibe-text);
		background: color-mix(in srgb, var(--vibe-surface) 82%, transparent);
	}

	.vibe-tabs button:focus-visible {
		outline: 2px solid color-mix(in srgb, var(--vibe-accent) 68%, transparent);
		outline-offset: 2px;
	}

	.vibe-tabs .vibe-tab--active {
		color: var(--button-primary-color, #fff);
		background: var(--vibe-accent);
		box-shadow: var(--shadow-sm, 0 1px 2px color-mix(in srgb, var(--vibe-text) 10%, transparent));
	}

	.vibe-status {
		border: 1px solid color-mix(in srgb, var(--vibe-success) 45%, transparent);
		border-radius: 8px;
		background: color-mix(in srgb, var(--vibe-success) 9%, var(--vibe-surface));
		padding: 0.65rem 0.75rem;
		font-size: 0.84rem;
		font-weight: 700;
	}

	.vibe-status--error {
		border-color: color-mix(in srgb, var(--vibe-error) 48%, transparent);
		background: color-mix(in srgb, var(--vibe-error) 8%, var(--vibe-surface));
	}

	.vibe-grid {
		display: grid;
		grid-template-columns: minmax(0, 1fr) minmax(17rem, 21rem);
		align-items: start;
		gap: 1rem;
	}

	.vibe-agents-shell {
		position: relative;
		display: block;
		min-width: 0;
	}

	.vibe-code-workspace {
		display: grid;
		grid-template-columns: minmax(14rem, 18rem) minmax(24rem, 1.2fr) minmax(20rem, 0.9fr);
		align-items: start;
		gap: 0.85rem;
		min-width: 0;
		min-height: calc(100vh - 7.2rem);
	}

	.vibe-code-rail,
	.vibe-code-main {
		min-width: 0;
		display: flex;
		flex-direction: column;
		gap: 0.85rem;
	}

	.vibe-code-rail {
		position: sticky;
		top: 0.65rem;
		max-height: calc(100vh - 6.6rem);
		overflow: auto;
		scrollbar-gutter: stable;
	}

	.vibe-code-main {
		min-height: 0;
	}

	.vibe-code-inspector {
		position: sticky;
		top: 0.65rem;
		min-width: 0;
		max-height: calc(100vh - 6.6rem);
		display: flex;
		flex-direction: column;
		border: 1px solid var(--vibe-border);
		border-radius: 8px;
		background: color-mix(in srgb, var(--vibe-surface) 97%, transparent);
		box-shadow: var(--shadow-sm, 0 1px 2px color-mix(in srgb, var(--vibe-text) 10%, transparent));
		overflow: hidden;
	}

	.vibe-code-panel {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
		min-width: 0;
		border: 1px solid var(--vibe-border);
		border-radius: 8px;
		background: color-mix(in srgb, var(--vibe-surface) 96%, transparent);
		padding: 0.85rem;
		box-shadow: var(--shadow-sm, 0 1px 2px color-mix(in srgb, var(--vibe-text) 10%, transparent));
	}

	.vibe-code-panel__head {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 0.75rem;
		min-width: 0;
	}

	.vibe-code-panel__head h2 {
		margin: 0;
		font-size: 0.92rem;
		line-height: 1.2;
		letter-spacing: 0;
	}

	.vibe-heading-count {
		color: var(--vibe-text-muted);
		font-weight: 760;
	}

	.vibe-code-panel__head p {
		margin: 0.25rem 0 0;
		color: var(--vibe-text-muted);
		font-size: 0.74rem;
		line-height: 1.35;
		overflow-wrap: anywhere;
	}

	.vibe-mini-action {
		min-height: 1.8rem;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		border: 1px solid var(--vibe-border-strong);
		border-radius: 7px;
		background: color-mix(in srgb, var(--vibe-page-surface) 72%, var(--vibe-surface));
		color: var(--vibe-text);
		font: inherit;
		font-size: 0.72rem;
		font-weight: 850;
		line-height: 1;
		padding: 0.32rem 0.55rem;
		text-decoration: none;
		white-space: nowrap;
		cursor: pointer;
	}

	.vibe-mini-action:hover {
		border-color: var(--vibe-accent);
		background: color-mix(in srgb, var(--vibe-accent) 8%, var(--vibe-surface));
	}

	.vibe-mini-action:disabled {
		opacity: 0.55;
		cursor: not-allowed;
	}

	.vibe-mini-action:focus-visible {
		outline: 2px solid color-mix(in srgb, var(--vibe-accent) 68%, transparent);
		outline-offset: 2px;
	}

	.vibe-mini-action--link {
		color: var(--vibe-accent);
	}

	.vibe-mini-action--danger {
		border-color: color-mix(in srgb, var(--vibe-error) 38%, var(--vibe-border-strong));
		color: var(--vibe-error);
	}

	.vibe-mini-action--danger:hover {
		border-color: var(--vibe-error);
		background: color-mix(in srgb, var(--vibe-error) 8%, var(--vibe-surface));
	}

	.vibe-project-panel {
		background: color-mix(in srgb, var(--vibe-page-surface) 64%, var(--vibe-surface));
	}

	.vibe-project-menu {
		position: relative;
		flex: 0 0 auto;
	}

	.vibe-project-menu__trigger {
		min-width: 4.7rem;
	}

	.vibe-project-menu__popover {
		position: absolute;
		z-index: 12;
		top: calc(100% + 0.35rem);
		right: 0;
		min-width: 9.5rem;
		display: flex;
		flex-direction: column;
		gap: 0.15rem;
		border: 1px solid var(--vibe-border-strong);
		border-radius: 8px;
		background: color-mix(in srgb, var(--vibe-surface) 98%, transparent);
		box-shadow: 0 12px 30px color-mix(in srgb, var(--vibe-text) 16%, transparent);
		padding: 0.3rem;
	}

	.vibe-project-menu__popover button {
		width: 100%;
		min-height: 1.85rem;
		display: flex;
		align-items: center;
		justify-content: flex-start;
		border: 0;
		border-radius: 6px;
		background: transparent;
		color: var(--vibe-text);
		font: inherit;
		font-size: 0.74rem;
		font-weight: 780;
		line-height: 1.1;
		padding: 0.35rem 0.45rem;
		text-align: left;
		cursor: pointer;
	}

	.vibe-project-menu__popover button:hover {
		background: color-mix(in srgb, var(--vibe-accent) 8%, var(--vibe-surface));
		color: var(--vibe-text);
	}

	.vibe-project-menu__popover button:disabled {
		opacity: 0.45;
		cursor: not-allowed;
	}

	.vibe-project-menu__popover button:focus-visible {
		outline: 2px solid color-mix(in srgb, var(--vibe-accent) 68%, transparent);
		outline-offset: 1px;
	}

	.vibe-project-menu__divider {
		height: 1px;
		margin: 0.2rem 0.1rem;
		background: var(--vibe-border);
	}

	.vibe-project-menu__popover .vibe-project-menu__danger {
		color: var(--vibe-error);
	}

	.vibe-project-menu__popover .vibe-project-menu__danger:hover {
		background: color-mix(in srgb, var(--vibe-error) 8%, var(--vibe-surface));
		color: var(--vibe-error);
	}

	.vibe-project-picker {
		display: grid;
		grid-template-columns: minmax(0, 1fr);
		gap: 0.45rem;
		align-items: center;
	}

	.vibe-project-picker--editing {
		grid-template-columns: minmax(0, 1fr) auto auto;
	}

	.vibe-project-name-input {
		min-width: 0;
		min-height: 2rem;
		border: 1px solid var(--vibe-border-strong);
		border-radius: 7px;
		background: color-mix(in srgb, var(--vibe-surface) 94%, transparent);
		color: var(--vibe-text);
		font: inherit;
		font-size: 0.78rem;
		font-weight: 750;
		line-height: 1.2;
		padding: 0.35rem 0.5rem;
	}

	.vibe-project-name-input:disabled {
		opacity: 0.6;
		cursor: wait;
	}

	.vibe-project-name-input:focus-visible {
		outline: 2px solid color-mix(in srgb, var(--vibe-accent) 68%, transparent);
		outline-offset: 2px;
	}

	.vibe-project-select {
		width: 100%;
		min-height: 2rem;
		border: 1px solid var(--vibe-border-strong);
		border-radius: 7px;
		background: color-mix(in srgb, var(--vibe-surface) 94%, transparent);
		color: var(--vibe-text);
		font: inherit;
		font-size: 0.78rem;
		font-weight: 800;
		line-height: 1.2;
		padding: 0.35rem 0.5rem;
	}

	.vibe-project-select:disabled {
		opacity: 0.6;
		cursor: wait;
	}

	.vibe-project-select:focus-visible {
		outline: 2px solid color-mix(in srgb, var(--vibe-accent) 68%, transparent);
		outline-offset: 2px;
	}

	.vibe-project-meta {
		display: grid;
		grid-template-columns: auto minmax(0, 1fr);
		gap: 0.45rem;
		align-items: center;
		color: var(--vibe-text-muted);
		font-size: 0.7rem;
		line-height: 1.25;
	}

	.vibe-project-meta strong {
		color: var(--vibe-text-muted);
		font-size: 0.7rem;
		font-weight: 850;
	}

	.vibe-project-meta a {
		color: var(--vibe-accent);
		font-weight: 850;
		text-decoration: none;
	}

	.vibe-project-meta a:hover {
		text-decoration: underline;
	}

	.vibe-project-meta span {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-family: var(--font-mono, ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace);
	}

	.vibe-project-settings {
		display: flex;
		flex-direction: column;
		gap: 0.55rem;
		border: 1px solid var(--vibe-border);
		border-radius: 8px;
		background: color-mix(in srgb, var(--vibe-surface) 88%, transparent);
		padding: 0.6rem;
	}

	.vibe-project-settings label {
		display: flex;
		flex-direction: column;
		gap: 0.3rem;
		min-width: 0;
	}

	.vibe-project-settings__row {
		display: grid;
		grid-template-columns: minmax(0, 1fr) auto;
		gap: 0.45rem;
		align-items: end;
	}

	.vibe-project-settings label span {
		color: var(--vibe-text-muted);
		font-size: 0.68rem;
		font-weight: 850;
		line-height: 1.2;
		text-transform: uppercase;
	}

	.vibe-project-settings input {
		width: 100%;
		min-width: 0;
		min-height: 2rem;
		border: 1px solid var(--vibe-border-strong);
		border-radius: 7px;
		background: color-mix(in srgb, var(--vibe-page-surface) 88%, var(--vibe-surface));
		color: var(--vibe-text);
		font: inherit;
		font-size: 0.76rem;
		font-weight: 760;
		line-height: 1.2;
		padding: 0.35rem 0.5rem;
	}

	.vibe-project-settings input:disabled {
		opacity: 0.6;
		cursor: wait;
	}

	.vibe-project-settings input:focus-visible {
		outline: 2px solid color-mix(in srgb, var(--vibe-accent) 68%, transparent);
		outline-offset: 2px;
	}

	.vibe-project-settings__hint {
		margin: -0.2rem 0 0;
		color: var(--vibe-text-muted);
		font-family: var(--font-mono, ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace);
		font-size: 0.7rem;
		font-weight: 760;
		line-height: 1.35;
		overflow-wrap: anywhere;
	}

	.vibe-project-settings__actions {
		display: flex;
		justify-content: flex-end;
		gap: 0.35rem;
		flex-wrap: wrap;
	}

	.vibe-project-dir-picker {
		display: flex;
		flex-direction: column;
		gap: 0.45rem;
		border: 1px solid var(--vibe-border-strong);
		border-radius: 8px;
		background: color-mix(in srgb, var(--vibe-surface) 94%, transparent);
		padding: 0.5rem;
		box-shadow: var(--shadow-sm, 0 1px 2px color-mix(in srgb, var(--vibe-text) 10%, transparent));
	}

	.vibe-project-dir-picker__head {
		display: grid;
		grid-template-columns: minmax(0, 1fr) auto;
		align-items: center;
		gap: 0.35rem;
		color: var(--vibe-text);
		font-size: 0.72rem;
		font-weight: 850;
		line-height: 1.25;
	}

	.vibe-project-dir-picker__head span {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-family: var(--font-mono, ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace);
	}

	.vibe-project-dir-picker__head button,
	.vibe-project-dir-picker__actions button,
	.vibe-project-dir-picker__list button {
		border: 1px solid var(--vibe-border);
		border-radius: 6px;
		background: color-mix(in srgb, var(--vibe-page-surface) 72%, var(--vibe-surface));
		color: var(--vibe-text);
		font: inherit;
		font-size: 0.7rem;
		font-weight: 800;
		line-height: 1.1;
		cursor: pointer;
	}

	.vibe-project-dir-picker__head button {
		width: 1.6rem;
		height: 1.6rem;
		padding: 0;
	}

	.vibe-project-dir-picker__actions {
		display: flex;
		gap: 0.3rem;
		flex-wrap: wrap;
	}

	.vibe-project-dir-picker__actions button {
		min-height: 1.55rem;
		padding: 0.25rem 0.42rem;
	}

	.vibe-project-dir-picker__head button:hover,
	.vibe-project-dir-picker__actions button:hover:not(:disabled),
	.vibe-project-dir-picker__list button:hover {
		border-color: var(--vibe-accent);
		background: color-mix(in srgb, var(--vibe-accent) 8%, var(--vibe-surface));
	}

	.vibe-project-dir-picker__actions button:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}

	.vibe-project-dir-picker__list {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
		max-height: 10rem;
		overflow: auto;
		scrollbar-gutter: stable;
	}

	.vibe-project-dir-picker__list button {
		width: 100%;
		min-height: 1.7rem;
		display: flex;
		align-items: center;
		justify-content: flex-start;
		padding: 0.3rem 0.45rem;
		text-align: left;
	}

	.vibe-project-dir-picker__list span {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.vibe-project-dir-picker__empty {
		border: 1px dashed var(--vibe-border);
		border-radius: 7px;
		color: var(--vibe-text-muted);
		font-size: 0.72rem;
		font-weight: 750;
		line-height: 1.35;
		padding: 0.45rem;
	}

	.vibe-project-dir-picker__error {
		border-color: color-mix(in srgb, var(--vibe-error) 45%, transparent);
		color: var(--vibe-error);
	}

	.vibe-project-error {
		border: 1px solid color-mix(in srgb, var(--vibe-error) 45%, transparent);
		border-radius: 7px;
		background: color-mix(in srgb, var(--vibe-error) 7%, var(--vibe-surface));
		color: var(--vibe-error);
		padding: 0.45rem 0.5rem;
		font-size: 0.74rem;
		font-weight: 750;
		line-height: 1.35;
		overflow-wrap: anywhere;
	}

	.vibe-file-list {
		display: flex;
		flex-direction: column;
		gap: 0.35rem;
	}

	.vibe-file-row {
		display: grid;
		grid-template-columns: minmax(0, 1fr) auto;
		align-items: center;
		gap: 0.55rem;
		border: 1px solid var(--vibe-border);
		border-radius: 7px;
		background: color-mix(in srgb, var(--vibe-page-surface) 72%, var(--vibe-surface));
		padding: 0.45rem 0.5rem;
	}

	.vibe-file-row--active {
		border-color: color-mix(in srgb, var(--vibe-accent) 52%, var(--vibe-border));
		background: color-mix(in srgb, var(--vibe-accent) 7%, var(--vibe-surface));
	}

	.vibe-file-row__path {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font: 0.76rem/1.3 var(--font-mono, ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace);
	}

	.vibe-file-row__stat {
		color: var(--vibe-text-muted);
		font-size: 0.7rem;
		font-weight: 850;
		white-space: nowrap;
	}

	.vibe-session-panel {
		background: color-mix(in srgb, var(--vibe-page-surface) 58%, var(--vibe-surface));
	}

	.vibe-session-stats {
		display: grid;
		grid-template-columns: repeat(4, minmax(0, 1fr));
		gap: 0.5rem;
	}

	.vibe-session-stats div {
		display: flex;
		flex-direction: column;
		gap: 0.15rem;
		border: 1px solid var(--vibe-border);
		border-radius: 7px;
		background: color-mix(in srgb, var(--vibe-surface) 82%, transparent);
		padding: 0.5rem;
		min-width: 0;
	}

	.vibe-session-stats strong {
		font-size: 1rem;
		line-height: 1;
	}

	.vibe-session-stats span {
		color: var(--vibe-text-muted);
		font-size: 0.68rem;
		font-weight: 800;
		text-transform: uppercase;
		white-space: nowrap;
	}

	.vibe-inspector-tabs {
		display: grid;
		grid-template-columns: repeat(3, minmax(0, 1fr));
		gap: 0.2rem;
		padding: 0.35rem;
		border-bottom: 1px solid var(--vibe-border);
		background: color-mix(in srgb, var(--vibe-page-surface) 75%, var(--vibe-surface));
	}

	.vibe-inspector-tabs button {
		min-height: 1.9rem;
		border: 0;
		border-radius: 6px;
		background: transparent;
		color: var(--vibe-text-muted);
		font: inherit;
		font-size: 0.72rem;
		font-weight: 850;
		cursor: pointer;
	}

	.vibe-inspector-tabs button:hover {
		color: var(--vibe-text);
		background: color-mix(in srgb, var(--vibe-surface) 82%, transparent);
	}

	.vibe-inspector-tabs button:focus-visible {
		outline: 2px solid color-mix(in srgb, var(--vibe-accent) 68%, transparent);
		outline-offset: 2px;
	}

	.vibe-inspector-tabs .vibe-inspector-tab--active {
		background: var(--vibe-accent);
		color: var(--button-primary-color, #fff);
	}

	.vibe-inspector-body {
		min-height: 0;
		flex: 0 1 auto;
		overflow: auto;
		padding: 0.75rem;
	}

	.vibe-inspector-body :global(.vibe-preview) {
		box-shadow: none;
	}

	.vibe-log-list {
		display: flex;
		flex-direction: column;
		gap: 0.45rem;
		max-height: min(34rem, calc(100vh - 13rem));
		overflow: auto;
		padding-right: 0.15rem;
	}

	.vibe-log-row {
		display: grid;
		grid-template-columns: 0.55rem minmax(0, 1fr);
		gap: 0.5rem;
		align-items: start;
		border: 1px solid var(--vibe-border);
		border-radius: 8px;
		background: color-mix(in srgb, var(--vibe-page-surface) 70%, var(--vibe-surface));
		padding: 0.6rem;
		min-width: 0;
	}

	.vibe-log-row__dot {
		width: 0.5rem;
		height: 0.5rem;
		border-radius: 999px;
		margin-top: 0.38rem;
		background: var(--vibe-text-muted);
	}

	.vibe-log-row--running .vibe-log-row__dot {
		background: var(--vibe-accent);
	}

	.vibe-log-row--waiting .vibe-log-row__dot {
		background: var(--vibe-warning);
	}

	.vibe-log-row--done .vibe-log-row__dot {
		background: var(--vibe-success);
	}

	.vibe-log-row--failed .vibe-log-row__dot {
		background: var(--vibe-error);
	}

	.vibe-log-row__body {
		min-width: 0;
	}

	.vibe-log-row__head {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		gap: 0.75rem;
		min-width: 0;
	}

	.vibe-log-row__head span {
		min-width: 0;
		font-size: 0.82rem;
		font-weight: 850;
		line-height: 1.25;
		overflow-wrap: anywhere;
	}

	.vibe-log-row__head time {
		color: var(--vibe-text-muted);
		font-size: 0.68rem;
		font-weight: 800;
		white-space: nowrap;
	}

	.vibe-log-row__meta {
		display: flex;
		flex-wrap: wrap;
		gap: 0.35rem;
		margin-top: 0.25rem;
		color: var(--vibe-text-muted);
		font: 0.68rem/1.3 var(--font-mono, ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace);
	}

	.vibe-log-row p {
		margin: 0.4rem 0 0;
		color: var(--vibe-text-muted);
		font-size: 0.78rem;
		line-height: 1.38;
		overflow-wrap: anywhere;
	}

	.vibe-workbench {
		width: 100%;
		height: calc(100vh - 8.7rem);
		min-height: 34rem;
		display: flex;
		flex-direction: column;
		min-width: 0;
	}

	.vibe-workbench :global(> *) {
		flex: 1;
		min-height: 0;
	}

	.vibe-side {
		position: sticky;
		top: 0.75rem;
		display: flex;
		flex-direction: column;
		gap: 0.8rem;
		min-width: 0;
	}

	.vibe-panel {
		display: flex;
		flex-direction: column;
		gap: 0.7rem;
		border: 1px solid var(--vibe-border);
		border-radius: 8px;
		background: color-mix(in srgb, var(--vibe-surface) 96%, transparent);
		padding: 0.85rem;
		box-shadow: var(--shadow-sm, 0 1px 2px color-mix(in srgb, var(--vibe-text) 10%, transparent));
	}

	.vibe-empty {
		border: 1px dashed var(--vibe-border);
		border-radius: 8px;
		padding: 1rem;
		color: var(--vibe-text-muted);
		font-size: 0.88rem;
		background: color-mix(in srgb, var(--vibe-page-surface) 72%, transparent);
	}

	.vibe-activity-meta {
		display: flex;
		flex-wrap: wrap;
		gap: 0.4rem;
		color: var(--vibe-text-muted);
		font-size: 0.74rem;
	}

	.vibe-activity-detail {
		margin: 0;
		color: var(--vibe-text-muted);
		font-size: 0.8rem;
		line-height: 1.35;
		overflow-wrap: anywhere;
	}

	.vibe-activity-list {
		display: flex;
		flex-direction: column;
		gap: 0.55rem;
	}

	.vibe-activity-list--wide {
		gap: 0.7rem;
	}

	.vibe-run-list {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(min(22rem, 100%), 1fr));
		gap: 0.7rem;
	}

	.vibe-run-list--rail {
		display: flex;
		flex-direction: column;
		gap: 0.55rem;
	}

	.vibe-run-card {
		display: flex;
		flex-direction: column;
		gap: 0.55rem;
		border: 1px solid var(--vibe-border);
		border-radius: 8px;
		background: color-mix(in srgb, var(--vibe-page-surface) 72%, var(--vibe-surface));
		padding: 0.55rem;
	}

	.vibe-run-card--active {
		border-color: color-mix(in srgb, var(--vibe-accent) 70%, var(--vibe-border));
		background: color-mix(in srgb, var(--vibe-accent) 8%, var(--vibe-surface));
		box-shadow: 0 0 0 2px color-mix(in srgb, var(--vibe-accent) 12%, transparent);
	}

	.vibe-run-card--linked {
		border-color: color-mix(in srgb, var(--vibe-accent) 34%, var(--vibe-border));
	}

	.vibe-run-select {
		width: 100%;
		display: grid;
		grid-template-columns: minmax(0, 1fr) auto;
		gap: 0.8rem;
		align-items: start;
		border: 0;
		background: transparent;
		color: inherit;
		padding: 0.1rem;
		text-align: left;
		font: inherit;
		cursor: pointer;
	}

	.vibe-run-list--rail .vibe-run-select {
		grid-template-columns: minmax(0, 1fr);
		gap: 0.5rem;
	}

	.vibe-run-list--rail .vibe-run-meta {
		align-items: flex-start;
		flex-direction: row;
		flex-wrap: wrap;
	}

	.vibe-run-card:hover {
		border-color: var(--vibe-accent);
		background: color-mix(in srgb, var(--vibe-accent) 5%, var(--vibe-surface));
	}

	.vibe-run-select:focus-visible,
	.vibe-run-open:focus-visible {
		outline: 2px solid color-mix(in srgb, var(--vibe-accent) 68%, transparent);
		outline-offset: 2px;
	}

	.vibe-run-open {
		align-self: flex-start;
		border-radius: 6px;
		color: var(--vibe-accent);
		font-size: 0.74rem;
		font-weight: 850;
		text-decoration: none;
	}

	.vibe-run-open:hover {
		text-decoration: underline;
	}

	.vibe-run-title {
		font-size: 0.88rem;
		font-weight: 850;
		line-height: 1.25;
		overflow-wrap: anywhere;
	}

	.vibe-run-detail {
		margin-top: 0.25rem;
		color: var(--vibe-text-muted);
		font-size: 0.78rem;
		line-height: 1.35;
		overflow-wrap: anywhere;
	}

	.vibe-run-meta {
		display: flex;
		flex-direction: column;
		align-items: flex-end;
		gap: 0.25rem;
		color: var(--vibe-text-muted);
		font-size: 0.72rem;
		font-weight: 800;
		text-transform: uppercase;
		white-space: nowrap;
	}

	.vibe-run-row--running .vibe-run-status,
	.vibe-run-row--planning .vibe-run-status {
		color: var(--vibe-accent);
	}

	.vibe-run-row--completed .vibe-run-status {
		color: var(--vibe-success);
	}

	.vibe-run-row--failed .vibe-run-status,
	.vibe-run-row--cancelled .vibe-run-status {
		color: var(--vibe-error);
	}

	.vibe-activity-row {
		display: grid;
		grid-template-columns: 0.6rem minmax(0, 1fr);
		gap: 0.55rem;
		align-items: start;
		border: 1px solid var(--vibe-border);
		border-radius: 8px;
		background: color-mix(in srgb, var(--vibe-page-surface) 72%, var(--vibe-surface));
		padding: 0.65rem;
	}

	.vibe-activity-status {
		width: 0.55rem;
		height: 0.55rem;
		border-radius: 999px;
		margin-top: 0.32rem;
		background: var(--text-muted, var(--vibe-text-muted));
	}

	.vibe-activity-row--running .vibe-activity-status {
		background: var(--vibe-accent);
	}

	.vibe-activity-row--waiting .vibe-activity-status {
		background: var(--vibe-warning);
	}

	.vibe-activity-row--done .vibe-activity-status {
		background: var(--vibe-success);
	}

	.vibe-activity-row--failed .vibe-activity-status {
		background: var(--vibe-error);
	}

	.vibe-activity-title {
		font-size: 0.85rem;
		font-weight: 800;
		line-height: 1.25;
		overflow-wrap: anywhere;
	}

	@media (max-width: 720px) {
		.vibe-agents-shell,
		.vibe-agents-shell--review-open,
		.vibe-agents-shell--review-collapsed {
			display: flex;
			flex-direction: column;
			align-items: stretch;
		}

		.vibe-code-workspace {
			display: flex;
			flex-direction: column;
			min-height: 0;
		}

		.vibe-code-rail,
		.vibe-code-inspector {
			position: static;
			height: auto;
			min-height: 0;
			max-height: none;
			overflow: visible;
		}

		.vibe-code-main {
			width: 100%;
		}

		.vibe-session-stats {
			grid-template-columns: repeat(2, minmax(0, 1fr));
		}

		.vibe-side {
			position: static;
			width: 100%;
			max-height: none;
		}

		.vibe-head {
			align-items: stretch;
			flex-direction: column;
		}

		.vibe-run-select {
			grid-template-columns: minmax(0, 1fr);
		}

		.vibe-run-meta {
			align-items: flex-start;
			flex-direction: row;
			flex-wrap: wrap;
		}

		.vibe-head__actions,
		.vibe-tabs {
			width: 100%;
		}

		.vibe-tabs button {
			flex: 1;
		}

		.vibe-workbench {
			height: calc(100vh - 12rem);
			min-height: 28rem;
		}
	}
</style>
