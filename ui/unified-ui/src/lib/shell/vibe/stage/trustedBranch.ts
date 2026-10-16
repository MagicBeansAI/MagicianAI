/**
 * Trusted repo mode — the cockpit's client for a chain's branch.
 *
 * A trusted project runs each chain in its own git clone on a `vibedev/…`
 * branch; review is the branch diff and landing is Merge or Open PR
 * (docs/archive/plans/2026-10-07-vibedev-trusted-repo-mode.md §4.8–4.11). Every call
 * is keyed by the chain's ROOT task id (`chainRootTaskId`).
 */
import { timedFetch } from '$lib/shared/fetch';
import type { DiffFile } from '$lib/shell/DiffStrip.svelte';

export interface TrustedBranchCommit {
	sha: string;
	subject: string;
	author: string;
	/** Unix seconds. */
	committed_at: number;
}

export interface TrustedMergeRecord {
	target: string;
	merged_sha: string;
	strategy: 'fast_forward' | 'merge_commit';
	merged_at_ms: number;
}

export interface TrustedBranchSummary {
	root_task_id: string;
	branch: string;
	base_branch: string | null;
	base_sha: string;
	head_sha: string;
	/** Newest first — the chain's checkpoints. */
	commits: TrustedBranchCommit[];
	files: DiffFile[];
	behind_base: number | null;
	dirty: boolean;
	merged: TrustedMergeRecord | null;
	pull_request_url: string | null;
}

/** The server's refusal, with its human message (e.g. "conflicts with main"). */
export class TrustedBranchError extends Error {
	constructor(
		message: string,
		readonly status: number,
		readonly code: string | null
	) {
		super(message);
	}
}

function runUrl(rootTaskId: string, action: string): string {
	return `/api/magician/v2/vibedev/runs/${encodeURIComponent(rootTaskId)}/${action}`;
}

async function readJson<T>(response: Response, fallback: string): Promise<T> {
	const payload = (await response.json().catch(() => null)) as
		| (T & { message?: string; error?: string })
		| null;
	if (!response.ok) {
		throw new TrustedBranchError(
			payload?.message || payload?.error || fallback,
			response.status,
			payload?.error ?? null
		);
	}
	if (!payload) throw new TrustedBranchError(fallback, response.status, null);
	return payload;
}

/** `null` when the chain is not a trusted chain (404) — a sandboxed run. */
export async function fetchTrustedBranch(rootTaskId: string): Promise<TrustedBranchSummary | null> {
	const response = await timedFetch(runUrl(rootTaskId, 'branch'));
	if (response.status === 404) return null;
	return readJson<TrustedBranchSummary>(response, `Could not load the branch (${response.status})`);
}

async function post<T>(rootTaskId: string, action: string, body: unknown, fallback: string): Promise<T> {
	const response = await timedFetch(runUrl(rootTaskId, action), {
		method: 'POST',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify(body ?? {})
	});
	return readJson<T>(response, fallback);
}

export function mergeTrustedBranch(rootTaskId: string): Promise<TrustedMergeRecord> {
	return post(rootTaskId, 'merge', {}, 'Merge failed');
}

export function openTrustedPullRequest(
	rootTaskId: string,
	title: string,
	body: string
): Promise<{ pull_request_url: string }> {
	return post(rootTaskId, 'pull-request', { title, body }, 'Could not open the pull request');
}

export interface TrustedRewindResult {
	head_sha: string;
	/** The next turn resumes the engine session that commit's turn ran in. */
	session_resume_queued: boolean;
	resume_engine: string | null;
}

export function rewindTrustedBranch(rootTaskId: string, commit: string): Promise<TrustedRewindResult> {
	return post(rootTaskId, 'rewind', { commit }, 'Rewind failed');
}

export function discardTrustedRun(
	rootTaskId: string,
	deleteBranch = true
): Promise<{ discarded: boolean; branch_deleted: boolean }> {
	return post(rootTaskId, 'discard', { delete_branch: deleteBranch }, 'Discard failed');
}

export interface TrustedDiskUsage {
	workspace_bytes: number;
	build_dir_bytes: number | null;
	free_bytes: number | null;
}

/** Clone size, shared build dir size and free space (server-cached 5 min). */
export async function fetchTrustedDisk(rootTaskId: string): Promise<TrustedDiskUsage | null> {
	const response = await timedFetch(runUrl(rootTaskId, 'disk'));
	if (response.status === 404) return null;
	return readJson<TrustedDiskUsage>(response, `Could not read disk usage (${response.status})`);
}

/** `1.2 GB`, `340 MB`, `12 KB` — decimal units, like the OS shows. */
export function formatBytes(bytes: number | null | undefined): string {
	if (bytes === null || bytes === undefined || !Number.isFinite(bytes)) return '—';
	const units = ['B', 'KB', 'MB', 'GB', 'TB'];
	let value = bytes;
	let unit = 0;
	while (value >= 1000 && unit < units.length - 1) {
		value /= 1000;
		unit += 1;
	}
	return `${value >= 10 || unit === 0 ? Math.round(value) : value.toFixed(1)} ${units[unit]}`;
}

export function shortSha(sha: string | null | undefined): string {
	return (sha ?? '').slice(0, 7);
}

/** A default PR title: the branch's oldest commit subject, else the branch. */
export function defaultPullRequestTitle(summary: TrustedBranchSummary): string {
	const oldest = summary.commits[summary.commits.length - 1];
	const subject = oldest?.subject?.trim();
	if (subject && !subject.startsWith('vibedev: end of turn')) return subject;
	return summary.branch.replace(/^vibedev\//, '');
}

/** A default PR body: the branch's commit subjects, oldest first. */
export function defaultPullRequestBody(summary: TrustedBranchSummary): string {
	const lines = [...summary.commits].reverse().map((commit) => `- ${commit.subject}`);
	return lines.length ? `Commits:\n\n${lines.join('\n')}\n` : '';
}
