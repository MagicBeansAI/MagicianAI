<script lang="ts">
	const REQUIREMENTS = [
		{
			title: 'A Mac',
			detail: 'Where Magican runs, remembers and works beside you.',
			status: 'macOS 13+'
		},
		{
			title: 'A browser it can drive',
			detail: 'So it can work in the apps and sites you already use.',
			status: 'Connected'
		},
		{
			title: 'A little trust',
			detail: 'Start small. Every plan and action stays visible.',
			status: 'Earned over time'
		}
	] as const;
</script>

<section class="requirements" aria-labelledby="what-it-takes-title">
	<header class="requirements-heading">
		<span>Before you begin</span>
		<h2 id="what-it-takes-title">What it takes.</h2>
		<p>Three things. Nothing more.</p>
	</header>

	<div class="checklist">
		<div class="checklist-heading" aria-hidden="true">
			<span>The pre-flight checklist</span>
			<span>03 requirements</span>
		</div>

		<ol>
			{#each REQUIREMENTS as requirement (requirement.title)}
				<li>
					<span class="check" aria-hidden="true">✓</span>
					<span class="copy">
						<strong>{requirement.title}</strong>
						<span>{requirement.detail}</span>
					</span>
					<span class="requirement-status">{requirement.status}</span>
				</li>
			{/each}
		</ol>
	</div>
</section>

<style>
	.requirements {
		position: relative;
		max-width: 64rem;
		margin: 0 auto;
		padding: clamp(3rem, 8vh, 6rem) clamp(1rem, 4vw, 2rem);
		font-family: var(--lp-font, system-ui, sans-serif);
		color: var(--text-primary);
	}

	.requirements::before {
		content: '';
		position: absolute;
		inset: 16% 12% 4%;
		z-index: 0;
		pointer-events: none;
		background: radial-gradient(
			ellipse at 50% 50%,
			color-mix(in srgb, var(--lp-secondary, var(--accent-secondary)) 9%, transparent),
			transparent 68%
		);
		filter: blur(22px);
	}

	.requirements-heading,
	.checklist {
		position: relative;
		z-index: 1;
	}

	.requirements-heading {
		margin: 0 auto clamp(1.5rem, 4vw, 2.25rem);
		text-align: center;
	}

	.requirements-heading > span,
	.checklist-heading {
		font-family: var(--lp-mono, ui-monospace, monospace);
		font-size: 0.66rem;
		font-weight: 650;
		letter-spacing: 0.13em;
		text-transform: uppercase;
	}

	.requirements-heading > span {
		color: var(--lp-primary, var(--accent-primary));
	}

	.requirements-heading h2 {
		margin: 0.55rem 0 0.5rem;
		font-size: clamp(2.3rem, 5.6vw, 4rem);
		font-weight: 400;
		letter-spacing: -0.03em;
		background: var(
			--landing-title-gradient,
			linear-gradient(100deg, var(--text-primary), var(--lp-primary, var(--accent-primary)))
		);
		-webkit-background-clip: text;
		background-clip: text;
		color: transparent;
		text-wrap: balance;
	}

	.requirements-heading p {
		margin: 0;
		color: var(--landing-subtitle, var(--text-secondary));
		font-size: clamp(0.95rem, 1.8vw, 1.08rem);
	}

	.checklist {
		max-width: 48rem;
		margin: 0 auto;
		overflow: hidden;
		border: 1px solid var(--border-default, var(--landing-header-border));
		border-radius: var(--radius-lg, 16px);
		background: color-mix(in srgb, var(--bg-elevated, var(--landing-bg)) 88%, transparent);
		box-shadow: var(--shadow-md, var(--landing-task-card-shadow));
	}

	.checklist::before {
		content: '';
		position: absolute;
		inset: 0 0 auto;
		height: 2px;
		background: linear-gradient(
			90deg,
			var(--lp-primary, var(--accent-primary)),
			var(--lp-secondary, var(--accent-secondary)),
			transparent 88%
		);
	}

	.checklist-heading {
		display: flex;
		justify-content: space-between;
		gap: 1rem;
		padding: 1rem clamp(1rem, 3vw, 1.5rem);
		border-bottom: 1px solid var(--border-subtle, var(--landing-header-border));
		color: var(--landing-subtitle, var(--text-secondary));
	}

	.checklist-heading span:first-child {
		color: var(--text-primary);
	}

	ol {
		list-style: none;
		margin: 0;
		padding: 0;
	}

	li {
		display: grid;
		grid-template-columns: 2rem minmax(0, 1fr) auto;
		align-items: center;
		gap: clamp(0.65rem, 2vw, 1rem);
		padding: clamp(1rem, 3vw, 1.4rem) clamp(1rem, 3vw, 1.5rem);
	}

	li + li {
		border-top: 1px solid var(--border-subtle, var(--landing-header-border));
	}

	.check {
		display: grid;
		place-items: center;
		width: 1.8rem;
		height: 1.8rem;
		border: 1px solid color-mix(in srgb, var(--lp-primary, var(--accent-primary)) 42%, transparent);
		border-radius: 50%;
		background: color-mix(in srgb, var(--lp-primary, var(--accent-primary)) 11%, transparent);
		color: var(--lp-primary, var(--accent-primary));
		font-size: 0.82rem;
		font-weight: 760;
	}

	.copy {
		display: grid;
		gap: 0.24rem;
		min-width: 0;
	}

	.copy strong {
		font-size: 1rem;
		font-weight: 620;
		letter-spacing: -0.01em;
	}

	.copy > span {
		color: var(--landing-subtitle, var(--text-secondary));
		font-size: 0.88rem;
		line-height: 1.45;
	}

	.requirement-status {
		justify-self: end;
		/* Keep this class distinct from DaisyUI's global `.status` utility, which
		   is an intentionally fixed-size circular presence indicator. */
		width: max-content;
		min-width: max-content;
		border: 1px solid color-mix(in srgb, var(--lp-secondary, var(--accent-secondary)) 28%, transparent);
		border-radius: 999px;
		padding: 0.3rem 0.58rem;
		background: color-mix(in srgb, var(--lp-secondary, var(--accent-secondary)) 8%, transparent);
		color: color-mix(in srgb, var(--lp-secondary, var(--accent-secondary)) 68%, var(--text-primary));
		font-family: var(--lp-mono, ui-monospace, monospace);
		font-size: 0.63rem;
		font-weight: 600;
		letter-spacing: 0.04em;
		text-transform: uppercase;
		white-space: nowrap;
	}

	/* Progressive enhancement: the checklist is complete and legible without
	   scroll timelines. Supporting browsers stamp each check into place as its
	   row arrives; reduced-motion visitors always receive the settled state. */
	@supports (animation-timeline: view()) {
		.check {
			animation: check-arrives linear both;
			animation-timeline: view();
			animation-range: entry 20% entry 55%;
		}

		li:nth-child(2) .check {
			animation-range: entry 30% entry 65%;
		}

		li:nth-child(3) .check {
			animation-range: entry 40% entry 75%;
		}
	}

	@keyframes check-arrives {
		from {
			opacity: 0;
			transform: scale(1.55) rotate(-12deg);
		}
		to {
			opacity: 1;
			transform: scale(1) rotate(0deg);
		}
	}

	@media (max-width: 640px) {
		li {
			grid-template-columns: 1.8rem minmax(0, 1fr);
		}

		.check {
			align-self: start;
		}

		.requirement-status {
			grid-column: 2;
			justify-self: start;
			margin-top: 0.25rem;
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.check {
			animation: none;
		}
	}
</style>
