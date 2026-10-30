<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	export let threadId: string | null = null;
	export let seedStagedAttachments: unknown = null;
	export let fireAndForget = 'never';
	export let hud = false;
	const dispatch = createEventDispatcher();
	export function focusComposer() {}
	export function attachFiles(_files: File[]) {}
	export function currentChatSessionId() { return 'selected-session'; }
</script>
<div class="chat-page" data-testid="hud-chat" data-thread={threadId} data-hud={hud} data-mode={fireAndForget} data-seeded={Boolean(seedStagedAttachments)}>
	<div class="chat-main">
		<div class="chat-messages-area">Transcript</div>
		<div class="composer-wrap"><div class="composer-shell">
			<div data-testid="composer-dock"><slot name="composer-dock" /></div>
			<div data-testid="attachment-actions"><slot name="composer-attachment-actions" /></div>
			<button type="button" on:click={() => dispatch('inline-send')}>Reply in HUD</button>
		</div></div>
	</div>
</div>
