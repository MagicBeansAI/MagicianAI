import { initControlPanel } from './control_panel.js';
import { initLivePreviewPanel } from './live_preview_panel.js';
import { wireAmbientPairing } from './ambient_pairing.js';
import { wireMagicianBearerSettings } from './magician_bearer_settings.js';
import { setupPanelTabs, wireCollapsibleStatus } from './panel_tabs.js';

void initControlPanel({
    surface: 'sidepanel',
    trackActiveTab: true
});

setupPanelTabs();
wireCollapsibleStatus();
initLivePreviewPanel();

// WEG ambient-capture pairing (P2.1b) — same controls as the popup.
wireAmbientPairing();
wireMagicianBearerSettings();
