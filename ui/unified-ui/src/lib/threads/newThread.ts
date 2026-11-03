/**
 * "New thread": ask for the name first, then create and open the thread.
 *
 * Every entry point (history rail, history drawer, command palette, the
 * `/t/new` page) goes through `startNewThread()`. The name prompt is the
 * shared themed dialog (`requestTextInput`): Enter submits, Esc cancels,
 * Create stays disabled while the name is empty. Cancel creates nothing.
 * The server derives the id and slug from the name
 * (`POST /api/magician/v2/ui-threads` without an id), and the UI opens
 * `/t/<slug>/chat`.
 */
import { goto } from '$app/navigation';
import { get } from 'svelte/store';

import { showError } from '$lib/shared/stores/notifications';
import { requestTextInput } from '$lib/stores/confirmationStore';
import { threadStore } from '$lib/stores/threadStore';
import { threadHrefFor } from './threadKeys';
import type { UiThreadRecord } from './types';

export const THREAD_NAME_MAX_LENGTH = 120;

export function promptForThreadName(): Promise<string | null> {
	return requestTextInput({
		title: 'New thread',
		confirmLabel: 'Create',
		input: {
			label: 'Thread name',
			placeholder: 'Name this thread',
			maxLength: THREAD_NAME_MAX_LENGTH
		}
	});
}

/** Create a thread named `name` and open its chat. `null` when creation fails. */
export async function createThreadAndOpen(
	name: string,
	options: { replaceState?: boolean } = {}
): Promise<UiThreadRecord | null> {
	const trimmed = name.trim();
	if (!trimmed) return null;
	const created = await threadStore.createThread(trimmed);
	if (!created) {
		showError(get(threadStore).error || 'Could not create the thread. Try again.');
		return null;
	}
	await goto(threadHrefFor([], created, '/chat'), { replaceState: options.replaceState ?? false });
	return created;
}

/** Ask for a name, then create and open the thread. Cancel creates nothing. */
export async function startNewThread(
	options: { replaceState?: boolean } = {}
): Promise<UiThreadRecord | null> {
	const name = await promptForThreadName();
	if (!name) return null;
	return createThreadAndOpen(name, options);
}
