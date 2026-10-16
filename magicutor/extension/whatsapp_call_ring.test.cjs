const test = require('node:test');
const assert = require('node:assert/strict');

test('a WhatsApp ring reports incoming and treats 404 as not stored', async () => {
  const ring = await import('./whatsapp_call_ring.mjs');
  const payload = ring.whatsAppRingPayload({
    decision: 'incoming',
    url: 'https://web.whatsapp.com/',
    title: '(161) WhatsApp'
  });
  assert.deepEqual(payload, {
    decision: 'incoming',
    url: 'https://web.whatsapp.com/',
    title: '(161) WhatsApp'
  });

  let seen = null;
  const result = await ring.reportWhatsAppCallRing(
    { decision: 'approved', url: 'https://evil.example/', title: 'x' },
    {
      apiBase: 'http://127.0.0.1:3002/api/magician/v2',
      sender: { origin: 'https://web.whatsapp.com', tab: { url: 'https://web.whatsapp.com/' } },
      fetchImpl: async (url, init) => {
        seen = { url, body: JSON.parse(init.body) };
        return { status: 404 };
      }
    }
  );
  assert.equal(seen.url, 'http://127.0.0.1:3002/api/magician/v2/meetings/whatsapp-ring');
  assert.equal(seen.body.decision, 'approved');
  assert.equal(seen.body.url, 'https://web.whatsapp.com/');
  assert.equal(result.ok, true);
  assert.equal(result.stored, false);
});

test('a missing bearer does not throw and other sites are ignored', async () => {
  const ring = await import('./whatsapp_call_ring.mjs');
  const missing = await ring.reportWhatsAppCallRing(
    { decision: 'incoming', url: 'https://web.whatsapp.com/' },
    {
      sender: { origin: 'https://web.whatsapp.com' },
      fetchImpl: async () => {
        throw new Error('No Magician bearer is installed. Add one in the extension popup or side panel.');
      }
    }
  );
  assert.equal(missing.ok, false);
  assert.match(missing.error, /bearer/i);

  const foreign = await ring.reportWhatsAppCallRing(
    { decision: 'incoming' },
    {
      sender: { origin: 'https://mail.google.com', tab: { url: 'https://mail.google.com/' } },
      fetchImpl: async () => {
        throw new Error('should not post');
      }
    }
  );
  assert.equal(foreign.error, 'ignored sender');
  assert.equal(ring.whatsAppRingPayload({ decision: 'decline' }), null);
  const join = ring.whatsAppRingPayload({
    decision: 'join',
    url: 'https://web.whatsapp.com/',
    caller: 'Ada'
  });
  assert.equal(join.decision, 'join');
  assert.equal(join.caller, 'Ada');
  assert.equal(ring.whatsAppRingPayload({ decision: 'ended', url: 'https://web.whatsapp.com/' }).decision, 'ended');
});

test('the web decision is applied once, and a 404 read stays quiet', async () => {
  const ring = await import('./whatsapp_call_ring.mjs');
  assert.deepEqual(ring.planRingApply(null, 'approved'), {
    applied: 'join',
    apply: 'join',
    keepPolling: true
  });
  assert.deepEqual(ring.planRingApply('approved', 'approved'), {
    applied: 'join',
    apply: null,
    keepPolling: false
  });
  assert.deepEqual(ring.planRingApply(null, 'listening'), {
    applied: 'listen',
    apply: 'listen',
    keepPolling: true
  });
  assert.deepEqual(ring.planRingApply(null, 'leaving'), {
    applied: 'leave',
    apply: 'leave',
    keepPolling: true
  });
  assert.deepEqual(ring.planRingApply('leave', 'leaving'), {
    applied: 'leave',
    apply: null,
    keepPolling: true
  });
  assert.deepEqual(ring.planRingApply(null, 'cancelled'), {
    applied: 'cancel',
    apply: 'cancel',
    keepPolling: false
  });
  assert.deepEqual(ring.planRingApply(null, 'ongoing'), {
    applied: null,
    apply: null,
    keepPolling: true
  });
  assert.deepEqual(ring.planRingApply(null, 'ignored'), {
    applied: 'ignored',
    apply: 'ignore',
    keepPolling: false
  });
  assert.deepEqual(ring.planRingApply('ignored', 'incoming'), {
    applied: null,
    apply: null,
    keepPolling: true
  });

  const missing = await ring.readWhatsAppCallRing({
    fetchImpl: async () => ({ status: 404 })
  });
  assert.equal(missing, null);
  const live = await ring.readWhatsAppCallRing({
    fetchImpl: async () => ({ status: 200, json: async () => ({ phase: 'incoming' }) })
  });
  assert.deepEqual(live, { phase: 'incoming', profile: '', surface: 'none' });
});

test('join announces a missing ring and speaks through the BlackHole engine', async () => {
  const ring = await import('./whatsapp_call_ring.mjs');
  const posts = [];
  const fetchImpl = async (_url, init) => {
    if (!init || init.method === 'GET') {
      return {
        status: 200,
        json: async () => ({
          phase: 'idle',
          profile: 'voice_realtime_default',
          surface: 'none'
        })
      };
    }
    posts.push(JSON.parse(init.body));
    return { status: 200, json: async () => ({ phase: 'approved' }) };
  };
  const sender = { origin: 'https://web.whatsapp.com', tab: { url: 'https://web.whatsapp.com/' } };
  const result = await ring.reportWhatsAppAttendance(
    { decision: 'join', url: 'https://web.whatsapp.com/', caller: 'Ada', title: '(161) WhatsApp' },
    { fetchImpl, sender }
  );
  assert.equal(result.stored, true);
  assert.equal(posts.length, 2);
  assert.equal(posts[0].decision, 'incoming');
  assert.equal(posts[0].caller, 'Ada');
  assert.equal(posts[0].profile, undefined);
  assert.equal(posts[1].decision, 'join');
  assert.equal(posts[1].profile, 'voice_realtime_openai_backend');
});

test('join keeps a chosen engine, and listen on an open call does not claim a ring', async () => {
  const ring = await import('./whatsapp_call_ring.mjs');
  const posts = [];
  const fetchImpl = async (_url, init) => {
    if (!init || init.method === 'GET') {
      return {
        status: 200,
        json: async () => ({
          phase: 'incoming',
          profile: 'voice_realtime_grok',
          surface: 'ringing'
        })
      };
    }
    posts.push(JSON.parse(init.body));
    return { status: 200, json: async () => ({ phase: 'approved' }) };
  };
  const sender = { origin: 'https://web.whatsapp.com' };
  await ring.reportWhatsAppAttendance(
    { decision: 'join', caller: 'Ada' },
    { fetchImpl, sender }
  );
  assert.deepEqual(posts.map((post) => post.decision), ['join']);
  assert.equal(posts[0].profile, 'voice_realtime_grok');

  posts.length = 0;
  const listenFetch = async (_url, init) => {
    if (!init || init.method === 'GET') {
      return {
        status: 200,
        json: async () => ({ phase: 'idle', profile: 'voice_realtime_gpt_live_1', surface: 'none' })
      };
    }
    posts.push(JSON.parse(init.body));
    return { status: 200, json: async () => ({ phase: 'listening' }) };
  };
  await ring.reportWhatsAppAttendance(
    { decision: 'listen', surface: 'ongoing', caller: 'Ada' },
    { fetchImpl: listenFetch, sender }
  );
  assert.deepEqual(posts.map((post) => post.decision), ['ongoing', 'listen']);
  assert.equal(posts[1].profile, 'voice_realtime_gpt_live_1');
});

test('the call banner is offered on other http tabs, not on WhatsApp itself', async () => {
  const ring = await import('./whatsapp_call_ring.mjs');
  assert.equal(ring.isWhatsAppPromptTab('http://localhost:5173/observe'), true);
  assert.equal(ring.isWhatsAppPromptTab('https://mail.google.com/mail/'), true);
  assert.equal(ring.isWhatsAppPromptTab('https://web.whatsapp.com/'), false);
  assert.equal(ring.isWhatsAppPromptTab('chrome://extensions/'), false);
  assert.equal(ring.isWhatsAppPromptTab(''), false);
});
