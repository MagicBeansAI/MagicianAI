<script lang="ts">
	import { onMount } from 'svelte';
	import AttentionCenter from '$lib/attention/AttentionCenter.svelte';
	import { openAttentionCenter } from '$lib/attention/centerState';
	import AttentionPromptModal from '$lib/magician/components/AttentionPromptModal.svelte';
	import TasksWorkspace from '$lib/magician/tasks/TasksWorkspace.svelte';
	import { mountEscapeHandler } from '$lib/shell/overlayCoordinator';

	onMount(() => mountEscapeHandler());
</script>

<button type="button" data-attention-trigger on:click={openAttentionCenter}>Open Attention</button>
<TasksWorkspace navigationMode="local" />
<AttentionCenter />
<AttentionPromptModal />
