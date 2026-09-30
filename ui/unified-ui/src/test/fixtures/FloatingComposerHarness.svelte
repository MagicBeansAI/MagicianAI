<script lang="ts">
	import FloatingComposer from '$lib/shell/FloatingComposer.svelte';

	export let isSending = false;
    export let allowParallel = false;
	export let canSend = true;
	export let disabled = false;
	export let isReadOnly = false;
	export let supportsAttachments = true;
	/** null → the composer derives emptiness from the typed draft. */
	export let hasContent: boolean | null = null;
	/** Fills the `dock` slot — stands in for the docked ContextPill. */
	export let showDock = false;
    export let showTop = false;
	/** Fills the `dock-actions` slot — stands in for the HUD's theme/expand. */
	export let showDockActions = false;

	let value = '';
	let mode: 'ask' | 'accept_in_scope' | 'plan' = 'ask';
	/** The host remembers the permission separately, so leaving Plan returns to
	 *  the posture that was in force. Mirrored here so the harness exercises the
	 *  same wiring rather than a simplified one. */
	let permission: 'ask' | 'accept_in_scope' = 'ask';
	let lastAction = '';
	let lastDroppedFiles = 0;
</script>

<FloatingComposer
	bind:value
	{mode}
	{permission}
	{isSending}
    {allowParallel}
	{canSend}
	{disabled}
	{isReadOnly}
	{supportsAttachments}
	{hasContent}
	dock={showDock || showDockActions}
	on:send={() => (lastAction = `send:${value}`)}
    on:parallel={() => (lastAction = `parallel:${value}`)}
    on:stopAndSend={() => (lastAction = `stopAndSend:${value}`)}
	on:stop={() => (lastAction = 'stop')}
	on:attach={() => (lastAction = 'attach')}
	on:attachFiles={(event) => {
		lastDroppedFiles = event.detail.files.length;
		lastAction = `attachFiles:${lastDroppedFiles}`;
	}}
	on:setMode={(event) => {
		mode = event.detail;
		// Selecting a permission also remembers it, so returning from Plan
		// restores the posture rather than silently resetting to Ask.
		if (event.detail !== 'plan') permission = event.detail;
		lastAction = `mode:${mode}`;
	}}
>
    <svelte:fragment slot="top">{#if showTop}<div data-testid="composer-top">Queued / background requests</div>{/if}</svelte:fragment>
	<svelte:fragment slot="dock">
		{#if showDock}<span data-testid="dock-content">session</span>{/if}
	</svelte:fragment>
	<svelte:fragment slot="dock-actions">
		{#if showDockActions}<button type="button" data-testid="dock-action">expand</button>{/if}
	</svelte:fragment>
</FloatingComposer>

<output data-testid="composer-value">{value}</output>
<output data-testid="composer-mode">{mode}</output>
<output data-testid="composer-action">{lastAction}</output>
<output data-testid="composer-dropped-files">{lastDroppedFiles}</output>
