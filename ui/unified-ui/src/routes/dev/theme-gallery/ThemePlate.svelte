<script lang="ts">
	/**
	 * One theme plate. Rendered inside its own document (`?plate=<id>` puts the
	 * theme on <html>) so every rule in app.css applies exactly as in the app:
	 * tokens, `body`-scoped overrides, textures, and daisyUI's own theme
	 * variables. A plate scoped only by a `data-theme` on this section would
	 * miss the 61 `[data-theme] body …` rules twelve themes rely on, which is
	 * why backgrounds and accents looked wrong for Risograph, Retro, Mario and
	 * friends in the first cut.
	 *
	 * The controls are the app's own component library (Button, Card, Input,
	 * Select, Checkbox, RadioGroup, Toggle, Tabs, Badge, Progress, Alert,
	 * Table): several themes carry their look inside those components (Retro
	 * squares every button and lifts every card on a block shadow there, not
	 * in app.css), so daisyUI stand-ins restyled from tokens showed rounded,
	 * flat Retro controls the app never renders.
	 */
	import { BRAND_MARK_PATH, BRAND_MARK_VIEWBOX } from '$lib/shared/brand/mark';
	import Alert from '$lib/magician/components/generative/Alert.svelte';
	import Badge from '$lib/magician/components/generative/Badge.svelte';
	import Button from '$lib/magician/components/generative/Button.svelte';
	import Card from '$lib/magician/components/generative/Card.svelte';
	import Checkbox from '$lib/magician/components/generative/Checkbox.svelte';
	import Input from '$lib/magician/components/generative/Input.svelte';
	import Progress from '$lib/magician/components/generative/Progress.svelte';
	import ProgressBar from '$lib/magician/components/generative/ProgressBar.svelte';
	import RadioGroup from '$lib/magician/components/generative/RadioGroup.svelte';
	import Select from '$lib/magician/components/generative/Select.svelte';
	import Table from '$lib/magician/components/generative/Table.svelte';
	import Tabs from '$lib/magician/components/generative/Tabs.svelte';
	import Toggle from '$lib/magician/components/generative/Toggle.svelte';
	import type { Plate } from './plates';

	let { item, number }: { item: Plate; number: string } = $props();

	const swatches = [
		['--bg-base', 'base'],
		['--bg-surface', 'surface'],
		['--bg-elevated', 'elevated'],
		['--text-primary', 'text'],
		['--text-secondary', 'text 2'],
		['--text-muted', 'muted'],
		['--accent-primary', 'accent'],
		['--accent-secondary', 'accent 2'],
		['--border-default', 'border']
	] as const;

	const workspaces = [
		{ value: 'default', label: 'Default workspace' },
		{ value: 'governance', label: 'Governance' },
		{ value: 'observability', label: 'Observability' }
	];
	const localities = [
		{ value: 'local', label: 'Local' },
		{ value: 'cloud', label: 'Cloud' }
	];
	const tabs = [
		{ label: 'Today', content: 'Three tasks need you; two finished overnight.' },
		{ label: 'Tasks', content: '' },
		{ label: 'Chat', content: '' }
	];
	const columns = [
		{ key: 'task', label: 'Task' },
		{ key: 'status', label: 'Status' },
		{ key: 'updated', label: 'Updated', width: '5rem' }
	];
	const rows = [
		{ id: 't1', task: 'Draft the Q3 memo', status: 'done', updated: '12:04' },
		{ id: 't2', task: 'Reconcile invoices', status: 'running', updated: '11:52' },
		{ id: 't3', task: 'Book the Kyoto stay', status: 'waiting', updated: '09:20' }
	];
	const bars = [34, 52, 41, 70, 58, 86, 63, 77];
	const line = bars.map((v, i) => `${14 + i * 30},${96 - v * 0.9}`).join(' ');
</script>

	<section class="plate" data-theme={item.id} id={item.id} aria-labelledby={`${item.id}-title`}>
		<header class="plate-head">
			<div class="plate-index">
				<span class="brand-tile" aria-hidden="true">
					<svg width="16" height="16" viewBox={BRAND_MARK_VIEWBOX} fill="currentColor"><path d={BRAND_MARK_PATH} /></svg>
				</span>
				<span class="plate-no">{number}</span>
			</div>
			<div>
				<h2 id={`${item.id}-title`} class="plate-title">{item.name}</h2>
				<p class="plate-desc">{item.description}</p>
			</div>
			<code class="plate-id">data-theme="{item.id}"</code>
		</header>

		<div class="voices">
			<div class="voice voice-display"><span class="voice-aa">Aa</span><span class="voice-meta">display · {item.fonts.display}</span></div>
			<div class="voice voice-primary"><span class="voice-aa">Aa</span><span class="voice-meta">primary · {item.fonts.primary}</span></div>
			<div class="voice voice-mono"><span class="voice-aa">Aa</span><span class="voice-meta">mono · {item.fonts.mono}</span></div>
			<div class="voice voice-brand"><span class="voice-aa">magican</span><span class="voice-meta">brand · {item.fonts.brand}</span></div>
		</div>

		<div class="swatches" role="list" aria-label="Palette">
			{#each swatches as [token, label]}
				<div class="swatch" role="listitem">
					<span class="swatch-chip" style={`background: var(${token}, transparent)`}></span>
					<span class="swatch-label">{label}</span>
				</div>
			{/each}
		</div>

		<div class="sheet">
			<div class="cell cell-type">
				<h1>The product, in its own voice</h1>
				<h3>Subtitles carry the second voice</h3>
				<p>Body copy is how the product speaks: short sentences, one idea each, set in the primary face so long passages stay comfortable.</p>
				<p class="meta">exec_9f2a1c · 12:04:31 · 3.2 s · 1,204 tok</p>
			</div>

			<div class="cell">
				<div class="row">
					<Button label="Run task" variant="primary" />
					<Button label="Review" variant="secondary" />
					<Button label="Later" variant="outline" />
					<Button label="Cancel" variant="outline" />
				</div>
				<div class="row fields">
					<Input placeholder="Ask anything…" ariaLabel="Text input" idBase={`${item.id}-ask`} />
					<Select options={workspaces} value="default" ariaLabel="Dropdown" idBase={`${item.id}-ws`} />
				</div>
				<div class="row">
					<Checkbox label="Notify me" checked idBase={`${item.id}-c1`} />
					<Checkbox label="Keep draft" idBase={`${item.id}-c2`} />
					<RadioGroup options={localities} value="local" name={`${item.id}-r`} idBase={`${item.id}-r`} />
					<Toggle label="Streaming" checked />
				</div>
				<Tabs {tabs} activeIndex={0} idBase={`${item.id}-tabs`} />
				<div class="row">
					<Badge text="running" color="primary" />
					<Badge text="done" color="success" />
					<Badge text="waiting" color="warning" />
					<Badge text="failed" color="error" />
					<Badge text="draft" />
				</div>
				<ProgressBar percent={64} label="Index rebuild" ariaLabel="Progress" />
				<Alert type="info" message="Approval envelopes are off; every gated act is asked about." />
			</div>

			<div class="cell">
				<Table {columns} {rows} />
				<figure class="chart">
					<svg viewBox="0 0 240 110" role="img" aria-label="Bars and a line in the theme's accents">
						<g class="grid">
							<line x1="0" y1="24" x2="240" y2="24" /><line x1="0" y1="48" x2="240" y2="48" /><line x1="0" y1="72" x2="240" y2="72" /><line x1="0" y1="96" x2="240" y2="96" />
						</g>
						{#each bars as v, i}
							<rect x={4 + i * 30} y={96 - v * 0.9} width="20" height={v * 0.9} class={i % 2 ? 'bar-alt' : 'bar'} rx="2" />
						{/each}
						<polyline points={line} class="trend" />
						<text x="4" y="108" class="axis">mon</text><text x="214" y="108" class="axis">sun</text>
					</svg>
					<figcaption class="meta">tokens / day · p95 3.2 s</figcaption>
				</figure>
			</div>

			<div class="cell cell-chat">
				<div class="chat chat-start">
					<div class="chat-bubble chat-bubble-neutral">Booked the Kyoto stay for the 14th to the 18th. The receipt is in your inbox and the calendar hold is in place.</div>
				</div>
				<div class="chat chat-end">
					<div class="chat-bubble chat-bubble-primary">Move it one day later and keep the same hotel.</div>
				</div>
				<div class="chat chat-start">
					<div class="chat-bubble chat-bubble-neutral chat-bubble--streaming">Checking availability for the 15th…</div>
				</div>
				<div class="composer">
					<Input placeholder="Reply…" ariaLabel="Composer" idBase={`${item.id}-reply`} />
					<Button label="Send" variant="primary" size="sm" />
				</div>
			</div>

			<div class="cell cell-task">
				<Card title="Reconcile September invoices" subtitle="agent cfo · started 11:52 · 9,310 tok" body="Match 42 vendor invoices against bank lines and flag anything over ten percent off." elevation={1}>
					<Progress percent={38} label="18 / 42 matched" />
					<div class="row card-actions">
						<Button label="Open" variant="primary" size="sm" />
						<Button label="Pause" variant="outline" size="sm" />
						<Badge text="running" color="primary" />
					</div>
				</Card>
				<ul class="task-list">
					<li><Checkbox label="Send the memo to legal" checked idBase={`${item.id}-l1`} /><span class="meta">today</span></li>
					<li><Checkbox label="Renew the domain" idBase={`${item.id}-l2`} /><span class="meta">thu</span></li>
					<li><Checkbox label="Reply to the landlord" idBase={`${item.id}-l3`} /><span class="meta">fri</span></li>
				</ul>
			</div>

			<div class="cell cell-log">
				<pre class="log"><span class="lv-info">INFO </span> dispatch queue started workers=12
<span class="lv-info">INFO </span> llm_call_completed op=channel_classify 2.1s
<span class="lv-warn">WARN </span> retrieval owner changed; re-seeding head
<span class="lv-err">ERROR</span> gws gmail: invalid_grant (account=work)</pre>
			</div>
		</div>
	</section>

<style>
	/* ── Plates: everything below reads the theme's own tokens ── */
	.plate {
		background: var(--bg-base, #fff);
		color: var(--text-primary, #111);
		font-family: var(--font-primary);
		border: 1px solid var(--border-default, var(--border-soft, rgba(0, 0, 0, 0.12)));
		border-radius: var(--radius-xl, 14px);
		padding: 1.75rem 1.75rem 2rem;
		box-shadow: 0 18px 40px -28px rgba(0, 0, 0, 0.45);
		scroll-margin-top: 1rem;
		/* The plate lays itself out by ITS width, not the viewport's: on the
		   sheet it lives in an iframe half the page wide, and a two-column
		   sheet in there left cells too narrow for a row of four buttons. */
		container-type: inline-size;
		container-name: plate;
		overflow: hidden;
	}
	.plate-head {
		display: grid; grid-template-columns: auto 1fr auto; gap: 1.1rem; align-items: start;
		padding-bottom: 1.1rem; border-bottom: 1px solid var(--border-default, var(--border-soft, currentColor));
	}
	.plate-index { display: flex; flex-direction: column; align-items: center; gap: 0.3rem; }
	.plate-no { font-family: var(--font-mono); font-size: 0.7rem; color: var(--text-muted, inherit); }
	.plate-title { margin: 0; font-family: var(--font-display); font-size: 1.35rem; line-height: 1.1; font-weight: 700; }
	.plate-desc { margin: 0.35rem 0 0; color: var(--text-secondary, inherit); font-size: 0.9rem; }
	.plate-id { font-family: var(--font-mono); font-size: 0.68rem; color: var(--text-muted, inherit); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; max-width: 16rem; }

	.voices { display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: 0.75rem; margin: 1.25rem 0; }
	.voice { background: var(--bg-surface, transparent); border-radius: var(--radius-md, 10px); padding: 0.85rem 1rem; display: grid; gap: 0.35rem; min-width: 0; }
	.voice-aa { font-size: 1.5rem; line-height: 1; }
	.voice-display .voice-aa { font-family: var(--font-display); font-weight: 700; }
	.voice-primary .voice-aa { font-family: var(--font-primary); }
	.voice-mono .voice-aa { font-family: var(--font-mono); }
	.voice-brand .voice-aa { font-family: var(--font-brand); font-size: 1.15rem; font-weight: 700; }
	.voice-meta { font-family: var(--font-mono); font-size: 0.62rem; color: var(--text-muted, inherit); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }

	.swatches { display: flex; flex-wrap: wrap; gap: 0.7rem; margin-bottom: 1.5rem; }
	.swatch { display: grid; gap: 0.25rem; justify-items: center; }
	.swatch-chip { width: 36px; height: 24px; border-radius: var(--radius-sm, 6px); border: 1px solid var(--border-default, rgba(127, 127, 127, 0.4)); display: block; }
	.swatch-label { font-family: var(--font-mono); font-size: 0.6rem; color: var(--text-muted, inherit); }

	/* Cells are plain regions in the theme's surface colour and radius; the
	   controls inside them are the app's own components, untouched. */
	.sheet { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 1.25rem; }
	.cell { background: var(--bg-surface, transparent); border: 1px solid var(--border-soft, var(--border-default, rgba(127, 127, 127, 0.25))); border-radius: var(--radius-lg, 12px); padding: 1.25rem 1.35rem; display: grid; gap: 0.9rem; align-content: start; min-width: 0; }
	.cell-type h1 { margin: 0 0 0.25rem; font-family: var(--font-display); font-size: 1.6rem; line-height: 1.1; font-weight: 700; letter-spacing: -0.01em; }
	.cell-type h3 { margin: 0; font-family: var(--font-display); font-size: 1rem; font-weight: 600; color: var(--text-secondary, inherit); }
	.cell-type p { margin: 0; font-family: var(--font-primary); font-size: 0.9rem; line-height: 1.6; color: var(--text-body, var(--text-primary, inherit)); }
	.meta { font-family: var(--font-data, var(--font-mono)); font-size: 0.72rem; color: var(--text-muted, inherit); }
	/* Every row wraps; nothing in a plate may overflow its cell. */
	.row { display: flex; flex-wrap: wrap; gap: 0.6rem; row-gap: 0.7rem; align-items: center; min-width: 0; }
	.row > :global(*) { min-width: 0; max-width: 100%; }
	.fields > :global(*) { flex: 1 1 12rem; }
	.card-actions { margin-top: 0.75rem; }

	.chart { margin: 0.25rem 0 0; display: grid; gap: 0.5rem; }
	.chart svg { width: 100%; height: auto; display: block; }
	.grid line { stroke: var(--border-default, currentColor); stroke-opacity: 0.35; stroke-width: 1; }
	.bar { fill: var(--accent-primary, currentColor); }
	.bar-alt { fill: var(--accent-secondary, var(--accent-primary, currentColor)); opacity: 0.85; }
	.trend { fill: none; stroke: var(--text-primary, currentColor); stroke-width: 1.5; stroke-linejoin: round; }
	.axis { fill: var(--text-muted, currentColor); font-family: var(--font-mono); font-size: 8px; }

	.cell-chat { gap: 0.5rem; }
	.composer { display: flex; gap: 0.6rem; margin-top: 0.75rem; align-items: center; }
	.composer > :global(:first-child) { flex: 1; min-width: 0; }
	.chat-bubble--streaming { opacity: 0.8; }

	.task-list { list-style: none; margin: 0.25rem 0 0; padding: 0; display: grid; gap: 0.55rem; }
	.task-list li { display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 0.5rem; align-items: center; font-size: 0.88rem; }

	/* ── Chat bubbles ──
	   The chat page renders daisyUI's chat classes and maps them to the theme
	   in its own scoped CSS (`.chat-bubble-primary` → --accent-primary /
	   --text-on-accent, neutral → --bg-card ringed by --border-soft); the
	   plate mirrors that mapping so bubbles read as the chat page paints
	   them rather than in daisyUI's default palette. */
	.plate :global(.chat-bubble) { font-family: var(--font-primary); font-size: 0.88rem; line-height: 1.5; padding: 0.7rem 1rem; max-width: 88%; overflow-wrap: anywhere; }
	.plate :global(.chat-bubble-primary), .plate :global(.chat-end .chat-bubble-primary::before) { background-color: var(--accent-primary); color: var(--text-on-accent, #fff); }
	.plate :global(.chat-bubble-neutral), .plate :global(.chat-start .chat-bubble-neutral::before) { background-color: var(--bg-card, var(--bg-elevated, transparent)); color: var(--text-primary, inherit); }
	.plate :global(.chat-bubble-neutral) { box-shadow: 0 0 0 1px var(--border-soft, var(--border-default, transparent)); }

	.cell-log { grid-column: 1 / -1; }

	/* Narrow plate: one column, two voice tiles per row. Container queries
	   answer to the plate's width wherever it is embedded. */
	@container plate (max-width: 760px) {
		.sheet { grid-template-columns: 1fr; }
	}
	@container plate (max-width: 560px) {
		.voices { grid-template-columns: repeat(2, minmax(0, 1fr)); }
		.plate-head { grid-template-columns: auto 1fr; }
		.plate-id { grid-column: 1 / -1; max-width: 100%; }
	}
	.log { margin: 0; padding: 1rem 1.1rem; border-radius: var(--radius-md, 10px); background: var(--bg-elevated, var(--bg-base, transparent)); border: 1px solid var(--border-soft, var(--border-default, transparent)); font-family: var(--font-mono); font-size: 0.74rem; line-height: 1.65; white-space: pre-wrap; color: var(--text-secondary, inherit); }
	.lv-info { color: var(--accent-secondary, var(--text-muted, inherit)); }
	.lv-warn { color: var(--warning, var(--accent-primary, inherit)); }
	.lv-err { color: var(--danger, var(--error, var(--accent-primary, inherit))); }

	/* Fallback for engines without container queries. */
	@media (max-width: 640px) {
		.sheet { grid-template-columns: 1fr; }
		.voices { grid-template-columns: repeat(2, minmax(0, 1fr)); }
	}
</style>
