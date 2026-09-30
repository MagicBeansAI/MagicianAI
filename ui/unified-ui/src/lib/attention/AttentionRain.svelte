<script lang="ts">
	import Icon from '$lib/shared/icons/Icon.svelte';

	export let count = 0;

	type Particle = {
		icon: 'alert' | 'sparkle';
		size: number;
		drift: number;
		duration: number;
		delay: number;
		rotation: number;
	};

	const particles: Particle[] = [
		{ icon: 'alert', size: 23, drift: -8, duration: 4.2, delay: -1.4, rotation: -12 },
		{ icon: 'sparkle', size: 18, drift: 12, duration: 4.8, delay: -3.8, rotation: 16 },
		{ icon: 'alert', size: 20, drift: -20, duration: 5.1, delay: -2.3, rotation: -18 },
		{ icon: 'sparkle', size: 17, drift: 20, duration: 4.4, delay: -3.1, rotation: 14 },
		{ icon: 'alert', size: 24, drift: -24, duration: 5.3, delay: -4.6, rotation: 20 },
		{ icon: 'sparkle', size: 19, drift: 7, duration: 4.6, delay: -1.8, rotation: -15 },
		{ icon: 'alert', size: 18, drift: -13, duration: 4.1, delay: -3.4, rotation: 12 }
	];

	$: visibleParticles = particles.slice(0, Math.min(particles.length, Math.max(3, count)));
</script>

{#if count > 0}
	<span class="attention-rain" aria-hidden="true" data-attention-rain>
		{#each visibleParticles as particle, index (`${particle.icon}-${index}`)}
			<span
				class:attention-rain__particle--important={particle.icon === 'sparkle'}
				class="attention-rain__particle"
				style={`--particle-size: ${particle.size}px; --particle-mid-drift: ${Math.round(particle.drift * 0.42)}px; --particle-drift: ${particle.drift}px; --particle-duration: ${particle.duration}s; --particle-delay: ${particle.delay}s; --particle-mid-rotation: ${Math.round(particle.rotation * 0.45)}deg; --particle-rotation: ${particle.rotation}deg;`}
			>
				<Icon name={particle.icon} size={Math.round(particle.size * 0.52)} />
			</span>
		{/each}
	</span>
{/if}

<style>
	.attention-rain {
		position: absolute;
		top: calc(100% - 1px);
		right: -24px;
		z-index: 1;
		display: block;
		width: min(112px, calc(100vw - 12px));
		height: 104px;
		pointer-events: none;
		overflow: hidden;
		contain: layout paint;
	}

	.attention-rain__particle {
		position: absolute;
		top: 0;
		right: 24px;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: var(--particle-size);
		height: var(--particle-size);
		border: 1px solid color-mix(in srgb, var(--color-warning, #d97706) 58%, var(--border-soft));
		border-radius: 50%;
		background: color-mix(in srgb, var(--bg-elevated, #ffffff) 91%, var(--color-warning, #d97706) 9%);
		color: var(--color-warning, #d97706);
		box-shadow:
			0 5px 16px color-mix(in srgb, var(--color-warning, #d97706) 20%, transparent),
			0 1px 0 color-mix(in srgb, white 55%, transparent) inset;
		opacity: 0;
		will-change: transform, opacity;
		animation: attention-fall var(--particle-duration) cubic-bezier(0.36, 0.08, 0.64, 0.96) var(--particle-delay) infinite;
	}

	.attention-rain__particle--important {
		border-color: color-mix(in srgb, var(--accent-primary, #7c3aed) 58%, var(--border-soft));
		background: color-mix(in srgb, var(--bg-elevated, #ffffff) 91%, var(--accent-primary, #7c3aed) 9%);
		color: var(--accent-primary, #7c3aed);
		box-shadow:
			0 5px 16px color-mix(in srgb, var(--accent-primary, #7c3aed) 20%, transparent),
			0 1px 0 color-mix(in srgb, white 55%, transparent) inset;
	}

	@keyframes attention-fall {
		0% {
			opacity: 0;
			transform: translate3d(0, -8px, 0) rotate(0deg) scale(0.72);
		}
		10% {
			opacity: 0.92;
		}
		45% {
			transform: translate3d(var(--particle-mid-drift), 42px, 0)
				rotate(var(--particle-mid-rotation)) scale(1);
		}
		75% {
			opacity: 0.66;
		}
		100% {
			opacity: 0;
			transform: translate3d(var(--particle-drift), 94px, 0)
				rotate(var(--particle-rotation)) scale(0.84);
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.attention-rain {
			display: none;
		}
	}
</style>
