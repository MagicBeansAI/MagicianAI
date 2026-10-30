<script lang="ts">
	import { tick } from 'svelte';
	import { verificationExplanation, type AnswerVerification } from '../answerVerification';
	let { verification }: { verification: AnswerVerification } = $props();
	let open = $state(false);
	let host: HTMLSpanElement;
	let trigger: HTMLButtonElement;
	let popup = $state<HTMLDivElement>();
	let left = $state(12);
	let top = $state(12);
	const id = $props.id();
	function close(restoreFocus = false) {
		open = false;
		if (restoreFocus) trigger?.focus({ preventScroll: true });
	}
	function portal(node: HTMLDivElement) {
		document.body.appendChild(node);
		return { destroy() { node.remove(); } };
	}
	function position() {
		if (!open || !popup || !trigger) return;
		const rect = trigger.getBoundingClientRect();
		left = Math.max(12, Math.min(rect.right - popup.offsetWidth, window.innerWidth - popup.offsetWidth - 12));
		top = rect.bottom + popup.offsetHeight + 12 <= window.innerHeight
			? rect.bottom + 6 : Math.max(12, rect.top - popup.offsetHeight - 6);
	}
	async function toggle() {
		if (open) { close(); return; }
		open = true;
		await tick();
		position();
		popup?.querySelector('button')?.focus({ preventScroll: true });
	}
</script>

<svelte:window onclick={event => { if (!host?.contains(event.target as Node) && !popup?.contains(event.target as Node)) close(); }}
	onkeydowncapture={event => { if (open && event.key === 'Escape') { event.preventDefault(); event.stopPropagation(); close(true); } }}
	onresize={position} onscrollcapture={position} />
<span class="verification-info" bind:this={host}>
	<button class="trigger" type="button" data-popover-trigger bind:this={trigger} aria-label="Not fully verified — view details"
		title="Not fully verified — view details" aria-haspopup="dialog" aria-expanded={open} aria-controls={open ? id : undefined} onclick={toggle}>
		<svg viewBox="0 0 20 20" aria-hidden="true"><circle cx="10" cy="10" r="7.5"/><path d="M10 9v5m0-8v.2"/></svg>
	</button>
	{#if open}
		<div use:portal bind:this={popup} {id} role="dialog" aria-label="Answer verification" class="verification-popup" style:left={`${left}px`} style:top={`${top}px`}>
			<div class="heading"><strong>Not fully verified</strong><button type="button" aria-label="Close verification details" onclick={() => close(true)}>×</button></div>
			<p class="check-details">{verification.details ?? verificationExplanation}</p>
		</div>
	{/if}
</span>

<style>
	.verification-info { display: inline-flex; align-items: center; vertical-align: middle; }
	.trigger { display: inline-flex; align-items: center; justify-content: center; width: 26px; height: 26px; padding: 4px; border: 0; border-radius: var(--radius-md); background: transparent; color: var(--status-attention); cursor: pointer; }
	.trigger:hover, .trigger[aria-expanded="true"] { background: var(--status-attention-soft); }
	button:focus-visible { outline: 2px solid var(--accent-primary); outline-offset: 2px; }
	svg { width: 16px; height: 16px; fill: none; stroke: currentColor; stroke-width: 1.7; stroke-linecap: round; }
	.verification-popup { display: block; position: fixed; inset: auto; margin: 0; z-index: 2000; width: 320px; max-width: calc(100vw - 24px); max-height: 60dvh; overflow: auto; box-sizing: border-box; padding: 12px; border: 1px solid var(--border-soft); border-radius: var(--radius-md); background: var(--bg-card); color: var(--text-primary); box-shadow: var(--shadow-lg); font: 400 var(--text-sm)/1.5 var(--font-primary); text-align: left; white-space: normal; }
	.heading { display: flex; align-items: center; justify-content: space-between; gap: 12px; }
	.heading button { width: 26px; height: 26px; padding: 0; border: 0; border-radius: var(--radius-sm); background: var(--bg-soft); color: var(--text-secondary); cursor: pointer; font-size: 20px; line-height: 1; }
	p { margin: 6px 0 0; }
	.check-details { overflow-wrap: anywhere; }
	@media (pointer: coarse) { .trigger, .heading button { width: 44px; height: 44px; } }
</style>
