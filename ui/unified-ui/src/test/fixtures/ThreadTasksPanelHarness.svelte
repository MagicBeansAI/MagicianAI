<script lang="ts">
	/**
	 * The thread workspace's Tasks tab, with the one thing its shell normally
	 * supplies: the thread page context. Everything else — the task store, the
	 * panel, its adapter — is the route's own, because the point of the test
	 * that uses this is the route's wiring, not a re-implementation of it.
	 */
	import { readable } from 'svelte/store';

	import { setThreadPageContext } from '$lib/threads/threadPageContext';
	import ThreadTasksPage from '../../routes/(app)/t/[name]/tasks/+page.svelte';

	export let threadName = 'general';

	setThreadPageContext(
		readable({
			threadName,
			threadDisplayName: threadName,
			threadDetail: null,
			loadingThread: false,
			threadError: null
		})
	);
</script>

<ThreadTasksPage />
