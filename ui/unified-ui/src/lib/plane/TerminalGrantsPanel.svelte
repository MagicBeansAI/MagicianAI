<script lang="ts">
	import { onMount } from 'svelte';
	import { showError, showSuccess } from '$lib/shared/stores/notifications';
	import { hasHydratedScopeBearer } from '$lib/stores/scopeIdentityStore';
	import {
		fetchEngineAvailability,
		listTerminalGrants,
		mintTerminalGrant,
		revokeTerminalGrant,
		type EngineAvailability,
		type MintedGrant,
		type TerminalGrantRow
	} from '$lib/plane/terminalGrants';

	let grants: TerminalGrantRow[] = [];
	let loading = false;
	let minting = false;

	// Mint form state — defaults narrow on purpose: a terminal is one human's
	// seat, and an empty allowlist is the floored catalog, never "everything".
	let label = '';
	let workspace = 'default';
	let agentIdentity = 'personal-assistant';
	let harnessEngine: string = 'magician';
	let allowedTools = '';
	let ttlHours = 24;
	let maxUsd = '';
	let maxWallClockHours = '';
	let maxConcurrentRuns = '';

	let lastMinted: MintedGrant | null = null;
	let engineAvailability: EngineAvailability[] = [{ name: 'magician', installed: true }];
	// Pi 0.87.1 reaches MCP through the private spawned bridge, not a terminal
	// CLI MCP client. Keep it in chat/run choices but out of terminal grants.
	$: installedEngines = engineAvailability.filter((engine) => engine.installed && engine.name !== 'pi');

	async function refresh() {
		loading = true;
		try {
			grants = await listTerminalGrants();
		} catch (error) {
			showError(error instanceof Error ? error.message : 'Terminal grants are unavailable.');
		} finally {
			loading = false;
		}
	}

	async function mint() {
		if (!label.trim() || !workspace.trim() || !agentIdentity.trim()) {
			showError('Label, workspace, and agent identity are required.');
			return;
		}
		if (!Number.isInteger(ttlHours) || ttlHours < 1 || ttlHours > 2160) {
			showError('TTL must be between 1 and 2160 hours.');
			return;
		}
		minting = true;
		try {
			lastMinted = await mintTerminalGrant({
				label: label.trim(),
				workspace: workspace.trim(),
				agent_identity: agentIdentity.trim(),
				harness_engine: harnessEngine,
				allowed_tools: allowedTools
					.split(',')
					.map((tool) => tool.trim())
					.filter((tool) => tool.length > 0),
				ttl_hours: ttlHours,
				max_usd: parseOptionalPositive(maxUsd),
				max_wall_clock_secs:
					maxWallClockHours.trim().length > 0
						? Math.round(parseFloat(maxWallClockHours) * 3600)
						: null,
				max_concurrent_runs: parseOptionalPositive(maxConcurrentRuns)
			});
			showSuccess(`Grant minted: ${lastMinted.grant.label}`);
			label = '';
			allowedTools = '';
			maxUsd = '';
			maxWallClockHours = '';
			maxConcurrentRuns = '';
			await refresh();
		} catch (error) {
			showError(error instanceof Error ? error.message : 'The grant could not be minted.');
		} finally {
			minting = false;
		}
	}

	async function revoke(row: TerminalGrantRow) {
		try {
			await revokeTerminalGrant(row.id);
			showSuccess(`Grant revoked: ${row.label}`);
			await refresh();
		} catch (error) {
			showError(error instanceof Error ? error.message : 'The grant could not be revoked.');
		}
	}

	function parseOptionalPositive(value: string): number | null {
		const trimmed = value.trim();
		if (trimmed.length === 0) return null;
		const parsed = Number(trimmed);
		return Number.isFinite(parsed) && parsed > 0 ? parsed : null;
	}

	function isExpired(row: TerminalGrantRow): boolean {
		return Date.parse(row.expires_at) <= Date.now();
	}

	onMount(() => {
		void refresh();
		void (async () => {
			if (!(await hasHydratedScopeBearer())) {
				engineAvailability = [{ name: 'magician', installed: true }];
				return;
			}
			try {
				const roster = await fetchEngineAvailability();
				engineAvailability = roster.engines;
				const installed = roster.engines.filter((engine) => engine.installed && engine.name !== 'pi');
				if (!installed.some((engine) => engine.name === harnessEngine) && installed.length > 0) {
					harnessEngine = installed[0].name;
				}
			} catch {
				engineAvailability = [{ name: 'magician', installed: true }];
			}
		})();
	});
</script>

<section class="settings-card settings-card-wide" aria-labelledby="terminal-grants-title">
	<div class="settings-section-header">
		<div>
			<p class="settings-overline">Magician plane</p>
			<h2 id="terminal-grants-title">Terminal grants</h2>
		</div>
		<div class="engine-status">
			<button class="btn btn-secondary" type="button" onclick={refresh} disabled={loading}>
				{loading ? 'Refreshing…' : 'Refresh'}
			</button>
		</div>
	</div>

	<p class="muted">
		A <code>plt_</code> grant is how a terminal harness (Claude Code, Codex, Grok, agy) binds
		to Magician as an MCP server —
		<code>claude mcp add --transport http magician-plane
		http://127.0.0.1:8080/api/magician/v2/plane/mcp --header "Authorization: Bearer plt_…"</code>.
		The token is shown once at mint; Magician stores only its hash.
	</p>

	{#if lastMinted}
		<div class="mint-result" data-testid="terminal-grant-minted">
			<strong>{lastMinted.grant.label}</strong> — copy the token now, it will not be shown again:
			<code class="token">{lastMinted.token}</code>
			{#if lastMinted.dropped_tools.length > 0}
				<p class="muted">
					The allowlist floor dropped: {lastMinted.dropped_tools.join(', ')}
				</p>
			{/if}
		</div>
	{/if}

	<div class="grants-table" data-testid="terminal-grant-list">
		{#if grants.length === 0 && !loading}
			<p class="muted">No terminal grants yet.</p>
		{:else}
			<table>
				<thead>
					<tr>
						<th>Label</th>
						<th>Engine</th>
						<th>Workspace</th>
						<th>Agent</th>
						<th>Tools</th>
						<th>Expires</th>
						<th></th>
					</tr>
				</thead>
				<tbody>
					{#each grants as row (row.id)}
						<tr class:expired={isExpired(row)}>
							<td>{row.label}</td>
							<td><code>{row.harness_engine}</code></td>
							<td>{row.workspace}</td>
							<td>{row.agent_identity}</td>
							<td>{row.allowed_tools.length === 0 ? 'floored catalog' : row.allowed_tools.length}</td>
							<td>{new Date(row.expires_at).toLocaleString()}</td>
							<td>
								<button class="btn btn-danger" type="button" onclick={() => revoke(row)}>Revoke</button>
							</td>
						</tr>
					{/each}
				</tbody>
			</table>
		{/if}
	</div>

	<form
		class="mint-form"
		onsubmit={(event) => {
			event.preventDefault();
			mint();
		}}
	>
		<h3>Mint a grant</h3>
		<div class="form-grid">
			<label>
				Label
				<input type="text" bind:value={label} placeholder="laptop terminal" required />
			</label>
			<label>
				Workspace
				<input type="text" bind:value={workspace} required />
			</label>
			<label>
				Agent identity
				<input type="text" bind:value={agentIdentity} required />
			</label>
			<label>
				Harness engine
				<select bind:value={harnessEngine}>
					{#each installedEngines as engine (engine.name)}
						<option value={engine.name}>{engine.name}</option>
					{/each}
				</select>
			</label>
			<label title="Hours, 1–2160">
				TTL, hours
				<input type="number" min="1" max="2160" bind:value={ttlHours} required />
			</label>
			<label title="Comma-separated; empty means the floored tool catalog">
				Allowed tools
				<input type="text" bind:value={allowedTools} placeholder="read_file, search_memory" />
			</label>
			<label title="Optional cap on spend per run">
				Max USD / run
				<input type="number" min="0" step="0.01" bind:value={maxUsd} />
			</label>
			<label title="Optional wall-clock cap per run">
				Max wall-clock, h
				<input type="number" min="0" step="0.5" bind:value={maxWallClockHours} />
			</label>
			<label title="Optional; default 4">
				Max concurrent
				<input type="number" min="1" bind:value={maxConcurrentRuns} />
			</label>
		</div>
		<button class="btn btn-primary" type="submit" disabled={minting}>
			{minting ? 'Minting…' : 'Mint grant'}
		</button>
		<p class="muted">
			The engine is what a run started through this grant thinks with — `magician` is the
			deliberate forced pin; only engines this build can launch are listed.
		</p>
	</form>
</section>

<style>
	.settings-card {
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		color: var(--text-primary);
		display: flex;
		flex-direction: column;
		gap: 1rem;
		padding: 1rem;
	}
	.settings-section-header {
		align-items: flex-start;
		display: flex;
		gap: 1rem;
		justify-content: space-between;
	}
	.settings-overline,
	h2,
	h3,
	p {
		margin: 0;
	}
	.settings-overline {
		color: var(--text-secondary);
		font-size: 0.75rem;
		font-weight: 700;
		text-transform: uppercase;
	}
	h2 {
		color: var(--text-primary);
		font-size: 1.05rem;
		font-weight: 600;
		line-height: 1.25;
	}
	h3 {
		color: var(--text-primary);
		font-size: 0.95rem;
		font-weight: 600;
	}
	code {
		color: var(--text-primary);
		font-size: 0.78rem;
	}
	.mint-result {
		background: color-mix(in srgb, var(--accent-primary) 10%, var(--bg-card));
		border: 1px solid color-mix(in srgb, var(--accent-primary) 28%, var(--border-soft));
		border-radius: 8px;
		color: var(--text-primary);
		display: flex;
		flex-direction: column;
		gap: 0.35rem;
		margin: 0;
		padding: 0.75rem 0.85rem;
	}
	.mint-result .token {
		background: var(--input-bg, var(--bg-soft));
		border-radius: 6px;
		overflow-wrap: anywhere;
		padding: 0.4rem 0.5rem;
		user-select: all;
	}
	.grants-table {
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		overflow: auto;
	}
	.grants-table table {
		border-collapse: collapse;
		width: 100%;
	}
	.grants-table th,
	.grants-table td {
		border-bottom: 1px solid var(--border-soft);
		color: var(--text-primary);
		padding: 0.5rem 0.65rem;
		text-align: left;
	}
	.grants-table th {
		background: color-mix(in srgb, var(--bg-card) 88%, var(--bg-soft) 12%);
		color: var(--text-secondary);
		font-size: 0.74rem;
		font-weight: 700;
	}
	.grants-table tr:last-child td {
		border-bottom: 0;
	}
	tr.expired td {
		opacity: 0.55;
		text-decoration: line-through;
	}
	.form-grid {
		display: grid;
		gap: 0.6rem 0.75rem;
		grid-template-columns: repeat(auto-fit, minmax(190px, 1fr));
		margin: 0.75rem 0;
	}
	.mint-form label {
		color: var(--text-primary);
		display: flex;
		flex-direction: column;
		font-size: 0.82rem;
		font-weight: 700;
		gap: 0.3rem;
	}
	.mint-form input,
	.mint-form select {
		background: var(--input-bg, var(--bg-soft));
		border: 1px solid var(--input-border, var(--border-soft));
		border-radius: 6px;
		color: var(--text-primary);
		font: inherit;
		font-weight: 500;
		margin-top: auto;
		min-height: 2.25rem;
		padding: 0.45rem 0.65rem;
		width: 100%;
	}
	.mint-form input:focus,
	.mint-form select:focus {
		background: var(--input-focus-bg, var(--bg-elevated));
		border-color: var(--input-focus-border, var(--accent-primary));
		box-shadow: var(--input-focus-shadow, 0 0 0 3px color-mix(in srgb, var(--accent-primary) 18%, transparent));
		outline: none;
	}
	.mint-form {
		background: color-mix(in srgb, var(--bg-card) 88%, var(--bg-soft) 12%);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		padding: 0.85rem;
	}
	.mint-form h3 {
		margin: 0 0 0.35rem;
	}
	.muted {
		color: var(--text-secondary);
		font-size: 0.85rem;
		line-height: 1.45;
	}
	.engine-status {
		align-items: center;
		display: flex;
		flex-wrap: wrap;
		gap: 0.65rem;
		justify-content: flex-end;
	}
	.btn {
		align-items: center;
		border: 1px solid transparent;
		border-radius: 6px;
		cursor: pointer;
		display: inline-flex;
		font: inherit;
		font-size: 0.86rem;
		font-weight: 600;
		justify-content: center;
		line-height: 1.2;
		min-height: 2.25rem;
		padding: 0.55rem 0.8rem;
		white-space: nowrap;
	}
	.btn-primary {
		background: var(--accent-primary);
		border-color: var(--accent-primary);
		color: var(--text-on-accent, var(--accent-on-primary, #fff));
	}
	.btn-secondary {
		background: var(--button-secondary-bg, var(--bg-soft));
		border-color: var(--button-secondary-border, var(--border-soft));
		color: var(--button-secondary-color, var(--text-primary));
	}
	.btn-danger {
		background: transparent;
		border-color: color-mix(in srgb, var(--color-error) 35%, var(--border-soft));
		color: var(--color-error);
	}
	.btn:disabled {
		cursor: not-allowed;
		opacity: 0.58;
	}
	@media (max-width: 980px) {
		.settings-section-header {
			flex-direction: column;
		}
		.engine-status {
			justify-content: flex-start;
		}
	}
</style>
