<script lang="ts" context="module">
	import type { AppResolvedSlotAssignment, AppWidgetRenderItem } from './appWidgets';

	/**
	 * Whether a rendered widget carries records of its own.
	 *
	 * `filled` is what a first-party section stands down for (gate M4), so it has
	 * to mean the widget really took the content over. `unavailable` and
	 * `unsupported` render a placeholder — and so does a `ready` widget whose
	 * query projected nothing: `AppNativeWidget` renders "No items yet." for an
	 * empty model exactly as this file renders the placeholder for an absent one.
	 * A section that retreated for that would take its own rows off the page and
	 * leave a blank where they were.
	 *
	 * The native model is what is counted even when a mini-frame is declared
	 * beside it: the runtime compiles the same-view native fallback either way, so
	 * its rows are the widget's content, and a frame this client may still refuse
	 * cannot be the proof that a section is safe to retire.
	 */
	export function widgetFillsRegion(widget: AppWidgetRenderItem): boolean {
		if (widget.state !== 'ready') return false;
		return widget.model.model === 'detail' ? widget.model.row !== null : widget.model.rows.length > 0;
	}

	/**
	 * Whether a slot carries an assignment the viewer should see as one.
	 *
	 * A workspace default that the server hid (package unavailable, identity or
	 * digest changed, widget no longer declared) is not something this viewer
	 * chose, so it reads as an empty slot — the add affordance, no "temporarily
	 * unavailable" placeholder and no Remove. The placeholder stays for the
	 * viewer's own assignment and for a package the viewer can act on (disabled
	 * or waiting on an update).
	 */
	export function slotShowsAssignment(assignment: AppResolvedSlotAssignment): boolean {
		if (!assignment.widget && !assignment.source && !assignment.hidden_reason) return false;
		if (assignment.source === 'workspace_default' && !assignment.widget && assignment.hidden_reason &&
			assignment.hidden_reason !== 'disabled' && assignment.hidden_reason !== 'update_pending') return false;
		return true;
	}
</script>

<script lang="ts">
	import { createEventDispatcher, onDestroy, onMount } from 'svelte';
	import {
		getCurrentScopeCredentialRevision,
		scopeCredentialIdentityIsCurrent,
		scopeIdentityStore
	} from '$lib/stores/scopeIdentityStore';
	import AppNativeWidget from './AppNativeWidget.svelte';
	import { miniFrameSessionLedger, type MiniFramePageLease } from './appMiniFrame';
	import {
		APP_WIDGET_REFRESH_FLOOR_MS,
		AppSlotTransportError,
		appWidgetWithinStaleness,
		fetchAppSlotSettings,
		mutateAppSlotAssignment,
		newAppSlotMutationId,
		renderAppWidgetBatch,
		resolveAppSlots,
		type AppResponseCache,
		type AppSlotAssignmentWriteRequest,
		type AppSlotPickerCandidate,
		type AppSlotSettingsPage,
		type AppWidgetRenderBatchResponse
	} from './appWidgets';

	export let page: string;
	export let region: string | undefined = undefined;
	export let regions: string[] = [];
	export let ariaLabel = 'App widgets';

	/**
	 * `filled` names the regions whose widget currently renders records of its
	 * own. A first-party section that a pinned app widget has taken over needs to
	 * know that it actually happened before it stands down (gate M4); an empty,
	 * unavailable or errored region emits nothing, so the default is always
	 * "the first-party section stays".
	 */
	const dispatch = createEventDispatcher<{ filled: { page: string; regions: string[] } }>();

	type ResolvedRegion = { region: string; assignment: AppResolvedSlotAssignment };
	type RenderedRegion = {
		region: string;
		widget: AppWidgetRenderItem;
		packageRevisionRef: string;
		credentialRevision: number;
	};
	type PendingSlotMutation = {
		scopeKey: string;
		bindingKey: string;
		credentialRevision: number;
		region: string;
		request: AppSlotAssignmentWriteRequest;
	};
	const MAX_ACCUMULATED_PICKER_ITEMS = 512;

	let mounted = false;
	let request: AbortController | null = null;
	let settingsRequest: AbortController | null = null;
	let mutationRequest: AbortController | null = null;
	let refreshTimer: ReturnType<typeof setTimeout> | undefined;
	let appliedScope = '';
	let appliedBinding = '';
	let appliedCredentialRevision = 0;
	let cache: AppResponseCache<AppWidgetRenderBatchResponse> | undefined;
	let cacheBinding = '';
	let resolvedSlots: ResolvedRegion[] = [];
	let rendered: RenderedRegion[] = [];
	// A render batch is in flight. An assigned slot with nothing rendered yet
	// shows a quiet loading state, never the unavailable placeholder.
	let renderPending = false;
	let pickerRegion = '';
	let settings: AppSlotSettingsPage | null = null;
	let settingsLoading = false;
	let mutating = false;
	let slotError = '';
	let pendingMutation: PendingSlotMutation | null = null;
	let pickerCursors = new Set<string>();
	// One page-bounded mini-frame budget per mounted region owner (gate S4).
	let miniFrameLease: MiniFramePageLease | null = null;
	let announcedFilled = '';

	$: requestedRegions = region === undefined ? [...regions] : [region];
	$: scopeKey = JSON.stringify([$scopeIdentityStore.principal, $scopeIdentityStore.workspace]);
	$: bindingKey = JSON.stringify([page, requestedRegions]);
	$: slotViews = resolvedSlots.map((slot) => ({ ...slot, rendered: rendered.find((item) => item.region === slot.region) }));
	// A region is filled only by a widget that put records on the page. A
	// placeholder — an `unavailable` or `unsupported` one, or the empty state a
	// `ready` widget with no rows renders — is not a replacement for the
	// first-party section it would otherwise retire.
	$: filledRegions = rendered
		.filter((item) => widgetFillsRegion(item.widget))
		.map((item) => item.region)
		.sort();
	// `page` is passed rather than read inside the helper: Svelte cannot see a
	// dependency that only exists in a called function, and this event names the
	// page it is about.
	$: if (mounted) announceFilled(page, filledRegions);
	$: visiblePickerCandidates = orderedPickerCandidates(settings, pickerRegion);
	$: if (mounted && (scopeKey !== appliedScope || bindingKey !== appliedBinding)) {
		resetSlotControls();
		void load(scopeKey, bindingKey, false);
	}

	function announceFilled(forPage: string, regions: string[]): void {
		const serialized = JSON.stringify([forPage, regions]);
		if (serialized === announcedFilled) return;
		announcedFilled = serialized;
		dispatch('filled', { page: forPage, regions });
	}

	function resetSlotControls(): void {
		// A scope or binding change invalidates every frame this page admitted:
		// the budget belongs to what is on screen now, not to what was.
		miniFrameLease?.close();
		miniFrameLease = miniFrameSessionLedger.openPage();
		settingsRequest?.abort();
		mutationRequest?.abort();
		settingsRequest = null;
		mutationRequest = null;
		appliedCredentialRevision = 0;
		pickerRegion = '';
		settings = null;
		settingsLoading = false;
		mutating = false;
		slotError = '';
		pendingMutation = null;
		pickerCursors = new Set();
	}

	function scheduleRefresh(deadline: string | number): void {
		if (refreshTimer) clearTimeout(refreshTimer);
		const deadlineMs = typeof deadline === 'number' ? deadline : Date.parse(deadline);
		const remaining = deadlineMs - Date.now();
		const delay = Number.isFinite(deadlineMs)
			? Math.min(24 * 60 * 60 * 1_000, Math.max(APP_WIDGET_REFRESH_FLOOR_MS, remaining))
			: 30_000;
		refreshTimer = setTimeout(() => {
			refreshTimer = undefined;
			const now = Date.now();
			// Stale-while-revalidate: a projection stays on screen through its own
			// deadline while the reload runs, bounded by the staleness cap.
			rendered = rendered.filter((item) => appWidgetWithinStaleness(item.widget, now));
			if (document.visibilityState === 'visible') void load(scopeKey, bindingKey, true);
		}, delay);
	}

	async function load(expectedScope: string, expectedBinding: string, retainCache: boolean): Promise<void> {
		const expectedCredentialRevision = getCurrentScopeCredentialRevision();
		if (!scopeCredentialIdentityIsCurrent(expectedCredentialRevision)) {
			request?.abort();
			request = null;
			appliedScope = expectedScope;
			appliedBinding = expectedBinding;
			hideCredentialBoundState();
			scheduleRefresh(Date.now() + 1_000);
			return;
		}
		if (refreshTimer) {
			clearTimeout(refreshTimer);
			refreshTimer = undefined;
		}
		request?.abort();
		const controller = new AbortController();
		request = controller;
		appliedScope = expectedScope;
		appliedBinding = expectedBinding;
		if (!retainCache) {
			cache = undefined;
			cacheBinding = '';
			resolvedSlots = [];
			rendered = [];
			renderPending = false;
		}
		try {
			const uniqueRegions = [...new Set(requestedRegions)].slice(0, 12);
			if (uniqueRegions.length !== requestedRegions.length || uniqueRegions.length === 0) throw new Error('The page widget regions are invalid.');
			const resolved = await resolveAppSlots(page, uniqueRegions, controller.signal);
			const assignments = uniqueRegions.map((name, index) => ({ region: name, assignment: resolved[index] }));
			if (controller.signal.aborted || expectedScope !== scopeKey || expectedBinding !== bindingKey) return;
			if (expectedCredentialRevision !== getCurrentScopeCredentialRevision()) {
				hideCredentialBoundState();
				scheduleRefresh(Date.now() + 1_000);
				return;
			}
			appliedCredentialRevision = expectedCredentialRevision;
			renderPending = assignments.some(({ assignment }) => assignment.widget?.current !== undefined);
			resolvedSlots = assignments;
			const nextCacheBinding = JSON.stringify([
				expectedCredentialRevision,
				assignments.map(({ region: name, assignment }) => {
					const current = assignment.widget?.current;
					return [name, current?.package.installation_id, current?.widget_id,
						current?.package.package_revision_ref, current?.package.package_content_digest,
						current?.package.installation_generation];
				})
			]);
			const uniqueTargets = new Map<string, { installation_id: string; widget_id: string }>();
			for (const { assignment } of assignments) {
				const current = assignment.widget?.current;
				if (current) uniqueTargets.set(
					`${current.package.installation_id}\u0000${current.widget_id}`,
					{ installation_id: current.package.installation_id, widget_id: current.widget_id }
				);
			}
			const targets = [...uniqueTargets.values()];
			if (targets.length === 0) {
				cache = undefined;
				cacheBinding = nextCacheBinding;
				rendered = [];
				// Empty and retained-hidden slots still need to discover remote
				// assignment, enablement, and default changes without a page reload.
				scheduleRefresh(Date.now() + 30_000);
				return;
			}
			if (cacheBinding !== nextCacheBinding) rendered = [];
			const reusableCache = retainCache && cacheBinding === nextCacheBinding ? cache : undefined;
			const next = await renderAppWidgetBatch(targets, reusableCache, controller.signal);
			if (controller.signal.aborted || expectedScope !== scopeKey || expectedBinding !== bindingKey) return;
			if (expectedCredentialRevision !== getCurrentScopeCredentialRevision()) {
				hideCredentialBoundState();
				scheduleRefresh(Date.now() + 1_000);
				return;
			}
			cache = next;
			cacheBinding = nextCacheBinding;
			const now = Date.now();
			const byTarget = new Map(next.value.widgets.map((widget) => [`${widget.installation_id}\u0000${widget.widget_id}`, widget]));
			rendered = assignments.flatMap(({ region: name, assignment }) => {
				const current = assignment.widget?.current;
				if (!current) return [];
				const widget = byTarget.get(`${current.package.installation_id}\u0000${current.widget_id}`);
				if (!widget || !appWidgetWithinStaleness(widget, now) ||
					(widget.state !== 'unavailable' && widget.installation_generation !== current.package.installation_generation)) return [];
				return [{
					region: name,
					widget,
					packageRevisionRef: current.package.package_revision_ref,
					credentialRevision: expectedCredentialRevision
				}];
			});
			scheduleRefresh(next.refreshAfter ?? next.value.refresh_after);
		} catch {
			if (!controller.signal.aborted && expectedScope === scopeKey && expectedBinding === bindingKey) {
				if (expectedCredentialRevision !== getCurrentScopeCredentialRevision()) {
					hideCredentialBoundState();
					scheduleRefresh(Date.now() + 1_000);
					return;
				}
				// A transient failure keeps the last good projection on screen within
				// the staleness cap; only the conditional-request cache is dropped so
				// the retry fetches a full body.
				cache = undefined;
				const now = Date.now();
				rendered = rendered.filter((item) => appWidgetWithinStaleness(item.widget, now));
				scheduleRefresh(Date.now() + 30_000);
			}
		} finally {
			if (request === controller) {
				request = null;
				renderPending = false;
			}
		}
	}

	function candidateKey(candidate: AppSlotPickerCandidate): string {
		const packageBinding = candidate.widget.package;
		return `${packageBinding.installation_id}\u0000${candidate.widget.widget_id}\u0000${packageBinding.package_revision_ref}\u0000${packageBinding.package_content_digest}\u0000${packageBinding.installation_generation}`;
	}

	function candidateTargetKey(candidate: AppSlotPickerCandidate): string {
		return `${candidate.widget.package.installation_id}\u0000${candidate.widget.widget_id}`;
	}

	function orderedPickerCandidates(pageSettings: AppSlotSettingsPage | null, targetRegion: string): AppSlotPickerCandidate[] {
		if (!pageSettings || !targetRegion) return [];
		const slotId = resolvedSlots.find((slot) => slot.region === targetRegion)?.assignment.slot_id;
		return [...pageSettings.picker].sort((left, right) => {
			const leftSuggested = left.suggested_slots.some((suggestion) => suggestion.slot_id === slotId) ? 0 : 1;
			const rightSuggested = right.suggested_slots.some((suggestion) => suggestion.slot_id === slotId) ? 0 : 1;
			return leftSuggested - rightSuggested || left.title.localeCompare(right.title);
		});
	}

	function slotAssignmentAuthorityKey(assignment: AppResolvedSlotAssignment | undefined, slotId: string): string {
		const widget = assignment?.widget;
		const pinned = widget?.pinned;
		const current = widget?.current;
		return JSON.stringify([
			slotId,
			assignment?.source ?? null,
			assignment?.pinned_system_default ?? false,
			assignment?.opted_out ?? false,
			assignment?.hidden_reason ?? null,
			pinned?.package.installation_id ?? null,
			pinned?.package.package_id ?? null,
			pinned?.package.package_revision_ref ?? null,
			pinned?.package.package_content_digest ?? null,
			pinned?.package.installation_generation ?? null,
			pinned?.widget_id ?? null,
			current?.package.installation_id ?? null,
			current?.package.package_id ?? null,
			current?.package.package_revision_ref ?? null,
			current?.package.package_content_digest ?? null,
			current?.package.installation_generation ?? null,
			current?.widget_id ?? null,
			widget?.restored_across_generation ?? null,
			widget?.assignment_compatibility ?? null
		]);
	}

	function settingsMatchResolvedSlot(pageSettings: AppSlotSettingsPage, resolved: AppResolvedSlotAssignment): boolean {
		const settingsAssignment = pageSettings.assignments.find((assignment) => assignment.slot_id === resolved.slot_id);
		return slotAssignmentAuthorityKey(settingsAssignment, resolved.slot_id) ===
			slotAssignmentAuthorityKey(resolved, resolved.slot_id);
	}

	function credentialAuthorityIsCurrent(revision = appliedCredentialRevision): boolean {
		return scopeCredentialIdentityIsCurrent(revision);
	}

	function hideCredentialBoundState(): void {
		cache = undefined;
		cacheBinding = '';
		resolvedSlots = [];
		rendered = [];
		resetSlotControls();
	}

	async function fetchSettingsForSlot(
		slotId: string,
		pickerLimit: number,
		expectedCredentialRevision: number,
		signal: AbortSignal
	): Promise<AppSlotSettingsPage> {
		if (!credentialAuthorityIsCurrent(expectedCredentialRevision)) {
			throw new Error('The signed-in Apps scope changed. Reload the widget slots and try again.');
		}
		const first = await fetchAppSlotSettings({ assignmentLimit: 128, pickerLimit }, signal);
		if (!credentialAuthorityIsCurrent(expectedCredentialRevision)) {
			throw new Error('The signed-in Apps scope changed. Reload the widget slots and try again.');
		}
		if (first.assignments.some((assignment) => assignment.slot_id === slotId) || !first.assignments_truncated) return first;
		const cursor = first.next_assignment_cursor;
		if (!cursor) throw new Error('The slot settings pagination response was invalid.');
		const second = await fetchAppSlotSettings({
			assignmentLimit: 128,
			assignmentCursor: cursor,
			pickerLimit
		}, signal);
		if (!credentialAuthorityIsCurrent(expectedCredentialRevision)) {
			throw new Error('The signed-in Apps scope changed. Reload the widget slots and try again.');
		}
		if (second.head.revision !== first.head.revision || second.head.fence <= first.head.fence ||
			second.inventory_revision !== first.inventory_revision ||
			second.assignments_truncated || second.next_assignment_cursor !== undefined) {
			throw new Error('The slot settings changed while it was being read. Reopen the picker.');
		}
		const assignments = [...first.assignments, ...second.assignments];
		if (assignments.length > 256 || new Set(assignments.map((assignment) => assignment.slot_id)).size !== assignments.length) {
			throw new Error('The slot settings pagination response was invalid.');
		}
		return {
			head: second.head,
			inventory_revision: second.inventory_revision,
			assignments,
			assignments_truncated: false,
			picker: second.picker,
			...(second.next_picker_cursor ? { next_picker_cursor: second.next_picker_cursor } : {}),
			picker_truncated: second.picker_truncated
		};
	}

	async function openPicker(name: string): Promise<void> {
		// A settings read advances the write fence. While a POST outcome is
		// ambiguous, preserve its exact fence and mutation identity for replay.
		if (pendingMutation || mutating) return;
		const expectedCredentialRevision = appliedCredentialRevision;
		if (!credentialAuthorityIsCurrent(expectedCredentialRevision)) {
			void load(scopeKey, bindingKey, false);
			return;
		}
		settingsRequest?.abort();
		const controller = new AbortController();
		settingsRequest = controller;
		const expectedScope = scopeKey;
		const expectedBinding = bindingKey;
		pickerRegion = name;
		settings = null;
		settingsLoading = true;
		slotError = '';
		try {
			const slot = resolvedSlots.find((item) => item.region === name);
			if (!slot) throw new Error('The widget slot is no longer available.');
			const next = await fetchSettingsForSlot(slot.assignment.slot_id, 32, expectedCredentialRevision, controller.signal);
			if (controller.signal.aborted || expectedScope !== scopeKey || expectedBinding !== bindingKey || pickerRegion !== name) return;
			if (!credentialAuthorityIsCurrent(expectedCredentialRevision)) {
				hideCredentialBoundState();
				void load(scopeKey, bindingKey, false);
				return;
			}
			if (!settingsMatchResolvedSlot(next, slot.assignment)) {
				throw new Error('The slot changed while the picker was opening. Reopen the picker.');
			}
			settings = next;
			pickerCursors = new Set(next.next_picker_cursor ? [next.next_picker_cursor] : []);
		} catch (cause) {
			if (!controller.signal.aborted && expectedScope === scopeKey && expectedBinding === bindingKey) {
				if (!credentialAuthorityIsCurrent(expectedCredentialRevision)) {
					hideCredentialBoundState();
					void load(scopeKey, bindingKey, false);
					return;
				}
				slotError = cause instanceof Error ? cause.message : 'The widget picker could not be loaded.';
			}
		} finally {
			if (settingsRequest === controller) settingsRequest = null;
			if (!controller.signal.aborted && expectedScope === scopeKey && expectedBinding === bindingKey) settingsLoading = false;
		}
	}

	async function loadMorePicker(): Promise<void> {
		if (!settings?.picker_truncated || !settings.next_picker_cursor || settingsLoading || mutating || pendingMutation) return;
		const expectedCredentialRevision = appliedCredentialRevision;
		if (!credentialAuthorityIsCurrent(expectedCredentialRevision)) {
			void load(scopeKey, bindingKey, false);
			return;
		}
		const pickerCursor = settings.next_picker_cursor;
		settingsRequest?.abort();
		const controller = new AbortController();
		settingsRequest = controller;
		const expectedScope = scopeKey;
		const expectedBinding = bindingKey;
		const expectedRegion = pickerRegion;
		const previous = settings;
		settingsLoading = true;
		slotError = '';
		try {
			const next = await fetchAppSlotSettings({
				assignmentLimit: 1,
				pickerLimit: 32,
				pickerCursor
			}, controller.signal);
			if (controller.signal.aborted || expectedScope !== scopeKey || expectedBinding !== bindingKey || pickerRegion !== expectedRegion) return;
			if (!credentialAuthorityIsCurrent(expectedCredentialRevision)) {
				hideCredentialBoundState();
				void load(scopeKey, bindingKey, false);
				return;
			}
			if (next.head.revision !== previous.head.revision || next.head.fence <= previous.head.fence ||
				next.inventory_revision !== previous.inventory_revision) {
				throw new Error('The widget picker changed while it was being read. Reopen the picker.');
			}
			const combined = [...previous.picker, ...next.picker];
			if (combined.length > MAX_ACCUMULATED_PICKER_ITEMS || new Set(combined.map(candidateTargetKey)).size !== combined.length ||
				(next.picker_truncated && (next.picker.length === 0 || combined.length >= MAX_ACCUMULATED_PICKER_ITEMS ||
					next.next_picker_cursor === pickerCursor || pickerCursors.has(next.next_picker_cursor!)))) {
				throw new Error('The widget picker pagination response was invalid.');
			}
			if (next.next_picker_cursor) pickerCursors.add(next.next_picker_cursor);
			settings = {
				head: next.head,
				inventory_revision: next.inventory_revision,
				assignments: previous.assignments,
				...(previous.next_assignment_cursor ? { next_assignment_cursor: previous.next_assignment_cursor } : {}),
				assignments_truncated: previous.assignments_truncated,
				picker: combined,
				...(next.next_picker_cursor ? { next_picker_cursor: next.next_picker_cursor } : {}),
				picker_truncated: next.picker_truncated
			};
		} catch (cause) {
			if (!controller.signal.aborted && expectedScope === scopeKey && expectedBinding === bindingKey) {
				if (!credentialAuthorityIsCurrent(expectedCredentialRevision)) {
					hideCredentialBoundState();
					void load(scopeKey, bindingKey, false);
					return;
				}
				// The failed GET may still have advanced the server fence. Discard
				// the prior head rather than offering a predictably stale mutation.
				settings = null;
				slotError = cause instanceof Error ? cause.message : 'More widgets could not be loaded. Reopen the picker.';
			}
		} finally {
			if (settingsRequest === controller) settingsRequest = null;
			if (!controller.signal.aborted && expectedScope === scopeKey && expectedBinding === bindingKey) settingsLoading = false;
		}
	}

	function closePicker(): void {
		if (mutating) return;
		settingsRequest?.abort();
		settingsRequest = null;
		pickerRegion = '';
		settings = null;
		settingsLoading = false;
		pickerCursors = new Set();
		if (!pendingMutation) slotError = '';
	}

	async function executeMutation(pending: PendingSlotMutation): Promise<void> {
		if (mutating || pending.scopeKey !== scopeKey || pending.bindingKey !== bindingKey) return;
		if (!credentialAuthorityIsCurrent(pending.credentialRevision)) {
			pendingMutation = null;
			settings = null;
			slotError = 'The signed-in Apps scope changed. Reloading widget slots before another change.';
			void load(scopeKey, bindingKey, false);
			return;
		}
		mutationRequest?.abort();
		const controller = new AbortController();
		mutationRequest = controller;
		mutating = true;
		slotError = '';
		try {
			await mutateAppSlotAssignment(pending.request, controller.signal);
			if (controller.signal.aborted || pending.scopeKey !== scopeKey || pending.bindingKey !== bindingKey) return;
			if (!credentialAuthorityIsCurrent(pending.credentialRevision)) {
				hideCredentialBoundState();
				void load(scopeKey, bindingKey, false);
				return;
			}
			pendingMutation = null;
			pickerRegion = '';
			settings = null;
			await load(scopeKey, bindingKey, false);
		} catch (cause) {
			if (controller.signal.aborted || pending.scopeKey !== scopeKey || pending.bindingKey !== bindingKey) return;
			if (!credentialAuthorityIsCurrent(pending.credentialRevision)) {
				hideCredentialBoundState();
				void load(scopeKey, bindingKey, false);
				return;
			}
			if (cause instanceof AppSlotTransportError && cause.status >= 400 && cause.status < 500 &&
				cause.status !== 408 && cause.status !== 425 && cause.status !== 429) {
				pendingMutation = null;
				settings = null;
				slotError = 'The slot changed elsewhere. Reopen the picker and try again.';
			} else {
				pendingMutation = pending;
				slotError = 'The slot change could not be confirmed. Retry the same change safely.';
			}
		} finally {
			if (mutationRequest === controller) mutationRequest = null;
			if (!controller.signal.aborted && pending.scopeKey === scopeKey && pending.bindingKey === bindingKey) mutating = false;
		}
	}

	function retryPendingMutation(): void {
		if (pendingMutation) void executeMutation(pendingMutation);
	}

	async function assignCandidate(candidate: AppSlotPickerCandidate): Promise<void> {
		if (!settings || !pickerRegion || pendingMutation || mutating) return;
		if (!credentialAuthorityIsCurrent()) {
			settings = null;
			void load(scopeKey, bindingKey, false);
			return;
		}
		const currentCandidate = settings.picker.find((item) => candidateKey(item) === candidateKey(candidate));
		const slot = resolvedSlots.find((item) => item.region === pickerRegion);
		if (!currentCandidate || !slot) {
			slotError = 'That widget is no longer in the current picker snapshot.';
			return;
		}
		if (!settingsMatchResolvedSlot(settings, slot.assignment)) {
			settings = null;
			slotError = 'The slot changed elsewhere. Reopen the picker and try again.';
			void load(scopeKey, bindingKey, false);
			return;
		}
		const pending: PendingSlotMutation = {
			scopeKey,
			bindingKey,
			credentialRevision: appliedCredentialRevision,
			region: pickerRegion,
			request: {
				expected_revision: settings.head.revision,
				write_fence: settings.head.fence,
				mutation_id: newAppSlotMutationId(),
				command: {
					command: 'assign',
					slot_id: slot.assignment.slot_id,
					installation_id: currentCandidate.widget.package.installation_id,
					widget_id: currentCandidate.widget.widget_id,
					expected_candidate: currentCandidate.widget
				}
			}
		};
		pendingMutation = pending;
		await executeMutation(pending);
	}

	async function optOut(name: string): Promise<void> {
		if (pendingMutation || mutating) return;
		const expectedCredentialRevision = appliedCredentialRevision;
		if (!credentialAuthorityIsCurrent(expectedCredentialRevision)) {
			void load(scopeKey, bindingKey, false);
			return;
		}
		const slot = resolvedSlots.find((item) => item.region === name);
		if (!slot || !slotShowsAssignment(slot.assignment)) return;
		settingsRequest?.abort();
		const controller = new AbortController();
		settingsRequest = controller;
		const expectedScope = scopeKey;
		const expectedBinding = bindingKey;
		settingsLoading = true;
		slotError = '';
		try {
			const head = await fetchSettingsForSlot(slot.assignment.slot_id, 1, expectedCredentialRevision, controller.signal);
			if (controller.signal.aborted || expectedScope !== scopeKey || expectedBinding !== bindingKey) return;
			if (!credentialAuthorityIsCurrent(expectedCredentialRevision)) {
				hideCredentialBoundState();
				void load(scopeKey, bindingKey, false);
				return;
			}
			if (!settingsMatchResolvedSlot(head, slot.assignment)) {
				settings = null;
				slotError = 'The slot changed elsewhere. Refresh and try again.';
				void load(scopeKey, bindingKey, false);
				return;
			}
			const pending: PendingSlotMutation = {
				scopeKey: expectedScope,
				bindingKey: expectedBinding,
				credentialRevision: expectedCredentialRevision,
				region: name,
				request: {
					expected_revision: head.head.revision,
					write_fence: head.head.fence,
					mutation_id: newAppSlotMutationId(),
					command: { command: 'opt_out', slot_id: slot.assignment.slot_id }
				}
			};
			pendingMutation = pending;
			await executeMutation(pending);
		} catch (cause) {
			if (!controller.signal.aborted && expectedScope === scopeKey && expectedBinding === bindingKey) {
				if (!credentialAuthorityIsCurrent(expectedCredentialRevision)) {
					hideCredentialBoundState();
					void load(scopeKey, bindingKey, false);
					return;
				}
				slotError = cause instanceof Error ? cause.message : 'The slot change could not be prepared.';
			}
		} finally {
			if (settingsRequest === controller) settingsRequest = null;
			if (!controller.signal.aborted && expectedScope === scopeKey && expectedBinding === bindingKey) settingsLoading = false;
		}
	}

	function refreshOnFocus(): void {
		if (document.visibilityState === 'visible' && request === null) void load(scopeKey, bindingKey, true);
	}

	// Browser listeners are attached and detached inside onMount: Svelte runs
	// onDestroy during server rendering too, where `window` does not exist.
	onMount(() => {
		mounted = true;
		miniFrameLease = miniFrameSessionLedger.openPage();
		appliedScope = scopeKey;
		appliedBinding = bindingKey;
		if (document.visibilityState === 'visible') void load(scopeKey, bindingKey, false);
		window.addEventListener('focus', refreshOnFocus);
		document.addEventListener('visibilitychange', refreshOnFocus);
		return () => {
			window.removeEventListener('focus', refreshOnFocus);
			document.removeEventListener('visibilitychange', refreshOnFocus);
		};
	});

	onDestroy(() => {
		mounted = false;
		miniFrameLease?.close();
		miniFrameLease = null;
		request?.abort();
		settingsRequest?.abort();
		mutationRequest?.abort();
		if (refreshTimer) clearTimeout(refreshTimer);
	});
</script>

{#if slotViews.length > 0}
	<section class="app-slot-region" aria-label={ariaLabel}>
		{#each slotViews as slot (slot.region)}
			<div class="app-slot" data-app-slot-page={page} data-app-slot-region={slot.region}>
				{#if slot.rendered?.widget.state === 'unavailable'}
					<div class="slot-placeholder">This assigned widget is temporarily unavailable.</div>
				{:else if slot.rendered && slot.assignment.widget}
					<AppNativeWidget
						widget={slot.rendered.widget}
						packageRevisionRef={slot.rendered.packageRevisionRef}
						credentialRevision={slot.rendered.credentialRevision}
						{scopeKey}
						{miniFrameLease}
					/>
				{:else if slot.assignment.widget && renderPending}
					<div class="slot-loading" aria-busy="true" aria-label={`Loading the widget for ${slot.region}`}></div>
				{:else if slotShowsAssignment(slot.assignment)}
					<div class="slot-placeholder">This assigned widget is temporarily unavailable.</div>
				{:else}
					<button class="slot-add" type="button" aria-label={`Add a widget to ${slot.region}`} disabled={mutating || Boolean(pendingMutation)} on:click={() => openPicker(slot.region)}>+</button>
				{/if}
				{#if slotShowsAssignment(slot.assignment)}
					<button class="slot-remove" type="button" aria-label={`Remove the widget from ${slot.region}`} disabled={mutating || settingsLoading} on:click={() => optOut(slot.region)}>Remove</button>
				{/if}
			</div>
		{/each}
		{#if pickerRegion}
			<div class="slot-picker" role="dialog" aria-label={`Choose a widget for ${pickerRegion}`}>
				<div class="slot-picker__header"><strong>Choose a widget</strong><button type="button" disabled={mutating} on:click={closePicker}>Close</button></div>
				{#if settingsLoading && !settings}
					<p>Loading widgets…</p>
				{:else if settings}
					{#if visiblePickerCandidates.length > 0}
						<div class="slot-picker__items">
							{#each visiblePickerCandidates as candidate (candidateKey(candidate))}
								<button type="button" disabled={mutating || Boolean(pendingMutation)} on:click={() => assignCandidate(candidate)}><span>{candidate.title}</span><small>{candidate.widget.package.installation_id}</small></button>
							{/each}
						</div>
					{:else}<p>No installed native widgets are available.</p>{/if}
					{#if settings.picker_truncated}
						<button class="slot-picker__more" type="button" disabled={settingsLoading || mutating || Boolean(pendingMutation) || settings.picker.length >= MAX_ACCUMULATED_PICKER_ITEMS} on:click={loadMorePicker}>Load more widgets</button>
					{/if}
				{/if}
			</div>
		{/if}
		{#if slotError}
			<div class="slot-error" role="alert"><span>{slotError}</span>{#if pendingMutation}<button type="button" disabled={mutating} on:click={retryPendingMutation}>Retry change</button>{/if}</div>
		{/if}
	</section>
{/if}

<style>
	.app-slot-region { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(100%, 20rem), 1fr)); gap: .85rem; min-width: 0; }
	.app-slot { position: relative; min-width: 0; }
	.slot-add { width: 100%; min-height: 5rem; border: 1px dashed var(--border-soft); border-radius: var(--radius-lg, 14px); background: color-mix(in srgb, var(--bg-card) 92%, transparent); color: var(--text-muted); font: inherit; font-size: 1.5rem; cursor: pointer; }
	.slot-placeholder { min-height: 5rem; display: grid; place-items: center; border: 1px solid var(--border-soft); border-radius: var(--radius-lg, 14px); color: var(--text-muted); background: var(--bg-card); padding: 1rem; }
	.slot-loading { min-height: 5rem; border: 1px solid var(--border-soft); border-radius: var(--radius-lg, 14px); background: color-mix(in srgb, var(--bg-card) 92%, transparent); }
	.slot-remove { position: absolute; top: .55rem; right: .55rem; border: 1px solid var(--border-soft); border-radius: 999px; background: var(--bg-card); color: var(--text-secondary); padding: .32rem .55rem; cursor: pointer; }
	.slot-picker, .slot-error { grid-column: 1 / -1; border: 1px solid var(--border-soft); border-radius: var(--radius-lg, 14px); background: var(--bg-card); padding: .85rem; }
	.slot-picker__header, .slot-error { display: flex; align-items: center; justify-content: space-between; gap: .75rem; }
	.slot-picker__header button, .slot-error button { border: 1px solid var(--border-soft); border-radius: 999px; background: transparent; color: var(--text-secondary); padding: .35rem .65rem; cursor: pointer; }
	.slot-picker__items { display: grid; grid-template-columns: repeat(auto-fit, minmax(12rem, 1fr)); gap: .5rem; margin-top: .75rem; }
	.slot-picker__items button { display: grid; gap: .15rem; text-align: left; border: 1px solid var(--border-soft); border-radius: var(--radius-md, 10px); background: var(--bg-elevated, var(--bg-card)); color: var(--text-primary); padding: .7rem; cursor: pointer; }
	.slot-picker__items small { color: var(--text-muted); }
	.slot-picker__more { margin-top: .75rem; border: 1px solid var(--border-soft); border-radius: 999px; background: transparent; color: var(--text-secondary); padding: .4rem .7rem; cursor: pointer; }
	button:disabled { opacity: .55; cursor: default; }
</style>
