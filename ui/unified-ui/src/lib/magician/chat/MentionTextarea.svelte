<script lang="ts">
	import { createEventDispatcher, tick } from 'svelte';
	import ChipTextarea from '$lib/magician/chat/ChipTextarea.svelte';
	import {
		type ComposerMentionItem,
		mentionMatchesFor,
		detectMentionTrigger
	} from '$lib/magician/chat/composerMentions';
	import type { ChipKind, ChipFieldToken } from '$lib/magician/chat/chipMarkup';

	/**
	 * The `@`-mention state machine + ChipTextarea, in one place.
	 *
	 * This wrapper owns everything that was previously copy-pasted into every
	 * composer (FloatingComposer): the mention trigger
	 * detection, the picker open/close state, the commit (`applyMention`), and the
	 * mention-only keyboard navigation. Hosts pass `mentionItems` + `bind:value`
	 * and keep ONLY their own chrome (submit chord, buttons) — see the keydown
	 * contract below.
	 *
	 * Option A (current): the wrapper EXPOSES picker state
	 * (`bind:mentionOpen`/`mentionMatches`/`mentionActiveIndex` + `applyMention()`)
	 * so each host renders its own `<MentionPicker>` dock at its own placement/CSS
	 * — zero layout change. (A later pass could fold the dock in via a placement
	 * prop + a trailing slot.)
	 */

	// ── ChipTextarea passthroughs (host owns the values) ──────────────────────
	export let value = '';
	export let placeholder = '';
	/** Fully-computed by the host (e.g. read-only, voice-live, sending). The
	 *  wrapper never recomputes it. */
	export let disabled = false;
	export let minHeight = 32;
	export let maxHeight = 220;
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

	// ── mention config ────────────────────────────────────────────────────────
	export let mentionItems: ComposerMentionItem[] = [];
	/** Gate the whole mention layer off (e.g. the dev composer variant). When
	 *  false the picker never opens, so the keyboard-nav branches never fire. */
	export let mentionsEnabled = true;
	export let mentionLimit = 9;
	/** Allow the bare `@…` query to span spaces, so multi-word labels (task titles)
	 *  can be narrowed (`@Q3 rev`). Leave false in chat — there, prose after a mention
	 *  should close the picker. See detectMentionTrigger. */
	export let allowSpacesInQuery = false;

	// ── picker state (bindable — the host renders the dock from these) ─────────
	export let mentionOpen = false;
	export let mentionMatches: ComposerMentionItem[] = [];
	export let mentionActiveIndex = 0;

	const dispatch = createEventDispatcher<{
		input: void;
		focus: void;
		blur: void;
		keydown: KeyboardEvent;
	}>();

	let textareaEl: ChipTextarea | null = null;
	let mentionQuery = '';
	// Length (in serialized characters) of the trailing `@…` fragment the picker
	// is matching — consumed when the user commits a selection.
	let mentionConsume = 0;

	$: mentionMatches = mentionMatchesFor(mentionItems, mentionQuery, mentionLimit);
	$: if (mentionActiveIndex >= mentionMatches.length) {
		mentionActiveIndex = Math.max(mentionMatches.length - 1, 0);
	}

	export function focus(): void {
		void tick().then(() => textareaEl?.focus());
	}

	/** Pass-through to the inner ChipTextarea — the structured list of chips
	 *  currently in the field (kind/slug/label), in order. Hosts use this to derive,
	 *  e.g., referenced task ids from `task` chips (`slug` = id) WITHOUT parsing the
	 *  serialized text (which over-matches typed `task:foo` and loses titles). */
	export function chipTokens(): ChipFieldToken[] {
		return textareaEl?.chipTokens() ?? [];
	}

	function closeMentionPicker(): void {
		mentionOpen = false;
		mentionQuery = '';
		mentionActiveIndex = 0;
	}

	function refreshMentionPicker(): void {
		if (!mentionsEnabled || !textareaEl || mentionItems.length === 0) {
			closeMentionPicker();
			return;
		}
		// `textBeforeCaret` returns the serialized form (chips emit as their typed
		// `agent:foo` tokens), so the `@` trigger only ever fires on user-typed
		// `@…`, never on a previously inserted chip.
		const trigger = detectMentionTrigger(textareaEl.textBeforeCaret(), {
			allowSpaces: allowSpacesInQuery
		});
		if (!trigger) {
			closeMentionPicker();
			return;
		}
		mentionConsume = trigger.consume;
		mentionQuery = trigger.query;
		mentionOpen = true;
		mentionActiveIndex = 0;
	}

	export function applyMention(item: ComposerMentionItem): void {
		if (!textareaEl) return;
		// `insertText` is the typed-prefix form (`agent:foo` / `skill:foo` /
		// `skill:foo via agent:bar` / `personality:foo` / `task:<id>` /
		// `feature:<slug>`) — split on the first `:` to get the chip kind + slug.
		// ChipTextarea does the DOM surgery + caret placement.
		const colonIdx = item.insertText.indexOf(':');
		if (colonIdx <= 0) {
			closeMentionPicker();
			return;
		}
		const kindStr = item.insertText.slice(0, colonIdx);
		const slug = item.insertText.slice(colonIdx + 1);
		const kind: ChipKind =
			kindStr === 'agent'
				? 'agent'
				: kindStr === 'personality'
					? 'personality'
					: kindStr === 'task'
						? 'task'
						: kindStr === 'feature'
							? 'feature'
							: 'skill';
		// Task chips show the title (chipLabel) while serializing `task:<id>`; feature
		// chips show `@tutor_quick` while serializing the command `@tutor #quick`.
		textareaEl.replaceBeforeCaretWithChip(mentionConsume, kind, slug, true, item.chipLabel);
		closeMentionPicker();
		void tick().then(() => {
			textareaEl?.focus();
			dispatch('input');
		});
	}

	function handleInput(): void {
		// Caret may have moved → refresh the picker, then notify the host.
		refreshMentionPicker();
		dispatch('input');
	}

	function handleInnerKeydown(event: KeyboardEvent): void {
		// Mention-nav keys are consumed ONLY while the picker is open with matches.
		if (mentionOpen && mentionMatches.length > 0) {
			if (event.key === 'ArrowDown') {
				event.preventDefault();
				mentionActiveIndex = (mentionActiveIndex + 1) % mentionMatches.length;
				return;
			}
			if (event.key === 'ArrowUp') {
				event.preventDefault();
				mentionActiveIndex =
					(mentionActiveIndex - 1 + mentionMatches.length) % mentionMatches.length;
				return;
			}
			if (event.key === 'Tab' || event.key === 'Enter') {
				event.preventDefault();
				applyMention(mentionMatches[mentionActiveIndex]);
				return;
			}
		}
		if (mentionOpen && event.key === 'Escape') {
			event.preventDefault();
			closeMentionPicker();
			return;
		}
		// Everything else (bare Enter with the picker closed, Shift+Enter, …) is
		// the HOST's to handle — forward the same native event so its submit chord
		// is unchanged. preventDefault by the host still works (synchronous chain).
		dispatch('keydown', event);
	}
</script>

<ChipTextarea
	bind:this={textareaEl}
	bind:value
	{placeholder}
	{disabled}
	{minHeight}
	{maxHeight}
	{inputmode}
	{enterkeyhint}
	{autocapitalize}
	class={className}
	on:input={handleInput}
	on:focus={() => dispatch('focus')}
	on:blur={() => dispatch('blur')}
	on:keydown={(e) => handleInnerKeydown(e.detail)}
/>
