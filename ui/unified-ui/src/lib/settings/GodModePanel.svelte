<script lang="ts">
	import { onMount } from 'svelte';
	import { requestConfirmation } from '$lib/stores/confirmationStore';
	import { showError, showSuccess } from '$lib/shared/stores/notifications';
	import {
		describeSuperGodModeEnd,
		fetchGodMode,
		forgetGodModeGrants,
		saveGodMode,
		saveGodModeAutoApprove,
		saveSuperGodMode,
		superGodModePoll,
		type GodModeState,
		type SuperGodModeDuration
	} from '$lib/settings/godMode';

	let godState = $state<GodModeState | null>(null);
	const superState = superGodModePoll.value;
	let superBusy = $state(false);
	let busy = $state(false);
	let error = $state<string | null>(null);

	onMount(() => {
		void refresh();
	});

	async function refresh(): Promise<void> {
		try {
			godState = await fetchGodMode();
			error = null;
		} catch (caught) {
			error = caught instanceof Error ? caught.message : 'God mode settings are unavailable.';
		}
	}

	async function toggle(): Promise<void> {
		if (!godState) return;
		const turningOn = !godState.enabled;
		const confirmed = await requestConfirmation(
			turningOn
				? {
						title: 'Turn on god mode by default?',
						message:
							'Every chat starts in god mode, and so do runs no chat started (scheduled, triggers, background). Trusted agents, on Magician’s own model and on harness engines such as Claude Code and Codex, get unrestricted shell, files and network: any directory, package installs, cloning and running code. Anything the normal sandbox would refuse asks you first (allow once, for this session, or always). Commands run with your user’s full permissions and can read or change anything you can. Any chat can still turn it off from the Magician orb in the composer.',
						confirmLabel: 'Turn on god mode',
						destructive: true
					}
				: {
						title: 'Turn off god mode by default?',
						message: 'Chats and runs go back to the normal sandbox; a chat can still turn god mode on from the Magician orb in the composer. Every “this session” and “always” approval, and every chat’s own choice, is forgotten.',
						confirmLabel: 'Turn off'
					}
		);
		if (!confirmed) return;
		busy = true;
		try {
			godState = await saveGodMode(turningOn);
			showSuccess(turningOn ? 'God mode is on' : 'God mode is off');
		} catch (caught) {
			showError(caught instanceof Error ? caught.message : 'God mode could not be saved.');
		} finally {
			busy = false;
		}
	}

	async function toggleAuto(): Promise<void> {
		if (!godState) return;
		const turningOn = !godState.auto_approve;
		if (turningOn) {
			const confirmed = await requestConfirmation({
				title: 'Auto-approve god mode by default?',
				message:
					'Wherever god mode is on, what it would ask you is approved without asking: commands outside the sandbox, files outside the workspace, network and MCP tools. It still asks before anything that deletes (rm -r or -f, find -delete, git reset --hard, git clean, a forced push, a deleting edit), anything run as root (sudo, doas, su), code a command does not show (piped into a shell, from a variable), and the blocked list. Each chat can turn it on or off from the Magician orb. Approvals you already gave are kept.',
				confirmLabel: 'Turn on auto-approve',
				destructive: true
			});
			if (!confirmed) return;
		}
		busy = true;
		try {
			godState = await saveGodModeAutoApprove(turningOn);
			showSuccess(turningOn ? 'Auto-approve is on' : 'Auto-approve is off');
		} catch (caught) {
			showError(caught instanceof Error ? caught.message : 'Auto-approve could not be saved.');
		} finally {
			busy = false;
		}
	}

	async function setSuper(duration: SuperGodModeDuration | null): Promise<void> {
		if (duration) {
			const confirmed = await requestConfirmation({
				title: duration === '1h' ? 'Turn on super god mode for 1 hour?' : 'Turn on super god mode until you turn it off?',
				message:
					'For debugging. Before a harness starts, you are asked once (allow once, this session, or always). Approved, it starts with no restrictions. Chat and agent-run engines (Claude Code, Codex, Grok, Antigravity, the coding agent) get their own native tools, your settings and MCP servers beside Magician’s tools. VibeDev coding engines launch with no sandbox at all. Denied, they start restricted as usual. Magician’s own tools are unchanged, and god mode still decides whether they ask. Every launch is logged. A red banner shows in chat, tasks and VibeDev with a Turn off button.',
				confirmLabel: 'Turn on super god mode',
				destructive: true
			});
			if (!confirmed) return;
		}
		superBusy = true;
		try {
			await saveSuperGodMode(duration);
			superGodModePoll.pollNow();
			showSuccess(duration ? 'Super god mode is on' : 'Super god mode is off');
		} catch (caught) {
			showError(caught instanceof Error ? caught.message : 'Super god mode could not be saved.');
		} finally {
			superBusy = false;
		}
	}

	async function forget(): Promise<void> {
		const confirmed = await requestConfirmation({
			title: 'Forget tool and God-mode approvals?',
			message: 'Every “this session” and “always” approval is forgotten. Agents ask again next time.',
			confirmLabel: 'Forget approvals'
		});
		if (!confirmed) return;
		busy = true;
		try {
			godState = await forgetGodModeGrants();
			showSuccess('God mode approvals forgotten');
		} catch (caught) {
			showError(caught instanceof Error ? caught.message : 'Approvals could not be forgotten.');
		} finally {
			busy = false;
		}
	}
</script>

<section class="god-card" class:god-on={godState?.enabled} aria-labelledby="god-mode-title">
	<div class="god-header">
		<div>
			<p class="god-overline">Agent powers</p>
			<h2 id="god-mode-title">
				{godState ? `God mode is ${godState.enabled ? 'on' : 'off'} by default` : 'Loading god mode'}
			</h2>
			<p>
				Unrestricted shell, files and network for trusted agents in chat and agent runs, including harness engines
				through Magician’s tools. Point an agent at a GitHub repo or an MCP URL and it can clone, install and use it.
				This is the default for every chat and for runs no chat started; each chat can turn it on or off from the
				Magician orb in the composer, for that chat and the runs it starts.
			</p>
		</div>
		<div class="god-actions">
			<button
				class="god-button"
				class:god-button-danger={godState && !godState.enabled}
				type="button"
				disabled={!godState || busy}
				onclick={toggle}
			>
				{busy ? 'Updating…' : godState?.enabled ? 'Turn off' : 'Turn on'}
			</button>
			{#if godState}
				<button class="god-button god-button-secondary" type="button" disabled={busy} onclick={forget}
					title="Forget remembered tool permissions and God-mode approvals">
					Forget approvals{godState.always_grants > 0 ? ` (${godState.always_grants})` : ''}
				</button>
			{/if}
		</div>
	</div>
	<div class="god-warning" role="note">
		<strong>Warning.</strong> Commands run with your user’s full permissions: they can read, change or delete anything
		you can, install software and reach the network. Anything beyond the normal sandbox asks you first: allow once, for
		this session, or always. Ask / Accept still applies inside the normal sandbox. Commands on the blocked list (like
		<code>rm -rf /</code>) can only be allowed once. Changing this setting forgets every approval and every chat’s own
		choice. VibeDev is not affected: its coding engines stay in Magician’s box, and only super god mode below unrestricts them.
	</div>
	<div class="auto-approve" class:auto-approve-on={godState?.auto_approve}>
		<div class="super-god-copy">
			<h3 id="auto-approve-title">Auto-approve {godState?.auto_approve ? 'is on by default' : 'is off by default'}</h3>
			<p>
				Where god mode is on, what it would ask you is approved without asking. It still asks before anything that
				deletes (<code>rm -rf</code>, <code>find -delete</code>, <code>git reset --hard</code>, a forced push, a
				deleting edit), anything run as root (<code>sudo</code>, <code>doas</code>), code a command does not show
				(<code>curl … | sh</code>, <code>$CMD</code>) and the blocked list. Each chat can turn it on or off from the
				Magician orb. Turning it on or off keeps your approvals.
			</p>
		</div>
		<div class="god-actions">
			<button
				class="god-button"
				class:god-button-danger={godState && !godState.auto_approve}
				type="button"
				disabled={!godState || busy}
				aria-describedby="auto-approve-title"
				onclick={toggleAuto}
			>
				{busy ? 'Updating…' : godState?.auto_approve ? 'Turn off' : 'Turn on'}
			</button>
		</div>
	</div>
	<div class="super-god" class:super-god-on={$superState?.active}>
		<div class="super-god-copy">
			<h3 id="super-god-title">
				Super god mode (debug){$superState?.active ? ` is on: ${describeSuperGodModeEnd($superState)}` : ''}
			</h3>
			<p>
				Harnesses start with no restrictions once you approve (allow once, this session, or always): their native
				tools, your settings and MCP servers, and no sandbox for VibeDev engines. Magician’s own tools are unchanged.
				Every launch is logged; a red banner with Turn off shows in chat, tasks and VibeDev, and the VibeDev composer
				has a ⚡ toggle.
			</p>
		</div>
		<div class="god-actions">
			{#if $superState?.active}
				<button class="god-button" type="button" disabled={superBusy} onclick={() => setSuper(null)}>
					{superBusy ? 'Updating…' : 'Turn off'}
				</button>
			{:else}
				<button class="god-button god-button-danger" type="button" disabled={superBusy} onclick={() => setSuper('1h')}>
					On for 1 hour
				</button>
				<button class="god-button god-button-danger" type="button" disabled={superBusy} onclick={() => setSuper('forever')}>
					On until turned off
				</button>
			{/if}
		</div>
	</div>
	{#if error}
		<div class="god-error" role="alert">{error}</div>
	{/if}
</section>

<style>
	.god-card {
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		color: var(--text-primary);
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
		padding: 1rem;
	}
	.god-card.god-on {
		border-color: var(--danger);
	}
	.god-header {
		align-items: flex-start;
		display: flex;
		flex-wrap: wrap;
		gap: 1rem;
		justify-content: space-between;
	}
	.god-header > div:first-child {
		flex: 1 1 18rem;
		min-width: 0;
	}
	.god-overline,
	h2,
	p {
		margin: 0;
	}
	.god-overline {
		color: var(--text-secondary);
		font-size: 0.75rem;
		font-weight: 700;
		text-transform: uppercase;
	}
	h2 {
		font-size: 1.05rem;
		margin: 0.15rem 0 0.35rem;
	}
	p {
		color: var(--text-secondary);
		line-height: 1.45;
	}
	.god-actions {
		display: flex;
		flex-wrap: wrap;
		gap: 0.5rem;
	}
	.god-button {
		background: var(--button-primary-bg);
		border: 1px solid var(--accent-primary);
		border-radius: 6px;
		color: var(--button-primary-color);
		cursor: pointer;
		font-weight: 600;
		padding: 0.45rem 0.9rem;
	}
	.god-button-danger {
		background: var(--danger);
		border-color: var(--danger);
	}
	.god-button-secondary {
		background: transparent;
		color: var(--text-primary);
		border-color: var(--border-soft);
	}
	.god-button:disabled {
		cursor: not-allowed;
		opacity: 0.6;
	}
	.god-warning {
		background: var(--warning-soft, transparent);
		border: 1px solid var(--warning);
		border-radius: 6px;
		color: var(--text-primary);
		font-size: 0.875rem;
		line-height: 1.45;
		padding: 0.6rem 0.75rem;
	}
	.super-god {
		align-items: flex-start;
		border: 1px dashed var(--danger);
		border-radius: 6px;
		display: flex;
		flex-wrap: wrap;
		gap: 0.75rem;
		justify-content: space-between;
		padding: 0.6rem 0.75rem;
	}
	.super-god-on {
		border-style: solid;
		box-shadow: inset 4px 0 0 var(--danger);
	}
	.auto-approve {
		align-items: flex-start;
		border: 1px dashed var(--warning);
		border-radius: 6px;
		display: flex;
		flex-wrap: wrap;
		gap: 0.75rem;
		justify-content: space-between;
		padding: 0.6rem 0.75rem;
	}
	.auto-approve-on {
		border-style: solid;
		box-shadow: inset 4px 0 0 var(--warning);
	}
	.auto-approve h3 {
		color: var(--text-primary);
	}
	.super-god-copy {
		flex: 1 1 18rem;
		min-width: 0;
	}
	h3 {
		color: var(--danger);
		font-size: 0.95rem;
		margin: 0 0 0.25rem;
	}
	.god-error {
		color: var(--danger);
		font-size: 0.875rem;
	}
</style>
