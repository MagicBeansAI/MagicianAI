<script lang="ts">
 import TasksWorkspace from '$lib/magician/tasks/TasksWorkspace.svelte';
 export let threadName = 'general';
</script>

<TasksWorkspace routeBase="/work" workContext threadContextId={threadName} />
