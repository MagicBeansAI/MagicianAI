<script lang="ts">
	import { onMount } from 'svelte';
	import {
		defaultCriticalDeliverySettings,
		fetchCriticalDelivery,
		fetchCriticalDeliveryStatus,
		saveCriticalDelivery,
		sendCriticalDeliveryTest,
		type CriticalDeliveryEnvelope,
		type CriticalDeliverySettings,
		type CriticalDeliveryStatus
	} from '$lib/stores/criticalDeliveryStore';
	import { showError, showSuccess } from '$lib/shared/stores/notifications';

	let envelope = $state<CriticalDeliveryEnvelope | null>(null);
	let status = $state<CriticalDeliveryStatus | null>(null);
	let draft = $state<CriticalDeliverySettings>(defaultCriticalDeliverySettings());
	let quietEnabled = $state(false);
	let error = $state<string | null>(null);
	let loading = $state(false);
	let saving = $state(false);
	let testing = $state(false);

	const dirty = $derived.by(() => {
		if (!envelope) return false;
		return JSON.stringify(effectiveDraft()) !== JSON.stringify(envelope.settings);
	});

	onMount(() => {
		void refresh(false);
	});

	function effectiveDraft(): CriticalDeliverySettings {
		return {
			...draft,
			enabled_channels: draft.enabled_channels.map((c) => c.trim().toLowerCase()).filter(Boolean),
			quiet_hours: quietEnabled ? (draft.quiet_hours ?? { start: '22:00', end: '07:00', timezone: 'UTC', interrupt_for_time_bound: true }) : null
		};
	}

	function adopt(next: CriticalDeliveryEnvelope): void {
		envelope = next;
		draft = structuredClone(next.settings);
		quietEnabled = next.settings.quiet_hours !== null;
		if (!draft.quiet_hours) {
			draft.quiet_hours = { start: '22:00', end: '07:00', timezone: 'UTC', interrupt_for_time_bound: true };
		}
	}

	async function refresh(showToast: boolean): Promise<void> {
		loading = true;
		error = null;
		try {
			adopt(await fetchCriticalDelivery());
			status = await fetchCriticalDeliveryStatus().catch(() => null);
			if (showToast) showSuccess('Critical alert settings loaded');
		} catch (caught) {
			const message = caught instanceof Error ? caught.message : 'Failed to load critical alert settings';
			error = message;
			if (showToast) showError(message);
		} finally {
			loading = false;
		}
	}

	async function save(): Promise<void> {
		saving = true;
		error = null;
		try {
			const result = await saveCriticalDelivery(effectiveDraft());
			adopt(result);
			if (result.reload_error) {
				showError(`Saved, but the live reload failed: ${result.reload_error}`);
			} else {
				showSuccess('Critical alert settings saved. Nothing was sent.');
			}
		} catch (caught) {
			const message = caught instanceof Error ? caught.message : 'Failed to save critical alert settings';
			error = message;
			showError(message);
		} finally {
			saving = false;
		}
	}

	async function sendTest(): Promise<void> {
		testing = true;
		try {
			const result = await sendCriticalDeliveryTest();
			showSuccess(
				result.destinations === 0
					? 'No destination is enabled — nothing was sent.'
					: `Test alert sent to ${result.destinations} destination${result.destinations === 1 ? '' : 's'}.`
			);
			status = await fetchCriticalDeliveryStatus().catch(() => status);
		} catch (caught) {
			showError(caught instanceof Error ? caught.message : 'The test alert could not be sent');
		} finally {
			testing = false;
		}
	}

	function toggleChannel(channel: string, enabled: boolean): void {
		const current = draft.enabled_channels.filter((c) => c !== channel);
		draft.enabled_channels = enabled ? [...current, channel] : current;
	}

	function move(channel: string, delta: number): void {
		const list = [...draft.enabled_channels];
		const index = list.indexOf(channel);
		const target = index + delta;
		if (index < 0 || target < 0 || target >= list.length) return;
		[list[index], list[target]] = [list[target], list[index]];
		draft.enabled_channels = list;
	}

	function channelInfo(channel: string): { addresses: string[]; hasOwner: boolean } {
		const known = envelope?.channels.find((c) => c.channel_type === channel);
		return { addresses: known?.owner_addresses ?? [], hasOwner: known?.has_owner ?? true };
	}

	const knownChannels = $derived.by(() => {
		const set = new Set<string>([
			...draft.enabled_channels,
			...(envelope?.channels.map((c) => c.channel_type) ?? []),
			...(envelope?.available_channels ?? [])
		]);
		return [...set];
	});

	function when(ms: number | null): string {
		if (!ms) return '—';
		return new Date(ms).toLocaleString();
	}

	function latency(p: { samples: number; p50_ms: number | null; p95_ms: number | null }): string {
		if (!p.samples) return 'no samples';
		return `p50 ${p.p50_ms ?? '—'} ms · p95 ${p.p95_ms ?? '—'} ms · ${p.samples} sample${p.samples === 1 ? '' : 's'}`;
	}
</script>

<section class="card" aria-labelledby="critical-delivery-title">
	<div class="header">
		<div>
			<p class="overline">Critical alerts</p>
			<h2 id="critical-delivery-title">Where a credential request reaches you</h2>
			<p>
				When Magician needs a password or a verification code — or a decision with a deadline — it
				alerts the private channels you enable here, in order, with a value-free card and a link to the
				exact request. Owner addresses come from <code>envoy.owner_identities</code>; this page never
				edits them, and only <strong>Send test alert</strong> sends anything.
			</p>
		</div>
		<div class="actions">
			<button type="button" disabled={loading || saving || testing} onclick={() => void refresh(true)}>
				{loading ? 'Refreshing…' : 'Refresh'}
			</button>
			<button class="secondary" type="button" disabled={loading || saving || testing || !envelope} onclick={() => void sendTest()}>
				{testing ? 'Sending…' : 'Send test alert'}
			</button>
			<button class="primary" type="button" disabled={loading || saving || testing || !dirty} onclick={() => void save()}>
				{saving ? 'Saving…' : 'Save'}
			</button>
		</div>
	</div>

	{#if error}
		<p class="alert" role="alert">{error}</p>
	{/if}

	{#if envelope}
		{#each envelope.warnings as warning (warning)}
			<p class="alert warn">{warning}</p>
		{/each}

		<div class="grid">
			<fieldset class="group">
				<legend>Channels, in preference order</legend>
				{#if knownChannels.length === 0}
					<p class="empty">No channel has an owner identity yet. Add one under <code>envoy.owner_identities</code> (or its env var) and refresh.</p>
				{/if}
				{#each knownChannels as channel (channel)}
					{@const enabled = draft.enabled_channels.includes(channel)}
					{@const info = channelInfo(channel)}
					<div class="channel" class:enabled>
						<label>
							<input
								type="checkbox"
								checked={enabled}
								disabled={loading || saving}
								onchange={(event) => toggleChannel(channel, (event.currentTarget as HTMLInputElement).checked)}
								aria-label={`Enable ${channel}`}
							/>
							<strong>{channel}</strong>
						</label>
						<span class="addresses">
							{#if info.addresses.length}{info.addresses.join(', ')}{:else if !info.hasOwner}<span class="chip warn">no owner identity</span>{:else}—{/if}
						</span>
						{#if status?.channels_last_claimed_ms[channel]}
							<span class="chip">bot seen {when(status.channels_last_claimed_ms[channel])}</span>
						{/if}
						{#if enabled}
							<span class="order">
								<button type="button" aria-label={`Move ${channel} earlier`} disabled={saving} onclick={() => move(channel, -1)}>↑</button>
								<button type="button" aria-label={`Move ${channel} later`} disabled={saving} onclick={() => move(channel, 1)}>↓</button>
							</span>
						{/if}
					</div>
				{/each}
				<label class="row">
					<input type="checkbox" bind:checked={draft.push_enabled} disabled={loading || saving} />
					<span>Registered phones receive the attention push</span>
				</label>
			</fieldset>

			<fieldset class="group">
				<legend>Policy</legend>
				<label class="row">
					<input type="radio" name="critical-policy" value="simultaneous" bind:group={draft.policy} disabled={saving} />
					<span>Alert every enabled destination at once</span>
				</label>
				<label class="row">
					<input type="radio" name="critical-policy" value="staged" bind:group={draft.policy} disabled={saving} />
					<span>Alert the first, then the next only if the provider has not accepted within</span>
					<input class="num" type="number" min="1" max="3600" bind:value={draft.staged_fallback_secs} disabled={saving || draft.policy !== 'staged'} aria-label="Staged fallback seconds" />
					<span>s</span>
				</label>
			</fieldset>

			<fieldset class="group">
				<legend>Quiet hours</legend>
				<label class="row">
					<input type="checkbox" bind:checked={quietEnabled} disabled={saving} />
					<span>Hold alerts during a daily window</span>
				</label>
				{#if quietEnabled && draft.quiet_hours}
					<div class="row">
						<label>From <input type="time" bind:value={draft.quiet_hours.start} disabled={saving} /></label>
						<label>to <input type="time" bind:value={draft.quiet_hours.end} disabled={saving} /></label>
						<label>Timezone <input type="text" bind:value={draft.quiet_hours.timezone} placeholder="Asia/Kolkata" disabled={saving} /></label>
					</div>
					<label class="row">
						<input type="checkbox" bind:checked={draft.quiet_hours.interrupt_for_time_bound} disabled={saving} />
						<span>A request with a deadline (a code that expires) may interrupt the window</span>
					</label>
				{/if}
			</fieldset>
		</div>

		<p class="meta">
			Secure link: {envelope.public_origin_configured ? 'the alert links the exact request' : 'no link origin configured — the alert says to open Magician → Attention'}.
			{#if status}
				Latency — request→queued: {latency(status.latency.request_to_enqueue)}; queued→provider accepted: {latency(status.latency.enqueue_to_acceptance)}.
			{/if}
		</p>

		{#if status && status.deliveries.length}
			<div class="table-wrap">
				<table aria-label="Recent critical-request deliveries">
					<thead>
						<tr><th>When</th><th>Request</th><th>Destination</th><th>State</th><th>Attempts</th><th>Note</th></tr>
					</thead>
					<tbody>
						{#each status.deliveries.slice(0, 20) as row (row.id)}
							<tr>
								<td>{when(row.enqueued_at_ms)}</td>
								<td><code>{row.kind === 'test' ? 'test' : row.correlation_id}</code></td>
								<td>{row.destination}</td>
								<td><span class="chip" class:warn={['failed', 'unavailable', 'ambiguous', 'expired'].includes(row.state)}>{row.state}</span></td>
								<td>{row.attempts}</td>
								<td>{row.reason ?? ''}</td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
		{/if}
	{:else if loading}
		<p class="empty">Loading critical alert settings…</p>
	{:else}
		<p class="empty">Refresh to load critical alert settings.</p>
	{/if}
</section>

<style>
	.card {
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		display: flex;
		flex-direction: column;
		gap: 1rem;
		padding: 1rem;
	}
	.header {
		display: flex;
		flex-direction: column;
		gap: 1rem;
	}
	.overline {
		color: var(--text-secondary);
		font-size: 0.75rem;
		font-weight: 700;
		margin: 0;
		text-transform: uppercase;
	}
	h2,
	p {
		margin: 0;
	}
	h2 {
		color: var(--text-primary);
		font-size: 1.05rem;
	}
	.empty,
	.meta {
		color: var(--text-secondary);
		font-size: 0.85rem;
	}
	code {
		font-size: 0.85em;
	}
	.actions {
		display: flex;
		flex-wrap: wrap;
		gap: 0.5rem;
	}
	button {
		background: var(--bg-secondary);
		border: 1px solid var(--border-soft);
		border-radius: 6px;
		color: var(--text-primary);
		cursor: pointer;
		padding: 0.45rem 0.85rem;
	}
	button.primary {
		background: var(--accent);
		border-color: var(--accent);
		color: var(--text-on-accent, #fff);
	}
	button.secondary {
		border-color: var(--accent);
	}
	button:disabled {
		cursor: not-allowed;
		opacity: 0.6;
	}
	.alert {
		background: var(--bg-secondary);
		border-left: 3px solid var(--danger, #c0392b);
		padding: 0.5rem 0.75rem;
	}
	.alert.warn {
		border-left-color: var(--warning, #d68910);
	}
	.grid {
		display: grid;
		gap: 1rem;
		grid-template-columns: repeat(auto-fit, minmax(18rem, 1fr));
	}
	.group {
		border: 1px solid var(--border-soft);
		border-radius: 6px;
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
		padding: 0.75rem;
	}
	legend {
		color: var(--text-secondary);
		font-size: 0.8rem;
		font-weight: 600;
		padding: 0 0.25rem;
	}
	.channel,
	.row {
		align-items: center;
		display: flex;
		flex-wrap: wrap;
		gap: 0.5rem;
	}
	.channel label {
		align-items: center;
		display: inline-flex;
		gap: 0.4rem;
	}
	.addresses {
		color: var(--text-secondary);
		font-family: var(--font-mono, monospace);
		font-size: 0.8rem;
	}
	.order button {
		padding: 0.1rem 0.4rem;
	}
	.num {
		width: 5rem;
	}
	.chip {
		background: var(--bg-secondary);
		border-radius: 999px;
		font-size: 0.72rem;
		padding: 0.1rem 0.5rem;
	}
	.chip.warn {
		background: var(--warning-soft, #fdf1dc);
		color: var(--warning, #8a5a00);
	}
	.table-wrap {
		overflow-x: auto;
	}
	table {
		border-collapse: collapse;
		font-size: 0.82rem;
		width: 100%;
	}
	th,
	td {
		border-bottom: 1px solid var(--border-soft);
		padding: 0.35rem 0.5rem;
		text-align: left;
		vertical-align: top;
	}
</style>
