<script lang="ts">
	import { showError } from '$lib/shared/stores/notifications';
	import type { ContentBlockRecord } from '$lib/stores/chatStore';
	import ChatMarkdown from '$lib/magician/components/chat/ChatMarkdown.svelte';
	import { timedFetch } from '$lib/shared/fetch';
	import {
		authenticatedTaskOutputImage,
		downloadAuthenticatedTaskOutput,
		openAuthenticatedTaskOutput
	} from '$lib/magician/tasks/taskOutputs';

	/**
	 * Optional chat session id. Drives the "Reveal in Finder" button
	 * for blocks whose `source` isn't `task_output` (e.g. legacy
	 * `session_output` files that need session-scoped path validation).
	 *
	 * `task_output` blocks (rendered from ExecutionPanel, chat task-card
	 * footers, etc.) ALWAYS get the OS-action buttons regardless of
	 * `sessionId` — they route through the V3 task-scoped open-folder /
	 * open-file endpoints which only need the task_id (carried in the
	 * block's `source` field).
	 */
	export let sessionId: string | null = null;
	export let blocks: ContentBlockRecord[] = [];

	let failedImages: Record<string, boolean> = {};
	let failedImageSessionId: string | null = null;

	$: if (sessionId !== failedImageSessionId) {
		failedImages = {};
		failedImageSessionId = sessionId;
	}

	function encodeRelativePath(relativePath: string): string {
		// Strip empty segments AND parent-dir / self refs. `encodeURIComponent`
		// leaves `..` and `.` untouched, so a server-supplied path containing
		// either would otherwise reach the backend as a literal traversal
		// component. Defense-in-depth — the backend also validates.
		return relativePath
			.split('/')
			.filter((segment) => segment.length > 0 && segment !== '.' && segment !== '..')
			.map((segment) => encodeURIComponent(segment))
			.join('/');
	}

	// Older chat messages stored `relative_path` with a leading
	// `outputs/` segment (legacy bug in `project_task_outputs_for_chat`
	// pre-v0.6.508). Both the v2 chat-session-output route and the v3
	// task-output route already serve from the task/session `outputs/`
	// dir, so the embedded prefix double-counts and produces a
	// `/outputs/outputs/<file>` URL that 404s. Strip it here so old
	// stored messages keep working alongside new (correct) ones.
	function normalizeRelativePath(value: string): string {
		return value.replace(/^\/?outputs\//, '');
	}

	function fileHref(block: ContentBlockRecord): string | null {
		if (block.type !== 'file' || !block.relative_path) return null;
		const encodedPath = encodeRelativePath(normalizeRelativePath(block.relative_path));
		if (block.source?.type === 'task_output' && block.source.task_id) {
			return `/api/magician/v3/tasks/${encodeURIComponent(block.source.task_id)}/outputs/${encodedPath}`;
		}
		if (!sessionId) return null;
		return `/api/magician/v2/chat/sessions/${encodeURIComponent(sessionId)}/outputs/${encodedPath}`;
	}

	/**
	 * Build the endpoint URL for "reveal in OS file manager". Picks
	 * between two implementations based on what owns the block:
	 *   - task_output: V3 task-scoped endpoint (works from ExecutionPanel
	 *     and anywhere else without a live chat session)
	 *   - everything else: chat-session-scoped endpoint (legacy path
	 *     for actual chat-rendered outputs that need session validation)
	 */
	function openFolderEndpoint(block: ContentBlockRecord): string | null {
		if (block.type === 'file' && block.source?.type === 'task_output' && block.source.task_id) {
			return `/api/magician/v3/tasks/${encodeURIComponent(block.source.task_id)}/outputs/open-folder`;
		}
		if (!sessionId) return null;
		return `/api/magician/v2/chat/sessions/${encodeURIComponent(sessionId)}/outputs/open-folder`;
	}

	/** Same dispatch as `openFolderEndpoint` for the "open in default app" endpoint. */
	function openFileEndpoint(block: ContentBlockRecord): string | null {
		if (block.type === 'file' && block.source?.type === 'task_output' && block.source.task_id) {
			return `/api/magician/v3/tasks/${encodeURIComponent(block.source.task_id)}/outputs/open-file`;
		}
		// Chat-session has no separate open-file endpoint today — the
		// browser "Open ↗" link covers the in-tab case for chat.
		return null;
	}

	/** Does this block render its "Reveal in Finder" affordance? */
	function canRevealFolder(block: ContentBlockRecord): boolean {
		return openFolderEndpoint(block) !== null;
	}

	/** Does this block render its "Open in default app" affordance? */
	function canOpenInApp(block: ContentBlockRecord): boolean {
		// HTML deliverables embed media via SERVER-RELATIVE URLs (e.g.
		// `<img src="/api/.../outputs/…jpg">`), which only resolve when the page
		// is served over http — that's what the "Open" button does. "Open in app"
		// opens the RAW file from disk as `file://`, where those URLs point at
		// nonexistent local paths → broken images + dead in-page links. There is no
		// way for a desktop app to render our HTML correctly from disk, and "Open"
		// already renders it perfectly, so suppress "Open in app" for HTML. Other
		// types (PDF/image/video/audio/docs) are self-contained files and stay.
		if (isHtml(block)) return false;
		return openFileEndpoint(block) !== null;
	}

	function isImage(block: ContentBlockRecord): boolean {
		return block.type === 'file' && (block.mime_type ?? '').startsWith('image/');
	}

	function isVideo(block: ContentBlockRecord): boolean {
		return block.type === 'file' && (block.mime_type ?? '').startsWith('video/');
	}

	function isAudio(block: ContentBlockRecord): boolean {
		return block.type === 'file' && (block.mime_type ?? '').startsWith('audio/');
	}

	function isPdf(block: ContentBlockRecord): boolean {
		const mime = block.type === 'file' ? block.mime_type ?? '' : '';
		return mime === 'application/pdf';
	}

	/**
	 * HTML reports / dashboards / standalone pages. Renders in an
	 * `<iframe>` (same treatment as PDFs) so the browser interprets the
	 * markup instead of dumping the raw source. Matches when the mime
	 * is text/html OR the relative_path extension is .html/.htm, since
	 * task finalizers occasionally omit the mime type but still write
	 * a sensible filename. MUST be checked BEFORE `isInlineableText` —
	 * otherwise HTML would match the text/* family there and render as
	 * a `<pre>` block full of un-rendered tags.
	 */
	function isHtml(block: ContentBlockRecord): boolean {
		if (block.type !== 'file') return false;
		const mime = (block.mime_type ?? '').toLowerCase();
		if (mime === 'text/html' || mime === 'application/xhtml+xml') return true;
		const name = (block.relative_path ?? block.display_name ?? '').toLowerCase();
		const ext = name.split('.').pop() ?? '';
		return ext === 'html' || ext === 'htm' || ext === 'xhtml';
	}

	/**
	 * Types that the browser renders perfectly well in a fresh tab —
	 * with its full native chrome (PDF.js toolbar, video controls,
	 * audio scrubber, full-viewport HTML). Inline iframes / video tags
	 * for these were noisy (variable sizing, blank space, autoplay
	 * pitfalls). Surfaced instead as compact "Open ↗" cards.
	 *
	 * Images stay separate (their own inline thumb branch) since a
	 * 200-px thumb is more scannable than a card for visual content.
	 * Markdown / JSON / CSV / TXT use the existing inline-preview
	 * toggle because the browser would just dump them as raw text.
	 */
	type OpenInBrowserKind = 'pdf' | 'html' | 'video' | 'audio';

	function openInBrowserKind(block: ContentBlockRecord): OpenInBrowserKind | null {
		if (isPdf(block)) return 'pdf';
		if (isHtml(block)) return 'html';
		if (isVideo(block)) return 'video';
		if (isAudio(block)) return 'audio';
		return null;
	}

	function openInBrowserLabel(kind: OpenInBrowserKind): string {
		switch (kind) {
			case 'pdf':
				return 'PDF document';
			case 'html':
				return 'HTML page';
			case 'video':
				return 'Video';
			case 'audio':
				return 'Audio';
		}
	}

	/** Max bytes we auto-fetch for inline text/markdown/json preview. */
	const TEXT_PREVIEW_CAP_BYTES = 256 * 1024;

	function isInlineableText(block: ContentBlockRecord): { kind: 'markdown' | 'json' | 'text' } | null {
		if (block.type !== 'file') return null;
		// HTML is rendered in an iframe by isHtml; never treat it as
		// inline text or the viewer dumps raw markup as a <pre> block.
		if (isHtml(block)) return null;
		const mime = (block.mime_type ?? '').toLowerCase();
		const name = (block.relative_path ?? block.display_name ?? '').toLowerCase();
		const ext = name.split('.').pop() ?? '';
		if (mime === 'text/markdown' || ext === 'md' || ext === 'markdown') return { kind: 'markdown' };
		if (mime === 'application/json' || mime === 'text/json' || ext === 'json') return { kind: 'json' };
		// Plain-text family: text/plain, text/csv, text/tab-separated-values, text/log, …
		if (mime.startsWith('text/')) return { kind: 'text' };
		if (['txt', 'log', 'csv', 'tsv'].includes(ext)) return { kind: 'text' };
		return null;
	}

	type TextPreviewState =
		| { status: 'idle' }
		| { status: 'loading' }
		| { status: 'too_large'; size: number }
		| { status: 'error'; message: string }
		| { status: 'ready'; content: string; truncated: boolean };

	let textPreviews: Record<string, TextPreviewState> = {};

	function previewKey(block: ContentBlockRecord, href: string): string {
		return `${sessionId}:${block.relative_path ?? href}`;
	}

	async function loadInlineText(block: ContentBlockRecord, href: string): Promise<void> {
		const key = previewKey(block, href);
		if (textPreviews[key] && textPreviews[key].status !== 'idle') return;
		textPreviews = { ...textPreviews, [key]: { status: 'loading' } };
		try {
			// Cheap pre-flight: known size from the block itself.
			if (typeof block.size === 'number' && block.size > TEXT_PREVIEW_CAP_BYTES) {
				textPreviews = { ...textPreviews, [key]: { status: 'too_large', size: block.size } };
				return;
			}
			const response = await timedFetch(href);
			if (!response.ok) {
				textPreviews = {
					...textPreviews,
					[key]: { status: 'error', message: `HTTP ${response.status}` },
				};
				return;
			}
			const reportedLen = parseInt(response.headers.get('content-length') ?? '0', 10);
			if (reportedLen && reportedLen > TEXT_PREVIEW_CAP_BYTES) {
				textPreviews = {
					...textPreviews,
					[key]: { status: 'too_large', size: reportedLen },
				};
				return;
			}
			const text = await response.text();
			const truncated = text.length > TEXT_PREVIEW_CAP_BYTES;
			const content = truncated ? text.slice(0, TEXT_PREVIEW_CAP_BYTES) : text;
			textPreviews = {
				...textPreviews,
				[key]: { status: 'ready', content, truncated },
			};
		} catch (error) {
			textPreviews = {
				...textPreviews,
				[key]: {
					status: 'error',
					message: error instanceof Error ? error.message : String(error),
				},
			};
		}
	}

	// Collapse a previously-loaded inline text preview back to its
	// initial idle state, so the operator can hide a long JSON / markdown
	// body once they've finished reading. Cheap — we only drop the
	// rendered state, not the fetched content; re-clicking "Preview
	// inline" refetches.
	function hideInlineText(block: ContentBlockRecord, href: string): void {
		const key = previewKey(block, href);
		const next = { ...textPreviews };
		delete next[key];
		textPreviews = next;
	}

	function imageStateKey(block: ContentBlockRecord, href: string): string {
		return `${sessionId}:${block.relative_path ?? href}`;
	}

	function markImageFailed(key: string): void {
		failedImages = {
			...failedImages,
			[key]: true
		};
	}

	// Pretty-print JSON content for inline preview. Files saved by
	// task finalizers are typically minified single-line JSON; rendering
	// them as-is inside the preview surface produces an unreadable
	// horizontal wall. Parse + re-stringify with 2-space indent so the
	// reader can scan keys/values. Falls back to the raw content if
	// parsing fails (malformed JSON, partial stream, etc.) so the
	// operator still sees *something* rather than a hard error.
	function prettyPrintJson(raw: string): string {
		try {
			return JSON.stringify(JSON.parse(raw), null, 2);
		} catch {
			return raw;
		}
	}

	function formatBytes(size?: number): string {
		if (!size || size <= 0) return '';
		const units = ['B', 'KB', 'MB', 'GB'];
		let value = size;
		let unitIndex = 0;
		while (value >= 1024 && unitIndex < units.length - 1) {
			value /= 1024;
			unitIndex += 1;
		}
		return `${value >= 10 || unitIndex === 0 ? value.toFixed(0) : value.toFixed(1)} ${units[unitIndex]}`;
	}

	function displayPath(block: ContentBlockRecord): string {
		return block.type === 'file' ? block.absolute_path ?? block.relative_path ?? '' : '';
	}

	function outputFilename(block: ContentBlockRecord): string {
		const pathName = block.type === 'file'
			? normalizeRelativePath(block.relative_path ?? '').split('/').filter(Boolean).at(-1)
			: undefined;
		return block.display_name ?? pathName ?? block.label ?? 'download';
	}

	async function openFileInBrowser(href: string): Promise<void> {
		try {
			await openAuthenticatedTaskOutput(href);
		} catch (error) {
			showError(error instanceof Error ? error.message : "Couldn't open that file.");
		}
	}

	async function downloadFile(block: ContentBlockRecord, href: string): Promise<void> {
		try {
			await downloadAuthenticatedTaskOutput(href, outputFilename(block));
		} catch (error) {
			showError(error instanceof Error ? error.message : "Couldn't download that file.");
		}
	}

	async function openFolder(block: ContentBlockRecord): Promise<void> {
		if (block.type !== 'file') return;
		const endpoint = openFolderEndpoint(block);
		if (!endpoint) {
			showError("Couldn't determine where to reveal this file.");
			return;
		}
		await postOsAction(endpoint, block, 'Failed to reveal folder');
	}

	async function openFileInApp(block: ContentBlockRecord): Promise<void> {
		if (block.type !== 'file') return;
		const endpoint = openFileEndpoint(block);
		if (!endpoint) {
			showError("Couldn't determine where to open this file.");
			return;
		}
		await postOsAction(endpoint, block, 'Failed to open file');
	}

	/**
	 * Shared POST + error-handling for the OS-action endpoints
	 * (open-folder, open-file). Both V3 (task-scoped) and V2
	 * (chat-session-scoped) endpoints accept the same `relative_path`
	 * key, so a single body shape works. The V2 endpoint additionally
	 * uses `source` + `absolute_path` for legacy compatibility, so we
	 * keep sending all three — V3 ignores the extras harmlessly.
	 */
	async function postOsAction(
		endpoint: string,
		block: ContentBlockRecord,
		fallbackError: string,
	): Promise<void> {
		const response = await timedFetch(endpoint, {
			method: 'POST',
			headers: { 'Content-Type': 'application/json' },
			body: JSON.stringify({
				source: block.source,
				relative_path: block.relative_path,
				absolute_path: block.absolute_path,
			}),
		});
		if (response.ok) return;
		let message = fallbackError;
		try {
			const payload = await response.json();
			if (typeof payload?.error === 'string' && payload.error.trim()) {
				message = payload.error;
			}
		} catch {
			// Ignore JSON parse failures and fall back to the generic error.
		}
		showError(message);
	}
</script>

{#if blocks.length > 0}
	<div class="chat-rich-blocks">
		{#each blocks as block, blockIndex (block.relative_path ?? block.display_name ?? block.url ?? blockIndex)}
			{#if block.type === 'text' && block.text}
				<div class="chat-rich-text">
					<ChatMarkdown content={block.text} />
				</div>
			{:else if block.type === 'file'}
				{@const href = fileHref(block)}
				{#if href}
					{@const imageKey = imageStateKey(block, href)}
					{#if isImage(block) && !failedImages[imageKey]}
						<div class="chat-rich-file-block">
							<button
								class="chat-rich-image-link"
								type="button"
								on:click={() => openFileInBrowser(href)}
								aria-label="Open {block.label ?? block.display_name ?? block.relative_path ?? 'image'} in a new browser tab"
							>
								<img
									class="chat-rich-image"
									use:authenticatedTaskOutputImage={href}
									alt={block.label ?? block.display_name ?? block.relative_path ?? 'image'}
									loading="lazy"
									on:error={() => markImageFailed(imageKey)}
								/>
							</button>
							<div class="chat-rich-image-meta">
								{#if block.label || block.display_name}
									<div class="chat-rich-image-name">
										{block.label ?? block.display_name}
									</div>
								{/if}
								<div class="chat-rich-image-size">
									{#if formatBytes(block.size)}
										<span>{formatBytes(block.size)}</span>
									{/if}
									{#if displayPath(block)}
										<span class="chat-rich-path">{displayPath(block)}</span>
									{/if}
								</div>
								<div class="chat-rich-actions">
									<button class="chat-rich-action-button" type="button" on:click={() => openFileInBrowser(href)} title="Open file in a new browser tab">
										<svg xmlns="http://www.w3.org/2000/svg" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"/><polyline points="15 3 21 3 21 9"/><line x1="10" y1="14" x2="21" y2="3"/></svg>
										Open file
									</button>
									{#if canRevealFolder(block)}

										<button class="chat-rich-action-button" type="button" on:click={() => openFolder(block)} title="Reveal the containing folder in your OS file manager">
										<svg xmlns="http://www.w3.org/2000/svg" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/></svg>
										Reveal in Finder
									</button>

									{/if}
								</div>
							</div>
						</div>
					{:else if openInBrowserKind(block)}
						<!-- PDF / HTML / video / audio — open in a fresh
						     browser tab where the native viewer (PDF.js,
						     <video controls>, <audio controls>, full HTML
						     page) handles rendering. Inline iframes for
						     these were noisy (sizing edge cases, blank
						     space, autoplay surprises); a compact card
						     with one click is cleaner. -->
						{@const kind = openInBrowserKind(block)!}
						<div class="chat-rich-open-card">
							<div class="chat-rich-open-card-icon chat-rich-open-card-icon--{kind}" aria-hidden="true">
								{#if kind === 'pdf'}
									<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><polyline points="14 2 14 8 20 8"/><text x="12" y="17" text-anchor="middle" font-size="6" font-weight="700" stroke-width="0" fill="currentColor">PDF</text></svg>
								{:else if kind === 'html'}
									<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="16 18 22 12 16 6"/><polyline points="8 6 2 12 8 18"/></svg>
								{:else if kind === 'video'}
									<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polygon points="23 7 16 12 23 17 23 7"/><rect x="1" y="5" width="15" height="14" rx="2" ry="2"/></svg>
								{:else}
									<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M3 18v-6a9 9 0 0 1 18 0v6"/><path d="M21 19a2 2 0 0 1-2 2h-1a2 2 0 0 1-2-2v-3a2 2 0 0 1 2-2h3z"/><path d="M3 19a2 2 0 0 0 2 2h1a2 2 0 0 0 2-2v-3a2 2 0 0 0-2-2H3z"/></svg>
								{/if}
							</div>
							<div class="chat-rich-open-card-body">
								<div class="chat-rich-open-card-name">
									{block.label ?? block.display_name ?? block.relative_path ?? openInBrowserLabel(kind)}
								</div>
								<div class="chat-rich-open-card-meta">
									<span>{openInBrowserLabel(kind)}</span>
									{#if block.mime_type}
										<span class="chat-rich-open-card-mime">{block.mime_type}</span>
									{/if}
									{#if formatBytes(block.size)}
										<span>{formatBytes(block.size)}</span>
									{/if}
								</div>
							</div>
							<div class="chat-rich-open-card-actions">
								<button class="chat-rich-action-button" type="button" on:click={() => openFileInBrowser(href)} title="Open in a new browser tab">
									<svg xmlns="http://www.w3.org/2000/svg" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"/><polyline points="15 3 21 3 21 9"/><line x1="10" y1="14" x2="21" y2="3"/></svg>
									Open ↗
								</button>
								<!-- Fetch through the authenticated client before creating the temporary
								     browser URL; direct navigation cannot attach the workspace bearer. -->
								<button class="chat-rich-action-button" type="button" on:click={() => downloadFile(block, href)} title="Download this file">
									<svg xmlns="http://www.w3.org/2000/svg" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="7 10 12 15 17 10"/><line x1="12" y1="15" x2="12" y2="3"/></svg>
									Download
								</button>
								{#if canOpenInApp(block)}
									<button class="chat-rich-action-button" type="button" on:click={() => openFileInApp(block)} title="Open in the OS-default app (Preview, Word, VLC, …)">
										<svg xmlns="http://www.w3.org/2000/svg" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="3" y="3" width="18" height="18" rx="2" ry="2"/><polyline points="9 11 12 14 15 11"/><line x1="12" y1="14" x2="12" y2="6"/></svg>
										Open in app
									</button>
								{/if}
								{#if canRevealFolder(block)}
									<button class="chat-rich-action-button" type="button" on:click={() => openFolder(block)} title="Reveal the containing folder in your OS file manager">
										<svg xmlns="http://www.w3.org/2000/svg" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/></svg>
										Reveal in Finder
									</button>
								{/if}
							</div>
						</div>
					{:else if isInlineableText(block)}
						{@const textKind = isInlineableText(block)!.kind}
						{@const previewState = textPreviews[previewKey(block, href)] ?? { status: 'idle' }}
						<div class="chat-rich-file-block">
							<div class="chat-rich-text-preview-header">
								<div class="chat-rich-file-name">
									{block.label ?? block.display_name ?? block.relative_path ?? 'file'}
								</div>
								<div class="chat-rich-file-meta">
									{block.mime_type ?? textKind}
									{#if formatBytes(block.size)}
										<span>&middot; {formatBytes(block.size)}</span>
									{/if}
								</div>
							</div>
							{#if previewState.status === 'idle'}
								<button
									class="chat-rich-action-button"
									type="button"
									on:click={() => loadInlineText(block, href)}
								>
									Preview inline
								</button>
							{:else if previewState.status === 'loading'}
								<div class="chat-rich-text-preview-status">Loading…</div>
							{:else if previewState.status === 'too_large'}
								<div class="chat-rich-text-preview-status">
									File too large for inline preview
									{#if formatBytes(previewState.size)}
										&middot; {formatBytes(previewState.size)}
									{/if}
								</div>
							{:else if previewState.status === 'error'}
								<div class="chat-rich-text-preview-status">Preview failed: {previewState.message}</div>
							{:else if previewState.status === 'ready'}
								{#if textKind === 'markdown'}
									<div class="chat-rich-text-preview-body chat-rich-text-preview-markdown">
										<ChatMarkdown content={previewState.content} />
									</div>
								{:else if textKind === 'json'}
									{@const pretty = prettyPrintJson(previewState.content)}
									<pre class="chat-rich-text-preview-body chat-rich-text-preview-pre chat-rich-text-preview-json">{pretty}</pre>
								{:else}
									<pre class="chat-rich-text-preview-body chat-rich-text-preview-pre">{previewState.content}</pre>
								{/if}
								{#if previewState.truncated}
									<div class="chat-rich-text-preview-status">Preview truncated to first {formatBytes(TEXT_PREVIEW_CAP_BYTES)}</div>
								{/if}
								<button
									class="chat-rich-action-button chat-rich-action-button--toggle"
									type="button"
									on:click={() => hideInlineText(block, href)}
									title="Collapse this preview back to the file header"
								>
									<svg xmlns="http://www.w3.org/2000/svg" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><polyline points="18 15 12 9 6 15"/></svg>
									Hide preview
								</button>
							{/if}
							<div class="chat-rich-actions">
								<button class="chat-rich-action-button" type="button" on:click={() => openFileInBrowser(href)} title="Open file in a new browser tab">
									<svg xmlns="http://www.w3.org/2000/svg" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"/><polyline points="15 3 21 3 21 9"/><line x1="10" y1="14" x2="21" y2="3"/></svg>
									Open file
								</button>
								{#if canRevealFolder(block)}

									<button class="chat-rich-action-button" type="button" on:click={() => openFolder(block)} title="Reveal the containing folder in your OS file manager">
									<svg xmlns="http://www.w3.org/2000/svg" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/></svg>
									Reveal in Finder
								</button>

								{/if}
							</div>
						</div>
					{:else}
						<div class="chat-rich-file-block">
							<button class="chat-rich-file" type="button" on:click={() => openFileInBrowser(href)}>
								<div class="chat-rich-file-name">
									{block.label ?? block.display_name ?? block.relative_path ?? 'file'}
								</div>
								<div class="chat-rich-file-meta">
									{block.mime_type ?? 'application/octet-stream'}
									{#if formatBytes(block.size)}
										<span>&middot; {formatBytes(block.size)}</span>
									{/if}
								</div>
							</button>
							{#if displayPath(block)}
								<div class="chat-rich-path">{displayPath(block)}</div>
							{/if}
							<div class="chat-rich-actions">
								<button class="chat-rich-action-button" type="button" on:click={() => openFileInBrowser(href)} title="Open file in a new browser tab">
									<svg xmlns="http://www.w3.org/2000/svg" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"/><polyline points="15 3 21 3 21 9"/><line x1="10" y1="14" x2="21" y2="3"/></svg>
									Open file
								</button>
								{#if canRevealFolder(block)}

									<button class="chat-rich-action-button" type="button" on:click={() => openFolder(block)} title="Reveal the containing folder in your OS file manager">
									<svg xmlns="http://www.w3.org/2000/svg" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/></svg>
									Reveal in Finder
								</button>

								{/if}
							</div>
						</div>
					{/if}
				{/if}
			{:else if block.type === 'url' && block.url}
				{@const isImg = (block.mime_type ?? '').startsWith('image/')}
				{#if isImg}
					<div class="chat-rich-file-block">
						<a class="chat-rich-image-link" href={block.url} target="_blank" rel="noreferrer">
							<img
								class="chat-rich-image"
								src={block.url}
								alt={block.label ?? block.display_name ?? 'image'}
								loading="lazy"
							/>
						</a>
						<div class="chat-rich-image-meta">
							{#if block.label || block.display_name}
								<div class="chat-rich-image-name">
									{block.label ?? block.display_name}
								</div>
							{/if}
							<div class="chat-rich-actions">
								<a class="chat-rich-action-link" href={block.url} target="_blank" rel="noreferrer" title="Open the source URL in a new browser tab">
									<svg xmlns="http://www.w3.org/2000/svg" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"/><polyline points="15 3 21 3 21 9"/><line x1="10" y1="14" x2="21" y2="3"/></svg>
									Open original
								</a>
							</div>
						</div>
					</div>
				{:else}
					<div class="chat-rich-file-block">
						<a class="chat-rich-file" href={block.url} target="_blank" rel="noreferrer" title="Open this URL in a new browser tab">
							<div class="chat-rich-file-name">
								{block.label ?? block.display_name ?? block.url}
							</div>
							<div class="chat-rich-file-meta">
								{block.mime_type ?? 'link'}
							</div>
						</a>
					</div>
				{/if}
			{/if}
		{/each}
	</div>
{/if}

<style>
	.chat-rich-blocks {
		display: grid;
		gap: 0.75rem;
		margin-top: 0.75rem;
	}

	.chat-rich-text {
		margin: 0;
		white-space: pre-wrap;
	}

	.chat-rich-file-block {
		display: grid;
		gap: 0.45rem;
	}

	.chat-rich-image-link {
		display: grid;
		gap: 0.45rem;
		appearance: none;
		margin: 0;
		padding: 0;
		border: 0;
		background: transparent;
		color: inherit;
		font: inherit;
		text-align: left;
		text-decoration: none;
		cursor: pointer;
	}

	.chat-rich-image {
		display: block;
		/* Compact thumbnail — quick visual context; click opens full
		   resolution in a new tab. Smaller than the previous 28rem/18rem
		   defaults to match the "Open in tab" model for non-image media
		   (one consistent compact-card aesthetic across types). */
		max-width: min(100%, 18rem);
		max-height: 12rem;
		border-radius: 0.7rem;
		border: 1px solid color-mix(in srgb, var(--text-primary, #111827) 10%, transparent);
		background: color-mix(in srgb, var(--bg-elevated, #fff) 92%, #f3f4f6);
		object-fit: cover;
	}

	/* Open-in-tab card — used for PDF / HTML / video / audio. Compact
	   horizontal pill: icon + (filename + meta) + actions. Clicking
	   "Open ↗" launches the native browser viewer in a fresh tab.
	   Replaces the per-type inline iframes / <video> / <audio> for a
	   consistent "open externally" model across types. */
	.chat-rich-open-card {
		display: grid;
		/* Row 1: [icon] [filename + meta].  Row 2: the action buttons (dropped
		   onto their own row via `.chat-rich-open-card-actions { grid-column: 2 }`).
		   Previously a third `auto` column held the buttons inline, which squeezed
		   the `1fr` name column and made long filenames wrap across several lines.
		   Giving the name the full row keeps it on one line (until genuinely huge). */
		grid-template-columns: auto 1fr;
		align-items: center;
		gap: 0.5rem 0.75rem;
		max-width: min(100%, 32rem);
		padding: 0.6rem 0.85rem;
		border-radius: 0.85rem;
		border: 1px solid color-mix(in srgb, var(--text-primary, #111827) 12%, transparent);
		background: color-mix(in srgb, var(--bg-elevated, #fff) 96%, #f3f4f6);
	}

	.chat-rich-open-card-icon {
		display: grid;
		place-items: center;
		width: 2.2rem;
		height: 2.2rem;
		border-radius: 0.6rem;
		background: color-mix(in srgb, currentColor 12%, transparent);
	}
	.chat-rich-open-card-icon--pdf { color: #dc2626; }
	.chat-rich-open-card-icon--html { color: #2563eb; }
	.chat-rich-open-card-icon--video { color: #7c3aed; }
	.chat-rich-open-card-icon--audio { color: #059669; }

	.chat-rich-open-card-body {
		display: grid;
		gap: 0.15rem;
		min-width: 0;
	}
	.chat-rich-open-card-name {
		font-size: 0.85rem;
		font-weight: 600;
		color: var(--text-primary, #2d3436);
		word-break: break-word;
	}
	.chat-rich-open-card-meta {
		display: flex;
		flex-wrap: wrap;
		gap: 0.4rem;
		font-size: 0.7rem;
		color: var(--text-muted, #7b8588);
	}
	.chat-rich-open-card-mime {
		font-family: var(--font-mono, ui-monospace, monospace);
	}
	.chat-rich-open-card-actions {
		display: flex;
		flex-wrap: wrap;
		gap: 0.35rem;
		align-items: center;
		/* Second row, aligned under the filename (grid col 2). Buttons wrap among
		   themselves if the panel is very narrow rather than overflowing. */
		grid-column: 2;
	}

	.chat-rich-text-preview-header {
		display: grid;
		gap: 0.15rem;
		padding-inline: 0.15rem;
	}

	.chat-rich-text-preview-status {
		font-size: 0.8rem;
		opacity: 0.72;
	}

	.chat-rich-text-preview-body {
		max-height: 22rem;
		overflow: auto;
		border-radius: 0.6rem;
		border: 1px solid color-mix(in srgb, var(--text-primary, #111827) 12%, transparent);
		background: color-mix(in srgb, var(--bg-elevated, #fff) 95%, #f8fafc);
		padding: 0.6rem 0.8rem;
	}

	.chat-rich-text-preview-pre {
		margin: 0;
		font-family: var(--font-mono);
		font-size: 0.85rem;
		white-space: pre-wrap;
		word-break: break-word;
	}

	/* JSON preview override: keep the indented structure visible by
	   preserving whitespace verbatim (no soft-wrapping in the middle
	   of an indented level — `pre` instead of `pre-wrap`) and let long
	   keys/values scroll horizontally inside the body. Reads like a
	   compact JSON viewer rather than a wall of wrapped tokens. */
	.chat-rich-text-preview-json {
		white-space: pre;
		word-break: normal;
		overflow-x: auto;
		line-height: 1.4;
		tab-size: 2;
	}

	.chat-rich-image-meta {
		display: grid;
		gap: 0.15rem;
		padding-inline: 0.15rem;
	}

	.chat-rich-image-name {
		font-size: 0.92rem;
		font-weight: 600;
		word-break: break-word;
	}

	.chat-rich-image-size {
		font-size: 0.8rem;
		opacity: 0.72;
		display: grid;
		gap: 0.18rem;
	}

	.chat-rich-file {
		display: flex;
		flex-direction: column;
		gap: 0.2rem;
		appearance: none;
		width: 100%;
		margin: 0;
		padding: 0.8rem 0.9rem;
		border-radius: 0.9rem;
		border: 1px solid color-mix(in srgb, var(--text-primary, #111827) 12%, transparent);
		background: color-mix(in srgb, var(--bg-elevated, #fff) 95%, #f8fafc);
		color: inherit;
		font: inherit;
		text-align: left;
		text-decoration: none;
		cursor: pointer;
	}

	.chat-rich-file:hover {
		border-color: color-mix(in srgb, var(--accent-primary, #2563eb) 35%, transparent);
		background: color-mix(in srgb, var(--accent-primary, #2563eb) 6%, var(--bg-elevated, #fff));
	}

	.chat-rich-file-name {
		font-weight: 600;
		word-break: break-word;
	}

	.chat-rich-file-meta {
		font-size: 0.82rem;
		opacity: 0.72;
		word-break: break-word;
	}

	.chat-rich-path {
		font-size: 0.78rem;
		line-height: 1.4;
		word-break: break-word;
		font-family: var(--font-mono);
		opacity: 0.8;
	}

	.chat-rich-actions {
		display: flex;
		flex-wrap: wrap;
		gap: 0.4rem;
		align-items: center;
		margin-top: 0.15rem;
	}

	.chat-rich-action-link,
	.chat-rich-action-button {
		display: inline-flex;
		align-items: center;
		gap: 0.32rem;
		font-size: 0.78rem;
		font-weight: 500;
		font-family: inherit;
		color: var(--text-primary, #1f2937);
		text-decoration: none;
		background: color-mix(in srgb, var(--text-primary, #111827) 5%, transparent);
		border: 1px solid color-mix(in srgb, var(--text-primary, #111827) 12%, transparent);
		border-radius: 0.5rem;
		padding: 0.28rem 0.6rem;
		cursor: pointer;
		transition: background 120ms ease, border-color 120ms ease, color 120ms ease;
	}

	.chat-rich-action-button:hover,
	.chat-rich-action-link:hover {
		background: color-mix(in srgb, var(--accent-primary, #2563eb) 10%, transparent);
		border-color: color-mix(in srgb, var(--accent-primary, #2563eb) 40%, transparent);
		color: var(--accent-primary, #2563eb);
	}

	.chat-rich-action-link :global(svg),
	.chat-rich-action-button :global(svg) {
		opacity: 0.78;
		flex-shrink: 0;
	}

	.chat-rich-action-link:hover :global(svg),
	.chat-rich-action-button:hover :global(svg) {
		opacity: 1;
	}
</style>
