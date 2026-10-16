(function installWhatsAppCallPrompt() {
  'use strict';

  if (window.top !== window) return;
  if (location.hostname === 'web.whatsapp.com') return;
  const watch = globalThis.MagicianWhatsAppCall;
  if (!watch) return;
  // executeScript can run this again in a tab that already has the listener.
  if (globalThis.__magicianWhatsAppPromptInstalled) return;
  globalThis.__magicianWhatsAppPromptInstalled = true;

  let offer = null;

  function paint() {
    if (!offer) {
      watch.removeCallBanner(document);
      return;
    }
    watch.renderCallBanner(document, {
      kind: offer.kind === 'ongoing' ? 'ongoing' : 'incoming',
      caller: offer.caller || '',
      onChoose(decision) {
        const chosen = offer;
        offer = null;
        watch.removeCallBanner(document);
        try {
          chrome.runtime.sendMessage({
            type: 'whatsapp_call_choose',
            decision,
            caller: chosen && chosen.caller || '',
            url: chosen && chosen.url || '',
            title: chosen && chosen.title || ''
          });
        } catch (_) {
          // The service worker may be restarting. The buttons already hid.
        }
      }
    });
  }

  function applyOffer(message) {
    if (!message) return;
    if (message.type === 'whatsapp_browser_bar') {
      const watchBar = globalThis.MagicianWhatsAppCall;
      if (watchBar && typeof watchBar.setBrowserBar === 'function') {
        watchBar.setBrowserBar(message.enabled !== false);
      }
      if (message.enabled === false) {
        offer = null;
        paint();
      }
      return;
    }
    if (message.type !== 'whatsapp_call_offer') return;
    if (message.browserBar === false) {
      offer = null;
      paint();
      return;
    }
    if (message.kind === 'hide' || !message.kind) {
      if (!offer && !document.getElementById('magician-whatsapp-call-banner')) return;
      offer = null;
      paint();
      return;
    }
    const next = {
      kind: message.kind === 'ongoing' ? 'ongoing' : 'incoming',
      caller: message.caller || '',
      url: message.url || '',
      title: message.title || ''
    };
    const banner = document.getElementById('magician-whatsapp-call-banner');
    if (
      offer
      && banner
      && offer.kind === next.kind
      && offer.caller === next.caller
      && offer.url === next.url
    ) {
      offer = next;
      return;
    }
    offer = next;
    paint();
  }

  function isMagicianApp() {
    const host = location.hostname;
    return (host === 'localhost' || host === '127.0.0.1') && location.port === '5173';
  }

  // Observe's WhatsApp Join and Leave run in the page. Forward them so the
  // WhatsApp tab clicks Accept or End call, the same as the banner.
  window.addEventListener('magician-whatsapp-browser-bar', (event) => {
    if (!isMagicianApp()) return;
    const detail = event && event.detail;
    try {
      chrome.runtime.sendMessage({
        type: 'whatsapp_browser_bar',
        enabled: Boolean(detail && detail.enabled)
      });
    } catch (_) {
      // The service worker may be restarting. The switch stays on this page.
    }
  });

  window.addEventListener('magician-whatsapp-act', (event) => {
    if (!isMagicianApp()) return;
    const detail = event && event.detail;
    const decision = detail && detail.decision;
    if (decision !== 'join' && decision !== 'listen' && decision !== 'leave' && decision !== 'cancel') {
      return;
    }
    try {
      chrome.runtime.sendMessage({
        type: 'whatsapp_call_choose',
        decision,
        profile: detail.profile || '',
        url: 'https://web.whatsapp.com/'
      });
    } catch (_) {
      // The service worker may be restarting. The server post still runs.
    }
  });

  try {
    chrome.runtime.onMessage.addListener((message) => {
      applyOffer(message);
    });
    // A call may already be up. The background answers with the current offer.
    chrome.runtime.sendMessage({ type: 'whatsapp_call_sync' }, (response) => {
      const runtime = globalThis.chrome && chrome.runtime;
      if (runtime && runtime.lastError) return;
      applyOffer(response);
    });
  } catch (_) {
    // Pages without the extension runtime do not show the call banner.
  }
})();
