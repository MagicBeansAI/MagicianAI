<script lang="ts">
 import { page } from '$app/stores';
 import { workSpecialistHref } from './specialistRoutes';
 import { goto } from '$app/navigation';
 import Button from '$lib/magician/components/native/Button.svelte';
 import { attentionBadgeCount } from '$lib/attention/attentionBadgeCount';
 import { workViewHref, type WorkView } from './routes';
 export let view: WorkView = 'all';
 export let creating = false;
 export let compact = false;
 export let scrollable = false;
 export let returnHref: string | null = null;
 export let heading = '';
 export let createHref = '/work?compose=1';
 let moreOpen = false;
 let moreElement: HTMLDivElement;
 let moreTrigger: HTMLButtonElement;
 function navigate(next: WorkView) { moreOpen = false; void goto(workViewHref(next)); }
 function dismissMenu(event: MouseEvent) { if (moreOpen && !moreElement?.contains(event.target as Node)) moreOpen = false; }
 function menuKey(event: KeyboardEvent) { if (moreOpen && event.key === 'Escape') { event.preventDefault(); moreOpen = false; moreTrigger?.focus(); } }
</script>
<svelte:window on:click={dismissMenu} on:keydown={menuKey} />
<svelte:head><title>Work · Magican</title></svelte:head>
<div class="work-workspace" class:compact-work-list={compact}>
	<header class="work-header">
		<div class="work-title"><h1>{creating ? 'New work' : 'Work'}</h1><p>{creating ? 'Describe what you want done. Schedule and other settings are optional.' : 'Your tasks, crew and ongoing work, together.'}</p></div>
		{#if !creating}<Button label="+ New work" href={createHref} className="work-create" />{/if}
	</header>
	{#if !creating}
	<nav class="work-nav" aria-label="Work views">
		{#each [['all', 'All work'], ['needs-you', 'Needs you'], ['crew', 'Crew']] as entry}
			<a href={workViewHref(entry[0] as WorkView)} aria-current={view === entry[0] ? 'page' : undefined}>{entry[1]}{#if entry[0] === 'needs-you' && $attentionBadgeCount > 0}<span class="count">{$attentionBadgeCount}</span>{/if}</a>
		{/each}
		<div class="work-more" bind:this={moreElement}>
			<button bind:this={moreTrigger} aria-expanded={moreOpen} aria-controls="work-more-menu" on:click={() => moreOpen = !moreOpen}>More ▾</button>
			{#if moreOpen}<div id="work-more-menu" class="more-menu">
				<button on:click={() => navigate('programs')}>Programs</button>
				<button on:click={() => navigate('quality')}>Quality and review</button>
				<button on:click={() => navigate('operations')}>Company loop and diagnostics</button>
				<a href={workSpecialistHref('/apps', $page.url)}>Apps and specialist tools →</a>
			</div>{/if}
		</div>
	</nav>
	{/if}
	{#if heading || (view !== 'all' && view !== 'needs-you' && view !== 'crew')}<div class="section-heading"><a href={returnHref || '/work'}>{returnHref ? '← Back to work item' : '← All work'}</a><span>{heading || (view === 'programs' ? 'Programs' : view === 'quality' ? 'Quality and review' : 'Company loop and diagnostics')}</span></div>{/if}
	<div class="work-body" class:scrollable><slot /></div>
</div>
<style>
	.work-workspace { height:100%; min-height:0; min-width:0; display:flex; flex-direction:column; color:var(--text-primary); background:var(--bg-base); }
	.work-header,.work-nav,.section-heading { width:100%; max-width:var(--app-content-max,1320px); margin:0 auto; box-sizing:border-box; }
	.work-header { display:flex; align-items:center; justify-content:space-between; flex-wrap:wrap; gap:var(--space-sm); padding:var(--space-md) var(--space-lg) var(--space-sm); }
	h1 { margin:0; font-family:var(--font-display); font-size:var(--text-xl); } .work-title p { margin:var(--space-xs) 0 0; font-size:var(--text-sm); color:var(--text-secondary); }
	.work-header :global(.native-button.work-create) { min-height:44px; }
	.work-nav { display:flex; flex-wrap:wrap; gap:var(--space-sm); padding:var(--space-sm) var(--space-lg); flex-shrink:0; }
	.work-nav>a,.work-more>button { display:inline-flex; align-items:center; justify-content:center; gap:var(--space-sm); min-height:44px; padding:var(--space-sm) var(--space-sm); box-sizing:border-box; border:1px solid transparent; border-radius:var(--radius-md); color:var(--text-secondary); background:transparent; font:inherit; font-size:var(--text-sm); font-weight:600; text-decoration:none; white-space:nowrap; cursor:pointer; }
	.work-nav>a[aria-current] { color:var(--text-primary); background:var(--accent-primary-soft); border-color:var(--border-soft); }
	.work-nav a:hover,.work-nav button:hover { color:var(--text-primary); background:var(--bg-card); }
	.work-more { position:relative; margin-left:auto; } .more-menu { position:absolute; right:0; top:100%; z-index:30; display:grid; min-width:240px; max-width:calc(100vw - 24px); padding:var(--space-sm); border:1px solid var(--border-soft); border-radius:var(--radius-lg); background:var(--bg-card); box-shadow:var(--shadow-lg); }
	.more-menu button,.more-menu a { padding:var(--space-sm); text-align:left; font:inherit; font-size:var(--text-sm); border:0; color:var(--text-primary); background:transparent; text-decoration:none; cursor:pointer; min-height:44px; }
	.more-menu button:hover,.more-menu a:hover { background:var(--accent-primary-soft); }
	.count { border-radius:var(--radius-full); padding:2px 6px; background:var(--bg-card); color:var(--text-primary); font-size:var(--text-xs); }
	.section-heading { display:flex; flex-wrap:wrap; gap:var(--space-md); padding:var(--space-sm) var(--space-lg); font-size:var(--text-sm); } .section-heading a { color:var(--text-primary); }
	.work-body { display:flex; flex-direction:column; flex:1; min-height:0; min-width:0; overflow:hidden; } .work-body.scrollable { overflow:auto; display:block; }
	@media(max-width:959px) { .work-body :global(button),.work-body :global(.native-button),.work-body :global(input:not([type="checkbox"]):not([type="radio"])),.work-body :global(select) { min-height:44px; } }
	a:focus-visible,button:focus-visible { outline:2px solid var(--accent-primary); outline-offset:3px; }
	@media(max-width:600px) {
		.compact-work-list { overflow-y:auto; }
		.compact-work-list .work-header { flex-shrink:0; }
		.compact-work-list .work-body { flex:none; overflow:visible; }
		.compact-work-list :global(.existing-workspace) { flex:none; height:65dvh; min-height:360px; }
		.compact-work-list :global(.native-task-main-row) { flex-wrap:wrap; }
		.compact-work-list :global(.native-task-title-button) { flex:1 1 60%; min-width:60%; }
	}
	@media(max-width:600px) { .more-menu { position:fixed; top:auto; right:var(--space-sm); bottom:var(--space-sm); max-height:60dvh; overflow:auto; overscroll-behavior:contain; box-sizing:border-box; } }
	@media(max-width:600px) { .work-header,.work-nav,.section-heading { padding-left:var(--space-sm); padding-right:var(--space-sm); } .work-nav { gap:var(--space-xs); } .work-nav>a,.work-more>button { padding:var(--space-sm); font-size:var(--text-xs); } .work-title p { max-width:200px; } }
</style>
