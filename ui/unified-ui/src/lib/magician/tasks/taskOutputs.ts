/**
 * The task panel's Output act, over the wire.
 *
 * `GET /api/magician/v3/tasks/{id}/outputs` is the only source that carries a
 * **relative path and a media type** per file. `Task.completionArtifactNames`
 * carries neither reliably — its entries are namespace-qualified artifact names
 * written at completion, not paths under the task's outputs directory — so the
 * open and reveal endpoints cannot resolve them. Those endpoints are the sink
 * for exactly the paths this returns, which is why the affordances are wired
 * from here and not from the names already in the store.
 *
 * See `docs/components/unified-ui/unified-task-panel.md`.
 */

import { timedFetch } from '$lib/shared/fetch';
import { scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';

import {
	previewKindOf,
	tooLargeLine,
	PREVIEW_MAX_BYTES,
	type TaskFilePreview
} from './taskFilePreview';
import { outputFilesFrom, type PanelOutputFile, type TaskOutputRef } from './taskPanelModel';

const API_BASE = '/api/magician/v3';

function scopedHeaders(): Headers {
	return scopedRequestHeaders({ Accept: 'application/json' });
}

/**
 * This task's output files, or `null` when the request failed.
 *
 * The two are different answers and the panel renders them differently
 * (design §6): `[]` is an Output act that loaded and found nothing, `null` is
 * one that could not load and must therefore be absent rather than empty — an
 * empty act asserts `no output`, which may be false.
 *
 * A malformed body is a failure, not an empty list, for the same reason: we did
 * not learn that the task produced nothing.
 */
export async function fetchTaskOutputFiles(
	taskId: string,
	principal: string,
	workspace: string
): Promise<PanelOutputFile[] | null> {
	try {
		const response = await timedFetch(
			`${API_BASE}/tasks/${encodeURIComponent(taskId)}/outputs`,
			{ headers: scopedHeaders() }
		);
		if (!response.ok) return null;
		const payload = await response.json();
		const refs = payload?.outputs?.outputs;
		if (!Array.isArray(refs)) return null;
		return outputFilesFrom(refs as TaskOutputRef[], (path) =>
			taskOutputUrl(taskId, path, principal, workspace)
		);
	} catch {
		return null;
	}
}

/**
 * Where one output file's bytes are served.
 *
 * This is an address, not a self-authorizing link. Consumers must fetch it
 * through `scopedRequestHeaders` and use a blob URL for images, new tabs, and
 * downloads; putting either scope selectors or the bearer in the URL would
 * leak authority into history, logs, referrers, and copied links.
 *
 * Each segment is encoded separately, and `.`/`..` segments are dropped: the
 * path comes from the server, `encodeURIComponent` leaves both untouched, and a
 * traversal component reaching the backend as a literal is defence the client
 * can afford even though the backend validates too. Same treatment as the chat
 * surface's own file links.
 */
export function taskOutputUrl(
	taskId: string,
	relativePath: string,
	principal: string,
	workspace: string
): string {
	void principal;
	void workspace;
	const encoded = relativePath
		.split('/')
		.filter((segment) => segment.length > 0 && segment !== '.' && segment !== '..')
		.map((segment) => encodeURIComponent(segment))
		.join('/');
	return `${API_BASE}/tasks/${encodeURIComponent(taskId)}/outputs/${encoded}`;
}

async function authenticatedOutputBlob(url: string): Promise<Blob> {
	const response = await timedFetch(url, {
		headers: scopedRequestHeaders({ Accept: '*/*' })
	});
	if (!response.ok) throw new Error(`The file could not be read (HTTP ${response.status})`);
	return await response.blob();
}

/** Open output bytes without placing the bearer or scope in the address bar. */
export async function openAuthenticatedTaskOutput(url: string): Promise<void> {
	const tab = window.open('', '_blank');
	if (tab) tab.opener = null;
	try {
		const objectUrl = URL.createObjectURL(await authenticatedOutputBlob(url));
		if (tab) tab.location.href = objectUrl;
		else window.open(objectUrl, '_blank', 'noopener,noreferrer');
		setTimeout(() => URL.revokeObjectURL(objectUrl), 60_000);
	} catch (error) {
		tab?.close();
		throw error;
	}
}

/** Download output bytes through an authenticated fetch. */
export async function downloadAuthenticatedTaskOutput(url: string, filename: string): Promise<void> {
	const objectUrl = URL.createObjectURL(await authenticatedOutputBlob(url));
	try {
		const anchor = document.createElement('a');
		anchor.href = objectUrl;
		anchor.download = filename;
		anchor.rel = 'noreferrer';
		anchor.click();
	} finally {
		setTimeout(() => URL.revokeObjectURL(objectUrl), 1_000);
	}
}

/** Svelte image action that authenticates a subresource before assigning it. */
export function authenticatedTaskOutputImage(
	node: HTMLImageElement,
	url: string
): { update(next: string): void; destroy(): void } {
	let generation = 0;
	let objectUrl: string | null = null;

	const load = async (next: string): Promise<void> => {
		const requestGeneration = ++generation;
		try {
			const nextObjectUrl = URL.createObjectURL(await authenticatedOutputBlob(next));
			if (requestGeneration !== generation) {
				URL.revokeObjectURL(nextObjectUrl);
				return;
			}
			if (objectUrl) URL.revokeObjectURL(objectUrl);
			objectUrl = nextObjectUrl;
			node.src = nextObjectUrl;
		} catch {
			if (requestGeneration === generation) node.removeAttribute('src');
		}
	};

	void load(url);
	return {
		update(next: string): void {
			void load(next);
		},
		destroy(): void {
			generation += 1;
			if (objectUrl) URL.revokeObjectURL(objectUrl);
		}
	};
}

/** The byte length of a decoded body, which is the only size we know is true. */
function byteLength(text: string): number {
	return new TextEncoder().encode(text).length;
}

/**
 * One output file's bytes, for the panel to read in place.
 *
 * **The surface fetches; the panel renders.** The panel is a pure prop render
 * and cannot make a request, which is exactly why it dispatches `previewFile`
 * instead — and why this lives here, beside the outputs list request, rather
 * than inside the component. Every other fetch behind this panel works the same
 * way: the outputs, the run state and the timeline all arrived as props.
 *
 * **One loader, both routes.** `/tasks` and `/tasks?type=internal` mint the same
 * URL through `taskOutputUrl` and hit the same GET route, so a second copy of
 * this would be a second answer to what the size ceiling is and what a failure
 * reads as.
 *
 * Four answers, and none of them is an empty preview:
 *
 * - **`failed`** — no URL, no previewable kind, a non-2xx, or a request that
 *   threw. The row keeps Open and Download either way; a preview that cannot be
 *   read says so rather than rendering a blank block, which would be
 *   indistinguishable from a file that really is empty.
 * - **`too-large`** — over `PREVIEW_MAX_BYTES`. Checked **twice**: against the
 *   size the outputs endpoint declared, so an eight-megabyte file costs no
 *   request at all, and again against what actually arrived, because the
 *   declared size can be absent and a body can be bigger than its record says.
 *   Neither check truncates.
 * - **`ready`** — the bytes, as text, for the panel to draw per its own reading
 *   of `previewKindOf`.
 *
 * `loading` is never returned; it is the record the caller writes the moment it
 * dispatches, so the row shows progress rather than the last file's contents.
 */
export async function loadOutputPreview(
	index: number,
	file: PanelOutputFile
): Promise<TaskFilePreview> {
	const failed = (detail: string): TaskFilePreview => ({
		index,
		status: 'failed',
		text: null,
		detail
	});

	// Neither is reachable from the panel, which offers no control without a URL
	// and none for a file it has no way to draw. Answered rather than thrown, so
	// a caller that dispatched anyway leaves the row saying why instead of
	// spinning on `loading` for as long as it stays open.
	if (file.url === null) return failed('This file has no address to read from');
	if (previewKindOf(file.mediaType, file.path) === null) {
		return failed("There's no preview for this kind of file");
	}

	if (file.sizeBytes !== null && file.sizeBytes > PREVIEW_MAX_BYTES) {
		return { index, status: 'too-large', text: null, detail: tooLargeLine(file.sizeBytes) };
	}

	try {
		const response = await timedFetch(file.url, {
			headers: scopedRequestHeaders({ Accept: '*/*' })
		});
		if (!response.ok) return failed(`The file could not be read (HTTP ${response.status})`);

		const text = await response.text();
		const size = byteLength(text);
		if (size > PREVIEW_MAX_BYTES) {
			return { index, status: 'too-large', text: null, detail: tooLargeLine(size) };
		}
		return { index, status: 'ready', text, detail: null };
	} catch (error) {
		return failed(error instanceof Error ? error.message : 'The request failed');
	}
}
