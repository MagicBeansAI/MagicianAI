import { browser } from '$app/environment';

/** Desktop host marks the bounded Attention window with this query flag. */
export const NATIVE_ATTENTION_QUERY = 'native_attention';

export function isNativeAttentionLocation(href?: string): boolean {
	if (!browser && href === undefined) return false;
	const url = new URL(href ?? window.location.href, 'http://localhost');
	return url.searchParams.get(NATIVE_ATTENTION_QUERY) === '1';
}

/**
 * Hide the native Attention window.
 *
 * The host's `close_attention_window` closes the notch side panel the way
 * tapping the ear does (that panel is a non-closable panel, so a page's
 * `close()` never reaches it) and sends the standalone window the same close
 * its red button does. An older host without the command gets `close()`: it
 * emits that CloseRequested, and the host prevents destruction, hides the
 * window and demotes the Dock icon. A thrown invoke means this page is not
 * running inside that window.
 */
export async function hideNativeAttentionWindow(): Promise<boolean> {
	if (!isNativeAttentionLocation()) return false;
	try {
		const { invoke } = await import('@tauri-apps/api/core');
		await invoke('plugin:magician-attention|close_attention_window');
		return true;
	} catch {
		// An older host: fall back to the window close below.
	}
	try {
		const { getCurrentWindow } = await import('@tauri-apps/api/window');
		await getCurrentWindow().close();
		return true;
	} catch (error) {
		const message = error instanceof Error ? error.message : String(error);
		// Older hosts reject the prevented close after they have already hidden.
		return /prevent/i.test(message);
	}
}
