// Per-install browser identity for the Magicutor bridge.
//
// Several browsers (Chrome profiles, Comet, Dia, Edge, ...) can share the
// bridge. Each extension install keeps, in chrome.storage.local, a random id
// that names it, a random secret key that proves a reconnect really is this
// install (sent only in the bridge handshake, never shown or echoed), and a
// display label the owner can rename in the panel's Settings.

const BROWSER_IDENTITY_STORAGE_KEY = 'magican_browser_identity_v1';
const MAX_LABEL_CHARS = 48;

// Most specific first: forks often also list "Google Chrome" or "Chromium".
const KNOWN_BRANDS = [
    ['comet', 'Comet'],
    ['dia', 'Dia'],
    ['arc', 'Arc'],
    ['microsoft edge', 'Edge'],
    ['brave', 'Brave'],
    ['opera', 'Opera'],
    ['vivaldi', 'Vivaldi'],
    ['google chrome', 'Chrome'],
    ['chromium', 'Chromium'],
];

const USER_AGENT_BRANDS = [
    [/\bComet\//i, 'Comet'],
    [/\bDia\//i, 'Dia'],
    [/\bEdg\//, 'Edge'],
    [/\bOPR\//, 'Opera'],
    [/\bVivaldi\//, 'Vivaldi'],
    [/\bBrave\//i, 'Brave'],
    [/\bChrome\//, 'Chrome'],
];

/** Best-effort browser brand ("Chrome", "Comet", "Dia", ...). */
function detectBrowserBrand(nav = globalThis.navigator) {
    const brands = Array.isArray(nav?.userAgentData?.brands)
        ? nav.userAgentData.brands.map((entry) => String(entry?.brand || '').toLowerCase())
        : [];
    for (const [needle, label] of KNOWN_BRANDS) {
        if (brands.some((brand) => brand === needle)) return label;
    }
    const userAgent = String(nav?.userAgent || '');
    for (const [pattern, label] of USER_AGENT_BRANDS) {
        if (pattern.test(userAgent)) return label;
    }
    return 'Browser';
}

function normalizeBrowserLabel(value) {
    const cleaned = String(value ?? '')
        .replace(/[\u0000-\u001f\u007f]/g, '')
        .trim()
        .slice(0, MAX_LABEL_CHARS)
        .trim();
    return cleaned;
}

function generateBrowserId(randomUUID = globalThis.crypto?.randomUUID?.bind(globalThis.crypto)) {
    if (typeof randomUUID === 'function') return randomUUID();
    const bytes = Array.from({ length: 16 }, () => Math.floor(Math.random() * 256));
    return bytes.map((byte) => byte.toString(16).padStart(2, '0')).join('');
}

function isValidBrowserId(id) {
    return typeof id === 'string' && /^[A-Za-z0-9_-]{1,64}$/.test(id) && !id.startsWith('legacy-browser');
}

function isValidBrowserKey(key) {
    return typeof key === 'string' && /^[A-Za-z0-9_-]{16,128}$/.test(key);
}

function defaultStorage() {
    return globalThis.chrome?.storage?.local || null;
}

// One in-flight load is shared: concurrent first calls (bridge connect and a
// panel ping at worker start) must not each mint a different id.
let identityPromise = null;

async function loadBrowserIdentity({ storage, nav, randomUUID }) {
    let stored = null;
    try {
        stored = storage ? (await storage.get(BROWSER_IDENTITY_STORAGE_KEY))?.[BROWSER_IDENTITY_STORAGE_KEY] : null;
    } catch (_) {
        stored = null;
    }
    const brand = detectBrowserBrand(nav);
    const id = isValidBrowserId(stored?.id) ? stored.id : generateBrowserId(randomUUID);
    const key = isValidBrowserKey(stored?.key) ? stored.key : generateBrowserId(randomUUID);
    const customLabel = normalizeBrowserLabel(stored?.customLabel);
    if (!stored || stored.id !== id || stored.key !== key) {
        try {
            await storage?.set({ [BROWSER_IDENTITY_STORAGE_KEY]: { id, key, customLabel } });
        } catch (_) {
            // A fresh id each worker start still connects; it only loses
            // same-browser reconnect replacement.
        }
    }
    return { id, key, brand, customLabel, label: customLabel || brand };
}

/**
 * The stable identity of this extension install: `{ id, key, label, brand,
 * customLabel }`. The id and key are generated once and stored; the label is
 * the owner's name for this browser, else the detected brand. The key is a
 * secret for the bridge handshake only — see `publicBrowserIdentity`.
 */
function getBrowserIdentity({ storage = defaultStorage(), nav, randomUUID } = {}) {
    if (!identityPromise) {
        identityPromise = loadBrowserIdentity({ storage, nav, randomUUID }).catch((error) => {
            identityPromise = null;
            throw error;
        });
    }
    return identityPromise;
}

/** Rename this browser. An empty label returns to the detected brand. */
async function setBrowserLabel(label, { storage = defaultStorage(), nav, randomUUID } = {}) {
    const current = await getBrowserIdentity({ storage, nav, randomUUID });
    const customLabel = normalizeBrowserLabel(label);
    try {
        await storage?.set({ [BROWSER_IDENTITY_STORAGE_KEY]: { id: current.id, key: current.key, customLabel } });
    } catch (_) {
        // The live connection is still renamed; the next start re-detects.
    }
    const renamed = { ...current, customLabel, label: customLabel || current.brand };
    identityPromise = Promise.resolve(renamed);
    return renamed;
}

/** The identity without its secret key, for panels and messages. */
function publicBrowserIdentity(identity) {
    if (!identity) return null;
    const { key: _secret, ...visible } = identity;
    return visible;
}

/** Handshake query suffix that states (and proves) this browser's identity. */
function bridgeIdentityQuery(identity) {
    if (!identity?.id) return '';
    const key = isValidBrowserKey(identity.key) ? `&browser_key=${encodeURIComponent(identity.key)}` : '';
    return `&browser_id=${encodeURIComponent(identity.id)}`
        + key
        + `&browser_label=${encodeURIComponent(identity.label || '')}`;
}

function resetBrowserIdentityCacheForTests() {
    identityPromise = null;
}

export {
    BROWSER_IDENTITY_STORAGE_KEY,
    bridgeIdentityQuery,
    detectBrowserBrand,
    getBrowserIdentity,
    normalizeBrowserLabel,
    publicBrowserIdentity,
    resetBrowserIdentityCacheForTests,
    setBrowserLabel
};
