(function installWhatsAppCallWatch(globalObject) {
  'use strict';

  // Exact accessible names, plus "Answer voice call" / "Decline call" style
  // labels. "Voice call" and "Video call" are the in-chat place buttons and
  // must not count as a ring. "Calls" is the sidebar tab.
  const ACCEPT_NAMES = new Set(['accept', 'answer', 'accept call', 'answer call']);
  const DECLINE_NAMES = new Set(['decline', 'reject', 'decline call', 'reject call']);
  const BANNER_ID = 'magician-whatsapp-call-banner';

  function normalizeName(value) {
    return String(value || '').replace(/\s+/g, ' ').trim().toLowerCase();
  }

  function isAcceptName(name) {
    const normalized = normalizeName(name);
    if (!normalized || normalized.includes('video')) return false;
    if (ACCEPT_NAMES.has(normalized)) return true;
    return normalized.startsWith('accept ') || normalized.startsWith('answer ');
  }

  function isDeclineName(name) {
    const normalized = normalizeName(name);
    if (!normalized) return false;
    if (DECLINE_NAMES.has(normalized)) return true;
    return normalized.startsWith('decline ') || normalized.startsWith('reject ');
  }

  function controlName(element) {
    if (!element || typeof element.getAttribute !== 'function') return '';
    const aria = element.getAttribute('aria-label');
    if (aria && String(aria).trim()) return String(aria).replace(/\s+/g, ' ').trim();
    const raw = element.innerText || element.textContent || '';
    const flat = String(raw).replace(/\s+/g, ' ').trim();
    if (flat && flat.length <= 40) return flat;
    return '';
  }

  function isShown(element) {
    if (!element) return false;
    if (typeof element.getAttribute === 'function' && element.getAttribute('aria-hidden') === 'true') {
      return false;
    }
    if (typeof element.getBoundingClientRect === 'function') {
      const rect = element.getBoundingClientRect();
      if (!rect || (rect.width === 0 && rect.height === 0)) return false;
    }
    return true;
  }

  function collectCallControls(root) {
    if (!root || typeof root.querySelectorAll !== 'function') return [];
    const nodes = root.querySelectorAll('button, [role="button"]');
    const controls = [];
    for (const element of nodes) {
      if (!isShown(element)) continue;
      if (element.closest && element.closest('#' + BANNER_ID)) continue;
      const name = controlName(element);
      if (!name) continue;
      controls.push({ name, element });
    }
    return controls;
  }

  const END_NAMES = new Set(['end call', 'hang up', 'leave call', 'leave the call']);

  function isEndCallName(name) {
    const normalized = normalizeName(name);
    if (!normalized || normalized.includes('video')) return false;
    if (END_NAMES.has(normalized)) return true;
    return normalized.startsWith('end call')
      || normalized.startsWith('hang up')
      || normalized.startsWith('leave call');
  }

  function callIsUp(controls) {
    return (controls || []).some((control) => isEndCallName(control && control.name));
  }

  // Armed after Join or Listen. Ended only after End call was seen and then left.
  // Accept and Decline disappearing is the call connecting, not a hangup.
  function noteCallSurface(latch, callUp) {
    const armed = Boolean(latch && latch.armed);
    const seenUp = Boolean(latch && latch.seenUp);
    if (!armed) return { armed: false, seenUp: false, ended: false };
    if (callUp) return { armed: true, seenUp: true, ended: false };
    if (seenUp) return { armed: false, seenUp: false, ended: true };
    return { armed: true, seenUp: false, ended: false };
  }

  function classifyCallControls(controls) {
    let accept = null;
    let decline = null;
    for (const control of controls || []) {
      if (!accept && isAcceptName(control.name)) accept = control;
      if (!decline && isDeclineName(control.name)) decline = control;
    }
    if (accept && decline) return { kind: 'incoming', accept, decline };
    return { kind: 'idle' };
  }

  const CALLER_CHROME = new Set([
    'chats', 'calls', 'status', 'search', 'menu', 'archived', 'communities',
    'channels', 'updates', 'settings', 'mute', 'video', 'camera', 'speaker',
    'voice', 'ringing', 'calling', 'whatsapp', 'whatsapp call', 'voice call',
    'video call', 'incoming voice call', 'incoming video call',
    'ongoing voice call', 'ongoing video call'
  ]);

  // The unread page title is "(161) WhatsApp". A caller is the person or the
  // phone number on the call card, including a name that is not Latin letters.
  function callerFromLines(lines) {
    for (const line of lines || []) {
      const flat = String(line || '').replace(/\s+/g, ' ').trim();
      if (!flat || flat.length > 80) continue;
      const lower = flat.toLowerCase();
      const letters = flat.replace(/[^\p{L}]/gu, '').toLowerCase();
      if (letters === 'whatsapp') continue;
      if (CALLER_CHROME.has(lower)) continue;
      if (isAcceptName(flat) || isDeclineName(flat) || isEndCallName(flat)) continue;
      if (lower.includes('incoming') && lower.includes('call')) continue;
      if (lower.includes('end-to-end')) continue;
      const digits = flat.replace(/\D/g, '');
      const phone = digits.length >= 8 && digits.length <= 15
        && /^[+()\d][\d\s().+-]*$/.test(flat);
      if (!letters && !phone) continue;
      return flat;
    }
    return '';
  }

  function callerIn(node, exclude) {
    if (!node || typeof node.querySelectorAll !== 'function') return '';
    const nodes = node.querySelectorAll('span[title], img[alt], h1, h2, h3, [dir="auto"]');
    const lines = [];
    for (const child of nodes) {
      if (exclude && (child === exclude || (exclude.contains && exclude.contains(child)))) continue;
      if (child.closest && child.closest('button, [role="button"], #' + BANNER_ID)) continue;
      if (child.closest && child.closest(
        '[data-testid="cell-frame-title"], [data-testid="chatlist-header"], #pane-side, #side'
      )) continue;
      const title = typeof child.getAttribute === 'function'
        ? (child.getAttribute('title') || child.getAttribute('alt') || '')
        : '';
      if (title) lines.push(title);
      const text = child.innerText || child.textContent || '';
      if (String(text).trim() && String(text).trim() !== String(title).trim()) lines.push(text);
    }
    return callerFromLines(lines);
  }

  // The closest name outside the Accept button. Walking only until a name
  // appears keeps the open chat and the chat list from winning.
  function callerNear(element) {
    let node = element;
    let previous = null;
    for (let depth = 0; node && depth < 12; depth += 1) {
      const caller = callerIn(node, previous);
      if (caller) return caller;
      previous = node;
      node = node.parentElement;
    }
    return '';
  }

  function initialRingState() {
    return { status: 'clear', caller: '' };
  }

  function keep(state, caller) {
    return {
      status: state && state.status ? state.status : 'clear',
      effects: [],
      caller: caller || (state && state.caller) || ''
    };
  }

  // One prompt per ring. Ignore hides the buttons and does not decline.
  // Join clicks Accept and starts voice. Listen clicks Accept only while the
  // call is still ringing. Cancel hangs up the WhatsApp call and tells
  // Magician to leave, including after Join or Listen.
  function reduceRing(state, event, caller) {
    const status = state && state.status ? state.status : 'clear';
    const nextCaller = callerFromLines([caller]) || (state && state.caller) || '';
    if (event === 'incoming') {
      if (status === 'joined' || status === 'listening' || status === 'ignored' || status === 'cancelled') {
        return keep(state, nextCaller);
      }
      if (status === 'prompting') {
        if ((state.caller || '') === nextCaller) return keep(state, nextCaller);
        return { status: 'prompting', effects: ['show', 'notify:incoming'], caller: nextCaller };
      }
      return { status: 'prompting', effects: ['show', 'notify:incoming'], caller: nextCaller };
    }
    if (event === 'ongoing') {
      if (status === 'joined' || status === 'listening' || status === 'ignored' || status === 'cancelled') {
        return keep(state, nextCaller);
      }
      if (status === 'ongoing' && (state.caller || '') === nextCaller) return keep(state, nextCaller);
      return { status: 'ongoing', effects: ['show', 'notify:ongoing'], caller: nextCaller };
    }
    if (event === 'gone') {
      if (status === 'joined' || status === 'listening') return keep(state, state && state.caller);
      if (status === 'clear') return { status: 'clear', effects: [], caller: '' };
      return { status: 'clear', effects: ['hide', 'notify:cleared'], caller: '' };
    }
    if (event === 'join' || event === 'approve') {
      if (status !== 'prompting') return keep(state, nextCaller);
      return { status: 'joined', effects: ['clickAccept', 'hide', 'notify:join'], caller: nextCaller };
    }
    if (event === 'listen') {
      if (status === 'prompting') {
        return { status: 'listening', effects: ['clickAccept', 'hide', 'notify:listen'], caller: nextCaller };
      }
      if (status === 'ongoing') {
        return { status: 'listening', effects: ['hide', 'notify:listen'], caller: nextCaller };
      }
      return keep(state, nextCaller);
    }
    if (event === 'ignore') {
      if (status !== 'prompting' && status !== 'ongoing') return keep(state, nextCaller);
      return { status: 'ignored', effects: ['hide', 'local:ignore'], caller: nextCaller };
    }
    if (event === 'cancel') {
      // Ignored, joined, and listening still have the call. Cancel hangs up
      // there too, and Magician drops its side when the notice arrives.
      if (
        status !== 'prompting'
        && status !== 'ongoing'
        && status !== 'ignored'
        && status !== 'joined'
        && status !== 'listening'
      ) {
        return keep(state, nextCaller);
      }
      return { status: 'cancelled', effects: ['clickHangup', 'hide', 'notify:cancel'], caller: nextCaller };
    }
    if (event === 'leave') {
      // Observe Leave and Stop listening hang up the call that is up.
      if (status !== 'joined' && status !== 'listening' && status !== 'ongoing' && status !== 'prompting') {
        return keep(state, nextCaller);
      }
      return { status: 'clear', effects: ['clickHangup', 'hide', 'notify:ended'], caller: nextCaller };
    }
    return keep(state, nextCaller);
  }

  function removeCallBanner(doc) {
    const existing = doc && typeof doc.getElementById === 'function' && doc.getElementById(BANNER_ID);
    if (existing) existing.remove();
  }

  let browserBar = true;

  function setBrowserBar(enabled) {
    browserBar = enabled !== false;
    return browserBar;
  }

  function browserBarEnabled() {
    return browserBar;
  }

  function bannerActions(kind) {
    return kind === 'ongoing'
      ? [['listen', 'Listen'], ['ignore', 'Ignore'], ['cancel', 'Cancel']]
      : [['join', 'Join as Magican'], ['listen', 'Listen'], ['ignore', 'Ignore'], ['cancel', 'Cancel']];
  }

  function renderCallBanner(doc, model) {
    if (!doc || !model || typeof doc.createElement !== 'function') return;
    if (!browserBar || model.browserBar === false) {
      removeCallBanner(doc);
      return;
    }
    removeCallBanner(doc);
    const host = doc.createElement('div');
    host.id = BANNER_ID;
    host.setAttribute('role', 'dialog');
    host.style.cssText = [
      'position:fixed',
      'top:16px',
      'left:50%',
      'transform:translateX(-50%)',
      'z-index:2147483647',
      'display:flex',
      'gap:10px',
      'align-items:center',
      'flex-wrap:wrap',
      'max-width:calc(100vw - 24px)',
      'padding:12px 14px',
      'background:#111',
      'color:#fff',
      'border-radius:12px',
      'font:14px/1.3 -apple-system,BlinkMacSystemFont,sans-serif',
      'box-shadow:0 8px 24px rgba(0,0,0,.35)'
    ].join(';');
    const who = model.caller ? model.caller : 'Someone';
    host.setAttribute(
      'aria-label',
      model.kind === 'ongoing'
        ? who + ' is on a WhatsApp call'
        : who + ' is calling on WhatsApp'
    );
    const label = doc.createElement('div');
    label.style.cssText = 'display:flex;flex-direction:column;min-width:10em;';
    const name = doc.createElement('strong');
    name.textContent = who;
    name.style.cssText = 'font-size:16px;font-weight:650;';
    const detail = doc.createElement('div');
    detail.textContent = model.kind === 'ongoing' ? 'On a WhatsApp call' : 'Calling on WhatsApp';
    detail.style.cssText = 'font-size:12px;opacity:.8;';
    label.appendChild(name);
    label.appendChild(detail);
    host.appendChild(label);
    const buttons = bannerActions(model.kind);
    for (const pair of buttons) {
      const button = doc.createElement('button');
      button.type = 'button';
      button.textContent = pair[1];
      button.style.cssText = pair[0] === 'join'
        ? 'background:#1f8f4e;color:#fff;border:0;border-radius:8px;padding:8px 12px;cursor:pointer;font:inherit;'
        : 'background:transparent;color:#fff;border:1px solid #888;border-radius:8px;padding:8px 12px;cursor:pointer;font:inherit;';
      button.addEventListener('click', () => {
        if (typeof model.onChoose === 'function') model.onChoose(pair[0]);
      });
      host.appendChild(button);
    }
    const parent = doc.documentElement || doc.body;
    if (parent) parent.appendChild(host);
  }

  const api = Object.freeze({
    isAcceptName,
    isDeclineName,
    controlName,
    collectCallControls,
    classifyCallControls,
    isEndCallName,
    callIsUp,
    noteCallSurface,
    callerFromLines,
    callerNear,
    initialRingState,
    reduceRing,
    removeCallBanner,
    renderCallBanner,
    setBrowserBar,
    browserBarEnabled,
    bannerActions
  });
  globalObject.MagicianWhatsAppCall = api;
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
})(typeof globalThis !== 'undefined' ? globalThis : this);
