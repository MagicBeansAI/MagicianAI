<script lang="ts">
	import { browser } from '$app/environment';
	import { onDestroy, onMount } from 'svelte';
	import MuijRenderer from '$lib/magician/components/generative/MuijRenderer.svelte';
	import ServerPager from '$lib/shared/components/ServerPager.svelte';
	import type { MuijInteractionEventDetail } from '$lib/stores/muijStore';
	import { getV2EventSequence, v2Events, type V2WebSocketEvent } from '$lib/realtime/v2-websocket';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import AppSurfaceEditor from './AppSurfaceEditor.svelte';
	import AppSlotRegion from './AppSlotRegion.svelte';
	import AppDeclarativeSurface from './AppDeclarativeSurface.svelte';
	import AppCustomSurfaceHost from './AppCustomSurfaceHost.svelte';
	import AppScriptedSurfaceHost from './AppScriptedSurfaceHost.svelte';
	import { fetchAppDirectory } from './appDirectory';
	import { appNavigationFallbackRoute } from './appNavigation';
	import {
		fetchCustomSurfaceHost,
		type AppCustomSurfaceHostEnvelope
	} from './appCustomSurface';
	import {
		fetchScriptedSurfaceHost,
		type AppScriptedSurfaceHostPlan
	} from './appScriptedSurface';
	import { appSurfaceSlotPage } from './appWidgets';
	import {
		AppSurfaceClientError,
		appSurfaceHostFallbackAllowed,
		appChangeSignalDisposition,
		appSurfaceMutationFailureKind,
		appSurfaceRealtimeSupported,
		buildSurfaceMutationRequest,
		diffAppSurfaceValues,
		fetchAppEntityChanges,
		fetchAppSurface,
		hydrateAppSurface,
		mutateAppSurface,
		newClientMutationId,
		optimisticAppSurfaceRecords,
		parseAppEntityChangeEvent,
		retainedAppSurfaceMutationRequest,
		resolveAppChangeHead,
		surfaceInteractionIsAppOwned,
		type AppSurfaceHydration,
		type AppSurfaceMutationRequest,
		type AppSurfaceRecord,
		type AppSurfaceRenderPage
	} from './appSurfaceRuntime';

	export let installationId: string;
	export let surfacePath: string = '';

	const PAGE_SIZE = 25;
	let hydration: AppSurfaceHydration | null = null;
	let rendered: AppSurfaceRenderPage | null = null;
	let customEnvelope: AppCustomSurfaceHostEnvelope | null = null;
	let scriptedPlan: AppScriptedSurfaceHostPlan | null = null;
	let fallbackSurfacePath: string | null = null;
	let fallbackNotice = '';
	let nativeReviewLink = false;
	let nativeLinkController: AbortController | null = null;
	let loading = true;
	let saving = false;
	let error = '';
	let editorMode: 'closed' | 'create' | 'edit' = 'closed';
	let editingRecord: AppSurfaceRecord | null = null;
	let pageIndex = 0;
	let cursors: Array<string | undefined> = [undefined];
	let sortField = '';
	let sortDirection: 'ascending' | 'descending' | undefined;
	let requestController: AbortController | null = null;
	let changeController: AbortController | null = null;
	let activeContextKey = '';
	let lastChangeSequence = 0;
	let lastObservedEventSequence = 0;
	let eventBridgeInitialized = false;
	let eventUnsubscribe: (() => void) | null = null;
	let connectionUnsubscribe: (() => void) | null = null;
	let changeSyncTimer: ReturnType<typeof setTimeout> | null = null;
	let changeSyncDueAt = 0;
	let changeSyncRunning = false;
	let changeSyncQueued = false;
	let changeSyncRetryAttempt = 0;
	let changeSyncNotBefore = 0;
	let changePollTimer: ReturnType<typeof setInterval> | null = null;
	let canonicalReloadTimer: ReturnType<typeof setTimeout> | null = null;
	let canonicalReloadAttempt = 0;
	type PendingMutation = {
		contextKey: string;
		request: AppSurfaceMutationRequest;
		previousHydration: AppSurfaceHydration;
		previousRendered: AppSurfaceRenderPage | null;
	};
	let pendingMutation: PendingMutation | null = null;

	$: routeKey = JSON.stringify([installationId, surfacePath]);
	$: scopeKey = JSON.stringify([
		$scopeIdentityStore.principal,
		$scopeIdentityStore.workspace
	]);
	$: contextKey = JSON.stringify([scopeKey, routeKey]);
	$: title = hydration?.surface.view_id
		? hydration.surface.view_id.split(/[_-]+/g).map((part) => part.charAt(0).toUpperCase() + part.slice(1)).join(' ')
		: scriptedPlan || customEnvelope
			? 'Custom surface'
			: 'App';
	$: canMutate = rendered !== null
		&& rendered.view.viewKind !== 'timeline'
		&& !customEnvelope
		&& !scriptedPlan;
	$: usesExternalPager = rendered?.view.viewKind === 'timeline'
		|| Boolean(rendered?.view.surfaceComponents.length);
	$: usesHeaderCreate = canMutate && !rendered?.view.surfaceComponents.length;
	$: externalPageCount = pageIndex + 1 + (rendered?.hasNextPage ? 1 : 0);
	$: externalTotalItems = pageIndex * PAGE_SIZE + (rendered?.records.length ?? 0);
	$: editorKey = `${editorMode}:${editingRecord?.record_id ?? 'new'}`;
	$: contextualSlotPage = appSurfaceSlotPage(installationId, surfacePath);

	function applyNativeSurface(
		next: AppSurfaceHydration,
		index: number,
		previousSurfaceRevision: number | undefined,
		replaceChangeHead: boolean
	): void {
		const nextRendered = hydrateAppSurface(next, {
			page: index + 1,
			pageSize: PAGE_SIZE,
			hasNextPage: Boolean(next.page.next_cursor),
			sortField: sortField || undefined,
			sortDirection
		});
		const nextCursor = next.page.next_cursor;
		cursors = nextCursor
			? [...cursors.slice(0, index + 1), nextCursor]
			: cursors.slice(0, index + 1);
		pageIndex = index;
		hydration = next;
		scriptedPlan = null;
		lastChangeSequence = resolveAppChangeHead(
			lastChangeSequence, previousSurfaceRevision, next.surface.surface_revision,
			next.change_sequence, replaceChangeHead
		);
		rendered = nextRendered;
		customEnvelope = null;
		if (canonicalReloadTimer) {
			clearTimeout(canonicalReloadTimer);
			canonicalReloadTimer = null;
		}
		canonicalReloadAttempt = 0;
	}

	async function load(index = pageIndex, replaceChangeHead = false): Promise<boolean> {
		nativeLinkController?.abort();
		requestController?.abort();
		const controller = new AbortController();
		requestController = controller;
		const requestedContextKey = contextKey;
		nativeReviewLink = false;
		loading = true;
		error = '';
		const previousSurfaceRevision = hydration?.surface.surface_revision
			?? customEnvelope?.surface_revision;
		const previousCustomSession = customEnvelope?.session_ref;
		try {
			const next = await fetchAppSurface(installationId, fallbackSurfacePath ?? surfacePath, {
				cursor: cursors[index],
				sortField: sortField || undefined,
				sortDirection,
				signal: controller.signal
			});
			if (controller.signal.aborted || requestedContextKey !== contextKey) return false;
			applyNativeSurface(next, index, previousSurfaceRevision, replaceChangeHead);
			return true;
		} catch (cause) {
			if (controller.signal.aborted || requestedContextKey !== contextKey) return false;
			if (fallbackSurfacePath !== null || !appSurfaceHostFallbackAllowed(cause, 'native')) {
				error = cause instanceof Error ? cause.message : 'The app surface could not be loaded.';
				return false;
			}
			// Plan 1.6: a package declaring the custom_surface permission
			// hosts its scripted entry document in a sandboxed frame first —
			// at the installation root and at every declared surface route
			// (`?route=` selects the entry point); every other package falls
			// through to the unchanged Phase 7 no-script host exactly as
			// before. The generic surface error renders only when every
			// host path has refused.
			try {
				const plan = await fetchScriptedSurfaceHost(installationId, {
					route: surfacePath ? `/${surfacePath}` : undefined,
					signal: controller.signal
				});
				if (controller.signal.aborted || requestedContextKey !== contextKey) return false;
				scriptedPlan = plan;
				customEnvelope = null;
				hydration = null;
				rendered = null;
				error = '';
				return true;
			} catch (scriptedCause) {
				if (controller.signal.aborted || requestedContextKey !== contextKey) return false;
				if (!appSurfaceHostFallbackAllowed(scriptedCause, 'scripted')) {
					error = scriptedCause instanceof Error ? scriptedCause.message : 'The app surface could not be loaded.';
					return false;
				}
				if (surfacePath) {
					try {
						// Match the palette's bounded directory admission set. Resolve the
						// fallback from fresh metadata for this installation, never a guessed URL.
						const directory = await fetchAppDirectory({ section: 'installed', limit: 48, signal: controller.signal });
						if (controller.signal.aborted || requestedContextKey !== contextKey) return false;
						const source = directory.entries.find((entry) => entry.installation_id === installationId);
						const route = source ? appNavigationFallbackRoute(source, `/${surfacePath}`) : null;
						if (route !== null) {
							const path = route.slice(1);
							const next = await fetchAppSurface(installationId, path, {
								sortField: sortField || undefined, sortDirection, signal: controller.signal
							});
							if (controller.signal.aborted || requestedContextKey !== contextKey) return false;
							applyNativeSurface(next, 0, previousSurfaceRevision, true);
							fallbackSurfacePath = path;
							fallbackNotice = 'The interactive page is unavailable. Showing this app’s standard view.';
							return true;
						}
					} catch (fallbackCause) {
						if (controller.signal.aborted || requestedContextKey !== contextKey) return false;
						error = fallbackCause instanceof Error ? fallbackCause.message : 'The app view could not be loaded.';
						return false;
					}
					error = scriptedCause instanceof Error ? scriptedCause.message : 'The app surface could not be loaded.';
					return false;
				}
			}
			if (!surfacePath) {
				try {
					const envelope = await fetchCustomSurfaceHost(installationId, {
						replaceSession: previousCustomSession,
						signal: controller.signal
					});
					if (controller.signal.aborted || requestedContextKey !== contextKey) return false;
					customEnvelope = envelope;
					hydration = null;
					rendered = null;
					lastChangeSequence = resolveAppChangeHead(
						lastChangeSequence,
						previousSurfaceRevision,
						envelope.surface_revision,
						envelope.change_sequence,
						replaceChangeHead
					);
					error = '';
					return true;
				} catch (customCause) {
					if (controller.signal.aborted) return false;
					error = customCause instanceof Error
						? customCause.message
						: cause instanceof Error
							? cause.message
							: 'The app surface could not be loaded.';
					return false;
				}
			}
			error = cause instanceof Error ? cause.message : 'The app surface could not be loaded.';
			return false;
		} finally {
			if (requestController === controller) {
				if (!error && !controller.signal.aborted) {
					nativeLinkController = controller;
					// Navigation only: the sandbox never gains owner API authority.
					void fetchAppDirectory({ section: 'installed', limit: 48, signal: controller.signal }).then((directory) => {
						if (controller.signal.aborted || requestedContextKey !== contextKey) return;
						nativeReviewLink = directory.entries.some((entry) => entry.installation_id === installationId
							&& entry.status === 'enabled' && entry.navigation?.some((nav) => nav.id === 'claims_review' && nav.route === '/claims-review'));
					}).catch(() => {});
				}
				requestController = null;
				loading = false;
				if (changeSyncQueued) scheduleChangeSync();
			}
		}
	}

	function scheduleChangeSync(delayMs = 80): void {
		changeSyncQueued = true;
		if (changeSyncRunning || !browser) return;
		const dueAt = Math.max(Date.now() + Math.max(0, delayMs), changeSyncNotBefore);
		if (changeSyncTimer && changeSyncDueAt <= dueAt) return;
		if (changeSyncTimer) clearTimeout(changeSyncTimer);
		changeSyncDueAt = dueAt;
		changeSyncTimer = setTimeout(() => {
			changeSyncTimer = null;
			changeSyncDueAt = 0;
			void synchronizeChanges();
		}, Math.max(0, dueAt - Date.now()));
	}

	async function synchronizeChanges(): Promise<void> {
		if (changeSyncRunning) return;
		if ((!hydration && !customEnvelope) || scriptedPlan || loading || saving) return;
		const syncSurfaceRevision = hydration?.surface.surface_revision
			?? customEnvelope?.surface_revision;
		if (!syncSurfaceRevision) return;
		changeSyncQueued = false;
		changeSyncRunning = true;
		changeController?.abort();
		const controller = new AbortController();
		changeController = controller;
		const syncContextKey = contextKey;
		const syncAfter = lastChangeSequence;
		let retryDelay: number | null = null;
		try {
			const batch = await fetchAppEntityChanges(
				installationId,
				syncSurfaceRevision,
				syncAfter,
				controller.signal
			);
			if (controller.signal.aborted || contextKey !== syncContextKey) return;
			if (
				batch.reset_required
				|| batch.has_more
				|| batch.through_change_sequence > syncAfter
			) {
				if (!(await reloadFromStart())) {
					throw new Error('The canonical app surface could not be refreshed.');
				}
			}
			changeSyncRetryAttempt = 0;
			changeSyncNotBefore = 0;
		} catch (cause) {
			if (controller.signal.aborted || contextKey !== syncContextKey) return;
			let needsRetry = true;
			if (cause instanceof AppSurfaceClientError && (cause.status === 409 || cause.status === 410)) {
				if (await reloadFromStart()) {
					changeSyncRetryAttempt = 0;
					changeSyncNotBefore = 0;
					needsRetry = false;
				} else {
					error = 'The app changed, but its canonical view could not be refreshed yet.';
				}
			} else {
				error = cause instanceof Error ? cause.message : 'The app changes could not be synchronized.';
			}
			if (needsRetry) {
				changeSyncRetryAttempt = Math.min(changeSyncRetryAttempt + 1, 6);
				retryDelay = Math.min(1_000 * 2 ** (changeSyncRetryAttempt - 1), 15_000);
				changeSyncNotBefore = Date.now() + retryDelay;
				changeSyncQueued = true;
			}
		} finally {
			if (changeController === controller) changeController = null;
			changeSyncRunning = false;
			if (changeSyncQueued) scheduleChangeSync(retryDelay ?? 80);
		}
	}

	function observeRealtimeEvents(events: V2WebSocketEvent[]): void {
		if (!eventBridgeInitialized) {
			lastObservedEventSequence = events.reduce(
				(maximum, event) => Math.max(maximum, getV2EventSequence(event)),
				lastObservedEventSequence
			);
			eventBridgeInitialized = true;
			return;
		}
		let nextObserved = lastObservedEventSequence;
		let reloadRequired = false;
		let syncRequired = false;
		for (const event of events) {
			const sequence = getV2EventSequence(event);
			if (sequence <= lastObservedEventSequence) continue;
			if (lastObservedEventSequence > 0 && sequence > lastObservedEventSequence + 1) {
				// The websocket is only a bounded wake-up hint. A transport gap
				// always schedules the authoritative per-installation sequence read.
				syncRequired = true;
			}
			nextObserved = Math.max(nextObserved, sequence);
			const surfaceRevision = hydration?.surface.surface_revision
				?? customEnvelope?.surface_revision;
			const signal = parseAppEntityChangeEvent(event, installationId, surfaceRevision);
			if (signal) {
				const disposition = appChangeSignalDisposition(lastChangeSequence, signal);
				if (disposition === 'reset') reloadRequired = true;
				else if (disposition === 'synchronize') syncRequired = true;
			}
		}
		lastObservedEventSequence = nextObserved;
		if (reloadRequired) void reloadFromStart();
		else if (syncRequired) scheduleChangeSync();
	}

	async function reloadFromStart(): Promise<boolean> {
		cursors = [undefined];
		pageIndex = 0;
		// Rebuild/reset signals can legitimately move the canonical sequence head
		// behind a stale client cursor without changing the surface revision.
		return load(0, true);
	}

	function scheduleCanonicalReload(): void {
		if (!browser || canonicalReloadTimer) return;
		const delay = Math.min(1_000 * 2 ** canonicalReloadAttempt, 15_000);
		canonicalReloadAttempt = Math.min(canonicalReloadAttempt + 1, 6);
		canonicalReloadTimer = setTimeout(async () => {
			canonicalReloadTimer = null;
			if (await reloadFromStart()) {
				canonicalReloadAttempt = 0;
				return;
			}
			scheduleCanonicalReload();
		}, delay);
	}

	async function goToPage(page: number): Promise<void> {
		const target = Math.floor(page) - 1;
		if (target >= 0 && target < cursors.length) await load(target);
	}

	function recordById(id: string): AppSurfaceRecord | null {
		return rendered?.records.find((record) => record.record_id === id) ?? null;
	}

	async function handleInteraction(event: CustomEvent<MuijInteractionEventDetail>): Promise<void> {
		const interaction = event.detail;
		if (!surfaceInteractionIsAppOwned(interaction) || loading || saving || pendingMutation) return;
		const action = typeof interaction.detail.action === 'string' ? interaction.detail.action : '';
		if (action === 'page' && typeof interaction.detail.page === 'number') {
			await goToPage(interaction.detail.page);
			return;
		}
		if (action === 'sort') {
			const field = typeof interaction.detail.sortKey === 'string' ? interaction.detail.sortKey : '';
			const direction = interaction.detail.sortDir === 'desc' ? 'descending' : 'ascending';
			if (field) {
				sortField = field;
				sortDirection = direction;
				await reloadFromStart();
			}
			return;
		}
		if (interaction.interaction !== 'action') return;
		const rowId = typeof interaction.detail.rowId === 'string' ? interaction.detail.rowId : '';
		const actionId = typeof interaction.detail.actionId === 'string' ? interaction.detail.actionId : '';
		const record = recordById(rowId);
		if (!record) return;
		if (actionId === 'edit' || action === 'select') {
			editingRecord = record;
			editorMode = 'edit';
		} else if (actionId === 'delete' && browser && window.confirm('Delete this record?')) {
			await submitMutation({
				kind: 'delete',
				record_id: record.record_id,
				expected_record_revision: record.record_revision
			});
		}
	}

	function applyOptimisticMutation(pending: PendingMutation): void {
		if (!pending.previousRendered) return;
		const optimisticRecords = optimisticAppSurfaceRecords(
			pending.previousRendered.records,
			pending.previousRendered.view.entity,
			pending.request.client_mutation_id,
			pending.request.operation,
			PAGE_SIZE
		);
		hydration = {
			...pending.previousHydration,
			page: {
				...pending.previousHydration.page,
				envelope: {
					...pending.previousHydration.page.envelope,
					value: optimisticRecords
				}
			}
		};
		rendered = hydrateAppSurface(hydration, {
			page: pageIndex + 1,
			pageSize: PAGE_SIZE,
			hasNextPage: Boolean(hydration.page.next_cursor),
			sortField: sortField || undefined,
			sortDirection
		});
	}

	async function executePendingMutation(pending: PendingMutation): Promise<void> {
		if (pending.contextKey !== contextKey) return;
		pendingMutation = pending;
		applyOptimisticMutation(pending);
		saving = true;
		error = '';
		try {
			await mutateAppSurface(installationId, pending.request);
			if (contextKey !== pending.contextKey) return;
			pendingMutation = null;
			editorMode = 'closed';
			editingRecord = null;
			if (!(await reloadFromStart())) {
				error = 'Saved. The canonical view is temporarily unavailable and will refresh automatically.';
				scheduleCanonicalReload();
			}
		} catch (cause) {
			if (contextKey !== pending.contextKey) return;
			hydration = pending.previousHydration;
			rendered = pending.previousRendered;
			const failureKind = appSurfaceMutationFailureKind(cause);
			if (failureKind === 'stale') {
				pendingMutation = null;
				const refreshed = await reloadFromStart();
				if (refreshed) {
					error = `${cause instanceof Error ? cause.message : 'The app changed.'} The surface has been refreshed.`;
				} else {
					error = `${cause instanceof Error ? cause.message : 'The app changed.'} The canonical surface is temporarily unavailable and will refresh automatically.`;
					scheduleCanonicalReload();
				}
			} else if (failureKind === 'ambiguous') {
				// The server may have committed before the transport failed. Retain
				// the exact idempotency key so an explicit retry cannot duplicate it.
				pendingMutation = retainedAppSurfaceMutationRequest(pending.request, cause)
					? pending
					: null;
				error = 'The save result could not be confirmed. Retry the same save safely.';
			} else {
				pendingMutation = null;
				error = cause instanceof Error ? cause.message : 'The app edit could not be saved.';
			}
		} finally {
			saving = false;
			if (changeSyncQueued) scheduleChangeSync();
		}
	}

	async function retryPendingMutation(): Promise<void> {
		if (saving || !pendingMutation) return;
		await executePendingMutation(pendingMutation);
	}

	async function submitMutation(operation: Parameters<typeof buildSurfaceMutationRequest>[2]): Promise<void> {
		if (!hydration) return;
		if (pendingMutation) {
			await retryPendingMutation();
			return;
		}
		const pending: PendingMutation = {
			contextKey,
			request: buildSurfaceMutationRequest(hydration, newClientMutationId(), operation),
			previousHydration: hydration,
			previousRendered: rendered
		};
		await executePendingMutation(pending);
	}

	async function saveEditor(values: Record<string, unknown>): Promise<void> {
		if (editorMode === 'create') {
			await submitMutation({ kind: 'create', values });
			return;
		}
		if (!editingRecord) return;
		const patch = diffAppSurfaceValues(editingRecord.fields, values);
		if (Object.keys(patch).length === 0) {
			editorMode = 'closed';
			editingRecord = null;
			return;
		}
		await submitMutation({
			kind: 'update',
			record_id: editingRecord.record_id,
			expected_record_revision: editingRecord.record_revision,
			patch
		});
	}

	async function deleteEditingRecord(): Promise<void> {
		if (!editingRecord || !browser || !window.confirm('Delete this record?')) return;
		await submitMutation({
			kind: 'delete',
			record_id: editingRecord.record_id,
			expected_record_revision: editingRecord.record_revision
		});
	}

	async function createDeclarativeRecord(event: CustomEvent<{ values: Record<string, unknown> }>): Promise<void> {
		await submitMutation({ kind: 'create', values: event.detail.values });
	}

	function editDeclarativeRecord(event: CustomEvent<{ record: AppSurfaceRecord }>): void {
		editingRecord = event.detail.record;
		editorMode = 'edit';
	}

	async function deleteDeclarativeRecord(event: CustomEvent<{ record: AppSurfaceRecord }>): Promise<void> {
		const record = event.detail.record;
		if (!browser || !window.confirm('Delete this record?')) return;
		await submitMutation({
			kind: 'delete',
			record_id: record.record_id,
			expected_record_revision: record.record_revision
		});
	}

	async function sortDeclarativeSurface(event: CustomEvent<{ field: string; direction: 'ascending' | 'descending' }>): Promise<void> {
		sortField = event.detail.field;
		sortDirection = event.detail.direction;
		await reloadFromStart();
	}

	function handleVisibilityChange(): void {
		if (document.visibilityState === 'visible' && (hydration || customEnvelope)) scheduleChangeSync();
	}

	onMount(() => {
		activeContextKey = contextKey;
		if (appSurfaceRealtimeSupported(window.location.protocol)) {
			eventUnsubscribe = v2Events.subscribe(observeRealtimeEvents);
			connectionUnsubscribe = v2Events.connectionStatus.subscribe((status) => {
				if (status === 'connected' && (hydration || customEnvelope)) scheduleChangeSync();
			});
			if (v2Events.getConnectionState() === 'CLOSED') v2Events.connectGlobal();
		}
		changePollTimer = setInterval(() => {
			if (document.visibilityState === 'visible' && (hydration || customEnvelope)) scheduleChangeSync();
		}, 15_000);
		document.addEventListener('visibilitychange', handleVisibilityChange);
		void reloadFromStart();
		// Detached here rather than in onDestroy: Svelte runs onDestroy during
		// server rendering too, where `document` does not exist.
		return () => document.removeEventListener('visibilitychange', handleVisibilityChange);
	});

	$: if (browser && activeContextKey && contextKey !== activeContextKey) {
		activeContextKey = contextKey;
		nativeLinkController?.abort();
		requestController?.abort();
		changeController?.abort();
		if (changeSyncTimer) {
			clearTimeout(changeSyncTimer);
			changeSyncTimer = null;
			changeSyncDueAt = 0;
		}
		changeSyncQueued = false;
		changeSyncRetryAttempt = 0;
		changeSyncNotBefore = 0;
		hydration = null;
		rendered = null;
		customEnvelope = null;
		scriptedPlan = null;
		fallbackSurfacePath = null;
		fallbackNotice = '';
		error = '';
		lastChangeSequence = 0;
		editorMode = 'closed';
		editingRecord = null;
		sortField = '';
		sortDirection = undefined;
		pendingMutation = null;
		canonicalReloadAttempt = 0;
		if (canonicalReloadTimer) {
			clearTimeout(canonicalReloadTimer);
			canonicalReloadTimer = null;
		}
		void reloadFromStart();
	}

	onDestroy(() => {
		nativeLinkController?.abort();
		requestController?.abort();
		changeController?.abort();
		if (changeSyncTimer) clearTimeout(changeSyncTimer);
		if (changePollTimer) clearInterval(changePollTimer);
		if (canonicalReloadTimer) clearTimeout(canonicalReloadTimer);
		eventUnsubscribe?.();
		connectionUnsubscribe?.();
	});
</script>

<svelte:head><title>{title} · Magican</title></svelte:head>

<main class="app-surface-page">
	<header class="app-surface-header">
		<div>
			<p class="eyebrow">App surface</p>
			<h1>{title}</h1>
		</div>
		<div class="header-actions">
			{#if nativeReviewLink}<a class="quiet" href="/claims-review">Open native Claims Review →</a>{/if}
			{#if sortField}
				<button class="quiet" type="button" disabled={loading} on:click={() => { sortField = ''; sortDirection = undefined; void reloadFromStart(); }}>Clear sort</button>
			{/if}
			<button class="quiet" type="button" disabled={loading} on:click={() => {
				if (fallbackSurfacePath !== null) {
					fallbackSurfacePath = null;
					fallbackNotice = '';
					void reloadFromStart();
				} else void load(pageIndex);
			}}>Refresh</button>
			{#if usesHeaderCreate}
				<button class="primary" type="button" disabled={loading || saving || Boolean(pendingMutation)} on:click={() => { editingRecord = null; editorMode = 'create'; }}>New record</button>
			{/if}
		</div>
	</header>

	{#if contextualSlotPage && (hydration || customEnvelope || scriptedPlan)}
		<AppSlotRegion page={contextualSlotPage} region="contextual" ariaLabel="Contextual app widgets" />
	{/if}

	{#if error}
		<div class="surface-alert" role="alert">
			<span>{error}</span>
			{#if pendingMutation}
				<button class="quiet" type="button" disabled={saving} on:click={retryPendingMutation}>Retry save</button>
			{/if}
		</div>
	{/if}
	{#if fallbackNotice}<p role="status">{fallbackNotice}</p>{/if}

	{#if editorMode !== 'closed' && rendered}
		{#key editorKey}
			<AppSurfaceEditor
				fields={rendered.view.fieldBindings}
				initialValues={editingRecord?.fields ?? {}}
				mode={editorMode === 'edit' ? 'edit' : 'create'}
				{saving}
				allowDelete={editorMode === 'edit'}
				on:save={(event) => saveEditor(event.detail.values)}
				on:delete={deleteEditingRecord}
				on:cancel={() => { editorMode = 'closed'; editingRecord = null; }}
			/>
		{/key}
	{/if}

	<section class="surface-canvas" class:loading aria-busy={loading}>
		{#if loading && !rendered && !customEnvelope && !scriptedPlan}
			<div class="surface-loading"><span></span><p>Loading app…</p></div>
		{:else if customEnvelope || scriptedPlan}
			{#if scriptedPlan}
				{#key scriptedPlan.session_ref}
					<AppScriptedSurfaceHost plan={scriptedPlan} {installationId} />
				{/key}
			{:else if customEnvelope}
				<AppCustomSurfaceHost envelope={customEnvelope} />
			{/if}
		{:else if rendered}
			{#if rendered.view.surfaceComponents.length > 0}
				<AppDeclarativeSurface
					components={rendered.view.surfaceComponents}
					records={rendered.records}
					fieldBindings={rendered.view.fieldBindings}
					{saving}
					{sortField}
					{sortDirection}
					on:create={createDeclarativeRecord}
					on:edit={editDeclarativeRecord}
					on:delete={deleteDeclarativeRecord}
					on:sort={sortDeclarativeSurface}
				/>
			{:else}
				<MuijRenderer
					components={rendered.components}
					agentId=""
					idNamespace={`app-surface-${installationId}`}
					validateRouteContract={false}
					on:interaction={handleInteraction}
				/>
			{/if}
			{#if usesExternalPager && externalPageCount > 1}
				<ServerPager
					currentPage={pageIndex + 1}
					pageCount={externalPageCount}
					pageCountExact={!rendered.hasNextPage}
					startItem={rendered.records.length === 0 ? 0 : pageIndex * PAGE_SIZE + 1}
					endItem={externalTotalItems}
					totalItems={externalTotalItems}
					totalItemsExact={!rendered.hasNextPage}
					{loading}
					ariaLabel="App surface pagination"
					on:pagechange={(event) => goToPage(event.detail.page)}
				/>
			{/if}
		{:else if !error}
			<div class="surface-empty">This app surface is unavailable.</div>
		{/if}
	</section>
</main>

<style>
	.app-surface-page { width: calc(100% - 2rem); max-width: var(--app-content-max, 1320px); margin: 0 auto; padding: clamp(1rem, 2.5vw, 2.2rem) 0 4rem; display: grid; gap: 1rem; font-family: var(--font-primary); }
	.app-surface-header { display: flex; align-items: flex-end; justify-content: space-between; gap: 1rem; }
	.eyebrow { margin: 0 0 .2rem; color: var(--text-muted); font-size: .72rem; font-weight: 750; letter-spacing: .12em; text-transform: uppercase; }
	h1 { margin: 0; color: var(--text-primary); font-family: var(--font-display, var(--font-primary)); font-size: clamp(1.7rem, 2vw, 2.35rem); letter-spacing: 0; }
	.header-actions { display: flex; flex-wrap: wrap; justify-content: flex-end; gap: .5rem; }
	button { border-radius: var(--radius-full, 999px); padding: .55rem .85rem; font: inherit; font-size: .86rem; font-weight: 650; cursor: pointer; }
	button:disabled { opacity: .55; cursor: default; }
	button.quiet { border: 1px solid var(--border-soft); background: var(--bg-card); color: var(--text-secondary); }
	button.primary { border: 1px solid transparent; background: var(--accent-primary); color: var(--text-on-accent, #fff); }
	.surface-alert { border: 1px solid color-mix(in srgb, var(--color-error, #c43d4d) 32%, var(--border-soft)); border-radius: var(--radius-lg, 14px); padding: .8rem 1rem; background: color-mix(in srgb, var(--color-error, #c43d4d) 8%, var(--bg-card)); color: var(--color-error, #c43d4d); display: flex; align-items: center; justify-content: space-between; gap: .75rem; }
	.surface-canvas { position: relative; min-height: 18rem; border: 1px solid var(--border-soft); border-radius: var(--radius-xl, 18px); background: var(--bg-card); padding: clamp(.7rem, 2vw, 1.2rem); overflow: hidden; }
	.surface-canvas.loading { opacity: .72; pointer-events: none; }
	.surface-loading, .surface-empty { min-height: 16rem; display: grid; place-content: center; justify-items: center; gap: .75rem; color: var(--text-muted); }
	.surface-loading span { width: 1.6rem; height: 1.6rem; border: 2px solid var(--border-soft); border-top-color: var(--accent-primary); border-radius: 50%; animation: spin .8s linear infinite; }
	@keyframes spin { to { transform: rotate(360deg); } }
	@media (max-width: 720px) { .app-surface-page { width: min(100% - 1rem, 1180px); padding-top: .75rem; } .app-surface-header { align-items: flex-start; flex-direction: column; } .header-actions { justify-content: flex-start; } .surface-canvas { padding: .45rem; border-radius: var(--radius-lg, 14px); } }
</style>
