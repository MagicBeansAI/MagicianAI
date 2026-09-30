<!--
  Impact reviews — the work-evidence graph read path (Phase 0).
  Pick an agent, a window (the slider — no fixed cadence), and a facet, then
  generate a grounded impact summary from accrued evidence. Reviews persist as
  durable artifacts (namespace `evidence-reviews`) served by the artifact API.
-->
<script lang="ts">
	import { onMount } from 'svelte';
	import { loadAgents } from '$lib/stores/agentStore';
	import Badge from '$lib/magician/components/native/Badge.svelte';
	import Button from '$lib/magician/components/native/Button.svelte';
	import Card from '$lib/magician/components/native/Card.svelte';
	import EmptyState from '$lib/magician/components/native/EmptyState.svelte';
	import Select from '$lib/magician/components/native/Select.svelte';
	import Spinner from '$lib/magician/components/native/Spinner.svelte';
	import ChatMarkdown from '$lib/magician/components/chat/ChatMarkdown.svelte';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import {
		fetchImpactDashboard,
		fetchImpactReviewArtifact,
		fetchImpactReviewUtility,
		generateImpactReview,
		listImpactReviews,
		publishImpactDashboard,
		recordImpactReviewFeedback,
		reviewRegenerationDefaults,
		type ImpactDashboardData as DashboardData,
		type ImpactReviewEntry as ReviewEntry,
		type ImpactReviewVerification as Verification
	} from '$lib/reviews/api';

	interface AgentOpt {
		agent_id: string;
		name?: string;
		is_primary?: boolean;
		kind?: string;
	}
	interface SelectOption {
		value: string;
		label: string;
	}

	type SelectChangeEvent = CustomEvent<{ value: string }>;

	const facetOptions: SelectOption[] = [
		{ value: 'all', label: 'All' },
		{ value: 'work', label: 'Work' },
		{ value: 'personal', label: 'Personal' },
		{ value: 'business', label: 'Business' }
	];

	let view: 'review' | 'dashboard' = 'review';
	let agents: AgentOpt[] = [];
	let selectedAgent = '';
	let days = 7;
	let facet = 'all';
	let generating = false;
	let review = '';
	let error = '';
	let notice = '';
	let reviews: ReviewEntry[] = [];
	let loadingReviews = false;
	let verification: Verification | null = null;
	let dashboard: DashboardData | null = null;
	let loadingDashboard = false;
	let publishing = false;
	let publishResult: { surface_id: string; route: string } | null = null;
	// Review acceptance feedback → the utility metric (WEG eval depth). The name
	// of the review currently shown (so feedback keys to the right artifact).
	let currentReviewName: string | null = null;
	let feedbackState: 'idle' | 'saving' | 'done' = 'idle';
	let recordedVerdict = '';
	let utilityRate: number | null = null;

	$: selectedAgentName =
		agents.find((agent) => agent.agent_id === selectedAgent)?.name ?? selectedAgent ?? 'Agent';
	$: agentOptions = agents.map((agent) => ({
		value: agent.agent_id,
		label: agent.name ?? agent.agent_id
	}));
	$: staleReviewCount = reviews.filter((entry) => entry.stale).length;
	$: citedEvidenceCount = reviews.reduce((total, entry) => total + (entry.cited_count ?? 0), 0);

	onMount(async () => {
		try {
			const list = (await loadAgents()) as AgentOpt[];
			agents = list ?? [];
			const def =
				agents.find((a) => a.is_primary) ??
				agents.find((a) => a.kind === 'Personal') ??
				agents[0];
			selectedAgent = def?.agent_id ?? '';
		} catch (e) {
			error = `Failed to load agents: ${e}`;
		}
		await loadReviews();
	});

	async function loadReviews(): Promise<void> {
		loadingReviews = true;
		try {
			reviews = await listImpactReviews();
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		}
		loadingReviews = false;
	}

	async function generate(): Promise<void> {
		if (!selectedAgent || generating) return;
		generating = true;
		error = '';
		notice = '';
		review = '';
		verification = null;
		currentReviewName = null;
		try {
			const data = await generateImpactReview({ agent: selectedAgent, days, facet });
			review = data.markdown;
			verification = data.verification;
			currentReviewName = data.artifact_name;
			if (data.evidence_count === 0 || (!currentReviewName && review)) {
				notice = 'No matching evidence was found for this agent, window, and facet, so no review artifact was saved.';
			}
			feedbackState = 'idle';
			await loadReviews();
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		}
		generating = false;
	}

	async function viewReview(name: string): Promise<void> {
		error = '';
		notice = '';
		verification = null;
		try {
			review = await fetchImpactReviewArtifact(name);
			currentReviewName = name;
			feedbackState = 'idle';
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		}
	}

	// Record what the user did with the shown review → the utility metric
	// (`POST /evidence/review/feedback`); then refresh the running rate.
	async function recordFeedback(verdict: 'accepted' | 'edited' | 'discarded'): Promise<void> {
		if (!currentReviewName || feedbackState === 'saving') return;
		feedbackState = 'saving';
		try {
			await recordImpactReviewFeedback(currentReviewName, verdict);
			recordedVerdict = verdict;
			feedbackState = 'done';
			await loadUtility();
		} catch (e) {
			feedbackState = 'idle';
			error = e instanceof Error ? e.message : String(e);
		}
	}

	async function loadUtility(): Promise<void> {
		try {
			utilityRate = await fetchImpactReviewUtility();
		} catch {
			/* non-fatal — the rate just stays hidden */
		}
	}

	// Past reviews are named `<facet>-<days>d-<stamp>.md`; regenerating re-runs
	// the same window/facet (and agent, if the artifact recorded one).
	function regenerate(entry: ReviewEntry): void {
		const defaults = reviewRegenerationDefaults(entry, days);
		if (defaults.facet) facet = defaults.facet;
		days = defaults.days;
		if (defaults.agent) selectedAgent = defaults.agent;
		generate();
	}

	function setView(v: 'review' | 'dashboard'): void {
		view = v;
		if (v === 'dashboard' && !dashboard) loadDashboard();
	}

	function onControlChange(): void {
		if (view === 'dashboard') loadDashboard();
	}

	function handleAgentChange(event: SelectChangeEvent): void {
		selectedAgent = event.detail.value;
		onControlChange();
	}

	function handleFacetChange(event: SelectChangeEvent): void {
		facet = event.detail.value;
		onControlChange();
	}

	async function loadDashboard(): Promise<void> {
		if (!selectedAgent) return;
		loadingDashboard = true;
		error = '';
		notice = '';
		publishResult = null;
		try {
			dashboard = await fetchImpactDashboard({ agent: selectedAgent, facet, days });
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
			dashboard = null;
		}
		loadingDashboard = false;
	}

	async function publishDashboard(): Promise<void> {
		if (!selectedAgent || publishing) return;
		publishing = true;
		error = '';
		notice = '';
		try {
			publishResult = await publishImpactDashboard({ agent: selectedAgent, facet, days });
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		}
		publishing = false;
	}

	function fmtDate(iso: string | null): string {
		if (!iso) return '';
		try {
			return new Date(iso).toLocaleString();
		} catch {
			return iso;
		}
	}
</script>

<div class="reviews-page">
	<header class="reviews-hero">
		<div class="hero-copy">
			<div class="hero-kicker">
				<Icon name="file-text" size={15} />
				<span>Evidence Review</span>
			</div>
			<h1>Impact reviews</h1>
			<p class="subtitle">
				Turn accumulated work evidence into grounded summaries, dashboards, and reusable
				artifacts.
			</p>
		</div>
		<div class="review-stats" aria-label="Review status">
			<div class="review-stat">
				<span class="review-stat-value">{reviews.length}</span>
				<span class="review-stat-label">Reviews</span>
			</div>
			<div class="review-stat">
				<span class="review-stat-value">{staleReviewCount}</span>
				<span class="review-stat-label">Stale</span>
			</div>
			<div class="review-stat">
				<span class="review-stat-value">{citedEvidenceCount}</span>
				<span class="review-stat-label">Citations</span>
			</div>
			<div class="review-stat">
				<span class="review-stat-value"
					>{utilityRate !== null ? `${Math.round(utilityRate * 100)}%` : '—'}</span
				>
				<span class="review-stat-label">Utility</span>
			</div>
		</div>
	</header>

	<Card elevation={1} className="reviews-command-card">
		<div class="mode-tabs" role="tablist" aria-label="Review mode">
			<Button
				variant={view === 'review' ? 'primary' : 'outline'}
				label="Generate review"
				icon="file-text"
				on:click={() => setView('review')}
			/>
			<Button
				variant={view === 'dashboard' ? 'primary' : 'outline'}
				label="Dashboard"
				icon="monitor"
				on:click={() => setView('dashboard')}
			/>
			<a class="evidence-link" href="/evidence">
				<Icon name="inbox" size={14} />
				Evidence inbox
			</a>
		</div>

		<section class="controls">
			<div class="control control--select">
				<Select
					label="Agent"
					options={agentOptions}
					value={selectedAgent}
					placeholder="Select agent"
					interactive={true}
					on:change={handleAgentChange}
				/>
			</div>

			<label class="control control--wide">
				<span class="control-label">Window — <strong>{days}</strong> day{days === 1 ? '' : 's'}</span>
				<input type="range" min="1" max="30" step="1" bind:value={days} on:change={onControlChange} />
			</label>

			<div class="control control--select">
				<Select
					label="Facet"
					options={facetOptions}
					value={facet}
					interactive={true}
					on:change={handleFacetChange}
				/>
			</div>

			{#if view === 'review'}
				<Button
					label={generating ? 'Generating…' : 'Generate review'}
					icon="sparkle"
					disabled={!selectedAgent || generating}
					on:click={generate}
				/>
			{:else}
				<Button
					label={publishing ? 'Publishing…' : 'Publish'}
					icon="arrow-up-right"
					disabled={!selectedAgent || publishing || !dashboard}
					on:click={publishDashboard}
				/>
			{/if}
		</section>
	</Card>

	{#if error}
		<div class="alert alert--error"><Icon name="alert" size={15} /> {error}</div>
	{/if}
	{#if notice}
		<div class="alert alert--notice"><Icon name="info" size={15} /> {notice}</div>
	{/if}

	{#if view === 'review'}
	{#if verification}
		<div class="verdict" class:flagged={!verification.grounded}>
			{#if verification.grounded}
				<Badge text="Grounded" color="success" />
				<span
					>{verification.total_bullets - verification.uncited_bullets}/{verification.total_bullets}
					claim(s) cited · {verification.cited_ids.length} evidence record(s)</span
				>
			{:else}
				<Badge text="Grounding flagged" color="warning" />
				<span
					>{verification.uncited_bullets} uncited claim(s){verification.ungrounded_claims.length
						? `, ${verification.ungrounded_claims.length} unverified`
						: ''} — treat unverified statements with caution.</span
				>
				{#if verification.ungrounded_claims.length}
					<ul class="ungrounded">
						{#each verification.ungrounded_claims as c (c)}<li>{c}</li>{/each}
					</ul>
				{/if}
			{/if}
		</div>
	{/if}

	<div class="body">
		<Card elevation={1} className="review-output-card">
		<section class="review">
			<div class="panel-title">
				<div>
					<h2>{view === 'review' ? selectedAgentName : 'Impact dashboard'}</h2>
					<p>{facet} · {days} day{days === 1 ? '' : 's'}</p>
				</div>
				{#if currentReviewName}<Badge text="artifact saved" color="success" />{/if}
			</div>
			{#if review}
				<ChatMarkdown content={review} />
			{:else if generating}
				<Spinner label="Assembling evidence and writing the review" />
			{:else}
				<EmptyState
					icon="◇"
					title="No review selected"
					description="Generate a review or open a saved artifact from the history panel."
				/>
			{/if}

			{#if review && currentReviewName}
				<div class="feedback">
					{#if feedbackState === 'done'}
						<span class="fb-done">Thanks — recorded as <strong>{recordedVerdict}</strong> ✓</span>
						{#if utilityRate !== null}
							<span class="fb-rate" title="Share of reviews accepted or lightly edited"
								>· utility {Math.round(utilityRate * 100)}%</span
							>
						{/if}
					{:else}
						<span class="fb-q">Was this review useful?</span>
						<Button
							variant="outline"
							size="sm"
							label="Accepted"
							icon="check"
							disabled={feedbackState === 'saving'}
							on:click={() => recordFeedback('accepted')}
						/>
						<Button
							variant="outline"
							size="sm"
							label="Edited"
							icon="pencil"
							disabled={feedbackState === 'saving'}
							on:click={() => recordFeedback('edited')}
							title="Kept after light edits"
						/>
						<Button
							variant="outline"
							size="sm"
							className="danger-action"
							label="Discarded"
							icon="x"
							disabled={feedbackState === 'saving'}
							on:click={() => recordFeedback('discarded')}
							title="Rewrote or threw it away"
						/>
					{/if}
				</div>
			{/if}
		</section>
		</Card>

		<Card elevation={1} className="review-history-card">
		<aside class="history">
			<div class="history-head">
				<h2>Past reviews</h2>
				<Badge text={`${reviews.length}`} color="default" />
			</div>
			{#if loadingReviews}
				<Spinner size="sm" label="Loading reviews" />
			{:else if reviews.length === 0}
				<EmptyState icon="○" title="No saved reviews" description="Generated reviews appear here." />
			{:else}
				<ul>
					{#each reviews as r (r.name)}
						<li>
							<button class="review-link" on:click={() => viewReview(r.name)}>
								<span class="review-name">
									{r.name}
									{#if r.stale}<Badge text="stale" color="warning" />{/if}
								</span>
								<span class="review-meta">{r.agent ?? ''} · {fmtDate(r.last_updated)}</span>
							</button>
							{#if r.stale}
								<Button
									variant="outline"
									size="sm"
									label="Regenerate"
									icon="rotate-ccw"
									on:click={() => regenerate(r)}
								/>
							{/if}
						</li>
					{/each}
				</ul>
			{/if}
		</aside>
		</Card>
	</div>
	{:else}
		{#if publishResult}
			<div class="published">
				<Icon name="check" size={15} />
				Published ✓ —
				<a href={publishResult.route}>{publishResult.route}</a>
				<span class="muted">({publishResult.surface_id})</span>
			</div>
		{/if}
		{#if loadingDashboard}
			<Spinner label="Loading dashboard" />
		{:else if !dashboard}
			<EmptyState icon="◇" title="No dashboard loaded" description="Pick an agent to load the impact dashboard." />
		{:else}
			<div class="dash">
				<div class="metrics">
					<Card elevation={1} className="metric-tile metric-tile--evidence">
						<span class="metric-value">{dashboard.total_evidence}</span>
						<span class="metric-label">Evidence</span>
					</Card>
					<Card elevation={1} className="metric-tile metric-tile--entities">
						<span class="metric-value">{dashboard.active_entities}</span>
						<span class="metric-label">Entities</span>
					</Card>
					<Card elevation={1} className="metric-tile metric-tile--facets">
						<span class="metric-value">{dashboard.facets_tracked}</span>
						<span class="metric-label">Facets</span>
					</Card>
					<Card elevation={1} className="metric-tile metric-tile--reviews">
						<span class="metric-value">{dashboard.reviews_generated}</span>
						<span class="metric-label">Reviews</span>
					</Card>
				</div>

				<div class="dash-grid">
					{#if dashboard.facet_coverage.length}
						<Card elevation={1} className="dash-card">
						<section>
							<h3>Coverage by facet</h3>
							<table>
								<tbody>
									{#each dashboard.facet_coverage as f (f.label)}
										<tr><td>{f.label}</td><td class="num">{f.count}</td></tr>
									{/each}
								</tbody>
							</table>
						</section>
						</Card>
					{/if}
					{#if dashboard.top_entities.length}
							<Card elevation={1} className="dash-card">
							<section>
								<h3>Top entities</h3>
								<table>
									<tbody>
										{#each dashboard.top_entities as e, i (`${e.name}:${e.entity_type}:${i}`)}
											<tr><td>{e.name}</td><td class="dim">{e.entity_type}</td><td class="num">{e.sources}</td></tr>
										{/each}
									</tbody>
								</table>
							</section>
							</Card>
					{/if}
					{#if dashboard.weekly_activity.length}
						<Card elevation={1} className="dash-card">
						<section>
							<h3>Activity by week</h3>
							<table>
								<tbody>
									{#each dashboard.weekly_activity as w (w.label)}
										<tr><td>{w.label}</td><td class="num">{w.count}</td></tr>
									{/each}
								</tbody>
							</table>
						</section>
						</Card>
					{/if}
					{#if dashboard.visibility_gaps.length}
						<Card elevation={1} className="dash-card">
						<section>
							<h3>Visibility gaps</h3>
							<ul class="gaps">
								{#each dashboard.visibility_gaps as g, i (`${g}:${i}`)}<li>{g}</li>{/each}
							</ul>
						</section>
						</Card>
					{/if}
				</div>

				{#if dashboard.recent_evidence.length}
					<Card elevation={1} className="dash-card">
					<section>
						<h3>Recent evidence</h3>
						<ul class="recent">
							{#each dashboard.recent_evidence as r, i (`${r.summary}:${r.when}:${i}`)}
								<li>
									<span class="kind">{r.kind}</span>
									{r.summary}
									<span class="muted">· {r.facets} · {r.when}</span>
								</li>
							{/each}
						</ul>
					</section>
					</Card>
				{/if}
			</div>
		{/if}
	{/if}
</div>

<style>
	.reviews-page {
		--review-accent-main: var(--accent-primary);
		--review-accent-alt: var(--accent-secondary);
		--review-accent-info: var(--status-running);
		--review-accent-success: var(--color-success);
		--review-accent-warning: var(--color-warning);
		--review-accent-error: var(--color-error);
		--button-primary-bg: linear-gradient(
			135deg,
			var(--review-accent-main),
			color-mix(in srgb, var(--review-accent-main) 62%, var(--review-accent-info))
		);
		--button-primary-color: var(--text-on-accent);
		--button-primary-shadow: 0 8px 18px color-mix(in srgb, var(--review-accent-main) 22%, transparent);
		--button-primary-shadow-hover: 0 12px 26px color-mix(in srgb, var(--review-accent-main) 30%, transparent);
		width: 100%;
		max-width: var(--app-content-max, 1320px);
		margin: 0 auto;
		padding: 1.5rem 1.25rem 3rem;
		font-family: var(--font-primary);
		color: var(--text-primary);
	}

	.reviews-hero {
		display: grid;
		grid-template-columns: minmax(0, 1fr) minmax(340px, 0.62fr);
		gap: 1rem;
		align-items: stretch;
		margin-bottom: 1rem;
		padding: 1rem;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-lg, 12px);
		background: var(--bg-card);
		box-shadow: var(--shadow-sm);
	}

	.hero-copy {
		display: flex;
		min-width: 0;
		flex-direction: column;
		justify-content: center;
	}

	.hero-kicker {
		display: inline-flex;
		align-items: center;
		gap: 0.4rem;
		color: var(--accent-primary);
		font-size: 0.72rem;
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.06em;
	}

	.reviews-hero h1 {
		margin: 0.25rem 0;
		font-family: var(--font-display, var(--font-primary));
		font-size: clamp(1.7rem, 2vw, 2.35rem);
		line-height: 1.05;
		letter-spacing: 0;
	}

	.subtitle {
		margin: 0;
		color: var(--text-secondary);
		font-size: 0.92rem;
		line-height: 1.5;
		max-width: 64ch;
	}

	.review-stats {
		display: grid;
		grid-template-columns: repeat(4, minmax(0, 1fr));
		gap: 0.55rem;
	}

	.review-stat {
		--stat-accent: var(--review-accent-main);
		position: relative;
		overflow: hidden;
		display: flex;
		min-width: 0;
		flex-direction: column;
		justify-content: center;
		gap: 0.2rem;
		padding: 0.75rem;
		border: 1px solid color-mix(in srgb, var(--stat-accent) 24%, var(--border-soft));
		border-radius: var(--radius-md, 8px);
		background: color-mix(in srgb, var(--stat-accent) 7%, var(--bg-card));
	}

	.review-stat::before {
		content: '';
		position: absolute;
		inset: 0 auto 0 0;
		width: 3px;
		background: var(--stat-accent);
	}

	.review-stat:nth-child(2) {
		--stat-accent: var(--review-accent-alt);
	}

	.review-stat:nth-child(3) {
		--stat-accent: var(--review-accent-info);
	}

	.review-stat:nth-child(4) {
		--stat-accent: var(--review-accent-success);
	}

	.review-stat-value {
		font-family: var(--font-mono);
		font-size: 1.05rem;
		font-weight: 800;
		line-height: 1;
	}

	.review-stat-label {
		font-family: var(--font-primary);
		color: var(--text-muted);
		font-size: 0.68rem;
		font-weight: 700;
		line-height: 1.15;
		text-transform: uppercase;
		letter-spacing: 0.04em;
	}

	:global(.reviews-page .reviews-command-card) {
		position: relative;
		overflow: hidden;
		padding: 1rem;
		margin-bottom: 1rem;
		border-color: color-mix(in srgb, var(--review-accent-main) 18%, var(--border-soft));
	}

	:global(.reviews-page .reviews-command-card::before) {
		content: '';
		position: absolute;
		inset: 0 0 auto;
		height: 3px;
		background: linear-gradient(
			90deg,
			var(--review-accent-main),
			var(--review-accent-info),
			var(--review-accent-success)
		);
	}

	.mode-tabs {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		flex-wrap: wrap;
		padding-bottom: 0.75rem;
		border-bottom: 1px solid var(--border-soft);
	}

	.evidence-link {
		display: inline-flex;
		align-items: center;
		gap: 0.35rem;
		margin-left: auto;
		color: var(--accent-primary);
		text-decoration: none;
		font-size: 0.82rem;
		font-weight: 700;
		white-space: nowrap;
	}

	.controls {
		display: flex;
		flex-wrap: wrap;
		align-items: flex-end;
		gap: 0.9rem;
		margin-top: 0.9rem;
	}

	.control {
		display: flex;
		flex-direction: column;
		gap: 0.3rem;
		min-width: 150px;
	}

	.control--wide {
		flex: 1;
		min-width: 220px;
	}

	.control--select {
		min-width: 180px;
	}

	.control-label {
		font-size: 0.72rem;
		font-weight: 600;
		letter-spacing: 0.04em;
		text-transform: uppercase;
		color: var(--text-muted);
	}

	.control input[type='range'] {
		width: 100%;
	}

	.control--select :global(.native-select__label) {
		font-size: 0.72rem;
		font-weight: 600;
		letter-spacing: 0.04em;
		text-transform: uppercase;
		color: var(--text-muted);
	}

	.control--select :global(.native-select__input) {
		min-height: 2.1rem;
		background: var(--input-bg);
		color: var(--text-primary);
	}

	.control input[type='range'] {
		accent-color: var(--accent-primary);
	}

	.alert {
		display: flex;
		align-items: flex-start;
		gap: 0.5rem;
		margin-bottom: 1rem;
		padding: 0.65rem 0.85rem;
		border-radius: var(--radius-md, 8px);
		font-size: 0.85rem;
		line-height: 1.4;
	}

	.alert--error {
		color: var(--review-accent-error);
		background: color-mix(in srgb, var(--review-accent-error) 10%, transparent);
		border: 1px solid color-mix(in srgb, var(--review-accent-error) 28%, transparent);
	}

	.alert--notice {
		color: var(--text-secondary);
		background: var(--bg-soft);
		border: 1px solid var(--border-soft);
	}

	.verdict {
		display: flex;
		flex-wrap: wrap;
		align-items: baseline;
		gap: 0.5rem;
		margin: 0 0 1rem;
		padding: 0.6rem 0.9rem;
		border-radius: var(--radius-md, 8px);
		border: 1px solid var(--color-success-soft, var(--border-soft));
		background: var(--color-success-soft, var(--bg-soft));
		font-size: 0.85rem;
		color: var(--text-secondary);
	}

	.verdict.flagged {
		border-color: var(--color-warning-soft, var(--border-soft));
		background: var(--color-warning-soft, var(--bg-soft));
	}

	.ungrounded {
		flex-basis: 100%;
		margin: 0.3rem 0 0;
		padding-left: 1.1rem;
		font-size: 0.8rem;
	}

	.published {
		display: flex;
		align-items: center;
		gap: 0.4rem;
		font-size: 0.88rem;
		padding: 0.55rem 0.8rem;
		margin-bottom: 1rem;
		border-radius: var(--radius-md, 8px);
		border: 1px solid var(--color-success-soft, var(--border-soft));
		background: var(--color-success-soft, var(--bg-soft));
	}

	.published a {
		color: var(--accent-primary);
		font-weight: 600;
	}

	.dash {
		display: flex;
		flex-direction: column;
		gap: 1.1rem;
	}

	.metrics {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(120px, 1fr));
		gap: 0.8rem;
	}

	:global(.reviews-page .metric-tile) {
		--metric-accent: var(--review-accent-main);
		position: relative;
		overflow: hidden;
		display: flex;
		min-height: 4.5rem;
		flex-direction: column;
		justify-content: center;
		gap: 0.25rem;
		padding: 0.85rem 0.95rem;
		border-color: color-mix(in srgb, var(--metric-accent) 20%, var(--border-soft));
		background: color-mix(in srgb, var(--metric-accent) 6%, var(--bg-card));
	}

	:global(.reviews-page .metric-tile::before) {
		content: '';
		position: absolute;
		inset: 0 0 auto;
		height: 3px;
		background: var(--metric-accent);
	}

	:global(.reviews-page .metric-tile--entities) {
		--metric-accent: var(--review-accent-info);
	}

	:global(.reviews-page .metric-tile--facets) {
		--metric-accent: var(--review-accent-success);
	}

	:global(.reviews-page .metric-tile--reviews) {
		--metric-accent: var(--review-accent-alt);
	}

	.metric-value {
		font-family: var(--font-mono);
		font-size: 1.2rem;
		font-weight: 800;
		line-height: 1;
		color: var(--text-primary);
		font-variant-numeric: tabular-nums;
	}

	.metric-label {
		font-size: 0.68rem;
		font-weight: 700;
		line-height: 1.15;
		color: var(--text-muted);
		text-transform: uppercase;
		letter-spacing: 0.04em;
	}

	.dash-grid {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(240px, 1fr));
		gap: 1rem;
		align-items: start;
	}

	:global(.reviews-page .dash-card),
	:global(.reviews-page .review-output-card),
	:global(.reviews-page .review-history-card) {
		padding: 1rem;
	}

	:global(.reviews-page .dash-card) h3 {
		margin: 0 0 0.5rem;
		font-size: 0.82rem;
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.03em;
		color: var(--text-secondary);
	}

	:global(.reviews-page .dash-card) table {
		width: 100%;
		border-collapse: collapse;
		font-size: 0.82rem;
	}

	:global(.reviews-page .dash-card) td {
		padding: 0.2rem 0.3rem;
		border-bottom: 1px solid var(--border-soft);
	}

	:global(.reviews-page .dash-card) td.num {
		text-align: right;
		font-variant-numeric: tabular-nums;
		color: var(--accent-primary);
		font-weight: 600;
	}

	:global(.reviews-page .dash-card) td.dim {
		color: var(--text-muted);
		font-size: 0.74rem;
	}

	.gaps,
	.recent {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 0.4rem;
		font-size: 0.84rem;
	}

	.recent .kind {
		font-size: 0.66rem;
		font-weight: 700;
		text-transform: uppercase;
		color: var(--accent-primary);
		margin-right: 0.3rem;
	}

	.body {
		display: grid;
		grid-template-columns: minmax(0, 1fr) 280px;
		gap: 1.25rem;
		align-items: start;
	}

	.review {
		min-width: 0;
		min-height: 220px;
	}

	.panel-title {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 1rem;
		margin-bottom: 0.9rem;
		padding-bottom: 0.75rem;
		border-bottom: 1px solid var(--border-soft);
	}

	.panel-title h2,
	.history-head h2 {
		margin: 0;
		font-size: 0.95rem;
		font-weight: 800;
		line-height: 1.2;
	}

	.panel-title p {
		margin: 0.2rem 0 0;
		color: var(--text-muted);
		font-size: 0.75rem;
	}

	.feedback {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.5rem;
		margin-top: 1rem;
		padding-top: 0.8rem;
		border-top: 1px solid var(--border-soft);
		font-size: 0.82rem;
		color: var(--text-secondary);
	}

	.fb-q {
		font-weight: 600;
	}

	.fb-done {
		color: var(--color-success, var(--accent-primary));
		font-weight: 600;
	}

	.fb-rate {
		color: var(--text-muted);
	}

	.history {
		position: sticky;
		top: 1rem;
	}

	.history-head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.6rem;
		margin-bottom: 0.75rem;
		padding-bottom: 0.65rem;
		border-bottom: 1px solid var(--border-soft);
	}

	.history ul {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 0.4rem;
	}

	.review-link {
		width: 100%;
		display: flex;
		flex-direction: column;
		gap: 0.15rem;
		padding: 0.55rem 0.65rem;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-md, 8px);
		background: var(--bg-elevated);
		color: var(--text-primary);
		text-align: left;
		cursor: pointer;
	}

	.review-link:hover {
		border-color: var(--accent-primary);
	}

	.review-name {
		display: flex;
		align-items: center;
		gap: 0.35rem;
		font-size: 0.78rem;
		font-weight: 600;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.review-meta {
		font-size: 0.7rem;
		color: var(--text-muted);
	}

	.muted {
		color: var(--text-muted);
		font-size: 0.88rem;
	}

	:global(.reviews-page .danger-action) {
		color: var(--review-accent-error);
		border-color: color-mix(in srgb, var(--review-accent-error) 42%, var(--border-soft));
		background: color-mix(in srgb, var(--review-accent-error) 8%, transparent);
	}

	@media (max-width: 820px) {
		.reviews-hero {
			grid-template-columns: 1fr;
		}

		.review-stats {
			grid-template-columns: repeat(2, minmax(0, 1fr));
		}

		.body {
			grid-template-columns: 1fr;
		}
		.history {
			position: static;
		}
	}

	@media (max-width: 560px) {
		.reviews-page {
			padding: 0.85rem 0.75rem 1.75rem;
		}

		.mode-tabs,
		.controls {
			align-items: stretch;
		}

		.evidence-link {
			margin-left: 0;
		}

		.control,
		.control--wide {
			min-width: 100%;
		}
	}
</style>
