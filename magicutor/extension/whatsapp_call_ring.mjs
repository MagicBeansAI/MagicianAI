/**
 * Tell Magician that a WhatsApp Web ring was seen, approved, or ignored.
 * The content script asks in the page. This module only reports the decision.
 * A 404 is success for the page: the running server may not have the route yet.
 * Approve does not call POST /meetings/join. That route launches another browser.
 */

const RING_DECISIONS = new Set([
    'incoming',
    'ongoing',
    'approved',
    'join',
    'listen',
    'ignored',
    'cleared',
    'cancel',
    'ended',
    'profile'
]);
export const WHATSAPP_RING_PATH = '/meetings/whatsapp-ring';
export const WHATSAPP_DIRECT_PROFILE = 'voice_realtime_default';
export const WHATSAPP_MOUTH_PROFILE = 'voice_realtime_openai_backend';

export function whatsAppProfileId(value) {
    const profile = typeof value === 'string' ? value.trim() : '';
    if (!/^voice_realtime_[A-Za-z0-9_]+$/.test(profile)) return '';
    if (profile.length < 16 || profile.length > 80) return '';
    return profile;
}

// The composer default is browser WebRTC. It cannot play into BlackHole.
export function whatsAppTalkProfile(value) {
    const profile = whatsAppProfileId(value);
    if (!profile || profile === WHATSAPP_DIRECT_PROFILE) return WHATSAPP_MOUTH_PROFILE;
    return profile;
}

// Other http(s) tabs can show the call banner. WhatsApp draws its own.
export function isWhatsAppPromptTab(url) {
    if (typeof url !== 'string' || !/^https?:\/\//i.test(url)) return false;
    try {
        return new URL(url).hostname !== 'web.whatsapp.com';
    } catch (_) {
        return false;
    }
}

export function isWhatsAppRingSender(sender) {
    const origin = sender && typeof sender.origin === 'string' ? sender.origin : '';
    const tabUrl = sender && sender.tab && typeof sender.tab.url === 'string' ? sender.tab.url : '';
    return origin === 'https://web.whatsapp.com'
        || tabUrl.startsWith('https://web.whatsapp.com/');
}

export function whatsAppRingPayload(message) {
    const decision = message && message.decision;
    if (!RING_DECISIONS.has(decision)) return null;
    const rawUrl = message && typeof message.url === 'string' ? message.url : '';
    const url = rawUrl.startsWith('https://web.whatsapp.com/')
        || rawUrl === 'https://web.whatsapp.com'
        ? rawUrl
        : 'https://web.whatsapp.com/';
    const title = message && typeof message.title === 'string'
        ? message.title.replace(/\s+/g, ' ').trim().slice(0, 180)
        : '';
    const payload = { decision, url, title };
    const caller = message && typeof message.caller === 'string'
        ? message.caller.replace(/\s+/g, ' ').trim().slice(0, 180)
        : '';
    if (caller) payload.caller = caller;
    const profile = whatsAppProfileId(message && message.profile);
    if (profile) payload.profile = profile;
    return payload;
}

export async function reportWhatsAppCallRing(message, options = {}) {
    if (options.sender && !isWhatsAppRingSender(options.sender)) {
        return { ok: false, stored: false, error: 'ignored sender' };
    }
    const payload = whatsAppRingPayload(message);
    if (!payload) return { ok: false, stored: false, error: 'unknown decision' };
    const fetchImpl = options.fetchImpl;
    if (typeof fetchImpl !== 'function') {
        return { ok: false, stored: false, error: 'no fetch' };
    }
    const base = String(options.apiBase || 'http://127.0.0.1:3002/api/magician/v2').replace(/\/$/, '');
    try {
        const response = await fetchImpl(`${base}${WHATSAPP_RING_PATH}`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(payload)
        });
        const status = Number(response && response.status);
        const stored = status >= 200 && status < 300;
        return { ok: stored || status === 404, stored, status };
    } catch (error) {
        return {
            ok: false,
            stored: false,
            error: error && error.message ? error.message : String(error)
        };
    }
}

export async function readWhatsAppCallRing(options = {}) {
    const fetchImpl = options.fetchImpl;
    if (typeof fetchImpl !== 'function') return null;
    const base = String(options.apiBase || 'http://127.0.0.1:3002/api/magician/v2').replace(/\/$/, '');
    try {
        const response = await fetchImpl(`${base}${WHATSAPP_RING_PATH}`, { method: 'GET' });
        const status = Number(response && response.status);
        if (!(status >= 200 && status < 300)) return null;
        const body = typeof response.json === 'function' ? await response.json() : null;
        const phase = body && typeof body.phase === 'string' ? body.phase : '';
        if (!phase) return null;
        const surface = body && (body.surface === 'ringing' || body.surface === 'ongoing')
            ? body.surface
            : 'none';
        return { phase, profile: whatsAppProfileId(body && body.profile), surface };
    } catch (_) {
        return null;
    }
}

function whatsAppOfferFields(message) {
    return {
        url: message && message.url,
        title: message && message.title,
        caller: message && message.caller
    };
}

function whatsAppTalkReady(decision, view) {
    const phase = view && view.phase;
    const surface = view && view.surface;
    if (decision === 'listen') {
        return phase === 'incoming'
            || phase === 'ongoing'
            || (phase === 'failed' && surface && surface !== 'none');
    }
    return phase === 'incoming' || (phase === 'failed' && surface === 'ringing');
}

// Join only starts voice after the server has seen the ring. A Join click
// that arrives while the ring post is still missing announces the ring first.
export async function reportWhatsAppAttendance(message, options = {}) {
    const decision = message && message.decision;
    if (decision !== 'join' && decision !== 'approved' && decision !== 'listen') {
        return reportWhatsAppCallRing(message, options);
    }
    const view = await readWhatsAppCallRing(options);
    const phase = view && view.phase;
    const quiet = phase === 'approved' || phase === 'listening' || phase === 'leaving';
    if (!quiet && !whatsAppTalkReady(decision, view)) {
        const prelude = decision === 'listen' && message && message.surface === 'ongoing'
            ? 'ongoing'
            : 'incoming';
        const announced = await reportWhatsAppCallRing({
            ...whatsAppOfferFields(message),
            decision: prelude
        }, options);
        if (!announced.ok || (!announced.stored && announced.status !== 404)) return announced;
    }
    return reportWhatsAppCallRing({
        ...whatsAppOfferFields(message),
        decision,
        profile: whatsAppTalkProfile((message && message.profile) || (view && view.profile))
    }, options);
}

// `applied` is the decision already sent to the WhatsApp tab.
// Join and listen are sent once. A later ring resets that latch.
export function planRingApply(applied, phase) {
    if (phase === 'approved' || phase === 'listening') {
        const verb = phase === 'listening' ? 'listen' : 'join';
        const already = applied === verb || (phase === 'approved' && (applied === 'approved' || applied === 'join'));
        if (already) return { applied: verb, apply: null, keepPolling: false };
        return { applied: verb, apply: verb, keepPolling: true };
    }
    if (phase === 'leaving') {
        if (applied === 'leave') return { applied: 'leave', apply: null, keepPolling: true };
        return { applied: 'leave', apply: 'leave', keepPolling: true };
    }
    if (phase === 'cancelled') {
        if (applied === 'cancel') return { applied: 'cancel', apply: null, keepPolling: false };
        return { applied: 'cancel', apply: 'cancel', keepPolling: false };
    }
    if (phase === 'ignored') {
        if (applied === 'ignored') return { applied: 'ignored', apply: null, keepPolling: false };
        return { applied: 'ignored', apply: 'ignore', keepPolling: false };
    }
    if (phase === 'incoming' || phase === 'ongoing') {
        return { applied: null, apply: null, keepPolling: true };
    }
    return { applied: null, apply: null, keepPolling: false };
}
