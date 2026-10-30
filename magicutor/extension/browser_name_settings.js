/**
 * Wire the "Browser name" setting: the label Magicutor shows for this browser
 * and matches against a run's `browser` selector. Useful when two profiles of
 * the same browser are connected and both default to its brand name.
 */
export function wireBrowserNameSettings() {
    const input = document.getElementById('browser-name');
    const save = document.getElementById('browser-name-save');
    const reset = document.getElementById('browser-name-reset');
    const status = document.getElementById('browser-name-status');
    if (!input || !save || !reset || !status) return;

    const show = (identity) => {
        if (!identity?.label) {
            status.textContent = identity?.error || 'Browser identity unavailable.';
            return;
        }
        input.placeholder = identity.brand || 'Browser';
        input.value = identity.customLabel || '';
        status.textContent = `Shown to Magicutor as “${identity.label}”.`;
    };

    const rename = async (label) => {
        try {
            show(await chrome.runtime.sendMessage({ action: 'set_browser_label', label }));
        } catch (error) {
            status.textContent = error?.message || 'Could not rename this browser.';
        }
    };

    save.addEventListener('click', () => {
        void rename((input.value || '').trim());
    });

    reset.addEventListener('click', () => {
        void rename('');
    });

    void chrome.runtime.sendMessage({ action: 'get_browser_identity' })
        .then(show)
        .catch(() => show(null));
}
