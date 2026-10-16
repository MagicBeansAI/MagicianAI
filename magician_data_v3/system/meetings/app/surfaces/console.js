(function () {
  'use strict';

  // ---------------------------------------------------------------------
  // Budget
  //
  // The scripted-surface host allows 32 bridge messages and a 15-minute TTL
  // per session. A console that polled every 5 seconds would spend that in
  // under three minutes and then die silently mid-meeting, which is the worst
  // possible failure for a capture surface. So the budget is explicit here:
  //
  //   * live polling spends from a bounded pool and STOPS at the reserve,
  //     switching the header chip to "paused" instead of failing;
  //   * the reserve is kept for controls, so a STOP is never blocked by
  //     having watched the transcript for too long;
  //   * each live tick costs ONE message, alternating "launch the governed
  //     sync" and "read the projection it wrote", which also matches the
  //     asynchronous run's own latency.
  //
  // The remaining count is shown in the header. Nothing here is a substitute
  // for the streaming surface primitive; it is an honest ceiling.
  // ---------------------------------------------------------------------
  var BRIDGE_CHANNEL = 'magician-surface-bridge';
  var BRIDGE_BUDGET = 30;
  var CONTROL_RESERVE = 9;
  var PAGE_LIMIT = 20;
  var REQUEST_TIMEOUT_MS = 12000;
  var MAX_RENDER_TEXT = 8000;
  var LIVE_TICK_MS = 5000;
  var IDLE_TICK_MS = 10000;
  // Below this much remaining poll budget the interval stretches instead of
  // running flat out into exhaustion.
  var BUDGET_STRETCH_BELOW = 12;
  var MAX_TICK_MS = 60000;
  // Kept below the contract's own two-minute ceiling so ordinary host latency
  // between this click and destination admission cannot expire a real intent.
  var GESTURE_LIFETIME_MS = 90000;

  var state = {
    installationId: null,
    surfaceSessionId: null,
    sequence: 0,
    requestCounter: 0,
    syncCounter: 0,
    messagesSent: 0,
    pending: Object.create(null),
    activePanel: 'live',
    loaded: Object.create(null),
    pollTimer: null,
    pollPhase: 'sync',
    sessions: [],
    selectedSessionId: null,
    threads: [],
    selectedThreadId: null,
    transcript: [],
    hasOlder: false,
    takeaways: [],
    upcoming: [],
    hits: [],
    requests: [],
    receipts: []
  };

  function node(id) {
    return window.document.getElementById(id);
  }

  function element(tag, className, text) {
    var value = window.document.createElement(tag);
    if (className) {
      value.className = className;
    }
    if (text !== undefined && text !== null) {
      value.textContent = boundedText(text, MAX_RENDER_TEXT);
    }
    return value;
  }

  function boundedText(value, limit) {
    var text = String(value === undefined || value === null ? '' : value);
    return text.length > limit ? text.slice(0, limit - 1) + '…' : text;
  }

  function setStatus(message, kind) {
    var status = node('status');
    status.textContent = boundedText(message, 1000);
    status.dataset.state = kind || 'idle';
  }

  function remainingBudget() {
    return BRIDGE_BUDGET - state.messagesSent;
  }

  function pollBudgetLeft() {
    return Math.max(0, remainingBudget() - CONTROL_RESERVE);
  }

  function renderBudget() {
    var chip = node('budget-chip');
    var left = pollBudgetLeft();
    if (left > 0) {
      chip.textContent = 'live updates ' + left;
      chip.title = left + ' more automatic refreshes this session; ' + CONTROL_RESERVE +
        ' messages stay reserved for controls.';
    } else {
      chip.textContent = 'live updates paused';
      chip.title = 'This session spent its polling budget. Controls still work; ' +
        'reopen the console for live updates again.';
    }
  }

  function installationIdFromLocation() {
    var parts = window.location.pathname.split('/');
    for (var index = 0; index < parts.length - 1; index += 1) {
      if (parts[index] === 'installations' && parts[index + 1]) {
        try {
          return decodeURIComponent(parts[index + 1]);
        } catch (error) {
          return null;
        }
      }
    }
    if (window.location.protocol.indexOf('magapp-surface:') === 0 && window.location.hostname) {
      return window.location.hostname;
    }
    return null;
  }

  // The bridge session this frame is speaking on. Derived from the host-minted
  // path segment when there is one so the gesture names the session the host
  // itself knows; otherwise a per-load id, which still distinguishes two
  // frames of the same installation.
  function surfaceSessionIdFromLocation() {
    var parts = window.location.pathname.split('/');
    for (var index = 0; index < parts.length - 1; index += 1) {
      // The host serves this document from `…/assets/{session}/{digest}/{tail}`,
      // so the segment after `assets` is the bridge session the host itself
      // minted. It is still frame-read, not host-attested: nothing downstream
      // treats it as proof of a live session, and the destination does not
      // resolve it. It identifies WHICH frame acted, not THAT a person did.
      if (parts[index] === 'assets' && parts[index + 1]) {
        try {
          return decodeURIComponent(parts[index + 1]);
        } catch (error) {
          break;
        }
      }
    }
    return 'surface-' + randomToken();
  }

  function randomToken() {
    var crypto = window.crypto;
    if (crypto && typeof crypto.randomUUID === 'function') {
      return crypto.randomUUID();
    }
    if (crypto && typeof crypto.getRandomValues === 'function') {
      var buffer = new Uint8Array(16);
      crypto.getRandomValues(buffer);
      var out = '';
      for (var index = 0; index < buffer.length; index += 1) {
        out += (buffer[index] + 256).toString(16).slice(1);
      }
      return out;
    }
    // Last resort. A predictable token is not an authority here — the host
    // re-derives installation and scope, and the destination re-checks the
    // gesture's expiry against its own clock.
    return 'g' + String(Date.now()) + String(state.requestCounter);
  }

  function callBridge(method, payload, viewOrAction) {
    return new Promise(function (resolve, reject) {
      if (state.messagesSent >= BRIDGE_BUDGET) {
        reject(new Error('bridge message budget exhausted; reopen the console for a fresh session'));
        return;
      }
      state.requestCounter += 1;
      state.sequence += 1;
      state.messagesSent += 1;
      renderBudget();
      var requestId = 'meetings-' + state.requestCounter;
      var timer = window.setTimeout(function () {
        var waiting = state.pending[requestId];
        if (waiting) {
          delete state.pending[requestId];
          waiting.reject(new Error('bridge request timed out closed'));
        }
      }, REQUEST_TIMEOUT_MS);
      state.pending[requestId] = { resolve: resolve, reject: reject, timer: timer };
      var request = {
        method: method,
        sequence: state.sequence,
        request_id: requestId,
        payload: payload
      };
      if (viewOrAction) {
        request.view_or_action = viewOrAction;
      }
      var nativeHandler = window.webkit && window.webkit.messageHandlers
        ? window.webkit.messageHandlers.magicianSurfaceBridge
        : null;
      if (nativeHandler && typeof nativeHandler.postMessage === 'function') {
        nativeHandler.postMessage(request);
      } else {
        window.parent.postMessage({ channel: BRIDGE_CHANNEL, request: request }, '*');
      }
    });
  }

  function launchAction(actionName, input, idempotencyKey) {
    return callBridge(
      'launch_action',
      { idempotency_key: idempotencyKey, input: input },
      actionName
    );
  }

  function settleBridgeReply(data) {
    if (!data || typeof data.request_id !== 'string') {
      return;
    }
    var waiting = state.pending[data.request_id];
    if (!waiting) {
      return;
    }
    delete state.pending[data.request_id];
    window.clearTimeout(waiting.timer);
    if (typeof data.error === 'string' && data.error) {
      waiting.reject(new Error(data.error));
    } else {
      waiting.resolve(data.result);
    }
  }

  window.__magicianSurfaceBridgeReply = function (data) {
    settleBridgeReply(data);
  };

  window.addEventListener('message', function (event) {
    if (event.source !== window.parent) {
      return;
    }
    var data = event.data;
    if (!data || data.channel !== BRIDGE_CHANNEL || data.kind !== 'reply') {
      return;
    }
    settleBridgeReply(data);
  });

  function queryEntity(entityName, fields, order, predicate, limit) {
    var payload = {
      protocol_version: '1',
      source_installation_id: state.installationId,
      entity: entityName,
      select: fields,
      limit: Number.isSafeInteger(limit) && limit > 0 && limit <= PAGE_LIMIT ? limit : PAGE_LIMIT,
      purpose: 'meetings_console'
    };
    if (order) {
      payload.order = order;
    }
    if (predicate) {
      payload.predicate = predicate;
    }
    return callBridge('query_data', payload).then(function (page) {
      var rows = page && page.envelope && Array.isArray(page.envelope.value)
        ? page.envelope.value
        : [];
      return rows.map(function (row) {
        return row && row.fields && typeof row.fields === 'object' ? row.fields : {};
      });
    });
  }

  function emptyInto(container, message) {
    container.textContent = '';
    container.appendChild(element('p', 'empty', message));
  }

  function clear(container) {
    container.textContent = '';
  }

  // -------------------------------------------------------------------
  // Rendering
  // -------------------------------------------------------------------

  function renderSessions() {
    var list = node('sessions-list');
    node('sessions-count').textContent = String(state.sessions.length);
    var live = state.sessions.filter(function (row) { return row.live === true; });
    var chip = node('capture-chip');
    chip.dataset.live = live.length > 0 ? 'true' : 'false';
    chip.textContent = live.length > 0
      ? (live.length === 1 ? 'Capturing' : live.length + ' captures live')
      : 'No capture';

    if (state.sessions.length === 0) {
      emptyInto(list, 'No capture session is registered right now.');
      renderControlAvailability();
      return;
    }
    clear(list);
    state.sessions.forEach(function (row) {
      var button = element('button', 'row');
      button.type = 'button';
      button.setAttribute('aria-current', row.session_id === state.selectedSessionId ? 'true' : 'false');
      var head = element('strong');
      var badge = element('span', row.live ? 'badge live' : 'badge', row.live ? 'live' : 'ended');
      head.appendChild(badge);
      // A row from another scope carries no thread, title or url by design.
      // Naming it by session id would read as one of this scope's meetings.
      head.appendChild(window.document.createTextNode(
        row.in_scope === false
          ? 'Capture in another workspace'
          : boundedText(row.title || row.thread_id || row.session_id, 160)
      ));
      button.appendChild(head);
      var facts = [row.mode, row.status];
      if (row.paused) {
        facts.push('paused');
      }
      if (row.capture_mic === true) {
        facts.push('mic on');
      }
      facts.push(describeAge(row));
      button.appendChild(element('small', null, facts.join(' · ')));
      button.addEventListener('click', function () {
        state.selectedSessionId = row.session_id;
        if (row.thread_id) {
          state.selectedThreadId = row.thread_id;
        }
        renderSessions();
      });
      list.appendChild(button);
    });
    renderControlAvailability();
  }

  function describeAge(row) {
    var started = Number(row.started_seconds_ago);
    var running = Number.isFinite(started) ? formatDuration(started) : 'unknown duration';
    if (row.live === true) {
      return 'running ' + running;
    }
    var ended = Number(row.ended_seconds_ago);
    var retained = Number(row.retained_for_seconds);
    if (Number.isFinite(ended) && Number.isFinite(retained)) {
      var left = Math.max(0, retained - ended);
      return 'ended ' + formatDuration(ended) + ' ago · listed for another ' + formatDuration(left);
    }
    return 'ran ' + running;
  }

  function formatDuration(seconds) {
    var total = Math.max(0, Math.floor(seconds));
    if (total < 60) {
      return total + 's';
    }
    var minutes = Math.floor(total / 60);
    if (minutes < 60) {
      return minutes + 'm';
    }
    return Math.floor(minutes / 60) + 'h ' + (minutes % 60) + 'm';
  }

  function selectedSession() {
    for (var index = 0; index < state.sessions.length; index += 1) {
      if (state.sessions[index].session_id === state.selectedSessionId) {
        return state.sessions[index];
      }
    }
    return null;
  }

  function renderControlAvailability() {
    var session = selectedSession();
    // The destination refuses a control on a session this scope does not own,
    // so the button is disabled rather than offering an act that must fail.
    var live = session !== null && session.live === true && session.in_scope !== false;
    var haveBudget = remainingBudget() > 0;
    node('do-pause').disabled = !live || session.paused === true || !haveBudget;
    node('do-resume').disabled = !live || session.paused !== true || !haveBudget;
    node('do-stop').disabled = !live || !haveBudget;
    var anyLive = state.sessions.some(function (row) { return row.live === true; });
    node('do-listen').disabled = anyLive || !haveBudget;
    node('do-join').disabled = anyLive || !haveBudget;
  }

  function renderThreads() {
    var list = node('threads-list');
    node('threads-count').textContent = String(state.threads.length);
    if (state.threads.length === 0) {
      emptyInto(list, 'No meeting threads yet.');
      return;
    }
    clear(list);
    state.threads.forEach(function (row) {
      var button = element('button', 'row');
      button.type = 'button';
      button.setAttribute('aria-current', row.thread_id === state.selectedThreadId ? 'true' : 'false');
      button.appendChild(element('strong', null, row.title || row.thread_id));
      button.appendChild(element('small', null, [row.thread_id, row.status].join(' · ')));
      button.addEventListener('click', function () {
        state.selectedThreadId = row.thread_id;
        state.transcript = [];
        state.hasOlder = false;
        renderThreads();
        refreshTranscript(null, null);
      });
      list.appendChild(button);
    });
  }

  function renderTranscript() {
    var list = node('transcript-lines');
    node('transcript-count').textContent = String(state.transcript.length);
    node('load-older').disabled = state.hasOlder !== true || remainingBudget() <= 0;
    if (!state.selectedThreadId) {
      emptyInto(list, 'Select a meeting thread to read its transcript.');
      return;
    }
    if (state.transcript.length === 0) {
      emptyInto(list, 'No transcript lines for this thread yet.');
      return;
    }
    clear(list);
    state.transcript.forEach(function (row) {
      var line = element('div', 'line');
      var when = element('span', 'when', formatStamp(row.line_at));
      line.appendChild(when);
      if (row.speaker) {
        line.appendChild(element('span', 'who', row.speaker));
      } else if (row.transcript !== true) {
        line.appendChild(element('span', 'who', 'note'));
      }
      line.appendChild(element('p', null, row.line_text));
      list.appendChild(line);
    });
  }

  // Timestamp fields cross the wire as RFC3339 strings — the only form the
  // package's `timestamp` contract accepts. Parsing them as epoch millis would
  // silently blank every stamp in the console.
  function formatStamp(value) {
    if (typeof value !== 'string' || value.length === 0) {
      return '';
    }
    var date = new Date(value);
    if (Number.isNaN(date.getTime())) {
      return '';
    }
    return date.toLocaleTimeString();
  }

  function renderTakeaways() {
    var list = node('takeaways-list');
    node('takeaways-count').textContent = String(state.takeaways.length);
    if (state.takeaways.length === 0) {
      emptyInto(list, 'No meeting takeaways retained.');
      return;
    }
    clear(list);
    state.takeaways.forEach(function (row) {
      var card = element('div', 'row');
      card.appendChild(element('strong', null, row.title || row.thread_id || row.takeaway_key));
      card.appendChild(element('small', null, [row.meeting_date, row.updated_at].filter(Boolean).join(' · ')));
      if (row.summary) {
        card.appendChild(element('p', null, row.summary));
      }
      if (row.decisions) {
        card.appendChild(element('small', null, 'Decisions'));
        card.appendChild(element('p', null, row.decisions));
      }
      if (row.action_items) {
        card.appendChild(element('small', null, 'Action items'));
        card.appendChild(element('p', null, row.action_items));
      }
      list.appendChild(card);
    });
  }

  function renderUpcoming() {
    var body = node('upcoming-body');
    node('upcoming-count').textContent = String(state.upcoming.length);
    clear(body);
    if (state.upcoming.length === 0) {
      var empty = element('tr');
      var cell = element('td', null, 'No upcoming meetings in the next twelve hours.');
      cell.colSpan = 5;
      empty.appendChild(cell);
      body.appendChild(empty);
      return;
    }
    state.upcoming.forEach(function (row) {
      var line = element('tr');
      line.appendChild(element('td', null, row.title));
      line.appendChild(element('td', null, row.starts_at));
      line.appendChild(element('td', null, row.ends_at || '—'));
      line.appendChild(element('td', null, row.live_now === true ? 'now' : ''));
      line.appendChild(element('td', null, row.account));
      if (row.meet_url) {
        line.style.cursor = 'pointer';
        line.title = 'Prefill the controls with this meeting';
        line.addEventListener('click', function () {
          node('start-url').value = String(row.meet_url);
          node('start-title').value = String(row.title || '');
          selectPanel('live');
          setStatus('Controls prefilled from the calendar. Listen or Join still needs your click.', 'idle');
        });
      }
      body.appendChild(line);
    });
  }

  function renderHits() {
    var list = node('search-list');
    node('search-count').textContent = String(state.hits.length);
    if (state.hits.length === 0) {
      emptyInto(list, 'No keyword hits. This search is an exact substring match, not a semantic one.');
      return;
    }
    clear(list);
    state.hits.forEach(function (row) {
      var button = element('button', 'row');
      button.type = 'button';
      var head = element('strong');
      head.appendChild(element('span', 'badge', row.kind));
      head.appendChild(window.document.createTextNode(boundedText(row.thread_id, 160)));
      button.appendChild(head);
      button.appendChild(element('small', null, [row.speaker, formatStamp(row.hit_at)].filter(Boolean).join(' · ')));
      button.appendChild(element('p', null, row.excerpt));
      button.addEventListener('click', function () {
        state.selectedThreadId = row.thread_id;
        state.transcript = [];
        state.hasOlder = false;
        selectPanel('transcript');
        refreshTranscript(null, null);
      });
      list.appendChild(button);
    });
  }

  function renderLedger() {
    var requests = node('requests-list');
    if (state.requests.length === 0) {
      emptyInto(requests, 'No control requests recorded from this console.');
    } else {
      clear(requests);
      state.requests.forEach(function (row) {
        var item = element('div', 'row');
        var head = element('strong');
        head.appendChild(element('span', 'badge', row.verb));
        head.appendChild(window.document.createTextNode(boundedText(row.target_ref || row.target_kind, 200)));
        item.appendChild(head);
        item.appendChild(element('small', null, [row.request_id, row.apply_state, row.requested_at].join(' · ')));
        requests.appendChild(item);
      });
    }
    var receipts = node('receipts-list');
    if (state.receipts.length === 0) {
      emptyInto(receipts, 'No signed destination receipt yet. A recorded request is not an application receipt.');
      return;
    }
    clear(receipts);
    state.receipts.forEach(function (row) {
      var item = element('div', 'row');
      var head = element('strong');
      head.appendChild(element('span', 'badge', row.outcome));
      head.appendChild(window.document.createTextNode(boundedText(row.verb, 60)));
      item.appendChild(head);
      item.appendChild(element('small', null, [row.session_id, row.thread_id, row.applied_at].filter(Boolean).join(' · ')));
      receipts.appendChild(item);
    });
  }

  // -------------------------------------------------------------------
  // Reads
  // -------------------------------------------------------------------

  function readSessions() {
    return queryEntity(
      'capture_session',
      ['session_id', 'mode', 'status', 'live', 'in_scope', 'paused', 'capture_mic',
        'thread_id', 'title', 'started_seconds_ago', 'ended_seconds_ago',
        'retained_for_seconds'],
      [{ field: 'started_seconds_ago', direction: 'descending' }],
      null,
      PAGE_LIMIT
    ).then(function (rows) {
      state.sessions = rows;
      if (state.selectedSessionId && !selectedSession()) {
        state.selectedSessionId = null;
      }
      if (!state.selectedSessionId) {
        var live = rows.filter(function (row) {
          return row.live === true && row.in_scope !== false;
        });
        state.selectedSessionId = live.length > 0 ? live[0].session_id : null;
      }
      renderSessions();
    });
  }

  function readThreads() {
    return queryEntity(
      'meeting_thread',
      ['thread_id', 'session_id', 'title', 'status', 'created_at', 'updated_at'],
      [{ field: 'updated_at', direction: 'descending' }],
      null,
      PAGE_LIMIT
    ).then(function (rows) {
      state.threads = rows;
      renderThreads();
    });
  }

  // Read the NEWEST page and reverse it for display.
  //
  // Reading ascending with no cursor returns the OLDEST rows in the projection,
  // so after one "load older" the console would render the oldest twenty
  // forever and lines arriving during a live meeting would never appear. The
  // read is therefore descending; `beforeStamp` walks backwards and PREPENDS,
  // so the rendered transcript grows upward from the newest page instead of
  // being replaced by an older one.
  function readTranscript(beforeStamp) {
    if (!state.selectedThreadId) {
      renderTranscript();
      return Promise.resolve();
    }
    var nodes = [
      { kind: 'compare', field: 'thread_id', operator: 'equal', value: state.selectedThreadId }
    ];
    var predicate;
    if (beforeStamp) {
      // `less_than_or_equal`, not `less_than`: same-second turns are ordinary in
      // a transcript, and a strict bound would drop every line sharing the
      // boundary stamp with no indication. The overlap it admits is removed by
      // de-duplicating on `message_id` below.
      nodes.push({
        kind: 'compare',
        field: 'line_at',
        operator: 'less_than_or_equal',
        value: beforeStamp
      });
      nodes.push({ kind: 'all', children: [0, 1] });
      predicate = { root: 2, nodes: nodes };
    } else {
      predicate = { root: 0, nodes: nodes };
    }
    return queryEntity(
      'transcript_line',
      ['message_id', 'thread_id', 'session_id', 'speaker', 'line_text', 'transcript', 'line_at'],
      [{ field: 'line_at', direction: 'descending' }],
      predicate,
      PAGE_LIMIT
    ).then(function (rows) {
      var ascending = rows.slice().reverse();
      if (beforeStamp) {
        var known = Object.create(null);
        state.transcript.forEach(function (line) {
          known[line.message_id] = true;
        });
        var fresh = ascending.filter(function (line) {
          return known[line.message_id] !== true;
        });
        state.transcript = fresh.concat(state.transcript);
      } else {
        state.transcript = ascending;
      }
      // A full page implies older lines may exist; a short page is the start of
      // the thread as far as the projection holds it. `hasOlder` is the enable
      // state for the button — the page boundary itself is read from the oldest
      // rendered line at click time, so it can never drift from what is shown.
      state.hasOlder = rows.length >= PAGE_LIMIT && state.transcript.length > 0;
      renderTranscript();
    });
  }

  function readTakeaways() {
    return queryEntity(
      'meeting_takeaway',
      ['takeaway_key', 'thread_id', 'title', 'meeting_date', 'summary', 'decisions', 'action_items', 'updated_at'],
      [{ field: 'synced_at', direction: 'descending' }],
      null,
      PAGE_LIMIT
    ).then(function (rows) {
      state.takeaways = rows;
      renderTakeaways();
    });
  }

  function readUpcoming() {
    return queryEntity(
      'upcoming_meeting',
      ['event_id', 'title', 'starts_at', 'ends_at', 'meet_url', 'live_now', 'account'],
      [{ field: 'starts_at', direction: 'ascending' }],
      null,
      PAGE_LIMIT
    ).then(function (rows) {
      state.upcoming = rows;
      renderUpcoming();
    });
  }

  function readHits() {
    return queryEntity(
      'meeting_search_hit',
      ['hit_id', 'kind', 'thread_id', 'session_id', 'message_id', 'speaker', 'excerpt', 'hit_at'],
      [{ field: 'synced_at', direction: 'descending' }],
      null,
      PAGE_LIMIT
    ).then(function (rows) {
      state.hits = rows;
      renderHits();
    });
  }

  function readLedger() {
    return queryEntity(
      'control_request',
      ['request_id', 'verb', 'target_kind', 'target_ref', 'gesture_id', 'apply_state', 'requested_at'],
      [{ field: 'requested_at', direction: 'descending' }],
      null,
      PAGE_LIMIT
    ).then(function (rows) {
      state.requests = rows;
      return queryEntity(
        'control_receipt',
        ['receipt_id', 'decision_id', 'verb', 'outcome', 'session_id', 'thread_id', 'applied_at'],
        [{ field: 'applied_at', direction: 'descending' }],
        null,
        PAGE_LIMIT
      );
    }).then(function (rows) {
      state.receipts = rows;
      renderLedger();
    });
  }

  // -------------------------------------------------------------------
  // Governed refreshes
  // -------------------------------------------------------------------

  // An idempotency key is an AppReference on the host side: ASCII alphanumeric
  // first character, then only [A-Za-z0-9_-.:/@#], at most 192 bytes. Free text
  // — a search phrase with a space, an apostrophe or any non-ASCII character —
  // is refused before the action ever runs, so every key is built through this.
  function referenceToken(value, limit) {
    var safe = String(value === undefined || value === null ? '' : value)
      .replace(/[^A-Za-z0-9_.\-]/g, '-')
      .replace(/^[^A-Za-z0-9]+/, '');
    if (safe.length === 0) {
      safe = 'x';
    }
    return safe.slice(0, limit);
  }

  // A polling sync needs a NEW key each tick. A minute-granular key meant most
  // sync phases recovered a completed binding and ran nothing, so the console
  // spent its whole message budget to deliver about two real refreshes — and
  // pressing Refresh twice inside a minute reported success while doing nothing.
  function syncKey(actionName) {
    state.syncCounter += 1;
    return referenceToken(actionName, 48) + ':' + Math.floor(Date.now() / 1000) +
      ':' + state.syncCounter;
  }

  function refreshTranscript(beforeMessageId, beforeStamp) {
    if (!state.selectedThreadId) {
      return Promise.resolve();
    }
    var input = { thread_id: state.selectedThreadId, limit: PAGE_LIMIT };
    if (beforeMessageId) {
      input.before_message_id = beforeMessageId;
    }
    setStatus('Reading the transcript through the governed host action…', 'busy');
    var key = referenceToken(
      'read_transcript:' + state.selectedThreadId + ':' + (beforeMessageId || 'newest'),
      160
    );
    return launchAction('read_transcript', input, key)
      .then(function () {
        return readTranscript(beforeStamp);
      })
      .then(function () {
        setStatus('Transcript page loaded.', 'ok');
      })
      .catch(reportError);
  }

  // `cost` is how many bridge messages this panel's READ spends. The budget is
  // checked against the real cost before a tick begins, so a two-message tick
  // can never dip into the reserve the controls depend on.
  function panelSyncAction(panel) {
    if (panel === 'live') {
      return { action: 'sync_sessions', input: {}, read: readSessions, cost: 1 };
    }
    if (panel === 'transcript') {
      return { action: 'sync_threads', input: { limit: PAGE_LIMIT }, read: readThreads, cost: 1 };
    }
    if (panel === 'takeaways') {
      return {
        action: 'sync_takeaways',
        input: { limit: PAGE_LIMIT },
        read: readTakeaways,
        cost: 1
      };
    }
    if (panel === 'upcoming') {
      return { action: 'sync_upcoming', input: {}, read: readUpcoming, cost: 1 };
    }
    if (panel === 'activity') {
      // Two entity reads, so two messages.
      return { action: null, input: null, read: readLedger, cost: 2 };
    }
    return { action: null, input: null, read: readHits, cost: 1 };
  }

  function reportError(error) {
    setStatus(String(error && error.message ? error.message : error), 'error');
    renderControlAvailability();
  }

  // -------------------------------------------------------------------
  // Polling
  // -------------------------------------------------------------------

  // What the NEXT tick on this panel will spend: one message for a governed
  // sync, or the panel read's own cost.
  function nextTickCost() {
    var plan = panelSyncAction(state.activePanel);
    if (plan.action && state.pollPhase === 'sync') {
      return 1;
    }
    return plan.cost;
  }

  function scheduleTick() {
    if (state.pollTimer !== null) {
      window.clearTimeout(state.pollTimer);
      state.pollTimer = null;
    }
    if (window.document.hidden) {
      return;
    }
    if (pollBudgetLeft() < nextTickCost()) {
      renderBudget();
      return;
    }
    state.pollTimer = window.setTimeout(runTick, tickInterval());
  }

  // Spend fast while there is budget to spend, then stretch rather than stop.
  // A flat 5s tick burns the whole pool in about ninety seconds and then leaves
  // a capture console dark for the rest of a fifteen-minute session; stretching
  // keeps it refreshing, more slowly, for the whole session.
  function tickInterval() {
    var anyLive = state.sessions.some(function (row) { return row.live === true; });
    var base = anyLive && state.activePanel === 'live' ? LIVE_TICK_MS : IDLE_TICK_MS;
    var left = pollBudgetLeft();
    if (left <= BUDGET_STRETCH_BELOW) {
      var stretch = Math.max(1, Math.ceil(BUDGET_STRETCH_BELOW / Math.max(1, left)));
      return Math.min(base * stretch, MAX_TICK_MS);
    }
    return base;
  }

  function runTick() {
    state.pollTimer = null;
    if (window.document.hidden || pollBudgetLeft() < nextTickCost()) {
      renderBudget();
      return;
    }
    var plan = panelSyncAction(state.activePanel);
    // Alternating: launch the governed sync, then read the projection it wrote.
    // The governed run is asynchronous, so reading on the FOLLOWING tick is
    // also what gives it time to project.
    var step;
    if (plan.action && state.pollPhase === 'sync') {
      step = launchAction(plan.action, plan.input, syncKey(plan.action));
      state.pollPhase = 'read';
    } else {
      step = plan.read();
      state.pollPhase = 'sync';
    }
    step.catch(reportError).then(scheduleTick);
  }

  // -------------------------------------------------------------------
  // Controls
  // -------------------------------------------------------------------

  function newGesture() {
    var observed = Date.now();
    return {
      gesture_id: 'gesture-' + randomToken(),
      surface_session_id: state.surfaceSessionId,
      gesture_observed_at_ms: observed,
      gesture_expires_at_ms: observed + GESTURE_LIFETIME_MS
    };
  }

  function trimmedValue(id) {
    var raw = node(id).value;
    var text = typeof raw === 'string' ? raw.trim() : '';
    return text.length > 0 ? text : null;
  }

  function startCapture(verb) {
    var url = trimmedValue('start-url');
    if (verb === 'join' && !url) {
      setStatus('An attendee join needs the meeting link.', 'error');
      return;
    }
    var gesture = newGesture();
    var input = {
      request_id: referenceToken(verb + ':' + gesture.gesture_id, 160),
      gesture_id: gesture.gesture_id,
      surface_session_id: gesture.surface_session_id,
      gesture_observed_at_ms: gesture.gesture_observed_at_ms,
      gesture_expires_at_ms: gesture.gesture_expires_at_ms
    };
    if (url) {
      input.url = url;
    }
    var title = trimmedValue('start-title');
    if (title) {
      input.title = title;
    }
    var date = trimmedValue('start-date');
    if (date) {
      input.meeting_date = date;
    }
    if (verb === 'listen') {
      input.capture_mic = node('start-mic').checked === true;
    }
    setStatus('Recording a governed ' + verb + ' request. It starts nothing until the owner signs it.', 'busy');
    launchAction(verb, input, input.request_id)
      .then(function () {
        setStatus(
          'The ' + verb + ' request is recorded with your gesture. It expires in ' +
            Math.round(GESTURE_LIFETIME_MS / 1000) + 's — sign it on the paired desktop before then.',
          'ok'
        );
        renderControlAvailability();
      })
      .catch(reportError);
  }

  function controlSession(verb) {
    var session = selectedSession();
    if (!session) {
      setStatus('Select a live capture session first.', 'error');
      return;
    }
    // `stop` is idempotent on a session, so a deterministic key is right: two
    // clicks are one intent and the second recovers the first run rather than
    // writing a second immutable ledger row.
    //
    // `pause` and `resume` are NOT. The host derives its task id from the
    // idempotency key, so a fixed key would make the second pause of a session
    // recover the first binding, write no `control_request`, and leave the
    // owner with nothing to sign — pause/resume cycling would be impossible.
    var requestId = verb === 'stop'
      ? referenceToken(verb + ':' + session.session_id, 160)
      : referenceToken(verb + ':' + session.session_id + ':' + randomToken(), 160);
    setStatus('Recording a governed ' + verb + ' request for ' + session.session_id + '…', 'busy');
    launchAction(verb, { request_id: requestId, session_id: session.session_id }, requestId)
      .then(function () {
        setStatus(
          'The ' + verb + ' request is recorded. The first-party meetings page and API remain ' +
            'authoritative and can act on this session without this frame.',
          'ok'
        );
      })
      .catch(reportError);
  }

  // -------------------------------------------------------------------
  // Panels
  // -------------------------------------------------------------------

  function selectPanel(panel) {
    state.activePanel = panel;
    state.pollPhase = 'sync';
    var tabs = window.document.querySelectorAll('.tab');
    for (var index = 0; index < tabs.length; index += 1) {
      var tab = tabs[index];
      var selected = tab.dataset.panel === panel;
      tab.setAttribute('aria-selected', selected ? 'true' : 'false');
      var section = node('panel-' + tab.dataset.panel);
      if (section) {
        section.hidden = !selected;
      }
    }
    if (!state.loaded[panel]) {
      var plan = panelSyncAction(panel);
      var first = plan.action
        ? launchAction(plan.action, plan.input, syncKey(plan.action)).then(plan.read)
        : plan.read();
      // Marked loaded only on SUCCESS: marking it before the load resolves
      // would make a panel whose first read was refused never retry, even when
      // the operator re-enters the tab.
      first
        .then(function () {
          state.loaded[panel] = true;
        })
        .catch(reportError)
        .then(scheduleTick);
      return;
    }
    scheduleTick();
  }

  function bindEvents() {
    var tabs = window.document.querySelectorAll('.tab');
    for (var index = 0; index < tabs.length; index += 1) {
      (function (tab) {
        tab.addEventListener('click', function () {
          selectPanel(tab.dataset.panel);
        });
      })(tabs[index]);
    }
    node('refresh').addEventListener('click', function () {
      var plan = panelSyncAction(state.activePanel);
      var step = plan.action
        ? launchAction(plan.action, plan.input, syncKey(plan.action)).then(plan.read)
        : plan.read();
      setStatus('Refreshing…', 'busy');
      step.then(function () {
        setStatus('Refreshed.', 'ok');
      }).catch(reportError).then(scheduleTick);
    });
    node('do-listen').addEventListener('click', function () { startCapture('listen'); });
    node('do-join').addEventListener('click', function () { startCapture('join'); });
    node('do-pause').addEventListener('click', function () { controlSession('pause'); });
    node('do-resume').addEventListener('click', function () { controlSession('resume'); });
    node('do-stop').addEventListener('click', function () { controlSession('stop'); });
    node('load-older').addEventListener('click', function () {
      var oldest = state.transcript.length > 0 ? state.transcript[0] : null;
      if (!oldest) {
        return;
      }
      refreshTranscript(oldest.message_id, oldest.line_at).then(scheduleTick);
    });
    node('start-form').addEventListener('submit', function (event) {
      event.preventDefault();
    });
    node('search-form').addEventListener('submit', function (event) {
      event.preventDefault();
      var text = trimmedValue('search-text');
      if (!text || text.length < 2) {
        setStatus('Enter at least two characters. This is a substring match.', 'error');
        return;
      }
      setStatus('Searching meeting memory…', 'busy');
      // A new key per submission: a search must re-run, not replay a page it
      // already served. Bounded and sanitized so the host accepts it.
      var searchKey = 'search:' + Math.floor(Date.now() / 1000) + ':' +
        referenceToken(text, 64);
      launchAction('search_meetings', { text: text, limit: PAGE_LIMIT }, searchKey)
        .then(readHits)
        .then(function () {
          setStatus('Keyword search complete.', 'ok');
        })
        .catch(reportError)
        .then(scheduleTick);
    });
    window.document.addEventListener('visibilitychange', function () {
      if (window.document.hidden) {
        if (state.pollTimer !== null) {
          window.clearTimeout(state.pollTimer);
          state.pollTimer = null;
        }
        return;
      }
      scheduleTick();
    });
  }

  // Before the app is revealed, `#status` is inside a hidden subtree — writing a
  // failure there would leave the operator looking at "Opening the reviewed
  // meetings console…" with the actual reason invisible. Boot failures append
  // to the notice that IS on screen.
  function bootFailure(message) {
    var notice = node('boot-notice');
    if (!notice) {
      return;
    }
    notice.appendChild(element('p', 'notice', boundedText(message, 1000)));
  }

  function boot() {
    state.installationId = installationIdFromLocation();
    state.surfaceSessionId = surfaceSessionIdFromLocation();
    if (!state.installationId) {
      bootFailure('This console could not resolve its installation from the host route.');
      return;
    }
    renderBudget();
    callBridge('contract_capabilities', { protocol_version: '1' })
      .then(function () {
        node('boot-notice').hidden = true;
        node('app').hidden = false;
        bindEvents();
        setStatus('Ready. Live updates spend a bounded per-session message budget.', 'ok');
        selectPanel('live');
      })
      .catch(function (error) {
        bootFailure('The host refused this console session: ' +
          String(error && error.message ? error.message : error));
      });
  }

  boot();
})();
