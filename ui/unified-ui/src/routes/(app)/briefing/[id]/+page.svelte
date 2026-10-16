<!--
  /briefing/[id] — single-surface detail page.

  Companion to the /briefing aggregator route. /briefing lists every surface
  pinned to that route as compact tiles; tapping a tile (or visiting by id)
  opens it here with the full DashboardChrome + format dispatcher.

  Works for ANY published surface regardless of the surface's own `route`
  field — accessed by surface_id. Loads the surface, picks the active
  theme, applies CSS variables, wraps the body in DashboardChrome, and
  dispatches to the right renderer based on the source `media_type`.

  Dispatch table:
    text/markdown           → MarkdownDashboard
    text/html               → HtmlDashboard
    application/json        → smart dispatch:
                                · MUI-JSON shape → MuijRenderer (live data via data_source)
                                · Array of objects → AutoTable
                                · Plain object   → KeyValueGrid
                                · Other JSON     → JsonViewer
    text/xml | application/xml → XmlViewer
    text/plain              → TextDashboard
-->
<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { page } from '$app/stores';
	import { goto } from '$app/navigation';
	import {
		ensureRegistryLoaded,
		currentTheme
	} from '$lib/stores/themeStore';
	import {
		loadPublishedSurfaceRecords,
		readPublishedSurfaceRender
	} from '$lib/magician/presto/surfaces/publishedSurfaces';
	import type { PublishedSurfaceRenderRecord } from '$lib/types/surfaces';

	import DashboardChrome from '$lib/magician/dashboard/DashboardChrome.svelte';
	import MarkdownDashboard from '$lib/magician/dashboard/MarkdownDashboard.svelte';
	import HtmlDashboard from '$lib/magician/dashboard/HtmlDashboard.svelte';
	import AutoTable from '$lib/magician/dashboard/AutoTable.svelte';
	import KeyValueGrid from '$lib/magician/dashboard/KeyValueGrid.svelte';
	import JsonViewer from '$lib/magician/dashboard/JsonViewer.svelte';
	import XmlViewer from '$lib/magician/dashboard/XmlViewer.svelte';
	import TextDashboard from '$lib/magician/dashboard/TextDashboard.svelte';
	import MuijRenderer from '$lib/magician/components/generative/MuijRenderer.svelte';
	import type { MuijComponent } from '$lib/stores/muijStore';

	$: surfaceId = $page.params.id;

	let render: PublishedSurfaceRenderRecord | null = null;
	let loadError: string | null = null;
	let isLoading = false;
	let lastRefreshed: number | null = null;

	async function loadSurface(): Promise<void> {
		if (!surfaceId) return;
		isLoading = true;
		loadError = null;
		try {
			render = await readPublishedSurfaceRender(surfaceId);
			lastRefreshed = Date.now();
		} catch (err) {
			try {
				const aliasRoute = `/briefing/${surfaceId}`;
				const records = await loadPublishedSurfaceRecords({
					route_target: aliasRoute,
					maxItems: 1
				});
				const resolvedSurfaceId = records[0]?.manifest.surface_id;
				if (!resolvedSurfaceId) {
					throw err;
				}
				render = await readPublishedSurfaceRender(resolvedSurfaceId);
				lastRefreshed = Date.now();
			} catch (fallbackErr) {
				const msg = fallbackErr instanceof Error ? fallbackErr.message : String(fallbackErr);
				loadError = msg;
				render = null;
			}
		} finally {
			isLoading = false;
		}
	}

	// No dashboard-theme override on this page. The previous version did
	// `selectedThemeId.set('editorial')` + `applyThemeToCssVariables(...)`
	// in onMount, which writes inline styles on `document.documentElement`
	// for every `--theme-color-*` variable. Those inline styles WIN over
	// the app-level `[data-theme="..."]` selectors in app.css, so the
	// user's chosen app theme (jarvis / longhand-dark / mario-8bit / ...)
	// was being steamrolled by the editorial dashboard theme — the
	// briefing page rendered with a permanently white-ish surface no
	// matter which theme the user picked. Same anti-pattern the `/llm`
	// page comment warned about.
	//
	// Letting the page inherit the app theme means DashboardChrome and
	// the body styles below pick up the user's theme tokens via the
	// `:root` bridge in app.css. Future: if a published surface
	// genuinely declares its own dashboard theme via
	// `surface.dashboard_theme`, we can opt back in there — but only
	// when the surface explicitly asks for it.
	onMount(async () => {
		await loadSurface();
		// Keep registry warmed up so DashboardChrome's theme-aware
		// children (charts, motion stagger) have the registry in place
		// if they look it up — but do NOT install inline overrides.
		void ensureRegistryLoaded();
	});

	onDestroy(() => {});

	function handleRefresh(): void {
		void loadSurface();
		// The chrome's CustomEvent (magician:dashboard-refresh) is already
		// being broadcast — bound chart/table/MetricCard components re-fetch
		// via window listener. We additionally reload the surface payload
		// itself so non-live content (markdown/html/text) also refreshes.
	}

	function isMuijShape(value: unknown): boolean {
		if (!value) return false;
		if (Array.isArray(value)) {
			return (
				value.length > 0 &&
				value.every(
					(v) =>
						typeof v === 'object' &&
						v !== null &&
						('type' in (v as object) || 'component_type' in (v as object))
				)
			);
		}
		if (typeof value === 'object' && value !== null) {
			return 'type' in value || 'component_type' in value || 'components' in value || 'layout' in value;
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

	function normalizeMuijComponent(value: unknown, fallbackId: string): MuijComponent | null {
		if (!isPlainObject(value)) return null;
		const rawType = value.component_type ?? value.type;
		const componentType = typeof rawType === 'string' ? rawType.trim() : '';
		const rawId = value.id;
		const id = typeof rawId === 'string' && rawId.trim().length > 0
			? rawId.trim()
			: fallbackId;
		if (!componentType) return null;
		const props = isPlainObject(value.props) ? value.props : {};
		const children = Array.isArray(value.children)
			? value.children
					.map((child, index) => normalizeMuijComponent(child, `${id}:child-${index + 1}`))
					.filter((child): child is MuijComponent => child !== null)
			: undefined;
		return {
			id,
			component_type: componentType,
			label: typeof value.label === 'string' ? value.label : undefined,
			source: typeof value.source === 'string' ? value.source : undefined,
			query: typeof value.query === 'string' ? value.query : undefined,
			props,
			static_snapshot: value.static_snapshot,
			...(children && children.length > 0 ? { children } : {})
		};
	}

	function muijComponentsFrom(value: unknown): MuijComponent[] {
		const arr: unknown[] = Array.isArray(value)
			? value
			: isPlainObject(value)
				? Array.isArray(value.components)
					? value.components
					: Array.isArray(value.layout)
						? value.layout
						: 'type' in value || 'component_type' in value
							? [value]
							: []
				: [];
		return arr
			.map((component, index) => normalizeMuijComponent(component, `component-${index + 1}`))
			.filter((component): component is MuijComponent => component !== null);
	}

	function renderText(record: PublishedSurfaceRenderRecord | null): string {
		if (!record) return '';
		if (typeof record.text_content === 'string' && record.text_content.trim().length > 0) {
			return record.text_content;
		}
		return record.source_output_summary ?? record.surface?.summary ?? '';
	}

	function jsonContent(record: PublishedSurfaceRenderRecord | null): unknown {
		if (!record) return undefined;
		if (record.json_content !== undefined) return record.json_content;
		const text = renderText(record);
		if (!text.trim()) return undefined;
		try {
			return JSON.parse(text);
		} catch {
			return undefined;
		}
	}

	function inferRenderKind(mediaType: string): string {
		if (mediaType.startsWith('text/markdown')) return 'markdown';
		if (mediaType.startsWith('text/html')) return 'html';
		if (mediaType.startsWith('application/json') || mediaType.includes('+json')) return 'json';
		if (mediaType.includes('xml')) return 'xml';
		if (mediaType.startsWith('text/plain')) return 'plain_text';
		return 'unsupported_output';
	}

	async function openBriefingCanvas(): Promise<void> {
		await goto('/briefing', { replaceState: false, noScroll: false });
	}

	$: theme = $currentTheme;
	$: surfaceMediaType = render?.media_type ?? render?.surface?.media_type ?? 'text/plain';
	$: renderKind = render?.render_kind ?? inferRenderKind(surfaceMediaType);
	$: textBody = renderText(render);
	$: jsonBody = jsonContent(render);
	$: muijBody = render?.muij_document;
	$: muijComponents =
		muijBody?.layout && muijBody.layout.length > 0
			? muijBody.layout
			: isMuijShape(jsonBody)
				? muijComponentsFrom(jsonBody)
				: [];
	$: title = render?.surface?.title ?? 'Dashboard';
	$: summary = render?.surface?.summary ?? '';
	$: publishedBy = render?.source_agent_id ?? null;
	$: permalink = typeof window !== 'undefined' ? window.location.href : null;
</script>

<svelte:head>
	<title>{title}</title>
</svelte:head>

<div class="briefing-detail-page">
	<div class="briefing-detail-nav">
		<button type="button" on:click={openBriefingCanvas}>Briefing canvas</button>
		{#if render?.surface?.surface_id}
			<span>{render.surface.surface_id}</span>
		{/if}
	</div>

	{#if isLoading && !render}
		<div class="dashboard-status">Loading dashboard…</div>
	{:else if loadError}
		<div class="dashboard-status dashboard-error">
			<strong>Failed to load dashboard.</strong>
			<div class="error-detail">{loadError}</div>
			<button type="button" on:click={loadSurface}>Retry</button>
		</div>
	{:else if render}
		<DashboardChrome
			{title}
			{summary}
			lastRefreshed={lastRefreshed}
			{publishedBy}
			{permalink}
			{theme}
			on:refresh={handleRefresh}
		>
			{#if renderKind === 'muij_surface'}
				{#if muijComponents.length > 0}
					<MuijRenderer
						components={muijComponents}
						agentId={muijBody?.agent_id ?? ''}
						idNamespace={`briefing-detail:${render.surface.surface_id}`}
					/>
				{:else}
					<div class="dashboard-status dashboard-error">
						<strong>Published layout is unavailable.</strong>
						<div class="error-detail">
							{render.unavailable_reason ?? 'No materialized MUIJ layout was returned for this surface.'}
						</div>
						<button type="button" on:click={loadSurface}>Retry</button>
					</div>
				{/if}
			{:else if renderKind === 'markdown'}
				<MarkdownDashboard content={textBody} />
			{:else if renderKind === 'html'}
				<HtmlDashboard html={textBody} />
			{:else if renderKind === 'json'}
				{#if isMuijShape(jsonBody)}
					<MuijRenderer
						components={muijComponents}
						idNamespace={`briefing-detail:${render.surface.surface_id}`}
					/>
				{:else if isArrayOfObjects(jsonBody)}
					<AutoTable rows={jsonBody} />
				{:else if isPlainObject(jsonBody)}
					<KeyValueGrid object={jsonBody} />
				{:else}
					<JsonViewer node={jsonBody} />
				{/if}
			{:else if renderKind === 'xml'}
				<XmlViewer xml={textBody} />
			{:else if renderKind === 'plain_text'}
				<TextDashboard text={textBody} />
			{:else}
				<div class="dashboard-status dashboard-error">
					<strong>This surface cannot be rendered.</strong>
					<div class="error-detail">
						{render.unavailable_reason ?? `Unsupported render kind: ${renderKind}`}
					</div>
				</div>
			{/if}
		</DashboardChrome>
	{:else}
		<div class="dashboard-status">No dashboard data.</div>
	{/if}
</div>

<style>
	.briefing-detail-page {
		min-height: calc(100vh - 120px);
		margin: -0.5rem -1rem -1rem;
		padding: 1rem 1rem 2.5rem;
		/* Transparent — let the app shell's `--bg-base` show through so
		   the briefing page picks up the user's app theme. Painting a
		   solid `--theme-color-background` (or radial accent gradient)
		   here would lay a second surface on top of the app background,
		   which on dark themes reads as an "extra background" panel that
		   doesn't change with the rest of the chrome. */
		background: transparent;
		box-sizing: border-box;
	}

	.briefing-detail-nav {
		width: min(100%, var(--theme-container-max-width, 960px));
		margin: 0 auto 0.75rem;
		display: flex;
		align-items: center;
		gap: 0.75rem;
		flex-wrap: wrap;
		color: var(--theme-color-foreground-muted, #666);
		font-family: var(--theme-font-body, system-ui, sans-serif);
		font-size: 0.82rem;
	}

	.briefing-detail-nav button {
		font: inherit;
		border: 1px solid var(--theme-color-border, rgba(0, 0, 0, 0.08));
		border-radius: 999px;
		padding: 0.38rem 0.7rem;
		background: color-mix(in srgb, var(--theme-color-surface, #fff) 72%, transparent);
		color: var(--theme-color-foreground, #1a1814);
		cursor: pointer;
		box-shadow: 0 1px 6px var(--theme-color-shadow, rgba(0, 0, 0, 0.05));
	}

	.briefing-detail-nav button:hover {
		border-color: var(--theme-color-accent, #c24e1b);
		color: var(--theme-color-accent, #c24e1b);
	}

	.dashboard-status {
		max-width: 720px;
		margin: 60px auto;
		padding: 24px;
		text-align: center;
		color: var(--theme-color-foreground-muted, #666);
		font-family: var(--theme-font-body, system-ui, sans-serif);
	}

	.dashboard-error {
		color: var(--theme-color-accent, #c00);
	}

	.dashboard-error strong {
		display: block;
		font-family: var(--theme-font-display, var(--theme-font-body));
		font-size: 1.25rem;
		margin-bottom: 8px;
	}

	.dashboard-error .error-detail {
		font-family: var(--theme-font-mono, monospace);
		font-size: 0.85rem;
		margin: 8px 0 16px;
	}

	.dashboard-error button {
		font-family: var(--theme-font-body);
		padding: 8px 14px;
		border-radius: 6px;
		border: 1px solid var(--theme-color-border);
		background-color: var(--theme-color-surface, transparent);
		color: inherit;
		cursor: pointer;
	}

	@media (max-width: 720px) {
		.briefing-detail-page {
			margin: -0.5rem -0.75rem -1rem;
			padding-inline: 0.75rem;
		}
	}
</style>
