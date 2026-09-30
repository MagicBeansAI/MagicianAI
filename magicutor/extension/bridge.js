// WebSocket bridge connection to Magicutor HTTP server.

import { MAGICUTOR_BRIDGE_URL } from './config.js';

const RECONNECT_INTERVAL = 5000; // 5 seconds
const MAX_RECONNECT_INTERVAL = 60000;
const BRIDGE_AUTH_TOKEN = 'magicutor-bridge-dev-token';
const KEEPALIVE_INTERVAL = 20000; // 20 seconds - MUST be < 30s (Chrome SW termination timeout)
// If magicutor restarts, our socket can stay readyState===OPEN (no clean close)
// while the server is gone — a "half-open" bridge where pings vanish and onclose
// never fires (the extension side-panel still shows "connected" but magicutor logs
// "Bridge not connected"). Treat the bridge as dead after this long without a pong
// and force a reconnect (see startKeepAlive). magicutor replies pong to our ping
// (magicutor/src/server/bridge.rs:273-280), so a healthy bridge never trips this.
const PONG_STALE_MS = KEEPALIVE_INTERVAL * 2.5; // ~50s without a pong => reconnect
// Backpressure cap for best-effort CDP events (sendCdpEventToBridge): a `DOM.enable`
// on a high-mutation page (e.g. an infinite feed) emits thousands of DOM events/sec;
// forwarding each one can grow the WS send buffer unbounded and OOM/stall the MV3
// service worker. Drop events once the buffer exceeds this.
const MAX_EVENT_BUFFER_BYTES = 64 * 1024 * 1024; // 64 MB

/**
 * Sanitize a string to remove control characters that can cause JSON parsing errors.
 * Removes: ASCII control chars (0x00-0x1F except \t\n\r), DEL (0x7F), and lone surrogates.
 */
function sanitizeString(str) {
    if (typeof str !== 'string') return str;

    // Remove control characters except tab, newline, carriage return
    // Also remove DEL (0x7F)
    let result = str.replace(/[\x00-\x08\x0B\x0C\x0E-\x1F\x7F]/g, '');

    // Handle lone surrogates by checking each character
    let sanitized = '';
    for (let i = 0; i < result.length; i++) {
        const code = result.charCodeAt(i);
        // Check for high surrogate
        if (code >= 0xD800 && code <= 0xDBFF) {
            // Check if next char is a valid low surrogate
            if (i + 1 < result.length) {
                const nextCode = result.charCodeAt(i + 1);
                if (nextCode >= 0xDC00 && nextCode <= 0xDFFF) {
                    // Valid surrogate pair - keep both
                    sanitized += result[i] + result[i + 1];
                    i++; // Skip the low surrogate
                    continue;
                }
            }
            // Lone high surrogate - replace with replacement char
            sanitized += '\uFFFD';
        } else if (code >= 0xDC00 && code <= 0xDFFF) {
            // Lone low surrogate - replace with replacement char
            sanitized += '\uFFFD';
        } else {
            sanitized += result[i];
        }
    }

    return sanitized;
}

/**
 * Recursively sanitize all string values in an object/array.
 * Used to clean data before JSON serialization to prevent parsing errors.
 */
function sanitizeForJson(obj) {
    if (obj === null || obj === undefined) return obj;
    if (typeof obj === 'string') return sanitizeString(obj);
    if (Array.isArray(obj)) return obj.map(sanitizeForJson);
    if (typeof obj === 'object') {
        const result = {};
        for (const key of Object.keys(obj)) {
            result[key] = sanitizeForJson(obj[key]);
        }
        return result;
    }
    return obj;
}

let bridgeSocket = null;
let bridgeConnected = false;
let keepAliveTimer = null;
let reconnectTimer = null;
let reconnectAttempt = 0;
let bridgeGeneration = 0;
let connectInFlight = null;
let lastConnectConfig = null;
let lastPongAt = 0;          // updated on (re)connect + every pong; drives staleness detection
let droppedEventCount = 0;   // best-effort CDP events dropped under backpressure

function isBridgeConnected() {
    return bridgeConnected && bridgeSocket !== null && bridgeSocket.readyState === WebSocket.OPEN;
}

function startKeepAlive(socket, generation) {
    if (keepAliveTimer) {
        clearInterval(keepAliveTimer);
    }
    // Reset the pong baseline so a fresh connection isn't immediately judged stale.
    lastPongAt = Date.now();

    keepAliveTimer = setInterval(() => {
        if (bridgeSocket === socket && bridgeGeneration === generation
            && socket.readyState === WebSocket.OPEN) {
            // Half-open detection: if the server vanished (e.g. magicutor restart)
            // the socket can stay OPEN here while pings go nowhere and onclose
            // never fires. No pong for PONG_STALE_MS => treat as dead, force close
            // + reconnect (this is what makes the bridge self-heal after a
            // magicutor restart without a manual extension reload).
            if (Date.now() - lastPongAt > PONG_STALE_MS) {
                console.warn('[Magicutor] Bridge stale (no pong for '
                    + Math.round((Date.now() - lastPongAt) / 1000)
                    + 's) — forcing reconnect');
                // Retire this generation before closing. Its eventual onclose is
                // stale and cannot clear or reconnect over a replacement socket.
                bridgeConnected = false;
                bridgeSocket = null;
                bridgeGeneration++;
                stopKeepAlive();
                scheduleReconnect();
                try { socket.close(); } catch (_) { /* already closing */ }
                return;
            }
            console.debug('[Magicutor] Sending keep-alive ping');
            socket.send(JSON.stringify({ type: 'ping', timestamp: Date.now() }));
        } else {
            console.log('[Magicutor] Keep-alive: bridge not connected');
        }
    }, KEEPALIVE_INTERVAL);

    console.log('[Magicutor] Keep-alive timer started (interval: ' + KEEPALIVE_INTERVAL + 'ms)');
}

function stopKeepAlive() {
    if (keepAliveTimer) {
        clearInterval(keepAliveTimer);
        keepAliveTimer = null;
        console.log('[Magicutor] Keep-alive timer stopped');
    }
}

function scheduleReconnect() {
    if (!lastConnectConfig || reconnectTimer || isBridgeConnected()
        || (bridgeSocket && bridgeSocket.readyState === WebSocket.CONNECTING)) {
        return;
    }
    const baseDelay = Math.min(
        RECONNECT_INTERVAL * (2 ** Math.min(reconnectAttempt, 4)),
        MAX_RECONNECT_INTERVAL
    );
    const jitter = Math.floor(baseDelay * 0.2 * Math.random());
    reconnectAttempt++;
    reconnectTimer = setTimeout(() => {
        reconnectTimer = null;
        void connectBridge(lastConnectConfig);
    }, baseDelay + jitter);
}

/**
 * Connect to WebSocket bridge
 * First checks if server is available via HTTP to avoid ERR_CONNECTION_REFUSED errors
 */
async function connectBridge({ checkHttpHealth, onRequest }) {
    lastConnectConfig = { checkHttpHealth, onRequest };

    if (isBridgeConnected()
        || (bridgeSocket && bridgeSocket.readyState === WebSocket.CONNECTING)) {
        return;
    }
    if (connectInFlight) {
        return connectInFlight;
    }

    connectInFlight = (async () => {
      const serverAvailable = await checkHttpHealth();
      if (!serverAvailable) {
          console.log('[Magicutor] Server not available; reconnect scheduled');
          scheduleReconnect();
          return;
      }
      if (isBridgeConnected()
          || (bridgeSocket && bridgeSocket.readyState === WebSocket.CONNECTING)) {
          return;
      }

      try {
        const url = `${MAGICUTOR_BRIDGE_URL}?token=${BRIDGE_AUTH_TOKEN}`;
        console.log(`[Magicutor] Connecting to bridge: ${MAGICUTOR_BRIDGE_URL}`);

        const generation = ++bridgeGeneration;
        const socket = new WebSocket(url);
        bridgeSocket = socket;

        socket.onopen = () => {
            if (bridgeSocket !== socket || bridgeGeneration !== generation) return;
            console.log('[Magicutor] Bridge connected');
            bridgeConnected = true;
            reconnectAttempt = 0;
            if (reconnectTimer) {
                clearTimeout(reconnectTimer);
                reconnectTimer = null;
            }
            lastPongAt = Date.now();
            startKeepAlive(socket, generation);
        };

        socket.onmessage = async (event) => {
            if (bridgeSocket !== socket || bridgeGeneration !== generation) return;
            try {
                const envelope = JSON.parse(event.data);
                const timestamp = new Date().toISOString();
                if (envelope.type === 'request') {
                    console.log(`[Magicutor] ${timestamp} ← RECEIVED request: action=${envelope.data?.action}, id=${envelope.data?.requestId}`);
                } else if (envelope.type === 'pong') {
                    lastPongAt = Date.now();
                    console.debug(`[Magicutor] ${timestamp} ← pong received`);
                } else {
                    console.log(`[Magicutor] ${timestamp} ← Bridge message type=${envelope.type}`);
                }

                if (envelope.type === 'request' && envelope.data) {
                    await onRequest(envelope.data);
                }
            } catch (error) {
                console.error('[Magicutor] Failed to parse bridge message:', error);
            }
        };

        socket.onclose = (event) => {
            if (bridgeSocket !== socket || bridgeGeneration !== generation) return;
            console.log(`[Magicutor] Bridge disconnected: code=${event.code}, reason="${event.reason}", wasClean=${event.wasClean}`);
            bridgeConnected = false;
            bridgeSocket = null;
            stopKeepAlive();
            scheduleReconnect();
        };

        socket.onerror = (error) => {
            if (bridgeSocket !== socket || bridgeGeneration !== generation) return;
            console.log('[Magicutor] Bridge connection error:', error.type || 'connection_failed');
        };
      } catch (error) {
        console.log('[Magicutor] Bridge not available, will retry:', error.message || error);
        scheduleReconnect();
      }
    })();
    try {
        await connectInFlight;
    } finally {
        connectInFlight = null;
    }
}

async function reconnectBridge(config) {
    if (connectInFlight) {
        await connectInFlight;
    }
    lastConnectConfig = config;
    const socket = bridgeSocket;
    bridgeConnected = false;
    bridgeSocket = null;
    bridgeGeneration++;
    stopKeepAlive();
    if (reconnectTimer) {
        clearTimeout(reconnectTimer);
        reconnectTimer = null;
    }
    if (socket) {
        try { socket.close(); } catch (_) { /* already closed */ }
    }
    return connectBridge(config);
}

/**
 * Send response to bridge
 * Uses async to wait for data to be flushed from buffer before returning
 */
async function sendToBridge(response) {
    const timestamp = () => new Date().toISOString();
    const logPrefix = `[Magicutor][${response.requestId}]`;

    if (bridgeSocket && bridgeSocket.readyState === WebSocket.OPEN) {
        const envelope = {
            type: 'response',
            data: response
        };
        try {
            // Sanitize all strings in the response to remove control characters
            // that can cause JSON parsing errors on the Rust side
            const sanitizedEnvelope = sanitizeForJson(envelope);
            const jsonStr = JSON.stringify(sanitizedEnvelope);
            bridgeSocket.send(jsonStr);
            console.log(`${logPrefix} ${timestamp()} → SENDING RESPONSE: success=${response.success}, size=${jsonStr.length} bytes, buffered=${bridgeSocket.bufferedAmount}`);

            if (bridgeSocket.bufferedAmount > 0) {
                console.log(`${logPrefix} ${timestamp()} ⏳ Waiting for buffer to flush: ${bridgeSocket.bufferedAmount} bytes`);
                const maxWait = 30000;
                const checkInterval = 50;
                let waited = 0;

                while (bridgeSocket.bufferedAmount > 0 && waited < maxWait) {
                    await new Promise(resolve => setTimeout(resolve, checkInterval));
                    waited += checkInterval;
                }

                if (bridgeSocket.bufferedAmount > 0) {
                    console.warn(`${logPrefix} ${timestamp()} ⚠ Buffer not fully flushed after ${maxWait}ms: ${bridgeSocket.bufferedAmount} bytes remaining`);
                } else {
                    console.log(`${logPrefix} ${timestamp()} ✓ Buffer flushed after ${waited}ms`);
                }
            }
        } catch (error) {
            console.error(`${logPrefix} ${timestamp()} ✗ SEND FAILED:`, error);
        }
    } else {
        console.error(`${logPrefix} ${timestamp()} ✗ CANNOT SEND: bridge not connected (readyState=${bridgeSocket?.readyState})`);
    }
}

/**
 * Push a fire-and-forget chrome.debugger event to the magicutor server.
 * Used by debugger.js to forward Page.loadEventFired, Target.targetCreated,
 * etc. to the CDP proxy so agent-browser sees them. Best-effort: silently
 * drops if bridge is disconnected — the server has no use for orphaned
 * events, and reconnect logic will resubscribe via attach next time.
 */
function sendCdpEventToBridge(tabId, method, params) {
    if (!bridgeSocket || bridgeSocket.readyState !== WebSocket.OPEN) return;
    // Backpressure: if the socket can't drain fast enough — a high-frequency event
    // flood such as DOM.* notifications after `DOM.enable` on a dynamic page — drop
    // rather than grow the send buffer unbounded and OOM/stall the service worker.
    // These events are best-effort; the proxy tolerates gaps.
    if (bridgeSocket.bufferedAmount > MAX_EVENT_BUFFER_BYTES) {
        droppedEventCount++;
        if (droppedEventCount % 500 === 1) {
            console.warn('[Magicutor] Bridge backpressure: dropping CDP events (buffered='
                + bridgeSocket.bufferedAmount + ' bytes, dropped=' + droppedEventCount + ')');
        }
        return;
    }
    try {
        const envelope = {
            type: 'cdp_event',
            data: { tabId, method, params: params ?? {} }
        };
        bridgeSocket.send(JSON.stringify(sanitizeForJson(envelope)));
    } catch (error) {
        // Don't log every failure — events are noisy and bridge errors
        // get logged elsewhere. Failing silently keeps the hot path fast.
    }
}

export {
    connectBridge,
    reconnectBridge,
    isBridgeConnected,
    sendToBridge,
    sendCdpEventToBridge
};
