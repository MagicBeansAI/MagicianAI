/**
 * Window/tab/profile management and navigation actions.
 */

import { debuggerSessions } from './background_state.js';
import { beginTabCreation, rememberCreatedTabs, automationTabOwners } from './automation_tabs.js';
import { openInOwnerWindow, groupOwnWindow, sessionSurface, groupSessionTabs } from './automation_surface.js';

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
    const generation = beginTabCreation(params.sessionId);
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

    // `surface` (automation_surface.js) comes from a proxy that knows tab
    // groups; without it this is the old focused window.
    const surface = params.surface;
    if (surface && surface !== 'own_window') {
        const inWindow = await openInOwnerWindow(params, tabIds => rememberCreatedTabs(params.sessionId, tabIds, generation));
        if (inWindow) return { ...inWindow, profile: await getCurrentProfile() };
    }

    const window = await chrome.windows.create({
        url: params.url || 'about:blank',
        focused: surface ? params.focused === true : params.focused !== false,
        type: params.type || 'normal',
        width: params.width,
        height: params.height
    });

    try {
        await rememberCreatedTabs(params.sessionId, window.tabs.map(tab => tab.id), generation);
    } catch (error) {
        await Promise.allSettled(window.tabs.map(tab => chrome.tabs.remove(tab.id)));
        throw error;
    }

    const grouping = surface ? await groupOwnWindow(params, window) : { surface: 'own_window' };
    return {
        windowId: window.id,
        tabs: window.tabs.map(t => ({ tabId: t.id, url: t.url })),
        ...grouping,
        profile: await getCurrentProfile() // Include actual profile in response
    };
}

async function closeWindow(params) {
    await chrome.windows.remove(params.windowId);
    return { closed: true };
}

// ===== Tab Management =====

async function createTab(params) {
    if (!params.sessionId) throw new Error('create_tab requires sessionId');
    const generation = beginTabCreation(params.sessionId);
    // A grouped task's new tab goes where its group is now, and never takes
    // the owner's view when that is their own window.
    const surface = await sessionSurface(params.sessionId).catch(() => null);
    const tab = await chrome.tabs.create({
        url: params.url || 'about:blank',
        windowId: surface?.windowId ?? params.windowId,
        active: surface?.surface === 'in_window' ? false : params.active !== false
    });
    try {
        await rememberCreatedTabs(params.sessionId, [tab.id], generation);
    } catch (error) {
        await chrome.tabs.remove(tab.id).catch(() => {});
        throw error;
    }
    if (surface) {
        await groupSessionTabs(params.sessionId, [tab.id], tab.windowId)
            .catch(error => console.warn(`[Magicutor] Could not group tab ${tab.id}: ${error.message}`));
    }
    return { tabId: tab.id, windowId: tab.windowId, url: tab.url };
}

async function closeTab(params) {
    await chrome.tabs.remove(params.tabId);
    return { closed: true };
}

async function listTabs(params = {}) {
    const query = params.windowId ? { windowId: params.windowId } : {};
    const tabs = await chrome.tabs.query(query);
    const owners = await automationTabOwners();

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
                automationSessionId: owners.get(t.id) || null,
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
    createTab,
    listTabs
};
