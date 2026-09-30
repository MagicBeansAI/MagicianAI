<!--
  ScrollCardPreview — format-aware preview shown inside scroll-card on
  /today (and anywhere else surfaces need a compact preview).

  Smart dispatch by source `media_type` and shape:
    - MUI-JSON dashboard → first KPI tile (if present) OR component-type summary
    - HTML               → first heading + element count
    - JSON array         → "N rows × M cols" mini-table
    - Plain JSON object  → first 3 keys
    - text/markdown      → leading paragraph or first heading
    - text/plain         → leading line
    - other              → fallback string

  Keeps preview compact: max ~3 lines vertical. Theme-styled.
-->
<script lang="ts">
	import type { PublishedSurfaceRecord } from '$lib/types/surfaces';

	export let record: PublishedSurfaceRecord;

	interface Kpi {
		label: string;
		value: string;
		unit: string | null;
	}

	type PreviewMode =
		| { kind: 'kpis'; tiles: Kpi[]; total: number }
		| { kind: 'rows'; rows: number; cols: number; firstColumns: string[] }
		| { kind: 'json-object'; keys: string[] }
		| { kind: 'html'; firstHeading: string; tagCount: number }
		| { kind: 'mui-summary'; types: string[] }
		| { kind: 'text'; text: string };

	function classify(rec: PublishedSurfaceRecord): PreviewMode {
		const mediaType = rec.render?.media_type ?? rec.render?.surface?.media_type ?? '';
		const text = rec.render?.text_content ?? '';
		const json = rec.render?.json_content;

		if (mediaType.startsWith('application/json') || rec.render?.render_kind === 'json') {
			return classifyJson(json ?? safeJsonParse(text));
		}
		if (mediaType.startsWith('text/html') || rec.render?.render_kind === 'html') {
			return classifyHtml(text);
		}
		if (rec.render?.render_kind === 'muij_surface' && rec.render.muij_document) {
			return classifyMuij(rec.render.muij_document.layout as unknown[]);
		}
		// markdown / plain / xml all fall through to text
		return classifyText(text || rec.manifest.summary || rec.render?.surface.summary || '');
	}

	function safeJsonParse(text: string): unknown {
		if (!text || !text.trim()) return null;
		try {
			return JSON.parse(text);
		} catch {
			return null;
		}
	}

	function classifyJson(value: unknown): PreviewMode {
		// MUI-JSON shape — drill into components to find KPI tiles.
		if (isMuijShape(value)) {
			const components = muijComponents(value);
			const kpis = extractMuijKpis(components);
			if (kpis.length > 0) {
				return { kind: 'kpis', tiles: kpis.slice(0, 3), total: kpis.length };
			}
			const types = uniqueComponentTypes(components).slice(0, 5);
			return { kind: 'mui-summary', types };
		}
		// Array of records → mini-table
		if (Array.isArray(value) && value.length > 0 && typeof value[0] === 'object' && value[0] !== null) {
			const cols = Object.keys(value[0] as Record<string, unknown>);
			return { kind: 'rows', rows: value.length, cols: cols.length, firstColumns: cols.slice(0, 3) };
		}
		// Plain object
		if (value && typeof value === 'object' && !Array.isArray(value)) {
			const keys = Object.keys(value as Record<string, unknown>).slice(0, 3);
			return { kind: 'json-object', keys };
		}
		return { kind: 'text', text: '(empty data)' };
	}

	function isMuijShape(value: unknown): boolean {
		if (!value) return false;
		if (Array.isArray(value)) {
			return value.every(
				(v) => typeof v === 'object' && v !== null && ('type' in (v as object) || 'component_type' in (v as object))
			);
		}
		if (typeof value === 'object' && value !== null) {
			return 'components' in value || 'layout' in value;
		}
		return false;
	}

	function muijComponents(value: unknown): unknown[] {
		if (Array.isArray(value)) return value;
		if (typeof value === 'object' && value !== null) {
			const components = (value as { components?: unknown }).components;
			if (Array.isArray(components)) return components;
			const layout = (value as { layout?: unknown }).layout;
			if (Array.isArray(layout)) return layout;
		}
		return [];
	}

	function extractMuijKpis(components: unknown[]): Kpi[] {
		const result: Kpi[] = [];
		for (const c of components) {
			if (typeof c !== 'object' || c === null) continue;
			const obj = c as Record<string, unknown>;
			const type = (obj.type ?? obj.component_type) as string | undefined;
			if (type === 'MetricCard') {
				const label = (obj.label as string | undefined) ?? '';
				let valueStr = '—';
				if (obj.value !== undefined) {
					valueStr = String(obj.value);
				} else if (obj.dataSource) {
					valueStr = '~live~';
				}
				if (label) result.push({ label, value: valueStr, unit: null });
			}
			// Recurse into Grid / Stack / Container / Panel children
			const kids = obj.components ?? (obj.props as { components?: unknown } | undefined)?.components;
			if (Array.isArray(kids)) {
				result.push(...extractMuijKpis(kids));
			}
		}
		return result;
	}

	function uniqueComponentTypes(components: unknown[]): string[] {
		const seen = new Set<string>();
		function walk(arr: unknown[]): void {
			for (const c of arr) {
				if (typeof c !== 'object' || c === null) continue;
				const obj = c as Record<string, unknown>;
				const type = (obj.type ?? obj.component_type) as string | undefined;
				if (type) seen.add(type);
				const kids = obj.components ?? (obj.props as { components?: unknown } | undefined)?.components;
				if (Array.isArray(kids)) walk(kids);
			}
		}
		walk(components);
		return Array.from(seen);
	}

	function classifyMuij(layout: unknown[]): PreviewMode {
		const kpis = extractMuijKpis(layout);
		if (kpis.length > 0) {
			return { kind: 'kpis', tiles: kpis.slice(0, 3), total: kpis.length };
		}
		return { kind: 'mui-summary', types: uniqueComponentTypes(layout).slice(0, 5) };
	}

	function classifyHtml(text: string): PreviewMode {
		const headingMatch = text.match(/<(h[1-3])[^>]*>([^<]+)<\/h[1-3]>/i);
		const tagCount = (text.match(/<[a-zA-Z][^>]*>/g) ?? []).length;
		return {
			kind: 'html',
			firstHeading: headingMatch ? headingMatch[2].trim() : '(no heading)',
			tagCount
		};
	}

	function classifyText(text: string): PreviewMode {
		const trimmed = text.trim();
		if (!trimmed) return { kind: 'text', text: '(empty)' };
		// Skip leading heading hashes
		const para = trimmed.split(/\n\n+/)[0].replace(/^#+\s+/, '').trim();
		const oneLine = para.replace(/\s+/g, ' ');
		return { kind: 'text', text: oneLine.slice(0, 200) };
	}

	$: mode = classify(record);
</script>

<div class="preview">
	{#if mode.kind === 'kpis'}
		<div class="preview-kpis" class:multi={mode.tiles.length > 1}>
			{#each mode.tiles as tile (tile.label)}
				<div class="kpi-mini">
					<div class="kpi-mini-label">{tile.label}</div>
					<div class="kpi-mini-value">{tile.value}</div>
				</div>
			{/each}
		</div>
		{#if mode.total > mode.tiles.length}
			<div class="preview-foot">+{mode.total - mode.tiles.length} more KPI{mode.total - mode.tiles.length === 1 ? '' : 's'}</div>
		{/if}
	{:else if mode.kind === 'rows'}
		<div class="preview-meta">📊 {mode.rows} row{mode.rows === 1 ? '' : 's'} · {mode.cols} col{mode.cols === 1 ? '' : 's'}</div>
		<div class="preview-foot">{mode.firstColumns.join(' · ')}</div>
	{:else if mode.kind === 'json-object'}
		<div class="preview-meta">{'{}'} {mode.keys.length} key{mode.keys.length === 1 ? '' : 's'}</div>
		<div class="preview-foot">{mode.keys.join(' · ')}</div>
	{:else if mode.kind === 'html'}
		<div class="preview-meta">🧩 {mode.firstHeading}</div>
		<div class="preview-foot">{mode.tagCount} HTML element{mode.tagCount === 1 ? '' : 's'}</div>
	{:else if mode.kind === 'mui-summary'}
		<div class="preview-meta">◍ Dashboard · {mode.types.length} component type{mode.types.length === 1 ? '' : 's'}</div>
		<div class="preview-foot">{mode.types.join(' · ')}</div>
	{:else}
		<div class="preview-text">{mode.text}</div>
	{/if}
</div>

<style>
	.preview {
		display: flex;
		flex-direction: column;
		gap: 6px;
		min-height: 56px;
	}

	.preview-kpis {
		display: flex;
		gap: 14px;
		flex-wrap: wrap;
	}

	.kpi-mini {
		display: flex;
		flex-direction: column;
		gap: 2px;
		min-width: 0;
	}

	.kpi-mini-label {
		font-size: 0.65rem;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.06em;
		color: var(--theme-color-foreground-muted, var(--text-secondary, #6B7280));
	}

	.kpi-mini-value {
		font-family: var(--theme-font-mono, ui-monospace, monospace);
		font-size: 1.1rem;
		font-weight: 600;
		color: var(--theme-color-accent, var(--accent-primary, #3b82f6));
		font-variant-numeric: tabular-nums;
		line-height: 1.1;
	}

	.preview-meta {
		font-family: var(--theme-font-display, var(--theme-font-body, system-ui));
		font-size: 0.875rem;
		font-weight: 500;
		color: var(--theme-color-foreground, var(--text-primary, #1F2937));
	}

	.preview-foot {
		font-family: var(--theme-font-mono, ui-monospace, monospace);
		font-size: 0.72rem;
		color: var(--theme-color-foreground-muted, var(--text-secondary, #6B7280));
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.preview-text {
		font-size: 0.875rem;
		color: var(--theme-color-foreground-muted, var(--text-secondary, #6B7280));
		display: -webkit-box;
		-webkit-line-clamp: 3;
		-webkit-box-orient: vertical;
		line-clamp: 3;
		overflow: hidden;
	}
</style>
