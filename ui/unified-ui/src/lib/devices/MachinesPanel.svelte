<!--
  Machines — every Magician this owner runs, each behind its own Cloudflare
  tunnel (scripts/ensure-machine-tunnel.sh), read from GET /remote-machines.
  The web UI is same-origin, so using another machine means opening its own UI
  host; pairing a phone with a machine uses that machine's QR, because only
  that machine can issue its token.
-->
<script lang="ts">
	import { onMount } from 'svelte';
	import { listRemoteMachines, type RemoteMachine } from './devicePairing';

	let machines: RemoteMachine[] = [];
	let selfName: string | null = null;
	let loading = true;
	let error: string | null = null;

	async function load(): Promise<void> {
		loading = true;
		error = null;
		try {
			const list = await listRemoteMachines();
			machines = list.machines;
			selfName = list.self;
		} catch (err) {
			error = err instanceof Error ? err.message : String(err);
		} finally {
			loading = false;
		}
	}

	function host(origin: string): string {
		try {
			return new URL(origin).host;
		} catch {
			return origin;
		}
	}

	onMount(load);
</script>

<section class="machines-panel" aria-labelledby="machines-title">
	<div class="panel-header">
		<div>
			<p class="overline">Self-hosted connection</p>
			<h2 id="machines-title">
				{selfName ? `Machines · this is ${selfName}` : 'Machines'}
			</h2>
			<p>
				Each Magician runs behind its own tunnel. Open another machine to use it in the
				browser; to connect a phone to it, scan that machine's QR under Connected devices.
			</p>
		</div>
		<div class="actions">
			<button class="secondary" type="button" disabled={loading} on:click={load}>
				{loading ? 'Refreshing…' : 'Refresh'}
			</button>
		</div>
	</div>

	{#if error}
		<p class="alert" role="alert">{error}</p>
	{:else if !loading && machines.length === 0}
		<p class="empty">
			No machines are registered. Run <code>make machine-tunnel MACHINE=&lt;name&gt; INSTALL=1</code>
			on each machine to give it a tunnel and add it here.
		</p>
	{:else}
		<ul class="machine-list">
			{#each machines as machine (machine.name)}
				<li class="machine-row" class:current={machine.is_self}>
					<div class="machine-info">
						<span class="machine-name">{machine.name}</span>
						<code>{host(machine.api_origin)}</code>
					</div>
					{#if machine.is_self}
						<span class="badge">This machine</span>
					{:else}
						<a
							class="open-link"
							href={machine.ui_origin}
							target="_blank"
							rel="noopener noreferrer"
							aria-label={`Open ${machine.name}`}
						>
							Open <span aria-hidden="true">↗</span>
						</a>
					{/if}
				</li>
			{/each}
		</ul>
	{/if}
</section>

<style>
	.machines-panel { background: var(--bg-card); border: 1px solid var(--border-soft); border-radius: 8px; color: var(--text-primary); padding: 1rem; }
	.panel-header { align-items: flex-start; display: flex; gap: 1rem; justify-content: space-between; }
	.overline { color: var(--text-secondary); font-size: .75rem; font-weight: 700; margin: 0 0 .25rem; text-transform: uppercase; }
	h2, p { margin-top: 0; }
	h2 { color: var(--text-primary); font-size: 1.05rem; font-weight: 600; margin-bottom: .35rem; }
	.panel-header p:not(.overline), .empty { color: var(--text-secondary); font-size: .82rem; margin-bottom: 0; }
	.actions { display: flex; gap: .5rem; }
	button { border-radius: 6px; cursor: pointer; font: inherit; font-size: .86rem; font-weight: 600; min-height: 2.25rem; padding: .52rem .78rem; }
	button:disabled { cursor: wait; opacity: .6; }
	.secondary { background: var(--button-secondary-bg, var(--bg-soft)); border: 1px solid var(--button-secondary-border, var(--border-soft)); color: var(--button-secondary-color, var(--text-primary)); }
	.alert { background: color-mix(in srgb, var(--color-error) 10%, var(--bg-card)); border: 1px solid color-mix(in srgb, var(--color-error) 30%, var(--border-soft)); border-radius: 6px; margin: .85rem 0 0; padding: .7rem; }
	.empty { margin-top: .85rem; }
	.empty code, .machine-info code { color: var(--text-muted); font-size: .72rem; overflow-wrap: anywhere; }
	.machine-list { display: flex; flex-direction: column; gap: .5rem; list-style: none; margin: .9rem 0 0; padding: 0; }
	.machine-row { align-items: center; background: var(--bg-soft); border: 1px solid var(--border-soft); border-radius: 8px; display: flex; gap: .75rem; justify-content: space-between; padding: .65rem .8rem; }
	.machine-row.current { border-color: color-mix(in srgb, var(--accent-primary) 45%, var(--border-soft)); }
	.machine-info { display: flex; flex-direction: column; gap: .15rem; min-width: 0; }
	.machine-name { font-weight: 600; }
	.badge { background: color-mix(in srgb, var(--accent-primary) 14%, transparent); border-radius: 999px; color: var(--accent-primary); font-size: .72rem; font-weight: 700; padding: .2rem .55rem; white-space: nowrap; }
	.open-link { border: 1px solid var(--border-soft); border-radius: 6px; color: var(--text-primary); font-size: .82rem; font-weight: 600; padding: .4rem .65rem; text-decoration: none; white-space: nowrap; }
	.open-link:hover, .open-link:focus-visible { border-color: var(--accent-primary); color: var(--accent-primary); outline: none; }
	@media (max-width: 640px) { .panel-header { flex-direction: column; } }
</style>
