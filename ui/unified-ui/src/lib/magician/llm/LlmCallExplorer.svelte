<script lang="ts">
	import { onMount } from 'svelte';

	export let rangeDays = 7;
	export let refreshToken = 0;

	type Fact = Record<string, unknown>;

	interface FactPage {
		rows: Fact[];
		total: number;
	}

	interface ReadEnvelope<T> {
		freshness: { source_latest_at_ms: number | null; stale: boolean };
		warnings: string[];
		data: T;
	}

	interface CallDetail {
		call: Fact;
		provider_attempts: Fact[];
		tool_timeline: Fact[];
	}

	let mounted = false;
	let calls: Fact[] = [];
	let selectedCallId: string | null = null;
	let detail: CallDetail | null = null;
	let loadingCalls = false;
	let loadingDetail = false;
	let callsError: string | null = null;
	let detailError: string | null = null;
	let callsAbort: AbortController | null = null;
	let detailAbort: AbortController | null = null;
	let lastLoadKey = '';

	/**
	 * The governed LLM endpoints put the real cause in the body
	 * (`{"error": "…"}`) — a cross-dataset integrity rejection, a contended
	 * DuckDB guard, and a malformed range all arrive as different messages.
	 * Throwing `HTTP ${status}` discarded every one of them and left the
	 * surface reporting a bare status with nothing to act on.
	 */
	async function readErrorMessage(response: Response): Promise<string> {
		try {
			const body = (await response.json()) as { error?: unknown };
			if (typeof body?.error === 'string' && body.error.trim()) return body.error;
		} catch {
			// Non-JSON or empty body — fall through to the status line.
		}
		return `HTTP ${response.status}`;
	}

	function text(row: Fact, key: string): string {
		const value = row[key];
		return typeof value === 'string' ? value : value == null ? '' : String(value);
	}

	function numberValue(row: Fact, key: string): number | null {
		const value = row[key];
		if (typeof value === 'number' && Number.isFinite(value)) return value;
		if (typeof value === 'string' && value.trim() !== '') {
			const parsed = Number(value);
			return Number.isFinite(parsed) ? parsed : null;
		}
		return null;
	}

	function callId(row: Fact): string {
		return text(row, 'llm_call_id');
	}

	function callTitle(row: Fact): string {
		return text(row, 'operation') || text(row, 'source_surface') || 'LLM call';
	}

	function callSubtitle(row: Fact): string {
		return [text(row, 'provider'), text(row, 'model')].filter(Boolean).join(' · ') || 'Route unavailable';
	}

	function status(row: Fact): string {
		return text(row, 'call_terminal_state') || (row.transport_success === true ? 'succeeded' : 'unknown');
	}

	function timestamp(row: Fact): string {
		const value = numberValue(row, 'observed_at_ms') ?? numberValue(row, 'timestamp_ms');
		if (value == null) return 'Time unavailable';
		return new Intl.DateTimeFormat(undefined, {
			month: 'short',
			day: 'numeric',
			hour: 'numeric',
			minute: '2-digit',
			second: '2-digit'
		}).format(new Date(value));
	}

	function duration(row: Fact): string {
		const value = numberValue(row, 'latency_ms');
		if (value == null) return '—';
		return value >= 1000 ? `${(value / 1000).toFixed(2)}s` : `${Math.round(value)}ms`;
	}

	function stageLabel(row: Fact): string {
		return text(row, 'tool_lineage_stage').replaceAll('_', ' ') || 'unknown stage';
	}

	function stageDetail(row: Fact): string {
		const outcome = text(row, 'tool_outcome');
		const owner = text(row, 'tool_failure_owner');
		const code = text(row, 'tool_failure_code');
		return [outcome, owner && owner !== 'none' ? `${owner} owned` : '', code]
			.filter(Boolean)
			.join(' · ');
	}

	function stageTone(row: Fact): string {
		const outcome = text(row, 'tool_outcome');
		if (['failed', 'denied', 'cancelled', 'timed_out'].includes(outcome)) return 'bad';
		if (outcome === 'succeeded' || outcome === 'consumed') return 'good';
		if (text(row, 'tool_lineage_stage') === 'linkage_gap') return 'warn';
		return 'neutral';
	}

	async function loadCalls(): Promise<void> {
		callsAbort?.abort();
		const controller = new AbortController();
		callsAbort = controller;
		loadingCalls = true;
		callsError = null;
		const toMs = Date.now();
		const effectiveDays = Math.min(31, Math.max(1, rangeDays || 31));
		const fromMs = toMs - effectiveDays * 24 * 60 * 60 * 1000;
		try {
			const query = new URLSearchParams({
				from_ms: String(fromMs),
				to_ms: String(toMs),
				limit: '30',
				order_by: 'observed_at_ms',
				descending: 'true'
			});
			const response = await fetch(`/api/magician/v2/analytics/llm/calls?${query}`, {
				signal: controller.signal
			});
			if (!response.ok) throw new Error(await readErrorMessage(response));
			const envelope = (await response.json()) as ReadEnvelope<FactPage>;
			calls = envelope.data.rows ?? [];
			if (selectedCallId && !calls.some((row) => callId(row) === selectedCallId)) {
				selectedCallId = null;
				detail = null;
			}
		} catch (error) {
			if (controller.signal.aborted) return;
			callsError = error instanceof Error ? error.message : 'Unknown error';
			calls = [];
		} finally {
			if (callsAbort === controller) loadingCalls = false;
		}
	}

	async function selectCall(row: Fact): Promise<void> {
		const id = callId(row);
		if (!id) return;
		selectedCallId = id;
		detailAbort?.abort();
		const controller = new AbortController();
		detailAbort = controller;
		loadingDetail = true;
		detailError = null;
		detail = null;
		try {
			const response = await fetch(
				`/api/magician/v2/analytics/llm/calls/${encodeURIComponent(id)}`,
				{ signal: controller.signal }
			);
			if (!response.ok) throw new Error(await readErrorMessage(response));
			const envelope = (await response.json()) as ReadEnvelope<CallDetail>;
			detail = envelope.data;
		} catch (error) {
			if (controller.signal.aborted) return;
			detailError = error instanceof Error ? error.message : 'Unknown error';
		} finally {
			if (detailAbort === controller) loadingDetail = false;
		}
	}

	function closeDetail(): void {
		detailAbort?.abort();
		selectedCallId = null;
		detail = null;
		detailError = null;
	}

	onMount(() => {
		mounted = true;
		return () => {
			callsAbort?.abort();
			detailAbort?.abort();
		};
	});

	$: loadKey = `${rangeDays}:${refreshToken}`;
	$: if (mounted && loadKey !== lastLoadKey) {
		lastLoadKey = loadKey;
		void loadCalls();
	}
</script>

<section class="explorer" aria-labelledby="llm-call-explorer-title">
	<header class="explorer__header">
		<div>
			<h2 id="llm-call-explorer-title">Call and tool lineage</h2>
			<p>Select a logical call to inspect provider attempts and the authoritative tool lifecycle.</p>
		</div>
		<button type="button" class="quiet-button" on:click={() => void loadCalls()} disabled={loadingCalls}>
			{loadingCalls ? 'Refreshing…' : 'Refresh'}
		</button>
	</header>

	<div class="explorer__body" class:explorer__body--detail={selectedCallId !== null}>
		<div class="call-list" aria-label="Recent logical LLM calls">
			{#if callsError}
				<p class="state state--error">Recent calls unavailable — {callsError}</p>
			{:else if loadingCalls && calls.length === 0}
				<p class="state">Loading recent calls…</p>
			{:else if calls.length === 0}
				<p class="state">No canonical calls were captured in this range.</p>
			{:else}
				{#each calls as row (callId(row))}
					<button
						type="button"
						class="call-row"
						class:call-row--selected={selectedCallId === callId(row)}
						on:click={() => void selectCall(row)}
						aria-pressed={selectedCallId === callId(row)}
					>
						<span class="call-row__main">
							<strong>{callTitle(row)}</strong>
							<small>{callSubtitle(row)}</small>
						</span>
						<span class="call-row__meta">
							<span class="status" data-status={status(row)}>{status(row)}</span>
							<small>{duration(row)} · {timestamp(row)}</small>
						</span>
					</button>
				{/each}
			{/if}
		</div>

		{#if selectedCallId}
			<aside class="detail" aria-live="polite" aria-label="Selected LLM call detail">
				<div class="detail__header">
					<div>
						<span class="eyebrow">Logical call</span>
						<strong>{selectedCallId}</strong>
					</div>
					<button type="button" class="icon-button" on:click={closeDetail} aria-label="Close call detail">×</button>
				</div>
				{#if loadingDetail}
					<p class="state">Assembling call lineage…</p>
				{:else if detailError}
					<p class="state state--error">Call detail unavailable — {detailError}</p>
				{:else if detail}
					<div class="detail__facts">
						<div><span>Operation</span><strong>{callTitle(detail.call)}</strong></div>
						<div><span>Route</span><strong>{callSubtitle(detail.call)}</strong></div>
						<div><span>Attempts</span><strong>{detail.provider_attempts.length}</strong></div>
						<div><span>Tool stages</span><strong>{detail.tool_timeline.length}</strong></div>
					</div>
					<div class="timeline-header">
						<h3>Tool timeline</h3>
						<span>{detail.tool_timeline.length} stage{detail.tool_timeline.length === 1 ? '' : 's'}</span>
					</div>
					{#if detail.tool_timeline.length === 0}
						<p class="state state--compact">This call did not emit a tool lifecycle.</p>
					{:else}
						<ol class="timeline">
							{#each detail.tool_timeline as stage, index (`${text(stage, 'tool_execution_id')}:${text(stage, 'record_revision')}:${index}`)}
								<li>
									<span class="timeline__marker" data-tone={stageTone(stage)}></span>
									<div class="timeline__content">
										<div class="timeline__title">
											<strong>{stageLabel(stage)}</strong>
											<span>{text(stage, 'tool_name') || 'unknown tool'}</span>
										</div>
										{#if stageDetail(stage)}<small>{stageDetail(stage)}</small>{/if}
										<code>{text(stage, 'tool_execution_id')}</code>
									</div>
								</li>
							{/each}
						</ol>
					{/if}
				{/if}
			</aside>
		{/if}
	</div>
</section>

<style>
	.explorer {
		display: flex;
		flex-direction: column;
		overflow: hidden;
		border: 1px solid var(--theme-color-border, rgba(0, 0, 0, 0.08));
		border-radius: 14px;
		background: var(--theme-color-surface, #fff);
		box-shadow: 0 1px 3px var(--theme-color-shadow, rgba(0, 0, 0, 0.04));
	}

	.explorer__header,
	.detail__header,
	.timeline__title {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 16px;
	}

	.explorer__header {
		padding: 18px 20px;
		border-bottom: 1px solid var(--theme-color-border, rgba(0, 0, 0, 0.08));
	}

	h2,
	h3,
	p {
		margin: 0;
	}

	h2 {
		color: var(--theme-color-foreground, #111827);
		font-family: var(--theme-font-display, var(--theme-font-body));
		font-size: 1.125rem;
	}

	.explorer__header p {
		margin-top: 5px;
		color: var(--theme-color-foreground-muted, #6b7280);
		font-size: 0.86rem;
	}

	.quiet-button,
	.icon-button,
	.call-row {
		font: inherit;
		color: inherit;
		border: 0;
		cursor: pointer;
	}

	.quiet-button,
	.icon-button {
		background: var(--theme-color-surface-muted, rgba(0, 0, 0, 0.04));
		border: 1px solid var(--theme-color-border, rgba(0, 0, 0, 0.08));
	}

	.quiet-button {
		padding: 7px 11px;
		border-radius: 8px;
		font-size: 0.78rem;
		font-weight: 650;
	}

	.quiet-button:disabled {
		opacity: 0.55;
		cursor: wait;
	}

	.explorer__body {
		display: grid;
		grid-template-columns: minmax(0, 1fr);
		min-height: 220px;
	}

	.explorer__body--detail {
		grid-template-columns: minmax(18rem, 0.85fr) minmax(24rem, 1.15fr);
	}

	.call-list {
		min-width: 0;
		max-height: 540px;
		overflow: auto;
	}

	.call-row {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 16px;
		width: 100%;
		padding: 13px 18px;
		text-align: left;
		background: transparent;
		border-bottom: 1px solid var(--theme-color-border, rgba(0, 0, 0, 0.07));
	}

	.call-row:hover,
	.call-row:focus-visible,
	.call-row--selected {
		background: color-mix(in srgb, var(--theme-color-accent, #6366f1) 8%, transparent);
		outline: none;
	}

	.call-row__main,
	.call-row__meta {
		display: flex;
		flex-direction: column;
		gap: 3px;
		min-width: 0;
	}

	.call-row__main strong,
	.call-row__main small,
	.call-row__meta small {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.call-row__main strong {
		color: var(--theme-color-foreground, #111827);
		font-size: 0.88rem;
	}

	.call-row small,
	.timeline small,
	.timeline-header span {
		color: var(--theme-color-foreground-muted, #6b7280);
		font-size: 0.74rem;
	}

	.call-row__meta {
		align-items: flex-end;
		max-width: 48%;
	}

	.status,
	.eyebrow {
		font-size: 0.67rem;
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.06em;
		color: var(--theme-color-foreground-muted, #6b7280);
	}

	.status[data-status='succeeded'] {
		color: var(--color-success, #15803d);
	}

	.status[data-status='failed'],
	.status[data-status='cancelled'],
	.status[data-status='tombstoned'] {
		color: var(--color-error, #b91c1c);
	}

	.detail {
		min-width: 0;
		max-height: 540px;
		overflow: auto;
		border-left: 1px solid var(--theme-color-border, rgba(0, 0, 0, 0.08));
		background: var(--theme-color-surface-muted, rgba(0, 0, 0, 0.025));
	}

	.detail__header {
		padding: 15px 17px;
		border-bottom: 1px solid var(--theme-color-border, rgba(0, 0, 0, 0.08));
	}

	.detail__header > div {
		display: flex;
		flex-direction: column;
		gap: 4px;
		min-width: 0;
	}

	.detail__header strong {
		overflow: hidden;
		color: var(--theme-color-foreground, #111827);
		font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
		font-size: 0.73rem;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.icon-button {
		flex: 0 0 auto;
		width: 30px;
		height: 30px;
		border-radius: 8px;
		font-size: 1.2rem;
		line-height: 1;
	}

	.detail__facts {
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		gap: 1px;
		background: var(--theme-color-border, rgba(0, 0, 0, 0.08));
	}

	.detail__facts > div {
		display: flex;
		flex-direction: column;
		gap: 4px;
		padding: 11px 14px;
		background: var(--theme-color-surface, #fff);
	}

	.detail__facts span {
		color: var(--theme-color-foreground-muted, #6b7280);
		font-size: 0.66rem;
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.06em;
	}

	.detail__facts strong {
		overflow: hidden;
		color: var(--theme-color-foreground, #111827);
		font-size: 0.8rem;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.timeline-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: 15px 17px 8px;
	}

	.timeline-header h3 {
		color: var(--theme-color-foreground, #111827);
		font-size: 0.85rem;
	}

	.timeline {
		margin: 0;
		padding: 2px 17px 18px;
		list-style: none;
	}

	.timeline li {
		position: relative;
		display: grid;
		grid-template-columns: 12px minmax(0, 1fr);
		gap: 10px;
		padding-bottom: 13px;
	}

	.timeline li:not(:last-child)::before {
		position: absolute;
		top: 12px;
		bottom: 0;
		left: 5px;
		width: 1px;
		background: var(--theme-color-border, rgba(0, 0, 0, 0.12));
		content: '';
	}

	.timeline__marker {
		position: relative;
		z-index: 1;
		width: 11px;
		height: 11px;
		margin-top: 3px;
		border: 2px solid var(--theme-color-surface, #fff);
		border-radius: 999px;
		background: var(--theme-color-foreground-muted, #6b7280);
		box-shadow: 0 0 0 1px var(--theme-color-border, rgba(0, 0, 0, 0.12));
	}

	.timeline__marker[data-tone='good'] { background: var(--color-success, #15803d); }
	.timeline__marker[data-tone='bad'] { background: var(--color-error, #b91c1c); }
	.timeline__marker[data-tone='warn'] { background: var(--color-warning, #b45309); }

	.timeline__content {
		display: flex;
		flex-direction: column;
		gap: 3px;
		min-width: 0;
	}

	.timeline__title strong {
		color: var(--theme-color-foreground, #111827);
		font-size: 0.78rem;
		text-transform: capitalize;
	}

	.timeline__title span {
		overflow: hidden;
		color: var(--theme-color-foreground-muted, #6b7280);
		font-size: 0.72rem;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.timeline code {
		overflow: hidden;
		color: var(--theme-color-foreground-muted, #6b7280);
		font-size: 0.64rem;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.state {
		padding: 18px;
		color: var(--theme-color-foreground-muted, #6b7280);
		font-size: 0.82rem;
	}

	.state--compact { padding-top: 4px; }
	.state--error { color: var(--color-error, #b91c1c); }

	@media (max-width: 800px) {
		.explorer__body--detail { grid-template-columns: minmax(0, 1fr); }
		.detail { border-top: 1px solid var(--theme-color-border, rgba(0, 0, 0, 0.08)); border-left: 0; }
		.call-row__meta { max-width: 42%; }
	}

	@media (max-width: 520px) {
		.explorer__header { align-items: flex-start; }
		.call-row { align-items: flex-start; padding-inline: 14px; }
		.call-row__meta { max-width: 45%; }
		.call-row__meta small { display: none; }
	}
</style>
