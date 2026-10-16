<script lang="ts">
	/**
	 * One-line text that keeps its start and its end and ellipsizes the middle,
	 * so a long file name still shows its extension ("report-sept…final.html")
	 * instead of wrapping a stray letter onto the next line. The head shrinks
	 * with `text-overflow: ellipsis`; the tail never shrinks. Full text in the
	 * title tooltip.
	 *
	 * With `copyText`, the text itself is the copy affordance: clicking it puts
	 * `copyText` (an absolute path) on the clipboard and flashes "Path copied".
	 */
	import { onDestroy } from 'svelte';
	import { showError } from '$lib/shared/stores/notifications';

	export let text: string;
	/** Characters always kept at the end (covers a typical extension + stem tail). */
	export let tail = 10;
	/** When set, clicking copies this value (e.g. the file's absolute path). */
	export let copyText: string | undefined = undefined;

	$: keep = Math.min(Math.max(tail, 0), text.length);
	$: head = text.slice(0, text.length - keep);
	$: end = text.slice(text.length - keep);

	let copied = false;
	let resetTimer: ReturnType<typeof setTimeout> | undefined;

	async function copy(event: MouseEvent) {
		event.stopPropagation();
		if (!copyText) return;
		try {
			await navigator.clipboard.writeText(copyText);
			copied = true;
			clearTimeout(resetTimer);
			resetTimer = setTimeout(() => (copied = false), 1400);
		} catch {
			showError('Could not copy the path');
		}
	}

	onDestroy(() => clearTimeout(resetTimer));
</script>

{#if copyText}
	<button
		type="button"
		class="mg-middle-ellipsis mg-middle-ellipsis--copy"
		title={`${text}\nClick to copy ${copyText}`}
		aria-label={copied ? 'Path copied' : `${text} — copy path`}
		on:click={copy}
	>
		{#if head}<span class="mg-middle-ellipsis__head">{head}</span>{/if}<span
			class="mg-middle-ellipsis__tail">{end}</span
		>
		<span class="mg-middle-ellipsis__copied" class:mg-middle-ellipsis__copied--on={copied} aria-hidden="true"
			>Path copied</span
		>
	</button>
{:else}
	<span class="mg-middle-ellipsis" title={text}>
		{#if head}<span class="mg-middle-ellipsis__head">{head}</span>{/if}<span
			class="mg-middle-ellipsis__tail">{end}</span
		>
	</span>
{/if}

<style>
	.mg-middle-ellipsis {
		display: flex;
		min-width: 0;
		max-width: 100%;
		white-space: nowrap;
	}

	.mg-middle-ellipsis__head {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.mg-middle-ellipsis__tail {
		flex: none;
	}

	.mg-middle-ellipsis--copy {
		position: relative;
		padding: 0;
		border: 0;
		background: transparent;
		color: inherit;
		font: inherit;
		text-align: left;
		cursor: copy;
	}

	.mg-middle-ellipsis--copy:hover .mg-middle-ellipsis__head,
	.mg-middle-ellipsis--copy:hover .mg-middle-ellipsis__tail {
		text-decoration: underline dotted;
		text-underline-offset: 2px;
	}

	.mg-middle-ellipsis--copy:focus-visible {
		outline: none;
		border-radius: 3px;
		box-shadow: var(--ring-focus);
	}

	/* Confirmation pill: pops above the name on the shared spring tokens. */
	.mg-middle-ellipsis__copied {
		position: absolute;
		bottom: calc(100% + 4px);
		left: 0;
		padding: 0.1rem 0.45rem;
		border-radius: var(--radius-full);
		background: var(--text-primary);
		color: var(--bg-base);
		font-size: 0.68rem;
		font-weight: 600;
		pointer-events: none;
		opacity: 0;
		transform: translateY(4px) scale(0.7);
		transform-origin: 0 100%;
		transition:
			transform var(--dur-quick) var(--spring-snappy),
			opacity var(--dur-quick) ease;
	}

	.mg-middle-ellipsis__copied--on {
		opacity: 1;
		transform: translateY(0) scale(1);
		transition:
			transform var(--dur-base) var(--spring-bouncy),
			opacity var(--dur-quick) ease;
	}
</style>
