/** Bearer authorization for extension → Magician API calls. */

import { MAGICIAN_API_BASE } from './config.js';

export const MAGICIAN_BEARER_STORAGE_KEY = 'magicianScopedBearer';

export async function magicianScopedHeaders(headers) {
    const next = new Headers(headers);
    next.delete('X-Principal');
    next.delete('X-Workspace');
    const token = await resolveMagicianBearer();
    if (!token && !next.has('Authorization')) {
        throw new Error('No Magician bearer is installed. Add one in the extension popup or side panel.');
    }
    if (token && !next.has('Authorization')) {
        next.set('Authorization', `Bearer ${token}`);
    }

    return next;
}

export async function resolveMagicianBearer() {
    const storage = globalThis.chrome?.storage?.local;
    if (!storage) return '';
    const value = await storage.get(MAGICIAN_BEARER_STORAGE_KEY);
    return typeof value?.[MAGICIAN_BEARER_STORAGE_KEY] === 'string'
        ? value[MAGICIAN_BEARER_STORAGE_KEY].trim()
        : '';
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

    const mergedHeaders = new Headers(
        input instanceof Request ? input.headers : undefined
    );
    if (init?.headers) {
        const initHeaders = new Headers(init.headers);
        initHeaders.forEach((value, key) => {
            mergedHeaders.set(key, value);
        });
    }

    const headers = await magicianScopedHeaders(mergedHeaders);
    if (input instanceof Request) {
        return fetch(new Request(input, { ...init, headers }));
    }
    return fetch(input, { ...init, headers });
}
