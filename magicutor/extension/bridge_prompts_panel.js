import { bridgePromptsHtml } from './bridge_prompts.js';

const POLL_INTERVAL_MS = 1500;

/**
 * Show the pairing prompt ("Connect this browser to Magician?") and per-run
 * browser-choice cards in the popup / side panel, and send the owner's
 * answers to the background worker.
 */
export function wireBridgePrompts() {
    const container = document.getElementById('bridge-prompts');
    if (!container) return;

    let lastHtml = null;
    const render = (snapshot) => {
        const html = bridgePromptsHtml(snapshot);
        if (html === lastHtml) return;
        lastHtml = html;
        container.innerHTML = html;
        container.hidden = html === '';
    };

    const refresh = async () => {
        try {
            render(await chrome.runtime.sendMessage({ action: 'get_bridge_prompts' }));
        } catch (_) {
            render(null);
        }
    };

    container.addEventListener('click', async (event) => {
        const button = event.target?.closest?.('[data-bridge-action]');
        if (!button) return;
        button.disabled = true;
        const action = button.dataset.bridgeAction;
        const message = {
            accept: { action: 'bridge_pairing_accept' },
            decline: { action: 'bridge_pairing_decline' },
            connect: { action: 'bridge_connect' },
            choose: {
                action: 'bridge_choice_answer',
                choiceId: button.dataset.choiceId,
                always: button.dataset.always === 'true'
            }
        }[action];
        if (!message) return;
        try {
            await chrome.runtime.sendMessage(message);
        } catch (_) {
            // The next refresh shows whatever state the worker holds.
        }
        await refresh();
    });

    void refresh();
    setInterval(() => void refresh(), POLL_INTERVAL_MS);
}
