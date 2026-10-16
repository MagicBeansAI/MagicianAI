// stealth-init.js — JS-layer belt-and-suspenders shim for CloakBrowser sessions.
//
// When agent-browser launches the CloakBrowser binary directly via
// AGENT_BROWSER_EXECUTABLE_PATH, the binary's compiled-in C++ patches
// already cover navigator.webdriver and the HeadlessChrome UA. This file
// is defense in depth — it re-applies the same patches at the JS layer so
// a regression in the binary (e.g. a Chromium upgrade that re-enables a
// signal we thought was patched) still leaves us covered.
//
// Order matters: this script must run BEFORE any page script. agent-browser
// passes the path via --init-script / AGENT_BROWSER_INIT_SCRIPTS, which
// translates to Chrome DevTools Protocol's
// `Page.addScriptToEvaluateOnNewDocument` — meaning we get a synchronous
// hook before window.onload fires.

(() => {
  // 1. navigator.webdriver -> false. Defining on the prototype is more
  //    robust than directly on the instance (some detectors check Navigator.prototype).
  try {
    Object.defineProperty(Navigator.prototype, 'webdriver', {
      get: () => false,
      configurable: true,
    });
  } catch (e) {}

  // 2. User-Agent: strip the "Headless" prefix some Chromium builds leak.
  //    Re-derive from the current UA so we don't have to hardcode a version.
  try {
    const real = navigator.userAgent;
    if (real.includes('HeadlessChrome/')) {
      const cleaned = real.replace('HeadlessChrome/', 'Chrome/');
      Object.defineProperty(Navigator.prototype, 'userAgent', {
        get: () => cleaned,
        configurable: true,
      });
      // userAgentData.brands also leaks "Chromium" — touch up if present.
      if (navigator.userAgentData && Array.isArray(navigator.userAgentData.brands)) {
        const patched = navigator.userAgentData.brands.map((b) => {
          if (b && b.brand === 'Chromium') return { ...b, brand: 'Google Chrome' };
          return b;
        });
        try {
          Object.defineProperty(navigator.userAgentData, 'brands', {
            get: () => patched,
            configurable: true,
          });
        } catch (e) {}
      }
    }
  } catch (e) {}

  // 3. window.chrome should look populated. Bare Chromium leaves it
  //    half-empty in some configurations; detectors often check chrome.runtime.
  try {
    if (typeof window.chrome === 'undefined' || !window.chrome) {
      window.chrome = {};
    }
    if (!window.chrome.runtime) {
      window.chrome.runtime = {
        OnInstalledReason: { INSTALL: 'install', UPDATE: 'update' },
        OnRestartRequiredReason: { APP_UPDATE: 'app_update', OS_UPDATE: 'os_update' },
        PlatformOs: { MAC: 'mac', WIN: 'win', LINUX: 'linux' },
      };
    }
    if (typeof window.chrome.loadTimes !== 'function') {
      window.chrome.loadTimes = () => ({});
    }
    if (typeof window.chrome.csi !== 'function') {
      window.chrome.csi = () => ({});
    }
  } catch (e) {}

  // 4. navigator.plugins / navigator.mimeTypes — some detectors trip when
  //    plugins.length === 0. The binary patches usually keep this populated
  //    but we guard against zero-plugin builds just in case.
  try {
    if (!navigator.plugins || navigator.plugins.length === 0) {
      const fakePlugins = [
        { name: 'PDF Viewer', filename: 'internal-pdf-viewer' },
        { name: 'Chrome PDF Viewer', filename: 'internal-pdf-viewer' },
        { name: 'Chromium PDF Viewer', filename: 'internal-pdf-viewer' },
      ];
      Object.defineProperty(Navigator.prototype, 'plugins', {
        get: () => Object.assign(fakePlugins, { length: fakePlugins.length }),
        configurable: true,
      });
    }
  } catch (e) {}

  // 5. Permissions.query — Chromium's permissions API sometimes leaks a
  //    "denied" state on notifications that a real browser doesn't. Patch
  //    notifications/clipboard-read/clipboard-write to mirror real-Chrome
  //    behavior of returning "prompt" when not explicitly granted.
  try {
    const realQuery = navigator.permissions && navigator.permissions.query;
    if (realQuery) {
      navigator.permissions.query = function (params) {
        if (params && params.name && [
          'notifications', 'clipboard-read', 'clipboard-write',
        ].includes(params.name)) {
          return Promise.resolve({ state: 'prompt', onchange: null });
        }
        return realQuery.call(navigator.permissions, params);
      };
    }
  } catch (e) {}
})();
