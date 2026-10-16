<script lang="ts">
	/**
	 * The Branch tab — review and landing for a trusted project's chain.
	 *
	 * A trusted chain works in its own git clone on a `vibedev/…` branch and
	 * every turn ends on a commit (docs/archive/plans/2026-10-07-vibedev-trusted-repo-mode.md).
	 * There is nothing to apply: the commits ARE the change. The owner merges
	 * the branch into its base, opens a pull request, rewinds to an earlier
	 * commit, or discards the chain. Read-only diff; the server owns every git
	 * operation and refuses unsafe ones with a message we show as-is.
	 */
	import { onDestroy } from 'svelte';
	import DiffStrip from '$lib/shell/DiffStrip.svelte';
	import { eventHub } from '$lib/realtime/eventHub';
	import { requestConfirmation } from '$lib/stores/confirmationStore';
	import { showError, showSuccess } from '$lib/shared/stores/notifications';
	import {
		defaultPullRequestBody,
		defaultPullRequestTitle,
		discardTrustedRun,
		fetchTrustedBranch,
		fetchTrustedDisk,
		formatBytes,
		mergeTrustedBranch,
		openTrustedPullRequest,
		rewindTrustedBranch,
		shortSha,
		type TrustedBranchSummary,
		type TrustedDiskUsage
	} from './trustedBranch';

	/** The chain's root task id; `null` before the project's first run. */
	export let rootTaskId: string | null = null;
	/** The project's configured base branch, for the empty state. */
	export let baseBranch: string | null = null;

	const REFRESH_MS = 15_000;

	let summary: TrustedBranchSummary | null = null;
	/** The chain exists but is not trusted (it started before the project was). */
	let notTrusted = false;
	let loading = false;
	let loadError: string | null = null;
	let busy: 'merge' | 'pr' | 'discard' | 'rewind' | null = null;
	let prOpen = false;
	let prTitle = '';
	let prBody = '';
	let loadedFor: string | null = null;
	let generation = 0;
	let disk: TrustedDiskUsage | null = null;
	const DISK_REFRESH_MS = 300_000;

	async function refreshDisk(): Promise<void> {
		const root = rootTaskId;
		if (!root) {
			disk = null;
			return;
		}
		try {
			const next = await fetchTrustedDisk(root);
			if (root === rootTaskId) disk = next;
		} catch {
			/* disk usage is informational; keep the last reading */
		}
	}

	async function refresh(): Promise<void> {
		const root = rootTaskId;
		if (!root) {
			summary = null;
			notTrusted = false;
			loadError = null;
			return;
		}
		const mine = ++generation;
		loading = summary === null;
		try {
			const next = await fetchTrustedBranch(root);
			if (mine !== generation) return;
			summary = next;
			notTrusted = next === null;
			loadError = null;
		} catch (error) {
			if (mine !== generation) return;
			loadError = error instanceof Error ? error.message : String(error);
		} finally {
			if (mine === generation) loading = false;
		}
	}

	$: if (rootTaskId !== loadedFor) {
		loadedFor = rootTaskId;
		summary = null;
		disk = null;
		prOpen = false;
		void refresh();
		void refreshDisk();
	}

	// A turn ending publishes `coding.branch_updated`; refresh then, plus a
	// slow poll for anything the hub missed (a rewind from another tab).
	let refreshTimer: ReturnType<typeof setTimeout> | null = null;
	function scheduleRefresh(): void {
		if (refreshTimer) clearTimeout(refreshTimer);
		refreshTimer = setTimeout(() => void refresh(), 400);
	}
	const unsubscribe = eventHub.subscribe(
		(event) => event.type === 'coding.branch_updated',
		() => {
			scheduleRefresh();
			void refreshDisk();
		},
		() => scheduleRefresh()
	);
	const poll = setInterval(() => void refresh(), REFRESH_MS);
	const diskPoll = setInterval(() => void refreshDisk(), DISK_REFRESH_MS);
	onDestroy(() => {
		unsubscribe();
		clearInterval(poll);
		clearInterval(diskPoll);
		if (refreshTimer) clearTimeout(refreshTimer);
		generation += 1;
	});

	$: commitCount = summary?.commits.length ?? 0;
	$: target = summary?.base_branch ?? null;
	$: canLand = Boolean(summary && commitCount > 0 && !summary.dirty && !summary.merged && busy === null);

	async function act<T>(kind: NonNullable<typeof busy>, run: (root: string) => Promise<T>, done: (result: T) => void): Promise<void> {
		const root = rootTaskId;
		if (!root || busy) return;
		busy = kind;
		try {
			done(await run(root));
			await refresh();
		} catch (error) {
			showError('Branch action refused', error instanceof Error ? error.message : String(error));
		} finally {
			busy = null;
		}
	}

	async function merge(): Promise<void> {
		if (!summary || !target) return;
		const behind = summary.behind_base ?? 0;
		const ok = await requestConfirmation({
			title: `Merge into ${target}?`,
			message:
				behind > 0
					? `${target} has ${behind} commit${behind === 1 ? '' : 's'} this branch does not, so this makes a merge commit. A conflict is refused and nothing changes.`
					: `Fast-forwards ${target} to ${shortSha(summary.head_sha)}. If ${target} is checked out, your working tree follows.`,
			confirmLabel: 'Merge'
		});
		if (!ok) return;
		await act('merge', mergeTrustedBranch, (record) =>
			showSuccess(
				`Merged into ${record.target}`,
				`${record.strategy === 'fast_forward' ? 'Fast-forward' : 'Merge commit'} ${shortSha(record.merged_sha)}`
			)
		);
	}

	function startPullRequest(): void {
		if (!summary) return;
		prTitle = defaultPullRequestTitle(summary);
		prBody = defaultPullRequestBody(summary);
		prOpen = true;
	}

	async function submitPullRequest(): Promise<void> {
		const title = prTitle.trim();
		if (!title) return;
		await act('pr', (root) => openTrustedPullRequest(root, title, prBody), (result) => {
			prOpen = false;
			showSuccess('Pull request opened', result.pull_request_url);
		});
	}

	// The PR route pushes the branch before returning an open PR's link, so
	// this brings a follow-up turn's commits into the pull request.
	async function updatePullRequest(): Promise<void> {
		if (!summary) return;
		// The title is only used when no pull request exists yet.
		const title = defaultPullRequestTitle(summary);
		await act('pr', (root) => openTrustedPullRequest(root, title, ''), (result) => {
			showSuccess('Pull request updated', result.pull_request_url);
		});
	}

	async function rewind(sha: string, subject: string): Promise<void> {
		const ok = await requestConfirmation({
			title: 'Rewind the branch?',
			message: `Moves the branch back to ${shortSha(sha)} (“${subject}”). Later commits leave the branch, and the agent's next turn resumes its conversation as it was at that point.`,
			confirmLabel: 'Rewind',
			destructive: true
		});
		if (!ok) return;
		await act('rewind', (root) => rewindTrustedBranch(root, sha), (result) =>
			showSuccess(
				'Branch rewound',
				`Now at ${shortSha(result.head_sha)}${result.session_resume_queued ? ' · the agent resumes from that turn' : ''}`
			)
		);
	}

	async function discard(): Promise<void> {
		if (!summary) return;
		const ok = await requestConfirmation({
			title: 'Discard this branch?',
			message: summary.merged
				? `Deletes the run's clone and the ${summary.branch} branch. It is already merged, so nothing is lost.`
				: `Deletes the run's clone and the ${summary.branch} branch, including ${commitCount} unmerged commit${commitCount === 1 ? '' : 's'}.`,
			confirmLabel: 'Discard',
			destructive: true
		});
		if (!ok) return;
		await act('discard', (root) => discardTrustedRun(root, true), () =>
			showSuccess('Branch discarded', summary?.branch ?? '')
		);
	}

	function relativeTime(seconds: number): string {
		const minutes = Math.round((Date.now() - seconds * 1000) / 60_000);
		if (minutes < 1) return 'just now';
		if (minutes < 60) return `${minutes}m ago`;
		const hours = Math.round(minutes / 60);
		if (hours < 48) return `${hours}h ago`;
		return `${Math.round(hours / 24)}d ago`;
	}
</script>

<div class="branch-tab">
	{#if summary}
		<div class="branch-tab__bar">
			<div class="branch-tab__ident">
				<code class="branch-tab__branch">{summary.branch}</code>
				<span class="branch-tab__meta">
					from {target ?? 'detached'}@{shortSha(summary.base_sha)} · {commitCount} commit{commitCount === 1 ? '' : 's'}
					{#if (summary.behind_base ?? 0) > 0}
						· <span class="branch-tab__warn">{target} moved on by {summary.behind_base}</span>
					{/if}
					{#if summary.dirty}
						· <span class="branch-tab__warn">turn in progress</span>
					{/if}
				</span>
				{#if disk}
					<span class="branch-tab__meta" title="Clone · shared cargo build dir · free disk">
						clone {formatBytes(disk.workspace_bytes)} · build {formatBytes(disk.build_dir_bytes)} ·
						<span class:branch-tab__warn={disk.free_bytes !== null && disk.free_bytes < 10_000_000_000}
							>{formatBytes(disk.free_bytes)} free</span
						>
					</span>
				{/if}
			</div>
			<div class="branch-tab__actions">
				<button type="button" class="vbtn vbtn--ghost" disabled={busy !== null} on:click={discard}
					>{busy === 'discard' ? 'Discarding…' : 'Discard'}</button
				>
				{#if summary.pull_request_url}
					<a class="vbtn vbtn--ghost" href={summary.pull_request_url} target="_blank" rel="noreferrer">View PR ↗</a>
					<button type="button" class="vbtn vbtn--ghost" disabled={!canLand} on:click={updatePullRequest}
						>{busy === 'pr' ? 'Pushing…' : 'Update PR'}</button
					>
				{:else}
					<button type="button" class="vbtn vbtn--ghost" disabled={!canLand} on:click={startPullRequest}>Open PR</button>
				{/if}
				<button
					type="button"
					class="vbtn vbtn--primary"
					disabled={!canLand || !target}
					title={target ? `Merge ${summary.branch} into ${target}` : 'Started from a detached HEAD — open a PR instead'}
					on:click={merge}>{busy === 'merge' ? 'Merging…' : target ? `Merge into ${target}` : 'Merge'}</button
				>
			</div>
		</div>

		{#if summary.merged}
			<div class="branch-tab__banner">
				Merged into <strong>{summary.merged.target}</strong> as <code>{shortSha(summary.merged.merged_sha)}</code>
				({summary.merged.strategy === 'fast_forward' ? 'fast-forward' : 'merge commit'}).
			</div>
		{/if}

		{#if prOpen}
			<form class="pr-form" on:submit|preventDefault={submitPullRequest}>
				<label class="pr-form__field">
					<span>Title</span>
					<input type="text" bind:value={prTitle} required />
				</label>
				<label class="pr-form__field">
					<span>Description</span>
					<textarea rows="5" bind:value={prBody}></textarea>
				</label>
				<div class="pr-form__actions">
					<span class="pr-form__hint">Pushes {summary.branch} to origin with your own git and gh logins.</span>
					<button type="button" class="vbtn vbtn--ghost" on:click={() => (prOpen = false)}>Cancel</button>
					<button type="submit" class="vbtn vbtn--primary" disabled={!prTitle.trim() || busy !== null}
						>{busy === 'pr' ? 'Opening…' : 'Create pull request'}</button
					>
				</div>
			</form>
		{/if}

		<div class="branch-tab__scroll">
			{#if commitCount === 0}
				<div class="branch-tab__empty">No commits yet. The run's changes appear here as commits on {summary.branch}.</div>
			{:else}
				<h3 class="branch-tab__section">Commits</h3>
				<ol class="commit-list">
					{#each summary.commits as commit, index (commit.sha)}
						<li class="commit-row">
							<div class="commit-row__text">
								<span class="commit-row__subject">{commit.subject}</span>
								<span class="commit-row__meta"><code>{shortSha(commit.sha)}</code> · {commit.author} · {relativeTime(commit.committed_at)}</span>
							</div>
							{#if index > 0}
								<button
									type="button"
									class="vbtn vbtn--ghost"
									disabled={busy !== null || Boolean(summary.merged)}
									on:click={() => rewind(commit.sha, commit.subject)}>Rewind here</button
								>
							{:else}
								<span class="commit-row__head">head</span>
							{/if}
						</li>
					{/each}
				</ol>
				<DiffStrip
					diff={{ files: summary.files }}
					lifecycle="live"
					title={`Changes since ${target ?? 'base'}@${shortSha(summary.base_sha)}`}
					view="split"
					showStats={true}
					allowCopy={true}
					autoExpandFirst={true}
					showLineNumbers={true}
					wrapLongLines={true}
					syntaxHighlight={true}
					wordLevelDiff={true}
					pollIntervalMs={0}
					truncateLinesThreshold={500}
				/>
			{/if}
		</div>
	{:else}
		<div class="branch-tab__empty">
			{#if loading}
				Loading the branch…
			{:else if loadError}
				Could not load the branch: {loadError}
			{:else if notTrusted}
				This conversation started before the project was trusted, so its changes are staged for review in the Diff tab.
			{:else}
				Trusted project: each conversation works on its own git branch, commits as it goes, and nothing reaches
				{baseBranch ?? 'your branch'} until you merge it or open a pull request. Runs start from your last commit;
				uncommitted changes in your checkout are not included.
			{/if}
		</div>
	{/if}
</div>

<style>
	.branch-tab {
		display: flex;
		flex-direction: column;
		min-height: 0;
		height: 100%;
	}
	.branch-tab__bar {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
		padding: 0.6rem 0.8rem;
		border-bottom: 1px solid var(--vibe-border);
	}
	.branch-tab__ident {
		display: flex;
		flex-direction: column;
		gap: 0.15rem;
		min-width: 0;
	}
	.branch-tab__branch {
		font-size: 0.82rem;
		font-weight: 600;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.branch-tab__meta {
		font-size: 0.74rem;
		color: var(--vibe-text-muted);
	}
	.branch-tab__warn {
		color: var(--color-warning, #b7791f);
	}
	.branch-tab__actions {
		display: inline-flex;
		gap: 0.45rem;
		flex-shrink: 0;
	}
	.branch-tab__banner {
		margin: 0.6rem 0.8rem 0;
		padding: 0.55rem 0.75rem;
		border-radius: var(--radius-sm, 10px);
		background: color-mix(in srgb, var(--vibe-success) 12%, transparent);
		color: var(--vibe-text);
		font-size: 0.8rem;
	}
	.branch-tab__scroll {
		flex: 1;
		min-height: 0;
		overflow: auto;
		display: flex;
		flex-direction: column;
		gap: 0.8rem;
		padding: 0.8rem;
	}
	.branch-tab__empty {
		margin: auto;
		max-width: 26rem;
		text-align: center;
		color: var(--vibe-text-muted);
		font-size: 0.88rem;
		line-height: 1.5;
		padding: 2rem 1rem;
	}
	.branch-tab__section {
		margin: 0;
		font-family: var(--font-display, inherit);
		font-size: 0.85rem;
		font-weight: 600;
		color: var(--vibe-text-muted);
	}
	.commit-list {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		border: 1px solid var(--vibe-border);
		border-radius: var(--radius-md, 18px);
		background: var(--vibe-surface);
	}
	.commit-row {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
		padding: 0.55rem 0.8rem;
		border-top: 1px solid var(--vibe-border);
	}
	.commit-row:first-child {
		border-top: none;
	}
	.commit-row__text {
		display: flex;
		flex-direction: column;
		min-width: 0;
	}
	.commit-row__subject {
		font-weight: 600;
		font-size: 0.85rem;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.commit-row__meta {
		font-size: 0.72rem;
		color: var(--vibe-text-muted);
	}
	.commit-row__head {
		font-size: 0.7rem;
		font-weight: 700;
		color: var(--vibe-accent);
	}
	.pr-form {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
		margin: 0.6rem 0.8rem 0;
		padding: 0.75rem;
		border: 1px solid var(--vibe-border);
		border-radius: var(--radius-md, 18px);
		background: var(--vibe-surface);
	}
	.pr-form__field {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
		font-size: 0.76rem;
		font-weight: 600;
	}
	.pr-form__field input,
	.pr-form__field textarea {
		font: inherit;
		font-weight: 400;
		font-size: 0.85rem;
		padding: 0.4rem 0.55rem;
		border: 1px solid var(--vibe-border-strong);
		border-radius: var(--radius-sm, 10px);
		background: var(--vibe-surface);
		color: var(--vibe-text);
	}
	.pr-form__actions {
		display: flex;
		align-items: center;
		gap: 0.45rem;
	}
	.pr-form__hint {
		margin-right: auto;
		font-size: 0.72rem;
		color: var(--vibe-text-muted);
	}
	.vbtn {
		min-height: 2rem;
		border: 1px solid var(--vibe-border-strong);
		border-radius: var(--radius-sm, 10px);
		background: var(--vibe-surface);
		color: var(--vibe-text);
		font: inherit;
		font-size: 0.78rem;
		font-weight: 600;
		padding: 0.35rem 0.7rem;
		cursor: pointer;
		text-decoration: none;
		display: inline-flex;
		align-items: center;
	}
	.vbtn--ghost {
		background: transparent;
	}
	.vbtn--primary {
		border-color: color-mix(in srgb, var(--vibe-accent) 72%, transparent);
		background: var(--accent-fill);
		color: var(--button-primary-color, #fff);
	}
	.vbtn:disabled {
		opacity: 0.55;
		cursor: not-allowed;
	}
	@media (max-width: 720px) {
		.branch-tab__bar {
			flex-direction: column;
			align-items: stretch;
		}
		.branch-tab__actions .vbtn {
			flex: 1;
			justify-content: center;
		}
	}
</style>
