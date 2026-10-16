<script lang="ts">
 import { page } from '$app/stores';
 import WorkFrame from '$lib/work/WorkFrame.svelte';
 import { workReturnHref, workAgentHref, workViewHref } from '$lib/work/routes';
 $: child = $page.url.pathname !== '/work' && $page.url.pathname !== '/work/';
 $: heading = $page.url.pathname.endsWith('/edit') ? 'Edit member' : $page.url.pathname.endsWith('/rules') ? 'Member rules' : $page.params.tier ? 'Memory tier' : 'Add member';
 $: returnHref = workReturnHref($page.url.searchParams.get('from')) || ($page.params.id ? workAgentHref($page.params.id) : workViewHref('crew'));
</script>
{#if child}<WorkFrame view="crew" scrollable {heading} {returnHref}><slot /></WorkFrame>{:else}<slot />{/if}
