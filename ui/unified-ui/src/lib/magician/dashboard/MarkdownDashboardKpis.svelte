<!--
  MarkdownDashboardKpis — scans markdown for KPI-shaped lines and renders
  them as a tile row above the main body. No KPIs found → renders nothing.

  Detection rules (most-specific first):
    Rule 1 — explicit: `**Label: value** (KPI)` anywhere in body
    Rule 2 — `## KPIs` (or `## Key Metrics` / `## Metrics`) heading block:
             every following bullet line until the next heading is parsed
             as `- Label: value` (or `- **Label**: value`)
    Rule 3 — bold-label bullets anywhere: `- **Label**: value` where the
             value passes the numeric-like test

  "Numeric-like" means: starts with optional currency symbol, then a digit,
  then digits / commas / dots, then an optional unit (letters or %).
-->
<script lang="ts">
	export let source: string = '';

	interface Kpi {
		label: string;
		value: string;
		unit: string | null;
	}

	function parse(text: string): Kpi[] {
		const out: Kpi[] = [];
		const seen = new Set<string>();

		const explicit = /\*\*([^*]+?):\s*([^*]+?)\*\*\s*\(KPI\)/gi;
		let m: RegExpExecArray | null;
		while ((m = explicit.exec(text)) !== null) {
			const k = makeKpi(m[1].trim(), m[2].trim());
			if (k && !seen.has(k.label.toLowerCase())) {
				out.push(k);
				seen.add(k.label.toLowerCase());
			}
		}

		const lines = text.split(/\r?\n/);
		for (let i = 0; i < lines.length; i++) {
			if (!/^#{1,3}\s+(KPIs?|Key\s+Metrics?|Metrics?)\s*$/i.test(lines[i])) continue;
			for (let j = i + 1; j < lines.length; j++) {
				const line = lines[j].trim();
				if (/^#{1,6}\s/.test(line)) break;
				if (!line) continue;
				const bullet = line.match(/^[-*+]\s+(?:\*\*([^*]+?)\*\*|([^:]+?))\s*:\s*(.+?)\s*$/);
				if (bullet) {
					const label = (bullet[1] ?? bullet[2] ?? '').trim();
					const value = bullet[3].trim();
					if (isNumericLike(value)) {
						const k = makeKpi(label, value);
						if (k && !seen.has(k.label.toLowerCase())) {
							out.push(k);
							seen.add(k.label.toLowerCase());
						}
					}
				}
			}
		}

		const boldBullet = /^\s*[-*+]\s+\*\*([^*]+)\*\*\s*:\s*(.+?)\s*$/;
		for (const line of lines) {
			const match = line.match(boldBullet);
			if (!match) continue;
			const label = match[1].trim();
			const value = match[2].trim();
			if (!isNumericLike(value)) continue;
			const k = makeKpi(label, value);
			if (k && !seen.has(k.label.toLowerCase())) {
				out.push(k);
				seen.add(k.label.toLowerCase());
			}
		}

		return out.slice(0, 6);
	}

	function isNumericLike(value: string): boolean {
		return /[$€£¥]?[\d][\d,.]*\s*[a-zA-Z%]*$/.test(value.trim());
	}

	function makeKpi(label: string, value: string): Kpi | null {
		if (!label || !value) return null;
		const split = value.match(/^([$€£¥]?[\d][\d,.]*)\s*([%a-zA-Z][a-zA-Z\s]*)?$/);
		if (split) {
			return { label, value: split[1].trim(), unit: split[2]?.trim() || null };
		}
		return { label, value, unit: null };
	}

	$: kpis = parse(source);
</script>

{#if kpis.length > 0}
	<div class="kpi-row" role="list">
		{#each kpis as kpi (kpi.label)}
			<div class="kpi-tile" role="listitem">
				<div class="kpi-label">{kpi.label}</div>
				<div class="kpi-value">
					<span class="value-text">{kpi.value}</span>
					{#if kpi.unit}<span class="value-unit">{kpi.unit}</span>{/if}
				</div>
			</div>
		{/each}
	</div>
{/if}

<style>
	.kpi-row {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
		gap: 16px;
		margin-bottom: var(--theme-block-gap, 32px);
	}
	.kpi-tile {
		background-color: var(--theme-color-surface, #fff);
		border: 1px solid var(--theme-color-border, rgba(0, 0, 0, 0.08));
		border-radius: 10px;
		padding: 18px 22px;
		box-shadow: 0 1px 3px var(--theme-color-shadow, rgba(0, 0, 0, 0.04));
		transition: transform 150ms ease-out;
	}
	.kpi-tile:hover {
		transform: translateY(calc(-1 * var(--theme-motion-hover-lift, 1px)));
	}
	.kpi-label {
		font-family: var(--theme-font-body, system-ui);
		font-size: 0.75rem;
		text-transform: uppercase;
		letter-spacing: 0.06em;
		color: var(--theme-color-foreground-muted, #6B7280);
		margin-bottom: 8px;
	}
	.kpi-value {
		display: flex;
		align-items: baseline;
		gap: 6px;
	}
	.value-text {
		font-family: var(--theme-font-mono, ui-monospace, monospace);
		font-size: clamp(1.5rem, 1.2rem + 0.8vw, 2rem);
		font-weight: 600;
		color: var(--theme-color-accent, #C24E1B);
		line-height: 1.1;
		letter-spacing: -0.01em;
	}
	.value-unit {
		font-family: var(--theme-font-body);
		font-size: 0.95rem;
		color: var(--theme-color-foreground-muted);
		font-weight: 500;
	}
</style>
