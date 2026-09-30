<!--
  Evidence inbox — the trust surface for the work-evidence graph (Phase 1).
  Two linked views:
    • Evidence  — accrued records; suppress / delete (tombstone) / re-label facets.
    • Entities  — canonical anchors; rename / merge / split / suppress / delete.
  Corrections are durable (re-distillation never resurrects them) and reviews
  (/reviews) only ever see `active` records, so a correction here flows straight
  into the next generated review.
-->
<script lang="ts">
	import { onMount } from 'svelte';
	import { loadAgents } from '$lib/stores/agentStore';
	import Badge from '$lib/magician/components/native/Badge.svelte';
	import Button from '$lib/magician/components/native/Button.svelte';
	import Card from '$lib/magician/components/native/Card.svelte';
	import Checkbox from '$lib/magician/components/native/Checkbox.svelte';
	import EmptyState from '$lib/magician/components/native/EmptyState.svelte';
	import Select from '$lib/magician/components/native/Select.svelte';
	import Spinner from '$lib/magician/components/native/Spinner.svelte';
	import ServerPager from '$lib/shared/components/ServerPager.svelte';

	interface AgentOpt {
		agent_id: string;
		name?: string;
		is_primary?: boolean;
		kind?: string;
	}
	interface FacetT {
		label: string;
		confidence: number;
		assigned_by: string;
	}
	interface EvidenceT {
		evidence_id: string;
		summary: string;
		evidence_kind: string;
		observed_actions: string[];
		entity_keys: string[];
		people_keys: string[];
		facets: FacetT[];
		importance: number;
		confidence: number;
		sensitivity: string;
		last_seen_at: string;
		status: 'active' | 'suppressed' | 'deleted';
		used_in?: string[];
	}
	interface EvidenceListResponse {
		error?: string;
		evidence?: EvidenceT[];
		count?: number;
		total_count?: number;
		returned_count?: number;
		limit?: number;
		offset?: number;
		has_more?: boolean;
	}
	interface EntityT {
		entity_key: string;
		entity_type: string;
		canonical_name: string;
		aliases: string[];
		source_refs: string[];
		facets: FacetT[];
		last_seen_at: string;
		status: 'active' | 'suppressed' | 'deleted';
		merged_into: string | null;
	}
	interface SelectOption {
		value: string;
		label: string;
	}
	type SelectChangeEvent = CustomEvent<{ value: string }>;
	type CheckboxChangeEvent = CustomEvent<{ checked: boolean }>;
	type DeleteTarget =
		| { kind: 'evidence'; id: string; title: string; summary: string }
		| { kind: 'entity'; key: string; title: string; summary: string };

	const facetOptions: SelectOption[] = [
		{ value: 'all', label: 'All' },
		{ value: 'work', label: 'Work' },
		{ value: 'personal', label: 'Personal' },
		{ value: 'business', label: 'Business' }
	];
	const EVIDENCE_PAGE_SIZE = 50;

	let view: 'evidence' | 'entities' = 'evidence';
	let agents: AgentOpt[] = [];
	let selectedAgent = '';
	let facetFilter = 'all';
	let error = '';
	let busyId = '';

	let evidence: EvidenceT[] = [];
	let loadingEvidence = false;
	let evidenceLimit = EVIDENCE_PAGE_SIZE;
	let evidenceOffset = 0;
	let evidenceTotal = 0;
	let editingId = '';
	let editFacets: string[] = [];
	let newFacet = '';

	let entities: EntityT[] = [];
	let loadingEntities = false;
	let showMerged = false;
	let mergingKey = '';
	let mergeTarget = '';
	let renameKey = '';
	let renameValue = '';
	let deleteTarget: DeleteTarget | null = null;

	const QUICK_FACETS = ['work', 'personal', 'business'];

	$: activeEntities = entities.filter((e) => e.status === 'active');
	$: agentOptions = agents.map((agent) => ({
		value: agent.agent_id,
		label: agent.name ?? agent.agent_id
	}));
	$: mergeTargetOptions = activeEntities
		.filter((entity) => entity.entity_key !== mergingKey)
		.map((entity) => ({
			value: entity.entity_key,
			label: `${entity.canonical_name} (${entity.entity_key})`
		}));
	$: recordCount = view === 'evidence' ? evidenceTotal : entities.length;
	$: isLoading = loadingEvidence || loadingEntities;
	$: evidencePageStart = evidenceTotal === 0 ? 0 : evidenceOffset + 1;
	$: evidencePageEnd = Math.min(evidenceOffset + evidence.length, evidenceTotal);
	$: evidencePageCount = Math.max(1, Math.ceil(evidenceTotal / Math.max(1, evidenceLimit)));
	$: evidenceCurrentPage =
		evidenceTotal === 0 ? 1 : Math.floor(evidenceOffset / Math.max(1, evidenceLimit)) + 1;

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
		await reload();
	});

	async function reload(): Promise<void> {
		if (view === 'evidence') await loadEvidence();
		else await loadEntities();
	}

	function setView(v: 'evidence' | 'entities'): void {
		view = v;
		if (v === 'evidence') resetEvidencePage();
		reload();
	}

	function handleAgentChange(event: SelectChangeEvent): void {
		selectedAgent = event.detail.value;
		resetEvidencePage();
		reload();
	}

	function handleFacetChange(event: SelectChangeEvent): void {
		facetFilter = event.detail.value;
		resetEvidencePage();
		reload();
	}

	function handleMergeTargetChange(event: SelectChangeEvent): void {
		mergeTarget = event.detail.value;
	}

	function handleShowMergedChange(event: CheckboxChangeEvent): void {
		showMerged = event.detail.checked;
		loadEntities();
	}

	function resetEvidencePage(): void {
		evidenceOffset = 0;
	}

	function setEvidencePage(offset: number): void {
		const nextOffset = Math.max(0, offset);
		if (nextOffset === evidenceOffset || loadingEvidence) return;
		evidenceOffset = nextOffset;
		loadEvidence();
	}

	function setEvidencePageNumber(pageNumber: number): void {
		const safePage = Math.min(evidencePageCount, Math.max(1, Math.floor(pageNumber)));
		setEvidencePage((safePage - 1) * evidenceLimit);
	}

	async function loadEvidence(): Promise<void> {
		if (!selectedAgent) return;
		loadingEvidence = true;
		error = '';
		editingId = '';
		try {
			const requestedOffset = evidenceOffset;
			const params = new URLSearchParams({
				agent: selectedAgent,
				with_usage: 'true',
				limit: String(evidenceLimit),
				offset: String(requestedOffset)
			});
			if (facetFilter !== 'all') params.set('facet', facetFilter);
			const res = await fetch(`/api/magician/v2/evidence?${params.toString()}`);
			const data = (await res.json()) as EvidenceListResponse;
			if (!res.ok) {
				error = data.error ?? 'Failed to load evidence';
				evidence = [];
				evidenceTotal = 0;
			} else {
				const items = data.evidence ?? [];
				const total = data.total_count ?? data.count ?? items.length;
				const serverLimit = data.limit ?? evidenceLimit;
				const serverOffset = data.offset ?? requestedOffset;
				if (items.length === 0 && total > 0 && serverOffset > 0) {
					evidenceOffset = Math.max(0, serverOffset - serverLimit);
					loadingEvidence = false;
					await loadEvidence();
					return;
				}
				evidence = items;
				evidenceTotal = total;
				evidenceLimit = serverLimit;
				evidenceOffset = serverOffset;
			}
		} catch (e) {
			error = `${e}`;
			evidence = [];
			evidenceTotal = 0;
		}
		loadingEvidence = false;
	}

	async function loadEntities(): Promise<void> {
		if (!selectedAgent) return;
		loadingEntities = true;
		error = '';
		mergingKey = '';
		try {
			const params = new URLSearchParams({ agent: selectedAgent });
			if (facetFilter !== 'all') params.set('facet', facetFilter);
			if (showMerged) params.set('include_deleted', 'true');
			const res = await fetch(`/api/magician/v2/entities?${params.toString()}`);
			const data = await res.json();
			if (!res.ok) {
				error = data.error ?? 'Failed to load entities';
				entities = [];
			} else {
				entities = data.entities ?? [];
			}
		} catch (e) {
			error = `${e}`;
			entities = [];
		}
		loadingEntities = false;
	}

	async function correctEvidence(
		id: string,
		action: string,
		facets?: { label: string; confidence?: number }[]
	): Promise<void> {
		busyId = id;
		error = '';
		try {
			const res = await fetch('/api/magician/v2/evidence/correct', {
				method: 'POST',
				headers: { 'Content-Type': 'application/json' },
				body: JSON.stringify({ agent: selectedAgent, evidence_id: id, action, facets })
			});
			const data = await res.json();
			if (!res.ok) error = data.error ?? 'Correction failed';
			else await loadEvidence();
		} catch (e) {
			error = `${e}`;
		}
		busyId = '';
	}

	async function correctEntity(
		key: string,
		action: string,
		extra: { name?: string; target_key?: string } = {}
	): Promise<void> {
		busyId = key;
		error = '';
		try {
			const res = await fetch('/api/magician/v2/entities/correct', {
				method: 'POST',
				headers: { 'Content-Type': 'application/json' },
				body: JSON.stringify({ agent: selectedAgent, entity_key: key, action, ...extra })
			});
			const data = await res.json();
			if (!res.ok) error = data.error ?? 'Correction failed';
			else await loadEntities();
		} catch (e) {
			error = `${e}`;
		}
		busyId = '';
		mergingKey = '';
	}

	function startMerge(item: EntityT): void {
		mergingKey = item.entity_key;
		mergeTarget = '';
	}

	function doMerge(absorbedKey: string): void {
		if (mergeTarget) correctEntity(absorbedKey, 'merge', { target_key: mergeTarget });
	}

	function startRename(item: EntityT): void {
		renameKey = item.entity_key;
		renameValue = item.canonical_name;
	}

	function cancelRename(): void {
		renameKey = '';
		renameValue = '';
	}

	function saveRename(item: EntityT): void {
		const name = renameValue.trim();
		if (name && name !== item.canonical_name) {
			correctEntity(item.entity_key, 'rename', { name });
		}
		cancelRename();
	}

	// Evidence facet editing
	function startEdit(item: EvidenceT): void {
		editingId = item.evidence_id;
		editFacets = item.facets.map((f) => f.label);
		newFacet = '';
	}
	function addFacet(label: string): void {
		const clean = label.trim().toLowerCase();
		if (clean && !editFacets.includes(clean)) editFacets = [...editFacets, clean];
		newFacet = '';
	}
	function removeFacet(label: string): void {
		editFacets = editFacets.filter((f) => f !== label);
	}
	function saveFacets(id: string): void {
		correctEvidence(
			id,
			'set_facets',
			editFacets.map((label) => ({ label, confidence: 1.0 }))
		);
		editingId = '';
	}
	function confirmDeleteEvidence(item: EvidenceT): void {
		deleteTarget = {
			kind: 'evidence',
			id: item.evidence_id,
			title: 'Delete evidence',
			summary: item.summary
		};
	}
	function confirmDeleteEntity(item: EntityT): void {
		deleteTarget = {
			kind: 'entity',
			key: item.entity_key,
			title: 'Delete entity',
			summary: item.canonical_name
		};
	}

	async function runDelete(): Promise<void> {
		if (!deleteTarget) return;
		const target = deleteTarget;
		deleteTarget = null;
		if (target.kind === 'evidence') await correctEvidence(target.id, 'delete');
		else await correctEntity(target.key, 'delete');
	}

	function fmtDate(iso: string): string {
		try {
			return new Date(iso).toLocaleDateString(undefined, {
				month: 'short',
				day: 'numeric',
				year: 'numeric'
			});
		} catch {
			return iso;
		}
	}
</script>

<div class="inbox-page">
	<header class="page-head">
		<div>
			<h1>Evidence inbox</h1>
			<p class="subtitle">
				Review what was captured and correct it. Suppressed and deleted records never appear in
				generated <a href="/reviews">reviews</a>, and corrections survive re-distillation.
			</p>
		</div>
		<a class="reviews-link" href="/reviews">Reviews →</a>
	</header>

	<div class="tabs">
		<Button
			label="Evidence"
			variant={view === 'evidence' ? 'primary' : 'outline'}
			className="tab-button"
			on:click={() => setView('evidence')}
		/>
		<Button
			label="Entities"
			variant={view === 'entities' ? 'primary' : 'outline'}
			className="tab-button"
			on:click={() => setView('entities')}
		/>
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
		<div class="control control--select">
			<Select
				label="Facet"
				options={facetOptions}
				value={facetFilter}
				interactive={true}
				on:change={handleFacetChange}
			/>
		</div>
		{#if view === 'entities'}
			<div class="check">
				<Checkbox
					label="Show merged / deleted"
					checked={showMerged}
					idBase="evidence-show-merged"
					on:change={handleShowMergedChange}
				/>
			</div>
		{/if}
		<Button
			label={isLoading ? 'Loading…' : 'Refresh'}
			icon="rotate-ccw"
			variant="secondary"
			disabled={isLoading}
			on:click={reload}
		/>
		<span class="count"
			>{recordCount} record{recordCount === 1 ? '' : 's'}</span
		>
		{#if view === 'evidence' && evidenceTotal > evidenceLimit}
			<div class="pager" aria-label="Evidence pagination">
				<ServerPager
					currentPage={evidenceCurrentPage}
					pageCount={evidencePageCount}
					startItem={evidencePageStart}
					endItem={evidencePageEnd}
					totalItems={evidenceTotal}
					loading={loadingEvidence}
					ariaLabel="Evidence pagination"
					on:pagechange={(event) => setEvidencePageNumber(event.detail.page)}
				/>
			</div>
		{/if}
	</section>

	{#if error}
		<p class="error">{error}</p>
	{/if}

	{#if view === 'evidence'}
		{#if loadingEvidence}
			<div class="state-row">
				<Spinner label="Loading evidence" />
			</div>
		{:else if evidence.length === 0}
			<EmptyState
				icon="◇"
				title="No evidence yet"
				description="Evidence accrues as the agent completes work and is distilled from episodes on memory consolidation."
			/>
		{:else}
			<ul class="list">
				{#each evidence as item (item.evidence_id)}
					<li class="card-row">
						<Card
							elevation={1}
							className={['evidence-card', item.status === 'suppressed' ? 'is-muted' : '']
								.filter(Boolean)
								.join(' ')}
						>
							<div class="card-layout">
								<div class="card-main">
									<div class="card-head">
										<span class="kind">{item.evidence_kind}</span>
										{#if item.status === 'suppressed'}<Badge text="suppressed" color="warning" />{/if}
										{#if item.observed_actions.length}<span class="actions"
												>{item.observed_actions.join(' · ')}</span
											>{/if}
									</div>
									<p class="summary">{item.summary}</p>

									{#if editingId === item.evidence_id}
										<div class="facet-editor">
											<div class="chips">
												{#each editFacets as f (f)}
													<span class="chip editable"
														>{f}<Button
															label="Remove facet"
															icon="x"
															iconOnly={true}
															size="sm"
															variant="outline"
															className="chip-remove"
															ariaLabel={`Remove ${f}`}
															on:click={() => removeFacet(f)}
														/></span
													>
												{/each}
											</div>
											<div class="facet-add">
												<input
													class="text-input"
													type="text"
													placeholder="add facet…"
													bind:value={newFacet}
													on:keydown={(e) => e.key === 'Enter' && addFacet(newFacet)}
												/>
												{#each QUICK_FACETS as q (q)}
													{#if !editFacets.includes(q)}
														<Button
															label={`+ ${q}`}
															variant="outline"
															size="sm"
															className="quick-action"
															on:click={() => addFacet(q)}
														/>
													{/if}
												{/each}
											</div>
											<div class="editor-actions">
												<Button
													label="Save facets"
													icon="check"
													size="sm"
													disabled={busyId === item.evidence_id}
													on:click={() => saveFacets(item.evidence_id)}
												/>
												<Button
													label="Cancel"
													icon="x"
													variant="outline"
													size="sm"
													on:click={() => (editingId = '')}
												/>
											</div>
										</div>
									{:else}
										<div class="chips">
											{#if item.facets.length}
												{#each item.facets as f (f.label)}
													<span class="chip" class:user={f.assigned_by === 'user'} title={f.assigned_by}
														>{f.label}</span
													>
												{/each}
											{:else}
												<span class="chip empty">no facets</span>
											{/if}
										</div>
									{/if}

									<div class="meta">
										<span>importance {item.importance.toFixed(2)}</span>
										<span>{item.sensitivity}</span>
										<span>{fmtDate(item.last_seen_at)}</span>
										{#if item.entity_keys.length}<span>{item.entity_keys.join(', ')}</span>{/if}
										{#if item.used_in?.length}<span class="usage"
												>used in {item.used_in.length} review{item.used_in.length === 1 ? '' : 's'}</span
											>{/if}
									</div>
								</div>
								<div class="card-actions">
									{#if item.status === 'suppressed'}
										<Button
											label="Restore"
											icon="rotate-ccw"
											variant="outline"
											size="sm"
											disabled={busyId === item.evidence_id}
											on:click={() => correctEvidence(item.evidence_id, 'unsuppress')}
										/>
									{:else}
										<Button
											label="Suppress"
											icon="archive"
											variant="outline"
											size="sm"
											disabled={busyId === item.evidence_id}
											on:click={() => correctEvidence(item.evidence_id, 'suppress')}
										/>
									{/if}
									<Button
										label="Facets"
										icon="pencil"
										variant="outline"
										size="sm"
										disabled={editingId === item.evidence_id}
										on:click={() => startEdit(item)}
									/>
									<Button
										label="Delete"
										icon="x"
										variant="outline"
										size="sm"
										className="danger-action"
										disabled={busyId === item.evidence_id}
										on:click={() => confirmDeleteEvidence(item)}
									/>
								</div>
							</div>
						</Card>
					</li>
				{/each}
			</ul>
			{#if evidenceTotal > evidenceLimit}
				<div class="pager pager--bottom" aria-label="Evidence pagination">
					<ServerPager
						currentPage={evidenceCurrentPage}
						pageCount={evidencePageCount}
						startItem={evidencePageStart}
						endItem={evidencePageEnd}
						totalItems={evidenceTotal}
						loading={loadingEvidence}
						ariaLabel="Evidence pagination"
						on:pagechange={(event) => setEvidencePageNumber(event.detail.page)}
					/>
				</div>
			{/if}
		{/if}
	{:else if loadingEntities}
		<div class="state-row">
			<Spinner label="Loading entities" />
		</div>
	{:else if entities.length === 0}
		<EmptyState
			icon="◇"
			title="No entities yet"
			description="Anchors are resolved from evidence as it accrues."
		/>
	{:else}
		<ul class="list">
			{#each entities as item (item.entity_key)}
				<li class="card-row">
					<Card
						elevation={1}
						className={['evidence-card', item.status === 'suppressed' || item.merged_into ? 'is-muted' : '']
							.filter(Boolean)
							.join(' ')}
					>
						<div class="card-layout">
							<div class="card-main">
								<div class="card-head">
									<span class="kind">{item.entity_type}</span>
									<strong class="ent-name">{item.canonical_name}</strong>
									{#if item.merged_into}<Badge text={`merged → ${item.merged_into}`} color="warning" />
									{:else if item.status === 'suppressed'}<Badge text="suppressed" color="warning" />
									{:else if item.status === 'deleted'}<Badge text="deleted" color="error" />{/if}
								</div>
								<div class="meta">
									<span class="mono">{item.entity_key}</span>
									<span>{item.source_refs.length} source{item.source_refs.length === 1 ? '' : 's'}</span>
									<span>{fmtDate(item.last_seen_at)}</span>
								</div>
								{#if item.aliases.length}
									<div class="chips">
										{#each item.aliases as a (a)}<span class="chip mono">{a}</span>{/each}
									</div>
								{/if}
								{#if item.facets.length}
									<div class="chips">
										{#each item.facets as f (f.label)}
											<span class="chip" class:user={f.assigned_by === 'user'}>{f.label}</span>
										{/each}
									</div>
								{/if}

								{#if renameKey === item.entity_key}
									<div class="facet-editor inline-editor">
										<input
											class="text-input"
											type="text"
											bind:value={renameValue}
											aria-label="Entity name"
											on:keydown={(event) => event.key === 'Enter' && saveRename(item)}
										/>
										<div class="editor-actions">
											<Button
												label="Save name"
												icon="check"
												size="sm"
												disabled={!renameValue.trim() || busyId === item.entity_key}
												on:click={() => saveRename(item)}
											/>
											<Button
												label="Cancel"
												icon="x"
												variant="outline"
												size="sm"
												on:click={cancelRename}
											/>
										</div>
									</div>
								{/if}

								{#if mergingKey === item.entity_key}
									<div class="facet-editor">
										<div class="facet-add">
											<Select
												label="Merge target"
												options={mergeTargetOptions}
												value={mergeTarget}
												placeholder="merge into…"
												interactive={true}
												disabled={mergeTargetOptions.length === 0}
												on:change={handleMergeTargetChange}
											/>
										</div>
										<div class="editor-actions">
											<Button
												label="Merge"
												icon="git-branch"
												size="sm"
												disabled={!mergeTarget || busyId === item.entity_key}
												on:click={() => doMerge(item.entity_key)}
											/>
											<Button
												label="Cancel"
												icon="x"
												variant="outline"
												size="sm"
												on:click={() => (mergingKey = '')}
											/>
										</div>
									</div>
								{/if}
							</div>
							<div class="card-actions">
								{#if item.merged_into}
									<Button
										label="Split out"
										icon="git-branch"
										variant="outline"
										size="sm"
										disabled={busyId === item.entity_key}
										on:click={() => correctEntity(item.entity_key, 'split')}
									/>
								{:else if item.status !== 'active'}
									<Button
										label="Restore"
										icon="rotate-ccw"
										variant="outline"
										size="sm"
										disabled={busyId === item.entity_key}
										on:click={() => correctEntity(item.entity_key, 'unsuppress')}
									/>
								{:else}
									<Button
										label="Rename"
										icon="pencil"
										variant="outline"
										size="sm"
										on:click={() => startRename(item)}
									/>
									<Button
										label="Merge"
										icon="git-branch"
										variant="outline"
										size="sm"
										disabled={mergingKey === item.entity_key}
										on:click={() => startMerge(item)}
									/>
									<Button
										label="Suppress"
										icon="archive"
										variant="outline"
										size="sm"
										disabled={busyId === item.entity_key}
										on:click={() => correctEntity(item.entity_key, 'suppress')}
									/>
									<Button
										label="Delete"
										icon="x"
										variant="outline"
										size="sm"
										className="danger-action"
										disabled={busyId === item.entity_key}
										on:click={() => confirmDeleteEntity(item)}
									/>
								{/if}
							</div>
						</div>
					</Card>
				</li>
			{/each}
		</ul>
	{/if}

	{#if deleteTarget}
		<div class="confirm-layer" role="presentation">
			<Card elevation={3} className="confirm-dialog">
				<div class="confirm-copy">
					<Badge text="Permanent action" color="error" />
					<h2>{deleteTarget.title}</h2>
					<p>
						This permanently removes
						<strong>{deleteTarget.summary}</strong>
						from the evidence graph.
					</p>
				</div>
				<div class="confirm-actions">
					<Button label="Cancel" icon="x" variant="outline" on:click={() => (deleteTarget = null)} />
					<Button
						label="Delete"
						icon="x"
						className="danger-primary"
						disabled={busyId !== ''}
						on:click={runDelete}
					/>
				</div>
			</Card>
		</div>
	{/if}
</div>

<style>
	.inbox-page {
		--evidence-accent-main: var(--accent-primary);
		--evidence-accent-alt: var(--accent-secondary);
		--evidence-accent-info: var(--status-running);
		--evidence-accent-success: var(--color-success);
		--evidence-accent-warning: var(--color-warning);
		--evidence-accent-error: var(--color-error);
		--button-primary-bg: linear-gradient(
			135deg,
			var(--evidence-accent-main),
			color-mix(in srgb, var(--evidence-accent-main) 62%, var(--evidence-accent-info))
		);
		--button-primary-color: var(--text-on-accent);
		--button-primary-shadow: 0 8px 18px color-mix(in srgb, var(--evidence-accent-main) 22%, transparent);
		--button-primary-shadow-hover: 0 12px 26px color-mix(in srgb, var(--evidence-accent-main) 30%, transparent);
		width: 100%;
		max-width: var(--app-content-max, 1320px);
		margin: 0 auto;
		padding: 1.5rem 1.25rem 3rem;
		font-family: var(--font-primary);
		color: var(--text-primary);
	}

	.page-head {
		display: flex;
		justify-content: space-between;
		align-items: flex-start;
		gap: 1rem;
	}

	.page-head h1 {
		margin: 0;
		font-size: 1.5rem;
		font-weight: 700;
	}

	.subtitle {
		margin: 0.35rem 0 0;
		color: var(--text-secondary);
		font-size: 0.9rem;
		max-width: 64ch;
	}

	.subtitle a,
	.reviews-link {
		color: var(--accent-primary);
		text-decoration: none;
	}

	.reviews-link {
		font-size: 0.85rem;
		font-weight: 600;
		white-space: nowrap;
		padding: 0.4rem 0.6rem;
		border: 1px solid color-mix(in srgb, var(--evidence-accent-main) 24%, var(--border-soft));
		border-radius: var(--radius-md, 8px);
		background: color-mix(in srgb, var(--evidence-accent-main) 7%, var(--bg-card));
		transition:
			background 140ms ease,
			border-color 140ms ease;
	}

	.reviews-link:hover {
		border-color: color-mix(in srgb, var(--evidence-accent-main) 46%, var(--border-soft));
		background: color-mix(in srgb, var(--evidence-accent-main) 11%, var(--bg-card));
	}

	.tabs {
		display: flex;
		gap: 0.4rem;
		margin: 1rem 0 0;
		flex-wrap: wrap;
	}

	.controls {
		display: flex;
		flex-wrap: wrap;
		align-items: flex-end;
		gap: 0.9rem;
		margin: 1rem 0 1.25rem;
	}

	.control {
		display: flex;
		flex-direction: column;
		gap: 0.3rem;
		min-width: 180px;
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

	.facet-add :global(.native-select) {
		min-width: min(100%, 24rem);
	}

	.check {
		display: flex;
		align-items: center;
		gap: 0.4rem;
		font-size: 0.82rem;
		color: var(--text-secondary);
		padding-bottom: 0.45rem;
	}

	.count {
		font-size: 0.78rem;
		color: var(--text-muted);
		margin-left: auto;
		padding-bottom: 0.5rem;
	}

	.pager {
		display: inline-flex;
		align-items: center;
		gap: 0.45rem;
		padding-bottom: 0.35rem;
	}

	.pager--bottom {
		justify-content: flex-end;
		margin-top: 0.85rem;
		padding-bottom: 0;
	}

	.error {
		margin: 0 0 1rem;
		padding: 0.65rem 0.85rem;
		border: 1px solid color-mix(in srgb, var(--evidence-accent-error) 30%, var(--border-soft));
		border-radius: var(--radius-md, 8px);
		background: color-mix(in srgb, var(--evidence-accent-error) 9%, var(--bg-card));
		color: var(--evidence-accent-error);
		font-size: 0.85rem;
	}

	.state-row {
		padding: 1rem 0;
	}

	.list {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 0.7rem;
	}

	.card-row {
		min-width: 0;
	}

	:global(.inbox-page .evidence-card) {
		position: relative;
		overflow: hidden;
		padding: 0.9rem 1rem;
		border-color: color-mix(in srgb, var(--evidence-accent-main) 12%, var(--border-soft));
	}

	:global(.inbox-page .evidence-card::before) {
		content: '';
		position: absolute;
		inset: 0 auto 0 0;
		width: 3px;
		background: color-mix(in srgb, var(--evidence-accent-main) 72%, transparent);
	}

	:global(.inbox-page .evidence-card.is-muted) {
		opacity: 0.66;
		background: color-mix(in srgb, var(--bg-card) 82%, var(--bg-soft));
	}

	.card-layout {
		display: flex;
		gap: 1rem;
	}

	.card-main {
		flex: 1;
		min-width: 0;
	}

	.card-head {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		flex-wrap: wrap;
	}

	.kind {
		font-size: 0.7rem;
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.04em;
		color: var(--accent-primary);
	}

	.ent-name {
		font-size: 0.95rem;
	}

	.actions {
		font-size: 0.72rem;
		color: var(--text-muted);
	}

	.summary {
		margin: 0.35rem 0 0.5rem;
		font-size: 0.92rem;
		line-height: 1.4;
	}

	.chips {
		display: flex;
		flex-wrap: wrap;
		gap: 0.35rem;
		margin-top: 0.3rem;
	}

	.chip {
		font-size: 0.72rem;
		padding: 0.1rem 0.5rem;
		border-radius: 999px;
		background: var(--bg-soft);
		color: var(--text-secondary);
		border: 1px solid color-mix(in srgb, var(--border-soft) 72%, transparent);
	}

	.chip.user {
		border-color: var(--accent-primary);
		color: var(--accent-primary);
	}

	.chip.empty {
		opacity: 0.6;
		font-style: italic;
	}

	.chip.mono,
	.mono {
		font-family: var(--font-mono);
		font-size: 0.72rem;
	}

	.chip.editable {
		display: inline-flex;
		align-items: center;
		gap: 0.25rem;
	}

	:global(.inbox-page .chip-remove) {
		width: 1.25rem;
		min-height: 1.25rem;
		border-radius: 999px;
		padding: 0;
	}

	.facet-editor {
		margin: 0.5rem 0;
		padding: 0.6rem;
		border: 1px dashed var(--border-default);
		border-radius: var(--radius-md, 8px);
		background: color-mix(in srgb, var(--bg-soft) 64%, transparent);
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
	}

	.inline-editor {
		max-width: 34rem;
	}

	.facet-add {
		display: flex;
		flex-wrap: wrap;
		gap: 0.4rem;
		align-items: center;
	}

	.text-input {
		min-width: min(100%, 14rem);
		padding: 0.4rem 0.6rem;
		border-radius: var(--radius-md, 8px);
		border: 1px solid var(--input-border);
		background: var(--input-bg);
		color: var(--text-primary);
		font-size: 0.8rem;
		font-family: var(--font-primary);
	}

	.text-input:focus {
		outline: none;
		background: var(--input-focus-bg);
		border-color: var(--input-focus-border);
		box-shadow: var(--input-focus-shadow);
	}

	:global(.inbox-page .quick-action) {
		border-radius: 999px;
	}

	.editor-actions {
		display: flex;
		flex-wrap: wrap;
		gap: 0.5rem;
	}

	.meta {
		display: flex;
		flex-wrap: wrap;
		gap: 0.75rem;
		margin-top: 0.5rem;
		font-size: 0.7rem;
		color: var(--text-muted);
	}

	.meta .usage {
		color: var(--accent-primary);
		font-weight: 600;
	}

	.card-actions {
		display: flex;
		flex-direction: column;
		gap: 0.4rem;
		align-items: stretch;
	}

	.card-actions :global(.native-button) {
		justify-content: flex-start;
	}

	:global(.inbox-page .danger-action) {
		color: var(--evidence-accent-error);
		border-color: color-mix(in srgb, var(--evidence-accent-error) 42%, var(--border-soft));
		background: color-mix(in srgb, var(--evidence-accent-error) 7%, transparent);
	}

	:global(.inbox-page .danger-action:hover:not(:disabled)) {
		border-color: var(--evidence-accent-error);
		background: color-mix(in srgb, var(--evidence-accent-error) 11%, transparent);
	}

	.confirm-layer {
		position: fixed;
		inset: 0;
		z-index: 60;
		display: grid;
		place-items: center;
		padding: 1rem;
		background: color-mix(in srgb, var(--bg-base) 72%, transparent);
		backdrop-filter: blur(6px);
	}

	:global(.inbox-page .confirm-dialog) {
		width: min(100%, 28rem);
		padding: 1rem;
		border-color: color-mix(in srgb, var(--evidence-accent-error) 24%, var(--border-soft));
	}

	.confirm-copy {
		display: flex;
		flex-direction: column;
		gap: 0.55rem;
	}

	.confirm-copy h2 {
		margin: 0;
		font-size: 1rem;
		line-height: 1.25;
	}

	.confirm-copy p {
		margin: 0;
		color: var(--text-secondary);
		font-size: 0.86rem;
		line-height: 1.45;
	}

	.confirm-copy strong {
		color: var(--text-primary);
	}

	.confirm-actions {
		display: flex;
		justify-content: flex-end;
		gap: 0.5rem;
		margin-top: 1rem;
	}

	:global(.inbox-page .danger-primary) {
		background: var(--evidence-accent-error);
		color: var(--text-on-accent);
		box-shadow: 0 8px 18px color-mix(in srgb, var(--evidence-accent-error) 22%, transparent);
	}

	@media (max-width: 640px) {
		.page-head {
			flex-direction: column;
		}

		.count {
			width: 100%;
			margin-left: 0;
			padding-bottom: 0;
		}

		.pager {
			width: 100%;
			flex-wrap: wrap;
			padding-bottom: 0;
		}

		.card-layout {
			flex-direction: column;
		}

		.card-actions {
			flex-direction: row;
			flex-wrap: wrap;
		}

		.card-actions :global(.native-button) {
			flex: 1 1 auto;
		}

		.confirm-actions {
			flex-direction: column-reverse;
		}
	}
</style>
