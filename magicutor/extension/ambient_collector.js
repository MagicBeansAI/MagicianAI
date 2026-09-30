/**
 * Ambient signal collector (WEG Phase 2, P2.2) — background side.
 *
 * Receives metadata-first browsing signals from `ambient_browsing.js`, queues
 * them in `chrome.storage.local` (the background is the single writer, so the
 * queue is race-free), and batch-uploads to Magician's `/ambient/signals/batch`
 * on a periodic alarm. The server gates on the user's consent flag (Observe
 * page) and applies the ingress policy, so this side is dumb + best-effort.
 *
 * Design choices (per docs/plans/2026-06-13-weg-phase-2-ambient-capture.md):
 *  - HTTP batch upload, NOT the execution WS bridge (that lane is agent control).
 *  - Incognito tabs are never captured (dropped on receipt).
 *  - Bounded queue; oldest dropped on overflow; survives SW restarts.
 *
 * Collector-token auth (P2.1b): once the browser is paired (token stored via the
 * popup's pairing field, issued by the Observe page's "Pair browser" →
 * /ambient/enroll), every batch carries the `X-Collector-Token` header and the
 * server resolves scope FROM the token, ignoring the client-asserted
 * a scoped bearer. Unpaired local-development uploads use the server's open-mode default.
 */

import { MAGICIAN_API_BASE } from './config.js';
// Reuse the existing scope-aware fetch — it attaches the scoped bearer
// (default anonymous/default). When a collector token is paired it is added as
// X-Collector-Token and the server resolves scope from it (headers advisory).
import { magicianFetch } from './magician_scope.js';

const FLUSH_ALARM = 'weg_ambient_flush';
const QUEUE_KEY = 'weg_ambient_queue';
const TOKEN_KEY = 'weg_collector_token';
const MAX_QUEUE = 200;
const BATCH_SIZE = 40;
const FLUSH_PERIOD_MINUTES = 0.5; // ~30s

function batchUrl() {
    return `${MAGICIAN_API_BASE}/ambient/signals/batch`;
}

async function getCollectorToken() {
    try {
        const data = await chrome.storage.local.get(TOKEN_KEY);
        const token = data[TOKEN_KEY];
        return typeof token === 'string' && token.trim() ? token.trim() : null;
    } catch (_) {
        return null;
    }
}

async function readQueue() {
    try {
        const data = await chrome.storage.local.get(QUEUE_KEY);
        return Array.isArray(data[QUEUE_KEY]) ? data[QUEUE_KEY] : [];
    } catch (_) {
        return [];
    }
}

async function writeQueue(queue) {
    try {
        await chrome.storage.local.set({ [QUEUE_KEY]: queue });
    } catch (_) {
        /* storage full / unavailable — drop silently */
    }
}

async function enqueueAmbientSignal(signal) {
    const queue = await readQueue();
    queue.push(signal);
    // Drop oldest, low-value items first when over the cap.
    while (queue.length > MAX_QUEUE) queue.shift();
    await writeQueue(queue);
}

async function flushAmbientQueue() {
    const queue = await readQueue();
    if (queue.length === 0) return;

    const batch = queue.slice(0, BATCH_SIZE);
    const headers = { 'Content-Type': 'application/json' };
    const token = await getCollectorToken();
    if (token) headers['X-Collector-Token'] = token;
    try {
        const res = await magicianFetch(batchUrl(), {
            method: 'POST',
            headers,
            body: JSON.stringify({
                signals: batch,
                client_queue_depth: queue.length
            })
        });
        if (res.ok) {
            // Re-read in case the content script enqueued more during the POST,
            // then drop exactly the sent prefix.
            const current = await readQueue();
            await writeQueue(current.slice(batch.length));
        }
        // Non-ok (e.g. server down / disabled): keep the queue, retry next alarm.
    } catch (_) {
        // Network error / server asleep: keep the queue and retry next alarm.
    }
}

// ── MV3: register listeners + alarm synchronously at top level on import ─────

chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
    // Pairing: store (or clear) the collector token issued by the Observe page.
    if (message?.type === 'weg_pair_collector') {
        const token = typeof message.token === 'string' ? message.token.trim() : '';
        (async () => {
            try {
                if (token) {
                    await chrome.storage.local.set({ [TOKEN_KEY]: token });
                    sendResponse({ ok: true, paired: true });
                } else {
                    await chrome.storage.local.remove(TOKEN_KEY);
                    sendResponse({ ok: true, paired: false });
                }
            } catch (e) {
                sendResponse({ ok: false, error: String(e) });
            }
        })();
        return true; // async sendResponse
    }
    if (message?.type === 'weg_collector_status') {
        (async () => sendResponse({ paired: !!(await getCollectorToken()) }))();
        return true; // async sendResponse
    }
    if (message?.type !== 'weg_ambient_signal' || !message.signal) return;
    // Never capture private/incognito browsing.
    if (sender?.tab?.incognito) return;
    void enqueueAmbientSignal(message.signal);
    // No async response — don't return true.
});

chrome.alarms.onAlarm.addListener((alarm) => {
    if (alarm.name === FLUSH_ALARM) void flushAmbientQueue();
});

chrome.alarms.create(FLUSH_ALARM, { periodInMinutes: FLUSH_PERIOD_MINUTES });

console.log('[WEG] Ambient collector registered; runtime endpoint discovery is enabled');
