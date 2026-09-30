<!--
  Toolbar button that opens the system camera (mobile) or file picker
  (desktop) and dispatches a `capture` event with the resulting `File`.

  Hidden on surfaces where it would never function — desktop without
  `capture=environment` support still gets a regular file picker, but
  that's already covered by the existing attach button; we only render
  on touch surfaces to avoid two buttons that do basically the same
  thing.
-->
<script lang="ts">
	import { browser } from '$app/environment';
	import { createEventDispatcher, onDestroy, onMount } from 'svelte';

	import { isCameraCaptureSupported, makeCapturedImageFile } from '$lib/media/capture/camera';
	import { publishMediaEvent } from '$lib/media/session';
	import {
		MEDIA_CAPTURE_CANCELLED,
		MEDIA_CAPTURE_COMPLETED,
		MEDIA_CAPTURE_ERROR,
		MEDIA_CAPTURE_STARTED
	} from '$lib/media/types';

	export let disabled: boolean = false;
	export let compact: boolean = false;
	/** Render even on desktop wide. Default false — the gate below
	 *  already covers touch surfaces AND narrow viewports, which are
	 *  the only cases where Camera meaningfully differs from the
	 *  existing Attach button. */
	export let alwaysShow: boolean = false;

	const dispatch = createEventDispatcher<{ capture: { file: File } }>();

	let available = false;
	let isTouchSurface = false;
	let isNarrowViewport = false;
	let inputEl: HTMLInputElement | null = null;
	let captureId: string | null = null;
	let narrowMql: MediaQueryList | null = null;

	onMount(() => {
		available = isCameraCaptureSupported();
		if (!browser) return;
		isTouchSurface =
			(typeof navigator !== 'undefined' && (navigator.maxTouchPoints ?? 0) > 0)
			|| window.matchMedia('(pointer: coarse)').matches;
		// Narrow viewport (e.g. devtools mobile emulation on a desktop
		// mouse pointer) should also surface Camera so the toolbar matches
		// what a real phone shows at the same width. Listen for breakpoint
		// crossings so an orientation flip / window resize re-evaluates
		// without a page reload.
		narrowMql = window.matchMedia('(max-width: 767px)');
		isNarrowViewport = narrowMql.matches;
		const onChange = (event: MediaQueryListEvent): void => {
			isNarrowViewport = event.matches;
		};
		narrowMql.addEventListener('change', onChange);
		return () => narrowMql?.removeEventListener('change', onChange);
	});

	onDestroy(() => {
		narrowMql = null;
	});

	$: shouldRender = available && (alwaysShow || isTouchSurface || isNarrowViewport);

	function handleClick(): void {
		if (!inputEl || disabled) return;
		captureId = `cam-${Date.now()}`;
		void publishMediaEvent(MEDIA_CAPTURE_STARTED, {
			capture_id: captureId,
			channel: 'camera'
		});
		inputEl.click();
	}

	async function handleChange(event: Event): Promise<void> {
		const input = event.currentTarget as HTMLInputElement;
		const files = Array.from(input.files ?? []);
		// Reset so picking the same file twice fires `change` again.
		input.value = '';
		const id = captureId;
		captureId = null;
		if (files.length === 0) {
			if (id) {
				void publishMediaEvent(MEDIA_CAPTURE_CANCELLED, {
					capture_id: id,
					channel: 'camera'
				});
			}
			return;
		}
		const original = files[0];
		try {
			// Normalise to a deterministic filename — the OS-supplied
			// name on iOS is `image.jpg` which is useless for any later
			// audit / debugging.
			const file = makeCapturedImageFile(original, original.type);
			void publishMediaEvent(MEDIA_CAPTURE_COMPLETED, {
				capture_id: id,
				channel: 'camera',
				mime_type: file.type,
				bytes: file.size,
				filename: file.name
			});
			dispatch('capture', { file });
		} catch (error) {
			void publishMediaEvent(MEDIA_CAPTURE_ERROR, {
				capture_id: id,
				channel: 'camera',
				reason: error instanceof Error ? error.message : 'unknown'
			});
		}
	}
</script>

{#if shouldRender}
	<button
		type="button"
		class="capture-btn"
		class:capture-btn--compact={compact}
		on:click={handleClick}
		{disabled}
		title="Take a photo"
		aria-label="Take a photo"
	>
		<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
			<path d="M23 19a2 2 0 0 1-2 2H3a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h4l2-3h6l2 3h4a2 2 0 0 1 2 2z" />
			<circle cx="12" cy="13" r="4" />
		</svg>
	</button>
	<!-- svelte-ignore a11y-no-static-element-interactions -->
	<input
		bind:this={inputEl}
		type="file"
		accept="image/*"
		capture="environment"
		class="capture-btn__input"
		on:change={handleChange}
	/>
{/if}

<style>
	.capture-btn {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 32px;
		height: 32px;
		padding: 0;
		border-radius: 8px;
		border: 1px solid var(--theme-color-border, rgba(0, 0, 0, 0.12));
		background: transparent;
		color: var(--theme-color-foreground-muted, #6b7280);
		cursor: pointer;
		transition:
			color 120ms ease,
			border-color 120ms ease,
			background-color 120ms ease;
	}
	.capture-btn--compact {
		width: 28px;
		height: 28px;
	}
	.capture-btn:hover:not(:disabled) {
		color: var(--theme-color-foreground, #111827);
		border-color: var(--theme-color-foreground-muted, #6b7280);
	}
	.capture-btn:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}
	.capture-btn svg {
		width: 16px;
		height: 16px;
		display: block;
	}
	.capture-btn--compact svg {
		width: 14px;
		height: 14px;
	}
	.capture-btn__input {
		position: absolute;
		width: 1px;
		height: 1px;
		padding: 0;
		margin: -1px;
		overflow: hidden;
		clip: rect(0, 0, 0, 0);
		border: 0;
	}
</style>
