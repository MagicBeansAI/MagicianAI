<script lang="ts">
	import { getContext } from 'svelte';
	import { goto } from '$app/navigation';
	import Markdown from '$lib/magician/components/generative/Markdown.svelte';
	import {
		CHAT_TASK_PANEL_OPENER,
		type ChatTaskPanelOpener,
	} from '$lib/magician/chat/taskPanelContext';
	import { showError } from '$lib/shared/stores/notifications';
	import { timedFetch } from '$lib/shared/fetch';
	import {
		classifyRef,
		refOpenMode,
		resolveHref,
	} from '$lib/magician/links/classifyRef';
	import {
		attachPathActions,
		buildPathLink,
		type PathActionHandlers,
	} from '$lib/magician/links/pathActions';
	import {
		buildChipElement,
		chipTokenRegex,
		parseChipMatch,
	} from '$lib/magician/chat/chipMarkup';
	import { get } from 'svelte/store';
	import { taskStore } from '$lib/stores/taskStore';

	export let content: unknown = '';
	/**
	 * Optional chat session id. When set, the rendered markdown is
	 * scanned for filesystem paths inside `<code>` blocks; matching
	 * paths get two trailing affordance icons (folder reveal /
	 * file open) wired to the chat session's `open-folder` and
	 * `open-file` endpoints. Paths outside the session/workspace
	 * scope get rejected by the backend and surface a toast.
	 *
	 * Omit `sessionId` for non-chat-surface markdown rendering — no
	 * scanning happens, no click handlers attached.
	 */
	export let sessionId: string | null = null;
	/**
	 * Deprecated — see `Markdown.svelte::streaming`. Was the streaming
	 * fast-path opt-in; v0.6.604 traced the bubble re-render to
	 * `animate:flip` on the parent row instead, so this prop is now a
	 * no-op. Retained until callers stop passing it.
	 */
	export let streaming = false;
	void streaming;

	// When rendered inside a chat surface, ChatPanel provides this so a task
	// id mentioned in LLM prose opens the chat-owned ExecutionPanel in place
	// (resolving Internal tasks the /tasks feed can't) instead of navigating
	// to /tasks. Undefined elsewhere → fall back to normal href navigation.
	const openTaskInPanel = getContext<ChatTaskPanelOpener | undefined>(
		CHAT_TASK_PANEL_OPENER,
	);

	// Matches a path inside an inline code block. We linkify when the
	// content looks like a filesystem path: leading `/`, leading `~/`,
	// or a relative path with a directory separator + file extension
	// (so we don't linkify language constructs like `foo.bar`). Conservative
	// on purpose — false positives are worse than false negatives because
	// they make innocuous backticks pop with phantom click handlers.
	const PATH_RE =
		/^(?:file:\/{0,3}\/[^\s]+|\/[^\s]+|~\/[^\s]+|[a-zA-Z0-9_.\-]+(?:\/[^\s]+)+\.[a-zA-Z0-9]+)$/;

	/** Strip a `file://` (or `file:///`, `file:`) prefix and return the
	 *  underlying absolute path. Returns the input unchanged when there
	 *  is no `file:` scheme. Used by both the inline-code linkifier and
	 *  the `<a href="...">` enhancer so they agree on canonicalization. */
	function stripFileScheme(value: string): string {
		if (!/^file:/i.test(value)) return value;
		const stripped = value.replace(/^file:\/{0,3}/i, '/');
		return stripped.startsWith('/') ? stripped : value;
	}

	type OpenAction = 'folder' | 'file';

	async function openPath(absolutePath: string, action: OpenAction): Promise<void> {
		if (!sessionId) return;
		const endpoint =
			`/api/magician/v2/chat/sessions/${encodeURIComponent(sessionId)}` +
			`/outputs/open-${action}`;
		const response = await timedFetch(endpoint, {
			method: 'POST',
			headers: { 'Content-Type': 'application/json' },
			// `absolute_path` is the only field we can supply with high
			// confidence from raw markdown text — we don't know source/
			// relative_path. The backend's `resolve_openable_output_path`
			// validates the path against the scope root and rejects
			// anything outside (or not session-referenced).
			body: JSON.stringify({ absolute_path: absolutePath }),
		});
		if (response.ok) return;
		let fallback =
			action === 'folder'
				? "Couldn't open that folder (path may be outside the workspace)"
				: "Couldn't open that file (path may be outside the workspace)";
		try {
			const payload = await response.json();
			if (typeof payload?.error === 'string' && payload.error.trim()) {
				fallback = payload.error;
			}
		} catch {
			// fall through to default message
		}
		showError(fallback);
	}

	/** Shared OS-action handlers wired to this surface's chat-session
	 *  scope. Passed into the `pathActions.ts` DOM builders so every
	 *  path affordance (inline-text, code-block, anchor) routes through
	 *  the same endpoint with the same trust gate. */
	const pathHandlers: PathActionHandlers = {
		onOpenFile: (absolutePath) => openPath(absolutePath, 'file'),
		onRevealFolder: (absolutePath) => openPath(absolutePath, 'folder'),
	};

	function annotateCodeAsPath(el: HTMLElement, raw: string): void {
		el.setAttribute('data-path-linked', '1');
		el.classList.add('chat-markdown-path');
		el.setAttribute(
			'role',
			'button',
		);
		el.setAttribute('tabindex', '0');
		el.setAttribute(
			'title',
			`Click to open ${raw} in its default application; use the icon to reveal the folder`,
		);
		// Make the code element itself the open-file click target so the
		// behaviour matches bare-prose paths (where the text is the link).
		// The reveal-folder icon next to it covers the second action.
		const openFile = (event: MouseEvent | KeyboardEvent): void => {
			if (event instanceof KeyboardEvent) {
				if (event.key !== 'Enter' && event.key !== ' ') return;
				event.preventDefault();
			} else {
				if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) {
					return;
				}
				event.preventDefault();
				event.stopPropagation();
			}
			void openPath(raw, 'file');
		};
		el.addEventListener('click', openFile);
		el.addEventListener('keydown', openFile);
		// Shared `attachPathActions` appends the reveal+open icon pair
		// right after the <code> element. Same chrome as the inline-text
		// and anchor variants — see `pathActions.ts`.
		attachPathActions(el, raw, 'sm', pathHandlers);
	}

	function linkifyPaths(node: HTMLElement): { destroy(): void } {
		if (!sessionId) return { destroy() {} };
		const annotate = () => {
			const codeEls = node.querySelectorAll<HTMLElement>('code:not([data-path-linked])');
			codeEls.forEach((el) => {
				const raw = el.textContent?.trim() ?? '';
				if (!raw || !PATH_RE.test(raw)) return;
				// Pass the absolute path (sans `file://` scheme) to the
				// open-folder/open-file endpoints. The displayed text
				// stays unchanged.
				const absolute = stripFileScheme(raw);
				annotateCodeAsPath(el, absolute);
			});
		};
		annotate();
		// Markdown may re-render when `content` updates (streaming
		// assistant turns); observe DOM mutations so newly-rendered
		// <code> elements get the same treatment.
		const observer = new MutationObserver(annotate);
		observer.observe(node, { childList: true, subtree: true });
		return {
			destroy() {
				observer.disconnect();
			},
		};
	}

	/**
	 * Scan all `<a href>` elements after render, classify each href via
	 * the shared `classifyRef` utility, and either:
	 *   - rewrite the href + intercept clicks for in-app navigation
	 *     (task / execution / briefing / dashboard / agent / skill)
	 *   - leave external URLs untouched (open in new tab as usual)
	 *   - strip the href on `unknown` to avoid broken nav
	 *
	 * This is the catch-all that complements server-side structured
	 * cards (`task_status_update`, `pack_progress`, …): when the LLM
	 * inlines an internal reference as prose, this turns it into a
	 * working click. See `classifyRef.ts` for the resource taxonomy.
	 */
	function enhanceLinks(node: HTMLElement): { destroy(): void } {
		const enhance = () => {
			const anchors = node.querySelectorAll<HTMLAnchorElement>(
				'a[href]:not([data-ref-enhanced])',
			);
			anchors.forEach((a) => {
				const originalHref = a.getAttribute('href') ?? '';
				if (!originalHref) return;
				const ref = classifyRef(originalHref);
				a.setAttribute('data-ref-enhanced', '1');
				a.setAttribute('data-ref-kind', ref.kind);

				if (ref.kind === 'external') {
					// Make sure target=_blank + safe rel attrs.
					a.setAttribute('target', '_blank');
					a.setAttribute('rel', 'noopener noreferrer');
					return;
				}

				if (ref.kind === 'unknown') {
					// Demote to plain text so the browser doesn't try
					// to navigate to a meaningless href.
					a.removeAttribute('href');
					a.classList.add('chat-markdown-link-disabled');
					a.setAttribute(
						'title',
						`Couldn't resolve "${originalHref}" to anything navigable`,
					);
					return;
				}

				const target = resolveHref(ref);
				if (!target) {
					a.removeAttribute('href');
					return;
				}

				const mode = refOpenMode(ref);
				a.setAttribute('href', target);
				a.classList.add('chat-markdown-link-internal');
				if (mode === 'newTab') {
					a.setAttribute('target', '_blank');
					a.setAttribute('rel', 'noopener noreferrer');
				} else if (mode === 'osAction' && ref.kind === 'filePath') {
					// Intercept: POST the absolute path to the chat
					// session's open-file endpoint. The backend re-
					// validates against the scope root so an out-of-
					// scope path returns an error instead of opening.
					a.classList.add('chat-markdown-link-filepath');
					a.setAttribute(
						'title',
						`Open ${ref.absolutePath} in its default application`,
					);
					a.addEventListener('click', (event) => {
						event.preventDefault();
						event.stopPropagation();
						void openPath(ref.absolutePath, 'file');
					});
					// Attach the shared reveal+open icon pair after the
					// anchor so the user can also get "Reveal in Finder"
					// without re-clicking. Same chrome as code blocks +
					// inline-prose paths.
					attachPathActions(a, ref.absolutePath, 'sm', pathHandlers);
				} else {
					a.addEventListener('click', (event) => {
						// Modifier-clicks (cmd/ctrl/shift) get browser default.
						if (
							event.metaKey ||
							event.ctrlKey ||
							event.shiftKey ||
							event.altKey
						) {
							return;
						}
						event.preventDefault();
						// Task mentions open the chat-owned ExecutionPanel in
						// place when a chat surface provided the opener — this
						// resolves Internal tasks the /tasks deep-link can't, and
						// keeps the user in chat. Fall back to navigation
						// elsewhere (e.g. non-chat markdown).
						if (ref.kind === 'task' && openTaskInPanel) {
							openTaskInPanel(ref.id);
							return;
						}
						void goto(target);
					});
				}
			});
		};
		enhance();
		const observer = new MutationObserver(enhance);
		observer.observe(node, { childList: true, subtree: true });
		return {
			destroy() {
				observer.disconnect();
			},
		};
	}

	/**
	 * Auto-linkify absolute filesystem paths in plain prose. Complements
	 * linkifyPaths (code blocks) and enhanceLinks (anchors). Without
	 * this, text like "saved at /private/tmp/foo.jpg" renders dead.
	 *
	 * Walks text nodes, finds path-like substrings, replaces each with a
	 * clickable button wired to the chat-session open-file endpoint.
	 * Same backend trust gate (scope root + session reference check).
	 *
	 * Pattern: optional file:// prefix, leading /, at least one more /,
	 * trailing .ext of 1-8 alphanumerics. Skips text under a/code/pre/
	 * button/input/textarea/script/style.
	 */
	function linkifyTextPaths(node: HTMLElement): { destroy(): void } {
		if (!sessionId) return { destroy() {} };
		const SKIP_ANCESTORS = new Set([
			'A',
			'BUTTON',
			'CODE',
			'INPUT',
			'PRE',
			'SCRIPT',
			'STYLE',
			'TEXTAREA',
		]);
		const PATH_IN_TEXT_RE =
			/(?:file:\/{0,3})?\/(?:[^\s<>"'/]+\/)+[^\s<>"'/]+\.[A-Za-z0-9]{1,8}/g;

		function shouldSkip(textNode: Node): boolean {
			let parent: Node | null = textNode.parentNode;
			while (parent && parent !== node) {
				if (parent.nodeType === Node.ELEMENT_NODE) {
					const el = parent as HTMLElement;
					if (SKIP_ANCESTORS.has(el.tagName)) return true;
				}
				parent = parent.parentNode;
			}
			return false;
		}

		function wrapPath(rawMatch: string): HTMLSpanElement {
			const absolute = stripFileScheme(rawMatch);
			// `buildPathLink` returns text (clickable → open file) + the
			// shared reveal+open icon pair. Same component used for code
			// blocks and anchors, so bare-prose paths get the full
			// affordance suite (the previous custom button lacked the
			// reveal-in-finder icon).
			return buildPathLink(absolute, rawMatch, 'sm', pathHandlers);
		}

		function processTextNode(textNode: Text): void {
			const text = textNode.textContent ?? '';
			if (!text) return;
			PATH_IN_TEXT_RE.lastIndex = 0;
			if (!PATH_IN_TEXT_RE.test(text)) return;
			PATH_IN_TEXT_RE.lastIndex = 0;
			const fragment = document.createDocumentFragment();
			let lastIndex = 0;
			let match: RegExpExecArray | null;
			while ((match = PATH_IN_TEXT_RE.exec(text)) !== null) {
				if (match.index > lastIndex) {
					fragment.appendChild(
						document.createTextNode(text.slice(lastIndex, match.index)),
					);
				}
				fragment.appendChild(wrapPath(match[0]));
				lastIndex = match.index + match[0].length;
			}
			if (lastIndex < text.length) {
				fragment.appendChild(document.createTextNode(text.slice(lastIndex)));
			}
			textNode.parentNode?.replaceChild(fragment, textNode);
		}

		function annotate(): void {
			const walker = document.createTreeWalker(node, NodeFilter.SHOW_TEXT);
			const candidates: Text[] = [];
			let current: Node | null;
			while ((current = walker.nextNode())) {
				if (shouldSkip(current)) continue;
				candidates.push(current as Text);
			}
			candidates.forEach(processTextNode);
		}

		let pending = false;
		const observer = new MutationObserver(() => {
			if (pending) return;
			pending = true;
			queueMicrotask(() => {
				pending = false;
				annotate();
			});
		});
		annotate();
		observer.observe(node, { childList: true, subtree: true, characterData: true });
		return {
			destroy() {
				observer.disconnect();
			},
		};
	}

	/**
	 * Replace typed-prefix tokens (`agent:foo`, `skill:foo`,
	 * `personality:foo`) inside text nodes with chip spans. Mirrors
	 * `linkifyTextPaths` shape: TreeWalker over visible text nodes,
	 * skip the same set of ancestors so we never chip-ify content
	 * inside `<a>`, `<code>`, `<pre>`, etc. MutationObserver re-runs
	 * the pass on streaming assistant turns.
	 *
	 * The chip HTML is byte-identical to what the composer's
	 * ChipTextarea inserts (both go through `buildChipElement` in
	 * `chipMarkup.ts`), so a message looks the same in the input box
	 * before send as it does in the transcript bubble after send.
	 */
	function chipifyTokens(node: HTMLElement): { destroy(): void } {
		const SKIP_ANCESTORS = new Set([
			'A',
			'BUTTON',
			'CODE',
			'INPUT',
			'PRE',
			'SCRIPT',
			'STYLE',
			'TEXTAREA',
		]);

		function shouldSkip(textNode: Node): boolean {
			let parent: Node | null = textNode.parentNode;
			while (parent && parent !== node) {
				if (parent.nodeType === Node.ELEMENT_NODE) {
					const el = parent as HTMLElement;
					if (SKIP_ANCESTORS.has(el.tagName)) return true;
					// Don't re-chip an already-chipped span (idempotent
					// reruns on MutationObserver tick).
					if (el.classList && el.classList.contains('chip')) return true;
				}
				parent = parent.parentNode;
			}
			return false;
		}

		function processTextNode(textNode: Text): void {
			const text = textNode.textContent ?? '';
			if (!text) return;
			const matches = Array.from(text.matchAll(chipTokenRegex()));
			if (matches.length === 0) return;
			const fragment = document.createDocumentFragment();
			let lastIndex = 0;
			for (const match of matches) {
				const start = match.index ?? 0;
				if (start > lastIndex) {
					fragment.appendChild(document.createTextNode(text.slice(lastIndex, start)));
				}
				const token = parseChipMatch(match);
				if (token) {
					const chip = buildChipElement(token.kind, token.slug, token.routeAgent);
					// A task token serializes as `task:<id>` (opaque). Resolve the
					// human title from the store and show it in the chip via
					// textContent (safe — plain text), keeping `data-slug` = the id.
					if (token.kind === 'task') {
						const title = get(taskStore)
							.tasks.find((t) => t.id === token.slug)
							?.title?.trim();
						if (title) {
							chip.setAttribute('data-label', title);
							const labelEl = chip.querySelector('.chip__label');
							if (labelEl) labelEl.textContent = title;
						}
					}
					fragment.appendChild(chip);
				} else {
					fragment.appendChild(document.createTextNode(match[0]));
				}
				lastIndex = start + match[0].length;
			}
			if (lastIndex < text.length) {
				fragment.appendChild(document.createTextNode(text.slice(lastIndex)));
			}
			textNode.parentNode?.replaceChild(fragment, textNode);
		}

		function annotate(): void {
			const walker = document.createTreeWalker(node, NodeFilter.SHOW_TEXT);
			const candidates: Text[] = [];
			let current: Node | null;
			while ((current = walker.nextNode())) {
				if (shouldSkip(current)) continue;
				candidates.push(current as Text);
			}
			candidates.forEach(processTextNode);
		}

		let pending = false;
		const observer = new MutationObserver(() => {
			if (pending) return;
			pending = true;
			queueMicrotask(() => {
				pending = false;
				annotate();
			});
		});
		annotate();
		observer.observe(node, { childList: true, subtree: true, characterData: true });
		return {
			destroy() {
				observer.disconnect();
			},
		};
	}
</script>

<div
	class="chat-markdown"
	use:linkifyPaths
	use:enhanceLinks
	use:linkifyTextPaths
	use:chipifyTokens
>
	<Markdown {content} />
</div>

<style>
	.chat-markdown {
		min-width: 0;
	}

	.chat-markdown :global(.muij-markdown) {
		font: inherit;
		line-height: inherit;
		color: inherit;
	}

	.chat-markdown :global(.muij-markdown-empty) {
		display: none;
	}

	.chat-markdown :global(.muij-md-h1),
	.chat-markdown :global(.muij-md-h2),
	.chat-markdown :global(.muij-md-h3) {
		font-size: 1em;
		line-height: 1.45;
		margin: 0.35rem 0 0.15rem;
		color: inherit;
	}

	.chat-markdown :global(.muij-md-p) {
		margin: 0.25rem 0;
	}

	/* Trim the outer paragraph margins so a single-paragraph reply
	   sits flush with the bubble padding (no phantom gap at the top or
	   bottom). Adjacent paragraphs still get 0.25rem between them via
	   normal margin collapse, so multi-paragraph replies still read as
	   distinct blocks — but the bubble doesn't grow an extra gap each
	   time the streamed text crosses a `\n\n` boundary. Pairs with the
	   parser fix that wraps EVERY paragraph in `<p>`, including the
	   first; without these rules the bubble would gain a constant 0.5rem
	   of dead space the moment streaming starts. */
	.chat-markdown :global(.muij-md-p:first-child) {
		margin-top: 0;
	}
	.chat-markdown :global(.muij-md-p:last-child) {
		margin-bottom: 0;
	}

	.chat-markdown :global(.muij-md-ul) {
		margin: 0.3rem 0;
		padding-left: 1.15rem;
	}

	.chat-markdown :global(.muij-md-li) {
		margin: 0.12rem 0;
	}

	.chat-markdown :global(.muij-md-link) {
		color: inherit;
		text-decoration: underline;
		text-underline-offset: 0.14em;
	}

	.chat-markdown :global(.muij-md-code) {
		font-size: 0.92em;
	}

	/* Inline `<code>` blocks whose content matches a filesystem path
	   regex get this class via the `use:linkifyPaths` action. The code
	   element is itself wired as the open-file click target (same
	   behaviour as bare-prose paths); the trailing icon strip handles
	   reveal-folder. */
	.chat-markdown :global(.chat-markdown-path) {
		padding-right: 0.1em;
		cursor: pointer;
		text-decoration: underline;
		text-decoration-style: dotted;
		text-decoration-color: color-mix(in srgb, var(--accent-primary, currentColor) 55%, transparent);
		text-underline-offset: 2px;
		transition: text-decoration-color 120ms ease, background 120ms ease;
	}
	.chat-markdown :global(.chat-markdown-path:hover) {
		text-decoration-style: solid;
		text-decoration-color: var(--accent-primary, currentColor);
	}
	.chat-markdown :global(.chat-markdown-path:focus-visible) {
		outline: 2px solid var(--accent-primary, currentColor);
		outline-offset: 2px;
		border-radius: 2px;
	}

	/* The trailing two-icon strip (folder reveal + file open) is now
	   built by the shared `pathActions.ts` module and styled by the
	   global `.path-actions`/`.path-ref`/`.path-action` rules in
	   `app.css`. Single source of styling across chat, activity rows,
	   and any future surface that surfaces a path. */

	/* Internal in-app refs — task/exec/briefing/dashboard/etc. links
	   that the classifier rewrote to a SvelteKit-navigable URL.
	   Visually identical to other markdown links; the class is mostly
	   a hook for future styling and debugging via DevTools. */
	.chat-markdown :global(.chat-markdown-link-internal) {
		text-decoration-style: solid;
	}

	/* Refs the classifier could not resolve to anything — href was
	   stripped to prevent broken navigation. Muted styling so the
	   user can still read the original text without it looking like
	   a working link. */
	.chat-markdown :global(.chat-markdown-link-disabled) {
		color: inherit;
		text-decoration: underline;
		text-decoration-style: dotted;
		text-decoration-color: color-mix(in srgb, currentColor 30%, transparent);
		cursor: help;
	}
</style>
