/**
 * Window/tab/profile management and navigation actions.
 */

import { debuggerSessions } from './background_state.js';

/**
 * Get current Chrome profile name.
 * Note: Chrome extensions cannot directly read profile names from the filesystem.
 * This returns a stored operator-provided label when available.
 */
async function getCurrentProfile() {
    try {
        const stored = await chrome.storage.local.get('magicutor_profile');
        return stored.magicutor_profile || null;
    } catch (error) {
        console.warn('[Magicutor] Could not determine current profile:', error);
        return null;
    }
}

// ===== Window Management =====

async function createWindow(params) {
    // Profile verification: Chrome extensions cannot switch profiles, but we can
    // detect mismatches and provide helpful error messages
    if (params.profile && params.connect_to_existing) {
        // Try to verify we're running in the expected profile
        // Note: Chrome extensions can't directly read profile names, but we can
        // store the expected profile and warn if there's a mismatch
        const currentProfile = await getCurrentProfile();
        if (currentProfile && currentProfile !== params.profile) {
            console.warn(`[Magicutor] Profile mismatch: requested '${params.profile}' but running in '${currentProfile}'`);
            throw new Error(
                `Profile mismatch: Extension is running in profile '${currentProfile}' but session requested '${params.profile}'. ` +
                `Please launch Chrome with the correct profile or update your session configuration.`
            );
        }
    }

    const window = await chrome.windows.create({
        url: params.url || 'about:blank',
        focused: params.focused !== false,
        type: params.type || 'normal',
        width: params.width,
        height: params.height
    });

    return {
        windowId: window.id,
        tabs: window.tabs.map(t => ({ tabId: t.id, url: t.url })),
        profile: await getCurrentProfile() // Include actual profile in response
    };
}

async function closeWindow(params) {
    await chrome.windows.remove(params.windowId);
    return { closed: true };
}

// ===== Tab Management =====

async function closeTab(params) {
    await chrome.tabs.remove(params.tabId);
    return { closed: true };
}

async function listTabs(params = {}) {
    const query = params.windowId ? { windowId: params.windowId } : {};
    const tabs = await chrome.tabs.query(query);

    // Keep the default list_tabs path independent of chrome.debugger.getTargets().
    // That API has been observed to hang inside Chrome's debugger subsystem; since
    // Target.getTargets depends on list_tabs during CDP connect, waiting on it can
    // wedge the entire attach flow. Foreign debugger ownership is detected later
    // by bounded chrome.debugger.attach, not during discovery.

    return {
        tabs: tabs.map(t => {
            const ownedByUs = debuggerSessions.has(t.id);
            return {
                tabId: t.id,
                windowId: t.windowId,
                url: t.url,
                title: t.title,
                active: t.active,
                debuggerAttached: ownedByUs,
                ownedByUs,
                attachable: true
            };
        })
    };
}

export {
    createWindow,
    closeWindow,
    closeTab,
    listTabs
};
