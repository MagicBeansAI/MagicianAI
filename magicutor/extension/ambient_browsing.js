/**
 * Ambient browsing capture (WEG Phase 2, P2.2).
 *
 * Runs as a content script on every normal page (top frame only). Computes a
 * METADATA-FIRST signal from the LIVE DOM — no CDP, no debugger banner, no full
 * HTML or screenshots — and hands it to the background collector
 * (`ambient_collector.js`), which queues + batch-uploads to Magician's
 * `/ambient/signals/batch`. The server gates on the user's consent flag (set
 * from the Observe page) and applies the ingress policy, so capturing here is
 * cheap + best-effort.
 *
 * `safeUrl` intentionally mirrors the redaction used by the Magicutor CDP-proxy
 * automation mirror; content scripts can't import the Rust helper, so keep the
 * sensitive-key list aligned when either side changes.
 */
(function () {
    if (window.top !== window) {
        return; // top frame only — skip iframes
    }

    const SENSITIVE_PARAM = /token|secret|password|passwd|auth|key|session|csrf|xsrf|otp/i;
    const DEBOUNCE_MS = 1200;

    function safeUrl(rawUrl) {
        if (!rawUrl || typeof rawUrl !== 'string') return { url: null, origin: null };
        try {
            const parsed = new URL(rawUrl);
            if (parsed.protocol !== 'http:' && parsed.protocol !== 'https:') {
                return { url: null, origin: null };
            }
            for (const key of [...parsed.searchParams.keys()]) {
                if (SENSITIVE_PARAM.test(key)) parsed.searchParams.set(key, '[REDACTED]');
            }
            parsed.hash = '';
            return { url: parsed.toString(), origin: parsed.origin };
        } catch (_) {
            return { url: null, origin: null };
        }
    }

    function domSummary() {
        const q = (sel) => {
            try {
                return document.querySelectorAll(sel).length;
            } catch (_) {
                return 0;
            }
        };
        return {
            formCount: q('form'),
            inputCount: q('input, textarea, select'),
            buttonCount: q('button'),
            linkCount: q('a'),
            headingCount: q('h1, h2, h3, h4, h5, h6'),
            iframeCount: q('iframe'),
            passwordInputCount: q('input[type="password"]')
        };
    }

    function topHeadings() {
        try {
            return [...document.querySelectorAll('h1, h2')]
                .map((h) => (h.textContent || '').trim())
                .filter((t) => t.length > 0)
                .slice(0, 5)
                .map((t) => (t.length > 120 ? `${t.slice(0, 120)}…` : t));
        } catch (_) {
            return [];
        }
    }

    function estimateDomBytes() {
        try {
            const html = document.documentElement?.outerHTML || '';
            return new TextEncoder().encode(html).length;
        } catch (_) {
            return 0;
        }
    }

    function newId() {
        if (globalThis.crypto?.randomUUID) return globalThis.crypto.randomUUID();
        return `amb_${Date.now()}_${Math.random().toString(16).slice(2)}`;
    }

    function capture(eventKind) {
        const { url, origin } = safeUrl(location.href);
        if (!origin) return; // non-http / unparseable → skip

        const summary = domSummary();
        const title = (document.title || '').trim();
        const headingCount = summary.headingCount || 0;
        const hasPasswordField = (summary.passwordInputCount || 0) > 0;
        const domEstimatedBytes = estimateDomBytes();
        const signal = {
            signal_id: newId(),
            origin,
            surface: 'browser',
            event_kind: eventKind,
            summary: title.length > 200 ? `${title.slice(0, 200)}…` : title,
            safe_url: url,
            path: location.pathname,
            title,
            content_type: document.contentType || 'text/html',
            dom_estimated_bytes: domEstimatedBytes,
            heading_count: headingCount,
            has_password_field: hasPasswordField,
            // Metadata-first only: sanitized url, title, a few headings, structure.
            // No body text / HTML. The server further redacts secret-keyed fields.
            extracted_fields: {
                url,
                path: location.pathname,
                title,
                content_type: document.contentType || 'text/html',
                dom_estimated_bytes: domEstimatedBytes,
                heading_count: headingCount,
                has_password_field: hasPasswordField,
                headings: topHeadings(),
                dom_summary: summary
            },
            dedupe_key: `browser:${origin}:${location.pathname}`,
            // A page with password inputs is sensitive; flag it so the server can
            // down-rank / suppress (capture stays metadata-only regardless).
            sensitivity: hasPasswordField ? 'sensitive' : 'unknown'
        };

        try {
            chrome.runtime.sendMessage({ type: 'weg_ambient_signal', signal });
        } catch (_) {
            // Background may be asleep/unavailable; best-effort, dropped silently.
        }
    }

    let timer = null;
    function schedule(eventKind) {
        clearTimeout(timer);
        timer = setTimeout(() => capture(eventKind), DEBOUNCE_MS);
    }

    // Initial load.
    schedule('page_load');

    // SPA route changes: wrap history + popstate so single-page navigations
    // (which don't reload the content script) still emit a signal.
    const fireRouteChange = () => schedule('page_change');
    for (const method of ['pushState', 'replaceState']) {
        const original = history[method];
        if (typeof original === 'function') {
            history[method] = function () {
                const result = original.apply(this, arguments);
                fireRouteChange();
                return result;
            };
        }
    }
    window.addEventListener('popstate', fireRouteChange);
})();
