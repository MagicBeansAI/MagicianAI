/**
 * Promise-based confirmation service.
 *
 * Replaces `window.confirm()` across the UI with a themed GAUI
 * `ConfirmDialog`. A single `<ConfirmationModalHost />` mounted at the
 * app root subscribes to this store, renders the active request, and
 * resolves the awaiting promise on confirm / cancel.
 *
 * Mirrors the shape of `attentionPromptStore` so both share idioms:
 *   const ok = await requestConfirmation({ title, message });
 *   if (!ok) return;
 *
 * Concurrent requests cancel the previous (the new one wins). Callers
 * must check the resolved boolean — `null`/`false` both mean "do not
 * proceed" but the boolean is more ergonomic at callsites.
 *
 * `requestTextInput()` is the same dialog with one text field (name a new
 * thread, …): it resolves the trimmed value, or `null` on cancel. Confirm
 * stays disabled while the field is empty.
 */
import { writable, type Readable } from 'svelte/store';

export interface ConfirmationRequest {
	id: number;
	title: string;
	message: string;
	confirmLabel?: string;
	cancelLabel?: string;
	/** When `true`, the confirm button renders in a destructive style. */
	destructive?: boolean;
	/** Renders one text field; set by `requestTextInput()`. */
	input?: ConfirmationTextInput;
}

export interface ConfirmationTextInput {
	/** Accessible label for the field. */
	label: string;
	placeholder?: string;
	initialValue?: string;
	maxLength?: number;
}

interface InternalState {
	request: ConfirmationRequest | null;
	resolve: ((value: boolean, text?: string) => void) | null;
}

const state = writable<InternalState>({ request: null, resolve: null });

export const confirmationStore: Readable<InternalState> = {
	subscribe: state.subscribe
};

let nextRequestId = 1;

/**
 * Ask the user to confirm an action via the themed GAUI dialog.
 * Resolves `true` on confirm, `false` on cancel / Esc / backdrop click.
 */
export function requestConfirmation(
	request: Omit<ConfirmationRequest, 'id'>
): Promise<boolean> {
	return new Promise((resolve) => {
		state.update((current) => {
			if (current.resolve) current.resolve(false);
			return {
				request: { ...request, id: nextRequestId++ },
				resolve
			};
		});
	});
}

/**
 * Ask the user for one line of text in the themed dialog. Resolves the
 * trimmed value on confirm, `null` on cancel / Esc / backdrop click or when
 * a newer request replaces this one.
 */
export function requestTextInput(
	request: Omit<ConfirmationRequest, 'id' | 'input' | 'destructive' | 'message'> & {
		message?: string;
		input: ConfirmationTextInput;
	}
): Promise<string | null> {
	return new Promise((resolve) => {
		state.update((current) => {
			if (current.resolve) current.resolve(false);
			return {
				request: { ...request, message: request.message ?? '', id: nextRequestId++ },
				resolve: (confirmed: boolean, text?: string) => {
					const value = (text ?? '').trim();
					resolve(confirmed && value.length > 0 ? value : null);
				}
			};
		});
	});
}

/**
 * Resolve the active request from inside the host component. `text` is the
 * field value for a `requestTextInput()` request.
 */
export function resolveConfirmation(value: boolean, text?: string): void {
	state.update((current) => {
		if (current.resolve) current.resolve(value, text);
		return { request: null, resolve: null };
	});
}
