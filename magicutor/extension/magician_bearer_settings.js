import { MAGICIAN_BEARER_STORAGE_KEY, resolveMagicianBearer } from './magician_scope.js';

/** Wire the PAT/session bearer used by extension → Magician API requests. */
export function wireMagicianBearerSettings() {
    const input = document.getElementById('magician-bearer-token');
    const save = document.getElementById('magician-bearer-save');
    const clear = document.getElementById('magician-bearer-clear');
    const status = document.getElementById('magician-bearer-status');
    if (!input || !save || !clear || !status) return;

    const refresh = async () => {
        status.textContent = await resolveMagicianBearer() ? 'Bearer installed ✓' : 'No bearer installed';
    };

    save.addEventListener('click', async () => {
        const token = (input.value || '').trim();
        if (!token) {
            status.textContent = 'Enter a Magician bearer first.';
            return;
        }
        await chrome.storage.local.set({ [MAGICIAN_BEARER_STORAGE_KEY]: token });
        input.value = '';
        await refresh();
    });

    clear.addEventListener('click', async () => {
        await chrome.storage.local.remove(MAGICIAN_BEARER_STORAGE_KEY);
        await refresh();
    });

    void refresh();
}
