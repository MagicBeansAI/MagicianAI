import { MAGICIAN_HOST_GATEWAY_URL } from './config.js';

const MENU_ID = 'magician_contextual_assist';
const MENU_TITLE = 'Magician';
const PAGE_MENU_ID = 'magician_contextual_assist_page';
const PAGE_MENU_TITLE = 'Ask Magician about this page';
const HOST_OPEN_URL = `${MAGICIAN_HOST_GATEWAY_URL}/host/contextual-assist/open`;

let listenerRegistered = false;

export function setupContextualAssistContextMenu() {
    if (!chrome.contextMenus) {
        return;
    }

    chrome.contextMenus.remove(MENU_ID, () => {
        // Ignore "Cannot find menu item" when this is the first registration.
        void chrome.runtime.lastError;
        chrome.contextMenus.create(
            {
                id: MENU_ID,
                title: MENU_TITLE,
                contexts: ['selection', 'editable']
            },
            () => {
                if (chrome.runtime.lastError) {
                    console.warn(
                        '[ContextualAssist] Failed to create context menu:',
                        chrome.runtime.lastError.message
                    );
                }
            }
        );
    });

    // Page-level entry: shows on ANY right-click, selection or not, so the
    // assist is reachable from the page itself rather than only from a text
    // target. Sends the page state (url/title/tab) with no contextText —
    // grounding comes from the tab capture on the desktop side.
    chrome.contextMenus.remove(PAGE_MENU_ID, () => {
        void chrome.runtime.lastError;
        chrome.contextMenus.create(
            {
                id: PAGE_MENU_ID,
                title: PAGE_MENU_TITLE,
                contexts: ['page']
            },
            () => {
                if (chrome.runtime.lastError) {
                    console.warn(
                        '[ContextualAssist] Failed to create page context menu:',
                        chrome.runtime.lastError.message
                    );
                }
            }
        );
    });

    if (!listenerRegistered) {
        chrome.contextMenus.onClicked.addListener((info, tab) => {
            if (info.menuItemId === PAGE_MENU_ID) {
                void openPageContextualAssist(info, tab);
                return;
            }
            if (info.menuItemId !== MENU_ID) return;
            void openContextualAssistFromMenu(info, tab);
        });
        listenerRegistered = true;
    }
}

function stateFromContext(info) {
    const hasSelection = typeof info.selectionText === 'string' && info.selectionText.trim().length > 0;
    if (hasSelection && info.editable) return 'selection-field';
    if (hasSelection) return 'selection';
    if (info.editable) return 'empty-context';
    return null;
}

function stateFromContextText(info, pageContext, contextText) {
    const hasSelection = typeof info.selectionText === 'string'
        && info.selectionText.trim().length > 0
        || pageContext?.hasSelection === true;
    if (hasSelection && info.editable) return 'selection-field';
    if (hasSelection) return 'selection';
    if (info.editable && contextText) return 'draft';
    if (info.editable) return 'empty-context';
    return null;
}

async function browserWindowCenter(tab) {
    const windowId = tab?.windowId;
    if (!windowId) return {};

    try {
        const browserWindow = await chrome.windows.get(windowId);
        const left = Number(browserWindow.left);
        const top = Number(browserWindow.top);
        const width = Number(browserWindow.width);
        const height = Number(browserWindow.height);
        if ([left, top, width, height].every(Number.isFinite) && width > 0 && height > 0) {
            return {
                anchorX: left + width / 2,
                anchorY: top + height / 2
            };
        }
    } catch (_) {
        // The host gateway can fall back to monitor center.
    }

    return {};
}

async function openContextualAssistFromMenu(info, tab) {    const anchor = await browserWindowCenter(tab);
    const pageContext = await contextualAssistPageContext(info, tab);
    if (pageContext?.secure === true) {
        return;
    }
    const menuSelectionText =
        typeof info.selectionText === 'string' ? info.selectionText.trim() : '';
    const contextText = menuSelectionText || pageContext?.contextText || '';
    const state = stateFromContextText(info, pageContext, contextText) || stateFromContext(info);
    if (!state) return;

    const payload = {
        source: 'chrome_context_menu',
        state,
        app: 'Google Chrome',
        windowTitle: tab?.title || null,
        url: info.pageUrl || tab?.url || null,
        frameUrl: info.frameUrl || null,
        editable: info.editable === true,
        hasSelection: Boolean(menuSelectionText) || pageContext?.hasSelection === true,
        selectedTextLength: menuSelectionText.length || pageContext?.selectedTextLength || 0,
        tabId: tab?.id || null,
        windowId: tab?.windowId || null,
        ...anchor
    };
    if (contextText.trim()) {
        payload.contextText = contextText.trim();
    }

    try {
        const response = await fetch(HOST_OPEN_URL, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(payload)
        });
        if (!response.ok) {
            console.warn('[ContextualAssist] Host gateway rejected context menu open:', response.status);
        }
    } catch (error) {
        console.warn('[ContextualAssist] Failed to open native context menu:', error.message);
    }
}

async function openPageContextualAssist(info, tab) {
    const url = info.pageUrl || tab?.url || null;
    if (!url || !/^https?:/i.test(url)) {
        return;
    }
    const anchor = await browserWindowCenter(tab);

    const payload = {
        source: 'chrome_context_menu',
        state: 'page-context',
        app: 'Google Chrome',
        windowTitle: tab?.title || null,
        url,
        tabId: tab?.id || null,
        windowId: tab?.windowId || null,
        ...anchor
    };

    try {
        const response = await fetch(HOST_OPEN_URL, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(payload)
        });
        if (!response.ok) {
            console.warn('[ContextualAssist] Host gateway rejected page menu open:', response.status);
        }
    } catch (error) {
        console.warn('[ContextualAssist] Failed to open native page menu:', error.message);
    }
}

async function contextualAssistPageContext(info, tab) {
    if (!tab?.id) return null;

    try {
        const options = typeof info.frameId === 'number' ? { frameId: info.frameId } : undefined;
        const message = { type: 'contextual_assist_context' };
        const response = options
            ? await chrome.tabs.sendMessage(tab.id, message, options)
            : await chrome.tabs.sendMessage(tab.id, message);
        return response?.ok ? response : null;
    } catch (_) {
        return null;
    }
}
