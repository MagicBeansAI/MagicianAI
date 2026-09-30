<script lang="ts">
	import {
		launchEmptyInputWidgetAction,
		type AppWidgetGovernedAction,
		type AppWidgetNativeModel,
		type AppWidgetRenderItem,
		type AppWidgetRenderRow
	} from './appWidgets';
	import {
		clearAppActionLaunchIntent,
		getAppActionRunStorage,
		launchIntentInput,
		rememberAppActionRun,
		stageAppActionLaunchIntent
	} from './actionRunHistory';
	import {
		AppRequestError,
		isRetryableAppRequestError,
		type AppActionRun
	} from './appDirectory';
	import {
		getCurrentScopeCredentialRevision,
		scopeCredentialIdentityIsCurrent
	} from '$lib/stores/scopeIdentityStore';
	import AppMiniFrameHost from './AppMiniFrameHost.svelte';
	import {
		miniFrameTargetFromRenderItem,
		type AppMiniFrameTarget,
		type MiniFramePageLease
	} from './appMiniFrame';

	export let widget: AppWidgetRenderItem;
	export let packageRevisionRef: string;
	export let credentialRevision: number;
	export let scopeKey: string;
	/**
	 * The owning page's mini-frame budget (gate S4). Absent on a surface that
	 * has not opened one, which is the same as having no budget: the frame is a
	 * page-bounded escalation, so a widget outside a page lease renders native.
	 */
	export let miniFrameLease: MiniFramePageLease | null = null;

	let busyAction = '';
	let actionError = '';
	let submissionGeneration = 0;
	let appliedAuthority = '';
	// The frame, when one is admitted, *replaces* the native model — the model
	// is the declared fallback, and showing both would render the same records
	// twice. It starts false so the fallback is what a reader sees first.
	let frameMounted = false;
	// Pair the lease with the target so the template narrows both together.
	$: miniFrame = ((): { target: AppMiniFrameTarget; lease: MiniFramePageLease } | null => {
		if (miniFrameLease === null) return null;
		const target = miniFrameTargetFromRenderItem(widget);
		return target === null ? null : { target, lease: miniFrameLease };
	})();
	$: if (miniFrame === null && frameMounted) frameMounted = false;
	$: authorityKey = `${scopeKey}\u0000${credentialRevision}\u0000${packageRevisionRef}\u0000${widget.installation_generation ?? 0}\u0000${widget.revision}`;
	$: if (authorityKey !== appliedAuthority) {
		appliedAuthority = authorityKey;
		submissionGeneration += 1;
		busyAction = '';
		actionError = '';
	}

	function scalar(value: unknown): string {
		if (value === null) return '—';
		if (typeof value === 'string') return value;
		if (typeof value === 'number' || typeof value === 'boolean') return String(value);
		try { return JSON.stringify(value) ?? '—'; } catch { return '—'; }
	}

	function field(row: AppWidgetRenderRow, path?: string): unknown {
		return path === undefined ? undefined : row.fields[path];
	}

	function rowTitle(row: AppWidgetRenderRow, model: AppWidgetNativeModel): string {
		const projected = field(row, model.hints.display_field);
		return projected === undefined ? row.record_id : scalar(projected);
	}

	function secondaryFields(row: AppWidgetRenderRow, model: AppWidgetNativeModel): Array<[string, unknown]> {
		return Object.entries(row.fields)
			.filter(([key]) => key !== model.hints.display_field)
			.slice(0, 4);
	}

	function rows(model: AppWidgetNativeModel): AppWidgetRenderRow[] {
		return model.model === 'detail' ? (model.row ? [model.row] : []) : model.rows;
	}

	async function launch(action: AppWidgetGovernedAction): Promise<void> {
		if (widget.state !== 'ready' || busyAction) return;
		if (credentialRevision !== getCurrentScopeCredentialRevision() ||
			!scopeCredentialIdentityIsCurrent(credentialRevision)) {
			actionError = 'The signed-in Apps scope changed. Wait for this widget to refresh before launching an action.';
			return;
		}
		const submittedScopeKey = scopeKey;
		const submittedPackageRevisionRef = packageRevisionRef;
		const submittedInstallationId = widget.installation_id;
		const submittedInstallationGeneration = widget.installation_generation;
		const submittedAuthorityKey = authorityKey;
		const submittedGeneration = ++submissionGeneration;
		const storage = getAppActionRunStorage(typeof window === 'undefined' ? null : window);
		if (!storage || submittedInstallationGeneration === null) {
			actionError = 'Durable browser storage and a current installation generation are required before launching this action.';
			return;
		}
		busyAction = action.action_id;
		actionError = '';
		try {
			const intent = stageAppActionLaunchIntent(storage, {
				scope_key: submittedScopeKey,
				installation_id: submittedInstallationId,
				installation_generation: submittedInstallationGeneration,
				package_revision_ref: submittedPackageRevisionRef,
				action_id: action.action_id,
				idempotency_key: `widget-action:${crypto.randomUUID()}`,
				input: {}
			});
			if (intent.scope_key !== submittedScopeKey || intent.installation_id !== submittedInstallationId ||
				intent.installation_generation !== submittedInstallationGeneration || intent.package_revision_ref !== submittedPackageRevisionRef ||
				intent.action_id !== action.action_id || Object.keys(launchIntentInput(intent)).length !== 0) {
				throw new Error('A retained action intent does not match this widget installation. Recover it from Apps before retrying here.');
			}
			let result: Awaited<ReturnType<typeof launchEmptyInputWidgetAction>>;
			try {
				result = await launchEmptyInputWidgetAction(
					submittedInstallationId,
					action.action_id,
					intent.idempotency_key,
					submittedInstallationGeneration,
					submittedPackageRevisionRef
				);
			} catch (error) {
				// A deterministic client/stale refusal proves no launch was admitted,
				// so it must not strand an obsolete binding intent. Ambiguous timeout,
				// throttle, server, network, and successful-body parse failures retain it.
				if (error instanceof AppRequestError && error.status >= 400 && error.status < 500 &&
					!isRetryableAppRequestError(error)) {
					clearAppActionLaunchIntent(storage, intent);
				}
				throw error;
			}
			const run: AppActionRun = {
				run_handle: result.run_handle,
				run_ref: result.run_handle.run_ref,
				status: result.result?.status ?? 'queued' as const,
				terminal: result.result !== undefined && result.result.status !== 'waiting',
				result_withheld: false,
				...(result.result ? { result: result.result } : {})
			};
			if (!rememberAppActionRun(storage, submittedScopeKey, run)) {
				throw new Error(`Run ${result.run_handle.run_ref} started, but durable recovery could not be verified. The exact launch intent remains retained.`);
			}
			clearAppActionLaunchIntent(storage, intent);
		} catch (error) {
			if (submissionGeneration === submittedGeneration && authorityKey === submittedAuthorityKey) {
				actionError = error instanceof Error ? error.message : 'The action could not be launched.';
			}
		} finally {
			if (submissionGeneration === submittedGeneration && authorityKey === submittedAuthorityKey) busyAction = '';
		}
	}
</script>

{#if widget.state === 'ready'}
	<article class="app-widget" aria-label={widget.title ?? widget.widget_id}>
		<header class="app-widget__header">
			<h2>{widget.title ?? widget.widget_id}</h2>
			<span>{widget.model.model}</span>
		</header>

		{#if miniFrame}
			<AppMiniFrameHost
				target={miniFrame.target}
				lease={miniFrame.lease}
				{authorityKey}
				on:mounted={() => (frameMounted = true)}
				on:refused={() => (frameMounted = false)}
			/>
		{/if}

		{#if frameMounted}
			<!-- The admitted frame owns the body; header and governed actions stay
			     host-rendered so a control the owner reviewed is never inside it. -->
		{:else if widget.model.model === 'table'}
			{#if widget.model.rows.length > 0}
				<div class="table-scroll">
					<table>
						<thead><tr>{#each widget.model.columns as column}<th>{column}</th>{/each}</tr></thead>
						<tbody>
							{#each widget.model.rows as row (`${row.entity}:${row.record_id}`)}
								<tr>{#each widget.model.columns as column}<td>{scalar(row.fields[column])}</td>{/each}</tr>
							{/each}
						</tbody>
					</table>
				</div>
			{:else}<p class="empty">No items yet.</p>{/if}
		{:else}
			{@const modelRows = rows(widget.model)}
			{#if modelRows.length > 0}
				<div class:timeline={widget.model.model === 'timeline'} class="rows">
					{#each modelRows as row (`${row.entity}:${row.record_id}`)}
						<section class="row">
							<strong>{rowTitle(row, widget.model)}</strong>
							{#if widget.model.hints.status_field && field(row, widget.model.hints.status_field) !== undefined}
								<span class="state">{scalar(field(row, widget.model.hints.status_field))}</span>
							{/if}
							{#if widget.model.hints.timestamp_field && field(row, widget.model.hints.timestamp_field) !== undefined}
								<time>{scalar(field(row, widget.model.hints.timestamp_field))}</time>
							{/if}
							<dl>
								{#each secondaryFields(row, widget.model) as [key, value]}
									<div><dt>{key}</dt><dd>{scalar(value)}</dd></div>
								{/each}
							</dl>
						</section>
					{/each}
				</div>
			{:else}<p class="empty">No items yet.</p>{/if}
		{/if}

		{#if widget.model.actions.length > 0}
			<footer>
				{#each widget.model.actions as action (action.action_id)}
					<button type="button" disabled={busyAction !== ''} on:click={() => void launch(action)}>
						{busyAction === action.action_id ? 'Starting…' : action.label}
					</button>
				{/each}
			</footer>
		{/if}
		{#if actionError}<p class="action-error" role="status">{actionError}</p>{/if}
	</article>
{:else if widget.state === 'unsupported' && widget.fallback.kind === 'message'}
	<article class="app-widget app-widget--message" aria-label={widget.fallback.title}>
		<h2>{widget.fallback.title}</h2>
		<p>{widget.fallback.body}</p>
	</article>
{/if}

<style>
	.app-widget { display: grid; gap: .8rem; min-width: 0; padding: 1rem; border: 1px solid var(--border-soft); border-radius: var(--radius-xl); background: var(--bg-card); color: var(--text-primary); }
	.app-widget__header { display: flex; align-items: baseline; justify-content: space-between; gap: .75rem; }
	h2, p { margin: 0; } h2 { font-size: var(--text-lg); }
	.app-widget__header span { color: var(--text-faint); font-size: var(--text-xs); letter-spacing: .08em; text-transform: uppercase; }
	.rows { display: grid; gap: .55rem; }
	.row { display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: .35rem .65rem; padding: .7rem; border: 1px solid var(--border-soft); border-radius: var(--radius-lg); background: var(--bg-soft); }
	.row strong { overflow-wrap: anywhere; }
	.row time { color: var(--text-secondary); font-size: var(--text-xs); }
	.state { align-self: start; padding: .15rem .45rem; border-radius: var(--radius-full); background: color-mix(in srgb, var(--accent-primary) 14%, transparent); color: var(--accent-primary); font-size: var(--text-xs); }
	dl { grid-column: 1 / -1; display: grid; gap: .22rem; margin: 0; }
	dl div { display: grid; grid-template-columns: minmax(6rem, .35fr) minmax(0, 1fr); gap: .5rem; }
	dt, dd { margin: 0; overflow-wrap: anywhere; font-size: var(--text-sm); }
	dt { color: var(--text-secondary); } dd { color: var(--text-primary); }
	.timeline .row { border-left: 3px solid color-mix(in srgb, var(--accent-primary) 50%, var(--border-soft)); }
	.table-scroll { max-width: 100%; overflow-x: auto; }
	table { width: 100%; border-collapse: collapse; font-size: var(--text-sm); }
	th, td { padding: .5rem; border-bottom: 1px solid var(--border-soft); text-align: left; overflow-wrap: anywhere; }
	th { color: var(--text-secondary); font-size: var(--text-xs); }
	footer { display: flex; flex-wrap: wrap; gap: .45rem; }
	button { padding: .42rem .7rem; border: 1px solid var(--border-soft); border-radius: var(--radius-full); background: var(--bg-soft); color: var(--text-primary); font: inherit; font-size: var(--text-sm); cursor: pointer; }
	button:disabled { cursor: wait; opacity: .65; }
	.empty, .app-widget--message p, .action-error { color: var(--text-secondary); font-size: var(--text-sm); }
	.action-error { color: var(--color-error, #b3261e); }
</style>
