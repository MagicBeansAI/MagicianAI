<script lang="ts">
	import { onDestroy, onMount } from 'svelte';
	import { rootPortal } from '$lib/shared/rootPortal';
	import type { ModelFeedback } from '$lib/llm/modelCatalog';

	export let profileName: string | null = null;
	export let profileModel: string | null = null;
	export let feedback: ModelFeedback | null = null;

	type BurstKind = 'pro' | 'instant';

	type Particle = {
		id: number;
		kind: BurstKind;
		left: number;
		dx: number;
		sway: number;
		rise: number;
		delay: number;
		duration: number;
		size: number;
		rotation: number;
	};

	let mounted = false;
	let lastProfileKey = '';
	let nextParticleId = 1;
	let particles: Particle[] = [];
	let burstKind: BurstKind | null = null;
	const cleanupTimers: ReturnType<typeof setTimeout>[] = [];
	let anchor: HTMLSpanElement;

	function floatingLayer(node: HTMLElement): { destroy(): void } {
		const portal = rootPortal(node);
		const position = () => {
			const rect = anchor.getBoundingClientRect();
			node.style.left = `${rect.left}px`;
			node.style.top = `${rect.top}px`;
			node.style.width = `${rect.width}px`;
			node.style.height = `${rect.height}px`;
		};
		position();
		const observer = typeof ResizeObserver === 'undefined' ? null : new ResizeObserver(position);
		observer?.observe(anchor);
		const shell = anchor.closest('.composer-shell');
		if (shell) observer?.observe(shell);
		window.addEventListener('resize', position);
		window.addEventListener('scroll', position, true);
		window.visualViewport?.addEventListener('resize', position);
		return { destroy() {
			observer?.disconnect();
			window.removeEventListener('resize', position);
			window.removeEventListener('scroll', position, true);
			window.visualViewport?.removeEventListener('resize', position);
			portal.destroy();
		} };
	}

	$: profileKey = `${profileName ?? ''}::${profileModel ?? ''}`;
	$: burstKind = feedback === 'fast' ? 'instant' : feedback === 'premium' ? 'pro' : null;
	$: if (mounted && profileKey !== lastProfileKey) {
		const previousProfileKey = lastProfileKey;
		lastProfileKey = profileKey;
		if (previousProfileKey && burstKind) {
			emitBurst(burstKind);
		}
	}

	onMount(() => {
		mounted = true;
		lastProfileKey = profileKey;
	});

	onDestroy(() => {
		for (const timer of cleanupTimers) clearTimeout(timer);
	});

	function emitBurst(kind: BurstKind): void {
		const count = kind === 'instant' ? 11 : 13;
		const burst = Array.from({ length: count }, (_, index): Particle => {
			const side = index % 2 === 0 ? 1 : -1;
			return {
				id: nextParticleId++,
				kind,
				left: 44 + Math.random() * 18,
				dx: side * (8 + Math.random() * (kind === 'instant' ? 34 : 28)),
				sway: side * (10 + Math.random() * (kind === 'instant' ? 24 : 18)),
				rise: kind === 'instant'
					? 86 + Math.random() * 66
					: 74 + Math.random() * 58,
				delay: index * (kind === 'instant' ? 24 : 38) + Math.random() * 46,
				duration: kind === 'instant'
					? 760 + Math.random() * 360
					: 1320 + Math.random() * 520,
				size: kind === 'instant'
					? 13 + Math.random() * 9
					: 12 + Math.random() * 8,
				rotation: side * (10 + Math.random() * (kind === 'instant' ? 38 : 24)),
			};
		});

		particles = [...particles, ...burst];
		const timer = setTimeout(() => {
			const ids = new Set(burst.map((particle) => particle.id));
			particles = particles.filter((particle) => !ids.has(particle.id));
		}, kind === 'instant' ? 1600 : 2300);
		cleanupTimers.push(timer);
	}
</script>

<span class="profile-selection-anchor" bind:this={anchor} aria-hidden="true"></span>
{#if particles.length > 0}
<span class="profile-selection-burst" use:floatingLayer aria-hidden="true">
	{#each particles as particle (particle.id)}
		<span
			class="profile-selection-symbol profile-selection-symbol--{particle.kind}"
			style={`left: ${particle.left}%; --dx: ${particle.dx}px; --sway: ${particle.sway}px; --rise: ${particle.rise}px; --delay: ${particle.delay}ms; --duration: ${particle.duration}ms; --size: ${particle.size}px; --rotation-start: ${particle.rotation * -0.35}deg; --rotation-mid: ${particle.rotation * 0.35}deg; --rotation-end: ${particle.rotation}deg;`}
		>{particle.kind === 'instant' ? '⚡︎' : '$'}</span>
	{/each}
</span>
{/if}

<style>
	.profile-selection-anchor {
		position: absolute;
		inset: 0;
		pointer-events: none;
	}

	.profile-selection-burst {
		/* Escape the composer's backdrop-filter stacking context and draw
		   above the model popup (940), without intercepting its controls. */
		position: fixed;
		overflow: visible;
		pointer-events: none;
		z-index: 1100;
	}

	.profile-selection-symbol {
		position: absolute;
		bottom: 64%;
		font-family: var(--font-display, var(--font-primary));
		font-size: var(--size);
		font-weight: 850;
		line-height: 1;
		opacity: 0;
		-webkit-text-stroke: 0.35px color-mix(in srgb, var(--text-primary, #1a1a1a) 28%, transparent);
		transform: translate(-50%, 4px) scale(0.58) rotate(var(--rotation-start));
		animation: profile-selection-float var(--duration) cubic-bezier(0.18, 0.78, 0.24, 1) forwards;
		animation-delay: var(--delay);
		will-change: transform, opacity;
	}

	.profile-selection-symbol--pro {
		color: color-mix(in srgb, var(--accent-primary, #c2502a) 72%, var(--text-primary, #1a1a1a) 28%);
		text-shadow:
			0 1px 0 color-mix(in srgb, var(--bg-elevated, #fff) 54%, transparent),
			0 10px 22px color-mix(in srgb, var(--accent-primary, #c2502a) 28%, transparent),
			0 3px 8px color-mix(in srgb, var(--text-primary, #1a1a1a) 18%, transparent);
	}

	.profile-selection-symbol--instant {
		color: color-mix(in srgb, var(--accent-secondary, var(--accent-primary, #c2502a)) 56%, var(--text-primary, #1a1a1a) 44%);
		text-shadow:
			0 1px 0 color-mix(in srgb, var(--bg-elevated, #fff) 50%, transparent),
			0 0 8px color-mix(in srgb, var(--accent-secondary, var(--accent-primary, #c2502a)) 34%, transparent),
			0 12px 24px color-mix(in srgb, var(--accent-primary, #c2502a) 26%, transparent),
			0 3px 8px color-mix(in srgb, var(--text-primary, #1a1a1a) 22%, transparent);
		animation-timing-function: cubic-bezier(0.1, 0.86, 0.2, 1);
	}

	@keyframes profile-selection-float {
		0% {
			opacity: 0;
			transform: translate(-50%, 5px) scale(0.58) rotate(var(--rotation-start));
		}
		12% {
			opacity: 1;
		}
		48% {
			opacity: 0.94;
			transform:
				translate(calc(-50% + var(--sway)), calc(var(--rise) * -0.48))
				scale(1)
				rotate(var(--rotation-mid));
		}
		100% {
			opacity: 0;
			transform:
				translate(calc(-50% + var(--dx)), calc(var(--rise) * -1))
				scale(0.74)
				rotate(var(--rotation-end));
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.profile-selection-symbol {
			animation-duration: 500ms;
			animation-timing-function: ease-out;
		}
	}
</style>
