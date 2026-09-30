<script lang="ts">
	import { createEventDispatcher, onMount } from 'svelte';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import Skeleton from '$lib/shared/components/Skeleton.svelte';
	import {
		deleteObservationSubscription,
		fetchObservableSources,
		fetchObservationSubscriptions,
		putObservationSubscription,
		runObservationSubscription,
		type ObservableSourceOffer,
		type ObservationCadence,
		type ObservationSubscription
	} from './sourceApi';

	type Tab = 'listening' | 'available' | 'setup';
	type Row = ObservableSourceOffer | ObservationSubscription;
	interface PageState {
		items: Row[];
		total: number;
		cursor: string | null;
		nextCursor: string | null;
		history: Array<string | null>;
		loading: boolean;
		loaded: boolean;
		error: string | null;
	}

	const PAGE_SIZE = 5;
	const dispatch = createEventDispatcher<{ countchange: { enabled: number } }>();
	const blank = (): PageState => ({
		items: [],
		total: 0,
		cursor: null,
		nextCursor: null,
		history: [],
		loading: false,
		loaded: false,
		error: null
	});

	let activeTab: Tab = 'listening';
	let pages: Record<Tab, PageState> = {
		listening: blank(),
		available: blank(),
		setup: blank()
	};
	let busyId: string | null = null;
	let customOpen = false;
	let customName = '';
	let customUrl = '';
	let customCadence: ObservationCadence = 'hourly';
	let customIntent = '';
	let customError: string | null = null;
	let enabledTotal = 0;

	$: current = pages[activeTab];
	$: dispatch('countchange', { enabled: enabledTotal });

	function errorText(error: unknown): string {
		return error instanceof Error ? error.message : String(error);
	}

	function errorCode(error: unknown): string | undefined {
		return (error as Error & { code?: string }).code;
	}

	function isStaleMutation(error: unknown): boolean {
		return ['stale_source', 'revision_conflict'].includes(errorCode(error) ?? '');
	}

	async function loadTab(
		tab: Tab,
		cursor: string | null = pages[tab].cursor,
		history: Array<string | null> = pages[tab].history
	): Promise<void> {
		pages = {
			...pages,
			[tab]: { ...pages[tab], cursor, history, loading: true, error: null }
		};
		try {
			const response =
				tab === 'listening'
					? await fetchObservationSubscriptions({ cursor, limit: PAGE_SIZE })
					: await fetchObservableSources({
							readiness: tab === 'available' ? 'eligible' : 'needs_setup',
							subscribed: tab === 'available' ? false : undefined,
							cursor,
							limit: PAGE_SIZE
						});
			pages = {
				...pages,
				[tab]: {
					...pages[tab],
					items: response.items,
					total: response.total,
					cursor,
					nextCursor: response.next_cursor ?? null,
					history,
					loading: false,
					loaded: true,
					error: null
				}
			};
		} catch (error) {
			if (errorCode(error) === 'stale_cursor' && cursor) {
				await loadTab(tab, null, []);
				return;
			}
			pages = {
				...pages,
				[tab]: {
					...pages[tab],
					items: [],
					loading: false,
					loaded: true,
					error: errorText(error)
				}
			};
		}
	}

	async function refreshAll(): Promise<void> {
		await Promise.all([
			loadTab('listening', null, []),
			loadTab('available', null, []),
			loadTab('setup', null, []),
			fetchObservationSubscriptions({ enabled: true, limit: 1 })
				.then((page) => {
					enabledTotal = page.total;
				})
				.catch(() => undefined)
		]);
	}

	function rowKey(row: Row): string {
		return 'subscription_id' in row ? row.subscription_id : row.offer_id;
	}

	function nextPage(): void {
		if (!current.nextCursor || current.loading) return;
		void loadTab(activeTab, current.nextCursor, [...current.history, current.cursor]);
	}

	function previousPage(): void {
		if (!current.history.length || current.loading) return;
		const history = [...current.history];
		const cursor = history.pop() ?? null;
		void loadTab(activeTab, cursor, history);
	}

	async function listen(offer: ObservableSourceOffer): Promise<void> {
		busyId = offer.offer_id;
		try {
			await putObservationSubscription('auto', {
				source_id: offer.source_id,
				profile_id: offer.profile_id,
				source_revision: offer.source_revision,
				enabled: true,
				cadence: offer.default_cadence,
				max_candidates_per_run: offer.limits.max_candidates_per_run,
				max_selected_per_run: offer.limits.max_selected_per_run
			});
			await refreshAll();
			activeTab = 'listening';
		} catch (error) {
			if (errorCode(error) === 'stale_source') {
				await loadTab('available', null, []);
			}
			pages = {
				...pages,
				available: { ...pages.available, error: errorText(error) }
			};
		} finally {
			busyId = null;
		}
	}

	function updateInput(
		subscription: ObservationSubscription,
		field: 'cadence' | 'intent',
		value: string
	) {
		pages = {
			...pages,
			listening: {
				...pages.listening,
				items: pages.listening.items.map((item) => {
					const currentItem = item as ObservationSubscription;
					if (currentItem.subscription_id !== subscription.subscription_id) return currentItem;
					return field === 'cadence'
						? { ...currentItem, cadence: value as ObservationCadence }
						: { ...currentItem, intent: value };
				})
			}
		};
	}

	async function saveSubscription(
		subscription: ObservationSubscription,
		enabled = subscription.enabled
	): Promise<void> {
		busyId = subscription.subscription_id;
		try {
			await putObservationSubscription(subscription.subscription_id, {
				source_id: subscription.custom ? undefined : subscription.source_id,
				profile_id: subscription.custom ? undefined : subscription.profile_id,
				source_revision: subscription.source_revision,
				expected_revision: subscription.revision,
				enabled,
				cadence: subscription.cadence,
				intent: subscription.intent ?? null,
				max_candidates_per_run: subscription.max_candidates_per_run,
				max_selected_per_run: subscription.max_selected_per_run,
				custom_rss: subscription.custom
					? {
							display_name: subscription.display_name,
							feed_url: subscription.targets[0] ?? ''
						}
					: undefined
			});
			await refreshAll();
		} catch (error) {
			pages = {
				...pages,
				listening: { ...pages.listening, error: errorText(error) }
			};
			if (isStaleMutation(error)) {
				await loadTab('listening', null, []);
			}
		} finally {
			busyId = null;
		}
	}

	async function stop(subscription: ObservationSubscription): Promise<void> {
		busyId = subscription.subscription_id;
		try {
			await deleteObservationSubscription(subscription.subscription_id);
			await refreshAll();
		} catch (error) {
			pages = {
				...pages,
				listening: { ...pages.listening, error: errorText(error) }
			};
		} finally {
			busyId = null;
		}
	}

	async function runNow(subscription: ObservationSubscription): Promise<void> {
		busyId = subscription.subscription_id;
		try {
			await runObservationSubscription(subscription.subscription_id);
			await loadTab('listening', pages.listening.cursor, pages.listening.history);
		} catch (error) {
			pages = {
				...pages,
				listening: { ...pages.listening, error: errorText(error) }
			};
			if (isStaleMutation(error)) {
				await loadTab('listening', null, []);
			}
		} finally {
			busyId = null;
		}
	}

	async function addCustomRss(): Promise<void> {
		customError = null;
		busyId = 'custom-rss';
		try {
			await putObservationSubscription('auto', {
				enabled: true,
				cadence: customCadence,
				intent: customIntent || null,
				custom_rss: { display_name: customName, feed_url: customUrl }
			});
			customOpen = false;
			customName = '';
			customUrl = '';
			customIntent = '';
			await refreshAll();
			activeTab = 'listening';
		} catch (error) {
			customError = errorText(error);
		} finally {
			busyId = null;
		}
	}

	function relativeTime(value?: number | null): string {
		if (!value) return 'Not checked yet';
		const seconds = Math.round((value - Date.now()) / 1000);
		const formatter = new Intl.RelativeTimeFormat(undefined, { numeric: 'auto' });
		if (Math.abs(seconds) < 90) return formatter.format(seconds, 'second');
		const minutes = Math.round(seconds / 60);
		if (Math.abs(minutes) < 90) return formatter.format(minutes, 'minute');
		return formatter.format(Math.round(minutes / 60), 'hour');
	}

	function titleCase(value: string): string {
		return value.replaceAll('_', ' ').replace(/\b\w/g, (char) => char.toUpperCase());
	}

	function viaLabel(actionId: string): string {
		const provider = actionId.split('.', 1)[0];
		return provider ? titleCase(provider) : 'Configured source';
	}

	function sourceIcon(actionId: string): 'file-text' | 'eye' {
		return actionId === 'notes.discover' ? 'file-text' : 'eye';
	}

	onMount(() => {
		void refreshAll();
	});
</script>

<section class="source-panel" aria-labelledby="observable-sources-title">
	<header class="source-panel__head">
		<div>
			<span class="source-panel__kicker">Continuous observation</span>
			<h2 id="observable-sources-title">Sources</h2>
		</div>
		<button
			type="button"
			class="icon-button"
			title="Refresh sources"
			aria-label="Refresh sources"
			disabled={Object.values(pages).some((page) => page.loading)}
			on:click={() => void refreshAll()}
		>
			<Icon name="rotate-ccw" size={15} />
		</button>
	</header>

	<div class="source-tabs" role="tablist" aria-label="Observable sources">
		{#each [
			{ id: 'listening', label: 'Listening', count: pages.listening.total },
			{ id: 'available', label: 'Available', count: pages.available.total },
			{ id: 'setup', label: 'Needs setup', count: pages.setup.total }
		] as tab}
			<button
				type="button"
				role="tab"
				class:active={activeTab === tab.id}
				aria-selected={activeTab === tab.id}
				aria-label={`${tab.label} ${tab.count}`}
				on:click={() => (activeTab = tab.id as Tab)}
			>
				{tab.label}<span>{tab.count}</span>
			</button>
		{/each}
	</div>

	<div class="source-panel__body" aria-live="polite" aria-busy={current.loading}>
		{#if current.loading && !current.loaded}
			<div class="source-skeletons">
				<Skeleton variant="card" />
				<Skeleton variant="card" />
				<Skeleton variant="card" />
			</div>
		{:else if current.error}
			<div class="source-state source-state--error">
				<strong>Sources unavailable</strong>
				<span>{current.error}</span>
				<button type="button" on:click={() => void loadTab(activeTab)}>Retry</button>
			</div>
		{:else if activeTab === 'listening'}
			{#if !current.items.length}
				<div class="source-state">
					<strong>No sources are listening</strong>
					<span>Choose Notes, another available source, or add an RSS feed.</span>
				</div>
			{:else}
				<div class="source-rows">
					{#each current.items as row (rowKey(row))}
						{@const subscription = row as ObservationSubscription}
						<article class="source-row">
							<div class="source-row__identity">
								<div class="source-icon"><Icon name={sourceIcon(subscription.action_id)} size={16} /></div>
								<div>
									<div class="source-row__title">
										<strong>{subscription.display_name}</strong>
										<span class:healthy={!subscription.consecutive_failures}>
											{subscription.enabled
												? subscription.consecutive_failures
													? 'Retrying'
													: 'Listening'
												: 'Paused'}
										</span>
									</div>
									<p>
										Via {viaLabel(subscription.action_id)} · Last check {relativeTime(
											subscription.last_success_at_ms
										)}
									</p>
									<small>Next check {relativeTime(subscription.next_run_at_ms)}</small>
								</div>
							</div>
							<div class="source-row__settings">
								<select
									aria-label={`Cadence for ${subscription.display_name}`}
									value={subscription.cadence}
									on:change={(event) =>
										updateInput(
											subscription,
											'cadence',
											(event.currentTarget as HTMLSelectElement).value
										)}
								>
									{#each subscription.supported_cadence as cadence}
										<option value={cadence}>{titleCase(cadence)}</option>
									{/each}
								</select>
								<input
									aria-label={`Interest filter for ${subscription.display_name}`}
									placeholder="Optional interest filter"
									value={subscription.intent ?? ''}
									on:input={(event) =>
										updateInput(
											subscription,
											'intent',
											(event.currentTarget as HTMLInputElement).value
										)}
								/>
							</div>
							<div class="source-row__actions">
								<button
									type="button"
									disabled={busyId === subscription.subscription_id}
									on:click={() => void saveSubscription(subscription)}
								>Save</button>
								<button
									type="button"
									disabled={!subscription.enabled || busyId === subscription.subscription_id}
									on:click={() => void runNow(subscription)}
								>Check now</button>
								<button
									type="button"
									disabled={busyId === subscription.subscription_id}
									on:click={() => void saveSubscription(subscription, !subscription.enabled)}
								>{subscription.enabled ? 'Pause' : 'Resume'}</button>
								<button
									type="button"
									class="danger"
									disabled={busyId === subscription.subscription_id}
									on:click={() => void stop(subscription)}
								>Stop</button>
							</div>
						</article>
					{/each}
				</div>
			{/if}
		{:else if activeTab === 'available'}
			<div class="available-toolbar">
				<span>Each source uses one exact, read-only observation path with no engine fallback.</span>
				<button type="button" on:click={() => (customOpen = !customOpen)}>
					<Icon name="plus" size={14} /> Add RSS feed
				</button>
			</div>
			{#if customOpen}
				<form class="custom-form" on:submit|preventDefault={() => void addCustomRss()}>
					<input required maxlength="160" placeholder="Feed name" bind:value={customName} />
					<input required type="url" placeholder="https://example.com/feed.xml" bind:value={customUrl} />
					<input maxlength="1024" placeholder="Optional interest filter" bind:value={customIntent} />
					<select aria-label="Custom feed cadence" bind:value={customCadence}>
						<option value="hourly">Hourly</option>
						<option value="twice_daily">Twice daily</option>
						<option value="daily">Daily</option>
					</select>
					<button type="submit" disabled={busyId === 'custom-rss'}>Start listening</button>
					{#if customError}<span class="form-error">{customError}</span>{/if}
				</form>
			{/if}
			{#if !current.items.length}
				<div class="source-state">
					<strong>No additional sources are ready</strong>
					<span>All eligible sources may already be listening.</span>
				</div>
			{:else}
				<div class="source-rows">
					{#each current.items as row (rowKey(row))}
						{@const offer = row as ObservableSourceOffer}
						<article class="source-row source-row--offer">
							<div class="source-row__identity">
								<div class="source-icon"><Icon name={sourceIcon(offer.action_bindings[0]?.action_id ?? '')} size={16} /></div>
								<div>
									<div class="source-row__title"><strong>{offer.display_name}</strong></div>
									<p>{offer.description}</p>
									<small>
										{titleCase(offer.category)} · Via {viaLabel(
											offer.action_bindings[0]?.action_id ?? ''
										)}
									</small>
								</div>
							</div>
							<button
								type="button"
								class="primary-action"
								disabled={busyId === offer.offer_id}
								on:click={() => void listen(offer)}
							>Listen</button>
						</article>
					{/each}
				</div>
			{/if}
		{:else}
			{#if !current.items.length}
				<div class="source-state">
					<strong>No sources need setup</strong>
					<span>Profiles missing non-secret configuration will appear here.</span>
				</div>
			{:else}
				<div class="source-rows">
					{#each current.items as row (rowKey(row))}
						{@const offer = row as ObservableSourceOffer}
						<article class="source-row source-row--offer">
							<div class="source-row__identity">
								<div class="source-icon"><Icon name="eye" size={16} /></div>
								<div>
									<div class="source-row__title"><strong>{offer.display_name}</strong></div>
									<p>{offer.description}</p>
									<small>{titleCase(offer.unavailable_reason ?? 'Missing configuration')}</small>
								</div>
							</div>
							<span class="setup-badge">Setup required</span>
						</article>
					{/each}
				</div>
			{/if}
		{/if}
	</div>

	<footer class="source-pager" aria-label="Source pagination">
		<span>
			{current.total === 0
				? '0 items'
				: `${current.history.length * PAGE_SIZE + 1}-${Math.min(
						(current.history.length + 1) * PAGE_SIZE,
						current.total
					)} of ${current.total}`}
		</span>
		<div>
			<button
				type="button"
				title="Previous page"
				aria-label="Previous page"
				disabled={!current.history.length || current.loading}
				on:click={previousPage}
			><Icon name="chevron-left" size={14} /></button>
			<button
				type="button"
				title="Next page"
				aria-label="Next page"
				disabled={!current.nextCursor || current.loading}
				on:click={nextPage}
			><Icon name="chevron-right" size={14} /></button>
		</div>
	</footer>
</section>

<style>
	.source-panel {
		padding: 1rem;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-md, 8px);
		background: var(--bg-card);
		box-shadow: var(--shadow-xs, 0 1px 2px rgb(0 0 0 / 0.04));
	}

	.source-panel__head,
	.source-panel__head > div,
	.source-row,
	.source-row__identity,
	.source-row__title,
	.source-row__actions,
	.available-toolbar,
	.source-pager,
	.source-pager > div {
		display: flex;
		align-items: center;
	}

	.source-panel__head,
	.source-row,
	.available-toolbar,
	.source-pager {
		justify-content: space-between;
	}

	.source-panel__head > div {
		gap: 0.65rem;
	}

	.source-panel__head h2 {
		margin: 0;
		font-size: 1rem;
		letter-spacing: 0;
		color: var(--text-primary);
	}

	.source-panel__kicker {
		font-size: 0.7rem;
		font-weight: 700;
		text-transform: uppercase;
		color: var(--accent-primary);
	}

	.icon-button,
	.source-pager button {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 2rem;
		height: 2rem;
		padding: 0;
		border: 1px solid var(--border-soft);
		border-radius: 6px;
		background: var(--bg-soft);
		color: var(--text-secondary);
		cursor: pointer;
	}

	.source-tabs {
		display: flex;
		gap: 0.25rem;
		overflow-x: auto;
		margin-top: 0.9rem;
		border-bottom: 1px solid var(--border-soft);
		scrollbar-width: thin;
	}

	.source-tabs button {
		display: inline-flex;
		align-items: center;
		gap: 0.4rem;
		flex: 0 0 auto;
		padding: 0.55rem 0.65rem;
		border: 0;
		border-bottom: 2px solid transparent;
		background: transparent;
		color: var(--text-secondary);
		font: inherit;
		font-size: 0.8rem;
		cursor: pointer;
	}

	.source-tabs button.active {
		border-bottom-color: var(--accent-primary);
		color: var(--text-primary);
		font-weight: 650;
	}

	.source-tabs span {
		min-width: 1.25rem;
		padding: 0.08rem 0.32rem;
		border-radius: 999px;
		background: var(--bg-soft);
		color: var(--text-muted);
		font-size: 0.68rem;
		text-align: center;
		font-variant-numeric: tabular-nums;
	}

	.source-panel__body {
		min-height: 17rem;
		padding-top: 0.8rem;
	}

	.source-skeletons,
	.source-rows {
		display: grid;
		gap: 0.55rem;
	}

	.source-row {
		gap: 0.8rem;
		min-height: 4.5rem;
		padding: 0.7rem;
		border: 1px solid var(--border-soft);
		border-radius: 7px;
		background: color-mix(in srgb, var(--bg-card) 92%, var(--bg-soft));
	}

	.source-row__identity {
		min-width: 13rem;
		gap: 0.7rem;
		flex: 1 1 30%;
	}

	.source-icon {
		display: grid;
		place-items: center;
		width: 2rem;
		height: 2rem;
		flex: 0 0 auto;
		border-radius: 6px;
		background: color-mix(in srgb, var(--accent-primary) 12%, var(--bg-soft));
		color: var(--accent-primary);
	}

	.source-row__title {
		gap: 0.45rem;
		color: var(--text-primary);
		font-size: 0.84rem;
		letter-spacing: 0;
	}

	.source-row__title span,
	.setup-badge {
		padding: 0.1rem 0.35rem;
		border-radius: 999px;
		background: color-mix(in srgb, var(--status-warning, #a16207) 12%, transparent);
		color: var(--status-warning, #a16207);
		font-size: 0.66rem;
		font-weight: 650;
	}

	.source-row__title span.healthy {
		background: color-mix(in srgb, var(--status-success, #15803d) 12%, transparent);
		color: var(--status-success, #15803d);
	}

	.source-row p,
	.source-row small {
		margin: 0.18rem 0 0;
		color: var(--text-muted);
		font-size: 0.73rem;
		line-height: 1.35;
	}

	.source-row__settings {
		display: grid;
		grid-template-columns: 8rem minmax(10rem, 1fr);
		gap: 0.45rem;
		flex: 1 1 34%;
	}

	.source-row__settings select,
	.source-row__settings input,
	.custom-form input,
	.custom-form select {
		min-width: 0;
		padding: 0.42rem 0.5rem;
		border: 1px solid var(--border-soft);
		border-radius: 6px;
		background: var(--bg-input, var(--bg-card));
		color: var(--text-primary);
		font: inherit;
		font-size: 0.75rem;
	}

	.source-row__actions {
		justify-content: flex-end;
		gap: 0.3rem;
		flex-wrap: wrap;
		max-width: 19rem;
	}

	.source-row__actions button,
	.source-row--offer > button,
	.available-toolbar button,
	.custom-form button,
	.source-state button {
		display: inline-flex;
		align-items: center;
		gap: 0.3rem;
		padding: 0.38rem 0.55rem;
		border: 1px solid var(--border-soft);
		border-radius: 6px;
		background: var(--bg-soft);
		color: var(--text-secondary);
		font: inherit;
		font-size: 0.72rem;
		cursor: pointer;
		white-space: nowrap;
	}

	.source-row--offer > button.primary-action,
	.custom-form button {
		border-color: var(--accent-primary);
		background: var(--accent-primary);
		color: var(--text-on-accent, white);
	}

	button.danger {
		color: var(--status-error, #b91c1c);
	}

	button:disabled {
		cursor: default;
		opacity: 0.5;
	}

	.available-toolbar {
		gap: 1rem;
		margin-bottom: 0.65rem;
		color: var(--text-muted);
		font-size: 0.74rem;
	}

	.custom-form {
		display: grid;
		grid-template-columns: minmax(8rem, 0.8fr) minmax(14rem, 1.4fr) minmax(10rem, 1fr) 8rem auto;
		gap: 0.45rem;
		margin-bottom: 0.7rem;
		padding: 0.65rem;
		border: 1px solid var(--border-soft);
		border-radius: 7px;
		background: var(--bg-soft);
	}

	.form-error {
		grid-column: 1 / -1;
		color: var(--status-error, #b91c1c);
		font-size: 0.72rem;
	}

	.source-state {
		display: grid;
		place-content: center;
		gap: 0.35rem;
		min-height: 13rem;
		text-align: center;
		color: var(--text-muted);
	}

	.source-state strong {
		color: var(--text-primary);
		font-size: 0.88rem;
	}

	.source-state span {
		font-size: 0.75rem;
	}

	.source-state--error strong,
	.source-state--error span {
		color: var(--status-error, #b91c1c);
	}

	.source-state button {
		justify-self: center;
	}

	.source-pager {
		min-height: 2.2rem;
		padding-top: 0.55rem;
		border-top: 1px solid var(--border-soft);
		color: var(--text-muted);
		font-size: 0.72rem;
		font-variant-numeric: tabular-nums;
	}

	.source-pager > div {
		gap: 0.3rem;
	}

	@media (max-width: 1100px) {
		.source-row {
			align-items: flex-start;
			flex-wrap: wrap;
		}

		.source-row__settings {
			order: 3;
			flex-basis: 100%;
		}

		.custom-form {
			grid-template-columns: 1fr 1fr;
		}
	}
</style>
