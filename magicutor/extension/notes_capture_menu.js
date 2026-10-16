/**
 * "Save selection to Notes" — file the highlighted text into Magician Notes.
 *
 * A sibling of the Contextual Assist menu rather than a mode of it: that one
 * opens an overlay to work with the selection, this one files it and gets out
 * of the way. Sharing an entry would make the common case — keep this, do
 * nothing else — a two-step interaction.
 *
 * This talks to Magician directly rather than through the desktop host gateway.
 * The gateway is how the extension reaches native surfaces, but saving a note
 * needs only the backend, and routing through the gateway would make browser
 * capture fail whenever the desktop app happens to be closed.
 */

import { MAGICIAN_API_BASE } from './config.js';
import { magicianFetch } from './magician_scope.js';

const MENU_ID = 'magician_save_selection_to_notes';
const MENU_TITLE = 'Save selection to Magician Notes';

let listenerRegistered = false;

export function setupNotesCaptureContextMenu() {
    if (!chrome.contextMenus) {
        return;
    }

    chrome.contextMenus.remove(MENU_ID, () => {
        // Ignore "Cannot find menu item" on first registration.
        void chrome.runtime.lastError;
        chrome.contextMenus.create(
            {
                id: MENU_ID,
                title: MENU_TITLE,
                // Selection only. Offering this on an empty right-click would
                // promise a capture with nothing to capture.
                contexts: ['selection']
            },
            () => {
                if (chrome.runtime.lastError) {
                    console.warn(
                        '[NotesCapture] Failed to create context menu:',
                        chrome.runtime.lastError.message
                    );
                }
            }
        );
    });

    if (!listenerRegistered) {
        chrome.contextMenus.onClicked.addListener((info, tab) => {
            if (info.menuItemId !== MENU_ID) return;
            void captureSelectionToNotes(info, tab);
        });
        listenerRegistered = true;
    }
}

/**
 * One id per click, so a resend after a lost response files the selection once.
 * The backend dedupes on this; a genuine second capture is a second click and
 * therefore a new id.
 */
function newCaptureId() {
    if (typeof crypto?.randomUUID === 'function') {
        return crypto.randomUUID();
    }
    return `capture-${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

async function captureSelectionToNotes(info, tab) {
    const text = typeof info.selectionText === 'string' ? info.selectionText.trim() : '';
    if (!text) {
        // Chrome only shows the item for a selection, so this means the
        // selection vanished between the right-click and the dispatch.
        return;
    }

    const payload = {
        text,
        source_url: info.pageUrl || tab?.url || null,
        source_title: tab?.title || null,
        source_app: 'Google Chrome',
        capture_id: newCaptureId()
    };

    try {
        const response = await magicianFetch(`${MAGICIAN_API_BASE}/notes/capture-selection`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(payload)
        });
        if (!response.ok) {
            console.warn('[NotesCapture] Magician rejected the capture:', response.status);
            return;
        }
        const note = await response.json();
        // Report where it actually landed. A silent success is indistinguishable
        // from a silent failure, and `provider` may not be the configured
        // default when a fallback took over.
        console.info('[NotesCapture] Saved to', note?.provider, note?.path);
    } catch (error) {
        console.warn('[NotesCapture] Failed to reach Magician:', error.message);
    }
}
