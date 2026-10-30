// Pairing and per-run browser-choice prompts for the Magicutor bridge.
//
// Magicutor admits a browser it has not seen before as `pending`: it holds a
// bridge slot but gets no work until the owner accepts it here (or remotely,
// from Magician). When an attended run could use several accepted browsers,
// Magicutor sends every one of them a "Use this browser for '<task>'?" card;
// the first answer wins and the others are withdrawn.
//
// This module is the background worker's in-memory view of those prompts plus
// the pure rendering the popup and side panel share. No chrome.* calls here.

const BRIDGE_CONSENT_STORAGE_KEY = 'magican_bridge_consent_v1';

/** Stored consent: 'accepted', 'declined', or 'pending' (ask). */
async function readBridgeConsent(storage = globalThis.chrome?.storage?.local) {
    try {
        const stored = storage ? (await storage.get(BRIDGE_CONSENT_STORAGE_KEY))?.[BRIDGE_CONSENT_STORAGE_KEY] : null;
        return ['accepted', 'declined', 'pending'].includes(stored) ? stored : 'pending';
    } catch (_) {
        return 'pending';
    }
}

async function writeBridgeConsent(value, storage = globalThis.chrome?.storage?.local) {
    try {
        await storage?.set({ [BRIDGE_CONSENT_STORAGE_KEY]: value });
    } catch (_) {
        // The live decision still applies; it is only not remembered.
    }
}

function createBridgePrompts({ now = () => Date.now() } = {}) {
    let pairing = null;          // 'pending' | 'accepted' | 'legacy' | null (not connected)
    let fingerprint = null;
    const choices = new Map();   // choiceId -> { choiceId, task, expiresAt }

    function prune() {
        const at = now();
        for (const [id, choice] of choices) {
            if (choice.expiresAt <= at) choices.delete(id);
        }
    }

    return {
        /** Apply a server notice (or the local `bridge_closed` marker). */
        applyNotice(envelope) {
            switch (envelope?.type) {
                case 'browser_state':
                    pairing = typeof envelope.state === 'string' ? envelope.state : null;
                    fingerprint = typeof envelope.fingerprint === 'string' ? envelope.fingerprint : null;
                    if (pairing !== 'accepted') choices.clear();
                    return true;
                case 'browser_choice_request': {
                    if (typeof envelope.choice_id !== 'string') return false;
                    const timeoutMs = Math.max(1, Number(envelope.timeout_secs) || 20) * 1000;
                    choices.set(envelope.choice_id, {
                        choiceId: envelope.choice_id,
                        task: String(envelope.task || 'this task'),
                        expiresAt: now() + timeoutMs
                    });
                    return true;
                }
                case 'browser_choice_withdrawn':
                    return choices.delete(envelope.choice_id);
                case 'bridge_closed':
                    pairing = null;
                    choices.clear();
                    return true;
                default:
                    return false;
            }
        },
        /** Forget a choice this browser just answered. */
        answered(choiceId) {
            return choices.delete(choiceId);
        },
        /** Mark the pairing decided locally before the server confirms it. */
        setPairing(state) {
            pairing = state;
            if (state !== 'accepted') choices.clear();
        },
        snapshot({ consent = 'pending' } = {}) {
            prune();
            const pendingChoices = [...choices.values()];
            const declined = consent === 'declined';
            return {
                pairing,
                fingerprint,
                declined,
                choices: pendingChoices,
                needsAttention: !declined && (pairing === 'pending' || pendingChoices.length > 0)
            };
        }
    };
}

/** Toolbar badge for a prompt snapshot. */
function badgeTextFor(snapshot) {
    return snapshot?.needsAttention ? '!' : '';
}

function escapePromptText(value) {
    return String(value ?? '').replace(/[&<>"']/g, (ch) => ({
        '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;'
    })[ch]);
}

/**
 * Panel markup for a prompt snapshot. Buttons carry `data-bridge-action`
 * (`accept`, `decline`, `connect`, `choose`) and, for choices,
 * `data-choice-id` / `data-always`.
 */
function bridgePromptsHtml(snapshot) {
    if (!snapshot) return '';
    if (snapshot.declined) {
        return `
            <div class="bridge-prompt">
                <div class="bridge-prompt-title">This browser is not connected to Magician.</div>
                <div class="bridge-prompt-actions">
                    <button type="button" data-bridge-action="connect">Connect</button>
                </div>
            </div>`;
    }
    const parts = [];
    if (snapshot.pairing === 'pending') {
        const fingerprint = snapshot.fingerprint ? ` <code>${escapePromptText(snapshot.fingerprint)}</code>` : '';
        parts.push(`
            <div class="bridge-prompt">
                <div class="bridge-prompt-title">Connect this browser to Magician?</div>
                <div class="bridge-prompt-detail">Magician can then run tasks in this browser's tabs.${fingerprint}</div>
                <div class="bridge-prompt-actions">
                    <button type="button" data-bridge-action="accept">Accept</button>
                    <button type="button" data-bridge-action="decline">Decline</button>
                </div>
            </div>`);
    }
    for (const choice of snapshot.choices || []) {
        const id = escapePromptText(choice.choiceId);
        parts.push(`
            <div class="bridge-prompt">
                <div class="bridge-prompt-title">Use this browser for “${escapePromptText(choice.task)}”?</div>
                <div class="bridge-prompt-actions">
                    <button type="button" data-bridge-action="choose" data-choice-id="${id}" data-always="false">Accept</button>
                    <button type="button" data-bridge-action="choose" data-choice-id="${id}" data-always="true">Always use this browser</button>
                </div>
            </div>`);
    }
    return parts.join('');
}

export {
    BRIDGE_CONSENT_STORAGE_KEY,
    badgeTextFor,
    bridgePromptsHtml,
    createBridgePrompts,
    readBridgeConsent,
    writeBridgeConsent
};
