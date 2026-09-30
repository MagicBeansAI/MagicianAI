<script lang="ts">
	import { browser } from '$app/environment';
	import { goto } from '$app/navigation';
	import { onDestroy, onMount, tick } from 'svelte';
	import { get } from 'svelte/store';

	import Badge from '$lib/magician/components/generative/Badge.svelte';
	import Button from '$lib/magician/components/generative/Button.svelte';
	import CodeBlock from '$lib/magician/components/generative/CodeBlock.svelte';
	import Markdown from '$lib/magician/components/generative/Markdown.svelte';
	import Spinner from '$lib/magician/components/generative/Spinner.svelte';
	import Skeleton from '$lib/shared/components/Skeleton.svelte';
	import MuijRenderer from '$lib/magician/components/generative/MuijRenderer.svelte';
	import MarkdownDashboard from '$lib/magician/dashboard/MarkdownDashboard.svelte';
	import HtmlDashboard from '$lib/magician/dashboard/HtmlDashboard.svelte';
	import AutoTable from '$lib/magician/dashboard/AutoTable.svelte';
	import KeyValueGrid from '$lib/magician/dashboard/KeyValueGrid.svelte';
	import JsonViewer from '$lib/magician/dashboard/JsonViewer.svelte';
	import XmlViewer from '$lib/magician/dashboard/XmlViewer.svelte';
	import TextDashboard from '$lib/magician/dashboard/TextDashboard.svelte';
	import { ensureRegistryLoaded } from '$lib/stores/themeStore';
	import {
		loadPublishedSurfacePage,
		republishPublishedSurface,
		subscribeToPublishedSurfaceRefresh,
		unpublishPublishedSurface
	} from '$lib/magician/presto/surfaces/publishedSurfaces';
	import { v2Events, type ConnectionStatus } from '$lib/realtime/v2-websocket';
	import { requestConfirmation } from '$lib/stores/confirmationStore';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import type { MuijDocument } from '$lib/stores/muijStore';
	import type { PublishedSurfaceRecord } from '$lib/types/surfaces';
	import { timedFetch } from '$lib/shared/fetch';

	type WidgetSize = 'compact' | 'wide' | 'hero';
	type RefreshReason = 'initial' | 'manual' | 'realtime' | 'reconnect' | 'scope' | 'load_more';

	interface CanvasLayoutPrefs {
		order: string[];
		hidden: string[];
		sizeBySurfaceId: Record<string, WidgetSize>;
	}

	const INITIAL_LIMIT = 12;
	const PAGE_STEP = 12;

	export let routeTarget: string = '/briefing';
	export let scopeTaskId: string | undefined = undefined;
	export let scopeAgentId: string | undefined = undefined;
	export let initialSelectedSurfaceId: string | null = null;
	export let heading: string = 'Scroll canvas';
	export let subheading: string = 'Recurring briefings and deliveries rendered as a customizable dashboard.';

	let records: PublishedSurfaceRecord[] = [];
	let requestedSurfaceCount = INITIAL_LIMIT;
	let hasMoreRecords = false;
	let routeError: string | null = null;
	let isLoadingRecords = false;
	// `hasFetchedOnce` drives the first-paint skeleton without
	// blocking the initial fetch. Stays `false` from the very first
	// render through the moment the first fetch resolves; the render
	// branch treats `!hasFetchedOnce` as "loading" so the page paints
	// the full-width skeleton grid instead of the narrower
	// `.scroll-canvas__empty` box. Empty + error branches only run
	// after a fetch has actually completed.
	let hasFetchedOnce = false;
	let isRefreshingRecords = false;
	let selectedSurfaceId: string | null = null;
	let lastUpdatedAt: number | null = null;
	let connectionStatus: ConnectionStatus = 'disconnected';
	let activeScopeKey = '';
	let hasLoadedOnce = false;
	let pendingRefresh = false;

	let layoutPrefs: CanvasLayoutPrefs = {
		order: [],
		hidden: [],
		sizeBySurfaceId: {}
	};
	let layoutPrefsLoaded = false;

	let realtimeUnsubscribe: (() => void) | null = null;
	let connectionUnsubscribe: (() => void) | null = null;
	let recordRequestSerial = 0;
	let appliedInitialSelection: string | null = null;

	const widgetElements = new Map<string, HTMLElement>();

	function scopeSignature(
		principal: string,
		workspace: string,
		taskId: string | undefined,
		agentId: string | undefined
	): string {
		return `${principal}::${workspace}::${taskId ?? ''}::${agentId ?? ''}`;
	}

	function prefsStorageKey(
		principal: string,
		workspace: string,
		taskId: string | undefined,
		agentId: string | undefined
	): string {
		return `magican-scroll-canvas:${principal}:${workspace}:${routeTarget}:${taskId ?? ''}:${agentId ?? ''}`;
	}

	function readLayoutPrefs(
		principal: string,
		workspace: string,
		taskId: string | undefined,
		agentId: string | undefined
	): CanvasLayoutPrefs {
		if (!browser) {
			return { order: [], hidden: [], sizeBySurfaceId: {} };
		}
		try {
			const raw = localStorage.getItem(prefsStorageKey(principal, workspace, taskId, agentId));
			if (!raw) {
				return { order: [], hidden: [], sizeBySurfaceId: {} };
			}
			const parsed = JSON.parse(raw) as unknown;
			if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) {
				return { order: [], hidden: [], sizeBySurfaceId: {} };
			}
			const record = parsed as Record<string, unknown>;
			const order = Array.isArray(record.order)
				? record.order.filter((value): value is string => typeof value === 'string')
				: [];
			const hidden = Array.isArray(record.hidden)
				? record.hidden.filter((value): value is string => typeof value === 'string')
				: [];
			const rawSizes =
				typeof record.sizeBySurfaceId === 'object' &&
				record.sizeBySurfaceId !== null &&
				!Array.isArray(record.sizeBySurfaceId)
					? (record.sizeBySurfaceId as Record<string, unknown>)
					: {};
			const sizeBySurfaceId: Record<string, WidgetSize> = {};
			for (const [surfaceId, value] of Object.entries(rawSizes)) {
				if (value === 'compact' || value === 'wide' || value === 'hero') {
					sizeBySurfaceId[surfaceId] = value;
				}
			}
			return { order, hidden, sizeBySurfaceId };
		} catch {
			return { order: [], hidden: [], sizeBySurfaceId: {} };
		}
	}

	function writeLayoutPrefs(): void {
		if (!browser || !layoutPrefsLoaded) return;
		localStorage.setItem(
			prefsStorageKey(
				$scopeIdentityStore.principal,
				$scopeIdentityStore.workspace,
				scopeTaskId,
				scopeAgentId
			),
			JSON.stringify(layoutPrefs)
		);
	}

	function normalizeLayoutPrefs(
		source: PublishedSurfaceRecord[],
		prefs: CanvasLayoutPrefs
	): CanvasLayoutPrefs {
		const validIds = new Set(source.map((record) => record.manifest.surface_id));
		const order = prefs.order.filter((surfaceId) => validIds.has(surfaceId));
		const hidden = prefs.hidden.filter((surfaceId) => validIds.has(surfaceId));
		const sizeBySurfaceId = Object.fromEntries(
			Object.entries(prefs.sizeBySurfaceId).filter(([surfaceId, size]) =>
				validIds.has(surfaceId) && (size === 'compact' || size === 'wide' || size === 'hero')
			)
		) as Record<string, WidgetSize>;
		return { order, hidden, sizeBySurfaceId };
	}

	function timestampForRecord(record: PublishedSurfaceRecord): number {
		const producedAt = Date.parse(record.metadata.producer.produced_at);
		if (Number.isFinite(producedAt)) return producedAt;
		const publishedAt = Date.parse(record.manifest.published_at);
		return Number.isFinite(publishedAt) ? publishedAt : 0;
	}

	function compareNewestFirst(left: PublishedSurfaceRecord, right: PublishedSurfaceRecord): number {
		return (
			timestampForRecord(right) - timestampForRecord(left)
			|| right.metadata.artifact_uid.localeCompare(left.metadata.artifact_uid)
		);
	}

	function sortVisibleRecords(
		source: PublishedSurfaceRecord[],
		prefs: CanvasLayoutPrefs,
		focusedSurfaceId: string | null
	): PublishedSurfaceRecord[] {
		const hidden = new Set(prefs.hidden);
		const remaining = [...source]
			.filter((record) => !hidden.has(record.manifest.surface_id))
			.sort(compareNewestFirst);
		const prioritizedIds = [
			...(focusedSurfaceId ? [focusedSurfaceId] : []),
			...prefs.order.filter((surfaceId) => surfaceId !== focusedSurfaceId)
		];
		const prioritized = new Set(prioritizedIds);
		const recordById = new Map(remaining.map((record) => [record.manifest.surface_id, record]));
		const ordered: PublishedSurfaceRecord[] = [];
		for (const surfaceId of prioritizedIds) {
			const record = recordById.get(surfaceId);
			if (record) {
				ordered.push(record);
				recordById.delete(surfaceId);
			}
		}
		for (const record of remaining) {
			if (!prioritized.has(record.manifest.surface_id)) {
				ordered.push(record);
			}
		}
		return ordered;
	}

	function humanizeScope(taskId: string | undefined, agentId: string | undefined): string {
		const parts: string[] = [];
		if (taskId) parts.push(`task ${taskId}`);
		if (agentId) parts.push(`agent ${agentId}`);
		return parts.length > 0 ? parts.join(' · ') : 'workspace';
	}

	function clearRecordsForScopeChange(): void {
		recordRequestSerial += 1;
		pendingRefresh = false;
		records = [];
		hasMoreRecords = false;
		routeError = null;
		lastUpdatedAt = null;
		hasLoadedOnce = false;
		isLoadingRecords = false;
		isRefreshingRecords = false;
		widgetElements.clear();
	}

	function formatRelative(timestamp: number | null): string {
		if (!timestamp) return 'just now';
		const diffMs = Date.now() - timestamp;
		const minutes = Math.round(diffMs / 60_000);
		if (Math.abs(minutes) < 1) return 'just now';
		if (Math.abs(minutes) < 60) return `${Math.abs(minutes)}m ago`;
		const hours = Math.round(minutes / 60);
		if (Math.abs(hours) < 48) return `${Math.abs(hours)}h ago`;
		const days = Math.round(hours / 24);
		return `${Math.abs(days)}d ago`;
	}

	function widgetMeta(record: PublishedSurfaceRecord): string {
		const bits: string[] = [];
		if (record.source_agent_id || record.metadata.producer.producer_agent_id) {
			bits.push(record.source_agent_id ?? record.metadata.producer.producer_agent_id);
		}
		if (record.metadata.ownership.task_id) {
			bits.push(record.metadata.ownership.task_id);
		}
		bits.push(formatRelative(Date.parse(record.manifest.published_at)));
		return bits.join(' · ');
	}

	function defaultWidgetSize(record: PublishedSurfaceRecord): WidgetSize {
		const spatial = record.manifest.spatial;
		if (!spatial) return 'wide';
		if (spatial.width >= 760 || spatial.height >= 560) return 'hero';
		if (spatial.width >= 420 || spatial.height >= 320) return 'wide';
		return 'compact';
	}

	function widgetSize(record: PublishedSurfaceRecord): WidgetSize {
		return layoutPrefs.sizeBySurfaceId[record.manifest.surface_id] ?? defaultWidgetSize(record);
	}

	function renderMuijDocument(record: PublishedSurfaceRecord): MuijDocument | null {
		return record.render?.muij_document ?? null;
	}

	function renderText(record: PublishedSurfaceRecord): string {
		if (typeof record.render?.text_content === 'string' && record.render.text_content.trim().length > 0) {
			return record.render.text_content;
		}
		if (typeof record.render?.source_output_summary === 'string') {
			return record.render.source_output_summary;
		}
		if (typeof record.manifest.summary === 'string') {
			return record.manifest.summary;
		}
		return '';
	}

	function renderJson(record: PublishedSurfaceRecord): string {
		if (record.render?.json_content !== undefined) {
			return JSON.stringify(record.render.json_content, null, 2);
		}
		return renderText(record);
	}

	function jsonContent(record: PublishedSurfaceRecord): unknown {
		if (record.render?.json_content !== undefined) {
			return record.render.json_content;
		}
		const text = renderText(record);
		if (!text) return null;
		try {
			return JSON.parse(text);
		} catch {
			return null;
		}
	}

	function isMuijShape(value: unknown): boolean {
		if (!value) return false;
		if (Array.isArray(value)) {
			return (
				value.length > 0 &&
				value.every(
					(v) => typeof v === 'object' && v !== null && ('type' in (v as object) || 'component_type' in (v as object))
				)
			);
		}
		if (typeof value === 'object' && value !== null) {
			return 'type' in value || 'components' in value || 'layout' in value;
		}
		return false;
	}

	function isArrayOfObjects(value: unknown): value is Array<Record<string, unknown>> {
		return (
			Array.isArray(value) &&
			value.length > 0 &&
			value.every((v) => typeof v === 'object' && v !== null && !Array.isArray(v))
		);
	}

	function isPlainObject(value: unknown): value is Record<string, unknown> {
		return typeof value === 'object' && value !== null && !Array.isArray(value);
	}

	function muijComponentsFrom(value: unknown): import('$lib/stores/muijStore').MuijComponent[] {
		const arr: unknown[] = Array.isArray(value)
			? value
			: isPlainObject(value)
				? (((value as { components?: unknown }).components as unknown[]) ??
					((value as { layout?: unknown }).layout as unknown[]) ??
					[])
				: [];
		return arr as import('$lib/stores/muijStore').MuijComponent[];
	}

	function cycleWidgetSize(surfaceId: string): void {
		const current = layoutPrefs.sizeBySurfaceId[surfaceId] ?? 'wide';
		const next: WidgetSize =
			current === 'compact' ? 'wide' : current === 'wide' ? 'hero' : 'compact';
		layoutPrefs = {
			...layoutPrefs,
			sizeBySurfaceId: {
				...layoutPrefs.sizeBySurfaceId,
				[surfaceId]: next
			}
		};
	}

	function moveSurface(surfaceId: string, direction: -1 | 1): void {
		const currentVisibleIds = visibleRecords.map((record) => record.manifest.surface_id);
		const currentIndex = currentVisibleIds.indexOf(surfaceId);
		if (currentIndex === -1) return;
		const targetIndex = currentIndex + direction;
		if (targetIndex < 0 || targetIndex >= currentVisibleIds.length) return;
		const reordered = [...currentVisibleIds];
		const [moved] = reordered.splice(currentIndex, 1);
		reordered.splice(targetIndex, 0, moved);
		layoutPrefs = {
			...layoutPrefs,
			order: reordered
		};
	}

	function hideSurface(surfaceId: string): void {
		if (layoutPrefs.hidden.includes(surfaceId)) return;
		layoutPrefs = {
			...layoutPrefs,
			hidden: [...layoutPrefs.hidden, surfaceId],
			order: layoutPrefs.order.filter((entry) => entry !== surfaceId)
		};
		if (selectedSurfaceId === surfaceId) {
			const nextVisible = visibleRecords.find((record) => record.manifest.surface_id !== surfaceId);
			selectedSurfaceId = nextVisible?.manifest.surface_id ?? null;
		}
	}

	/**
	 * Permanently remove a surface (vs `hideSurface` which is a UI-only
	 * preference). Calls the V3 unpublish endpoint, drops the record from
	 * local state on success, and clears any layout prefs that referenced
	 * it. The surface is marked unpublished server-side, recoverable via
	 * the V3 republish API by an admin.
	 */
	async function removeSurface(surfaceId: string, title: string): Promise<void> {
		if (!browser) return;
		const confirmed = await requestConfirmation({
			title: `Remove "${title}" from the dashboard?`,
			message: 'This unpublishes the surface — it will disappear from the canvas and the feed. An admin can republish it via the V3 API.',
			confirmLabel: 'Remove',
			destructive: true
		});
		if (!confirmed) return;
		try {
			await unpublishPublishedSurface(surfaceId);
		} catch (error) {
			console.error('[PublishedScrollCanvas] unpublish failed', error);
			window.alert(`Failed to remove surface: ${error instanceof Error ? error.message : String(error)}`);
			return;
		}
		records = records.filter((record) => record.manifest.surface_id !== surfaceId);
		layoutPrefs = {
			...layoutPrefs,
			hidden: layoutPrefs.hidden.filter((entry) => entry !== surfaceId),
			order: layoutPrefs.order.filter((entry) => entry !== surfaceId),
			sizeBySurfaceId: Object.fromEntries(
				Object.entries(layoutPrefs.sizeBySurfaceId).filter(([id]) => id !== surfaceId)
			)
		};
		if (selectedSurfaceId === surfaceId) {
			const nextVisible = visibleRecords.find((record) => record.manifest.surface_id !== surfaceId);
			selectedSurfaceId = nextVisible?.manifest.surface_id ?? null;
		}
	}

	/**
	 * Republish a single surface so the backend re-runs materialization
	 * against the current source bytes. Used to refresh existing surfaces
	 * after a backend rendering change (e.g., the v0.6.441 inline-deliverable
	 * fix) without the user having to delete-and-recreate.
	 */
	async function refreshSurface(surfaceId: string): Promise<void> {
		if (!browser) return;
		try {
			await republishPublishedSurface(surfaceId);
			// The backend broadcasts `published_surface.changed`, which the
			// canvas's existing realtime subscription folds back through
			// `refreshRecords('realtime')` — no manual reload needed here.
		} catch (error) {
			console.error('[PublishedScrollCanvas] refresh failed', error);
			window.alert(`Failed to refresh surface: ${error instanceof Error ? error.message : String(error)}`);
		}
	}

	/**
	 * Re-run the task user-output synthesis (v1.1.0 prompt) for the
	 * surface's parent task. The synthesizer is free to pick a richer
	 * format than the original (e.g., upgrade `text/markdown` to
	 * `application/json` MUI-JSON with live dataSource bindings) now
	 * that the allowed_media_types policy includes application/json
	 * for task user-outputs. Backend endpoint is wired but the
	 * underlying context reconstruction is still a follow-up; the
	 * button surfaces the resulting error inline for now.
	 */
	async function resynthesizeSurface(record: PublishedSurfaceRecord): Promise<void> {
		if (!browser) return;
		const taskId = record.metadata.ownership.task_id;
		if (!taskId) {
			window.alert('This surface has no parent task — cannot re-synthesize.');
			return;
		}
		const scope = get(scopeIdentityStore);
		const params = new URLSearchParams();
		const url = `/api/magician/v3/tasks/${encodeURIComponent(taskId)}/resynthesize_user_output${params.toString() ? `?${params.toString()}` : ''}`;
		try {
			const response = await timedFetch(url, {
				method: 'POST',
				headers: {
					'Content-Type': 'application/json'
				},
				body: '{}'
			});
			if (!response.ok) {
				const body = await response.text();
				window.alert(`Re-synthesize failed (${response.status}): ${body}`);
				return;
			}
			// Trigger a republish so the new user-output materializes into
			// the surface and the realtime broadcast fires.
			await republishPublishedSurface(record.manifest.surface_id);
		} catch (error) {
			console.error('[PublishedScrollCanvas] resynthesize failed', error);
			window.alert(
				`Re-synthesize failed: ${error instanceof Error ? error.message : String(error)}`
			);
		}
	}

	/**
	 * Refresh every visible surface on the canvas. Useful after a backend
	 * change to bulk re-materialize without manually clicking each card.
	 */
	async function refreshAllSurfaces(): Promise<void> {
		if (!browser) return;
		const total = visibleRecords.length;
		if (total === 0) return;
		const confirmed = await requestConfirmation({
			title: `Refresh all ${total} ${total === 1 ? 'surface' : 'surfaces'}?`,
			message: 'Each is republished server-side so the rendered content matches the latest source.',
			confirmLabel: 'Refresh all'
		});
		if (!confirmed) return;
		const failed: string[] = [];
		for (const record of visibleRecords.slice()) {
			try {
				await republishPublishedSurface(record.manifest.surface_id);
			} catch (error) {
				failed.push(record.manifest.title || record.manifest.surface_id);
				console.error('[PublishedScrollCanvas] refresh failed during refresh-all', error);
			}
		}
		if (failed.length > 0) {
			window.alert(`Failed to refresh ${failed.length} ${failed.length === 1 ? 'surface' : 'surfaces'}:\n\n${failed.join('\n')}`);
		}
	}

	/**
	 * Clear all currently visible surfaces from this canvas. Iterates the
	 * visible records and calls the V3 unpublish endpoint on each. Network
	 * errors are surfaced to the user but the loop continues — surfaces
	 * that succeed are removed locally even if later ones fail.
	 */
	async function clearAllSurfaces(): Promise<void> {
		if (!browser) return;
		const total = visibleRecords.length;
		if (total === 0) return;
		const confirmed = await requestConfirmation({
			title: `Remove all ${total} ${total === 1 ? 'surface' : 'surfaces'} from this canvas?`,
			message: 'Each is unpublished server-side. An admin can republish via the V3 API.',
			confirmLabel: 'Remove all',
			destructive: true
		});
		if (!confirmed) return;
		const failed: string[] = [];
		const removed = new Set<string>();
		for (const record of visibleRecords.slice()) {
			try {
				await unpublishPublishedSurface(record.manifest.surface_id);
				removed.add(record.manifest.surface_id);
			} catch (error) {
				failed.push(record.manifest.title || record.manifest.surface_id);
				console.error('[PublishedScrollCanvas] unpublish failed during clear-all', error);
			}
		}
		records = records.filter((record) => !removed.has(record.manifest.surface_id));
		layoutPrefs = {
			...layoutPrefs,
			hidden: layoutPrefs.hidden.filter((entry) => !removed.has(entry)),
			order: layoutPrefs.order.filter((entry) => !removed.has(entry)),
			sizeBySurfaceId: Object.fromEntries(
				Object.entries(layoutPrefs.sizeBySurfaceId).filter(([id]) => !removed.has(id))
			)
		};
		if (selectedSurfaceId && removed.has(selectedSurfaceId)) {
			const nextVisible = visibleRecords.find((record) => !removed.has(record.manifest.surface_id));
			selectedSurfaceId = nextVisible?.manifest.surface_id ?? null;
		}
		if (failed.length > 0) {
			window.alert(`Failed to remove ${failed.length} ${failed.length === 1 ? 'surface' : 'surfaces'}:\n\n${failed.join('\n')}`);
		}
	}

	function restoreHidden(): void {
		layoutPrefs = {
			...layoutPrefs,
			hidden: []
		};
	}

	function resetLayout(): void {
		layoutPrefs = {
			order: [],
			hidden: [],
			sizeBySurfaceId: {}
		};
	}

	function bindWidgetElement(surfaceId: string) {
		return {
			update(nextSurfaceId: string): void {
				if (nextSurfaceId !== surfaceId) {
					widgetElements.delete(surfaceId);
					surfaceId = nextSurfaceId;
				}
			},
			destroy(): void {
				widgetElements.delete(surfaceId);
			}
		};
	}

	function captureWidget(node: HTMLElement, surfaceId: string) {
		widgetElements.set(surfaceId, node);
		return bindWidgetElement(surfaceId);
	}

	async function scrollSelectionIntoView(surfaceId: string): Promise<void> {
		await tick();
		widgetElements.get(surfaceId)?.scrollIntoView({
			behavior: 'smooth',
			block: 'center',
			inline: 'nearest'
		});
	}

	function focusSurface(surfaceId: string): void {
		if (layoutPrefs.hidden.includes(surfaceId)) {
			layoutPrefs = {
				...layoutPrefs,
				hidden: layoutPrefs.hidden.filter((entry) => entry !== surfaceId)
			};
		}
		selectedSurfaceId = surfaceId;
		void scrollSelectionIntoView(surfaceId);
	}

	async function openTask(taskId: string): Promise<void> {
		await goto(`/tasks?selected=${encodeURIComponent(taskId)}`, {
			replaceState: false,
			noScroll: true
		});
	}

	async function openAgent(agentId: string): Promise<void> {
		await goto(`/crew/${encodeURIComponent(agentId)}`, {
			replaceState: false,
			noScroll: true
		});
	}

	async function openSurface(surfaceId: string): Promise<void> {
		const base = routeTarget.startsWith('/') ? routeTarget : '/briefing';
		await goto(`${base.replace(/\/$/, '')}/${encodeURIComponent(surfaceId)}`, {
			replaceState: false,
			noScroll: false
		});
	}

	async function refreshRecords(reason: RefreshReason): Promise<void> {
		if (isLoadingRecords || isRefreshingRecords) {
			pendingRefresh = true;
			return;
		}

		const requestScopeKey = scopeKey;
		const requestScope = {
			principal: $scopeIdentityStore.principal,
			workspace: $scopeIdentityStore.workspace
		};
		const requestTaskId = scopeTaskId;
		const requestAgentId = scopeAgentId;
		const requestLimit = requestedSurfaceCount;
		const useLoadingState = records.length === 0 && (reason === 'initial' || reason === 'scope');
		if (useLoadingState) {
			isLoadingRecords = true;
		} else {
			isRefreshingRecords = true;
		}

		const requestSerial = ++recordRequestSerial;
		try {
			const page = await loadPublishedSurfacePage({
				route_target: routeTarget,
				task_id: requestTaskId,
				agent_id: requestAgentId,
				maxItems: requestLimit,
				scope: requestScope
			});
			if (requestSerial !== recordRequestSerial || requestScopeKey !== scopeKey) return;

			const nextRecords = [...page.records].sort(compareNewestFirst);
			records = nextRecords;
			hasMoreRecords = page.hasMore;
			routeError = null;
			lastUpdatedAt = Date.now();
			hasLoadedOnce = true;
			layoutPrefs = normalizeLayoutPrefs(nextRecords, layoutPrefs);

			if (selectedSurfaceId && !nextRecords.some((record) => record.manifest.surface_id === selectedSurfaceId)) {
				selectedSurfaceId = nextRecords[0]?.manifest.surface_id ?? null;
			}
			if (!selectedSurfaceId && nextRecords.length > 0) {
				selectedSurfaceId = nextRecords[0].manifest.surface_id;
			}

		} catch (error) {
			if (requestSerial !== recordRequestSerial || requestScopeKey !== scopeKey) return;
			routeError =
				error instanceof Error ? error.message : 'Failed to load published scrolls';
			hasMoreRecords = false;
		} finally {
			if (requestSerial === recordRequestSerial) {
				isLoadingRecords = false;
				isRefreshingRecords = false;
				hasFetchedOnce = true;
			}
			if (requestSerial === recordRequestSerial && pendingRefresh) {
				pendingRefresh = false;
				void refreshRecords('realtime');
			}
		}
	}

	function resetRealtimeSubscription(): void {
		realtimeUnsubscribe?.();
		realtimeUnsubscribe = subscribeToPublishedSurfaceRefresh(
			{
				route_target: routeTarget,
				task_id: scopeTaskId,
				agent_id: scopeAgentId
			},
			() => {
				void refreshRecords('realtime');
			}
		);
	}

	function loadScopePrefs(): void {
		layoutPrefs = readLayoutPrefs(
			$scopeIdentityStore.principal,
			$scopeIdentityStore.workspace,
			scopeTaskId,
			scopeAgentId
		);
		layoutPrefsLoaded = true;
	}

	$: scopeKey = scopeSignature(
		$scopeIdentityStore.principal,
		$scopeIdentityStore.workspace,
		scopeTaskId,
		scopeAgentId
	);
	$: visibleRecords = sortVisibleRecords(records, layoutPrefs, selectedSurfaceId);
	$: hiddenCount = layoutPrefs.hidden.length;
		$: if (browser && layoutPrefsLoaded && activeScopeKey === scopeKey) {
			writeLayoutPrefs();
		}
	$: if (browser && initialSelectedSurfaceId && appliedInitialSelection !== initialSelectedSurfaceId) {
		appliedInitialSelection = initialSelectedSurfaceId;
		focusSurface(initialSelectedSurfaceId);
	}
		$: {
			if (browser && activeScopeKey && scopeKey !== activeScopeKey) {
				activeScopeKey = scopeKey;
				requestedSurfaceCount = INITIAL_LIMIT;
				selectedSurfaceId = null;
				appliedInitialSelection = null;
				clearRecordsForScopeChange();
				loadScopePrefs();
				resetRealtimeSubscription();
				void refreshRecords('scope');
			}
	}

	// No dashboard-theme override in this shared canvas. Previously this
	// component did `selectedThemeId.set('editorial')` +
	// `applyThemeToCssVariables(...)` in onMount, which writes inline
	// styles on `document.documentElement` for every `--theme-color-*`
	// variable. Those inline styles WIN over the app-level
	// `[data-theme="..."]` selectors in app.css, so the user's chosen
	// app theme was being steamrolled by the editorial dashboard theme
	// on every surface that mounted this canvas. We just keep the
	// registry warmed up so any tile that looks up theme metadata has
	// it available — but the visible tokens cascade from the app
	// theme, not from forced inline overrides. Per-surface dashboard
	// theme opt-in can be added later by reading a `dashboard_theme`
	// field from surface metadata at the tile level.
	onMount(async () => {
		void ensureRegistryLoaded();
	});

	// Trick to keep the original signature compatible — actual mount body below.
	onMount(() => {
		if (!browser) return;

		activeScopeKey = scopeKey;
		loadScopePrefs();
		resetRealtimeSubscription();

		let previousConnectionStatus: ConnectionStatus | null = null;
		connectionUnsubscribe = v2Events.connectionStatus.subscribe((next) => {
			connectionStatus = next;
			if (hasLoadedOnce && previousConnectionStatus === 'disconnected' && next === 'connected') {
				void refreshRecords('reconnect');
			}
			previousConnectionStatus = next;
		});

		void refreshRecords('initial');

		return () => {
			realtimeUnsubscribe?.();
			realtimeUnsubscribe = null;
			connectionUnsubscribe?.();
			connectionUnsubscribe = null;
		};
	});

	onDestroy(() => {
		realtimeUnsubscribe?.();
		realtimeUnsubscribe = null;
		connectionUnsubscribe?.();
		connectionUnsubscribe = null;
	});
</script>

<div class="scroll-canvas">
	<header class="scroll-canvas__header">
		<div class="scroll-canvas__copy">
			<h3>{heading}</h3>
			<p>{subheading}</p>
		</div>

		<div class="scroll-canvas__actions">
			<Badge text={`${visibleRecords.length} visible`} color="default" />
			{#if hiddenCount > 0}
				<Badge text={`${hiddenCount} hidden`} color="warning" />
			{/if}
			<Badge text={connectionStatus === 'connected' ? 'live' : connectionStatus} color={connectionStatus === 'connected' ? 'success' : 'default'} />
			<Badge text={humanizeScope(scopeTaskId, scopeAgentId)} color="info" />
			<Button
				label={isRefreshingRecords ? 'Refreshing…' : 'Refresh'}
				variant="outline"
				size="sm"
				on:click={() => {
					void refreshRecords('manual');
				}}
			/>
			{#if hiddenCount > 0}
				<Button label="Show hidden" variant="outline" size="sm" on:click={restoreHidden} />
			{/if}
			<Button label="Reset layout" variant="outline" size="sm" on:click={resetLayout} />
			{#if visibleRecords.length > 0}
				<Button
					label="Refresh all"
					ariaLabel="Re-render every surface on this canvas"
					title="Republish every visible surface so it picks up the latest source / backend rendering"
					variant="outline"
					size="sm"
					on:click={() => { void refreshAllSurfaces(); }}
				/>
				<Button
					label="Clear all"
					ariaLabel="Unpublish every surface on this canvas"
					title="Permanently unpublish every visible surface"
					variant="outline"
					size="sm"
					on:click={() => { void clearAllSurfaces(); }}
				/>
			{/if}
			{#if hasMoreRecords}
				<Button
					label="Load more"
					variant="secondary"
					size="sm"
					on:click={() => {
						requestedSurfaceCount += PAGE_STEP;
						void refreshRecords('load_more');
					}}
				/>
			{/if}
		</div>
	</header>

	<div class="scroll-canvas__status">
		<span>Updated {formatRelative(lastUpdatedAt)}</span>
		<span>{records.length} scrolls loaded</span>
	</div>

	{#if routeError}
		<div class="scroll-canvas__error">
			<p>{routeError}</p>
		</div>
	{:else if !hasFetchedOnce || (isLoadingRecords && records.length === 0)}
		<!-- Skeleton grid mirrors the eventual .scroll-canvas__grid
		     layout (12 cols, widgets span 6) so the page reserves its
		     full final width during load and doesn't visibly expand
		     when records arrive. The `!hasFetchedOnce` guard makes the
		     skeleton paint on the very first render — before the
		     initial fetch even starts — so SSR doesn't flash the
		     narrow empty-state box. */ -->
		<div class="scroll-canvas__grid scroll-canvas__skeleton">
			<div class="scroll-widget-skeleton">
				<Skeleton variant="card" />
			</div>
			<div class="scroll-widget-skeleton">
				<Skeleton variant="card" />
			</div>
		</div>
	{:else if visibleRecords.length === 0}
		<div class="scroll-canvas__empty">
			<p>No published scrolls are available for this scope.</p>
			{#if hiddenCount > 0}
				<Button label="Restore hidden scrolls" variant="secondary" size="sm" on:click={restoreHidden} />
			{/if}
		</div>
	{:else}
		<div class="scroll-canvas__grid">
			{#each visibleRecords as record (record.manifest.surface_id)}
				<article
					class:selected={selectedSurfaceId === record.manifest.surface_id}
					class:compact={widgetSize(record) === 'compact'}
					class:wide={widgetSize(record) === 'wide'}
					class:hero={widgetSize(record) === 'hero'}
					class="scroll-widget"
					use:captureWidget={record.manifest.surface_id}
				>
					<header class="scroll-widget__header">
						<div class="scroll-widget__heading">
							<div class="scroll-widget__eyebrow">
								<span>Scroll</span>
								<span>{widgetMeta(record)}</span>
							</div>
							<h4>{record.manifest.title}</h4>
							{#if record.manifest.summary}
								<p>{record.manifest.summary}</p>
							{/if}
							<div class="scroll-widget__badges">
								{#each record.manifest.tags.slice(0, 4) as tag}
									<Badge text={`#${tag}`} color="default" />
								{/each}
								{#if record.metadata.ownership.task_id}
									<Badge text={record.metadata.ownership.task_id} color="info" />
								{/if}
								{#if record.manifest.spatial?.layer}
									<Badge text={record.manifest.spatial.layer} color="default" />
								{/if}
							</div>
						</div>

						<div class="scroll-widget__toolbar">
							<Button label="Open" ariaLabel="Open individual briefing" title="Open this briefing as a full page" variant="outline" size="sm" on:click={() => void openSurface(record.manifest.surface_id)} />
							<Button label="Focus" variant={selectedSurfaceId === record.manifest.surface_id ? 'primary' : 'outline'} size="sm" on:click={() => focusSurface(record.manifest.surface_id)} />
							<Button label="Size" variant="outline" size="sm" on:click={() => cycleWidgetSize(record.manifest.surface_id)} />
							<Button label="←" ariaLabel="Move earlier" title="Move earlier" variant="outline" size="sm" on:click={() => moveSurface(record.manifest.surface_id, -1)} />
							<Button label="→" ariaLabel="Move later" title="Move later" variant="outline" size="sm" on:click={() => moveSurface(record.manifest.surface_id, 1)} />
							<Button label="Refresh" ariaLabel="Refresh surface render" title="Re-run server-side rendering to pick up latest content / backend changes" variant="outline" size="sm" on:click={() => void refreshSurface(record.manifest.surface_id)} />
							<Button label="Re-synthesize" ariaLabel="Re-synthesize user output with current prompt" title="Re-run the synthesis prompt (v1.1.0) on the parent task — may upgrade markdown to a richer MUI-JSON dashboard with live data" variant="outline" size="sm" on:click={() => void resynthesizeSurface(record)} />
							<Button label="Hide" variant="outline" size="sm" on:click={() => hideSurface(record.manifest.surface_id)} />
							<Button label="Remove" ariaLabel="Remove surface (unpublish)" title="Permanently unpublish this surface" variant="outline" size="sm" on:click={() => removeSurface(record.manifest.surface_id, record.manifest.title || record.manifest.surface_id)} />
						</div>
					</header>

					<div class="scroll-widget__body">
						{#if record.render?.render_kind === 'muij_surface' && renderMuijDocument(record)}
							{@const muijDocument = renderMuijDocument(record)!}
							<MuijRenderer
								components={muijDocument.layout}
								agentId={muijDocument.agent_id}
								idNamespace={`scroll-canvas:${record.manifest.surface_id}`}
							/>
						{:else if record.render?.render_kind === 'muij_surface'}
							<p class="scroll-widget__error">
								{record.render?.unavailable_reason ?? 'Published surface layout is unavailable'}
							</p>
						{:else if record.render?.render_kind === 'markdown'}
							<div class="scroll-widget__direct scroll-widget__direct--markdown">
								<MarkdownDashboard content={renderText(record)} />
							</div>
						{:else if record.render?.render_kind === 'html'}
							<div class="scroll-widget__direct">
								<HtmlDashboard html={renderText(record)} />
							</div>
						{:else if record.render?.render_kind === 'json'}
							{@const jsonValue = jsonContent(record)}
							<div class="scroll-widget__direct">
								{#if isMuijShape(jsonValue)}
									<MuijRenderer
										components={muijComponentsFrom(jsonValue)}
										idNamespace={`scroll-canvas:${record.manifest.surface_id}`}
									/>
								{:else if isArrayOfObjects(jsonValue)}
									<AutoTable rows={jsonValue} />
								{:else if isPlainObject(jsonValue)}
									<KeyValueGrid object={jsonValue} />
								{:else}
									<JsonViewer node={jsonValue} />
								{/if}
							</div>
						{:else if record.render?.render_kind === 'xml'}
							<div class="scroll-widget__direct">
								<XmlViewer xml={renderText(record)} />
							</div>
						{:else if record.render?.render_kind === 'plain_text'}
							<div class="scroll-widget__direct">
								<TextDashboard text={renderText(record)} />
							</div>
						{:else if record.render?.render_kind === 'unsupported_output' || record.render?.render_kind === 'unavailable'}
							<div class="scroll-widget__error-block">
								<p class="scroll-widget__error">
									{record.render?.unavailable_reason || 'This published surface is not renderable in the dashboard yet.'}
								</p>
								{#if record.render?.source_output_summary}
									<p class="scroll-widget__summary-fallback">{record.render.source_output_summary}</p>
								{/if}
							</div>
						{:else}
							<div class="scroll-widget__loading">
								<Spinner size="sm" label="Preparing surface" />
							</div>
						{/if}
					</div>

					<footer class="scroll-widget__footer">
						{#if record.metadata.ownership.task_id}
							<Button
								label="Open task"
								variant="secondary"
								size="sm"
								on:click={() => void openTask(record.metadata.ownership.task_id!)}
							/>
						{/if}
						{#if record.source_agent_id || record.metadata.producer.producer_agent_id}
							<Button
								label="Open agent"
								variant="outline"
								size="sm"
								on:click={() => void openAgent(record.source_agent_id ?? record.metadata.producer.producer_agent_id)}
							/>
						{/if}
					</footer>
				</article>
			{/each}
		</div>
	{/if}
</div>

<style>
	.scroll-canvas {
		display: flex;
		flex-direction: column;
		gap: 1rem;
		min-height: 0;
		/* Always span the full parent column so neither the loading
		   skeleton nor the records grid can collapse the canvas
		   horizontally — was reading as "page narrows during load". */
		width: 100%;
	}

	.scroll-canvas__header {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 1rem;
		flex-wrap: wrap;
	}

	.scroll-canvas__copy h3 {
		margin: 0;
		font-size: 1.2rem;
		letter-spacing: -0.02em;
	}

	.scroll-canvas__copy p {
		margin: 0.35rem 0 0;
		color: var(--text-secondary);
		max-width: 58rem;
	}

	.scroll-canvas__actions,
	.scroll-canvas__status,
	.scroll-widget__badges,
	.scroll-widget__toolbar,
	.scroll-widget__footer,
	.scroll-widget__eyebrow {
		display: flex;
		align-items: center;
		gap: 0.55rem;
		flex-wrap: wrap;
	}

	.scroll-canvas__status {
		color: var(--text-tertiary, var(--text-secondary));
		font-size: 0.82rem;
	}

	.scroll-canvas__empty,
	.scroll-canvas__error {
		padding: 2rem;
		border-radius: 1rem;
		border: 1px solid var(--border-soft);
		background: color-mix(in srgb, var(--bg-elevated, var(--bg-surface)) 86%, white 14%);
		/* Always span the full canvas width so the page doesn't shrink
		   horizontally when transitioning between loading / empty /
		   error / records states. */
		width: 100%;
		box-sizing: border-box;
	}

	.scroll-canvas__grid {
		display: grid;
		grid-template-columns: repeat(12, minmax(0, 1fr));
		gap: 1rem;
		align-items: start;
	}

	/* Loading-state skeleton tiles match each widget's grid-column
	   span so the page reserves its final width while records load. */
	.scroll-widget-skeleton {
		grid-column: span 6;
		min-height: 14rem;
	}
	.scroll-widget-skeleton :global(.skeleton-card) {
		height: 100%;
	}

	.scroll-widget {
		grid-column: span 6;
		display: flex;
		flex-direction: column;
		gap: 0.9rem;
		padding: 1rem;
		border-radius: 1.15rem;
		border: 1px solid var(--border-soft);
		background:
			linear-gradient(180deg, color-mix(in srgb, var(--bg-elevated, var(--bg-surface)) 94%, white 6%), var(--bg-surface));
		box-shadow: var(--shadow-md);
		min-height: 20rem;
	}

	.scroll-widget.compact {
		grid-column: span 4;
	}

	.scroll-widget.wide {
		grid-column: span 6;
	}

	.scroll-widget.hero {
		grid-column: 1 / -1;
	}

	.scroll-widget.selected {
		border-color: color-mix(in srgb, var(--accent-primary) 42%, var(--border-soft));
		box-shadow: 0 20px 48px color-mix(in srgb, var(--accent-primary) 18%, transparent);
	}

	.scroll-widget__header {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 1rem;
	}

	.scroll-widget__heading {
		display: flex;
		flex-direction: column;
		gap: 0.55rem;
		min-width: 0;
	}

	.scroll-widget__eyebrow {
		font-size: 0.78rem;
		color: var(--text-tertiary, var(--text-secondary));
		text-transform: uppercase;
		letter-spacing: 0.06em;
	}

	.scroll-widget__heading h4 {
		margin: 0;
		font-size: 1.05rem;
		letter-spacing: -0.02em;
	}

	.scroll-widget__heading p {
		margin: 0;
		color: var(--text-secondary);
	}

	.scroll-widget__body {
		min-height: 14rem;
	}

	.scroll-widget__body :global(.muij-renderer-root) {
		display: flex;
		flex-direction: column;
		gap: 0.85rem;
	}

	.scroll-widget__direct,
	.scroll-widget__error-block {
		min-height: 14rem;
		padding: 0.35rem;
	}

	.scroll-widget__direct--markdown {
		padding: 0.4rem 0.25rem;
	}

	.scroll-widget__loading {
		min-height: 14rem;
		display: flex;
		align-items: center;
		justify-content: center;
	}

	.scroll-widget__error {
		color: var(--color-error);
		margin: 0;
	}

	.scroll-widget__summary-fallback {
		margin: 0.65rem 0 0;
		color: var(--text-secondary);
		white-space: pre-wrap;
	}

	.scroll-widget__footer {
		justify-content: flex-end;
		margin-top: auto;
	}

	@media (max-width: 1180px) {
		.scroll-widget,
		.scroll-widget.compact,
		.scroll-widget.wide {
			grid-column: span 6;
		}
	}

	@media (max-width: 820px) {
		.scroll-canvas__header,
		.scroll-widget__header {
			flex-direction: column;
		}

		.scroll-canvas__grid {
			grid-template-columns: minmax(0, 1fr);
		}

		.scroll-widget,
		.scroll-widget.compact,
		.scroll-widget.wide,
		.scroll-widget.hero {
			grid-column: 1 / -1;
		}
	}
</style>
