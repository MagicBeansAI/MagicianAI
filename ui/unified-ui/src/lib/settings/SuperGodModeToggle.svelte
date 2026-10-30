<script lang="ts">
	import { tick } from 'svelte';
	import PresenceOrb from '$lib/magician/chat/next/PresenceOrb.svelte';
	import type { PresenceState } from '$lib/magician/chat/next/presence';
	import { clickOutside } from '$lib/shared/clickOutside';
	import { createMenuKeydown } from '$lib/shared/menuKeydown';
	import { requestConfirmation } from '$lib/stores/confirmationStore';
	import { showError, showSuccess } from '$lib/shared/stores/notifications';
	import {
		describeSuperGodModeEnd,
		saveSuperGodMode,
		superGodModePoll,
		type SuperGodModeDuration
	} from '$lib/settings/godMode';

	const superState = superGodModePoll.value;
	let { orb = false, labelled = false, presenceState = 'idle' }: { orb?: boolean; labelled?: boolean; presenceState?: PresenceState } = $props();
	let menuOpen = $state(false);
	let busy = $state(false);
	let root = $state<HTMLDivElement>();
	let menu = $state<HTMLDivElement>();
	let trigger = $state<HTMLButtonElement>();
	function closeMenu(refocus: boolean): void {
		menuOpen = false;
		if (refocus) (trigger ?? root?.querySelector<HTMLButtonElement>('button'))?.focus();
	}
	const menuKeydown = createMenuKeydown({ getMenuEl: () => menu ?? null, close: closeMenu });
	async function toggleMenu(event?: MouseEvent): Promise<void> {
		trigger = event?.currentTarget instanceof HTMLButtonElement ? event.currentTarget : root?.querySelector<HTMLButtonElement>('button') ?? undefined;
		menuOpen = !menuOpen;
		if (menuOpen) { await tick(); menu?.querySelector<HTMLButtonElement>('button')?.focus(); }
	}

	async function turnOn(duration: SuperGodModeDuration): Promise<void> {
		menuOpen = false;
		const confirmed = await requestConfirmation({
			title: duration === '1h' ? 'Turn on super god mode for 1 hour?' : 'Turn on super god mode until you turn it off?',
			message:
				'For debugging. Before a harness starts, you are asked once (allow once, this session, or always). Approved, it starts with no restrictions. VibeDev coding engines get no sandbox at all. Chat and agent-run engines get their own native tools, your settings and MCP servers beside Magician’s tools. Magician’s own tools are unchanged. Every launch is logged, and a red banner shows with Turn off.',
			confirmLabel: 'Turn on super god mode',
			destructive: true
		});
		if (!confirmed) return;
		await save(duration);
	}

	async function save(duration: SuperGodModeDuration | null): Promise<void> {
		busy = true;
		try {
			await saveSuperGodMode(duration);
			superGodModePoll.pollNow();
			showSuccess(duration ? 'Super god mode is on' : 'Super god mode is off');
		} catch (caught) {
			showError(caught instanceof Error ? caught.message : 'Super god mode could not be saved.');
		} finally {
			busy = false;
		}
	}
</script>

<div class="sgm-toggle" class:orb class:labelled bind:this={root} use:clickOutside={{ enabled: menuOpen, handler: () => closeMenu(false) }}>
	{#if orb}
		<PresenceOrb variant="dev" state={presenceState} godMode={$superState?.active ?? null} godToggleable={!busy}
			modeMenuOpen={menuOpen} on:toggleGodMode={() => $superState?.active ? save(null) : toggleMenu()} />
	{/if}
	{#if !orb || labelled}
	{#if $superState?.active}
		<button
			type="button"
			class="sgm-button sgm-on"
			disabled={busy}
			title="Super god mode is on ({describeSuperGodModeEnd($superState)}). Click to turn it off."
			aria-label="Turn off super god mode"
			onclick={() => save(null)}
		>
			<span aria-hidden="true">⚡</span>
			<span class="sgm-label">Super god · {describeSuperGodModeEnd($superState)}</span>
		</button>
	{:else}
		<button
			type="button"
			class="sgm-button"
			disabled={busy}
			title="Super god mode (debug): start harnesses with no restrictions, after you approve"
			aria-label="Super god mode"
			aria-haspopup="menu"
			aria-expanded={menuOpen}
			onclick={toggleMenu}
		>
			<span aria-hidden="true">⚡</span>
			<span class="sgm-label">Super God</span>
		</button>
	{/if}
	{/if}
		{#if menuOpen && !$superState?.active}
			<div class="sgm-menu" role="menu" aria-label="Super god mode" tabindex="-1" bind:this={menu} onkeydown={menuKeydown}>
				<button type="button" role="menuitem" onclick={() => turnOn('1h')}>Super god mode for 1 hour</button>
				<button type="button" role="menuitem" onclick={() => turnOn('forever')}>Super god mode until turned off</button>
			</div>
		{/if}
</div>

<style>
	.sgm-toggle {
		position: relative;
		display: inline-flex;
	}
	.labelled { align-items: center; gap: 0.35rem; }
	.sgm-button {
		align-items: center;
		background: transparent;
		border: 1px solid var(--border-soft);
		border-radius: 6px;
		color: var(--text-secondary);
		cursor: pointer;
		display: inline-flex;
		font-size: 0.8rem;
		gap: 0.3rem;
		height: 28px;
		padding: 0 0.5rem;
	}
	.sgm-button:hover:not(:disabled) {
		border-color: var(--danger);
		color: var(--danger);
	}
	.sgm-on {
		background: var(--danger);
		border-color: var(--danger);
		color: var(--accent-on-primary, #fff);
		font-weight: 600;
	}
	.sgm-on:hover:not(:disabled) {
		color: var(--accent-on-primary, #fff);
	}
	.sgm-button:disabled {
		cursor: not-allowed;
		opacity: 0.6;
	}
	.sgm-label {
		white-space: nowrap;
	}
	.sgm-menu {
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		bottom: calc(100% + 6px);
		box-shadow: 0 6px 20px rgb(0 0 0 / 0.18);
		display: flex;
		flex-direction: column;
		left: 0;
		min-width: 15rem;
		padding: 0.25rem;
		position: absolute;
		z-index: 20;
	}
	.sgm-menu button {
		background: transparent;
		border: 0;
		border-radius: 6px;
		color: var(--text-primary);
		cursor: pointer;
		font-size: 0.85rem;
		padding: 0.45rem 0.6rem;
		text-align: left;
	}
	.orb .sgm-menu { left: auto; right: 0; max-width: calc(100vw - 32px); }
	.sgm-menu button:hover {
		background: var(--bg-hover, rgb(127 127 127 / 0.12));
		color: var(--danger);
	}
</style>
