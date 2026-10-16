// Shared panel chrome for the popup + side panel surfaces.

// Tab switching over `[data-panel-tab]` buttons and `[data-panel-view]` views.
// Activates whichever tab is marked `active` in markup, else the first one.
export function setupPanelTabs() {
    const buttons = Array.from(document.querySelectorAll('[data-panel-tab]'));
    const views = Array.from(document.querySelectorAll('[data-panel-view]'));
    if (!buttons.length) return;

    function activate(tabName) {
        for (const button of buttons) {
            button.classList.toggle('active', button.dataset.panelTab === tabName);
        }
        for (const view of views) {
            view.classList.toggle('active', view.dataset.panelView === tabName);
        }
    }

    for (const button of buttons) {
        button.addEventListener('click', () => activate(button.dataset.panelTab));
    }

    const initial = buttons.find((b) => b.classList.contains('active')) || buttons[0];
    activate(initial.dataset.panelTab);
}

// Collapsible connection status: clicking the "Server Ready" header shows/hides
// the Bridge / Magicutor / Magician version breakdown (rendered by
// control_panel.js into #status-details). Collapsed by default to keep the
// surface compact; the summary line stays visible.
export function wireCollapsibleStatus() {
    const status = document.getElementById('status');
    const details = document.getElementById('status-details');
    const caret = document.getElementById('status-caret');
    if (!status || !details) return;

    function render() {
        if (caret) caret.textContent = details.classList.contains('collapsed') ? '▸' : '▾';
    }

    details.classList.add('collapsed');
    render();
    status.addEventListener('click', () => {
        details.classList.toggle('collapsed');
        render();
    });
}
