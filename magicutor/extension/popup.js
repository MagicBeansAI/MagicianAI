import { initControlPanel } from './control_panel.js';
import { wireAmbientPairing } from './ambient_pairing.js';
import { wireMagicianBearerSettings } from './magician_bearer_settings.js';
import { setupPanelTabs, wireCollapsibleStatus } from './panel_tabs.js';

void initControlPanel({
    surface: 'popup',
    trackActiveTab: false
});

setupPanelTabs();
wireCollapsibleStatus();

// WEG ambient-capture pairing (P2.1b) — shared with the side panel.
wireAmbientPairing();
wireMagicianBearerSettings();
