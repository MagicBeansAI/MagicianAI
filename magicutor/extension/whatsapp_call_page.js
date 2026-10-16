(function installWhatsAppCallPage() {
  'use strict';

  if (window.top !== window) return;
  const watch = globalThis.MagicianWhatsAppCall;
  if (!watch || location.hostname !== 'web.whatsapp.com') return;
  // A revived injection must not start a second watcher beside this one.
  if (globalThis.__magicianWhatsAppPageInstalled) return;
  globalThis.__magicianWhatsAppPageInstalled = true;

  let state = watch.initialRingState();
  let acceptEl = null;
  let declineEl = null;
  let endEl = null;
  let timer = 0;
  let surface = { armed: false, seenUp: false };
  let scanned = false;

  function notify(decision, fromStatus) {
    try {
      chrome.runtime.sendMessage({
        type: 'whatsapp_call_ring',
        decision,
        url: location.href,
        title: document.title || '',
        caller: state.caller || '',
        surface: fromStatus === 'ongoing' ? 'ongoing' : ''
      });
    } catch (_) {
      // The service worker may be restarting. The banner still works.
    }
  }

  function showBanner() {
    watch.renderCallBanner(document, {
      kind: state.status === 'ongoing' ? 'ongoing' : 'incoming',
      caller: state.caller || '',
      onChoose(decision) {
        apply(decision);
      }
    });
  }

  function apply(event, caller) {
    const fromStatus = state.status;
    const next = watch.reduceRing(state, event, caller || state.caller);
    if (next.status === 'prompting' && state.status !== 'prompting' && !surface.seenUp) {
      surface = { armed: false, seenUp: false };
    }
    if (next.status === 'joined' || next.status === 'listening') surface.armed = true;
    state = { status: next.status, caller: next.caller || '' };
    for (const effect of next.effects) {
      if (effect === 'show') showBanner();
      else if (effect === 'hide') watch.removeCallBanner(document);
      else if (effect === 'clickAccept' && acceptEl && typeof acceptEl.click === 'function') {
        try { acceptEl.click(); } catch (_) {}
      } else if (effect === 'clickHangup') {
        clickHangup();
      } else if (effect === 'local:ignore') {
        try {
          chrome.runtime.sendMessage({ type: 'whatsapp_call_choose', decision: 'ignore' });
        } catch (_) {}
      } else if (effect.indexOf('notify:') === 0) {
        notify(effect.slice('notify:'.length), fromStatus);
      }
    }
  }

  function scan() {
    try {
      scanControls();
    } finally {
      scanned = true;
    }
  }

  function hangupElement() {
    const controls = watch.collectCallControls(document);
    const end = controls.find((control) => watch.isEndCallName(control.name));
    if (end && end.element) return end.element;
    const decline = controls.find((control) => watch.isDeclineName(control.name));
    if (decline && decline.element) return decline.element;
    return endEl || declineEl;
  }

  function clickHangup() {
    const element = hangupElement();
    if (element && typeof element.click === 'function') {
      try { element.click(); } catch (_) {}
    }
  }

  function scanControls() {
    const controls = watch.collectCallControls(document);
    const view = watch.classifyCallControls(controls);
    const up = watch.callIsUp(controls);
    if (view.kind === 'incoming') {
      acceptEl = view.accept.element;
      declineEl = view.decline.element;
      endEl = null;
      apply('incoming', watch.callerNear(view.accept.element));
    } else if (up) {
      const end = controls.find((control) => watch.isEndCallName(control.name));
      acceptEl = null;
      declineEl = null;
      endEl = end && end.element;
      apply('ongoing', watch.callerNear(endEl));
    } else {
      acceptEl = null;
      declineEl = null;
      endEl = null;
      apply('gone');
    }
    const noted = watch.noteCallSurface(surface, up);
    surface = { armed: noted.armed, seenUp: noted.seenUp };
    if (noted.ended) notify('ended');
  }

  function pollServer() {
    if (state.status !== 'prompting' && state.status !== 'ongoing') return;
    try {
      chrome.runtime.sendMessage({ type: 'whatsapp_call_poll' }, (response) => {
        const runtime = globalThis.chrome && chrome.runtime;
        if (runtime && runtime.lastError) return;
        if (!response || !response.apply) return;
        const decision = response.apply === 'approve' ? 'join' : response.apply;
        if (decision === 'join' || decision === 'listen' || decision === 'ignore' || decision === 'cancel') {
          apply(decision);
        }
      });
    } catch (_) {
      // The service worker may be restarting. The banner still works.
    }
  }

  function schedule() {
    if (timer) return;
    timer = setTimeout(() => {
      timer = 0;
      scan();
    }, 200);
  }

  function start() {
    if (!document.body) {
      document.addEventListener('DOMContentLoaded', start, { once: true });
      return;
    }
    watch.removeCallBanner(document);
    const observer = new MutationObserver(schedule);
    observer.observe(document.body, {
      subtree: true,
      childList: true,
      attributes: true,
      attributeFilter: ['aria-label', 'aria-hidden', 'role']
    });
    schedule();
    setInterval(pollServer, 2000);
    try {
      chrome.runtime.sendMessage({ type: 'whatsapp_browser_bar_sync' }, (response) => {
        const runtime = globalThis.chrome && chrome.runtime;
        if (runtime && runtime.lastError) return;
        watch.setBrowserBar(!response || response.enabled !== false);
        if (!watch.browserBarEnabled()) watch.removeCallBanner(document);
      });
    } catch (_) {
      // Pages without the extension runtime keep the buttons.
    }
  }

  try {
    chrome.runtime.onMessage.addListener((message, _sender, sendResponse) => {
      if (!message) return;
      if (message.type === 'whatsapp_call_snapshot') {
        const answer = () => {
          const kind = state.status === 'prompting'
            ? 'incoming'
            : state.status === 'ongoing'
              ? 'ongoing'
              : '';
          sendResponse({
            kind,
            caller: state.caller || '',
            url: location.href,
            title: document.title || ''
          });
        };
        if (scanned) {
          answer();
          return;
        }
        setTimeout(answer, 300);
        return true;
      }
      if (message.type === 'whatsapp_browser_bar') {
        watch.setBrowserBar(message.enabled !== false);
        if (!watch.browserBarEnabled()) watch.removeCallBanner(document);
        else if (state.status === 'prompting' || state.status === 'ongoing') showBanner();
        return;
      }
      if (message.type !== 'whatsapp_call_apply') return;
      const decision = message.decision === 'approve' ? 'join' : message.decision;
      if (
        decision === 'join' || decision === 'listen' || decision === 'ignore'
        || decision === 'cancel' || decision === 'leave' || decision === 'ended'
      ) {
        // A just-injected script has not scanned yet. Find Accept, Decline,
        // or End call before the click, or it is skipped.
        if (state.status === 'clear' && decision !== 'ignore') scanControls();
        apply(decision === 'ended' ? 'leave' : decision);
      }
    });
  } catch (_) {
    // Tests and pages without the extension runtime keep the banner buttons.
  }

  start();
})();
