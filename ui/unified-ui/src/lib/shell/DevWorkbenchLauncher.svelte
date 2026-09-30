<script lang="ts">
	import { createEventDispatcher, onMount } from 'svelte';
	import { agentList, loadAgents, type AgentSummary } from '$lib/stores/agentStore';
	import { timedFetch } from '$lib/shared/fetch';
	import {
		normalizeInteractiveSessionSummary,
		scopedInteractiveSessionParams,
		type InteractiveSessionList,
		type LiveInteractiveSession
	} from '$lib/shell/interactiveSessionApi';

	type CliOption = {
		id: string;
		program: string;
		label: string;
		detail: string;
		agents: string[];
	};

	type CliRuntimeCatalogResponse = {
		runtimes?: Array<{
			program?: string;
			label?: string;
			detail?: string;
		}>;
		error?: string;
	};

	type StartSessionResponse = {
		session_id?: string;
		program?: string;
		error?: string;
	};

	type DirectoryEntry = {
		name: string;
		path: string;
	};

	type DirectoryListResponse = {
		path: string;
		parent?: string | null;
		home?: string | null;
		root: string;
		current_dir: string;
		entries: DirectoryEntry[];
		truncated?: boolean;
		error?: string;
	};

	const dispatch = createEventDispatcher<{
		started: { sessionId: string; program: string | null };
		closed: { sessionId: string };
		focus: void;
	}>();

	export let threadId: string | null = null;

	let selectedProgram = '';
	let configuredCliOptions: CliOption[] = [];
	let cliOptionsLoading = false;
	let cliOptionsError: string | null = null;
	let workingDir = '';
	let startingProgram: string | null = null;
	let launcherError: string | null = null;
	let pickerOpen = false;
	let pickerLoading = false;
	let pickerError: string | null = null;
	let pickerPath = '';
	let pickerParent: string | null = null;
	let pickerHome: string | null = null;
	let pickerRoot = '/';
	let pickerCurrentDir = '';
	let pickerEntries: DirectoryEntry[] = [];
	let pickerTruncated = false;
	let liveSessionsOpen = false;
	let liveSessionsLoading = false;
	let liveSessionsError: string | null = null;
	let liveSessions: LiveInteractiveSession[] = [];
	let closingSessionIds = new Set<string>();

	$: cliOptions = buildCliOptions($agentList, configuredCliOptions);
	$: if (cliOptions.length > 0 && !cliOptions.some((option) => option.program === selectedProgram)) {
		selectedProgram = cliOptions[0].program;
	}
	$: if (cliOptions.length === 0 && selectedProgram) {
		selectedProgram = '';
	}

	onMount(() => {
		void loadAgents({ limit: 250 });
		void loadCliRuntimes();
	});

	function cliProgramFromTool(toolName: string): string | null {
		const normalized = toolName.trim().toLowerCase();
		const match = normalized.match(/^code_generate_([a-z0-9_]+)_cli$/);
		if (!match) return null;
		return match[1].replace(/_/g, '-');
	}

	function buildCliOptions(agents: AgentSummary[], configuredOptions: CliOption[]): CliOption[] {
		const byProgram = new Map<string, CliOption>();
		for (const option of configuredOptions) {
			const program = option.program.trim();
			if (!program) continue;
			byProgram.set(program.toLowerCase(), {
				...option,
				id: `configured:${program}`,
				program,
				label: option.label || program,
				detail: option.detail || 'Configured CLI runtime',
				agents: [...option.agents]
			});
		}
		for (const agent of agents) {
			if (agent.disabled) continue;
			for (const tool of agent.tools ?? []) {
				const program = cliProgramFromTool(tool);
				if (!program) continue;
				const agentLabel = agent.name?.trim() || agent.agent_id;
				const existing = byProgram.get(program.toLowerCase());
				if (existing) {
					if (!existing.agents.includes(agentLabel)) {
						existing.agents.push(agentLabel);
					}
				}
			}
		}
		return Array.from(byProgram.values())
			.map((option) => ({
				...option,
				detail:
					option.agents.length > 0
						? `${option.detail}; agent tools: ${option.agents.join(', ')}`
						: option.detail
			}))
			.sort((a, b) => a.label.localeCompare(b.label));
	}

	async function loadCliRuntimes(): Promise<void> {
		cliOptionsLoading = true;
		cliOptionsError = null;
		try {
			const response = await timedFetch(
				`/api/magician/v2/interactive-sessions/cli-runtimes?${scopedInteractiveSessionParams().toString()}`
			);
			const payload = (await response.json().catch(() => null)) as CliRuntimeCatalogResponse | null;
			if (!response.ok || !payload) {
				throw new Error(payload?.error || `server returned ${response.status}`);
			}
			configuredCliOptions = (payload.runtimes ?? [])
				.map((runtime): CliOption | null => {
					const program = runtime.program?.trim();
					if (!program) return null;
					return {
						id: `configured:${program}`,
						program,
						label: runtime.label?.trim() || program,
						detail: runtime.detail?.trim() || 'Configured CLI runtime',
						agents: []
					};
				})
				.filter((option): option is CliOption => !!option);
		} catch (error) {
			cliOptionsError = error instanceof Error ? error.message : 'Failed to load CLI runtimes';
			configuredCliOptions = [];
		} finally {
			cliOptionsLoading = false;
		}
	}

	async function openDirectoryPicker(): Promise<void> {
		liveSessionsOpen = false;
		pickerOpen = true;
		await loadDirectory(workingDir.trim() || undefined);
	}

	async function loadDirectory(path?: string): Promise<void> {
		pickerLoading = true;
		pickerError = null;
		try {
			const params = scopedInteractiveSessionParams();
			if (path?.trim()) params.set('path', path.trim());
			const response = await timedFetch(
				`/api/magician/v2/filesystem/directories?${params.toString()}`
			);
			const payload = (await response.json().catch(() => null)) as DirectoryListResponse | null;
			if (!response.ok || !payload) {
				throw new Error(payload?.error || `server returned ${response.status}`);
			}
			pickerPath = payload.path;
			pickerParent = payload.parent ?? null;
			pickerHome = payload.home ?? null;
			pickerRoot = payload.root || '/';
			pickerCurrentDir = payload.current_dir || '';
			pickerEntries = payload.entries ?? [];
			pickerTruncated = !!payload.truncated;
		} catch (error) {
			pickerError = error instanceof Error ? error.message : 'Failed to list directories';
		} finally {
			pickerLoading = false;
		}
	}

	function chooseDirectory(path: string): void {
		workingDir = path;
		pickerOpen = false;
	}

	async function startSelectedCli(): Promise<void> {
		const program = selectedProgram.trim();
		if (!program || startingProgram) return;
		startingProgram = program;
		launcherError = null;
		try {
			const body: Record<string, unknown> = {
				program,
				args: [],
				rows: 40,
				cols: 140
			};
			if (workingDir.trim()) body.working_dir = workingDir.trim();
			if (threadId?.trim()) body.ui_thread_id = threadId.trim();
			const response = await timedFetch(
				`/api/magician/v2/interactive-sessions?${scopedInteractiveSessionParams().toString()}`,
				{
					method: 'POST',
					headers: { 'Content-Type': 'application/json' },
					body: JSON.stringify(body)
				}
			);
			const payload = (await response.json().catch(() => null)) as StartSessionResponse | null;
			if (!response.ok || !payload?.session_id) {
				throw new Error(payload?.error || `server returned ${response.status}`);
			}
			dispatch('started', {
				sessionId: payload.session_id,
				program: payload.program ?? program
			});
			dispatch('focus');
		} catch (error) {
			launcherError = error instanceof Error ? error.message : 'Failed to start CLI session';
		} finally {
			startingProgram = null;
		}
	}

	async function toggleLiveSessions(): Promise<void> {
		liveSessionsOpen = !liveSessionsOpen;
		if (liveSessionsOpen) {
			pickerOpen = false;
			await refreshLiveSessions();
		}
	}

	async function refreshLiveSessions(): Promise<void> {
		liveSessionsLoading = true;
		liveSessionsError = null;
		try {
			const response = await timedFetch(
				`/api/magician/v2/interactive-sessions?${scopedInteractiveSessionParams().toString()}`
			);
			const payload = (await response.json().catch(() => null)) as InteractiveSessionList | null;
			if (!response.ok || !payload) {
				throw new Error((payload as { error?: string } | null)?.error || `server returned ${response.status}`);
			}
			liveSessions = (payload.sessions ?? [])
				.map((session) => normalizeInteractiveSessionSummary(session))
				.sort((a, b) => b.createdAtMs - a.createdAtMs);
		} catch (error) {
			liveSessionsError = error instanceof Error ? error.message : 'Failed to load sessions';
		} finally {
			liveSessionsLoading = false;
		}
	}

	async function closeLiveSession(sessionId: string): Promise<void> {
		if (closingSessionIds.has(sessionId)) return;
		closingSessionIds = new Set([...closingSessionIds, sessionId]);
		try {
			const response = await timedFetch(
				`/api/magician/v2/interactive-sessions/${encodeURIComponent(sessionId)}?${scopedInteractiveSessionParams().toString()}`,
				{ method: 'DELETE' }
			);
			if (!response.ok) {
				throw new Error(`server returned ${response.status}`);
			}
			liveSessions = liveSessions.filter((session) => session.id !== sessionId);
			dispatch('closed', { sessionId });
			window.dispatchEvent(
				new CustomEvent('magician:interactive-session-closed', { detail: { sessionId } })
			);
		} catch (error) {
			liveSessionsError = error instanceof Error ? error.message : 'Failed to close session';
		} finally {
			closingSessionIds.delete(sessionId);
			closingSessionIds = new Set(closingSessionIds);
		}
	}

	function formatAge(ms: number): string {
		if (!Number.isFinite(ms) || ms <= 0) return 'now';
		const minutes = Math.floor(ms / 60_000);
		if (minutes < 1) return '<1m';
		if (minutes < 60) return `${minutes}m`;
		const hours = Math.floor(minutes / 60);
		if (hours < 24) return `${hours}h ${minutes % 60}m`;
		const days = Math.floor(hours / 24);
		return `${days}d ${hours % 24}h`;
	}
</script>

<div class="dev-launcher" aria-label="Developer mode CLI controls">
	<div class="dev-launcher__controls">
		<label class="dev-launcher__field">
			<span>CLI</span>
			<select
				bind:value={selectedProgram}
				disabled={cliOptions.length === 0 || !!startingProgram}
				aria-label="CLI"
			>
				{#each cliOptions as option (option.id)}
					<option value={option.program}>{option.label}</option>
				{/each}
			</select>
		</label>
		<label class="dev-launcher__field dev-launcher__field--wide">
			<span>cwd</span>
			<input
				type="text"
				bind:value={workingDir}
				placeholder="CWD: default backend working directory"
				disabled={!!startingProgram}
				aria-label="Working directory"
			/>
		</label>
		<button
			type="button"
			class="dev-launcher__button"
			on:click={() => void openDirectoryPicker()}
			title="Browse directories on the backend server/container"
			aria-label="Browse backend server working directory"
		>
			Browse
		</button>
		<button
			type="button"
			class="dev-launcher__button dev-launcher__button--primary"
			disabled={!selectedProgram || !!startingProgram}
			on:click={() => void startSelectedCli()}
		>
			{startingProgram ? 'Starting' : 'Start'}
		</button>
		<button
			type="button"
			class="dev-launcher__button"
			on:click={() => void toggleLiveSessions()}
			aria-expanded={liveSessionsOpen}
			title="Show all live PTY sessions in this workspace"
		>
			Sessions
		</button>
	</div>
	<div class="dev-launcher__status" aria-live="polite">
		{#if launcherError}
			<span class="dev-launcher__error">{launcherError}</span>
		{:else if cliOptions.length > 0}
			{@const selectedCli = cliOptions.find((option) => option.program === selectedProgram)}
			{#if selectedCli}
				<span>{selectedCli.detail}</span>
			{/if}
		{:else if cliOptionsLoading}
			<span>Loading CLI runtimes...</span>
		{:else if cliOptionsError}
			<span class="dev-launcher__error">CLI runtime catalog unavailable: {cliOptionsError}</span>
		{:else}
			<span>No CLI runtimes configured.</span>
		{/if}
	</div>
	{#if pickerOpen}
		<div class="dev-launcher__picker" role="dialog" aria-label="Choose working directory">
			<div class="dev-launcher__picker-head">
				<span class="dev-launcher__picker-path" title={pickerPath || pickerCurrentDir}>
					{pickerPath || pickerCurrentDir || 'Server directories'}
				</span>
				<button
					type="button"
					class="dev-launcher__picker-close"
					on:click={() => pickerOpen = false}
					aria-label="Close directory picker"
				>×</button>
			</div>
			<div class="dev-launcher__picker-actions">
				<button type="button" on:click={() => void loadDirectory(pickerParent ?? pickerPath)} disabled={!pickerParent || pickerLoading}>Parent</button>
				<button type="button" on:click={() => void loadDirectory(pickerHome ?? undefined)} disabled={!pickerHome || pickerLoading}>Home</button>
				<button type="button" on:click={() => void loadDirectory(pickerRoot)} disabled={pickerLoading}>Root</button>
				<button type="button" on:click={() => void loadDirectory(pickerCurrentDir)} disabled={!pickerCurrentDir || pickerLoading}>Current</button>
				<button type="button" class="dev-launcher__picker-use" on:click={() => chooseDirectory(pickerPath)} disabled={!pickerPath}>Use this</button>
			</div>
			{#if pickerError}
				<div class="dev-launcher__picker-error">{pickerError}</div>
			{:else if pickerLoading}
				<div class="dev-launcher__picker-empty">Loading directories...</div>
			{:else if pickerEntries.length === 0}
				<div class="dev-launcher__picker-empty">No child directories.</div>
			{:else}
				<div class="dev-launcher__picker-list">
					{#each pickerEntries as entry (entry.path)}
						<button
							type="button"
							class="dev-launcher__picker-entry"
							on:click={() => void loadDirectory(entry.path)}
							on:dblclick={() => chooseDirectory(entry.path)}
							title={entry.path}
						>
							<span>{entry.name}</span>
						</button>
					{/each}
				</div>
				{#if pickerTruncated}
					<div class="dev-launcher__picker-empty">Showing first 240 directories.</div>
				{/if}
			{/if}
		</div>
	{/if}
	{#if liveSessionsOpen}
		<div class="dev-launcher__sessions" role="dialog" aria-label="Live developer CLI sessions">
			<div class="dev-launcher__picker-head">
				<span class="dev-launcher__picker-path">
					Live sessions
				</span>
				<button
					type="button"
					class="dev-launcher__picker-close"
					on:click={() => liveSessionsOpen = false}
					aria-label="Close live sessions"
				>×</button>
			</div>
			<div class="dev-launcher__picker-actions">
				<button type="button" on:click={() => void refreshLiveSessions()} disabled={liveSessionsLoading}>Refresh</button>
			</div>
			{#if liveSessionsError}
				<div class="dev-launcher__picker-error">{liveSessionsError}</div>
			{:else if liveSessionsLoading}
				<div class="dev-launcher__picker-empty">Loading sessions...</div>
			{:else if liveSessions.length === 0}
				<div class="dev-launcher__picker-empty">No live sessions.</div>
			{:else}
				<div class="dev-launcher__session-list">
					{#each liveSessions as session (session.id)}
						<div class="dev-launcher__session-row">
							<div class="dev-launcher__session-main">
								<span class="dev-launcher__session-program">{session.program ?? 'session'}</span>
								<span class="dev-launcher__session-thread">{session.uiThreadId ? `#${session.uiThreadId}` : 'unthreaded'}</span>
								<span class="dev-launcher__session-id">{session.id.slice(0, 8)}</span>
							</div>
							<div class="dev-launcher__session-meta">
								<span>{formatAge(Date.now() - session.createdAtMs)} old</span>
								<span>{session.workingDir ?? 'default cwd'}</span>
							</div>
							<button
								type="button"
								class="dev-launcher__session-close"
								disabled={closingSessionIds.has(session.id)}
								on:click={() => void closeLiveSession(session.id)}
							>
								Close
							</button>
						</div>
					{/each}
				</div>
			{/if}
		</div>
	{/if}
</div>

<style>
	.dev-launcher {
		display: block;
		position: relative;
		width: 100%;
	}

	.dev-launcher__controls {
		display: flex;
		align-items: center;
		gap: 6px;
		width: 100%;
		min-width: 0;
	}

	/* Phone width: a 5-control row (CLI + cwd + Browse + Start +
	   Sessions) overflows a 360px screen. Wrap to two rows with the
	   primary actions on top, the supporting controls below:

	     row 1: [CLI▾]                              [Start]
	     row 2: [cwd ──────────] [Browse] [Sessions]

	   Start uses `margin-left: auto` to push to the right edge of row
	   1, so visually it sits adjacent to the kebab menu rendered by
	   WorkbenchColumn outside the launcher. CLI select stays at 78px
	   flex-basis (down from desktop 108px) so the dropdown reads
	   clearly without dominating the row. */
	@media (max-width: 767px) {
		.dev-launcher__controls {
			flex-wrap: wrap;
			row-gap: 6px;
		}
		/* Row 1: CLI dropdown (left) + Start (pushed right via auto margin) */
		.dev-launcher__field {
			order: -1;
			flex: 0 0 78px;
			min-width: 0;
		}
		.dev-launcher__button--primary {
			order: -1;
			flex: 0 0 auto;
			margin-left: auto;
		}
		/* Row 2: cwd + Browse + Sessions (cwd grows to fill remaining space) */
		.dev-launcher__field--wide {
			order: 0;
			flex: 1 1 auto;
			min-width: 120px;
		}
		.dev-launcher__button {
			flex: 0 0 auto;
			padding: 0 7px;
		}
	}

	.dev-launcher__field {
		display: block;
		flex: 0 0 108px;
		min-width: 86px;
		color: var(--text-muted, #777);
		font: 11px var(--font-primary);
	}

	.dev-launcher__field--wide {
		flex: 1 1 auto;
		min-width: 180px;
	}

	.dev-launcher__field span {
		position: absolute;
		width: 1px;
		height: 1px;
		padding: 0;
		margin: -1px;
		overflow: hidden;
		clip: rect(0, 0, 0, 0);
		white-space: nowrap;
		border: 0;
	}

	.dev-launcher__field select,
	.dev-launcher__field input {
		width: 100%;
		height: 26px;
		min-height: 26px;
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.14));
		border-radius: 6px;
		background: var(--bg-base, #fff);
		color: var(--text-primary, #1a1a1a);
		font: 11px var(--font-mono, ui-monospace, monospace);
		padding: 3px 7px;
		outline: none;
	}

	.dev-launcher__field select:focus,
	.dev-launcher__field input:focus {
		border-color: var(--accent-primary, #c2502a);
		box-shadow: 0 0 0 2px color-mix(in srgb, var(--accent-primary, #c2502a) 18%, transparent);
	}

	.dev-launcher__button {
		height: 26px;
		min-height: 26px;
		white-space: nowrap;
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.14));
		border-radius: 6px;
		background: var(--bg-base, #fff);
		color: var(--text-primary, #1a1a1a);
		font: 600 11px var(--font-primary);
		padding: 0 9px;
		cursor: pointer;
	}

	.dev-launcher__button--primary {
		border-color: var(--accent-primary, #c2502a);
		background: var(--accent-primary, #c2502a);
		color: var(--accent-contrast, #fff);
	}

	.dev-launcher__button:disabled {
		cursor: not-allowed;
		opacity: 0.58;
	}

	.dev-launcher__status {
		position: absolute;
		width: 1px;
		height: 1px;
		padding: 0;
		margin: -1px;
		overflow: hidden;
		clip: rect(0, 0, 0, 0);
		white-space: nowrap;
		border: 0;
	}

	.dev-launcher__error {
		color: var(--danger, #b42318);
	}

	.dev-launcher__picker,
	.dev-launcher__sessions {
		position: absolute;
		left: 0;
		right: 0;
		bottom: calc(100% + 10px);
		z-index: 80;
		display: flex;
		flex-direction: column;
		max-height: min(360px, calc(100vh - 220px));
		overflow: hidden;
		border: 1px solid var(--border-default, rgba(0, 0, 0, 0.18));
		border-radius: var(--radius-lg, 14px);
		background: var(--bg-elevated, #fff);
		box-shadow: var(--shadow-lg, 0 24px 48px -14px rgba(0, 0, 0, 0.32));
		color: var(--text-primary, #1a1a1a);
	}

	.dev-launcher__sessions {
		max-height: min(420px, calc(100vh - 220px));
	}

	.dev-launcher__picker-head {
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 10px 12px 8px;
		border-bottom: 1px solid var(--border-soft, rgba(0, 0, 0, 0.08));
	}

	.dev-launcher__picker-path {
		min-width: 0;
		flex: 1 1 auto;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font: 11.5px var(--font-mono, ui-monospace, monospace);
		color: var(--text-secondary, #444);
	}

	.dev-launcher__picker-close {
		width: 24px;
		height: 24px;
		border: 0;
		border-radius: 6px;
		background: transparent;
		color: var(--text-muted, #777);
		cursor: pointer;
		font-size: 16px;
		line-height: 1;
	}

	.dev-launcher__picker-close:hover {
		background: var(--bg-soft, rgba(0, 0, 0, 0.06));
		color: var(--text-primary, #1a1a1a);
	}

	.dev-launcher__picker-actions {
		display: flex;
		align-items: center;
		gap: 6px;
		padding: 8px 10px;
		border-bottom: 1px solid var(--border-soft, rgba(0, 0, 0, 0.06));
		overflow-x: auto;
	}

	.dev-launcher__picker-actions button {
		height: 24px;
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.14));
		border-radius: 6px;
		background: var(--bg-base, #fff);
		color: var(--text-secondary, #444);
		font: 600 10.5px var(--font-primary);
		padding: 0 8px;
		white-space: nowrap;
		cursor: pointer;
	}

	.dev-launcher__picker-actions button:hover:not(:disabled),
	.dev-launcher__picker-entry:hover {
		background: var(--bg-soft, rgba(0, 0, 0, 0.06));
	}

	.dev-launcher__picker-actions button:disabled {
		cursor: not-allowed;
		opacity: 0.5;
	}

	.dev-launcher__picker-actions .dev-launcher__picker-use {
		margin-left: auto;
		border-color: var(--accent-primary, #c2502a);
		background: var(--accent-primary, #c2502a);
		color: var(--accent-contrast, #fff);
	}

	.dev-launcher__picker-list {
		overflow: auto;
		padding: 6px;
	}

	.dev-launcher__picker-entry {
		display: flex;
		width: 100%;
		align-items: center;
		min-height: 28px;
		border: 0;
		border-radius: 7px;
		background: transparent;
		color: var(--text-primary, #1a1a1a);
		text-align: left;
		cursor: pointer;
		font: 12px var(--font-mono, ui-monospace, monospace);
		padding: 0 8px;
	}

	.dev-launcher__picker-entry span {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.dev-launcher__picker-empty,
	.dev-launcher__picker-error {
		padding: 18px 14px;
		font: 12px var(--font-primary);
		color: var(--text-muted, #777);
	}

	.dev-launcher__picker-error {
		color: var(--danger, #b42318);
	}

	.dev-launcher__session-list {
		display: grid;
		gap: 6px;
		overflow: auto;
		padding: 8px;
	}

	.dev-launcher__session-row {
		display: grid;
		grid-template-columns: minmax(0, 1fr) auto;
		gap: 5px 10px;
		align-items: center;
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.1));
		border-radius: 8px;
		background: var(--bg-base, #fff);
		padding: 8px 8px 8px 10px;
	}

	.dev-launcher__session-main,
	.dev-launcher__session-meta {
		min-width: 0;
		display: flex;
		align-items: center;
		gap: 7px;
	}

	.dev-launcher__session-main {
		grid-column: 1;
	}

	.dev-launcher__session-meta {
		grid-column: 1;
		color: var(--text-muted, #777);
		font: 10.5px var(--font-mono, ui-monospace, monospace);
	}

	.dev-launcher__session-meta span {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.dev-launcher__session-program {
		color: var(--text-primary, #1a1a1a);
		font: 700 12px var(--font-mono, ui-monospace, monospace);
	}

	.dev-launcher__session-thread,
	.dev-launcher__session-id {
		color: var(--text-muted, #777);
		font: 10.5px var(--font-mono, ui-monospace, monospace);
	}

	.dev-launcher__session-close {
		grid-column: 2;
		grid-row: 1 / span 2;
		height: 25px;
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.14));
		border-radius: 6px;
		background: transparent;
		color: var(--text-secondary, #444);
		font: 600 10.5px var(--font-primary);
		padding: 0 8px;
		cursor: pointer;
	}

	.dev-launcher__session-close:hover:not(:disabled) {
		border-color: var(--danger, #b42318);
		color: var(--danger, #b42318);
	}

	.dev-launcher__session-close:disabled {
		cursor: not-allowed;
		opacity: 0.55;
	}

	@media (max-width: 700px) {
		.dev-launcher__controls {
			align-items: stretch;
			flex-wrap: wrap;
		}

		.dev-launcher__field,
		.dev-launcher__field--wide {
			flex: 1 1 160px;
		}
	}
</style>
