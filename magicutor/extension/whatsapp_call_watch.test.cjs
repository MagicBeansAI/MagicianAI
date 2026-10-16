const test = require('node:test');
const assert = require('node:assert/strict');

const watch = require('./whatsapp_call_watch.js');

function control(name, extra = {}) {
  return {
    name,
    element: {
      getAttribute(attr) {
        if (attr === 'aria-label') return name;
        return extra[attr] || null;
      },
      innerText: name,
      getBoundingClientRect: extra.rect || (() => ({ width: 40, height: 40 })),
      click: extra.click || (() => {}),
      closest: () => null
    }
  };
}

test('Accept and Decline together are an incoming call', () => {
  const view = watch.classifyCallControls([
    control('Accept'),
    control('Decline')
  ]);
  assert.equal(view.kind, 'incoming');
  assert.equal(view.accept.name, 'Accept');
  assert.equal(view.decline.name, 'Decline');
});

test('Answer voice call and Decline call are an incoming call', () => {
  const view = watch.classifyCallControls([
    control('Answer voice call'),
    control('Decline call')
  ]);
  assert.equal(view.kind, 'incoming');
});

test('a logged-in chat header is not a ring', () => {
  const names = ['Calls', 'Video call', 'Voice call', 'Stop automation', 'Search', 'Menu'];
  const view = watch.classifyCallControls(names.map((name) => control(name)));
  assert.equal(view.kind, 'idle');
});

test('Accept alone does not prompt', () => {
  assert.equal(watch.classifyCallControls([control('Accept')]).kind, 'idle');
});

test('hidden zero-size buttons are ignored', () => {
  const root = {
    querySelectorAll() {
      return [
        control('Accept', { rect: () => ({ width: 0, height: 0 }) }).element,
        control('Decline').element
      ];
    }
  };
  const controls = watch.collectCallControls(root);
  assert.deepEqual(controls.map((item) => item.name), ['Decline']);
});

test('join clicks Accept and ignore does not decline', () => {
  let state = watch.initialRingState();
  state = watch.reduceRing(state, 'incoming', 'Ada');
  assert.deepEqual(state.effects, ['show', 'notify:incoming']);
  assert.equal(state.caller, 'Ada');

  const ignored = watch.reduceRing(state, 'ignore');
  assert.equal(ignored.status, 'ignored');
  assert.deepEqual(ignored.effects, ['hide', 'local:ignore']);
  assert.equal(ignored.effects.includes('clickAccept'), false);
  assert.equal(ignored.effects.includes('clickHangup'), false);

  const declined = watch.reduceRing(state, 'cancel');
  assert.equal(declined.status, 'cancelled');
  assert.deepEqual(declined.effects, ['clickHangup', 'hide', 'notify:cancel']);
  const afterIgnore = watch.reduceRing(ignored, 'cancel');
  assert.deepEqual(afterIgnore.effects, ['clickHangup', 'hide', 'notify:cancel']);

  const joined = watch.reduceRing(state, 'join');
  assert.deepEqual(joined.effects, ['clickAccept', 'hide', 'notify:join']);
  const approved = watch.reduceRing(state, 'approve');
  assert.deepEqual(approved.effects, ['clickAccept', 'hide', 'notify:join']);
});

test('ignore holds until the ring leaves, then the next ring asks again', () => {
  let state = watch.reduceRing(watch.initialRingState(), 'incoming');
  state = watch.reduceRing(state, 'ignore');
  const again = watch.reduceRing(state, 'incoming');
  assert.equal(again.status, 'ignored');
  assert.deepEqual(again.effects, []);

  state = watch.reduceRing(state, 'gone');
  assert.equal(state.status, 'clear');
  assert.equal(state.effects.includes('notify:cleared'), true);

  const next = watch.reduceRing(state, 'incoming');
  assert.equal(next.status, 'prompting');
  assert.deepEqual(next.effects, ['show', 'notify:incoming']);
});

test('listen accepts a ring and does not click when the call is already up', () => {
  const prompting = watch.reduceRing(watch.initialRingState(), 'incoming', 'Ada');
  const listened = watch.reduceRing(prompting, 'listen');
  assert.equal(listened.status, 'listening');
  assert.deepEqual(listened.effects, ['clickAccept', 'hide', 'notify:listen']);

  const ongoing = watch.reduceRing(watch.initialRingState(), 'ongoing', 'Ada');
  assert.deepEqual(ongoing.effects, ['show', 'notify:ongoing']);
  const quiet = watch.reduceRing(ongoing, 'listen');
  assert.equal(quiet.status, 'listening');
  assert.deepEqual(quiet.effects, ['hide', 'notify:listen']);
  assert.equal(watch.reduceRing(ongoing, 'join').effects.includes('clickAccept'), false);

  const cancelled = watch.reduceRing(ongoing, 'cancel');
  assert.equal(cancelled.status, 'cancelled');
  assert.deepEqual(cancelled.effects, ['clickHangup', 'hide', 'notify:cancel']);
  assert.equal(cancelled.effects.includes('clickAccept'), false);

  const joined = watch.reduceRing(prompting, 'join');
  const cancelledJoin = watch.reduceRing(joined, 'cancel');
  assert.equal(cancelledJoin.status, 'cancelled');
  assert.deepEqual(cancelledJoin.effects, ['clickHangup', 'hide', 'notify:cancel']);
  const left = watch.reduceRing(joined, 'leave');
  assert.equal(left.status, 'clear');
  assert.deepEqual(left.effects, ['clickHangup', 'hide', 'notify:ended']);
  const listening = watch.reduceRing(ongoing, 'listen');
  assert.deepEqual(watch.reduceRing(listening, 'leave').effects, ['clickHangup', 'hide', 'notify:ended']);
});

test('a page title is not the caller', () => {
  assert.equal(watch.callerFromLines(['(161) WhatsApp', 'Incoming voice call']), '');
  assert.equal(watch.callerFromLines(['(161) WhatsApp', 'Ada']), 'Ada');
  assert.equal(watch.callerFromLines(['Chats', 'प्रिया']), 'प्रिया');
  assert.equal(watch.callerFromLines(['Voice call', '+91 98450 12345']), '+91 98450 12345');
});

test('the caller is the name on the call card, not the open chat', () => {
  function element(tag, attrs, children) {
    const node = {
      tag,
      attrs: attrs || {},
      children: children || [],
      parentElement: null,
      getAttribute(name) { return Object.prototype.hasOwnProperty.call(this.attrs, name) ? this.attrs[name] : null; },
      closest(selector) {
        const button = selector.includes('button');
        let current = this;
        while (current) {
          if (button && (current.tag === 'button' || current.attrs.role === 'button')) return current;
          if (selector.includes('cell-frame-title') && current.attrs['data-testid'] === 'cell-frame-title') return current;
          current = current.parentElement;
        }
        return null;
      },
      contains(other) {
        if (other === this) return true;
        return this.children.some((child) => child && child.contains && child.contains(other));
      },
      querySelectorAll() {
        const found = [];
        const walk = (item) => {
          for (const child of item.children) {
            if (!child || typeof child !== 'object') continue;
            found.push(child);
            walk(child);
          }
        };
        walk(this);
        return found;
      }
    };
    Object.defineProperty(node, 'innerText', {
      get() {
        return this.children.map((child) => typeof child === 'string' ? child : child.innerText).join(' ').trim();
      }
    });
    Object.defineProperty(node, 'textContent', { get() { return this.innerText; } });
    for (const child of node.children) {
      if (child && typeof child === 'object') child.parentElement = node;
    }
    return node;
  }

  const accept = element('button', { 'aria-label': 'Accept', role: 'button' }, ['Accept']);
  const decline = element('button', { 'aria-label': 'Decline', role: 'button' }, ['Decline']);
  const name = element('span', { dir: 'auto', title: '+91 98450 12345' }, ['+91 98450 12345']);
  const status = element('span', { dir: 'auto' }, ['Incoming voice call']);
  const card = element('div', {}, [name, status, accept, decline]);
  const openChat = element('span', { dir: 'auto', title: 'Wrong chat', 'data-testid': 'cell-frame-title' }, ['Wrong chat']);
  element('div', {}, [openChat, card]);
  assert.equal(watch.callerNear(accept), '+91 98450 12345');
});

test('the ring banner joins as Magican and hides when the browser bar is off', () => {
  const incoming = watch.bannerActions('incoming');
  assert.deepEqual(incoming[0], ['join', 'Join as Magican']);
  assert.equal(watch.bannerActions('ongoing').some((pair) => pair[0] === 'join'), false);
  assert.equal(watch.browserBarEnabled(), true);
  const removed = { gone: false, remove() { this.gone = true; } };
  const doc = {
    getElementById() { return removed; },
    createElement() { throw new Error('hidden bar must not paint'); }
  };
  watch.setBrowserBar(false);
  watch.renderCallBanner(doc, { kind: 'incoming', caller: 'Ada', onChoose() {} });
  assert.equal(removed.gone, true);
  watch.setBrowserBar(true);
  assert.equal(watch.browserBarEnabled(), true);
});

test('a ring that disappears before a choice reports cleared', () => {
  const prompting = watch.reduceRing(watch.initialRingState(), 'incoming');
  const gone = watch.reduceRing(prompting, 'gone');
  assert.equal(gone.status, 'clear');
  assert.deepEqual(gone.effects, ['hide', 'notify:cleared']);
});

test('End call is the live call, and hangup is seen-then-gone', () => {
  assert.equal(watch.callIsUp([control('Voice call'), control('End call')]), true);
  assert.equal(watch.callIsUp([control('Video call'), control('Calls')]), false);
  assert.equal(watch.classifyCallControls([control('End call')]).kind, 'idle');

  assert.deepEqual(watch.noteCallSurface({ armed: false, seenUp: false }, true), {
    armed: false,
    seenUp: false,
    ended: false
  });
  assert.deepEqual(watch.noteCallSurface({ armed: true, seenUp: false }, false), {
    armed: true,
    seenUp: false,
    ended: false
  });
  const up = watch.noteCallSurface({ armed: true, seenUp: false }, true);
  assert.equal(up.ended, false);
  assert.deepEqual(watch.noteCallSurface(up, false), {
    armed: false,
    seenUp: false,
    ended: true
  });
});
