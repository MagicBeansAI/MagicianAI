<!--
  /skills/evolution — Skill graduation observability.

  Surfaces the LearningEvolutionDashboard at its own URL so operators can
  bookmark it, deep-link from notifications about procedure→skill
  promotions, and refresh it independently of the skill catalog at
  /skills. Mirrors the pattern of /llm and /memory: a focused
  observability surface that gets its own route rather than being buried
  in a tab on a management page.

  Why dedicated rather than a tab on /skills:
   - Different audience: /skills is for installing / promoting / demoting
     skills (write actions); /skills/evolution is for watching how the
     catalog evolves over time (read-only metrics).
   - Deep-linkable: alerts and badges that say "a procedure was promoted"
     can link straight here.
   - Refresh semantics differ: catalog reload re-fetches one API; the
     evolution dashboard has its own refresh cadence and chart-bound
     queries.

  See `skills/+page.svelte` for the catalog surface; both pages link to
  each other via header buttons.
-->
<script lang="ts">
	import { goto } from '$app/navigation';
	import LearningEvolutionDashboard from '$lib/magician/learning/LearningEvolutionDashboard.svelte';

	function backToSkills(): void {
		void goto('/skills');
	}
</script>

<svelte:head>
	<title>Skill evolution · Magican</title>
</svelte:head>

<div class="skills-evolution-page">
	<header class="evolution-header">
		<div>
			<h1>Skill evolution</h1>
			<p class="subhead">
				Observability for procedure → skill graduation: candidates, promotions, deprecations, and evidence trail. The skill catalog itself lives at <a href="/skills">Skills</a>.
			</p>
		</div>
		<div class="header-actions">
			<button class="back-btn" type="button" on:click={backToSkills}>
				← Back to Skills
			</button>
		</div>
	</header>

	<LearningEvolutionDashboard />
</div>

<style>
	.skills-evolution-page {
		/* Match the catalog page's content column so the two sibling pages
		   line up identically when navigating between them. */
		width: 100%;
		max-width: var(--app-content-max, 1320px);
		margin: 0 auto;
		padding: 1rem;
		color: var(--text-primary);
	}

	.evolution-header {
		display: flex;
		justify-content: space-between;
		align-items: flex-start;
		gap: 1rem;
		margin-bottom: 1.25rem;
		padding-bottom: 0.85rem;
		border-bottom: 1px solid var(--border-soft);
		flex-wrap: wrap;
	}

	.evolution-header h1 {
		margin: 0;
		font-family: var(--font-display, var(--font-body));
		font-size: 1.5rem;
		font-weight: 600;
		color: var(--text-primary);
	}

	.subhead {
		margin: 0.35rem 0 0;
		font-size: 0.875rem;
		color: var(--text-secondary);
		max-width: 70ch;
	}

	.subhead :global(a) {
		color: var(--accent-primary);
		text-decoration: none;
	}

	.subhead :global(a:hover) {
		text-decoration: underline;
	}

	.header-actions {
		display: flex;
		gap: 0.5rem;
		flex-shrink: 0;
	}

	.back-btn {
		font-family: var(--font-body);
		font-size: 0.85rem;
		font-weight: 500;
		padding: 0.45rem 0.85rem;
		border-radius: 6px;
		border: 1px solid var(--border-default);
		background-color: var(--bg-surface);
		color: var(--text-primary);
		cursor: pointer;
		transition: background-color 120ms ease-out, transform 120ms ease-out;
	}

	.back-btn:hover {
		background-color: var(--bg-elevated, var(--bg-surface));
		transform: translateY(-1px);
	}
</style>
