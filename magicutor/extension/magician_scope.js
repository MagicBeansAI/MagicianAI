/** Bearer authorization for extension → Magician API calls. */

import { MAGICIAN_API_BASE } from './config.js';

export const MAGICIAN_BEARER_STORAGE_KEY = 'magicianScopedBearer';
const UI_BEARER_STORAGE_KEY = 'magician:auth:scoped-bearer';
const UI_BEARER_TAB_PATTERNS = ['http://localhost:5173/*', 'http://127.0.0.1:5173/*'];

let borrowedBearer = '';

export function clearMagicianBearerCache() {
    borrowedBearer = '';
}

function cleanToken(value) {
    const token = typeof value === 'string' ? value.trim() : '';
    if (!token) return '';
    return token.replace(/^Bearer\s+/i, '').trim();
}

async function readStoredBearer() {
    const storage = globalThis.chrome?.storage?.local;
    if (!storage || typeof storage.get !== 'function') return '';
    try {
        const value = await storage.get(MAGICIAN_BEARER_STORAGE_KEY);
        return cleanToken(value?.[MAGICIAN_BEARER_STORAGE_KEY]);
    } catch (_) {
        return '';
    }
}

async function borrowUiSessionBearer() {
    const scripting = globalThis.chrome?.scripting;
    const tabs = globalThis.chrome?.tabs;
    if (!scripting || typeof scripting.executeScript !== 'function') return '';
    if (!tabs || typeof tabs.query !== 'function') return '';
    for (const pattern of UI_BEARER_TAB_PATTERNS) {
        let found = [];
        try {
            found = await tabs.query({ url: pattern });
        } catch (_) {
            continue;
        }
        for (const tab of found) {
            if (!tab || !tab.id) continue;
            try {
                const results = await scripting.executeScript({
                    target: { tabId: tab.id },
                    world: 'MAIN',
                    func: (key) => {
                        try {
                            const value = window.sessionStorage.getItem(key);
                            return typeof value === 'string' ? value : '';
                        } catch (_) {
                            return '';
                        }
                    },
                    args: [UI_BEARER_STORAGE_KEY]
                });
                const token = cleanToken(results && results[0] && results[0].result);
                if (token) return token;
            } catch (_) {
                // That Magician tab closed, or Chrome blocked the read.
            }
        }
    }
    return '';
}

export async function magicianScopedHeaders(headers, options = {}) {
    const next = new Headers(headers);
    next.delete('X-Principal');
    next.delete('X-Workspace');
    const token = await resolveMagicianBearer(options);
    if (!token && !next.has('Authorization')) {
        throw new Error('No Magician bearer is installed. Add one in the extension popup or side panel.');
    }
    if (token && !next.has('Authorization')) {
        next.set('Authorization', `Bearer ${token}`);
    }

    return next;
}

export async function resolveMagicianBearer(options = {}) {
    if (!options.skipStored) {
        const stored = await readStoredBearer();
        if (stored) return stored;
        if (borrowedBearer) return borrowedBearer;
    }
    const borrowed = await borrowUiSessionBearer();
    if (borrowed) borrowedBearer = borrowed;
    return options.skipStored ? borrowed : borrowedBearer;
}

export async function magicianFetch(input, init = {}) {
    const raw = input instanceof Request ? input.url : String(input);
    let target;
    let base;
    try {
        target = new URL(raw);
        base = new URL(MAGICIAN_API_BASE);
    } catch (_) {
        throw new Error('Refusing to attach the Magician bearer to an invalid URL.');
    }
    const basePath = base.pathname.replace(/\/$/, '');
    if (
        target.origin !== base.origin
        || target.username
        || target.password
        || (target.pathname !== basePath && !target.pathname.startsWith(`${basePath}/`))
    ) {
        throw new Error('Refusing to attach the Magician bearer to an untrusted URL.');
    }

    return magicianFetchOnce(input, init, false);
}

async function magicianFetchOnce(input, init, skipStored) {
    const headers = await magicianScopedHeaders(await mergedMagicianHeaders(input, init), { skipStored });
    const response = input instanceof Request
        ? await fetch(new Request(input, { ...init, headers }))
        : await fetch(input, { ...init, headers });
    if (skipStored || response.status !== 401 || input instanceof Request) return response;
    borrowedBearer = '';
    try {
        return await magicianFetchOnce(input, init, true);
    } catch (_) {
        return response;
    }
}

async function mergedMagicianHeaders(input, init) {
    const mergedHeaders = new Headers(
        input instanceof Request ? input.headers : undefined
    );
    if (init?.headers) {
        const initHeaders = new Headers(init.headers);
        initHeaders.forEach((value, key) => {
            mergedHeaders.set(key, value);
        });
    }
    return mergedHeaders;
}
