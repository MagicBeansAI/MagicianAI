<script lang="ts">
	import { onMount } from 'svelte';
	import DecisionRoutingPanel from './DecisionRoutingPanel.svelte';
	import { showError, showSuccess } from '$lib/shared/stores/notifications';
	import { hasHydratedScopeBearer } from '$lib/stores/scopeIdentityStore';
	import { chatProfileStore } from '$lib/stores/chatProfileStore';
	import { chatHarnessPreferenceStore } from '$lib/stores/chatHarnessPreferenceStore';
	import {
		fetchEngineAvailability,
		setDecisionMode,
		type DecisionMode,
		fetchChatProfileChoices,
		setChatHarnessEngine,
		setRunHarnessEngine,
		type EngineAvailability,
		type ChatProfileChoice
	} from '$lib/plane/terminalGrants';

	let decisionMode: DecisionMode | '' = '';
	let selectedDecisionMode: DecisionMode | '' = '';
	let savingDecisionMode = false;
	async function saveDecisionMode() {
		if (!selectedDecisionMode) return;
		savingDecisionMode = true;
		try {
			decisionMode = await setDecisionMode(selectedDecisionMode);
			selectedDecisionMode = decisionMode;
			showSuccess('Decision Engine setting saved for all clients.');
		} catch (error) {
			showError(error instanceof Error ? error.message : 'The Decision Engine setting could not be saved.');
		} finally {
			savingDecisionMode = false;
		}
	}

	let engineAvailability: EngineAvailability[] = [{ name: 'magician', installed: true }];
	let currentEngine = 'magician';
	let chatEngine = 'magician';
	let selectedChatEngine = 'magician';
	let chatModel = 'default';
	let selectedChatModel = 'default';
	let selectedChatProfile = '';
	let chatProfileCurrent = '';
	let selectedRunEngine = 'magician';
	let selectedRunModel = 'default';
	let chatProfiles: ChatProfileChoice[] = [];
	let selectedPiProfile = '';
	let piProfileCurrent = '';
	let runModelCurrent: string | undefined = undefined;
	let savingRunEngine = false;
	// The server's chat engine: what clients with no pick of their own use
	// (bots, push-to-talk, devices), and what every open composer adopts when
	// it changes.
	let serverChatEngine = 'magician';
	let serverChatModel = 'default';
	let savingChatDefault = false;
	$: modelsFor = (engine: string): string[] =>
		engineAvailability.find((e) => e.name === engine)?.models ?? ['default'];
	$: {
		chatEngine = $chatHarnessPreferenceStore.engine;
		chatModel = $chatHarnessPreferenceStore.model;
		selectedChatEngine = chatEngine;
		selectedChatModel = chatModel;
	}
	$: {
		chatProfileCurrent = $chatProfileStore.selected ?? '';
		selectedChatProfile = chatProfileCurrent;
	}

	function nativeToolPostureLabel(engine: EngineAvailability): string {
		if (engine.name === 'magician') return '';
		switch (engine.native_tool_posture) {
			case 'stripped':
				return ' (native tools stripped)';
			case 'denylisted':
				return ' (native tools denylisted)';
			case 'sandboxed':
				return ' (native tools sandboxed)';
			case 'live':
				return ' (native tools live)';
			default:
				return '';
		}
	}

	function engineLabel(engine: string, native: string): string {
		return engine === 'magician' ? native : engine;
	}

	async function saveRunEngine() {
		const selected = engineAvailability.find((engine) => engine.name === selectedRunEngine);
		if (selectedRunEngine !== 'magician' && (!selected || !selected.installed)) {
			showError('That engine is not installed on this machine.');
			return;
		}
		savingRunEngine = true;
		try {
			const result = await setRunHarnessEngine(selectedRunEngine, selectedRunModel, selectedPiProfile || null);
			currentEngine = result.engine;
			selectedRunEngine = result.engine;
			selectedRunModel = result.harness_model;
			runModelCurrent = result.harness_model;
			selectedPiProfile = result.pi_profile ?? '';
			piProfileCurrent = selectedPiProfile;
			showSuccess(
				`Background runs that start from now on think with ${engineLabel(currentEngine, 'Magician (own loop)')}. Runs already going keep their engine.`
			);
		} catch (error) {
			showError(error instanceof Error ? error.message : 'The background run engine could not be updated.');
		} finally {
			savingRunEngine = false;
		}
	}

	function saveChatEngine() {
		if (!selectedChatEngine.trim()) {
			showError('Choose who thinks a chat turn.');
			return;
		}
		const selected = engineAvailability.find((engine) => engine.name === selectedChatEngine);
		if (selectedChatEngine !== 'magician' && (!selected || !selected.installed)) {
			showError('That engine is not installed on this machine.');
			return;
		}
		chatHarnessPreferenceStore.select(selectedChatEngine, selectedChatModel);
		if ((selectedChatEngine === 'magician' || selectedChatEngine === 'pi') && selectedChatProfile) {
			chatProfileStore.select(selectedChatProfile);
		}
		showSuccess(`Chat preference saved in this browser: ${selectedChatEngine}.`);
	}

	async function saveChatDefault() {
		const selected = engineAvailability.find((engine) => engine.name === selectedChatEngine);
		if (selectedChatEngine !== 'magician' && (!selected || !selected.installed)) {
			showError('That engine is not installed on this machine.');
			return;
		}
		savingChatDefault = true;
		try {
			const result = await setChatHarnessEngine(selectedChatEngine, selectedChatModel);
			serverChatEngine = result.chat_current;
			serverChatModel = result.chat_model ?? 'default';
			chatHarnessPreferenceStore.adoptServer(serverChatEngine, serverChatModel);
			if ((serverChatEngine === 'magician' || serverChatEngine === 'pi') && selectedChatProfile) {
				chatProfileStore.select(selectedChatProfile);
			}
			showSuccess(
				`Every client now defaults to ${engineLabel(serverChatEngine, 'Magician (own mouth)')} for chat; open composers switch to it.`
			);
		} catch (error) {
			showError(error instanceof Error ? error.message : 'The chat default could not be updated.');
		} finally {
			savingChatDefault = false;
		}
	}

	onMount(() => {
		void (async () => {
			if (!(await hasHydratedScopeBearer())) {
				engineAvailability = [{ name: 'magician', installed: true }];
				return;
			}
			void chatProfileStore.load();
			try {
				const roster = await fetchEngineAvailability();
				engineAvailability = roster.engines;
				decisionMode = roster.decision_mode ?? '';
				selectedDecisionMode = decisionMode;
				chatHarnessPreferenceStore.reconcileWithRoster(roster);
				serverChatEngine = roster.chat_current;
				serverChatModel = roster.chat_model ?? 'default';
				currentEngine = roster.current;
				selectedRunEngine = roster.current;
				selectedRunModel = roster.run_model ?? 'default';
				runModelCurrent = roster.run_model;
				selectedPiProfile = roster.run_pi_profile ?? '';
				piProfileCurrent = selectedPiProfile;
				try {
					chatProfiles = await fetchChatProfileChoices();
				} catch (error) {
					showError(error instanceof Error ? error.message : 'Pi profiles are unavailable.');
				}
			} catch {
				engineAvailability = [{ name: 'magician', installed: true }];
			}
		})();
	});
</script>

<section class="settings-card settings-card-wide" aria-labelledby="engines-title">
	<div class="settings-section-header">
		<div>
			<p class="settings-overline">Who thinks</p>
			<h2 id="engines-title">Engines</h2>
		</div>
		<div class="engine-status">
			<span class="engine-chip" data-testid="chat-current-engine">
				Chat thinks with: <strong>{engineLabel(chatEngine, 'Magician (own mouth)')}</strong>
			</span>
			<span class="engine-chip" data-testid="current-engine">
				Background runs think with: <strong>{engineLabel(currentEngine, 'Magician (own loop)')}</strong>
			</span>
		</div>
	</div>

	<p class="muted">
		Work inherits the engine of whatever started it. A run launched from a chat thinks with the
		chat's engine; delegations, tasks, and coding tasks a run starts think with the run's engine.
		A run keeps the engine it started with, so saving here only affects work that starts
		afterwards. An engine named explicitly (a terminal grant, an agent's coding profile, the
		composer's pick) always wins.
	</p>

	<form class="engine-form" onsubmit={(event) => { event.preventDefault(); void saveDecisionMode(); }}>
		<h3>Decision Engine</h3>
		<p class="muted">
			Choose which chat and agentic engines use Jev, Kev and Laya through the Decision Engine.
			This setting is shared by every device connected to this Magician server.
			Changes apply to subsequent decisions; a chat harness turn already running finishes with its current setting.
		</p>
		<label>
			Use Decision Engine
			<select bind:value={selectedDecisionMode} disabled={!decisionMode || savingDecisionMode}>
				{#if !decisionMode}<option value="">Unavailable</option>{/if}
				<option value="all_engines">All Engines</option>
				<option value="magician_only">Magician Only</option>
				<option value="off">Off</option>
			</select>
		</label>
		<p class="muted">
			All Engines includes Magician, Pi, Claude Code, Codex, Codex App Server, Grok and Agy.
			Magician Only bypasses the Decision Engine for other harnesses. Off bypasses it for all engines.
		</p>
		<button class="btn btn-primary" type="submit" disabled={!decisionMode || savingDecisionMode || selectedDecisionMode === decisionMode}>
			{savingDecisionMode ? 'Saving…' : 'Save Decision Engine setting'}
		</button>
	</form>

	<DecisionRoutingPanel />

	<form
		class="chat-engine-form"
		data-testid="chat-engine-picker"
		onsubmit={(event) => {
			event.preventDefault();
			saveChatEngine();
		}}
	>
		<h3>Chat</h3>
		<p class="muted">
			Who thinks a chat turn, and so every run the chat launches. Magician is today's LLM
			mouth. Every installed roster engine can speak through the plane. Native-tool strip is per
			engine (stripped, denylisted, or sandboxed when the CLI has no empty-registry flag).
			One-shot engines reply as one chunk. Save keeps the choice in this browser, shared with
			its composer and its voice calls. The default for all clients is what clients without a
			pick of their own use; setting it also switches every open composer.
		</p>
		<p class="muted" data-testid="chat-server-default">
			Default for all clients: <strong>{engineLabel(serverChatEngine, 'Magician (own mouth)')}</strong>{serverChatModel !== 'default' ? ` / ${serverChatModel}` : ''}
		</p>
		<div class="form-grid">
			<label>
				Chat engine
				<select
					bind:value={selectedChatEngine}
					data-testid="chat-engine-select"
					onchange={() => (selectedChatModel = 'default')}
				>
					{#each engineAvailability as engine (engine.name)}
						<option value={engine.name} disabled={!engine.installed}>
							{engine.name}{engine.installed ? '' : ' (not installed)'}{nativeToolPostureLabel(
								engine
							)}
						</option>
					{/each}
				</select>
			</label>
			{#if selectedChatEngine === 'magician' || selectedChatEngine === 'pi'}
				<label>
					Chat profile
					<select bind:value={selectedChatProfile} data-testid="chat-settings-profile-select">
						{#if !selectedChatProfile}<option value="">Choose a profile</option>{/if}
						{#if selectedChatProfile && !chatProfiles.some((profile) => profile.name === selectedChatProfile)}
							<option value={selectedChatProfile}>{selectedChatProfile} (unavailable)</option>
						{/if}
						{#each chatProfiles as profile (profile.name)}
							<option value={profile.name}>{profile.name} — {profile.provider} / {profile.model}{selectedChatEngine === 'pi' && profile.is_adaptive ? ' (fast model)' : ''}</option>
						{/each}
					</select>
				</label>
			{:else}
				<label>
					Chat model
					<select bind:value={selectedChatModel}>
						{#each modelsFor(selectedChatEngine) as model (model)}
							<option value={model}>{model === 'default' ? 'CLI default' : model}</option>
						{/each}
					</select>
				</label>
			{/if}
		</div>
		<button
			class="btn btn-primary"
			type="submit"
			disabled={selectedChatEngine === chatEngine && selectedChatModel === chatModel && selectedChatProfile === chatProfileCurrent}
		>
			Save chat preference
		</button>
		<button
			class="btn btn-secondary"
			type="button"
			data-testid="chat-default-for-all"
			onclick={saveChatDefault}
			disabled={savingChatDefault || (selectedChatEngine === serverChatEngine && selectedChatModel === serverChatModel)}
		>
			{savingChatDefault ? 'Saving…' : 'Make this the default for all clients'}
		</button>
	</form>

	<form
		class="engine-form"
		data-testid="run-engine-picker"
		onsubmit={(event) => {
			event.preventDefault();
			saveRunEngine();
		}}
		aria-label="Background run engine selection"
	>
		<h3>Background runs</h3>
		<p class="muted">
			The default for runs nothing launched: schedules, monitors, autonomous agents, and API
			starts. It drives each of their loop iterations, and the delegations, tasks, and coding
			tasks they start inherit it. A Pi profile controls Pi turns.
		</p>
		<div class="form-grid">
			<label>
				Background run engine
				<select bind:value={selectedRunEngine} data-testid="run-engine-select" onchange={() => (selectedRunModel = 'default')}>
					{#each engineAvailability as engine (engine.name)}
						<option value={engine.name} disabled={!engine.installed}>
							{engine.name}{engine.installed ? '' : ' (not installed)'}
						</option>
					{/each}
				</select>
			</label>
			{#if selectedRunEngine === 'pi'}
				<label>
					Pi profile
					<select bind:value={selectedPiProfile} data-testid="run-pi-profile-select">
						<option value="">Pi own credentials and model</option>
						{#if selectedPiProfile && !chatProfiles.some((profile) => profile.name === selectedPiProfile)}
							<option value={selectedPiProfile}>{selectedPiProfile} (unavailable)</option>
						{/if}
						{#each chatProfiles as profile (profile.name)}
							<option value={profile.name}>{profile.name} — {profile.provider} / {profile.model}{profile.is_adaptive ? ' (fast model)' : ''}</option>
						{/each}
					</select>
				</label>
			{:else}
				<label>
					Background run model
					<select bind:value={selectedRunModel} disabled={selectedRunEngine === 'magician'}>
						{#each modelsFor(selectedRunEngine) as model (model)}
							<option value={model}>{model === 'default' ? 'CLI default' : model}</option>
						{/each}
					</select>
				</label>
			{/if}
		</div>
		<button
			class="btn btn-primary"
			type="submit"
			disabled={savingRunEngine || (selectedRunEngine === currentEngine && selectedRunModel === (runModelCurrent ?? 'default') && selectedPiProfile === piProfileCurrent)}
		>
			{savingRunEngine ? 'Saving…' : 'Save background run engine'}
		</button>
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
	.form-grid {
		display: grid;
		gap: 0.6rem 0.75rem;
		grid-template-columns: repeat(auto-fit, minmax(190px, 1fr));
		margin: 0.75rem 0;
	}
	.chat-engine-form label,
	.engine-form label {
		color: var(--text-primary);
		display: flex;
		flex-direction: column;
		font-size: 0.82rem;
		font-weight: 700;
		gap: 0.3rem;
	}
	.chat-engine-form select,
	.engine-form select {
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
	.chat-engine-form select:focus,
	.engine-form select:focus {
		background: var(--input-focus-bg, var(--bg-elevated));
		border-color: var(--input-focus-border, var(--accent-primary));
		box-shadow: var(--input-focus-shadow, 0 0 0 3px color-mix(in srgb, var(--accent-primary) 18%, transparent));
		outline: none;
	}
	.chat-engine-form,
	.engine-form {
		background: color-mix(in srgb, var(--bg-card) 88%, var(--bg-soft) 12%);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		padding: 0.85rem;
	}
	.chat-engine-form h3,
	.engine-form h3 {
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
	.engine-chip {
		background: color-mix(in srgb, var(--accent-primary) 12%, var(--bg-card));
		border: 1px solid color-mix(in srgb, var(--accent-primary) 28%, var(--border-soft));
		border-radius: 999px;
		color: var(--text-primary);
		font-size: 0.78rem;
		padding: 0.28rem 0.7rem;
		white-space: nowrap;
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
	.btn-secondary {
		background: var(--button-secondary-bg, var(--bg-soft));
		border-color: var(--button-secondary-border, var(--border-soft));
		color: var(--button-secondary-color, var(--text-primary));
	}
	.btn-primary {
		background: var(--accent-primary);
		border-color: var(--accent-primary);
		color: var(--text-on-accent, var(--accent-on-primary, #fff));
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
