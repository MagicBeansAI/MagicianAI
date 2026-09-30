<script lang="ts">
	import ModelRoutingPanel from '$lib/settings/ModelRoutingPanel.svelte';
	import ProfileCatalogPanel from '$lib/settings/ProfileCatalogPanel.svelte';
</script>

<svelte:head>
	<title>Model routing · Settings</title>
</svelte:head>

<div class="page">
	<header class="page-header">
		<div>
			<a class="back" href="/settings">← Settings</a>
			<p class="overline">Models and engines</p>
			<h1>Model routing</h1>
			<p class="lede">
				Inspect every configured LLM operation, see which model serves it, and apply a live
				per-operation override.
			</p>
		</div>
	</header>

	<section class="explanation" aria-labelledby="routing-order-title">
		<div>
			<p class="overline">Resolution order</p>
			<h2 id="routing-order-title">What happens when an engine changes</h2>
		</div>
		<ol>
			<li><strong>Execution pin</strong><span>A durable run keeps any routing sealed into that execution, even if Settings changes later.</span></li>
			<li><strong>Operation override</strong><span>A profile selected below wins for that operation and applies immediately.</span></li>
			<li><strong>Active harness engine</strong><span>When chat or execution is driven by a harness, eligible operations without an override follow that harness profile. Operations with a local base profile keep their config mapping.</span></li>
			<li><strong>Config mapping</strong><span>Otherwise the router uses the operation's local/cloud and request-shape mapping from <code>llm-router.yaml</code>.</span></li>
		</ol>
		<p class="boundary">
			The main chat or run turn uses the selected harness and its model setting. Secondary
			Magician operations use the matching one-shot harness profile; Codex App Server bridges
			to <code>op-harness-codex</code> because App Server is a stateful engine rather than a
			stateless MagicLLM provider.
		</p>
		<p class="boundary">
			Engine affinity is process-wide in the current runtime. It is not yet propagated as a
			request-scoped parent identity, so simultaneous chat and execution engines cannot each carry
			a different inherited model through their own child calls. Local-base routes remain exempt
			from engine affinity and continue through their locality-aware config mapping.
		</p>
	</section>

	<section class="card">
		<ModelRoutingPanel />
	</section>

	<section class="card">
		<ProfileCatalogPanel />
	</section>
</div>

<style>
	.page {
		width: min(1500px, calc(100% - 2rem));
		margin: 0 auto;
		padding: 1.5rem 0 3rem;
		display: flex;
		flex-direction: column;
		gap: 1rem;
	}
	.page-header {
		display: flex;
		justify-content: space-between;
		align-items: flex-start;
		gap: 1rem;
	}
	.back {
		display: inline-block;
		margin-bottom: 0.85rem;
		color: var(--text-secondary);
		font-size: 0.82rem;
		text-decoration: none;
	}
	.back:hover { color: var(--accent-primary); }
	.overline {
		margin: 0 0 0.25rem;
		color: var(--text-secondary);
		font-size: 0.7rem;
		font-weight: 700;
		letter-spacing: 0.08em;
		text-transform: uppercase;
	}
	h1, h2 { margin: 0; }
	h1 { font-size: clamp(1.65rem, 3vw, 2.35rem); }
	h2 { font-size: 1.05rem; }
	.lede {
		max-width: 72ch;
		margin: 0.45rem 0 0;
		color: var(--text-secondary);
		line-height: 1.5;
	}
	.card, .explanation {
		border: 1px solid var(--border-soft);
		border-radius: 14px;
		background: var(--bg-card);
		padding: 1rem;
	}
	.explanation { display: grid; grid-template-columns: minmax(180px, 0.7fr) minmax(360px, 1.7fr); gap: 1rem 1.5rem; }
	.explanation ol { margin: 0; padding-left: 1.25rem; display: grid; gap: 0.55rem; }
	.explanation li { padding-left: 0.2rem; }
	.explanation li strong { display: block; font-size: 0.82rem; }
	.explanation li span, .boundary { color: var(--text-secondary); font-size: 0.79rem; line-height: 1.45; }
	.boundary { grid-column: 2; margin: 0; padding: 0.65rem 0.75rem; border-radius: 9px; background: var(--bg-soft); }
	code { font-size: 0.76rem; }
	@media (max-width: 720px) {
		.page { width: min(100% - 1rem, 1500px); padding-top: 0.75rem; }
		.explanation { grid-template-columns: 1fr; }
		.boundary { grid-column: 1; }
	}
</style>
