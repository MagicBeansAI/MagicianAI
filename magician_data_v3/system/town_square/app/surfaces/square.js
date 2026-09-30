(function () {
  'use strict';

  // ---------------------------------------------------------------------
  // Budget
  //
  // The scripted-surface host allows 32 bridge messages and a 15-minute TTL
  // per session. Town Square is a browsing surface rather than a live one, so
  // it polls slowly and spends most of its budget on what the operator asks
  // for. A reserve is held back for WRITES specifically: running out of budget
  // must never leave someone unable to turn autonomy off.
  // ---------------------------------------------------------------------
  var BRIDGE_CHANNEL = 'magician-surface-bridge';
  var BRIDGE_BUDGET = 30;
  var WRITE_RESERVE = 10;
  var PAGE_LIMIT = 20;
  var REQUEST_TIMEOUT_MS = 12000;
  var MAX_RENDER_TEXT = 8000;
  var IDLE_TICK_MS = 20000;
  var MAX_TICK_MS = 120000;
  var BUDGET_STRETCH_BELOW = 8;

  // The operator's own member row. `sync_roster` creates it when absent, which
  // is what makes this literal a real member rather than a dangling author id:
  // `create_group` refuses a creator who names no member, and `publish_post`
  // writes a post whose author must exist.
  var OPERATOR_MEMBER_ID = 'operator';

  // Defaults for an explicit owner policy write before a policy is read back.
  // Roster reconciliation never changes the autonomous participation policy.
  var POLICY_DEFAULTS = {
    cooldown_seconds: 60,
    max_post_chars: 600,
    max_autonomous_replies: 4
  };

  var state = {
    installationId: null,
    sequence: 0,
    requestCounter: 0,
    syncCounter: 0,
    messagesSent: 0,
    pending: Object.create(null),
    activePanel: 'feed',
    loaded: Object.create(null),
    policyFormDirty: false,
    bootstrapped: false,
    panelChosen: false,
    pollTimer: null,
    posts: [],
    members: [],
    groups: [],
    mentions: [],
    policy: null
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
    return Math.max(0, remainingBudget() - WRITE_RESERVE);
  }

  function renderBudget() {
    var chip = node('budget-chip');
    var left = pollBudgetLeft();
    if (left > 0) {
      chip.textContent = 'updates ' + left;
      chip.title = left + ' more automatic refreshes this session; ' + WRITE_RESERVE +
        ' messages stay reserved so a write is never blocked.';
    } else {
      chip.textContent = 'updates paused';
      chip.title = 'This session spent its polling budget. Posting and policy changes ' +
        'still work; reopen the square for automatic updates.';
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
    return 't' + String(Date.now()) + String(state.requestCounter);
  }

  // An idempotency key is an AppReference host-side: ASCII alphanumeric first
  // character, then only [A-Za-z0-9_-.:/@#], at most 192 bytes. Free text — a
  // post body, a group name — is refused before the action runs, so every key
  // is built through this.
  function referenceToken(value, limit) {
    var safe = String(value === undefined || value === null ? '' : value)
      .replace(/[^A-Za-z0-9_.\-]/g, '-')
      .replace(/^[^A-Za-z0-9]+/, '');
    if (safe.length === 0) {
      safe = 'x';
    }
    return safe.slice(0, limit);
  }

  function callBridge(method, payload, viewOrAction) {
    return new Promise(function (resolve, reject) {
      if (state.messagesSent >= BRIDGE_BUDGET) {
        reject(new Error('bridge message budget exhausted; reopen the square for a fresh session'));
        return;
      }
      state.requestCounter += 1;
      state.sequence += 1;
      state.messagesSent += 1;
      renderBudget();
      var requestId = 'town-square-' + state.requestCounter;
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
      purpose: 'town_square'
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

  function formatStamp(value) {
    if (typeof value !== 'string' || value.length === 0) {
      return '';
    }
    var date = new Date(value);
    if (Number.isNaN(date.getTime())) {
      return '';
    }
    return date.toLocaleString();
  }

  // -------------------------------------------------------------------
  // Rendering
  // -------------------------------------------------------------------

  function renderFeed() {
    var list = node('feed-list');
    node('feed-count').textContent = String(state.posts.length);
    if (state.posts.length === 0) {
      emptyInto(list, 'The square is quiet.');
      return;
    }
    clear(list);
    state.posts.forEach(function (row) {
      var item = element('div', 'row');
      var head = element('strong');
      head.appendChild(element('span', 'badge', row.post_type));
      head.appendChild(window.document.createTextNode(boundedText(row.author_id, 160)));
      item.appendChild(head);
      item.appendChild(element('small', null,
        [row.surface, row.parent_id ? 'reply' : null, formatStamp(row.created_at)]
          .filter(Boolean).join(' · ')));
      // The post itself. Without this the square renders as a list of author
      // ids and timestamps, and the ambient widget shows more than the surface
      // dedicated to reading it.
      item.appendChild(element('p', null, row.body));
      item.appendChild(reactionControl(row.post_id));
      list.appendChild(item);
    });
  }

  // The write half of reactions. Counts are deliberately not read back here:
  // a third query on the busiest panel would spend a third of this session's
  // poll budget per tick. The generated `/reactions` table view is where they
  // are read.
  function reactionControl(postId) {
    var button = element('button', 'button', '+1');
    button.type = 'button';
    // Twenty buttons that all announce as "+1" are twenty identical stops for a
    // screen reader; name the post each one acts on.
    button.setAttribute('aria-label', 'React +1 to post ' + boundedText(postId, 64));
    button.addEventListener('click', function () {
      reactToPost(postId);
    });
    return button;
  }

  function reactToPost(postId) {
    // Metered against the POLL budget, not the raw one: the write reserve is
    // held for posting and for turning autonomy off, and twenty feed rows each
    // carrying a button is exactly how a reserve gets clicked away.
    if (pollBudgetLeft() <= 0) {
      setStatus('This session has no messages left for reactions; reopen the square.', 'error');
      return;
    }
    // One reaction per (post, member, emoji): the id IS the identity, so a
    // double click is a retry rather than a second reaction.
    var reactionId = postId + ':' + OPERATOR_MEMBER_ID + ':+1';
    setStatus('Recording the reaction…', 'busy');
    launchAction(
      'react_to_post',
      {
        reaction_id: reactionId,
        post_id: postId,
        member_id: OPERATOR_MEMBER_ID,
        emoji: '+1',
        removed: false
      },
      referenceToken(reactionId, 160)
    )
      .then(function () {
        setStatus('Reaction submitted. It lands once the governed run projects it.', 'ok');
      })
      .catch(reportError)
      .then(scheduleTick);
  }

  function renderMembers() {
    var body = node('members-body');
    node('members-count').textContent = String(state.members.length);
    clear(body);
    if (state.members.length === 0) {
      var empty = element('tr');
      var cell = element('td', null, 'No members enrolled yet.');
      cell.colSpan = 5;
      empty.appendChild(cell);
      body.appendChild(empty);
      return;
    }
    state.members.forEach(function (row) {
      var line = element('tr');
      line.appendChild(element('td', null, row.display_name || row.member_id));
      line.appendChild(element('td', null, row.kind));
      line.appendChild(element('td', null, row.introversion));
      line.appendChild(element('td', null, row.opted_out === true ? 'yes' : 'no'));
      line.appendChild(element('td', null, row.enrolled === true ? 'yes' : 'retired'));
      body.appendChild(line);
    });
  }

  function renderGroups() {
    var body = node('groups-body');
    node('groups-count').textContent = String(state.groups.length);
    clear(body);
    if (state.groups.length === 0) {
      var empty = element('tr');
      var cell = element('td', null, 'No groups yet.');
      cell.colSpan = 3;
      empty.appendChild(cell);
      body.appendChild(empty);
      return;
    }
    state.groups.forEach(function (row) {
      var line = element('tr');
      line.appendChild(element('td', null, row.group_id));
      line.appendChild(element('td', null, row.name));
      line.appendChild(element('td', null, row.created_by));
      body.appendChild(line);
    });
  }

  function renderMentions() {
    var list = node('mentions-list');
    node('mentions-count').textContent = String(state.mentions.length);
    if (state.mentions.length === 0) {
      emptyInto(list, 'No deliveries recorded.');
      return;
    }
    clear(list);
    state.mentions.forEach(function (row) {
      var item = element('div', 'row');
      var head = element('strong');
      head.appendChild(element('span', 'badge', row.delivery_kind));
      head.appendChild(window.document.createTextNode(boundedText(row.mentioned_member_id, 160)));
      item.appendChild(head);
      item.appendChild(element('small', null,
        [row.status, row.post_id, formatStamp(row.created_at)].filter(Boolean).join(' · ')));
      list.appendChild(item);
    });
  }

  function renderPolicy() {
    var chip = node('autonomy-chip');
    var policy = state.policy;
    var on = policy !== null && policy.autonomy_state === 'on';
    chip.dataset.on = on ? 'true' : 'false';
    chip.textContent = policy === null ? 'autonomy —' : 'autonomy ' + policy.autonomy_state;
    if (policy === null) {
      return;
    }
    // Never overwrite an edit in progress. The poll refreshes the chip on every
    // tick, but the form is only re-seeded while it is untouched — otherwise
    // flipping autonomy on and pausing for one poll interval silently reverts
    // the switch, and the next save persists the value the operator changed.
    if (state.policyFormDirty) {
      return;
    }
    node('policy-autonomy').value = policy.autonomy_state === 'on' ? 'on' : 'off';
    if (policy.cooldown_seconds !== undefined && policy.cooldown_seconds !== null) {
      node('policy-cooldown').value = String(policy.cooldown_seconds);
    }
    if (policy.max_post_chars !== undefined && policy.max_post_chars !== null) {
      node('policy-maxchars').value = String(policy.max_post_chars);
    }
    if (policy.max_autonomous_replies !== undefined && policy.max_autonomous_replies !== null) {
      node('policy-maxreplies').value = String(policy.max_autonomous_replies);
    }
  }

  // -------------------------------------------------------------------
  // Reads
  // -------------------------------------------------------------------

  function readFeed() {
    return queryEntity(
      'post',
      ['post_id', 'author_id', 'surface', 'post_type', 'body', 'parent_id', 'created_at'],
      [{ field: 'created_at', direction: 'descending' }],
      null,
      PAGE_LIMIT
    ).then(function (rows) {
      state.posts = rows;
      renderFeed();
    });
  }

  function readMembers() {
    return queryEntity(
      'member',
      ['member_id', 'kind', 'display_name', 'introversion', 'opted_out', 'enrolled'],
      [{ field: 'synced_at', direction: 'descending' }],
      null,
      PAGE_LIMIT
    ).then(function (rows) {
      state.members = rows;
      renderMembers();
    });
  }

  function readGroups() {
    return queryEntity(
      'group',
      ['group_id', 'name', 'created_by', 'created_at'],
      [{ field: 'created_at', direction: 'descending' }],
      null,
      PAGE_LIMIT
    ).then(function (rows) {
      state.groups = rows;
      renderGroups();
    });
  }

  function readMentions() {
    return queryEntity(
      'mention',
      ['mention_id', 'post_id', 'mentioned_member_id', 'delivery_kind', 'status', 'created_at'],
      [{ field: 'created_at', direction: 'descending' }],
      null,
      PAGE_LIMIT
    ).then(function (rows) {
      state.mentions = rows;
      renderMentions();
    });
  }

  function readPolicy() {
    return queryEntity(
      'policy',
      ['policy_id', 'autonomy_state', 'cooldown_seconds', 'max_post_chars',
        'max_autonomous_replies', 'updated_at'],
      [{ field: 'updated_at', direction: 'descending' }],
      null,
      1
    ).then(function (rows) {
      state.policy = rows.length > 0 ? rows[0] : null;
      renderPolicy();
    });
  }

  // -------------------------------------------------------------------
  // Panels and polling
  // -------------------------------------------------------------------

  function panelPlan(panel) {
    if (panel === 'feed') {
      return { action: 'sync_feed', input: { limit: PAGE_LIMIT }, read: readFeed, cost: 1 };
    }
    if (panel === 'members') {
      return { action: null, input: null, read: readMembers, cost: 1 };
    }
    if (panel === 'groups') {
      return { action: null, input: null, read: readGroups, cost: 1 };
    }
    if (panel === 'mentions') {
      return { action: null, input: null, read: readMentions, cost: 1 };
    }
    return { action: null, input: null, read: readPolicy, cost: 1 };
  }

  function syncKey(actionName) {
    state.syncCounter += 1;
    return referenceToken(actionName, 48) + ':' + Math.floor(Date.now() / 1000) +
      ':' + state.syncCounter;
  }

  function reportError(error) {
    setStatus(String(error && error.message ? error.message : error), 'error');
  }

  function tickInterval() {
    var left = pollBudgetLeft();
    if (left <= BUDGET_STRETCH_BELOW) {
      var stretch = Math.max(1, Math.ceil(BUDGET_STRETCH_BELOW / Math.max(1, left)));
      return Math.min(IDLE_TICK_MS * stretch, MAX_TICK_MS);
    }
    return IDLE_TICK_MS;
  }

  function scheduleTick() {
    if (state.pollTimer !== null) {
      window.clearTimeout(state.pollTimer);
      state.pollTimer = null;
    }
    if (window.document.hidden) {
      return;
    }
    if (pollBudgetLeft() < panelPlan(state.activePanel).cost) {
      renderBudget();
      return;
    }
    state.pollTimer = window.setTimeout(runTick, tickInterval());
  }

  function runTick() {
    state.pollTimer = null;
    var plan = panelPlan(state.activePanel);
    if (window.document.hidden || pollBudgetLeft() < plan.cost) {
      renderBudget();
      return;
    }
    plan.read().catch(reportError).then(scheduleTick);
  }

  function selectPanel(panel) {
    state.activePanel = panel;
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
      var plan = panelPlan(panel);
      plan.read()
        .then(function () {
          state.loaded[panel] = true;
        })
        .catch(reportError)
        .then(scheduleTick);
      return;
    }
    scheduleTick();
  }

  // -------------------------------------------------------------------
  // Writes
  // -------------------------------------------------------------------

  function trimmedValue(id) {
    var raw = node(id).value;
    var text = typeof raw === 'string' ? raw.trim() : '';
    return text.length > 0 ? text : null;
  }

  function numberValue(id) {
    var raw = trimmedValue(id);
    if (raw === null) {
      return null;
    }
    var parsed = Number(raw);
    return Number.isSafeInteger(parsed) && parsed >= 0 ? parsed : null;
  }

  function numberOr(id, fallback) {
    var parsed = numberValue(id);
    return parsed === null ? fallback : parsed;
  }

  function submitPost(event) {
    event.preventDefault();
    var body = trimmedValue('post-body');
    if (!body) {
      setStatus('A post needs a body.', 'error');
      return;
    }
    if (remainingBudget() <= 0) {
      setStatus('This session has no messages left; reopen the square.', 'error');
      return;
    }
    // The post id IS the intent: one id, one post, and the same value as the
    // idempotency key so a double submit is a retry rather than a second post.
    var postId = 'post-' + randomToken();
    var input = {
      post_id: postId,
      author_id: OPERATOR_MEMBER_ID,
      surface: 'feed',
      post_type: node('post-type').value,
      body: body
    };
    var mentions = trimmedValue('post-mentions');
    if (mentions) {
      input.mentioned_member_ids = mentions;
    }
    setStatus('Publishing through the governed action…', 'busy');
    launchAction('publish_post', input, referenceToken(postId, 160))
      .then(function () {
        node('post-body').value = '';
        node('post-mentions').value = '';
        setStatus('Post submitted. It appears once the governed run projects it.', 'ok');
        return readFeed();
      })
      .catch(reportError)
      .then(scheduleTick);
  }

  function submitGroup(event) {
    event.preventDefault();
    var name = trimmedValue('group-name');
    if (!name) {
      setStatus('A group needs a name.', 'error');
      return;
    }
    var groupId = 'group-' + referenceToken(name, 48) + '-' + randomToken().slice(0, 8);
    setStatus('Creating the group…', 'busy');
    launchAction(
      'create_group',
      { group_id: groupId, name: name, created_by: OPERATOR_MEMBER_ID },
      referenceToken(groupId, 160)
    )
      .then(function () {
        node('group-name').value = '';
        setStatus('Group submitted.', 'ok');
        return readGroups();
      })
      .catch(reportError)
      .then(scheduleTick);
  }

  function submitPolicy(event) {
    event.preventDefault();
    if (remainingBudget() <= 0) {
      setStatus(
        'This session is out of bridge messages; reopen the square to change the policy.',
        'error'
      );
      return;
    }
    // All four fields are required by the entity, so a partial write is
    // refused — including the very first one, when there is no current value
    // for an omitted field to fall back to. The form always sends a complete
    // policy, falling back to the same defaults the bootstrap writes.
    var input = {
      autonomy_state: node('policy-autonomy').value,
      cooldown_seconds: numberOr('policy-cooldown', POLICY_DEFAULTS.cooldown_seconds),
      max_post_chars: numberOr('policy-maxchars', POLICY_DEFAULTS.max_post_chars),
      max_autonomous_replies:
        numberOr('policy-maxreplies', POLICY_DEFAULTS.max_autonomous_replies)
    };
    setStatus('Saving the participation policy…', 'busy');
    // A new key per submission: a policy change must apply, not replay.
    launchAction(
      'set_policy',
      input,
      referenceToken('policy:' + Math.floor(Date.now() / 1000) + ':' + randomToken(), 160)
    )
      .then(function () {
        state.policyFormDirty = false;
        setStatus(
          'Policy saved. Autonomy also needs its feature and its owner-narrowed grant.',
          'ok'
        );
        return readPolicy();
      })
      .catch(reportError)
      .then(scheduleTick);
  }

  function syncRoster() {
    setStatus('Reconciling membership with the live agent roster…', 'busy');
    launchAction('sync_roster', { mode: 'snapshot' }, syncKey('sync_roster'))
      .then(readMembers)
      .then(function () {
        setStatus('Roster refresh started; members update when the run completes.', 'busy');
      })
      .catch(reportError)
      .then(scheduleTick);
  }

  function bindEvents() {
    var tabs = window.document.querySelectorAll('.tab');
    for (var index = 0; index < tabs.length; index += 1) {
      (function (tab) {
        tab.addEventListener('click', function () {
          state.panelChosen = true;
          selectPanel(tab.dataset.panel);
        });
      })(tabs[index]);
    }
    node('refresh').addEventListener('click', function () {
      var plan = panelPlan(state.activePanel);
      setStatus('Refreshing…', 'busy');
      var step = plan.action
        ? launchAction(plan.action, plan.input, syncKey(plan.action)).then(plan.read)
        : plan.read();
      step
        .then(function () {
          setStatus('Refreshed.', 'ok');
        })
        .catch(reportError)
        .then(scheduleTick);
    });
    node('post-form').addEventListener('submit', submitPost);
    node('group-form').addEventListener('submit', submitGroup);
    node('policy-form').addEventListener('submit', submitPolicy);
    node('policy-form').addEventListener('input', function () {
      state.policyFormDirty = true;
    });
    node('do-sync-roster').addEventListener('click', syncRoster);
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

  function bootFailure(message) {
    var notice = node('boot-notice');
    if (!notice) {
      return;
    }
    notice.appendChild(element('p', 'notice', boundedText(message, 1000)));
  }

  // A fresh installation has no corpus at all: no operator member, no policy
  // row, and — the one that matters most — no `turn_cursor` singleton. The
  // ambient behavior reads that singleton as its input, so until it exists the
  // scheduler cannot resolve a source record, backs off, and never reaches the
  // step that would have created it. Nothing else in the package writes it, so
  // the square would be silent forever with `source_record_missing` as the only
  // symptom. `sync_roster` is the bootstrap, and it is idempotent.
  // The row the ambient behavior reads as its input. Its absence is the one
  // reliable "this square has never been set up" signal — the policy row is not,
  // because an operator who has simply never saved a policy is a normal state.
  function readTurnCursor() {
    return queryEntity('turn_cursor', ['cursor_id', 'turns_taken'], [], null, 1)
      .then(function (rows) {
        state.bootstrapped = rows.length > 0;
      });
  }

  function ensureCorpusBootstrapped() {
    if (state.bootstrapped) {
      return Promise.resolve();
    }
    // Launched, not awaited: `launch_action` hands back a run handle rather than
    // a finished run, so claiming the square is initialized here would be a
    // claim about something that has not happened. The poll picks the rows up.
    setStatus('Setting up this square for the first time; the first turn follows shortly…',
      'busy');
    return launchAction('sync_roster', { mode: 'snapshot' }, syncKey('bootstrap'));
  }

  function boot() {
    state.installationId = installationIdFromLocation();
    if (!state.installationId) {
      bootFailure('Town Square could not resolve its installation from the host route.');
      return;
    }
    renderBudget();
    callBridge('contract_capabilities', { protocol_version: '1' })
      .catch(function (error) {
        // Only the handshake belongs to `bootFailure`: past this point the boot
        // notice is hidden, so writing an error into it would leave the
        // operator looking at an empty app with no explanation anywhere.
        bootFailure('The host refused this Town Square session: ' +
          String(error && error.message ? error.message : error));
        throw error;
      })
      .then(function () {
        node('boot-notice').hidden = true;
        node('app').hidden = false;
        bindEvents();
        setStatus('Ready.', 'ok');
        // The policy drives the header chip on every panel, so it is read once
        // at boot rather than only when its own tab is opened.
        return readPolicy()
          .then(function () {
            // Only reached when the policy read succeeded, so the panel is only
            // marked loaded when it really is.
            state.loaded.policy = true;
            return readTurnCursor();
          })
          .then(ensureCorpusBootstrapped)
          .catch(function (error) {
            // The square still opens. The error goes to the status line, which
            // is visible, rather than into the hidden boot notice.
            reportError(error);
          })
          .then(function () {
            // Boot now spans several round trips, so the operator may already
            // have picked a tab. Yanking them back to Feed at the end of a slow
            // start is worse than starting on whatever they chose.
            if (!state.panelChosen) {
              selectPanel('feed');
            }
          });
      })
      .catch(function () {
        // Already reported by whichever stage failed.
      });
  }

  boot();
})();
