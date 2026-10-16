// WEG ambient-capture pairing (P2.1b) — shared by the popup and the side panel.
//
// Stores/clears the collector token in the background's `chrome.storage.local`
// so ambient batch uploads carry `X-Collector-Token` and the server resolves
// scope from it. The token is issued by the Observe page's "Pair browser"
// control and pasted here. Both surfaces use this same wiring so the controls
// behave identically; call it once after the surface's DOM is present.
export function wireAmbientPairing() {
    const tokenInput = document.getElementById('weg-pair-token');
    const saveBtn = document.getElementById('weg-pair-save');
    const clearBtn = document.getElementById('weg-pair-clear');
    const statusEl = document.getElementById('weg-pair-status');
    if (!tokenInput || !saveBtn || !clearBtn || !statusEl) return;

    function refreshStatus() {
        try {
            chrome.runtime.sendMessage({ type: 'weg_collector_status' }, (resp) => {
                if (chrome.runtime.lastError) return;
                statusEl.textContent = resp?.paired ? 'Paired ✓' : 'Not paired';
            });
        } catch (_) {
            /* background asleep — ignore */
        }
    }

    saveBtn.addEventListener('click', () => {
        const token = (tokenInput.value || '').trim();
        if (!token) {
            statusEl.textContent = 'Enter a pairing token first.';
            return;
        }
        chrome.runtime.sendMessage({ type: 'weg_pair_collector', token }, (resp) => {
            if (chrome.runtime.lastError || !resp?.ok) {
                statusEl.textContent = 'Pairing failed.';
                return;
            }
            tokenInput.value = '';
            statusEl.textContent = 'Paired ✓';
        });
    });

    clearBtn.addEventListener('click', () => {
        chrome.runtime.sendMessage({ type: 'weg_pair_collector', token: '' }, () => {
            statusEl.textContent = 'Not paired';
        });
    });

    refreshStatus();
}
