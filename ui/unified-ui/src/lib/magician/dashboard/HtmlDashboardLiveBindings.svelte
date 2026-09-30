<!--
  HtmlDashboardLiveBindings — post-render scanner that finds elements with
  `data-magician-source` inside an HtmlDashboard scope and renders the
  declared chart/table/KPI into them via live data.

  Attribute value is JSON-encoded LiveSourceSpec:

    <div data-magician-source='{
      "kind": "llm_calls_sql",
      "sql": "SELECT agent_id, SUM(cost_usd) AS spend FROM llm_calls GROUP BY agent_id LIMIT 10",
      "render": "bar",
      "xField": "agent_id",
      "yField": "spend"
    }'></div>

  Supported `render` values: bar, line, pie, table, kpi.
  Re-fetches on `magician:dashboard-refresh` events bubbling through scopeRoot.

  All rendering happens via programmatic DOM construction (no innerHTML on
  untrusted data) — the spec is validated, values escaped via textContent.
-->
<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { timedFetch } from '$lib/shared/fetch';

	export let scopeRoot: HTMLElement;

	type Render = 'bar' | 'line' | 'pie' | 'table' | 'kpi';
	interface LiveSourceSpec {
		kind: 'llm_calls_sql' | 'memory_events_sql';
		sql: string;
		render: Render;
		xField?: string;
		yField?: string;
		labelField?: string;
		valueField?: string;
		title?: string;
	}

	const SVG_NS = 'http://www.w3.org/2000/svg';

	let refreshHandler: (() => void) | null = null;

	function endpointForKind(kind: LiveSourceSpec['kind']): string {
		if (kind === 'memory_events_sql') {
			return '/api/magician/v2/analytics/memory_events/query';
		}
		return '/api/magician/v2/analytics/llm_calls/query';
	}

	async function fetchRows(spec: LiveSourceSpec): Promise<{ columns: string[]; rows: unknown[][] }> {
		const r = await timedFetch(endpointForKind(spec.kind), {
			method: 'POST',
			headers: { 'Content-Type': 'application/json' },
			body: JSON.stringify({ sql: spec.sql })
		});
		if (!r.ok) {
			const body = await r.text();
			throw new Error(`Live source fetch failed (${r.status}): ${body}`);
		}
		return r.json();
	}

	function rowsToRecords(
		columns: string[],
		rows: unknown[][]
	): Array<Record<string, unknown>> {
		return rows.map((row) => {
			const out: Record<string, unknown> = {};
			columns.forEach((col, i) => {
				out[col] = row[i];
			});
			return out;
		});
	}

	function clear(target: HTMLElement): void {
		while (target.firstChild) target.removeChild(target.firstChild);
	}

	function renderMessage(target: HTMLElement, message: string, isError: boolean): void {
		clear(target);
		const box = document.createElement('div');
		box.style.padding = '12px 14px';
		box.style.border = '1px solid var(--theme-color-border, rgba(0,0,0,0.08))';
		box.style.borderRadius = '6px';
		box.style.color = 'var(--theme-color-foreground-muted, #666)';
		box.style.fontFamily = 'var(--theme-font-mono, monospace)';
		box.style.fontSize = '0.85em';
		if (isError) box.style.color = 'var(--theme-color-accent, #c00)';
		box.textContent = (isError ? '⚠ ' : '') + message;
		target.appendChild(box);
	}

	function formatNumber(v: unknown): string {
		if (typeof v === 'number') {
			return new Intl.NumberFormat(undefined, { maximumFractionDigits: 2 }).format(v);
		}
		return String(v ?? '—');
	}

	function renderKpi(target: HTMLElement, value: unknown, label?: string): void {
		clear(target);
		const card = document.createElement('div');
		card.style.background = 'var(--theme-color-surface, #fff)';
		card.style.border = '1px solid var(--theme-color-border)';
		card.style.borderRadius = '10px';
		card.style.padding = '18px 22px';
		if (label) {
			const labelEl = document.createElement('div');
			labelEl.style.fontSize = '0.75rem';
			labelEl.style.textTransform = 'uppercase';
			labelEl.style.letterSpacing = '0.06em';
			labelEl.style.color = 'var(--theme-color-foreground-muted)';
			labelEl.style.marginBottom = '8px';
			labelEl.textContent = label;
			card.appendChild(labelEl);
		}
		const valueEl = document.createElement('div');
		valueEl.style.fontFamily = 'var(--theme-font-mono, monospace)';
		valueEl.style.fontSize = '2rem';
		valueEl.style.fontWeight = '600';
		valueEl.style.color = 'var(--theme-color-accent)';
		valueEl.textContent = formatNumber(value);
		card.appendChild(valueEl);
		target.appendChild(card);
	}

	function renderTable(target: HTMLElement, columns: string[], rows: unknown[][]): void {
		clear(target);
		if (rows.length === 0) {
			renderMessage(target, 'No rows', false);
			return;
		}
		const table = document.createElement('table');
		table.style.width = '100%';
		table.style.borderCollapse = 'separate';
		table.style.borderSpacing = '0';
		table.style.border = '1px solid var(--theme-color-border)';
		table.style.borderRadius = '8px';
		table.style.overflow = 'hidden';
		table.style.background = 'var(--theme-color-surface)';

		const thead = document.createElement('thead');
		thead.style.background = 'var(--theme-color-background)';
		const trh = document.createElement('tr');
		for (const col of columns) {
			const th = document.createElement('th');
			th.textContent = col;
			th.style.padding = '10px 14px';
			th.style.textAlign = 'left';
			th.style.borderBottom = '1px solid var(--theme-color-border)';
			th.style.textTransform = 'uppercase';
			th.style.letterSpacing = '0.04em';
			th.style.fontSize = '0.78em';
			trh.appendChild(th);
		}
		thead.appendChild(trh);
		table.appendChild(thead);

		const tbody = document.createElement('tbody');
		for (const row of rows) {
			const tr = document.createElement('tr');
			for (const cell of row) {
				const td = document.createElement('td');
				td.textContent = formatNumber(cell);
				td.style.padding = '10px 14px';
				td.style.borderBottom = '1px solid var(--theme-color-border)';
				tr.appendChild(td);
			}
			tbody.appendChild(tr);
		}
		table.appendChild(tbody);
		target.appendChild(table);
	}

	function makeSvg(viewBox: string, maxHeight: string): SVGSVGElement {
		const svg = document.createElementNS(SVG_NS, 'svg');
		svg.setAttribute('viewBox', viewBox);
		svg.style.width = '100%';
		svg.style.maxHeight = maxHeight;
		return svg;
	}

	function svgText(x: number, y: number, content: string, css: Partial<CSSStyleDeclaration>): SVGTextElement {
		const el = document.createElementNS(SVG_NS, 'text');
		el.setAttribute('x', String(x));
		el.setAttribute('y', String(y));
		Object.assign((el as unknown as { style: CSSStyleDeclaration }).style, css);
		el.textContent = content;
		return el;
	}

	function renderBarOrLine(
		target: HTMLElement,
		kind: 'bar' | 'line',
		records: Array<Record<string, unknown>>,
		xField: string,
		yField: string,
		title?: string
	): void {
		clear(target);
		const values = records.map((r) => Number(r[yField] ?? 0));
		const labels = records.map((r) => String(r[xField] ?? ''));
		if (values.length === 0) {
			renderMessage(target, 'No data', false);
			return;
		}
		const max = Math.max(...values, 0);
		const width = 600;
		const height = 220;
		const margin = { top: 16, right: 12, bottom: 32, left: 40 };
		const plotW = width - margin.left - margin.right;
		const plotH = height - margin.top - margin.bottom;

		const svg = makeSvg(`0 0 ${width} ${height}`, '320px');
		const g = document.createElementNS(SVG_NS, 'g');
		g.setAttribute('transform', `translate(${margin.left},${margin.top})`);

		if (title) {
			g.appendChild(
				svgText(0, -2, title, {
					fontFamily: 'var(--theme-font-display)',
					fontSize: '0.95rem',
					fontWeight: '600',
					fill: 'var(--theme-color-foreground)'
				})
			);
		}

		if (kind === 'bar') {
			const bw = plotW / values.length;
			values.forEach((v, i) => {
				const h = max > 0 ? (v / max) * plotH : 0;
				const x = i * bw + 4;
				const y = plotH - h;
				const rect = document.createElementNS(SVG_NS, 'rect');
				rect.setAttribute('x', String(x));
				rect.setAttribute('y', String(y));
				rect.setAttribute('width', String(bw - 8));
				rect.setAttribute('height', String(h));
				rect.setAttribute('rx', '2');
				rect.setAttribute('fill', 'var(--theme-color-accent, #C24E1B)');
				g.appendChild(rect);
			});
			labels.forEach((l, i) => {
				const cx = i * bw + bw / 2;
				const t = svgText(cx, plotH + 16, l.slice(0, 8), {
					fontFamily: 'var(--theme-font-mono)',
					fontSize: '0.7rem',
					fill: 'var(--theme-color-foreground-muted)'
				});
				t.setAttribute('text-anchor', 'middle');
				g.appendChild(t);
			});
		} else {
			const pts = values
				.map((v, i) => {
					const x = (i * plotW) / Math.max(values.length - 1, 1);
					const y = plotH - (max > 0 ? (v / max) * plotH : 0);
					return `${x},${y}`;
				})
				.join(' ');
			const line = document.createElementNS(SVG_NS, 'polyline');
			line.setAttribute('points', pts);
			line.setAttribute('fill', 'none');
			line.setAttribute('stroke', 'var(--theme-color-accent, #C24E1B)');
			line.setAttribute('stroke-width', '2');
			g.appendChild(line);
		}

		svg.appendChild(g);
		target.appendChild(svg);
	}

	function renderPie(
		target: HTMLElement,
		records: Array<Record<string, unknown>>,
		labelField: string,
		valueField: string,
		title?: string
	): void {
		clear(target);
		const values = records.map((r) => Number(r[valueField] ?? 0));
		const labels = records.map((r) => String(r[labelField] ?? ''));
		const total = values.reduce((a, b) => a + b, 0);
		if (total === 0) {
			renderMessage(target, 'No data', false);
			return;
		}

		const wrapper = document.createElement('div');
		wrapper.style.display = 'flex';
		wrapper.style.alignItems = 'center';
		wrapper.style.gap = '16px';
		wrapper.style.flexWrap = 'wrap';

		const svg = makeSvg('0 0 200 200', '200px');
		svg.style.width = '200px';
		svg.style.height = '200px';
		if (title) {
			const t = svgText(100, 14, title, {
				fontFamily: 'var(--theme-font-display)',
				fontSize: '0.85rem',
				fontWeight: '600',
				fill: 'var(--theme-color-foreground)'
			});
			t.setAttribute('text-anchor', 'middle');
			svg.appendChild(t);
		}

		let angle = -Math.PI / 2;
		const r = 80;
		const cx = 100;
		const cy = 100;
		values.forEach((v, i) => {
			const a = (v / total) * Math.PI * 2;
			const x1 = cx + r * Math.cos(angle);
			const y1 = cy + r * Math.sin(angle);
			const x2 = cx + r * Math.cos(angle + a);
			const y2 = cy + r * Math.sin(angle + a);
			const large = a > Math.PI ? 1 : 0;
			const path = document.createElementNS(SVG_NS, 'path');
			path.setAttribute('d', `M ${cx} ${cy} L ${x1} ${y1} A ${r} ${r} 0 ${large} 1 ${x2} ${y2} Z`);
			path.setAttribute('fill', `var(--theme-chart-color-${i % 8}, var(--theme-color-accent))`);
			svg.appendChild(path);
			angle += a;
		});
		wrapper.appendChild(svg);

		const legend = document.createElement('div');
		legend.style.display = 'flex';
		legend.style.flexDirection = 'column';
		legend.style.gap = '6px';
		labels.forEach((l, i) => {
			const row = document.createElement('div');
			row.style.display = 'flex';
			row.style.alignItems = 'center';
			row.style.gap = '8px';
			row.style.fontSize = '0.85rem';
			const swatch = document.createElement('span');
			swatch.style.display = 'inline-block';
			swatch.style.width = '10px';
			swatch.style.height = '10px';
			swatch.style.borderRadius = '2px';
			swatch.style.background = `var(--theme-chart-color-${i % 8}, var(--theme-color-accent))`;
			row.appendChild(swatch);
			row.appendChild(document.createTextNode(`${l}: ${formatNumber(values[i])}`));
			legend.appendChild(row);
		});
		wrapper.appendChild(legend);

		target.appendChild(wrapper);
	}

	async function activate(spec: LiveSourceSpec, target: HTMLElement): Promise<void> {
		try {
			const { columns, rows } = await fetchRows(spec);
			const records = rowsToRecords(columns, rows);
			switch (spec.render) {
				case 'kpi':
					renderKpi(target, rows[0]?.[0] ?? null, spec.title);
					break;
				case 'table':
					renderTable(target, columns, rows);
					break;
				case 'bar':
					renderBarOrLine(
						target,
						'bar',
						records,
						spec.xField ?? columns[0],
						spec.yField ?? columns[1],
						spec.title
					);
					break;
				case 'line':
					renderBarOrLine(
						target,
						'line',
						records,
						spec.xField ?? columns[0],
						spec.yField ?? columns[1],
						spec.title
					);
					break;
				case 'pie':
					renderPie(
						target,
						records,
						spec.labelField ?? columns[0],
						spec.valueField ?? columns[1],
						spec.title
					);
					break;
				default:
					renderMessage(target, `Unknown render kind: ${(spec as { render: string }).render}`, true);
			}
		} catch (err) {
			const msg = err instanceof Error ? err.message : String(err);
			renderMessage(target, msg, true);
		}
	}

	function scan(): void {
		if (!scopeRoot) return;
		const targets = scopeRoot.querySelectorAll<HTMLElement>('[data-magician-source]');
		for (const target of Array.from(targets)) {
			const raw = target.getAttribute('data-magician-source');
			if (!raw) continue;
			let spec: LiveSourceSpec;
			try {
				spec = JSON.parse(raw);
			} catch (err) {
				renderMessage(target, `Invalid JSON in data-magician-source: ${(err as Error).message}`, true);
				continue;
			}
			if ((spec.kind !== 'llm_calls_sql' && spec.kind !== 'memory_events_sql') || !spec.sql || !spec.render) {
				renderMessage(target, 'data-magician-source requires {kind:"llm_calls_sql"|"memory_events_sql", sql, render}', true);
				continue;
			}
			renderMessage(target, 'Loading…', false);
			void activate(spec, target);
		}
	}

	onMount(() => {
		scan();
		refreshHandler = () => scan();
		scopeRoot?.addEventListener('magician:dashboard-refresh', refreshHandler);
	});

	onDestroy(() => {
		if (refreshHandler && scopeRoot) {
			scopeRoot.removeEventListener('magician:dashboard-refresh', refreshHandler);
		}
	});
</script>
