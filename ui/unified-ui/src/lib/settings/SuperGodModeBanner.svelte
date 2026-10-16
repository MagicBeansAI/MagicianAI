<script lang="ts">
	import { showError, showSuccess } from '$lib/shared/stores/notifications';
	import { describeSuperGodModeEnd, saveSuperGodMode, superGodModePoll } from '$lib/settings/godMode';

	const superState = superGodModePoll.value;
	let busy = $state(false);

	async function turnOff(): Promise<void> {
		busy = true;
		try {
			await saveSuperGodMode(null);
			superGodModePoll.pollNow();
			showSuccess('Super god mode is off');
		} catch (caught) {
			showError(caught instanceof Error ? caught.message : 'Super god mode could not be turned off.');
		} finally {
			busy = false;
		}
	}
</script>

{#if $superState?.active}
	<div class="super-god-banner" role="alert">
		<span class="super-god-text">
			<strong>Super god mode is on</strong> ({describeSuperGodModeEnd($superState)}). Harnesses you approve
			start with no restrictions: native tools, your own settings and MCP servers, no sandbox.
		</span>
		<button class="super-god-off" type="button" disabled={busy} onclick={turnOff}>
			{busy ? 'Turning off…' : 'Turn off'}
		</button>
	</div>
{/if}

<style>
	.super-god-banner {
		align-items: center;
		background: var(--danger);
		color: var(--accent-on-primary, #fff);
		display: flex;
		flex-wrap: wrap;
		gap: 0.5rem 1rem;
		justify-content: space-between;
		padding: 0.45rem 1rem;
	}
	.super-god-text {
		font-size: 0.875rem;
		line-height: 1.4;
		min-width: 0;
	}
	.super-god-off {
		background: transparent;
		border: 1px solid currentColor;
		border-radius: 6px;
		color: inherit;
		cursor: pointer;
		font-weight: 600;
		padding: 0.25rem 0.8rem;
	}
	.super-god-off:disabled {
		cursor: not-allowed;
		opacity: 0.6;
	}
</style>
