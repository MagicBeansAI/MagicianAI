<script lang="ts">
	import { onMount } from "svelte";

	export let query: string = "";
	export let isSubmitting: boolean = false;
	export let onSubmit: (title: string, description: string) => void | Promise<void>;

	let title = "";
	let mounted = false;

	onMount(() => {
		// Trigger entrance animations after mount
		setTimeout(() => {
			mounted = true;
		}, 100);
	});

	function handleSubmit() {
		if (title.trim() && query.trim() && !isSubmitting) {
			onSubmit(title, query);
		}
	}

	function handleKeyDown(event: KeyboardEvent) {
		if (event.key === "Enter" && !event.shiftKey) {
			event.preventDefault();
			handleSubmit();
		}
	}
</script>

<div class="landing-content" class:mounted>
	<div class="hero-section">
		<div class="hero-badge animate-item" style="--delay: 0.2s">
			<span class="badge-icon pulse-icon">✨</span>
			<span class="badge-text">AI-Powered Automation</span>
		</div>
		<h1 class="hero-title animate-item" style="--delay: 0.3s">
			<span class="shimmer-text">magican.</span>
		</h1>
		<p class="hero-subtitle animate-item" style="--delay: 0.4s">
			Ask once. It's handled.
		</p>
	</div>

	<div class="query-section animate-item" style="--delay: 0.5s">
		<div class="query-input-wrapper">
			<div class="query-input-glow"></div>
			<div class="query-input-container">
				<div class="input-col">
					<input
						type="text"
						bind:value={title}
						on:keydown={handleKeyDown}
						placeholder='Title — e.g. "Book Tokyo flights"'
						disabled={isSubmitting}
						class="task-input focus-ring"
					/>
					<textarea
						bind:value={query}
						on:keydown={handleKeyDown}
						placeholder='Describe what you want done — e.g. "Find the cheapest round-trip to Tokyo in June, economy class, from SFO"'
						disabled={isSubmitting}
						class="task-input task-description focus-ring"
						rows="4"
					></textarea>
					<button
						on:click={handleSubmit}
						disabled={!title.trim() || !query.trim() || isSubmitting}
						class="add-button btn-press"
						class:submitting={isSubmitting}
						aria-label="Submit task"
					>
						{#if isSubmitting}
							<span class="loading-dots-inline">
								<span></span>
								<span></span>
								<span></span>
							</span>
						{:else}
							<svg
								width="24"
								height="24"
								viewBox="0 0 24 24"
								fill="none"
								stroke="currentColor"
								stroke-width="3"
							>
								<path d="M12 5v14M5 12h14" />
							</svg>
						{/if}
					</button>
				</div>
			</div>
		</div>

		<div class="examples animate-item" style="--delay: 0.6s">
			<p class="examples-title">Starter Tasks</p>
			<div class="example-chips">
				<button
					class="example-chip"
					on:click={() =>
						(query =
							"Monitor my support inbox, summarize daily issues, and post the report to #product on Slack")}
				>
					<span class="chip-icon">📧</span>
					Summarize support to Slack
				</button>
				<button
					class="example-chip"
					on:click={() =>
						(query =
							"Find Kyoto flight deals for next month and message my travel buddy on Whatsapp")}
				>
					<span class="chip-icon">✈️</span>
					Find & notify flight deals
				</button>
				<button
					class="example-chip"
					on:click={() =>
						(query =
							"Make a live dashboard for the latest AI news")}
				>
					<span class="chip-icon">📊</span>
					Make live dashboard for AI news
				</button>
				<button
					class="example-chip"
					on:click={() =>
						(query =
							"Collate my Github tickets, PRDs, Meeting notes, 1:1s for performance appraisal")}
				>
					<span class="chip-icon">📈</span>
					Collate my work for appraisal
				</button>
			</div>
		</div>
	</div>
</div>

<style>
	/* ============================================
	   VIBRANT LANDING CONTENT
	   ============================================ */

	.landing-content {
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		width: 100%;
		max-width: 800px;
		margin: 0 auto;
		position: relative;
		opacity: 0;
		transition: opacity 1s ease;
	}

	.landing-content.mounted {
		opacity: 1;
	}

	/* Hero Section */
	.hero-section {
		text-align: center;
		margin-bottom: 3.5rem;
		width: 100%;
	}

	.hero-badge {
		display: inline-flex;
		align-items: center;
		gap: 0.6rem;
		background: rgba(232, 93, 93, 0.08);
		color: var(--accent-primary, #e85d5d);
		padding: 0.6rem 1.25rem;
		border-radius: 9999px;
		font-size: 0.85rem;
		font-weight: 700;
		margin-bottom: 2rem;
		border: 1px solid rgba(232, 93, 93, 0.15);
		box-shadow: 0 4px 12px rgba(0, 0, 0, 0.03);
		text-transform: uppercase;
		letter-spacing: 0.05em;
	}

	.badge-icon {
		font-size: 1.1rem;
	}

	.hero-title {
		font-size: 6rem;
		font-weight: 900;
		letter-spacing: -0.05em;
		margin-bottom: 1.5rem;
		line-height: 0.9;
		color: var(--text-primary);
	}

	.shimmer-text {
		background: linear-gradient(
			135deg,
			var(--accent-primary, #e85d5d) 0%,
			var(--accent-secondary, #8b7ec8) 40%,
			#ff9a9e 60%,
			var(--accent-primary, #e85d5d) 100%
		);
		background-size: 200% auto;
		-webkit-background-clip: text;
		-webkit-text-fill-color: transparent;
		background-clip: text;
		animation: shimmer 8s linear infinite;
	}

	@keyframes shimmer {
		to {
			background-position: 200% center;
		}
	}

	.hero-subtitle {
		font-size: 1.15rem;
		font-weight: 600;
		color: var(--text-secondary);
		max-width: 400px;
		margin: 0 auto;
		line-height: 1.5;
		opacity: 0.7;
	}

	/* Query Input Section */
	.query-section {
		width: 100%;
		position: relative;
	}

	.query-input-wrapper {
		position: relative;
		margin-bottom: 2.5rem;
	}

	.query-input-glow {
		position: absolute;
		inset: -15px;
		background: radial-gradient(
			circle,
			var(--accent-primary, #e85d5d) 0%,
			transparent 70%
		);
		opacity: 0.05;
		filter: blur(20px);
		z-index: -1;
		transition: opacity 0.5s ease;
	}

	.query-input-wrapper:focus-within .query-input-glow {
		opacity: 0.15;
	}

	.query-input-container {
		background: var(--bg-card, rgba(255, 255, 255, 0.7));
		backdrop-filter: blur(20px);
		border: 1px solid var(--border-soft, rgba(235, 231, 224, 0.3));
		border-radius: 32px;
		padding: 1rem;
		box-shadow: 0 20px 50px rgba(0, 0, 0, 0.1);
		transition: all 0.4s cubic-bezier(0.4, 0, 0.2, 1);
	}

	.query-input-container:focus-within {
		transform: translateY(-4px) scale(1.01);
		border-color: var(--accent-primary, #e85d5d);
		box-shadow: 0 30px 60px rgba(232, 93, 93, 0.15);
	}

	.input-col {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
	}

	.task-input {
		flex: 1;
		padding: 1.25rem 1.5rem;
		background: var(--bg-soft, rgba(243, 240, 234, 0.5));
		border: 1px solid transparent;
		border-radius: 24px;
		font-size: 1.25rem;
		font-family: inherit;
		font-weight: 500;
		color: var(--text-primary);
		transition: all 0.3s ease;
		outline: none;
	}

	.task-input::placeholder {
		color: var(--text-muted);
		opacity: 0.5;
	}

	.task-input:focus {
		background: var(--bg-elevated, #fff);
		box-shadow: inset 0 2px 8px rgba(0, 0, 0, 0.02);
	}

	.task-description {
		resize: vertical;
		min-height: 80px;
		font-size: 1rem;
		padding: 1rem 1.5rem;
		line-height: 1.5;
	}

	.add-button {
		display: flex;
		align-items: center;
		justify-content: center;
		width: 100%;
		height: 48px;
		background: linear-gradient(
			135deg,
			var(--accent-primary, #e85d5d),
			var(--accent-primary-hover, #d04f4f)
		);
		color: white;
		border: none;
		border-radius: 16px;
		cursor: pointer;
		transition: all 0.3s cubic-bezier(0.34, 1.56, 0.64, 1);
		box-shadow: 0 10px 20px rgba(232, 93, 93, 0.3);
		font-weight: 600;
		font-size: 1rem;
	}

	.add-button:hover:not(:disabled) {
		transform: scale(1.1) rotate(5deg);
		box-shadow: 0 15px 30px rgba(232, 93, 93, 0.4);
	}

	.add-button:active:not(:disabled) {
		transform: scale(0.95);
	}

	.add-button:disabled {
		opacity: 0.3;
		cursor: not-allowed;
		transform: none;
	}

	/* Examples Section */
	.examples {
		text-align: center;
		width: 100%;
	}

	.examples-title {
		font-size: 0.9rem;
		font-weight: 700;
		color: var(--text-muted);
		margin-bottom: 1.5rem;
		text-transform: uppercase;
		letter-spacing: 0.1em;
		opacity: 0.6;
	}

	.example-chips {
		display: flex;
		flex-wrap: wrap;
		gap: 1rem;
		justify-content: center;
	}

	.example-chip {
		display: inline-flex;
		align-items: center;
		gap: 0.6rem;
		padding: 0.5rem 1.1rem;
		background: var(--bg-card, rgba(255, 255, 255, 0.6));
		backdrop-filter: blur(10px);
		border: 1px solid var(--border-soft, rgba(235, 231, 224, 0.5));
		border-radius: 100px;
		font-size: 0.825rem;
		font-weight: 600;
		color: var(--text-secondary);
		cursor: pointer;
		transition: all 0.3s ease;
		box-shadow: 0 4px 12px rgba(0, 0, 0, 0.05);
	}

	.example-chip:hover {
		background: var(--bg-elevated, #fff);
		border-color: var(--accent-primary);
		transform: translateY(-3px) scale(1.03);
		box-shadow: 0 10px 20px rgba(0, 0, 0, 0.08);
		color: var(--text-primary);
	}

	.chip-icon {
		font-size: 0.95rem;
	}

	/* Entrance Animations */
	.animate-item {
		opacity: 0;
		transform: translateY(30px) scale(0.95);
		transition: all 0.8s cubic-bezier(0.2, 0.8, 0.2, 1);
		transition-delay: var(--delay, 0s);
	}

	.landing-content.mounted .animate-item {
		opacity: 1;
		transform: translateY(0) scale(1);
	}

	/* Retro 16-bit Theme Overrides */
	:global([data-theme="retro-16bit"]) .query-input-container {
		background: #000;
		border: 2px solid #ffb000;
		border-radius: 0;
		box-shadow: 10px 10px 0px #805800;
	}

	:global([data-theme="retro-16bit"]) .task-input {
		background: #000;
		border: 1px solid #402c00;
		color: #ffb000;
		border-radius: 0;
		font-family: var(--font-mono);
	}

	:global([data-theme="retro-16bit"]) .example-chip {
		background: #000;
		border: 1px solid #ffb000;
		color: #ffcc00;
		border-radius: 0;
		box-shadow: 4px 4px 0px #805800;
		font-family: var(--font-mono);
	}

	:global([data-theme="retro-16bit"]) .example-chip:hover {
		background: #221c00;
		transform: translate(2px, 2px);
		box-shadow: 2px 2px 0px #805800;
	}

	:global([data-theme="retro-16bit"]) .add-button {
		border-radius: 0;
		background: #ffb000;
		color: #000;
		box-shadow: 4px 4px 0px #805800;
	}

	:global([data-theme="retro-16bit"]) .hero-title {
		font-family: var(--font-mono);
		text-transform: uppercase;
	}

	/* Responsive */
	@media (max-width: 768px) {
		.hero-title {
			font-size: 4rem;
		}
		.hero-subtitle {
			font-size: 1.1rem;
		}
		.query-input-container {
			padding: 0.75rem;
			border-radius: 24px;
		}
		.add-button {
			width: 56px;
			height: 56px;
			border-radius: 18px;
		}
		.example-chips {
			flex-direction: column;
			align-items: stretch;
		}
		.example-chip {
			justify-content: center;
		}
	}
</style>
