<script lang="ts">
	import { createEventDispatcher, onMount, tick } from 'svelte';
	import {
		type ChipKind,
		type ChipFieldToken,
		buildChipHtml,
		normalizeKind,
		renderValueToHtml,
		serializeChipDom
	} from '$lib/magician/chat/chipMarkup';

	export type { ChipKind };

	export let value = '';
	export let placeholder = '';
	export let disabled = false;
	export let minHeight = 32;
	export let maxHeight = 220;
	/** Soft-keyboard hints. The contenteditable host honours these on
	 *  mobile browsers in the same way a `<textarea>` does. Leave at
	 *  their defaults for the desktop composer; set them through from
	 *  the mobile composer where the "Send" affordance on the keyboard
	 *  is part of the affordance contract. */
	export let inputmode:
		| 'none'
		| 'text'
		| 'search'
		| 'email'
		| 'url'
		| 'tel'
		| 'numeric'
		| 'decimal' = 'text';
	export let enterkeyhint:
		| 'enter'
		| 'done'
		| 'go'
		| 'next'
		| 'previous'
		| 'search'
		| 'send'
		| undefined = undefined;
	export let autocapitalize: 'off' | 'none' | 'sentences' | 'words' | 'characters' = 'sentences';
	let className = '';
	export { className as class };

	let editorEl: HTMLDivElement | null = null;
	let lastEmittedValue = '';

	const dispatch = createEventDispatcher<{
		input: void;
		keydown: KeyboardEvent;
		focus: void;
		blur: void;
	}>();

	// Render external `value` mutations into the DOM. When the user types,
	// we update `value` ourselves and stash the result in
	// `lastEmittedValue`; the reactive watcher skips re-rendering in that
	// case so the caret stays put.
	$: if (typeof window !== 'undefined' && editorEl && value !== lastEmittedValue) {
		editorEl.innerHTML = renderValueToHtml(value);
		lastEmittedValue = value;
	}

	onMount(() => {
		if (editorEl && value) {
			editorEl.innerHTML = renderValueToHtml(value);
			lastEmittedValue = value;
		}
	});

	function emit(): void {
		if (!editorEl) return;
		const serialized = serializeChipDom(editorEl);
		if (serialized !== lastEmittedValue) {
			lastEmittedValue = serialized;
			value = serialized;
		}
		dispatch('input');
	}

	function handleInput(): void {
		emit();
	}

	function handleKeydown(event: KeyboardEvent): void {
		// Delete an adjacent chip as a single unit before the browser
		// peels off its inner text. Covers both Backspace (delete left)
		// and Delete (delete right).
		if (
			(event.key === 'Backspace' || event.key === 'Delete') &&
			!event.altKey &&
			!event.metaKey &&
			!event.ctrlKey
		) {
			const sel = window.getSelection();
			if (sel && sel.isCollapsed && sel.rangeCount > 0) {
				const range = sel.getRangeAt(0);
				const adjacent = findAdjacentChip(range, event.key === 'Backspace');
				if (adjacent) {
					event.preventDefault();
					adjacent.remove();
					emit();
					return;
				}
			}
		}
		dispatch('keydown', event);
	}

	function findAdjacentChip(range: Range, backward: boolean): HTMLElement | null {
		const startContainer = range.startContainer;
		const startOffset = range.startOffset;

		if (backward) {
			if (startContainer.nodeType === Node.TEXT_NODE) {
				if (startOffset > 0) return null;
				return walkPrevSibling(startContainer);
			}
			const el = startContainer as Element;
			const node = el.childNodes[startOffset - 1] ?? null;
			if (node instanceof HTMLElement && node.dataset.chip === '1') return node;
			if (!node && el !== editorEl) return walkPrevSibling(el);
			return null;
		}

		if (startContainer.nodeType === Node.TEXT_NODE) {
			const txt = startContainer.textContent ?? '';
			if (startOffset < txt.length) return null;
			return walkNextSibling(startContainer);
		}
		const el = startContainer as Element;
		const node = el.childNodes[startOffset] ?? null;
		if (node instanceof HTMLElement && node.dataset.chip === '1') return node;
		if (!node && el !== editorEl) return walkNextSibling(el);
		return null;
	}

	function walkPrevSibling(node: Node): HTMLElement | null {
		let cur: Node | null = node;
		while (cur && cur !== editorEl) {
			const prev = cur.previousSibling;
			if (prev) {
				if (prev instanceof HTMLElement && prev.dataset.chip === '1') return prev;
				return null;
			}
			cur = cur.parentNode;
		}
		return null;
	}

	function walkNextSibling(node: Node): HTMLElement | null {
		let cur: Node | null = node;
		while (cur && cur !== editorEl) {
			const next = cur.nextSibling;
			if (next) {
				if (next instanceof HTMLElement && next.dataset.chip === '1') return next;
				return null;
			}
			cur = cur.parentNode;
		}
		return null;
	}

	function handlePaste(event: ClipboardEvent): void {
		// Strip formatting on paste — only plain text is allowed. Any
		// `<kind>:<slug>` tokens it contains will be re-chipped via the
		// next external value-set cycle. Keeps the editor invariant
		// "every chip in the DOM came from our renderer" intact.
		event.preventDefault();
		const text = event.clipboardData?.getData('text/plain') ?? '';
		if (!text) return;
		const sel = window.getSelection();
		if (!sel || sel.rangeCount === 0) return;
		const range = sel.getRangeAt(0);
		range.deleteContents();
		const node = document.createTextNode(text);
		range.insertNode(node);
		range.setStartAfter(node);
		range.collapse(true);
		sel.removeAllRanges();
		sel.addRange(range);
		emit();
	}

	// ---------------------------------------------------------------------
	// Public API — methods callers reach for via `bind:this`.
	// ---------------------------------------------------------------------

	export function focus(): void {
		void tick().then(() => {
			editorEl?.focus();
			if (
				editorEl &&
				(!window.getSelection()?.rangeCount ||
					!editorEl.contains(window.getSelection()?.anchorNode ?? null))
			) {
				placeCaretAtEnd();
			}
		});
	}

	function placeCaretAtEnd(): void {
		if (!editorEl) return;
		const range = document.createRange();
		range.selectNodeContents(editorEl);
		range.collapse(false);
		const sel = window.getSelection();
		sel?.removeAllRanges();
		sel?.addRange(range);
	}

	/** Text content between the start of the editor and the current caret,
	 *  serialized with `<kind>:<slug>` for chips. Used by the mention
	 *  picker to run its trigger regex. */
	export function textBeforeCaret(): string {
		if (!editorEl) return '';
		const sel = window.getSelection();
		if (!sel || sel.rangeCount === 0) return '';
		const range = sel.getRangeAt(0);
		if (!editorEl.contains(range.startContainer)) return '';
		const preRange = range.cloneRange();
		preRange.selectNodeContents(editorEl);
		preRange.setEnd(range.startContainer, range.startOffset);
		const frag = preRange.cloneContents();
		const wrapper = document.createElement('div');
		wrapper.appendChild(frag);
		return serializeChipDom(wrapper);
	}

	/** Structured readout of every chip currently in the field, in left-to-right
	 *  (document) order. Unlike `value` / `textBeforeCaret()` — which serialize
	 *  chips to `kind:slug` and DROP the display label — this walks the live DOM,
	 *  so the human label survives (a task chip yields `slug`=<id>, `label`=<title>).
	 *  Reads `data-slug` (the precise id), never the label, so callers stay exact. */
	export function chipTokens(): ChipFieldToken[] {
		if (!editorEl) return [];
		const out: ChipFieldToken[] = [];
		for (const el of Array.from(editorEl.querySelectorAll<HTMLElement>('[data-chip="1"]'))) {
			const slug = el.dataset.slug ?? '';
			if (!slug) continue;
			out.push({
				kind: normalizeKind(el.dataset.kind ?? 'agent'),
				slug,
				label: el.dataset.label ?? slug
			});
		}
		return out;
	}

	/** Replace the last `consumeChars` characters before the caret with a
	 *  chip span. Used by the mention picker to commit a selection. */
	export function replaceBeforeCaretWithChip(
		consumeChars: number,
		kind: ChipKind,
		slug: string,
		appendSpace = true,
		/** Human label to show inside the chip (e.g. a task TITLE while `slug` is
		 *  the task id). Serialization still emits `kind:slug`. */
		displayLabel?: string
	): string {
		if (!editorEl) return '';
		const sel = window.getSelection();
		if (!sel || sel.rangeCount === 0) return '';
		if (!editorEl.contains(sel.getRangeAt(0).startContainer)) return '';

		let remaining = consumeChars;
		while (remaining > 0) {
			const probe = sel.getRangeAt(0);
			const prevPosition = stepLeftOnce(probe);
			if (!prevPosition) break;
			probe.setStart(prevPosition.node, prevPosition.offset);
			probe.deleteContents();
			remaining -= 1;
		}

		const wrapper = document.createElement('span');
		wrapper.innerHTML = buildChipHtml(kind, slug, undefined, displayLabel);
		const chipNode = wrapper.firstElementChild as HTMLElement;
		const insertRange = sel.getRangeAt(0);
		insertRange.insertNode(chipNode);

		if (appendSpace) {
			const spaceNode = document.createTextNode('\u00a0');
			chipNode.after(spaceNode);
			const r = document.createRange();
			r.setStartAfter(spaceNode);
			r.collapse(true);
			sel.removeAllRanges();
			sel.addRange(r);
		} else {
			const r = document.createRange();
			r.setStartAfter(chipNode);
			r.collapse(true);
			sel.removeAllRanges();
			sel.addRange(r);
		}
		emit();
		return `${kind}:${slug}`;
	}

	function stepLeftOnce(range: Range): { node: Node; offset: number } | null {
		const { startContainer, startOffset } = range;
		if (startContainer.nodeType === Node.TEXT_NODE) {
			if (startOffset > 0) return { node: startContainer, offset: startOffset - 1 };
		} else if (startOffset > 0) {
			const child = (startContainer as Element).childNodes[startOffset - 1];
			if (child.nodeType === Node.TEXT_NODE) {
				const len = (child.textContent ?? '').length;
				return { node: child, offset: len > 0 ? len - 1 : 0 };
			}
			return { node: startContainer, offset: startOffset - 1 };
		}
		let cur: Node | null = startContainer;
		while (cur && cur !== editorEl) {
			const prev = cur.previousSibling;
			if (prev) {
				if (prev.nodeType === Node.TEXT_NODE) {
					const len = (prev.textContent ?? '').length;
					return { node: prev, offset: len > 0 ? len - 1 : 0 };
				}
				if (prev instanceof HTMLElement) {
					const parent = prev.parentNode;
					if (!parent) return null;
					const idx = Array.prototype.indexOf.call(parent.childNodes, prev);
					return { node: parent, offset: idx };
				}
			}
			cur = cur.parentNode;
		}
		return null;
	}

	$: showPlaceholder = !value || value.length === 0;
</script>

<div
	class="chip-textarea-wrap {className}"
	class:disabled
	style:--chip-min-height={`${minHeight}px`}
	style:--chip-max-height={`${maxHeight}px`}
>
	<div
		class="chip-textarea"
		bind:this={editorEl}
		contenteditable={!disabled}
		role="textbox"
		tabindex={disabled ? -1 : 0}
		aria-multiline="true"
		aria-disabled={disabled}
		on:input={handleInput}
		on:keydown={handleKeydown}
		on:paste={handlePaste}
		on:focus={() => dispatch('focus')}
		on:blur={() => dispatch('blur')}
		spellcheck="true"
		{inputmode}
		{enterkeyhint}
		{autocapitalize}
	></div>
	{#if showPlaceholder && placeholder}
		<span class="chip-textarea__placeholder" aria-hidden="true">{placeholder}</span>
	{/if}
</div>

<style>
	.chip-textarea-wrap {
		position: relative;
		flex: 1;
		min-width: 0;
		display: flex;
	}

	.chip-textarea {
		flex: 1;
		min-width: 0;
		background: transparent;
		border: 0;
		outline: 0;
		font-family: var(--font-primary);
		font-size: 14.5px;
		line-height: 22px;
		color: var(--text-primary, #1a1a1a);
		min-height: var(--chip-min-height, 32px);
		max-height: var(--chip-max-height, 220px);
		padding: 5px 0;
		letter-spacing: -0.005em;
		overflow-y: auto;
		white-space: pre-wrap;
		word-break: break-word;
		cursor: text;
	}

	.chip-textarea:focus {
		outline: 0;
	}

	.chip-textarea-wrap.disabled .chip-textarea {
		opacity: 0.6;
		cursor: not-allowed;
	}

	.chip-textarea__placeholder {
		position: absolute;
		top: 5px;
		left: 0;
		pointer-events: none;
		color: var(--text-faint, #aaa);
		font-family: var(--font-primary);
		font-size: 14.5px;
		line-height: 22px;
		letter-spacing: -0.005em;
	}

	/* Chip palette is global — see `src/app.css` (`.chip`, `.chip--*`).
	   Same atoms render in the transcript via ChatMarkdown's chipify
	   pass, so the styles live in one place rather than being scoped
	   to this component. */
</style>
