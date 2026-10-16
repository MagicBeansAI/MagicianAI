/**
 * Centralized configuration constants for the Magicutor browser extension.
 */

const DEFAULT_MAGICIAN_API_BASE = 'http://127.0.0.1:3002/api/magician/v2';
const DEFAULT_MAGICIAN_HEALTH_URL = 'http://127.0.0.1:3002/health';
const DEFAULT_MAGICUTOR_API_BASE = 'http://127.0.0.1:3003';
const DEFAULT_MAGICUTOR_BRIDGE_URL = 'ws://127.0.0.1:3003/bridge/native';
const RUNTIME_ENDPOINTS_STORAGE_KEY = 'magician_runtime_endpoints_v1';
const RUNTIME_ENDPOINTS_URL = 'http://127.0.0.1:3017/host/runtime/endpoints';

export let MAGICIAN_API_BASE = DEFAULT_MAGICIAN_API_BASE;
export const MAGICIAN_API_TIMEOUT_MS = 20000;
export let MAGICIAN_HEALTH_URL = DEFAULT_MAGICIAN_HEALTH_URL;

export let MAGICUTOR_API_BASE = DEFAULT_MAGICUTOR_API_BASE;
export let MAGICUTOR_BRIDGE_URL = DEFAULT_MAGICUTOR_BRIDGE_URL;
export const MAGICUTOR_API_TIMEOUT_MS = 20000;

// The gateway is the well-known loopback discovery/control endpoint. Service
// ports behind it are configurable and are resolved by refreshRuntimeEndpoints.
export const MAGICIAN_HOST_GATEWAY_URL = 'http://127.0.0.1:3017';

let discoveryInFlight = null;

function isLoopbackUrl(value, allowedProtocols) {
    try {
        const url = new URL(value);
        return allowedProtocols.includes(url.protocol)
            && (url.hostname === '127.0.0.1' || url.hostname === 'localhost');
    } catch (_) {
        return false;
    }
}

function normalizeRuntimeEndpoints(candidate) {
    if (!candidate || candidate.schemaVersion !== 1) return null;
    const normalized = {
        schemaVersion: 1,
        magicianApiBase: String(candidate.magicianApiBase || '').replace(/\/$/, ''),
        magicianHealthUrl: String(candidate.magicianHealthUrl || ''),
        magicutorApiBase: String(candidate.magicutorApiBase || '').replace(/\/$/, ''),
        magicutorBridgeUrl: String(candidate.magicutorBridgeUrl || '')
    };
    if (!isLoopbackUrl(normalized.magicianApiBase, ['http:', 'https:'])
        || !isLoopbackUrl(normalized.magicianHealthUrl, ['http:', 'https:'])
        || !isLoopbackUrl(normalized.magicutorApiBase, ['http:', 'https:'])
        || !isLoopbackUrl(normalized.magicutorBridgeUrl, ['ws:', 'wss:'])) {
        return null;
    }
    return normalized;
}

function applyRuntimeEndpoints(endpoints) {
    MAGICIAN_API_BASE = endpoints.magicianApiBase;
    MAGICIAN_HEALTH_URL = endpoints.magicianHealthUrl;
    MAGICUTOR_API_BASE = endpoints.magicutorApiBase;
    MAGICUTOR_BRIDGE_URL = endpoints.magicutorBridgeUrl;
}

function currentRuntimeEndpoints() {
    return {
        magicianApiBase: MAGICIAN_API_BASE,
        magicianHealthUrl: MAGICIAN_HEALTH_URL,
        magicutorApiBase: MAGICUTOR_API_BASE,
        magicutorBridgeUrl: MAGICUTOR_BRIDGE_URL
    };
}

function endpointsChanged(previous) {
    const current = currentRuntimeEndpoints();
    return Object.keys(current).some((key) => current[key] !== previous[key]);
}

async function readCachedRuntimeEndpoints() {
    if (!globalThis.chrome?.storage?.local) return null;
    try {
        const stored = await chrome.storage.local.get(RUNTIME_ENDPOINTS_STORAGE_KEY);
        return normalizeRuntimeEndpoints(stored?.[RUNTIME_ENDPOINTS_STORAGE_KEY]);
    } catch (_) {
        return null;
    }
}

async function cacheRuntimeEndpoints(endpoints) {
    if (!globalThis.chrome?.storage?.local) return;
    try {
        await chrome.storage.local.set({ [RUNTIME_ENDPOINTS_STORAGE_KEY]: endpoints });
    } catch (_) {
        // Discovery remains useful even when extension storage is unavailable.
    }
}

/**
 * Resolve configured service ports through the desktop host gateway.
 * Last-known-good values are applied first so an MV3 worker can reconnect even
 * while the desktop gateway is starting; compiled defaults are the final
 * backwards-compatible fallback.
 */
export async function refreshRuntimeEndpoints({
    fetchImpl = globalThis.fetch,
    discoveryUrl = RUNTIME_ENDPOINTS_URL
} = {}) {
    if (discoveryInFlight) return discoveryInFlight;

    discoveryInFlight = (async () => {
        const previous = currentRuntimeEndpoints();
        const cached = await readCachedRuntimeEndpoints();
        if (cached) applyRuntimeEndpoints(cached);
        if (typeof fetchImpl !== 'function') {
            return { source: cached ? 'cache' : 'default', changed: endpointsChanged(previous) };
        }

        const controller = new AbortController();
        const timeout = setTimeout(() => controller.abort(), 1500);
        try {
            const response = await fetchImpl(discoveryUrl, {
                method: 'GET',
                cache: 'no-store',
                signal: controller.signal
            });
            if (!response.ok) throw new Error(`endpoint discovery returned ${response.status}`);
            const discovered = normalizeRuntimeEndpoints(await response.json());
            if (!discovered) throw new Error('endpoint discovery returned an invalid contract');
            applyRuntimeEndpoints(discovered);
            await cacheRuntimeEndpoints(discovered);
            return {
                source: 'gateway',
                changed: endpointsChanged(previous),
                endpoints: discovered
            };
        } catch (error) {
            console.debug('[Magicutor] Runtime endpoint discovery unavailable; using fallback:', error.message);
            return {
                source: cached ? 'cache' : 'default',
                changed: endpointsChanged(previous),
                error: error.message
            };
        } finally {
            clearTimeout(timeout);
        }
    })();

    try {
        return await discoveryInFlight;
    } finally {
        discoveryInFlight = null;
    }
}
