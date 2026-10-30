<script lang="ts">
 import { page } from '$app/stores';
 import { workSpecialistHref } from './specialistRoutes';
 import { goto } from '$app/navigation';
 import Button from '$lib/magician/components/native/Button.svelte';
 import { attentionBadgeCount } from '$lib/attention/attentionBadgeCount';
 import { workViewHref, type WorkView } from './routes';
 import WorkHelp from './WorkHelp.svelte';
 import { WORK_HELP } from './help';
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
			<a href={workViewHref(entry[0] as WorkView)} title={WORK_HELP[entry[0] as WorkView].description} aria-current={view === entry[0] ? 'page' : undefined}>{entry[1]}{#if entry[0] === 'needs-you' && $attentionBadgeCount > 0}<span class="count">{$attentionBadgeCount}</span>{/if}</a>
		{/each}
		<a href={workViewHref('goals')} class="nav-item--goals" title={WORK_HELP.goals.description} aria-current={view === 'goals' ? 'page' : undefined}>{WORK_HELP.goals.title}</a>
		<a href={workViewHref('programs')} class="nav-item--programs" title={WORK_HELP.programs.description} aria-current={view === 'programs' ? 'page' : undefined}>{WORK_HELP.programs.title}</a>
		<a href={workViewHref('quality')} class="nav-item--quality" title={WORK_HELP.quality.description} aria-current={view === 'quality' ? 'page' : undefined}>{WORK_HELP.quality.title}</a>
		<a href={workViewHref('operations')} class="nav-item--operations" title={WORK_HELP.operations.description} aria-current={view === 'operations' ? 'page' : undefined}>{WORK_HELP.operations.title}</a>
		<WorkHelp topic={WORK_HELP[view]} />
		<div class="work-more" bind:this={moreElement}>
			<button bind:this={moreTrigger} aria-expanded={moreOpen} aria-controls="work-more-menu" on:click={() => moreOpen = !moreOpen}>More ▾</button>
			{#if moreOpen}<div id="work-more-menu" class="more-menu">
				<div class="more-entry more-item--goals"><button title={WORK_HELP.goals.description} on:click={() => navigate('goals')}>{WORK_HELP.goals.title}</button><WorkHelp topic={WORK_HELP.goals} /></div>
				<div class="more-entry more-item--programs"><button title={WORK_HELP.programs.description} on:click={() => navigate('programs')}>{WORK_HELP.programs.title}</button><WorkHelp topic={WORK_HELP.programs} /></div>
				<div class="more-entry more-item--quality"><button title={WORK_HELP.quality.description} on:click={() => navigate('quality')}>{WORK_HELP.quality.title}</button><WorkHelp topic={WORK_HELP.quality} /></div>
				<div class="more-entry more-item--operations"><button title={WORK_HELP.operations.description} on:click={() => navigate('operations')}>{WORK_HELP.operations.title}</button><WorkHelp topic={WORK_HELP.operations} /></div>
				<a href={workSpecialistHref('/apps', $page.url)} class="more-item--apps">Apps and specialist tools →</a>
			</div>{/if}
		</div>
	</nav>
	{/if}
	{#if heading || returnHref}<div class="section-heading"><a href={returnHref || '/work'}>{returnHref ? '← Back to work item' : '← All work'}</a>{#if heading}<span>{heading}</span>{/if}</div>{/if}
	<div class="work-body" class:scrollable><slot /></div>
</div>
<style>
	.work-workspace {
		height: 100%;
		min-height: 0;
		min-width: 0;
		display: flex;
		flex-direction: column;
		color: var(--text-primary);
		background: var(--bg-base);
		overflow-y: auto;
		overflow-x: hidden;
	}
	.work-header,.work-nav,.section-heading { width:100%; max-width:var(--app-content-max,1320px); margin:0 auto; box-sizing:border-box; }
	.work-header { display:flex; align-items:center; justify-content:space-between; flex-wrap:wrap; gap:var(--space-sm); padding:var(--space-md) 1.45rem var(--space-xs); flex-shrink:0; }
	h1 { margin:0; font-family:var(--font-display); font-size:var(--text-xl); font-weight:700; letter-spacing:-0.01em; }
	.work-title p { margin:var(--space-xs) 0 0; font-size:var(--text-sm); color:var(--text-secondary); line-height:1.4; }
	.work-header :global(.native-button.work-create) { min-height:40px; font-weight:600; border-radius:var(--radius-md, 8px); box-shadow:0 1px 2px rgba(0,0,0,0.04); }
	.work-nav { display:flex; flex-wrap:wrap; align-items:center; gap:var(--space-xs); padding:var(--space-xs) 1.45rem var(--space-xs); flex-shrink:0; }
	.work-nav>a,.work-more>button { display:inline-flex; align-items:center; justify-content:center; gap:var(--space-xs); min-height:36px; padding:6px 14px; box-sizing:border-box; border:1px solid transparent; border-radius:var(--radius-md, 8px); color:var(--text-secondary); background:transparent; font:inherit; font-size:var(--text-sm); font-weight:600; text-decoration:none; white-space:nowrap; cursor:pointer; transition:all 0.15s ease; }
	.work-nav>a[aria-current] {
		color: var(--text-primary);
		background: linear-gradient(
			135deg,
			color-mix(in srgb, var(--accent-primary) 18%, var(--bg-card)) 0%,
			color-mix(in srgb, var(--accent-primary) 8%, var(--bg-card)) 100%
		);
		border-color: color-mix(in srgb, var(--accent-primary) 45%, var(--border-soft));
		box-shadow: 0 1px 4px color-mix(in srgb, var(--accent-primary) 12%, transparent);
	}
	.work-nav a:hover:not([aria-current]),.work-nav button:hover {
		color: var(--text-primary);
		background: linear-gradient(
			180deg,
			color-mix(in srgb, var(--accent-primary) 5%, var(--bg-card)) 0%,
			var(--bg-card) 100%
		);
		border-color: color-mix(in srgb, var(--accent-primary) 25%, var(--border-soft));
	}
	.work-more { position:relative; margin-left:auto; }
	.more-menu { position:absolute; right:0; top:calc(100% + 4px); z-index:30; display:grid; min-width:240px; max-width:calc(100vw - 24px); padding:var(--space-xs); border:1px solid var(--border-soft); border-radius:var(--radius-md, 8px); background:var(--bg-card); box-shadow:var(--shadow-lg); backdrop-filter:blur(8px); }
	.more-menu button,.more-menu a { padding:var(--space-xs) var(--space-sm); border-radius:var(--radius-sm, 6px); text-align:left; font:inherit; font-size:var(--text-sm); border:0; color:var(--text-primary); background:transparent; text-decoration:none; cursor:pointer; min-height:38px; display:flex; align-items:center; transition:background 0.12s ease; }
	.more-menu button:hover,.more-menu a:hover { background:var(--accent-primary-soft); color:var(--text-primary); }
	.count { border-radius:var(--radius-sm, 6px); padding:2px 6px; background:var(--color-error, #ff6b6b); color:#ffffff; font-size:var(--text-2xs); font-weight:700; line-height:1; }

	.work-nav>a.nav-item--operations { display: none; }
	.work-nav>a.nav-item--quality { display: none; }
	.work-nav>a.nav-item--programs { display: none; }
	.work-nav>a.nav-item--goals { display: none; }

	.more-entry { display:flex; align-items:center; } .more-entry>button { flex:1; }

	@media (min-width: 620px) {
		.work-nav>a.nav-item--goals { display: inline-flex; }
		.more-menu .more-item--goals { display: none; }
	}
	@media (min-width: 740px) {
		.work-nav>a.nav-item--programs { display: inline-flex; }
		.more-menu .more-item--programs { display: none; }
	}
	@media (min-width: 900px) {
		.work-nav>a.nav-item--quality { display: inline-flex; }
		.more-menu .more-item--quality { display: none; }
	}
	@media (min-width: 1040px) {
		.work-nav>a.nav-item--operations { display: inline-flex; }
		.more-menu .more-item--operations { display: none; }
	}
	.section-heading { display:flex; align-items:center; flex-wrap:wrap; gap:var(--space-md); padding:var(--space-sm) 1.45rem; font-size:var(--text-sm); flex-shrink:0; }
	.section-heading a { color:var(--text-secondary); text-decoration:none; font-weight:500; }
	.section-heading a:hover { color:var(--text-primary); text-decoration:underline; }
	.section-heading span { font-weight:600; color:var(--text-primary); }
	.work-body { display:flex; flex-direction:column; flex:1 0 auto; min-height:0; min-width:0; }
	.work-body.scrollable { overflow:visible; display:block; }
	@media(max-width:959px) { .work-body :global(button),.work-body :global(.native-button),.work-body :global(input:not([type="checkbox"]):not([type="radio"])),.work-body :global(select) { min-height:44px; } }
	a:focus-visible,button:focus-visible { outline:2px solid var(--accent-primary); outline-offset:3px; }
	@media(max-width:600px) {
		.compact-work-list { overflow-y:auto; }
		.compact-work-list .work-header { flex-shrink:0; }
		.compact-work-list .work-body { flex:none; overflow:visible; }
		.compact-work-list :global(.existing-workspace) { flex:none; height:auto; min-height:360px; }
		.compact-work-list :global(.native-task-main-row) { flex-wrap:wrap; }
		.compact-work-list :global(.native-task-title-button) { flex:1 1 60%; min-width:60%; }
		.more-menu { position:fixed; top:auto; right:var(--space-sm); bottom:var(--space-sm); max-height:60dvh; overflow:auto; overscroll-behavior:contain; box-sizing:border-box; }
		.work-header,.work-nav,.section-heading { padding-left:var(--space-sm); padding-right:var(--space-sm); }
		.work-nav { gap:var(--space-2xs, 4px); }
		.work-nav>a,.work-more>button { padding:6px 10px; font-size:var(--text-xs); min-height:36px; }
		.work-title p { max-width:240px; }
	}
	@media(max-width:600px) { .work-nav { flex-wrap:nowrap; align-items:center; } .work-nav>a { flex:1; min-width:0; white-space:normal; text-align:center; } .work-more { margin-left:0; } }
</style>
