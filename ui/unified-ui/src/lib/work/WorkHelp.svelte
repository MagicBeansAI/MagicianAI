<script lang="ts">
	import { tick } from 'svelte';
	import type { WorkHelpTopic } from './help';
	let { topic }: { topic: WorkHelpTopic } = $props();
	let container: HTMLSpanElement;
	let trigger: HTMLButtonElement;
	let bubble = $state<HTMLSpanElement>();
	let open = $state(false);
	let pinned = $state(false);
	let left = $state(8);
	let top = $state(8);
	const id = $props.id();
	async function show() {
		open = true;
		await tick();
		if (!open || !trigger || !bubble) return;
		const rect = trigger.getBoundingClientRect();
		left = Math.max(8, Math.min(rect.left, window.innerWidth - bubble.offsetWidth - 8));
		top = rect.bottom + bubble.offsetHeight + 8 < window.innerHeight
			? rect.bottom + 6 : Math.max(8, rect.top - bubble.offsetHeight - 6);
	}
	function close() { open = false; pinned = false; }
	function dismiss(event: MouseEvent) { if (!container?.contains(event.target as Node)) close(); }
</script>

<svelte:window onclick={dismiss} onkeydown={event => { if (event.key === 'Escape') close(); }} onresize={close} onscrollcapture={event => { if (!bubble?.contains(event.target as Node)) close(); }} />
<span class="work-help" role="presentation" bind:this={container} onmouseleave={() => { if (!pinned) open = false; }}>
	<button type="button" bind:this={trigger} aria-label={`About ${topic.title}`} aria-expanded={open}
		aria-describedby={open ? id : undefined}
		onmouseenter={show} onfocus={show} onblur={close}
		onclick={() => { if (pinned) close(); else { pinned = true; void show(); } }}>
		<svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="9" /><path d="M12 11v6m0-10v.1" /></svg>
	</button>
	{#if open}<span bind:this={bubble} {id} role="tooltip" class="bubble" style:left={`${left}px`} style:top={`${top}px`}>
		<strong>{topic.title}</strong><span>{topic.description}</span>
	</span>{/if}
</span>

<style>
	.work-help { display:inline-flex; align-items:center; vertical-align:middle; font-weight:400; text-transform:none; letter-spacing:normal; }
	button { display:inline-flex; align-items:center; justify-content:center; width:32px; height:32px; min-height:32px; flex-shrink:0; padding:6px; border:0; border-radius:var(--radius-md); background:transparent; color:var(--text-secondary); cursor:pointer; }
	button:hover,button[aria-expanded="true"] { color:var(--text-primary); background:var(--accent-primary-soft); }
	button:focus-visible { outline:2px solid var(--accent-primary); outline-offset:2px; }
	svg { width:17px; height:17px; fill:none; stroke:currentColor; stroke-width:1.8; stroke-linecap:round; }
	.bubble { position:fixed; z-index:2000; display:grid; gap:6px; width:290px; max-width:calc(100vw - 16px); max-height:50dvh; overflow:auto; padding:12px; box-sizing:border-box; border:1px solid var(--border-soft); border-radius:var(--radius-md); background:var(--bg-card); color:var(--text-primary); box-shadow:var(--shadow-lg); font:400 var(--text-sm)/1.5 var(--font-primary); text-align:left; white-space:normal; }
	strong { font-weight:600; }
	@media(pointer:coarse) { button { width:44px; height:44px; min-height:44px; } }
</style>
