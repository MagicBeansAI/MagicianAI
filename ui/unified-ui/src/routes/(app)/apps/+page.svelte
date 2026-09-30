<script lang="ts">
	import AppMemoryAccessPanel from '$lib/apps/AppMemoryAccessPanel.svelte';
	import { goto } from '$app/navigation';
	import { onDestroy, onMount } from 'svelte';
	import ServerPager from '$lib/shared/components/ServerPager.svelte';
	import AppContributionStatePanel from '$lib/apps/AppContributionStatePanel.svelte';
	import AppDataCleanup from '$lib/apps/AppDataCleanup.svelte';
	import AppInteractiveRunPanel from '$lib/apps/AppInteractiveRunPanel.svelte';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import {
		appActionSubmissionIsCurrent,
		clearAppActionLaunchIntent,
		getAppActionRunStorage,
		launchIntentInput,
		loadAppActionLaunchIntent,
		loadAppActionRunHistory,
		rememberAppActionRun,
		restoreAppActionRun,
		stageAppActionLaunchIntent,
		type PersistedAppActionLaunchIntent,
		type PersistedAppActionRun
	} from '$lib/apps/actionRunHistory';
	import {
		appRequestRetryDelayMs,
		cancelAppActionRun,
		fetchAppActionContract,
		fetchAppActionRun,
		fetchAppDirectory,
		isRetryableAppRequestError,
		launchAppAction,
		recordAppDirectoryLaunch,
		setAppDirectoryActionPin,
		setAppDirectoryPin,
		type AppActionRun,
		type AppDirectoryAction,
		type AppDirectoryEntry,
		type AppDirectorySection,
		type AppDirectActionContract
	} from '$lib/apps/appDirectory';
	// Enabling, disabling or removing an app changes what first-party
	// navigation the shell may mount (gate S3). This page is the only surface
	// that changes it deliberately, so it drives the shared poller instead of
	// leaving the TopBar to notice minutes later.
	import {
		pollAppNavigationNow,
		requestFastAppNavigationPolling
	} from '$lib/stores/appNavigationStore';
	import {
		appLifecycleControls,
		appPackageExportAvailability,
		approveAppDataImport,
		backupAppUpdate,
		commitAppDataImport,
		commitAppDataRewind,
		commitAppReenable,
		commitAppPurge,
		exportAppDataArchive,
		exportAppPackage,
		fetchAppReenableReview,
		importAppPackage,
		prepareAppUpdatePlan,
		previewAppDataRewind,
		publishAppCandidate,
		previewAppPurge,
		previewAppDataImport,
		recoverRetainedAppLifecycleIntents,
		recoverRetainedAppDataImportCommits,
		rollbackCodeAppUpdate,
		runAppLifecycleOperation,
		type AppLifecycleControl,
		type AppDataImportApprovalReceipt,
		type AppDataImportPreviewReceipt,
		type AppPackageImportReceipt,
		type AppDataRewindPreviewReceipt,
		type AppReenableReviewIdentity,
		type AppPurgePreview,
		type AppUpdatePlanReceipt
	} from '$lib/apps/appLifecycle';
	import {
		approveAppInstallation,
		appInstallationReviewMatrix,
		defaultGrantedNames,
		toggleMemoryGrant,
		toggleSecretUseGrant,
		toggleAnyPublicHostGrant,
		secretReach,
		setSecretUseScope,
		appNamedHosts,
		fetchAppInstallationReview,
		grantDisplayName,
		inertWorkflowsForGrant,
		type AppInstallationApproveRequest,
		type AppInstallationReview
	} from '$lib/apps/installationReview';
	import AppCustomSurfaceReview from '$lib/apps/AppCustomSurfaceReview.svelte';

	const sections: Array<{ id: AppDirectorySection; label: string }> = [
		{ id: 'installed', label: 'Installed' },
		{ id: 'pinned', label: 'Pinned' },
		{ id: 'recent', label: 'Recently used' },
		{ id: 'needs_attention', label: 'Needs attention' },
		{ id: 'disabled', label: 'Disabled' },
		{ id: 'recovery', label: 'Retained / recovery' }
	];
	const PAGE_SIZE_OPTIONS = [12, 24, 48];
	const MAX_ACTION_POLL_FAILURES = 5;
	const MAX_ACTION_POLL_DELAY_MS = 30_000;
	const MAX_ACTION_FIELDS = 128;
	const MAX_ACTION_TEXT_BYTES = 64 * 1024;
	const MAX_ACTION_RENDER_BYTES = 20_000;
	const MAX_ACTION_RENDER_NODES = 512;
	const MAX_ACTION_RENDER_DEPTH = 16;

	let entries: AppDirectoryEntry[] = [];
	/** Which rows are expanded, keyed by installation id. Collapsed is the
	 * default: the directory is a scannable list first, and the review panel
	 * plus lifecycle controls are the detail you open one row at a time. */
	let rowExpanded: Record<string, boolean> = {};

	function toggleRow(installationId: string): void {
		rowExpanded = { ...rowExpanded, [installationId]: !rowExpanded[installationId] };
	}

	/** Human-readable record-store size for the compact row meta line. */
	function formatBytes(bytes: number): string {
		if (bytes < 1024) return `${bytes} B`;
		if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(bytes < 10_240 ? 1 : 0)} KB`;
		return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
	}

	let section: AppDirectorySection = 'installed';
	let searchDraft = '';
	let search = '';
	let pageSize = PAGE_SIZE_OPTIONS[0];
	let page = 1;
	let cursors: Array<string | undefined> = [undefined];
	let hasMore = false;
	let loading = true;
	let error = '';
	let request: AbortController | null = null;
	let pinning = '';
	let reviewing = '';
	let approving = '';
	let reviewRequest: AbortController | null = null;
	let approvalRequest: AbortController | null = null;
	let reviewGeneration = 0;
	let actionRequest: AbortController | null = null;
	let actionEntry: AppDirectoryEntry | null = null;
	let actionDefinition: AppDirectoryAction | null = null;
	let actionContract: AppDirectActionContract | null = null;
	let actionValues: Record<string, string | boolean> = {};
	let actionNulls: Record<string, boolean> = {};
	let actionLoading = false;
	let actionSubmitting = false;
	let actionSubmissionGeneration = 0;
	let actionMessage = '';
	let actionIdempotencyKey = '';
	let actionRun: AppActionRun | null = null;
	let actionPollRequest: AbortController | null = null;
	let actionPollTimer: ReturnType<typeof setTimeout> | null = null;
	let actionPollFailures = 0;
	let actionCancelling = false;
	let recentActionRuns: PersistedAppActionRun[] = [];
	let pendingLaunchIntent: PersistedAppActionLaunchIntent | null = null;
	let copiedRunRef = '';
	let lifecycleBusy = '';
	let lifecycleMessage = '';
	let purgePreview: { entry: AppDirectoryEntry; preview: AppPurgePreview } | null = null;
	let reenableReview: { entry: AppDirectoryEntry; review: AppReenableReviewIdentity } | null = null;
	let purgeRequest: AbortController | null = null;
	let importBusy = false;
	let candidatePublishBusy = false;
	let candidateTarget = 'initial';
	let packageInput: HTMLInputElement | null = null;
	let stagedPackage: {
		archive: File;
		receipt: AppPackageImportReceipt;
		requestId: string;
	} | null = null;
	let dataArchiveInput: HTMLInputElement | null = null;
	let dataImportTarget: AppDirectoryEntry | null = null;
	let dataImportBusy = false;
	let dataImportReview: {
		entry: AppDirectoryEntry;
		preview: AppDataImportPreviewReceipt;
		approval: AppDataImportApprovalReceipt | null;
		approvalRequestId: string;
		commitRequestId: string;
	} | null = null;
	let updatePlanByInstall: Record<string, AppUpdatePlanReceipt> = {};
	let dataRewindReview: {
		entry: AppDirectoryEntry;
		plan: AppUpdatePlanReceipt;
		preview: AppDataRewindPreviewReceipt;
	} | null = null;
	let reviewByInstall: Record<string, AppInstallationReview> = {};
	let grantByInstall: Record<string, AppInstallationApproveRequest> = {};
	let mounted = false;
	let appliedScope = '';
	$: scopeKey = JSON.stringify([
		$scopeIdentityStore.principal,
		$scopeIdentityStore.workspace
	]);
	$: if (mounted && scopeKey !== appliedScope) {
		abandonReviewState();
		abandonActionState();
		purgeRequest?.abort();
		purgeRequest = null;
		purgePreview = null;
		reenableReview = null;
		dataRewindReview = null;
		updatePlanByInstall = {};
		candidateTarget = 'initial';
		appliedScope = scopeKey;
		entries = [];
		void load(1, true, scopeKey);
	}

	$: pageCount = page + (hasMore ? 1 : 0);
	$: startItem = entries.length === 0 ? 0 : (page - 1) * pageSize + 1;
	$: endItem = entries.length === 0 ? 0 : startItem + entries.length - 1;

	async function load(
		targetPage: number,
		reset = false,
		expectedScope = scopeKey,
		recoverLifecycle = true
	): Promise<void> {
		request?.abort();
		const controller = new AbortController();
		request = controller;
		if (reset) {
			cursors = [undefined];
			targetPage = 1;
		}
		loading = true;
		error = '';
		try {
			const result = await fetchAppDirectory({
				section,
				search,
				limit: pageSize,
				cursor: cursors[targetPage - 1],
				signal: controller.signal
			});
			if (controller.signal.aborted || expectedScope !== scopeKey) return;
			if (recoverLifecycle) {
				const recovery = await recoverRetainedAppLifecycleIntents(controller.signal);
				if (controller.signal.aborted || expectedScope !== scopeKey) return;
				if (recovery.errors.length > 0) {
					lifecycleMessage = recovery.errors[0] ?? 'A retained lifecycle request still needs recovery.';
				}
				if (recovery.receipts.length > 0) {
					lifecycleMessage = `Recovered ${recovery.receipts.length} retained lifecycle ${recovery.receipts.length === 1 ? 'request' : 'requests'} from durable server receipts.`;
					await load(targetPage, reset, expectedScope, false);
					return;
				}
				const importRecovery = await recoverRetainedAppDataImportCommits(controller.signal);
				if (controller.signal.aborted || expectedScope !== scopeKey) return;
				if (importRecovery.errors.length > 0) {
					lifecycleMessage = importRecovery.errors[0] ?? 'A retained data-import commit still needs recovery.';
				}
				if (importRecovery.receipts.length > 0) {
					lifecycleMessage = `Recovered ${importRecovery.receipts.length} reviewed data-import ${importRecovery.receipts.length === 1 ? 'commit' : 'commits'} from durable receipts.`;
					await load(targetPage, reset, expectedScope, false);
					return;
				}
			}
			entries = result.entries;
			hasMore = result.has_more;
			page = targetPage;
			// Every lifecycle mutation on this page ends in a `load`, so this one
			// call covers approve, enable, disable, revoke, re-enable and purge
			// without a hook at each site. It costs one extra directory read per
			// load on this admin surface, which is the price of the shell's
			// navigation never lagging a change the owner just made here.
			pollAppNavigationNow();
			cursors = result.next_cursor
				? [...cursors.slice(0, targetPage), result.next_cursor]
				: cursors.slice(0, targetPage);
		} catch (cause) {
			if (!controller.signal.aborted && expectedScope === scopeKey) {
				error = cause instanceof Error ? cause.message : 'The Apps directory could not be loaded.';
				entries = [];
				hasMore = false;
			}
		} finally {
			if (request === controller) {
				request = null;
				loading = false;
			}
		}
	}

	async function chooseSection(next: AppDirectorySection): Promise<void> {
		if (next === section || loading) return;
		section = next;
		await load(1, true);
	}

	async function submitSearch(): Promise<void> {
		const next = searchDraft.trim();
		if (next === search && page === 1) return;
		search = next;
		await load(1, true);
	}

	async function changePageSize(next: number): Promise<void> {
		if (!PAGE_SIZE_OPTIONS.includes(next) || next === pageSize || loading) return;
		pageSize = next;
		await load(1, true);
	}

	async function launch(entry: AppDirectoryEntry, route?: string, viewId?: string): Promise<void> {
		const destination = route ?? entry.default_route;
		if (!destination || entry.status !== 'enabled') return;
		if (viewId) void recordAppDirectoryLaunch(entry.installation_id, viewId).catch(() => undefined);
		await goto(destination);
	}

	function statusMayHaveReview(status: AppDirectoryEntry['status']): boolean {
		return status === 'ready_for_review' || status === 'update_pending' || status === 'uninstalled_retained';
	}

	function abandonReviewState(): void {
		reviewGeneration += 1;
		reviewRequest?.abort();
		approvalRequest?.abort();
		reviewRequest = null;
		approvalRequest = null;
		reviewing = '';
		approving = '';
		reviewByInstall = {};
		grantByInstall = {};
	}

	async function prepareReviewedUpdate(
		entry: AppDirectoryEntry,
		review: AppInstallationReview,
		promptForOperations = false,
		signal?: AbortSignal
	): Promise<AppUpdatePlanReceipt | null> {
		if (review.attempt_kind === 'initial_install') return null;
		let operations: Array<Record<string, unknown>> = [];
		if (promptForOperations) {
			const source = window.prompt(
				'Enter the exact migration operation array as JSON. Use [] to infer the unique safe plan.',
				JSON.stringify(updatePlanByInstall[entry.installation_id]?.migration_operations ?? [], null, 2)
			);
			if (source === null) return null;
			let parsed: unknown;
			try { parsed = JSON.parse(source); } catch { throw new Error('Migration operations must be valid JSON.'); }
			if (!Array.isArray(parsed) || parsed.length > 256 ||
				parsed.some((operation) => operation === null || typeof operation !== 'object' || Array.isArray(operation))) {
				throw new Error('Migration operations must be a bounded JSON object array.');
			}
			operations = parsed as Array<Record<string, unknown>>;
		}
		const plan = await prepareAppUpdatePlan(entry, review.attempt_id, operations, signal);
		if (plan.destination_package_revision_ref !== review.package_revision_ref) {
			throw new Error('The update plan does not target the displayed package review.');
		}
		updatePlanByInstall = { ...updatePlanByInstall, [entry.installation_id]: plan };
		return plan;
	}

	async function promptForReviewedUpdatePlan(entry: AppDirectoryEntry): Promise<void> {
		const review = reviewByInstall[entry.installation_id];
		if (!review || approving || reviewing) return;
		reviewing = entry.installation_id;
		lifecycleMessage = '';
		try {
			const plan = await prepareReviewedUpdate(entry, review, true);
			if (plan) lifecycleMessage = `Prepared exact update plan ${plan.migration_run_id}; ${plan.dry_run_representable}/${plan.dry_run_examined} records passed the bounded dry-run.`;
		} catch (cause) {
			lifecycleMessage = cause instanceof Error ? cause.message : 'The exact update plan could not be prepared.';
		} finally {
			reviewing = '';
		}
	}

	async function openReview(entry: AppDirectoryEntry): Promise<void> {
		if (!statusMayHaveReview(entry.status) || reviewing || approving) return;
		if (reviewByInstall[entry.installation_id]) return;
		reviewRequest?.abort();
		const controller = new AbortController();
		const generation = ++reviewGeneration;
		const expectedScope = scopeKey;
		const expectedInstallationGeneration = entry.installation_generation;
		const expectedPackageRevision = entry.package_revision_ref;
		reviewRequest = controller;
		reviewing = entry.installation_id;
		error = '';
		try {
			const review = await fetchAppInstallationReview(entry.installation_id, controller.signal);
			if (controller.signal.aborted || generation !== reviewGeneration || expectedScope !== scopeKey ||
				entry.installation_generation !== expectedInstallationGeneration ||
				entry.package_revision_ref !== expectedPackageRevision) return;
			reviewByInstall = { ...reviewByInstall, [entry.installation_id]: review };
			grantByInstall = { ...grantByInstall, [entry.installation_id]: defaultGrantedNames(review) };
			if (review.attempt_kind !== 'initial_install') {
				try {
					const plan = await prepareReviewedUpdate(entry, review, false, controller.signal);
					if (plan) lifecycleMessage = `Bound review to update plan ${plan.migration_run_id}; dry-run ${plan.dry_run_representable}/${plan.dry_run_examined}.`;
				} catch (cause) {
					if (!controller.signal.aborted) {
						lifecycleMessage = `${cause instanceof Error ? cause.message : 'The migration plan needs explicit operations.'} Use “Prepare migration plan” to review exact operation JSON.`;
					}
				}
			}
		} catch (cause) {
			if (!controller.signal.aborted && generation === reviewGeneration && expectedScope === scopeKey) {
				error = cause instanceof Error ? cause.message : 'The app review could not be loaded.';
			}
		} finally {
			if (reviewRequest === controller) {
				reviewRequest = null;
				reviewing = '';
			}
		}
	}

	function toggleGrant(
		installationId: string,
		field: 'granted_tools' | 'granted_agents' | 'granted_personalities',
		name: string
	): void {
		const current = grantByInstall[installationId];
		if (!current) return;
		const values = new Set(current[field] ?? []);
		if (values.has(name)) values.delete(name);
		else values.add(name);
		grantByInstall = {
			...grantByInstall,
			[installationId]: { ...current, [field]: [...values] }
		};
	}

	async function approveEntry(entry: AppDirectoryEntry): Promise<void> {
		if (!statusMayHaveReview(entry.status) || approving) return;
		if (!reviewByInstall[entry.installation_id]) {
			await openReview(entry);
		}
		const grant = grantByInstall[entry.installation_id];
		const review = reviewByInstall[entry.installation_id];
		if (!grant || !review) return;
		approvalRequest?.abort();
		const controller = new AbortController();
		const generation = ++reviewGeneration;
		const expectedScope = scopeKey;
		approvalRequest = controller;
		approving = entry.installation_id;
		error = '';
		try {
			let exactGrant = grant;
			let plan = updatePlanByInstall[entry.installation_id];
			if (review.attempt_kind !== 'initial_install') {
				if (!plan) {
					const prepared = await prepareReviewedUpdate(entry, review, false, controller.signal);
					if (prepared) plan = prepared;
				}
				if (!plan) throw new Error('An exact update migration plan is required before approval.');
				if (plan.installation_id !== entry.installation_id || plan.attempt_id !== review.attempt_id ||
					plan.destination_package_revision_ref !== review.package_revision_ref) {
					throw new Error('The update plan no longer matches the displayed installation review.');
				}
				if (plan.backup_required && plan.state !== 'ready_to_switch') {
					const passphrase = window.prompt('Enter a new passphrase for the exact encrypted pre-update backup. It is not stored.');
					if (passphrase === null) return;
					plan = await backupAppUpdate(plan, passphrase, controller.signal);
					updatePlanByInstall = { ...updatePlanByInstall, [entry.installation_id]: plan };
				}
				if (plan.state !== 'ready_to_switch') {
					throw new Error(`The update plan is ${plan.state.replaceAll('_', ' ')}, not ready to switch.`);
				}
				if (plan.destructive && !window.confirm(
					`Approve ${plan.migration_operations.length} destructive migration operation(s) after the exact encrypted backup?`
				)) return;
				exactGrant = {
					...grant,
					migration_run_id: plan.migration_run_id,
					update_plan_digest: plan.update_plan_digest,
					destructive_migration_confirmed: plan.destructive
				};
			}
			await approveAppInstallation(entry, review, exactGrant, controller.signal);
			if (controller.signal.aborted || generation !== reviewGeneration || expectedScope !== scopeKey) return;
			if (plan) {
				updatePlanByInstall = {
					...updatePlanByInstall,
					[entry.installation_id]: { ...plan, state: 'switched' }
				};
			}
			reviewByInstall = Object.fromEntries(Object.entries(reviewByInstall)
				.filter(([installationId]) => installationId !== entry.installation_id));
			grantByInstall = Object.fromEntries(Object.entries(grantByInstall)
				.filter(([installationId]) => installationId !== entry.installation_id));
			section = 'installed';
			await load(1, true);
		} catch (cause) {
			if (!controller.signal.aborted && generation === reviewGeneration && expectedScope === scopeKey) {
				error = cause instanceof Error ? cause.message : 'The app could not be approved.';
			}
		} finally {
			if (approvalRequest === controller) {
				approvalRequest = null;
				approving = '';
			}
		}
	}

	async function togglePin(entry: AppDirectoryEntry, viewId: string, pinned: boolean): Promise<void> {
		const key = `${entry.installation_id}:${viewId}`;
		if (pinning || entry.status !== 'enabled') return;
		pinning = key;
		error = '';
		try {
			await setAppDirectoryPin(entry.installation_id, viewId, !pinned);
			await load(page);
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'The app pin could not be updated.';
		} finally {
			pinning = '';
		}
	}

	async function toggleActionPin(entry: AppDirectoryEntry, actionId: string, pinned: boolean): Promise<void> {
		const key = `${entry.installation_id}:action:${actionId}`;
		if (pinning || entry.status !== 'enabled') return;
		pinning = key;
		error = '';
		try {
			await setAppDirectoryActionPin(entry.installation_id, actionId, !pinned);
			await load(page);
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'The app action pin could not be updated.';
		} finally {
			pinning = '';
		}
	}

	async function runLifecycle(
		entry: AppDirectoryEntry,
		operation: AppLifecycleControl['operation']
	): Promise<void> {
		if (lifecycleBusy) return;
		const control = appLifecycleControls(entry).find((item) => item.operation === operation);
		if (!control?.available) {
			lifecycleMessage = control?.reason ?? 'This lifecycle operation is unavailable.';
			return;
		}
		if (operation === 'purge') {
			purgeRequest?.abort();
			const controller = new AbortController();
			purgeRequest = controller;
			const requestedScope = scopeKey;
			lifecycleBusy = `${entry.installation_id}:purge`;
			lifecycleMessage = '';
			try {
				const preview = await previewAppPurge(entry, controller.signal);
				if (scopeKey === requestedScope && purgeRequest === controller) {
					purgePreview = { entry, preview };
					lifecycleMessage = `Review every storage disposition before permanently purging ${entry.name}.`;
				}
			} catch (cause) {
				if (!controller.signal.aborted && scopeKey === requestedScope) {
					lifecycleMessage = cause instanceof Error ? cause.message : 'The purge preview could not be prepared.';
				}
			} finally {
				if (purgeRequest === controller) purgeRequest = null;
				if (scopeKey === requestedScope) lifecycleBusy = '';
			}
			return;
		}
		if (operation === 'reenable') {
			const controller = new AbortController();
			const requestedScope = scopeKey;
			lifecycleBusy = `${entry.installation_id}:reenable`;
			lifecycleMessage = '';
			try {
				const review = await fetchAppReenableReview(entry, controller.signal);
				if (scopeKey === requestedScope) {
					reenableReview = { entry, review };
					lifecycleMessage = `Review the exact current authority before re-enabling ${entry.name}.`;
				}
			} catch (cause) {
				if (!controller.signal.aborted && scopeKey === requestedScope) {
					lifecycleMessage = cause instanceof Error ? cause.message : 'The re-enable review could not be loaded.';
				}
			} finally {
				if (scopeKey === requestedScope) lifecycleBusy = '';
			}
			return;
		}
		if (operation === 'commit_update') {
			lifecycleMessage = control.reason ?? 'This lifecycle operation requires reviewed authority.';
			return;
		}
		if (control.dangerous && !window.confirm(`${control.label} ${entry.name}?`)) return;
		lifecycleBusy = `${entry.installation_id}:${operation}`;
		lifecycleMessage = '';
		try {
			const receipt = await runAppLifecycleOperation(entry, operation);
			if (operation === 'begin_update') candidateTarget = `update:${entry.installation_id}`;
			if (operation === 'abort_update') {
				updatePlanByInstall = Object.fromEntries(Object.entries(updatePlanByInstall)
					.filter(([installationId]) => installationId !== entry.installation_id));
				if (candidateTarget === `update:${entry.installation_id}`) candidateTarget = 'initial';
			}
			lifecycleMessage = operation === 'revoke_grant'
				? `Revoked ${entry.name}'s current grant at installation generation ${receipt.generation}.`
				: `${entry.name} is now ${receipt.status.replaceAll('_', ' ')} at generation ${receipt.generation}.`;
			await load(page);
		} catch (cause) {
			lifecycleMessage = cause instanceof Error ? cause.message : 'The lifecycle operation failed.';
		} finally {
			lifecycleBusy = '';
		}
	}

	async function confirmReenable(): Promise<void> {
		if (!reenableReview || lifecycleBusy) return;
		const { entry, review } = reenableReview;
		lifecycleBusy = `${entry.installation_id}:reenable`;
		const requestedScope = scopeKey;
		lifecycleMessage = '';
		try {
			const receipt = await commitAppReenable(entry, review);
			if (scopeKey === requestedScope) {
				reenableReview = null;
				lifecycleMessage = `${entry.name} is re-enabled at generation ${receipt.generation} from review ${review.review_digest}.`;
				await load(page);
			}
		} catch (cause) {
			if (scopeKey === requestedScope) {
				lifecycleMessage = cause instanceof Error ? cause.message : 'The reviewed re-enable failed.';
			}
		} finally {
			if (scopeKey === requestedScope) lifecycleBusy = '';
		}
	}

	async function confirmPurge(): Promise<void> {
		if (!purgePreview || lifecycleBusy) return;
		const { entry, preview } = purgePreview;
		if (!window.confirm(`Permanently purge installation-owned data for ${entry.name}? Shared and policy-retained evidence listed in the preview will remain.`)) return;
		lifecycleBusy = `${entry.installation_id}:purge`;
		purgeRequest?.abort();
		const controller = new AbortController();
		purgeRequest = controller;
		const requestedScope = scopeKey;
		lifecycleMessage = '';
		try {
			const receipt = await commitAppPurge(entry, preview, controller.signal);
			if (scopeKey === requestedScope && purgeRequest === controller) {
				purgePreview = null;
				lifecycleMessage = `${entry.name} is purged. Receipt ${receipt.receipt_ref} reports ${receipt.completion.replaceAll('_', ' ')}.`;
				await load(page);
			}
		} catch (cause) {
			if (!controller.signal.aborted && scopeKey === requestedScope) {
				lifecycleMessage = cause instanceof Error ? cause.message : 'The retained data could not be purged.';
			}
		} finally {
			if (purgeRequest === controller) purgeRequest = null;
			if (scopeKey === requestedScope) lifecycleBusy = '';
		}
	}

	function purgeTargetLabel(target: string): string {
		return target.replaceAll('_', ' ').replace(/\b\w/g, (character) => character.toUpperCase());
	}

	async function downloadPackage(entry: AppDirectoryEntry): Promise<void> {
		if (lifecycleBusy) return;
		lifecycleBusy = `${entry.installation_id}:export`;
		lifecycleMessage = '';
		try {
			const archive = await exportAppPackage(entry.installation_id);
			const url = URL.createObjectURL(archive);
			const anchor = document.createElement('a');
			anchor.href = url;
			anchor.download = `${entry.name.replace(/[^A-Za-z0-9_.-]+/g, '-')}-${entry.package_version}.app.zip`;
			anchor.click();
			setTimeout(() => URL.revokeObjectURL(url), 0);
			lifecycleMessage = `Exported ${entry.name} without workspace grants or app records.`;
		} catch (cause) {
			lifecycleMessage = cause instanceof Error ? cause.message : 'The app package could not be exported.';
		} finally {
			lifecycleBusy = '';
		}
	}

	async function importPackage(event: Event): Promise<void> {
		const input = event.currentTarget as HTMLInputElement;
		const archive = input.files?.[0];
		if (!archive || importBusy) return;
		importBusy = true;
		lifecycleMessage = '';
		try {
			const receipt = await importAppPackage(archive);
			stagedPackage = {
				archive,
				receipt,
				requestId: `candidate-request:${crypto.randomUUID()}`
			};
			lifecycleMessage = `Staged ${receipt.package_id} (${receipt.stage_outcome.replaceAll('_', ' ')}). Local conformance and owner review are still required; no foreign grant transferred.`;
		} catch (cause) {
			lifecycleMessage = cause instanceof Error ? cause.message : 'The app package could not be imported.';
		} finally {
			input.value = '';
			importBusy = false;
		}
	}

	async function publishStagedCandidate(): Promise<void> {
		if (!stagedPackage || candidatePublishBusy) return;
		candidatePublishBusy = true;
		lifecycleMessage = '';
		try {
			const target = candidateTarget === 'initial' ? undefined
				: candidateTarget.startsWith('update:') ? {
					installation_id: candidateTarget.slice('update:'.length),
					attempt_kind: 'update' as const
				} : candidateTarget.startsWith('reinstall:') ? {
					installation_id: candidateTarget.slice('reinstall:'.length),
					attempt_kind: 'reinstall' as const
				} : null;
			if (target === null || target?.installation_id.length === 0) {
				throw new Error('The selected update/reinstall target is invalid. Refresh and choose it again.');
			}
			const receipt = await publishAppCandidate(
				stagedPackage.archive,
				stagedPackage.receipt,
				stagedPackage.requestId,
				target
			);
			if (target && receipt.installation_id !== target.installation_id) {
				throw new Error('The candidate receipt did not match the selected existing installation.');
			}
			stagedPackage = null;
			candidateTarget = 'initial';
			lifecycleMessage = `Created ${target ? target.attempt_kind : 'initial'} review candidate ${receipt.installation_id} (${receipt.publication_outcome.replaceAll('_', ' ')}). It remains inert until owner review and approval.`;
			section = 'needs_attention';
			await load(1, true);
		} catch (cause) {
			lifecycleMessage = cause instanceof Error ? cause.message : 'The review candidate could not be published.';
		} finally {
			candidatePublishBusy = false;
		}
	}

	async function rollbackSwitchedUpdate(entry: AppDirectoryEntry): Promise<void> {
		const plan = updatePlanByInstall[entry.installation_id];
		if (!plan || !['switched', 'rewind_review_pending'].includes(plan.state) || lifecycleBusy) return;
		if (plan.migration_operations.length === 0) {
			if (!window.confirm(`Roll ${entry.name} back to its prior code revision? Current grants and app data stay in force.`)) return;
			lifecycleBusy = `${entry.installation_id}:update-rollback`;
			lifecycleMessage = '';
			try {
				const receipt = await rollbackCodeAppUpdate(entry, plan);
				updatePlanByInstall = {
					...updatePlanByInstall,
					[entry.installation_id]: { ...plan, state: 'rolled_back' }
				};
				lifecycleMessage = `Rolled back code at generation ${receipt.installation_generation}. Current grants and records were not restored or rewritten.`;
				await load(page);
			} catch (cause) {
				lifecycleMessage = cause instanceof Error ? cause.message : 'The code-only update rollback failed.';
			} finally {
				lifecycleBusy = '';
			}
			return;
		}
		const passphrase = window.prompt('Enter the exact pre-update backup passphrase to prepare a data-rewind preview. It is not stored.');
		if (passphrase === null) return;
		lifecycleBusy = `${entry.installation_id}:rewind-preview`;
		lifecycleMessage = '';
		try {
			const preview = await previewAppDataRewind(plan, passphrase);
			const pendingPlan = { ...plan, state: 'rewind_review_pending' as const };
			updatePlanByInstall = { ...updatePlanByInstall, [entry.installation_id]: pendingPlan };
			dataRewindReview = { entry, plan: pendingPlan, preview };
			lifecycleMessage = `Prepared an exact data-rewind preview for ${preview.preview.source_record_count} backup records. No active data changed.`;
		} catch (cause) {
			lifecycleMessage = cause instanceof Error ? cause.message : 'The data-rewind preview failed.';
		} finally {
			lifecycleBusy = '';
		}
	}

	async function commitReviewedDataRewind(): Promise<void> {
		if (!dataRewindReview || dataImportBusy || dataRewindReview.preview.preview.status === 'blocked') return;
		const { entry, plan, preview } = dataRewindReview;
		if (!window.confirm(
			`Rewind ${entry.name} to the ${preview.preview.source_record_count}-record backup projection? Post-update records are tombstoned; current grants remain in force.`
		)) return;
		const passphrase = window.prompt('Re-enter the exact pre-update backup passphrase to commit this preview. It is not stored.');
		if (passphrase === null) return;
		dataImportBusy = true;
		lifecycleMessage = '';
		try {
			const receipt = await commitAppDataRewind(preview, passphrase);
			updatePlanByInstall = {
				...updatePlanByInstall,
				[entry.installation_id]: { ...plan, state: 'rolled_back' }
			};
			dataRewindReview = null;
			lifecycleMessage = `Committed reviewed data rewind at generation ${receipt.rollback.installation_generation}: ${receipt.import_receipt.created_count} local records created, ${receipt.import_receipt.skipped_count} conflicts skipped. Current grants were not restored.`;
			await load(page);
		} catch (cause) {
			lifecycleMessage = cause instanceof Error ? cause.message : 'The reviewed data rewind failed.';
		} finally {
			dataImportBusy = false;
		}
	}

	async function downloadDataArchive(entry: AppDirectoryEntry, kind: 'data' | 'combined'): Promise<void> {
		if (lifecycleBusy) return;
		const passphrase = window.prompt('Choose an archive passphrase (12–1024 printable characters). It is not stored.');
		if (passphrase === null) return;
		lifecycleBusy = `${entry.installation_id}:data-export`;
		lifecycleMessage = '';
		try {
			const archive = await exportAppDataArchive(
				entry.installation_id,
				kind,
				`data-export:${crypto.randomUUID()}`,
				passphrase
			);
			const url = URL.createObjectURL(archive);
			const anchor = document.createElement('a');
			anchor.href = url;
			anchor.download = `${entry.name.replace(/[^A-Za-z0-9_.-]+/g, '-')}-${kind}.appdata`;
			anchor.click();
			setTimeout(() => URL.revokeObjectURL(url), 0);
			lifecycleMessage = `Exported encrypted ${kind === 'combined' ? 'package + data' : 'data-only'} archive for ${entry.name}.`;
		} catch (cause) {
			lifecycleMessage = cause instanceof Error ? cause.message : 'The app data archive could not be exported.';
		} finally {
			lifecycleBusy = '';
		}
	}

	function chooseDataImport(entry: AppDirectoryEntry): void {
		if (dataImportBusy) return;
		dataImportTarget = entry;
		dataArchiveInput?.click();
	}

	async function previewDataImport(event: Event): Promise<void> {
		const input = event.currentTarget as HTMLInputElement;
		const archive = input.files?.[0];
		const entry = dataImportTarget;
		if (!archive || !entry || dataImportBusy) return;
		const entered = window.prompt('Enter the archive passphrase. Leave empty only for an explicitly exported plaintext archive.');
		if (entered === null) { input.value = ''; return; }
		dataImportBusy = true;
		lifecycleMessage = '';
		try {
			const preview = await previewAppDataImport(
				entry.installation_id,
				archive,
				`data-import-preview:${crypto.randomUUID()}`,
				entered.length === 0 ? null : entered
			);
			dataImportReview = {
				entry,
				preview,
				approval: null,
				approvalRequestId: `data-import-approval:${crypto.randomUUID()}`,
				commitRequestId: `data-import-commit:${crypto.randomUUID()}`
			};
			lifecycleMessage = `Previewed ${preview.preview.source_record_count} records for ${entry.name}; ${preview.preview.status.replaceAll('_', ' ')}. No data has been changed.`;
		} catch (cause) {
			lifecycleMessage = cause instanceof Error ? cause.message : 'The app data archive could not be previewed.';
		} finally {
			input.value = '';
			dataImportBusy = false;
		}
	}

	async function approveDataImport(): Promise<void> {
		if (!dataImportReview || dataImportBusy || dataImportReview.preview.preview.status === 'blocked') return;
		if (!window.confirm(`Approve the reviewed import of ${dataImportReview.preview.preview.source_record_count} records into ${dataImportReview.entry.name}? Conflicts will not be overwritten.`)) return;
		dataImportBusy = true;
		try {
			const approval = await approveAppDataImport(
				dataImportReview.entry.installation_id,
				dataImportReview.preview.preview.preview_digest,
				dataImportReview.approvalRequestId
			);
			dataImportReview = { ...dataImportReview, approval };
			lifecycleMessage = 'The exact preview is approved. Commit is still a separate action.';
		} catch (cause) {
			lifecycleMessage = cause instanceof Error ? cause.message : 'The app data import could not be approved.';
		} finally {
			dataImportBusy = false;
		}
	}

	async function commitDataImport(): Promise<void> {
		if (!dataImportReview?.approval || dataImportBusy) return;
		dataImportBusy = true;
		try {
			const committed = await commitAppDataImport(
				dataImportReview.entry.installation_id,
				dataImportReview.preview.preview.preview_digest,
				dataImportReview.approval.approval_ref,
				dataImportReview.commitRequestId
			);
			lifecycleMessage = `Imported ${committed.receipt.created_count} records; skipped ${committed.receipt.skipped_count} conflicts. No foreign authority transferred.`;
			dataImportReview = null;
		} catch (cause) {
			lifecycleMessage = cause instanceof Error ? cause.message : 'The reviewed app data import could not be committed.';
		} finally {
			dataImportBusy = false;
		}
	}

	function actionFieldLabel(name: string): string {
		return name.replaceAll('_', ' ').replace(/\b\w/g, (character) => character.toUpperCase());
	}

	function setActionValue(name: string, value: string | boolean): void {
		if (actionSubmitting || pendingLaunchIntent) return;
		actionValues = { ...actionValues, [name]: value };
		actionIdempotencyKey = `action-request:${crypto.randomUUID()}`;
	}

	function setActionNull(name: string, value: boolean): void {
		if (actionSubmitting || pendingLaunchIntent) return;
		actionNulls = { ...actionNulls, [name]: value };
		actionIdempotencyKey = `action-request:${crypto.randomUUID()}`;
	}

	function actionRunStorage(): Storage | null {
		return getAppActionRunStorage(typeof window === 'undefined' ? null : window);
	}

	function cancellationIntentKey(runRef: string): string {
		return `magician.apps.action-cancellation.v1:${scopeKey}:${runRef}`;
	}

	function retainedCancellationIntent(runRef: string): { expected_generation: number; idempotency_key: string } | null {
		const storage = actionRunStorage();
		if (!storage) return null;
		try {
			const value = JSON.parse(storage.getItem(cancellationIntentKey(runRef)) ?? 'null') as unknown;
			if (!value || typeof value !== 'object' || Array.isArray(value)) return null;
			const record = value as Record<string, unknown>;
			if (Object.keys(record).length !== 2 || !Number.isSafeInteger(record.expected_generation) ||
				(record.expected_generation as number) < 0 || typeof record.idempotency_key !== 'string' ||
				!/^[A-Za-z0-9][A-Za-z0-9_.:/@#-]{0,191}$/.test(record.idempotency_key)) return null;
			return {
				expected_generation: record.expected_generation as number,
				idempotency_key: record.idempotency_key
			};
		} catch {
			return null;
		}
	}

	function clearCancellationIntent(runRef: string): void {
		try { actionRunStorage()?.removeItem(cancellationIntentKey(runRef)); } catch { /* server truth remains pollable */ }
	}

	async function requestActionCancellation(runRef: string): Promise<void> {
		if (!actionRun || actionRun.run_ref !== runRef || actionRun.terminal || actionCancelling) return;
		// A status poll must not replace a cancellation error with "running"
		// before the owner can read it or retry the retained exact intent.
		actionPollRequest?.abort();
		actionPollRequest = null;
		if (actionPollTimer) clearTimeout(actionPollTimer);
		actionPollTimer = null;
		const storage = actionRunStorage();
		if (!storage) {
			actionMessage = 'Durable browser storage is required before requesting cancellation.';
			return;
		}
		let intent = retainedCancellationIntent(runRef);
		if (!intent) {
			intent = {
				expected_generation: actionRun.cancellation_generation ?? 0,
				idempotency_key: `app-cancel:${crypto.randomUUID()}`
			};
			try {
				storage.setItem(cancellationIntentKey(runRef), JSON.stringify(intent));
				if (retainedCancellationIntent(runRef)?.idempotency_key !== intent.idempotency_key) {
					throw new Error('cancellation intent readback failed');
				}
			} catch {
				actionMessage = 'The cancellation request was not sent because its durable intent could not be retained.';
				return;
			}
		}
		actionCancelling = true;
		actionMessage = 'Submitting the exact durable cancellation intent…';
		try {
			const receipt = await cancelAppActionRun(
				runRef,
				intent.expected_generation,
				intent.idempotency_key
			);
			if (!actionRun || actionRun.run_ref !== runRef) return;
			actionRun = {
				...actionRun,
				status: receipt.status,
				terminal: receipt.status === 'cancelled',
				cancellation_generation: receipt.generation
			};
			rememberCurrentActionRun();
			actionMessage = receipt.status === 'cancelled'
				? `Run ${runRef} was cancelled before physical I/O.`
				: `Run ${runRef} is cancelling. Final status will preserve the actual or uncertain outcome.`;
			if (receipt.status === 'cancelled') clearCancellationIntent(runRef);
			else scheduleActionPoll(runRef, 500);
		} catch (cause) {
			actionMessage = `${cause instanceof Error ? cause.message : 'The cancellation response was interrupted.'} The exact durable intent is retained for identical retry.`;
		} finally {
			actionCancelling = false;
		}
	}

	function matchingActionRuns(entry: AppDirectoryEntry, action: AppDirectoryAction): PersistedAppActionRun[] {
		const storage = actionRunStorage();
		if (!storage) return [];
		return loadAppActionRunHistory(storage, scopeKey).filter((run) =>
			run.run_handle.installation_id === entry.installation_id &&
			run.run_handle.action_id === action.action_id
		);
	}

	function matchingLaunchIntent(
		entry: AppDirectoryEntry,
		action: AppDirectoryAction
	): PersistedAppActionLaunchIntent | null {
		const storage = actionRunStorage();
		return storage
			? loadAppActionLaunchIntent(storage, scopeKey, entry.installation_id, action.action_id)
			: null;
	}

	function rememberCurrentActionRun(): void {
		const storage = actionRunStorage();
		if (!storage || !actionRun) return;
		rememberAppActionRun(storage, scopeKey, actionRun);
		if (actionEntry && actionDefinition) {
			recentActionRuns = matchingActionRuns(actionEntry, actionDefinition);
		}
	}

	function resumeActionRun(run: PersistedAppActionRun): void {
		actionRun = restoreAppActionRun(run);
		actionMessage = `Recovering ${run.run_handle.run_ref}…`;
		void refreshActionRun(run.run_handle.run_ref);
	}

	async function copyRunReference(runRef: string): Promise<void> {
		try {
			await navigator.clipboard.writeText(runRef);
			copiedRunRef = runRef;
			setTimeout(() => {
				if (copiedRunRef === runRef) copiedRunRef = '';
			}, 2_000);
		} catch {
			actionMessage = 'The run reference could not be copied. Select it manually instead.';
		}
	}

	function abandonActionState(): void {
		// Invalidate launch continuations before changing scope/modal state. A
		// launched effect is not aborted, but its late receipt cannot take over
		// a newer scope or clear a newer submission's busy state.
		actionSubmissionGeneration += 1;
		actionRequest?.abort();
		actionPollRequest?.abort();
		if (actionPollTimer) clearTimeout(actionPollTimer);
		actionRequest = null;
		actionPollRequest = null;
		actionPollTimer = null;
		actionPollFailures = 0;
		actionEntry = null;
		actionDefinition = null;
		actionContract = null;
		actionValues = {};
		actionNulls = {};
		actionMessage = '';
		actionIdempotencyKey = '';
		actionRun = null;
		recentActionRuns = [];
		pendingLaunchIntent = null;
		copiedRunRef = '';
		actionLoading = false;
		actionSubmitting = false;
		actionCancelling = false;
	}

	function closeAction(): void {
		if (actionSubmitting) return;
		abandonActionState();
	}

	let actionRecoveryRef = '';

	async function recoverActionReference(): Promise<void> {
		if (!actionEntry || !actionDefinition || actionSubmitting || actionLoading || actionCancelling) return;
		const runRef = actionRecoveryRef.trim();
		if (!runRef.startsWith('run:app-action:') || runRef.length > 256) {
			actionMessage = 'Paste a complete app run reference.';
			return;
		}
		const entry = actionEntry;
		const action = actionDefinition;
		const expectedScope = scopeKey;
		actionPollRequest?.abort();
		if (actionPollTimer) clearTimeout(actionPollTimer);
		const controller = new AbortController();
		actionPollRequest = controller;
		try {
			const run = await fetchAppActionRun(runRef, controller.signal);
			if (controller.signal.aborted || scopeKey !== expectedScope || actionEntry !== entry || actionDefinition !== action) return;
			if (run.run_handle?.installation_id !== entry.installation_id || run.run_handle.action_id !== action.action_id) {
				throw new Error('This run does not belong to the selected app action.');
			}
			actionRun = run;
			rememberCurrentActionRun();
			actionMessage = `Run ${runRef} is ${run.status}.`;
			if (!run.terminal) scheduleActionPoll(runRef);
		} catch (cause) {
			if (!controller.signal.aborted && scopeKey === expectedScope) {
				actionMessage = cause instanceof Error ? cause.message : 'The run could not be opened.';
			}
		} finally {
			if (actionPollRequest === controller) actionPollRequest = null;
		}
	}

	async function openAction(entry: AppDirectoryEntry, action: AppDirectoryAction): Promise<void> {
		if (entry.status !== 'enabled' || actionLoading || actionSubmitting) return;
		actionRequest?.abort();
		actionPollRequest?.abort();
		if (actionPollTimer) clearTimeout(actionPollTimer);
		actionPollRequest = null;
		actionPollTimer = null;
		actionPollFailures = 0;
		const controller = new AbortController();
		actionRequest = controller;
		actionEntry = entry;
		actionDefinition = action;
		actionContract = null;
		actionValues = {};
		actionNulls = {};
		actionMessage = '';
		actionIdempotencyKey = `action-request:${crypto.randomUUID()}`;
		actionRun = null;
		recentActionRuns = [];
		actionRecoveryRef = '';
		copiedRunRef = '';
		actionLoading = true;
		try {
			const contract = await fetchAppActionContract(entry.installation_id, action.action_id, controller.signal);
			if (controller.signal.aborted) return;
			if (Object.keys(contract.input.fields).length > MAX_ACTION_FIELDS) {
				throw new Error('The action contract contains too many fields for the bounded owner form.');
			}
			actionContract = contract;
			const initial: Record<string, string | boolean> = {};
			for (const [name, field] of Object.entries(contract.input.fields)) {
				initial[name] = field.type === 'boolean' && field.required ? false : '';
			}
			actionValues = initial;
			recentActionRuns = matchingActionRuns(entry, action);
			pendingLaunchIntent = matchingLaunchIntent(entry, action);
			if (pendingLaunchIntent) {
				if (pendingLaunchIntent.installation_generation !== entry.installation_generation ||
					pendingLaunchIntent.package_revision_ref !== entry.package_revision_ref) {
					actionMessage = 'A retained launch belongs to an older installation generation. It is preserved but cannot be replayed from this review state.';
				} else {
					const retainedInput = launchIntentInput(pendingLaunchIntent);
					actionValues = Object.fromEntries(Object.entries(retainedInput)
						.filter(([, value]) => value !== null)
						.map(([name, value]) => [name, typeof value === 'boolean' ? value : String(value)]));
					actionNulls = Object.fromEntries(Object.entries(retainedInput)
						.filter(([, value]) => value === null).map(([name]) => [name, true]));
					actionIdempotencyKey = pendingLaunchIntent.idempotency_key;
					actionMessage = 'A durable launch intent may have reached the server. Recover it with the exact retained input.';
				}
			} else if (recentActionRuns[0]) resumeActionRun(recentActionRuns[0]);
		} catch (cause) {
			if (!controller.signal.aborted) {
				actionMessage = cause instanceof Error ? cause.message : 'The app action could not be opened.';
			}
		} finally {
			if (actionRequest === controller) {
				actionRequest = null;
				actionLoading = false;
			}
		}
	}

	function actionInput(): Record<string, unknown> {
		if (!actionContract) throw new Error('The app action contract is unavailable.');
		if (Object.keys(actionContract.input.fields).length > MAX_ACTION_FIELDS) {
			throw new Error('The action contract exceeds the bounded form field limit.');
		}
		const input: Record<string, unknown> = {};
		for (const [name, field] of Object.entries(actionContract.input.fields)) {
			if (actionNulls[name]) {
				if (!field.nullable) throw new Error(`${actionFieldLabel(name)} cannot be null.`);
				input[name] = null;
				continue;
			}
			const raw = actionValues[name];
			if (field.type === 'boolean') {
				if (!field.required && raw === '') continue;
				if (raw === true || raw === 'true') input[name] = true;
				else if (raw === false || raw === 'false') input[name] = false;
				else throw new Error(`${actionFieldLabel(name)} must be true or false.`);
				continue;
			}
			const text = typeof raw === 'string' ? raw : '';
			if (!field.required && text.length === 0) continue;
			if (field.type === 'integer') {
				if (!/^-?\d+$/.test(text)) throw new Error(`${actionFieldLabel(name)} must be an integer.`);
				const number = Number(text);
				if (!Number.isSafeInteger(number)) throw new Error(`${actionFieldLabel(name)} is outside the supported integer range.`);
				input[name] = number;
			} else if (field.type === 'decimal') {
				if (text.length > 64) throw new Error(`${actionFieldLabel(name)} is too long.`);
				const number = Number(text);
				if (!Number.isFinite(number) || text.trim().length === 0) throw new Error(`${actionFieldLabel(name)} must be a number.`);
				input[name] = number;
			} else if (field.type === 'enum') {
				if (!field.values?.includes(text)) throw new Error(`${actionFieldLabel(name)} must use one of its declared values.`);
				input[name] = text;
			} else {
				const maximum = field.type === 'reference' ? 192 : field.type === 'timestamp' ? 64 : MAX_ACTION_TEXT_BYTES;
				if (new TextEncoder().encode(text).byteLength > maximum) {
					throw new Error(`${actionFieldLabel(name)} exceeds its local UTF-8 byte bound.`);
				}
				if (field.type === 'reference' && !/^[A-Za-z0-9][A-Za-z0-9_.:@/#-]{0,191}$/.test(text)) {
					throw new Error(`${actionFieldLabel(name)} is not a canonical record reference.`);
				}
				if (field.type === 'timestamp' && !isExactRfc3339(text)) {
					throw new Error(`${actionFieldLabel(name)} must be an exact RFC 3339 timestamp.`);
				}
				input[name] = text;
			}
		}
		return input;
	}

	function isExactRfc3339(value: string): boolean {
		return /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{1,9})?(?:Z|[+-]\d{2}:\d{2})$/.test(value) &&
			Number.isFinite(Date.parse(value));
	}

	function actionOutput(value: unknown): string {
		let nodes = 0;
		let bytes = 0;
		const stack: Array<{ value: unknown; depth: number }> = [{ value, depth: 0 }];
		while (stack.length > 0) {
			const current = stack.pop()!;
			if (++nodes > MAX_ACTION_RENDER_NODES || current.depth > MAX_ACTION_RENDER_DEPTH) {
				return 'Result is available but exceeds the bounded owner renderer.';
			}
			if (typeof current.value === 'string') bytes += new TextEncoder().encode(current.value).byteLength;
			else if (Array.isArray(current.value)) {
				if (current.value.length > 256) return 'Result is available but exceeds the bounded owner renderer.';
				for (const item of current.value) stack.push({ value: item, depth: current.depth + 1 });
			} else if (current.value && typeof current.value === 'object') {
				const entries = Object.entries(current.value as Record<string, unknown>);
				if (entries.length > 256) return 'Result is available but exceeds the bounded owner renderer.';
				for (const [key, item] of entries) {
					bytes += new TextEncoder().encode(key).byteLength;
					stack.push({ value: item, depth: current.depth + 1 });
				}
			}
			if (bytes > MAX_ACTION_RENDER_BYTES) return 'Result is available but exceeds the bounded owner renderer.';
		}
		const encoded = JSON.stringify(value, null, 2) ?? '';
		return new TextEncoder().encode(encoded).byteLength <= MAX_ACTION_RENDER_BYTES
			? encoded : 'Result is available but exceeds the bounded owner renderer.';
	}

	function scheduleActionPoll(runRef: string, delayMs = 1_000): void {
		if (actionPollTimer) clearTimeout(actionPollTimer);
		actionPollTimer = setTimeout(() => {
			actionPollTimer = null;
			void refreshActionRun(runRef);
		}, delayMs);
	}

	function retryActionRun(runRef: string): void {
		if (actionPollTimer) clearTimeout(actionPollTimer);
		actionPollTimer = null;
		actionPollFailures = 0;
		void refreshActionRun(runRef);
	}

	async function refreshActionRun(runRef: string): Promise<void> {
		if (!actionEntry || actionRun?.run_ref !== runRef) return;
		const runHandle = actionRun.run_handle;
		actionPollRequest?.abort();
		const controller = new AbortController();
		actionPollRequest = controller;
		try {
			const run = await fetchAppActionRun(runRef, controller.signal);
			if (controller.signal.aborted || actionRun?.run_ref !== runRef) return;
			actionPollFailures = 0;
			actionRun = {
				...run,
				...(runHandle && !run.run_handle ? { run_handle: runHandle } : {})
			};
			rememberCurrentActionRun();
			if (run.terminal) clearCancellationIntent(runRef);
			const label = runRef;
			actionMessage = run.result_withheld
				? `Run ${label} finished, but its result is withheld by current app or source policy.`
				: run.terminal ? `Run ${label} finished with status ${run.status}.` : `Run ${label} is ${run.status}.`;
			if (!run.terminal) scheduleActionPoll(runRef);
		} catch (cause) {
			if (!controller.signal.aborted) {
				const message = cause instanceof Error ? cause.message : 'The app action status could not be loaded.';
				const retryable = isRetryableAppRequestError(cause);
				if (retryable) actionPollFailures += 1;
				if (retryable && actionPollFailures <= MAX_ACTION_POLL_FAILURES && actionRun?.run_ref === runRef) {
					const fallbackDelay = Math.min(
						MAX_ACTION_POLL_DELAY_MS,
						1_000 * (2 ** (actionPollFailures - 1))
					);
					const delay = Math.min(
						MAX_ACTION_POLL_DELAY_MS,
						appRequestRetryDelayMs(cause, fallbackDelay)
					);
					actionMessage = `${message} Retrying…`;
					scheduleActionPoll(runRef, delay);
				} else {
					actionMessage = message;
				}
			}
		} finally {
			if (actionPollRequest === controller) actionPollRequest = null;
		}
	}

	async function submitAction(): Promise<void> {
		if (!actionEntry || !actionDefinition || !actionContract || actionSubmitting) return;
		const storage = actionRunStorage();
		if (!storage) {
			actionMessage = 'Durable browser storage is required before launching an app action.';
			return;
		}
		let intent = pendingLaunchIntent;
		try {
			if (!intent) {
				if (!actionIdempotencyKey) actionIdempotencyKey = `action-request:${crypto.randomUUID()}`;
				intent = stageAppActionLaunchIntent(storage, {
					scope_key: scopeKey,
					installation_id: actionEntry.installation_id,
					installation_generation: actionEntry.installation_generation,
					package_revision_ref: actionEntry.package_revision_ref,
					action_id: actionDefinition.action_id,
					idempotency_key: actionIdempotencyKey,
					input: actionInput()
				});
				pendingLaunchIntent = intent;
			}
		} catch (cause) {
			actionMessage = cause instanceof Error ? cause.message : 'The durable launch intent could not be retained.';
			return;
		}
		await dispatchLaunchIntent(intent, storage);
	}

	async function dispatchLaunchIntent(
		intent: PersistedAppActionLaunchIntent,
		storage: Storage
	): Promise<void> {
		if (!actionEntry || !actionDefinition || actionSubmitting ||
			intent.scope_key !== scopeKey || intent.installation_id !== actionEntry.installation_id ||
			intent.installation_generation !== actionEntry.installation_generation ||
			intent.package_revision_ref !== actionEntry.package_revision_ref ||
			intent.action_id !== actionDefinition.action_id) {
			actionMessage = 'The retained launch does not match the current installation generation.';
			return;
		}
		const submissionGeneration = ++actionSubmissionGeneration;
		const submissionScopeKey = scopeKey;
		const submittedEntry = actionEntry;
		const submittedAction = actionDefinition;
		const controller = new AbortController();
		actionRequest?.abort();
		actionRequest = controller;
		actionSubmitting = true;
		actionMessage = 'Submitting the exact durable launch intent…';
		try {
			actionPollFailures = 0;
			const launch = await launchAppAction(
				submittedEntry.installation_id,
				submittedAction.action_id,
				intent.idempotency_key,
				launchIntentInput(intent),
				controller.signal
			);
			const launchedRun: AppActionRun = {
				run_handle: launch.run_handle,
				run_ref: launch.run_handle.run_ref,
				status: launch.result?.status ?? 'queued',
				terminal: launch.result !== undefined && launch.result.status !== 'waiting',
				result_withheld: false,
				...(launch.result ? { result: launch.result } : {})
			};
			// The server may have launched the effect even if the owner switched
			// scope while awaiting the response. Retain only its non-result
			// control metadata under the scope captured before dispatch.
			const runRetained = rememberAppActionRun(storage, submissionScopeKey, launchedRun);
			if (runRetained) {
				clearAppActionLaunchIntent(storage, intent);
				pendingLaunchIntent = null;
			}
			if (!appActionSubmissionIsCurrent(
				actionSubmissionGeneration,
				scopeKey,
				submissionGeneration,
				submissionScopeKey
			)) return;
			actionRun = launchedRun;
			recentActionRuns = matchingActionRuns(submittedEntry, submittedAction);
			actionMessage = !runRetained
				? `Run ${launch.run_handle.run_ref} was returned, but durable run recovery could not be verified. The exact launch intent remains retained.`
				: launchedRun.terminal
				? `Run ${launch.run_handle.run_ref} finished with status ${launch.result?.status}.`
				: `Started run ${launch.run_handle.run_ref}.`;
			if (!launchedRun.terminal) scheduleActionPoll(launch.run_handle.run_ref);
			if (runRetained) actionIdempotencyKey = `action-request:${crypto.randomUUID()}`;
		} catch (cause) {
			if (appActionSubmissionIsCurrent(
				actionSubmissionGeneration,
				scopeKey,
				submissionGeneration,
				submissionScopeKey
			)) {
				actionMessage = `${cause instanceof Error ? cause.message : 'The app action launch was interrupted.'} The exact durable intent is retained for identical recovery.`;
			}
		} finally {
			if (actionRequest === controller) actionRequest = null;
			if (appActionSubmissionIsCurrent(
				actionSubmissionGeneration,
				scopeKey,
				submissionGeneration,
				submissionScopeKey
			)) actionSubmitting = false;
		}
	}

	let releaseFastAppNavigation: (() => void) | null = null;

	onMount(() => {
		mounted = true;
		appliedScope = scopeKey;
		releaseFastAppNavigation = requestFastAppNavigationPolling();
		void load(1, true, scopeKey);
	});
	onDestroy(() => {
		releaseFastAppNavigation?.();
		releaseFastAppNavigation = null;
		actionSubmissionGeneration += 1;
		abandonReviewState();
		request?.abort();
		purgeRequest?.abort();
		actionRequest?.abort();
		actionPollRequest?.abort();
		if (actionPollTimer) clearTimeout(actionPollTimer);
	});
</script>

<svelte:head><title>Apps · Magican</title></svelte:head>

<main class="apps-directory">
	<header class="hero">
		<div>
			<p class="eyebrow">Your software</p>
			<h1>Apps</h1>
			<p>Installed tools and views in this workspace. Search here uses package metadata only—never your app records.</p>
		</div>
		<div class="hero-actions">
			<button type="button" disabled={importBusy || candidatePublishBusy} on:click={() => packageInput?.click()}>{importBusy ? 'Importing…' : 'Import package'}</button>
			{#if stagedPackage}
				<label class="candidate-target">Candidate for
					<select bind:value={candidateTarget} disabled={candidatePublishBusy}>
						<option value="initial">New installation</option>
						{#if candidateTarget !== 'initial' && !entries.some((entry) => candidateTarget === `${entry.status === 'update_pending' ? 'update' : 'reinstall'}:${entry.installation_id}` && (entry.status === 'update_pending' || entry.status === 'uninstalled_retained'))}
							<option value={candidateTarget}>Pending {candidateTarget.startsWith('update:') ? 'update' : 'reinstall'} · {candidateTarget.split(':').slice(1).join(':')}</option>
						{/if}
						{#each entries.filter((entry) => entry.status === 'update_pending' || entry.status === 'uninstalled_retained') as entry}
							<option value={`${entry.status === 'update_pending' ? 'update' : 'reinstall'}:${entry.installation_id}`}>{entry.status === 'update_pending' ? 'Update' : 'Reinstall'} · {entry.name}</option>
						{/each}
					</select>
				</label>
				<button type="button" disabled={candidatePublishBusy} on:click={() => void publishStagedCandidate()}>{candidatePublishBusy ? 'Running conformance…' : 'Run conformance and create review candidate'}</button>
			{/if}
			<input class="visually-hidden" bind:this={packageInput} type="file" accept=".zip,application/vnd.app-platform.package+zip" on:change={(event) => void importPackage(event)} />
			<input class="visually-hidden" bind:this={dataArchiveInput} type="file" accept=".appdata,application/vnd.magician.app-archive.encrypted+octet-stream,application/vnd.magician.app-archive+json" on:change={(event) => void previewDataImport(event)} />
			<a class="create" href="/vibe">Create app</a>
		</div>
	</header>

	<AppContributionStatePanel />

	<section class="controls" aria-label="Apps directory controls">
		<div class="tabs" role="tablist" aria-label="App lifecycle sections">
			{#each sections as item}
				<button type="button" role="tab" aria-selected={section === item.id} class:active={section === item.id} on:click={() => void chooseSection(item.id)}>{item.label}</button>
			{/each}
		</div>
		<form class="search" on:submit|preventDefault={() => void submitSearch()}>
			<label for="app-directory-search">Search app metadata</label>
			<div><input id="app-directory-search" bind:value={searchDraft} maxlength="128" placeholder="Name or description" /><button type="submit" disabled={loading} aria-label="Search" title="Search"><svg viewBox="0 0 16 16" aria-hidden="true" focusable="false"><circle cx="7" cy="7" r="4.5" fill="none" stroke="currentColor" stroke-width="1.6" /><line x1="10.4" y1="10.4" x2="14" y2="14" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" /></svg></button></div>
		</form>
	</section>

	{#if error}<div class="error" role="alert">{error}</div>{/if}
	{#if lifecycleMessage}<div class="lifecycle-message" role="status">{lifecycleMessage}</div>{/if}

	<!-- No column-header strip: rows are cards that carry their own meta line,
	     so a shared 5-column header could never line up with them and read as
	     a broken table. Scannability comes from the aligned name/status line. -->
	<section class:loading class="rows" aria-busy={loading} aria-live="polite">
		{#if !loading && entries.length === 0}
			<div class="empty"><strong>No apps here yet.</strong><span>{search ? 'Try a different metadata search.' : 'This section will fill as apps are installed and used.'}</span></div>
		{:else}
			{#each entries as entry (entry.installation_id)}
				{@const expanded = Boolean(rowExpanded[entry.installation_id])}
				<article class="app-row" class:expanded={expanded}>
					<!-- One compact head line: glyph, name+description, quiet inline meta,
					     status, and the expander at the far right — the conventional
					     chevron position, not floating over the app icon. Views, actions,
					     review and lifecycle live behind the expander. -->
					<div class="card-head">
						<span class="monogram" aria-hidden="true">{entry.icon.value}</span>
						<div class="row-title"><h2>{entry.name}</h2><p>{entry.description}</p></div>
						<span class="meta-inline" aria-hidden="true"><span>v{entry.package_version}</span><span>gen {entry.installation_generation}</span><span>{entry.record_count} records</span><span>{formatBytes(entry.payload_bytes)}</span></span>
						<span class:attention={Boolean(entry.attention_reason)} class="app-status" data-status={entry.status} title={entry.status.replaceAll('_', ' ')}>{entry.status.replaceAll('_', ' ')}</span>
						<button
							class="disclose"
							type="button"
							aria-expanded={expanded}
							aria-label={`${expanded ? 'Collapse' : 'Expand'} ${entry.name}`}
							on:click={() => toggleRow(entry.installation_id)}
						><svg viewBox="0 0 16 16" aria-hidden="true" focusable="false"><path d="M4 6l4 4 4-4" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" /></svg></button>
					</div>
					{#if entry.attention_reason}<p class="attention-copy">{entry.attention_reason}</p>{/if}
					{#if expanded}
						{#if entry.views.length > 0}
							<div class="views" aria-label={`${entry.name} views`}>
								{#each entry.views as view}
									<div class="view-row">
										<button class="view-link" type="button" disabled={entry.status !== 'enabled'} on:click={() => void launch(entry, view.route, view.view_id)}>{view.label}</button>
										<button class="pin" class:pinned={view.pinned} type="button" disabled={entry.status !== 'enabled' || Boolean(pinning)} aria-label={`${view.pinned ? 'Unpin' : 'Pin'} ${view.label}`} on:click={() => void togglePin(entry, view.view_id, view.pinned)}>{view.pinned ? '★' : '☆'}</button>
									</div>
								{/each}
							</div>
						{/if}
						{#if entry.actions.length > 0}<div class="actions" aria-label={`${entry.name} actions`}>{#each entry.actions as action}<div class="action-row"><button type="button" disabled={entry.status !== 'enabled' || actionLoading || actionSubmitting} on:click={() => void openAction(entry, action)}>{action.label}</button><button class="pin" class:pinned={action.pinned} type="button" disabled={entry.status !== 'enabled' || Boolean(pinning)} aria-label={`${action.pinned ? 'Unpin' : 'Pin'} ${action.label}`} on:click={() => void toggleActionPin(entry, action.action_id, action.pinned)}>{action.pinned ? '★' : '☆'}</button></div>{/each}</div>{/if}
					{/if}
					<!-- Review and lifecycle are the expanded detail. Collapsed, a row stays a
					     scannable line; nothing is removed, it is opened one row at a time. -->
					{#if statusMayHaveReview(entry.status) && rowExpanded[entry.installation_id]}
						<div class="review">
							{#if reviewByInstall[entry.installation_id]}
								{@const review = reviewByInstall[entry.installation_id]}
								{@const grant = grantByInstall[entry.installation_id]}
								<p class="review-copy">Grant a subset of what the package requested, then enable it in this workspace.</p>
								{#if review.requested_tools.length > 0}
									<fieldset>
										<legend>Tools</legend>
										{#each review.requested_tools as tool}
											{@const dispatch = review.tool_dispatch.find((item) => item.tool === tool)}
											<label>
												<input type="checkbox" checked={grant?.granted_tools?.includes(tool)} on:change={() => toggleGrant(entry.installation_id, 'granted_tools', tool)} />
												<span>{grantDisplayName(tool)}</span>
												{#if dispatch && !dispatch.dispatchable}<span class="review-note">not dispatchable yet</span>{/if}
											</label>
											{#if dispatch && !dispatch.dispatchable}<p class="review-copy">{dispatch.reason}</p>{/if}
										{/each}
									</fieldset>
								{/if}
								{#if review.requested_agents.length > 0}
									<fieldset>
										<legend>Agents</legend>
										{#each review.requested_agents as agent}
											<label><input type="checkbox" checked={grant?.granted_agents?.includes(agent)} on:change={() => toggleGrant(entry.installation_id, 'granted_agents', agent)} />{grantDisplayName(agent)}</label>
										{/each}
									</fieldset>
								{/if}
								{#if review.requested_personalities.length > 0}
									<fieldset>
										<legend>Personalities</legend>
										{#each review.requested_personalities as personality}
											<label><input type="checkbox" checked={grant?.granted_personalities?.includes(personality)} on:change={() => toggleGrant(entry.installation_id, 'granted_personalities', personality)} />{grantDisplayName(personality)}</label>
										{/each}
									</fieldset>
								{/if}
								{#if review.requested_interactive_capabilities.length > 0}
									<fieldset>
										<legend>Interactive physical authority</legend>
										{#each review.requested_interactive_capabilities as interactive}
											<details class="review-policy">
												<summary>{grantDisplayName(interactive.dependency_ref)} · {interactive.granted.owner} · {grantDisplayName(interactive.action.action_ref)}</summary>
												<p class="review-copy">Owner: {interactive.granted.owner}; profile: {interactive.granted.target_profile_class.replaceAll('_', ' ')}; exact action: {grantDisplayName(interactive.action.action_ref)} ({interactive.action.class.replaceAll('_', ' ')}).</p>
												<p class="review-copy">Origins: {interactive.granted.allowed_origins.join(', ') || 'none (paired native target)'}.</p>
												<p class="review-copy">Selectors: bundles {interactive.granted.target_selectors.bundle_ids.join(', ') || 'none'}; packages {interactive.granted.target_selectors.package_ids.join(', ') || 'none'}; app refs {interactive.granted.target_selectors.application_refs.map(grantDisplayName).join(', ') || 'none'}; current reviewed pairing {interactive.granted.target_selectors.current_reviewed_pairing ? 'required' : 'not used'}.</p>
												<p class="review-copy">Posture: {interactive.granted.background.replaceAll('_', ' ')}; capture {interactive.granted.capture.replaceAll('_', ' ')}; transfer {interactive.granted.transfer.replaceAll('_', ' ')}.</p>
												<p class="review-copy">Ceilings: {interactive.granted.resources.max_sessions} session(s), {interactive.granted.resources.max_steps} step(s), {interactive.granted.resources.max_duration_seconds}s, {interactive.granted.resources.max_evidence_bytes.toLocaleString()} evidence bytes, {interactive.granted.resources.max_evidence_nodes.toLocaleString()} nodes, {interactive.granted.resources.max_output_bytes.toLocaleString()} output bytes; expiry {interactive.granted.expiry_session.grant_lifetime_seconds}s, session {interactive.granted.expiry_session.max_session_seconds}s ({interactive.granted.expiry_session.session.replaceAll('_', ' ')}).</p>
												<p class="review-copy">Exact request <code>{interactive.requested_request_digest}</code>; locked action <code>{interactive.action.action_digest}</code>.</p>
											</details>
										{/each}
										<p class="review-copy">Leaving the interactive grant field omitted accepts only these displayed reviewed requests whose dependency tool remains selected. It never grants undisplayed actions.</p>
									</fieldset>
								{/if}
								{#if review.requested_behaviors.length > 0}
									<fieldset>
										<legend>Scheduled behaviours</legend>
										<p class="review-copy">Approving grants all of these. They run on their own schedule, without a prompt from you.</p>
										{#each review.requested_behaviors as behavior}
											<details class="review-policy">
												<summary>{grantDisplayName(behavior.behavior_id)} · {grantDisplayName(behavior.action)}</summary>
												<p class="review-copy">{behavior.purpose}</p>
												<p class="review-copy">Operations: {behavior.operations.map(grantDisplayName).join(', ') || 'none'}.</p>
											</details>
										{/each}
									</fieldset>
								{/if}
								{#if review.requested_event_behaviors.length > 0}
									<fieldset>
										<legend>Event behaviours</legend>
										<p class="review-copy">Event and notification authority is denied by default; these are shown so you can see what the package asked for.</p>
										{#each review.requested_event_behaviors as behavior}
											<details class="review-policy">
												<summary>{grantDisplayName(behavior.behavior_id)} · {grantDisplayName(behavior.action)}</summary>
												<p class="review-copy">{behavior.purpose}</p>
												<p class="review-copy">Operations: {behavior.operations.map(grantDisplayName).join(', ') || 'none'}.</p>
											</details>
										{/each}
									</fieldset>
								{/if}
								{#if review.requested_custom_surface && grant}
									<AppCustomSurfaceReview {review} {grant} on:change={(event) => {
										grantByInstall = { ...grantByInstall, [entry.installation_id]: event.detail };
									}} />
								{/if}
								{#if review.requested_memory_read && grant?.granted_memory_read}
									{@const memory = review.requested_memory_read}
									{@const chosen = grant.granted_memory_read}
									<fieldset class="review-memory">
										<legend>Memory access</legend>
										<p class="review-copy">{memory.request.purpose}</p>
										<p class="review-copy">Choose what this app may read from your memory. Background runs happen without you; they get nothing unless you tick it. You can change this later in the app's settings.</p>
										<table>
											<thead>
												<tr><th scope="col">Memory</th><th scope="col">While you use it</th><th scope="col">In the background</th></tr>
											</thead>
											<tbody>
												{#each memory.request.user_tiers as tier}
													<tr>
														<th scope="row">{tier.replaceAll('_', ' ')}{#if memory.sensitive_tiers.includes(tier)} <span class="review-sensitive">sensitive</span>{/if}</th>
														{#each ['interactive', 'background'] as const as mode}
															<td><input type="checkbox" aria-label={`${tier} ${mode}`} checked={chosen[mode].user_tiers.includes(tier)} on:change={() => (grantByInstall = { ...grantByInstall, [entry.installation_id]: toggleMemoryGrant(grant, mode, 'user_tiers', tier) })} /></td>
														{/each}
													</tr>
												{/each}
												{#each memory.request.agents as agent}
													<tr>
														<th scope="row">What {grantDisplayName(agent)} has learned</th>
														{#each ['interactive', 'background'] as const as mode}
															<td><input type="checkbox" aria-label={`${agent} ${mode}`} checked={chosen[mode].agents.includes(agent)} on:change={() => (grantByInstall = { ...grantByInstall, [entry.installation_id]: toggleMemoryGrant(grant, mode, 'agents', agent) })} /></td>
														{/each}
													</tr>
												{/each}
											</tbody>
										</table>
										<p class="review-copy">The app never sees memory from client engagements or meetings, or memory other apps added.</p>
									</fieldset>
								{/if}
								{#if review.tool_runtime && grant}
									<fieldset class="review-secrets">
										<legend>Network</legend>
										<p class="review-copy">These tools run in a sandbox that can read only their own files and runtimes, write only a private scratch folder, and reach only the hosts below.</p>
										<table>
											<thead>
												<tr><th scope="col">Tool</th><th scope="col">Runs</th><th scope="col">Can reach</th></tr>
											</thead>
											<tbody>
												{#each review.tool_runtime as runtime}
													<tr>
														<th scope="row">{grantDisplayName(runtime.tool)}</th>
														<td>{runtime.in_place_from ? `runs in place from ${runtime.in_place_from}` : 'private copy'}</td>
														<td>{runtime.reaches_granted_hosts && grant.granted_any_public_host ? 'any website (a key it uses goes only where you allow that key)' : runtime.reachable_hosts?.length ? runtime.reachable_hosts.join(', ') : 'no host'}</td>
													</tr>
												{/each}
											</tbody>
										</table>
										{#if review.offers_any_public_host}
											<label class="review-copy"><input type="checkbox" checked={grant.granted_any_public_host === true} on:change={() => (grantByInstall = { ...grantByInstall, [entry.installation_id]: toggleAnyPublicHostGrant(review, grant) })} /> Allow these tools to reach <strong>any public website</strong> (off unless you tick it; only for tools that declare no host or any site; a key such a tool uses still goes only to the host(s) you pick for that key, or to any site only if you tick "any site" for that key)</label>
										{/if}
									</fieldset>
								{/if}
								{#if review.requested_secret_uses && grant}
									{@const ticked = grant.granted_secret_uses ?? []}
									<fieldset class="review-secrets">
										<legend>Secrets</legend>
										<p class="review-copy">These tools need one of your saved keys. A ticked key is handed only to that tool, only inside its sandbox, and can only reach the hosts shown: the hosts the tool declares, or, for a tool that declares none, the host(s) you pick for that key. A key goes to any website only if you tick "any site" for that key, and only when this app is allowed to reach any public website. Nothing is shared unless you allow it; an ungranted required key keeps that tool from running.</p>
										<table>
											<thead>
												<tr><th scope="col">Tool</th><th scope="col">Key</th><th scope="col">Only sent to</th><th scope="col">Allow</th></tr>
											</thead>
											<tbody>
												{#each review.requested_secret_uses as use}
													<tr>
														<th scope="row">{grantDisplayName(use.tool)}</th>
														<td><code>{use.secret_ref}</code>{#if use.required} <span class="review-sensitive">required</span>{/if}{#if use.delivery === 'config_file'} (as a config file){/if}</td>
														<td>{secretReach(use, ticked.find((item) => item.tool === use.tool && item.secret_ref === use.secret_ref))}{#if use.not_grantable}<br /><span class="review-copy">{use.not_grantable}</span>{/if}</td>
														<td>
															{#if use.not_grantable}
																<span aria-label={`${use.secret_ref} cannot be granted`}>—</span>
															{:else if use.app_granted_hosts || use.destination === '*'}
																{@const chosen = ticked.find((item) => item.tool === use.tool && item.secret_ref === use.secret_ref)}
																{#if use.app_granted_hosts}
																	<select multiple aria-label={`Hosts ${grantDisplayName(use.tool)} may send ${use.secret_ref} to`} disabled={chosen?.any_site === true} on:change={(event) => (grantByInstall = { ...grantByInstall, [entry.installation_id]: setSecretUseScope(review, grant, use.tool, use.secret_ref, { hosts: Array.from(event.currentTarget.selectedOptions, (option) => option.value), anySite: false }) })}>
																		{#each appNamedHosts(review) as host}
																			<option value={host} selected={chosen?.hosts?.includes(host) ?? false}>{host}</option>
																		{/each}
																	</select>
																{/if}
																{#if review.requested_data_handling_policy.external_egress === 'any_public_host'}
																	<label class="review-copy"><input type="checkbox" checked={chosen?.any_site === true} on:change={() => (grantByInstall = { ...grantByInstall, [entry.installation_id]: setSecretUseScope(review, grant, use.tool, use.secret_ref, { hosts: [], anySite: chosen?.any_site !== true }) })} /> <span class="review-sensitive">any site</span> — this key may be sent to <strong>any website</strong> the tool contacts</label>
																{/if}
															{:else}
																<input type="checkbox" aria-label={`Allow ${grantDisplayName(use.tool)} to use ${use.secret_ref}`} checked={ticked.some((item) => item.tool === use.tool && item.secret_ref === use.secret_ref)} on:change={() => (grantByInstall = { ...grantByInstall, [entry.installation_id]: toggleSecretUseGrant(review, grant, use.tool, use.secret_ref) })} />
															{/if}
														</td>
													</tr>
												{/each}
											</tbody>
										</table>
									</fieldset>
								{/if}
								{#if review.requested_tools.length === 0 && review.requested_agents.length === 0 && review.requested_personalities.length === 0 && review.requested_interactive_capabilities.length === 0 && review.requested_behaviors.length === 0 && review.requested_event_behaviors.length === 0 && !review.requested_custom_surface && !review.requested_memory_read && !review.requested_secret_uses}
									<p class="review-copy">This package requested no extra tools, agents or personalities.</p>
								{/if}
								{#if grant}
									{@const inert = inertWorkflowsForGrant(review, grant)}
									{#if inert.length > 0}
										<div class="review-inert" role="status">
											<p class="review-copy">These workflows will not run with the current grant. Enablement still succeeds.</p>
											<ul>
												{#each inert as workflow}
													<li><strong>{workflow.workflow_id}</strong> — {workflow.reasons.join('; ')}</li>
												{/each}
											</ul>
										</div>
									{/if}
								{/if}
								{#if review.workflow_material_bindings.length > 0}
									<details class="review-policy">
										<summary>Exact agent and personality revisions included in this approval</summary>
										<ul>{#each review.workflow_material_bindings as binding}<li><strong>{binding.workflow_id}</strong>: {grantDisplayName(binding.agent_ref)} revision {binding.agent_definition_revision}{binding.personality_ref ? ` with ${grantDisplayName(binding.personality_ref)}` : ' with no personality'} · binding <code>{binding.binding_digest}</code></li>{/each}</ul>
									</details>
								{/if}
								<div class="review-matrix" aria-label="Requested app authority matrix">
									<p class="review-copy">Data handling, interactive capability availability, background execution, network, and resource ceilings are reviewed as one exact grant.</p>
									<table><thead><tr><th>Authority</th><th>Request</th><th>Posture</th></tr></thead><tbody>{#each appInstallationReviewMatrix(review) as row}<tr><th scope="row">{row.category}</th><td>{row.request}</td><td><span class:denied={row.posture === 'denied'} class:conditional={row.posture === 'conditional'}>{row.posture}</span></td></tr>{/each}</tbody></table>
									<p class="captured-content-note">Captured screenshots, accessibility text, prompts, and other observed content are never shown in this review. Only declared targets, operations, policy, and bounded metadata appear.</p>
								</div>
								{#if review.permission_diff}<p class="review-copy">Package request before selections: {review.permission_diff.requires_review ? 'review required' : 'no expansion'} · {Object.entries(review.permission_diff).filter(([key, value]) => key !== 'diff_digest' && key !== 'requires_review' && value !== 'unchanged').map(([key, value]) => `${key.replaceAll('_', ' ')} ${value}`).join('; ') || 'unchanged'}</p>{/if}
								{#if review.attempt_kind !== 'initial_install'}
									{@const plan = updatePlanByInstall[entry.installation_id]}
									{#if plan}
										<p class="review-copy">Migration run <code>{plan.migration_run_id}</code> · {plan.migration_operations.length === 0 ? 'code-only/no record rewrite' : `${plan.migration_operations.length} exact operation(s)`} · dry-run {plan.dry_run_representable}/{plan.dry_run_examined} · {plan.destructive ? 'destructive approval and encrypted backup required' : 'non-destructive'} · state {plan.state.replaceAll('_', ' ')}.</p>
										<details class="review-policy"><summary>Exact migration operations and digest</summary><pre>{JSON.stringify(plan.migration_operations, null, 2)}</pre><p><code>{plan.update_plan_digest}</code></p></details>
									{/if}
									<button type="button" disabled={Boolean(reviewing) || Boolean(approving)} on:click={() => void promptForReviewedUpdatePlan(entry)}>{plan ? 'Replace exact migration plan…' : 'Prepare migration plan…'}</button>
								{/if}
							{/if}
						</div>
					{/if}
					<footer>
						{#if statusMayHaveReview(entry.status)}
							<button type="button" class="open" disabled={Boolean(reviewing) || Boolean(approving)} on:click={() => void (reviewByInstall[entry.installation_id] ? approveEntry(entry) : openReview(entry))}>
								{approving === entry.installation_id ? 'Committing review…' : reviewByInstall[entry.installation_id] ? entry.status === 'update_pending' ? 'Approve update' : entry.status === 'uninstalled_retained' ? 'Approve reinstall' : 'Approve and enable' : reviewing === entry.installation_id ? 'Loading review…' : 'Review'}
							</button>
						{:else}
							<button type="button" class="open" disabled={!entry.default_route || entry.status !== 'enabled'} on:click={() => void launch(entry, entry.default_route, entry.views.find((view) => view.route === entry.default_route)?.view_id)}>Open</button>
						{/if}
					{#if ['enabled', 'disabled', 'quarantined', 'uninstalled_retained'].includes(entry.status)}<AppDataCleanup installationId={entry.installation_id} appName={entry.name} />{/if}
						{#if entry.last_opened_at}<time datetime={entry.last_opened_at}>Used {new Date(entry.last_opened_at).toLocaleDateString()}</time>{/if}
					</footer>
					{#if rowExpanded[entry.installation_id]}
                    {#if entry.status === 'enabled' || entry.status === 'disabled'}<AppMemoryAccessPanel installationId={entry.installation_id} />{/if}
                    <details class="lifecycle-controls"><summary>Lifecycle and portability</summary><div>{#each appLifecycleControls(entry) as control}<button type="button" class:danger={control.dangerous} disabled={Boolean(lifecycleBusy) || !control.available} title={control.reason} on:click={() => control.available && void runLifecycle(entry, control.operation)}>{lifecycleBusy === `${entry.installation_id}:${control.operation}` ? 'Working…' : control.label}</button>{/each}<button type="button" disabled={Boolean(lifecycleBusy) || !appPackageExportAvailability(entry).available} title={appPackageExportAvailability(entry).reason} on:click={() => void downloadPackage(entry)}>{lifecycleBusy === `${entry.installation_id}:export` ? 'Exporting…' : 'Export package'}</button><button type="button" disabled={Boolean(lifecycleBusy) || !appPackageExportAvailability(entry).available} on:click={() => void downloadDataArchive(entry, 'data')}>{lifecycleBusy === `${entry.installation_id}:data-export` ? 'Exporting…' : 'Export encrypted data'}</button><button type="button" disabled={Boolean(lifecycleBusy) || !appPackageExportAvailability(entry).available} on:click={() => void downloadDataArchive(entry, 'combined')}>Export encrypted package + data</button><button type="button" disabled={dataImportBusy || entry.status !== 'enabled'} title={entry.status === 'enabled' ? undefined : 'Choose an enabled compatible destination.'} on:click={() => chooseDataImport(entry)}>Import data here…</button>{#if updatePlanByInstall[entry.installation_id] && ['switched', 'rewind_review_pending'].includes(updatePlanByInstall[entry.installation_id].state)}<button type="button" class="danger" disabled={Boolean(lifecycleBusy) || entry.status !== 'enabled'} on:click={() => void rollbackSwitchedUpdate(entry)}>{updatePlanByInstall[entry.installation_id].migration_operations.length === 0 ? 'Roll back update code' : 'Review data rewind…'}</button>{/if}</div>{#each appLifecycleControls(entry).filter((control) => !control.available) as control}<p><strong>{control.label} unavailable:</strong> {control.reason}</p>{/each}{#if !appPackageExportAvailability(entry).available}<p><strong>Export unavailable:</strong> {appPackageExportAvailability(entry).reason}</p>{/if}</details>
					{/if}
				</article>
			{/each}
		{/if}
	</section>

	{#if reenableReview}
		<div class="action-backdrop">
			<div class="purge-dialog" role="dialog" aria-modal="true" aria-labelledby="app-reenable-title">
				<header><div><p class="eyebrow">Reviewed re-enable</p><h2 id="app-reenable-title">{reenableReview.entry.name}</h2></div><button type="button" disabled={Boolean(lifecycleBusy)} aria-label="Close re-enable review" on:click={() => reenableReview = null}>×</button></header>
				<p>This restores only the current reviewed app authority. Previously invalidated memory and personal-agent retrieval contributions stay invalidated.</p>
				<dl class="review-identity">
					<dt>Package</dt><dd>{reenableReview.review.package_id} · {reenableReview.review.package_version}</dd>
					<dt>Package bytes</dt><dd><code>{reenableReview.review.package_content_digest}</code></dd>
					<dt>Package lock</dt><dd><code>{reenableReview.review.package_lock_digest}</code></dd>
					<dt>Grant</dt><dd>revision {reenableReview.review.grant_revision} · <code>{reenableReview.review.grant_identity_digest}</code></dd>
					<dt>Schema</dt><dd>revision {reenableReview.review.schema_revision} · <code>{reenableReview.review.schema_identity_digest}</code></dd>
					<dt>Surface</dt><dd>revision {reenableReview.review.surface_revision} · <code>{reenableReview.review.surface_identity_digest}</code></dd>
					<dt>Host policy</dt><dd>revision {reenableReview.review.global_policy_revision}</dd>
					<dt>Implementation</dt><dd><code>{reenableReview.review.implementation_identity_digest}</code></dd>
					<dt>Sealed review</dt><dd><code>{reenableReview.review.review_digest}</code></dd>
				</dl>
				<footer><button type="button" disabled={Boolean(lifecycleBusy)} on:click={() => reenableReview = null}>Keep disabled</button><button type="button" disabled={Boolean(lifecycleBusy)} on:click={() => void confirmReenable()}>{lifecycleBusy ? 'Revalidating…' : 'Revalidate and re-enable'}</button></footer>
			</div>
		</div>
	{/if}

	{#if purgePreview}
		<div class="action-backdrop">
			<div class="purge-dialog" role="dialog" aria-modal="true" aria-labelledby="app-purge-title">
				<header><div><p class="eyebrow">Permanent retained-data purge</p><h2 id="app-purge-title">{purgePreview.entry.name}</h2></div><button type="button" disabled={Boolean(lifecycleBusy)} aria-label="Close purge preview" on:click={() => purgePreview = null}>×</button></header>
				<p>This exact preview expires at <time datetime={purgePreview.preview.expires_at}>{new Date(purgePreview.preview.expires_at).toLocaleString()}</time>. Installation-owned rows are deleted; shared package, Artifact, provider, and policy evidence remains only where the disposition below says so.</p>
				<div class="purge-table-wrap"><table><thead><tr><th>Storage owner</th><th>Items</th><th>Bytes</th><th>Classification</th></tr></thead><tbody>{#each purgePreview.preview.inventory_entries as inventory}<tr><th scope="row">{purgeTargetLabel(inventory.target)}</th><td>{inventory.item_count.toLocaleString()}</td><td>{inventory.byte_count.toLocaleString()}</td><td>{inventory.maximum_classification}</td></tr>{/each}</tbody></table></div>
				<p class="review-copy">Preview digest <code>{purgePreview.preview.preview_digest}</code>. Confirmation is journaled before dispatch and exact retries reuse the same idempotency identity after response loss.</p>
				{#if lifecycleMessage}<p class="dialog-error" role="alert">{lifecycleMessage}</p>{/if}
				<footer><button type="button" disabled={Boolean(lifecycleBusy)} on:click={() => purgePreview = null}>Keep retained data</button><button type="button" class="danger" disabled={Boolean(lifecycleBusy)} on:click={() => void confirmPurge()}>{lifecycleBusy ? 'Purging…' : 'Confirm permanent purge'}</button></footer>
			</div>
		</div>
	{/if}

	{#if dataImportReview}
		<div class="action-backdrop">
			<div class="purge-dialog" role="dialog" aria-modal="true" aria-labelledby="app-data-import-title">
				<header><div><p class="eyebrow">Reviewed data import</p><h2 id="app-data-import-title">{dataImportReview.entry.name}</h2></div><button type="button" disabled={dataImportBusy} aria-label="Close data import review" on:click={() => dataImportReview = null}>×</button></header>
				<p>The authenticated archive contains {dataImportReview.preview.preview.source_record_count} records. Status: <strong>{dataImportReview.preview.preview.status.replaceAll('_', ' ')}</strong>. {dataImportReview.preview.package_payload_present ? 'The combined package payload was verified separately and transfers no authority.' : 'This is a data-only archive.'}</p>
				<p>Creates: {dataImportReview.preview.preview.record_decisions.filter((decision) => decision.decision === 'create').length}; conflicts/skips: {dataImportReview.preview.preview.record_decisions.filter((decision) => decision.decision !== 'create').length}; missing attachments: {dataImportReview.preview.preview.missing_attachments?.length ?? 0}.</p>
				<p class="review-copy">Preview digest <code>{dataImportReview.preview.preview.preview_digest}</code>. Destination generation {dataImportReview.preview.preview.destination_installation_generation}. Existing records are never overwritten.</p>
				<footer><button type="button" disabled={dataImportBusy} on:click={() => dataImportReview = null}>Cancel</button>{#if dataImportReview.approval}<button type="button" disabled={dataImportBusy} on:click={() => void commitDataImport()}>{dataImportBusy ? 'Committing…' : 'Commit exact reviewed import'}</button>{:else}<button type="button" disabled={dataImportBusy || dataImportReview.preview.preview.status === 'blocked'} on:click={() => void approveDataImport()}>{dataImportBusy ? 'Approving…' : 'Approve exact preview'}</button>{/if}</footer>
			</div>
		</div>
	{/if}

	{#if dataRewindReview}
		<div class="action-backdrop">
			<div class="purge-dialog" role="dialog" aria-modal="true" aria-labelledby="app-data-rewind-title">
				<header><div><p class="eyebrow">Reviewed update data rewind</p><h2 id="app-data-rewind-title">{dataRewindReview.entry.name}</h2></div><button type="button" disabled={dataImportBusy} aria-label="Close data rewind review" on:click={() => dataRewindReview = null}>×</button></header>
				<p>The exact encrypted pre-update backup projects {dataRewindReview.preview.preview.source_record_count} records into the current schema. Status: <strong>{dataRewindReview.preview.preview.status.replaceAll('_', ' ')}</strong>.</p>
				<p>New local IDs: {dataRewindReview.preview.preview.record_decisions.filter((decision) => decision.decision === 'create').length}; conflicts/skips: {dataRewindReview.preview.preview.record_decisions.filter((decision) => decision.decision !== 'create').length}. Records created after the update are tombstoned on commit; current permissions and grants remain in force.</p>
				<p class="review-copy">Preview digest <code>{dataRewindReview.preview.preview.preview_digest}</code>. Backup <code>{dataRewindReview.preview.backup_ref}</code>. Update plan <code>{dataRewindReview.preview.update_plan_digest}</code>.</p>
				<details class="review-policy"><summary>Exact conflict and new-local-ID receipt projection</summary><pre>{JSON.stringify(dataRewindReview.preview.preview.record_decisions, null, 2)}</pre></details>
				<footer><button type="button" disabled={dataImportBusy} on:click={() => dataRewindReview = null}>Keep current data</button><button type="button" class="danger" disabled={dataImportBusy || dataRewindReview.preview.preview.status === 'blocked'} on:click={() => void commitReviewedDataRewind()}>{dataImportBusy ? 'Committing…' : 'Confirm exact data rewind'}</button></footer>
			</div>
		</div>
	{/if}

	{#if actionEntry && actionDefinition}
		<div class="action-backdrop">
			<div class="action-dialog" role="dialog" aria-modal="true" aria-labelledby="app-action-title">
				<header><div><p class="eyebrow">{actionEntry.name}</p><h2 id="app-action-title">{actionDefinition.label}</h2></div><button type="button" disabled={actionSubmitting} aria-label="Close app action" on:click={closeAction}>×</button></header>
				{#if actionLoading}<p class="action-state">Loading the current action contract…</p>{/if}
				{#if actionContract}
					<form class="action-form" on:submit|preventDefault={() => void submitAction()}>
						{#each Object.entries(actionContract.input.fields) as [name, field] (name)}
							<label class="action-field">
								<span>{actionFieldLabel(name)}{field.required ? ' *' : ''}</span>
								{#if field.type === 'boolean' && field.required}
									<input type="checkbox" checked={Boolean(actionValues[name])} disabled={actionSubmitting || Boolean(pendingLaunchIntent) || Boolean(actionNulls[name])} on:change={(event) => setActionValue(name, (event.currentTarget as HTMLInputElement).checked)} />
								{:else if field.type === 'boolean'}
									<select value={String(actionValues[name] ?? '')} disabled={actionSubmitting || Boolean(pendingLaunchIntent) || Boolean(actionNulls[name])} on:change={(event) => setActionValue(name, (event.currentTarget as HTMLSelectElement).value)}><option value="">Not provided</option><option value="true">True</option><option value="false">False</option></select>
								{:else if field.type === 'enum'}
									<select value={String(actionValues[name] ?? '')} disabled={actionSubmitting || Boolean(pendingLaunchIntent) || Boolean(actionNulls[name])} on:change={(event) => setActionValue(name, (event.currentTarget as HTMLSelectElement).value)}><option value="" disabled={field.required}>Select…</option>{#each field.values ?? [] as value}<option {value}>{actionFieldLabel(value)}</option>{/each}</select>
								{:else if field.type === 'markdown'}
									<textarea rows="5" maxlength="65536" autocomplete="off" value={String(actionValues[name] ?? '')} disabled={actionSubmitting || Boolean(pendingLaunchIntent) || Boolean(actionNulls[name])} on:input={(event) => setActionValue(name, (event.currentTarget as HTMLTextAreaElement).value)}></textarea>
								{:else}
									<input type={field.type === 'integer' || field.type === 'decimal' ? 'number' : 'text'} step={field.type === 'integer' ? '1' : field.type === 'decimal' ? 'any' : undefined} maxlength={field.type === 'reference' ? 192 : field.type === 'timestamp' ? 64 : field.type === 'text' ? 65536 : undefined} autocomplete="off" aria-describedby={field.type === 'reference' ? `action-field-${name}-hint` : undefined} placeholder={field.type === 'timestamp' ? '2026-08-21T12:00:00Z' : field.type === 'reference' ? `${field.entity} record id` : undefined} value={String(actionValues[name] ?? '')} disabled={actionSubmitting || Boolean(pendingLaunchIntent) || Boolean(actionNulls[name])} on:input={(event) => setActionValue(name, (event.currentTarget as HTMLInputElement).value)} />
								{/if}
							</label>
							{#if field.type === 'reference'}<p class="field-hint" id={`action-field-${name}-hint`}>Reference to a declared {field.entity} record.</p>{/if}
							{#if field.nullable}<label class="null-field"><input type="checkbox" checked={Boolean(actionNulls[name])} disabled={actionSubmitting || Boolean(pendingLaunchIntent)} on:change={(event) => setActionNull(name, (event.currentTarget as HTMLInputElement).checked)} /> Use null</label>{/if}
						{/each}
						{#if Object.keys(actionContract.input.fields).length === 0}<p class="action-state">This action needs no input.</p>{/if}
						<button class="open" type="submit" disabled={actionSubmitting}>{actionSubmitting ? 'Submitting…' : pendingLaunchIntent ? 'Recover exact launch' : 'Start action'}</button>
					</form>
				{/if}
				<form class="action-form" on:submit|preventDefault={() => void recoverActionReference()}>
					<label class="action-field"><span>Open existing run</span><input type="text" bind:value={actionRecoveryRef} maxlength="256" autocomplete="off" placeholder="Paste run reference" /></label>
					<button type="submit" disabled={!actionRecoveryRef.trim() || actionSubmitting || actionLoading || actionCancelling || Boolean(actionPollRequest)}>Open run</button>
				</form>
				{#if actionMessage}<p class="action-state" role="status">{actionMessage}</p>{/if}
				{#if actionRun}
					{@const currentActionRun = actionRun}
					<div class="action-run" aria-label="App action status">
						<p><strong>{currentActionRun.status}</strong></p>
						<div class="action-run-identity"><code>{currentActionRun.run_ref}</code><button type="button" on:click={() => void copyRunReference(currentActionRun.run_ref)}>{copiedRunRef === currentActionRun.run_ref ? 'Copied' : 'Copy run reference'}</button></div>
						{#if !currentActionRun.terminal || currentActionRun.result_withheld}<div class="action-run-controls"><button type="button" disabled={Boolean(actionPollRequest)} on:click={() => retryActionRun(currentActionRun.run_ref)}>Refresh</button>{#if !currentActionRun.terminal}<button type="button" disabled={actionCancelling || currentActionRun.status === 'cancelling'} on:click={() => void requestActionCancellation(currentActionRun.run_ref)}>{actionCancelling ? 'Requesting…' : currentActionRun.status === 'cancelling' ? 'Cancelling…' : retainedCancellationIntent(currentActionRun.run_ref) ? 'Retry cancellation' : 'Cancel run'}</button>{/if}<span>{currentActionRun.result_withheld ? 'Result access follows the current app and source policy.' : currentActionRun.status === 'cancelling' ? 'Cancellation is durable; final status will preserve actual or uncertain physical outcome.' : 'Closing this dialog does not cancel the run.'}</span></div>{/if}
						{#if actionRun.result?.error}<div class="action-error" role="alert"><strong>{actionRun.result.error.code.replaceAll('_', ' ')}</strong><p>{actionRun.result.error.message}</p><span>Recovery: {actionRun.result.error.disposition.replaceAll('_', ' ')}</span>{#if actionRun.result.error.retry_after_ms}<span>Retry after {actionRun.result.error.retry_after_ms.toLocaleString()} ms</span>{/if}</div>{/if}
						{#if actionRun.result?.output}
							<div class="action-result-meta" aria-label="Typed action result metadata"><span>{actionRun.result.output.handling_labels.classification}</span><span>{actionRun.result.output.handling_labels.model_processing.replaceAll('_', ' ')}</span><span>schema {actionRun.result.output.schema_revision}</span><span>grant {actionRun.result.output.grant_revision}</span><span>{actionRun.result.output.source_refs.length} source references</span></div>
							<pre>{actionOutput(actionRun.result.output.value)}</pre>
						{/if}
						{#if actionRun.result && (actionRun.result.mutation_receipt_refs.length > 0 || actionRun.result.external_effect_receipt_refs.length > 0)}
							<details class="action-receipts"><summary>Committed receipts ({actionRun.result.mutation_receipt_refs.length + actionRun.result.external_effect_receipt_refs.length})</summary><ul>{#each actionRun.result.mutation_receipt_refs as receipt}<li>Mutation <code>{receipt}</code></li>{/each}{#each actionRun.result.external_effect_receipt_refs as receipt}<li>External effect <code>{receipt}</code></li>{/each}</ul></details>
						{/if}
						<AppInteractiveRunPanel runRef={actionRun.run_ref} runTerminal={actionRun.terminal} />
					</div>
				{/if}
				{#if recentActionRuns.length > 1}
					<div class="recent-action-runs" aria-label="Recent recoverable action runs">
						<p>Recent runs</p>
						{#each recentActionRuns.filter((run) => run.run_handle.run_ref !== actionRun?.run_ref) as run (run.run_handle.run_ref)}
							<button type="button" on:click={() => resumeActionRun(run)}><span>{run.status}</span><code>{run.run_handle.run_ref}</code></button>
						{/each}
					</div>
				{/if}
			</div>
		</div>
	{/if}

	<div class="pagination">
		<label>Page size <select value={pageSize} disabled={loading} on:change={(event) => void changePageSize(Number((event.currentTarget as HTMLSelectElement).value))}>{#each PAGE_SIZE_OPTIONS as option}<option value={option}>{option}</option>{/each}</select></label>
		<ServerPager currentPage={page} pageCount={pageCount} startItem={startItem} endItem={endItem} totalItems={endItem + (hasMore ? 1 : 0)} pageCountExact={!hasMore} totalItemsExact={!hasMore} {loading} ariaLabel="Apps directory pagination" on:pagechange={(event) => void load(event.detail.page)} />
	</div>
</main>

<style>
	.apps-directory { width: calc(100% - 32px); max-width: var(--app-content-max, 1320px); margin: 0 auto; padding: 26px 0 72px; color: var(--text-primary); font-family: var(--font-primary); }
	.hero, .controls, .card-head, .view-row, .app-row footer, .pagination, .hero-actions { display: flex; align-items: center; }
	.hero { justify-content: space-between; gap: 28px; padding: clamp(18px, 3vw, 28px); border: 1px solid var(--border-soft); border-radius: var(--radius-lg, 18px); background: var(--bg-card, var(--bg-elevated)); box-shadow: var(--shadow-sm); }
	h1, h2, p { margin: 0; }
	/* Same display scale as the other primary pages (Reviews, Today, …):
	   clamp(1.7rem, 2vw, 2.35rem) in the theme's display voice. The old
	   clamp(2.3rem, 6vw, 4.7rem) made "Apps" 2-3x every sibling page's hero. */
	h1 { font-family: var(--font-display, var(--font-primary)); font-size: clamp(1.7rem, 2vw, 2.35rem); line-height: 1.05; letter-spacing: 0; }
	.card-head h2 { font-family: var(--font-display, var(--font-primary)); }
	.hero > div > p:last-child { max-width: 650px; margin-top: 10px; color: var(--text-secondary); font-size: .92rem; line-height: 1.5; }
	.eyebrow { margin-bottom: 8px; color: var(--accent-primary); font-size: .72rem; font-weight: 800; letter-spacing: .12em; text-transform: uppercase; }
	.create, button, input, select { font: inherit; } .create, button { border: 1px solid var(--border-soft); border-radius: var(--radius-md, 8px); background: var(--bg-elevated); color: var(--text-primary); padding: 7px 12px; text-decoration: none; cursor: pointer; }
	.create { background: var(--accent-primary); color: var(--text-on-accent, #fff); border-color: transparent; }
	.hero-actions { gap: 8px; } .visually-hidden { position: absolute; width: 1px; height: 1px; overflow: hidden; clip: rect(0 0 0 0); white-space: nowrap; }
	.controls { align-items: flex-end; justify-content: space-between; gap: 20px; margin: 24px 0 18px; }
	/* Controls bar runs at a smaller scale than the page's default button/input
	   sizing. Scoped to .tabs/.search so cards, review panels and pagination
	   keep the sizes they already had. */
	.tabs button { font-size: .76rem; padding: 5px 11px; }
	.search input { font-size: .78rem; padding: 6px 9px; }
	.search input::placeholder { font-size: .78rem; }
	.search button { display: inline-flex; align-items: center; justify-content: center; padding: 6px 10px; }
	.search button svg { display: block; width: 14px; height: 14px; }
	.tabs { display: flex; flex-wrap: wrap; gap: 7px; } .tabs button.active { background: var(--accent-primary); color: var(--accent-contrast, white); border-color: transparent; }
	.search { min-width: min(100%, 300px); } .search label { display: block; margin-bottom: 5px; color: var(--text-secondary); font-size: .75rem; } .search div { display: flex; gap: 7px; } .search div input { flex: 1; }
	.search input, select { min-width: 0; border: 1px solid var(--border-soft); border-radius: 10px; background: var(--bg-elevated); color: var(--text-primary); padding: 8px 10px; }
	/* Tabular rows, not a card grid: the directory is scanned far more often
	   than it is browsed, and one installation per line keeps versions,
	   status and size comparable down a column. */
	.rows { display: flex; flex-direction: column; gap: 6px; min-height: 180px; } .rows.loading { opacity: .58; }
	.app-row { display: flex; min-width: 0; flex-direction: column; gap: 7px; padding: 10px 14px; border: 1px solid var(--border-soft); border-radius: 12px; background: var(--bg-elevated); }
	.app-row:hover { border-color: color-mix(in srgb, var(--accent-primary) 22%, var(--border-soft)); }
	.app-row.expanded { gap: 10px; box-shadow: var(--shadow-sm); border-color: color-mix(in srgb, var(--accent-primary) 34%, var(--border-soft)); }
	/* Head is one dense line: glyph · name/description · inline meta · status · expander. */
	.card-head { align-items: center; gap: 10px; }
	.row-title { flex: 1; min-width: 0; }
	.row-title h2 { font-size: .95rem; font-weight: 700; line-height: 1.25; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
	.row-title p { margin-top: 1px; color: var(--text-secondary); font-size: .78rem; line-height: 1.35; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
	.monogram { display: grid; flex: none; width: 34px; height: 34px; place-items: center; border-radius: 9px; background: color-mix(in srgb, var(--accent-primary) 14%, var(--bg-soft)); color: var(--accent-primary); font-size: .95rem; font-weight: 700; }
	/* Status speaks in the theme's semantic state colors. The class is
	   `app-status`, NOT `status`: daisyUI ships a `.status` utility (an 8×8px
	   dot) whose fixed width/height crushed this pill and made the label
	   overshoot its container. */
	.app-status { flex: none; padding: 3px 9px; border-radius: 999px; background: var(--bg-soft); color: var(--text-secondary); font-size: .72rem; font-weight: 650; line-height: 1.3; text-transform: capitalize; white-space: nowrap; }
	.app-status[data-status='enabled'] { background: var(--color-success-soft, var(--bg-soft)); color: var(--color-success, var(--text-secondary)); }
	.app-status[data-status='disabled'] { background: var(--bg-soft); color: var(--text-muted); }
	.app-status[data-status='update_pending'] { background: var(--color-info-soft, var(--bg-soft)); color: var(--color-info, var(--text-secondary)); }
	.app-status[data-status='uninstalled_retained'] { background: var(--color-warning-soft, var(--bg-soft)); color: var(--color-warning, var(--text-secondary)); }
	.app-status.attention { background: var(--color-warning-soft, var(--bg-soft)); color: var(--color-warning, #9a6410); }
	/* Quiet inline meta beside the title — the row's data at a glance. */
	.meta-inline { display: inline-flex; flex: none; align-items: center; gap: 0; max-width: 34%; color: var(--text-muted); font-size: .72rem; font-family: var(--font-data, var(--font-mono)); white-space: nowrap; overflow: hidden; }
	.meta-inline span + span::before { content: '·'; margin: 0 7px; color: var(--text-faint, var(--text-muted)); }
	/* Expander: proper square hit target at the row's right edge. */
	.disclose { flex: none; display: grid; place-items: center; width: 26px; height: 26px; padding: 0; border: 0; border-radius: 7px; background: none; color: var(--text-muted); cursor: pointer; transition: transform 160ms var(--ease-settle, ease), background 140ms ease, color 140ms ease; }
	.disclose svg { display: block; width: 14px; height: 14px; transform: rotate(-90deg); transition: transform 160ms var(--ease-settle, ease); }
	.disclose:hover { color: var(--text-primary); background: var(--bg-soft); }
	.app-row.expanded .disclose svg { transform: rotate(0deg); }
	/* Expanded detail: views and actions read as compact chip rows, indented
	   under the title column. */
	.views, .actions { display: flex; flex-wrap: wrap; gap: 6px; margin-left: 44px; }
	.view-row, .action-row { display: flex; align-items: stretch; border: 1px solid var(--border-soft); border-radius: var(--radius-md, 8px); background: var(--bg-card, var(--bg-soft)); overflow: hidden; }
	.view-link { padding: 4px 10px; border: 0; border-radius: 0; background: transparent; font-weight: 650; font-size: .78rem; }
	.actions button { padding: 4px 10px; border: 0; background: transparent; color: var(--text-secondary); font-size: .74rem; }
	.pin { display: grid; place-items: center; padding: 0 8px; border: 0; border-left: 1px solid var(--border-soft); border-radius: 0; background: transparent; color: var(--text-muted); } .pin.pinned { color: var(--accent-primary); }
	.action-backdrop { position: fixed; z-index: 1200; inset: 0; display: grid; place-items: center; padding: 16px; background: rgb(0 0 0 / .52); }
	.action-dialog { width: min(560px, 100%); max-height: min(760px, calc(100vh - 32px)); overflow: auto; padding: 20px; border: 1px solid var(--border-soft); border-radius: 18px; background: var(--bg-elevated); box-shadow: var(--shadow-lg); }
	.action-dialog header { display: flex; align-items: flex-start; justify-content: space-between; gap: 16px; margin-bottom: 16px; } .action-dialog header button { padding: 3px 9px; font-size: 1.2rem; }
	.action-form { display: grid; gap: 12px; } .action-field { display: grid; gap: 5px; color: var(--text-secondary); font-size: .78rem; } .action-field input:not([type='checkbox']), .action-field select, .action-field textarea { width: 100%; border: 1px solid var(--border-soft); border-radius: 10px; background: var(--bg-soft); color: var(--text-primary); padding: 8px 10px; font: inherit; }
	.field-hint { margin: -8px 0 0; color: var(--text-muted); font-size: .72rem; }
	.null-field { display: flex; align-items: center; gap: 6px; margin-top: -7px; color: var(--text-muted); font-size: .72rem; } .action-state { margin-top: 10px; color: var(--text-secondary); font-size: .8rem; overflow-wrap: anywhere; }
	.action-run { display: grid; gap: 8px; margin-top: 12px; padding: 10px; border: 1px solid var(--border-soft); border-radius: 12px; color: var(--text-secondary); font-size: .78rem; } .action-run-controls, .action-run-identity { display: flex; align-items: center; gap: 8px; } .action-run-identity code { flex: 1; min-width: 0; overflow-wrap: anywhere; user-select: all; } .action-run pre { max-height: 240px; overflow: auto; margin: 0; padding: 9px; border-radius: 8px; background: var(--bg-soft); white-space: pre-wrap; overflow-wrap: anywhere; }
	.action-run-controls { flex-wrap: wrap; } .action-run-controls span { flex: 1 1 220px; } .action-error { display: grid; gap: 4px; padding: 9px; border: 1px solid color-mix(in srgb, var(--color-error, #c43f3f) 35%, var(--border-soft)); border-radius: 9px; } .action-error p { margin: 0; } .action-error span { color: var(--text-muted); font-size: .7rem; } .action-result-meta { display: flex; flex-wrap: wrap; gap: 5px; } .action-result-meta span { padding: 3px 6px; border-radius: 999px; background: var(--bg-soft); color: var(--text-muted); font-size: .72rem; } .action-receipts ul { margin: 7px 0 0; padding-left: 18px; } .action-receipts code { overflow-wrap: anywhere; }
	.recent-action-runs { display: grid; gap: 6px; margin-top: 12px; } .recent-action-runs > p { color: var(--text-muted); font-size: .72rem; font-weight: 700; text-transform: uppercase; } .recent-action-runs button { display: grid; grid-template-columns: auto minmax(0, 1fr); gap: 8px; text-align: left; } .recent-action-runs code { overflow: hidden; text-overflow: ellipsis; }
	.attention-copy, .error { color: var(--color-error, #c43f3f); } .attention-copy { font-size: .8rem; }
	.review { display: grid; gap: 10px; } .review-copy { color: var(--text-secondary); font-size: .8rem; line-height: 1.45; }
	.review fieldset { margin: 0; padding: 8px 10px; border: 1px solid var(--border-soft); border-radius: 12px; display: grid; gap: 6px; }
	.review legend { padding: 0 4px; color: var(--text-muted); font-size: .7rem; text-transform: uppercase; letter-spacing: .06em; }
	.review label { display: flex; align-items: center; gap: 8px; font-size: .82rem; flex-wrap: wrap; }
	.review-note { color: var(--text-muted); font-size: .72rem; text-transform: uppercase; letter-spacing: .04em; }
	.review-inert { padding: 8px 10px; border: 1px solid color-mix(in srgb, var(--color-warning, #9a6410) 35%, var(--border-soft)); border-radius: 12px; }
	.review-inert ul { margin: 6px 0 0; padding-left: 18px; color: var(--text-secondary); font-size: .8rem; }
	.review-memory table { width: 100%; border-collapse: collapse; font-size: .82rem; margin: 6px 0; }
	.review-memory th, .review-memory td { padding: 5px 6px; text-align: left; border-top: 1px solid var(--border-soft, rgba(127, 127, 127, .2)); }
	.review-memory td { text-align: center; width: 9rem; }
	.review-secrets table { width: 100%; border-collapse: collapse; font-size: .82rem; margin: 6px 0; }
	.review-secrets th, .review-secrets td { padding: 5px 6px; text-align: left; border-top: 1px solid var(--border-soft, rgba(127, 127, 127, .2)); }
	.review-secrets td:last-child { text-align: center; width: 5rem; }
	.review-sensitive { margin-left: 6px; padding: 1px 6px; border-radius: 999px; font-size: .7rem; background: color-mix(in srgb, var(--accent-warning, #d97706) 18%, transparent); color: var(--text-primary); }
	.review-policy { font-size: .78rem; color: var(--text-secondary); } .review-policy summary { cursor: pointer; color: var(--text-muted); } .review-policy ul { margin: 7px 0 0; padding-left: 18px; } .review-policy code { overflow-wrap: anywhere; }
	.review-matrix { min-width: 0; overflow-x: auto; } .review-matrix table { width: 100%; border-collapse: collapse; font-size: .72rem; } .review-matrix th, .review-matrix td { padding: 6px; border-bottom: 1px solid var(--border-soft); text-align: left; vertical-align: top; } .review-matrix thead th { color: var(--text-muted); text-transform: uppercase; letter-spacing: .04em; } .review-matrix td span { padding: 2px 5px; border-radius: 999px; background: var(--bg-soft); } .review-matrix td span.denied { color: var(--text-muted); } .review-matrix td span.conditional { color: var(--status-warning, #9a6700); } .captured-content-note { margin-top: 8px; color: var(--text-muted); font-size: .72rem; line-height: 1.45; }
	.lifecycle-controls { border-top: 1px solid var(--border-soft); padding-top: 8px; color: var(--text-muted); font-size: .72rem; } .lifecycle-controls summary { cursor: pointer; font-weight: 700; } .lifecycle-controls > div { display: flex; flex-wrap: wrap; gap: 6px; margin-top: 8px; } .lifecycle-controls button { padding: 5px 8px; font-size: .72rem; } .lifecycle-controls button.danger { color: var(--color-error, #c43f3f); } .lifecycle-controls p { margin-top: 6px; line-height: 1.4; }
	.lifecycle-message { margin-bottom: 14px; padding: 10px 12px; border: 1px solid var(--border-soft); border-radius: 10px; background: var(--bg-elevated); color: var(--text-secondary); }
	.purge-dialog { width: min(900px, calc(100vw - 32px)); max-height: min(860px, calc(100vh - 32px)); overflow: auto; border: 1px solid var(--border-soft); border-radius: 16px; /* --bg-primary is not a token this app defines (the surfaces are
	   --bg-base/--bg-card/--bg-elevated/--bg-soft), so this resolved to
	   nothing and both dialogs rendered transparent over the backdrop. */
	background: var(--bg-elevated); padding: 18px; box-shadow: 0 20px 70px rgb(0 0 0 / .34); }
	.purge-dialog > header, .purge-dialog > footer { display: flex; align-items: center; justify-content: space-between; gap: 12px; }
	.purge-dialog > footer { justify-content: flex-end; margin-top: 16px; }
	/* `confirmPurge` keeps the dialog open on failure, and `lifecycleMessage`
	   renders on the page BEHIND the backdrop — so a rejected purge looked
	   like the button did nothing. Show the reason where the click was. */
	.dialog-error { margin-top: 14px; border: 1px solid color-mix(in srgb, var(--color-error, #c43f3f) 35%, var(--border-soft)); border-radius: 10px;
		background: color-mix(in srgb, var(--color-error, #c43f3f) 8%, var(--bg-elevated)); color: var(--text-primary); padding: 10px 12px; font-size: .82rem; }
	.purge-dialog p { color: var(--text-secondary); line-height: 1.5; }
	.purge-table-wrap { max-height: 420px; overflow: auto; border: 1px solid var(--border-soft); border-radius: 10px; }
	.purge-table-wrap table { width: 100%; border-collapse: collapse; font-size: .78rem; }
	.purge-table-wrap th, .purge-table-wrap td { padding: 8px 10px; border-bottom: 1px solid var(--border-soft); text-align: left; }
	.purge-dialog button.danger { color: var(--color-error, #c43f3f); border-color: currentColor; }
	.app-row footer { justify-content: space-between; gap: 12px; margin-top: auto; } footer time { color: var(--text-muted); font-size: .72rem; } .open { background: var(--accent-primary); color: var(--text-on-accent, #fff); border-color: transparent; }
	.app-row footer button { padding: 4px 11px; font-size: .78rem; font-weight: 650; }
	.empty { grid-column: 1 / -1; display: grid; place-items: center; align-content: center; gap: 6px; min-height: 180px; border: 1px dashed var(--border-soft); border-radius: 18px; color: var(--text-secondary); text-align: center; }
	.error { margin-bottom: 14px; padding: 10px 12px; border: 1px solid color-mix(in srgb, currentColor 35%, transparent); border-radius: 10px; background: var(--bg-elevated); }
	.pagination { justify-content: space-between; gap: 16px; margin-top: 18px; } .pagination > label { display: flex; align-items: center; gap: 8px; color: var(--text-secondary); font-size: .78rem; }
	button:disabled { cursor: default; opacity: .52; }
	@media (max-width: 900px) { .meta-inline { display: none; } }
	@media (max-width: 760px) { .hero, .controls, .pagination { align-items: stretch; flex-direction: column; } .create { align-self: flex-start; } .search { width: 100%; } .search input { flex: 1; } .views, .actions { margin-left: 0; } }
</style>
