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
import {
	orderTaskDeliverables,
	outputFilesFrom,
	reportSourcePath,
	sidecarFilesFrom,
	withPrimaryDeliverable,
	type PanelOutputFile,
	type TaskOutputRef,
	type TaskSidecarRef
} from './taskPanelModel';

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
		const urlFor = (path: string) => taskOutputUrl(taskId, path, principal, workspace);
		// Every output, each marked deliverable or not (by the endpoint, or by the
		// same rule when an older server omits the mark), then the sidecar files.
		// The panel lists deliverables under Output and the rest under Technical
		// details; keeping them in one list keeps the row indexes the drawer's
		// events carry pointing into a single array.
		//
		// The deliverables then read primary deliverable, the other delivered
		// files, the summary (`orderTaskDeliverables`); the primary is named on
		// each row and once at the top, and either is enough.
		const sidecars = payload?.outputs?.internal_files;
		const marked = withPrimaryDeliverable(
			refs as TaskOutputRef[],
			payload?.outputs?.primary_deliverable_output_id
		);
		const files = orderTaskDeliverables([
			...outputFilesFrom(marked, urlFor),
			...(Array.isArray(sidecars) ? sidecarFilesFrom(sidecars as TaskSidecarRef[], urlFor) : [])
		]);
		// The file the task's written report is read from, so the panel can give
		// the rendered report that file's row rather than listing it twice.
		const source = reportSourcePath(
			refs as TaskOutputRef[],
			payload?.outputs?.primary_user_output_id
		);
		return source === null
			? files
			: files.map((file) => (file.path === source ? { ...file, reportSource: true } : file));
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

/** Resolve only our protected file routes; never send scope headers to a foreign host. */
function protectedAttachmentUrl(raw: string, base: URL): string | null {
	try {
		const resolved = new URL(raw, base);
		if (resolved.origin !== window.location.origin || resolved.username || resolved.password) return null;
		if (!/^\/api\/magician\/v3\/(?:tasks\/[^/]+\/outputs\/|executions\/[^/]+\/artifacts\/).+/.test(resolved.pathname)) return null;
		return resolved.href;
	} catch {
		return null;
	}
}

async function embeddedAttachment(blob: Blob): Promise<string> {
	const bytes = new Uint8Array(await blob.arrayBuffer());
	const chunks: string[] = [];
	for (let offset = 0; offset < bytes.length; offset += 32_768) {
		chunks.push(String.fromCharCode(...bytes.subarray(offset, offset + 32_768)));
	}
	return `data:${blob.type || 'application/octet-stream'};base64,${btoa(chunks.join(''))}`;
}

async function outputViewerBlob(blob: Blob, url: string): Promise<Blob> {
	if (blob.type.split(';')[0].toLowerCase() !== 'text/html') return blob;
	const base = new URL(url, window.location.href);
	const html = new DOMParser().parseFromString(await blob.text(), 'text/html');
	const attachments = new Map<string, Promise<string>>();
	const refs = html.querySelectorAll('a[href], img[src], audio[src], video[src], video[poster], source[src], track[src]');
	const prepared = await Promise.allSettled(Array.from(refs, async (element) => {
		for (const attr of ['href', 'src', 'poster']) {
			const raw = element.getAttribute(attr);
			const attachment = raw === null ? null : protectedAttachmentUrl(raw, base);
			if (!attachment) continue;
			let loaded = attachments.get(attachment);
			if (!loaded) {
				loaded = authenticatedOutputBlob(attachment).then(embeddedAttachment);
				attachments.set(attachment, loaded);
			}
			element.setAttribute(attr, await loaded);
			if (element.tagName === 'A' && attr === 'href' && !element.getAttribute('download')) {
				const filename = new URL(attachment).pathname.split('/').pop() || 'attachment';
				element.setAttribute('download', filename);
			}
		}
	}));
	const failed = prepared.find((result) => result.status === 'rejected');
	if (failed?.status === 'rejected') throw failed.reason;
	// Keep the static preview self-contained after the originating page reloads
	// or closes. Protected links download embedded bytes; scripts stay disabled.
	const frame = document.createElement('iframe');
	frame.title = html.title || 'Task output';
	frame.setAttribute('sandbox', 'allow-same-origin allow-popups allow-downloads');
	frame.style.cssText = 'display:block;width:100%;height:100vh;border:0';
	frame.srcdoc = '<!doctype html>' + html.documentElement.outerHTML;
	return new Blob(['<!doctype html><meta charset="utf-8"><body style="margin:0">', frame.outerHTML], { type: 'text/html' });
}

/** Open output bytes without placing the bearer or scope in the address bar. */
export async function openAuthenticatedTaskOutput(url: string): Promise<void> {
	const tab = window.open('', '_blank');
	if (tab) tab.opener = null;
	const objectUrls: string[] = [];
	const release = () => objectUrls.forEach((objectUrl) => URL.revokeObjectURL(objectUrl));
	try {
		const blob = await outputViewerBlob(await authenticatedOutputBlob(url), url);
		const objectUrl = URL.createObjectURL(blob);
		objectUrls.push(objectUrl);
		if (tab) {
			tab.location.href = objectUrl;
			// Attachment links must remain usable after the first minute.
			const timer = setInterval(() => {
				if (tab.closed) { clearInterval(timer); release(); }
			}, 60_000);
		} else {
			window.open(objectUrl, '_blank', 'noopener,noreferrer');
			setTimeout(release, 60_000);
		}
	} catch (error) {
		release();
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

/**
 * Shared body of the authenticated subresource actions: fetch the bytes with
 * the scoped bearer, hand the element a blob URL, revoke the previous one, and
 * ignore answers that arrive after a newer request or after destroy.
 * `retype` forces the blob's media type (see `authenticatedTaskOutputPdf`).
 */
function authenticatedSubresource<E extends HTMLElement>(
	node: E,
	url: string,
	assign: (node: E, objectUrl: string) => void,
	clear: (node: E) => void,
	retype?: string
): { update(next: string): void; destroy(): void } {
	let generation = 0;
	let objectUrl: string | null = null;

	const load = async (next: string): Promise<void> => {
		const requestGeneration = ++generation;
		try {
			const blob = await authenticatedOutputBlob(next);
			const typed = retype ? new Blob([blob], { type: retype }) : blob;
			const nextObjectUrl = URL.createObjectURL(typed);
			if (requestGeneration !== generation) {
				URL.revokeObjectURL(nextObjectUrl);
				return;
			}
			if (objectUrl) URL.revokeObjectURL(objectUrl);
			objectUrl = nextObjectUrl;
			assign(node, nextObjectUrl);
		} catch {
			if (requestGeneration === generation) clear(node);
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

/** Svelte image action that authenticates a subresource before assigning it. */
export function authenticatedTaskOutputImage(
	node: HTMLImageElement,
	url: string
): { update(next: string): void; destroy(): void } {
	return authenticatedSubresource(
		node,
		url,
		(img, objectUrl) => (img.src = objectUrl),
		(img) => img.removeAttribute('src')
	);
}

/**
 * Svelte iframe action for an inline PDF preview. The blob is re-typed as
 * `application/pdf` whatever the response said, so the frame only ever runs
 * the browser's PDF viewer: a mislabelled output can never execute as an
 * HTML page on the app's origin through a same-origin blob URL.
 */
export function authenticatedTaskOutputPdf(
	node: HTMLIFrameElement,
	url: string
): { update(next: string): void; destroy(): void } {
	return authenticatedSubresource(
		node,
		url,
		(frame, objectUrl) => (frame.src = objectUrl),
		(frame) => {
			frame.removeAttribute('src');
			frame.dispatchEvent(new Event('error'));
		},
		'application/pdf'
	);
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
