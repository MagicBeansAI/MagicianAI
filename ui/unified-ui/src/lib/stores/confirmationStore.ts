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
}

interface InternalState {
	request: ConfirmationRequest | null;
	resolve: ((value: boolean) => void) | null;
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

/** Resolve the active request from inside the host component. */
export function resolveConfirmation(value: boolean): void {
	state.update((current) => {
		if (current.resolve) current.resolve(value);
		return { request: null, resolve: null };
	});
}
