(function () {
  'use strict';

  var BRIDGE_CHANNEL = 'magician-surface-bridge';
  var BRIDGE_BUDGET = 24;
  var PAGE_LIMIT = 20;
  var REQUEST_TIMEOUT_MS = 12000;
  var MAX_RENDER_TEXT = 8000;
  var MAX_TRANSCRIPT_BYTES = 131072;
  var MAX_SPEAKERS = 64;
  var MAX_UTTERANCES = 1000;

  var state = {
    installationId: null,
    sequence: 0,
    requestCounter: 0,
    messagesSent: 0,
    pending: Object.create(null),
    activePanel: 'claims',
    loaded: Object.create(null),
    claims: [],
    claimPage: null,
    claimGeneration: 0,
    claimDetails: [],
    claimDetailsLoaded: Object.create(null),
    selectedClaimId: null,
    commitments: [],
    evidence: [],
    entities: [],
    decisions: [],
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

  function callBridge(method, payload, viewOrAction) {
    return new Promise(function (resolve, reject) {
      if (state.messagesSent >= BRIDGE_BUDGET) {
        reject(new Error('bridge message budget exhausted; reopen the console for a fresh session'));
        return;
      }
      state.requestCounter += 1;
      state.sequence += 1;
      state.messagesSent += 1;
      var requestId = 'claims-review-' + state.requestCounter;
      var timer = window.setTimeout(function () {
        var waiting = state.pending[requestId];
        if (waiting) {
          delete state.pending[requestId];
          waiting.reject(new Error('bridge request timed out closed'));
        }
      }, REQUEST_TIMEOUT_MS);
      state.pending[requestId] = {
        resolve: resolve,
        reject: reject,
        timer: timer
      };
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
        window.parent.postMessage(
          {
            channel: BRIDGE_CHANNEL,
            request: request
          },
          '*'
        );
      }
    });
  }

  function launchAction(actionName, input, idempotencyKey) {
    setStatus('Submitting ' + actionName + ' through the governed host action boundary…', 'busy');
    return callBridge(
      'launch_action',
      {
        idempotency_key: idempotencyKey,
        input: input
      },
      actionName
    ).then(function (launch) {
      var runRef = launch && launch.run_handle && launch.run_handle.run_ref;
      setStatus(
        actionName + ' admitted' + (runRef ? ' as ' + boundedText(runRef, 192) : '') +
          '. Refresh after the governed run projects its result.',
        'ok'
      );
      return launch;
    });
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

  function syncClaims(after, includeSelected) {
    ['sync-claims', 'next-claims', 'first-claims'].forEach(function (id) { node(id).disabled = true; });
    var input = { limit: PAGE_LIMIT, status: 'all' };
    if (after) input.after_claim_id = after;
    if (includeSelected && state.selectedClaimId) input.claim_id = state.selectedClaimId;
    launchAction('sync_claims', input, 'claims-sync-' + Date.now()).then(function () {
      setStatus('Sync started. Refresh after the run finishes. App decisions are review requests; apply them in native Claims Review.', 'ok');
    }).catch(function (error) { setStatus(String(error), 'error'); }).finally(updatePaging);
  }
  function updatePaging() {
    node('sync-claims').disabled = false;
    node('next-claims').disabled = !state.claimPage || !state.claimPage.next_cursor;
    node('first-claims').disabled = !state.claimPage || !state.claimPage.after_claim_id;
  }
  node('sync-claims').addEventListener('click', function () {
    syncClaims(state.claimPage && state.claimPage.after_claim_id, true);
  });
  node('next-claims').addEventListener('click', function () {
    if (state.claimPage && state.claimPage.next_cursor) syncClaims(state.claimPage.next_cursor, false);
  });
  node('first-claims').addEventListener('click', function () { syncClaims(null, false); });

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
      purpose: 'claims_review'
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

  function makeMeta(parts) {
    return parts.filter(function (part) { return part !== undefined && part !== null && part !== ''; })
      .join(' · ');
  }

  function loadClaims() {
    var generation = ++state.claimGeneration;
    state.claimDetails = [];
    state.claimDetailsLoaded = Object.create(null);
    setStatus('Reading bounded claim projections…', 'busy');
    return queryEntity('claim_sync_page', ['page_id', 'after_claim_id', 'next_cursor', 'claim_ids_json'], null,
      { root: 0, nodes: [{ kind: 'compare', field: 'page_id', operator: 'equal', value: 'current' }] }, 1
    ).then(function (pages) {
      if (generation !== state.claimGeneration) return;
      state.claimPage = pages[0] || null;
      var ids = state.claimPage ? JSON.parse(state.claimPage.claim_ids_json) : null;
      if (state.claimPage && (!Array.isArray(ids) || ids.length > PAGE_LIMIT || ids.some(function (id) { return typeof id !== 'string'; }))) {
        throw new Error('invalid synced claim page');
      }
      updatePaging();
      if (ids && !ids.length) return [];
      return queryEntity(
      'claim_summary',
      ['claim_id', 'status', 'claim_text', 'speaker_name', 'audience_kind', 'audience_id', 'context_excerpt', 'expected_revision', 'created_at'],
      [{ field: 'created_at', direction: 'ascending' }],
      ids ? { root: 0, nodes: [{ kind: 'in', field: 'claim_id', values: ids }] } : null
      );
    }).then(function (rows) {
      if (generation !== state.claimGeneration) return;
      state.claims = rows;
      if (!state.claims.some(function (claim) { return claim.claim_id === state.selectedClaimId; })) {
        state.selectedClaimId = state.claims[0] ? state.claims[0].claim_id || null : null;
      }
      renderClaims();
      return loadSelectedClaimDetail();
    }).then(function () {
      if (generation !== state.claimGeneration) return;
      state.loaded.claims = true;
      setStatus('Loaded ' + state.claims.length + ' bounded claim projection(s).', 'ok');
    });
  }

  function loadSelectedClaimDetail() {
    var generation = state.claimGeneration;
    var claimId = state.selectedClaimId;
    if (!claimId || state.claimDetailsLoaded[claimId]) {
      return Promise.resolve();
    }
    // Claim detail carries transcript identifiers and surrounding words. Read
    // exactly the selected row instead of bulk-reading unrelated sensitive
    // detail records merely because they share the current first page.
    state.claimDetailsLoaded[claimId] = 'loading';
    return queryEntity(
      'claim_detail',
      ['claim_id', 'transcript_id', 'utterance_id', 'claim_text', 'prior_context', 'following_context'],
      [{ field: 'synced_at', direction: 'descending' }],
      {
        root: 0,
        nodes: [{ kind: 'compare', field: 'claim_id', operator: 'equal', value: claimId }]
      },
      1
    ).then(function (rows) {
      if (generation !== state.claimGeneration) return;
      state.claimDetails = state.claimDetails.filter(function (detail) {
        return detail.claim_id !== claimId;
      }).concat(rows);
      state.claimDetailsLoaded[claimId] = true;
      if (state.selectedClaimId === claimId) {
        renderClaimDetail();
      }
    }).catch(function (error) {
      if (generation !== state.claimGeneration) return;
      delete state.claimDetailsLoaded[claimId];
      throw error;
    });
  }

  function renderClaims() {
    var list = node('claims-list');
    list.textContent = '';
    node('claims-count').textContent = String(state.claims.length);
    if (!state.claims.length) {
      emptyInto(list, 'No synced statements yet. Use Sync claim register, then refresh this view. Envoy replies and imported conversations share the same register.');
      renderClaimDetail();
      return;
    }
    state.claims.forEach(function (claim) {
      var button = element('button', 'row');
      button.type = 'button';
      button.setAttribute('aria-current', claim.claim_id === state.selectedClaimId ? 'true' : 'false');
      var heading = element('strong');
      heading.appendChild(element('span', 'badge', claim.status || 'unknown'));
      heading.appendChild(window.document.createTextNode(boundedText(claim.claim_text || claim.claim_id, 220)));
      button.appendChild(heading);
      button.appendChild(element('small', null, makeMeta([
        claim.speaker_name,
        claim.audience_kind && claim.audience_id ? claim.audience_kind + ':' + claim.audience_id : '',
        claim.expected_revision !== undefined ? 'revision ' + claim.expected_revision : ''
      ])));
      button.addEventListener('click', function () {
        state.selectedClaimId = claim.claim_id || null;
        renderClaims();
        loadSelectedClaimDetail().catch(function (error) {
          setStatus('Claim detail unavailable: ' + (error && error.message ? error.message : error), 'error');
        });
      });
      list.appendChild(button);
    });
    renderClaimDetail();
  }

  function appendDetail(list, label, value) {
    list.appendChild(element('dt', null, label));
    list.appendChild(element('dd', null, value === undefined || value === null || value === '' ? '—' : value));
  }

  function selectedClaim() {
    return state.claims.filter(function (claim) { return claim.claim_id === state.selectedClaimId; })[0] || null;
  }

  function selectedClaimDetail() {
    return state.claimDetails.filter(function (claim) { return claim.claim_id === state.selectedClaimId; })[0] || null;
  }

  function labeledInput(labelText, id, placeholder) {
    var label = element('label', null, labelText);
    var input = element('input');
    input.id = id;
    input.maxLength = 1024;
    input.autocomplete = 'off';
    input.placeholder = placeholder;
    label.appendChild(input);
    return label;
  }

  function renderClaimDetail() {
    var container = node('claim-detail');
    container.textContent = '';
    var claim = selectedClaim();
    if (!claim) {
      emptyInto(container, 'Select a claim to review it.');
      return;
    }
    var detail = selectedClaimDetail() || claim;
    container.appendChild(element('h3', null, claim.speaker_name || 'Unknown speaker'));
    var quote = element('p', 'claim-copy');
    // Claim detail can be older than a refreshed summary. Canonical words in
    // that summary are complete; never substitute cached detail or truncate.
    quote.textContent = claim.claim_text || '';
    container.appendChild(quote);
    var facts = element('dl');
    appendDetail(facts, 'Claim id', claim.claim_id);
    appendDetail(facts, 'Status', claim.status);
    appendDetail(facts, 'Relationship', makeMeta([claim.audience_kind, claim.audience_id]) || 'Not linked');
    appendDetail(facts, 'Expected revision', claim.expected_revision);
    appendDetail(facts, 'Prior context', detail.prior_context || claim.context_excerpt);
    appendDetail(facts, 'Following context', detail.following_context);
    appendDetail(facts, 'Transcript', detail.transcript_id);
    appendDetail(facts, 'Utterance', detail.utterance_id);
    container.appendChild(facts);

    var form = element('div', 'form-grid');
    form.appendChild(labeledInput('Reason', 'claim-reason', 'Recorded with this review request'));
    container.appendChild(form);
    var actions = element('div', 'action-bar');
    var confirm = element('button', 'button primary', 'Request confirmation (J)');
    confirm.type = 'button';
    confirm.disabled = claim.status !== 'pending';
    confirm.addEventListener('click', function () { prepareClaimDecision('confirm_claim'); });
    var reject = element('button', 'button danger', 'Request rejection (K)');
    reject.type = 'button';
    reject.disabled = claim.status !== 'pending';
    reject.addEventListener('click', function () { prepareClaimDecision('reject_claim'); });
    var recordCommitment = element('button', 'button', 'Record commitment');
    recordCommitment.type = 'button';
    recordCommitment.disabled = claim.status !== 'confirmed' || !claim.audience_kind || !claim.audience_id;
    recordCommitment.addEventListener('click', prepareRecordCommitment);
    actions.appendChild(confirm);
    actions.appendChild(reject);
    actions.appendChild(recordCommitment);
    container.appendChild(actions);
    var notice = element('p', 'notice', 'These buttons save review requests. They do not confirm that a statement is true or apply an owner decision. Open native Claims Review to review the source and apply your decision.');
    container.appendChild(notice);
    var prepared = element('pre', null, 'No decision prepared.');
    prepared.id = 'claim-prepared-action';
    container.appendChild(prepared);
  }

  function stableIntentId(actionName, identity) {
    if (!window.crypto || !window.crypto.subtle ||
        typeof window.crypto.subtle.digest !== 'function' ||
        typeof window.TextEncoder !== 'function') {
      return Promise.reject(new Error('secure intent identity is unavailable; no action was launched'));
    }
    var bytes = new window.TextEncoder().encode(JSON.stringify({ action: actionName, identity: identity }));
    return window.crypto.subtle.digest('SHA-256', bytes).then(function (digest) {
      var hex = Array.prototype.map.call(new Uint8Array(digest), function (value) {
        return value.toString(16).padStart(2, '0');
      }).join('');
      return 'claims-review-intent:' + hex;
    });
  }

  function prepareClaimDecision(actionName) {
    var claim = selectedClaim();
    var reason = node('claim-reason');
    if (!claim || claim.status !== 'pending' || !reason || !reason.value.trim()) {
      setStatus('Select a pending claim and enter the review reason before preparing a decision.', 'error');
      return;
    }
    if (!Number.isSafeInteger(Number(claim.expected_revision)) || Number(claim.expected_revision) < 1) {
      setStatus('The selected claim has no safe positive destination revision; sync it again.', 'error');
      return;
    }
    var exactInput = {
      claim_id: String(claim.claim_id || ''),
      expected_revision: Number(claim.expected_revision),
      reason: reason.value.trim()
    };
    stableIntentId(actionName, exactInput).then(function (requestId) {
      var payload = {
        request_id: requestId,
        claim_id: exactInput.claim_id,
        expected_revision: exactInput.expected_revision,
        reason: exactInput.reason
      };
      node('claim-prepared-action').textContent = JSON.stringify({ action: actionName, input: payload }, null, 2);
      return launchAction(actionName, payload, requestId);
    }).catch(function (error) {
      setStatus('Action refused: ' + (error && error.message ? error.message : error), 'error');
    });
  }

  function prepareRecordCommitment() {
    var claim = selectedClaim();
    if (!claim || claim.status !== 'confirmed' || !claim.audience_kind || !claim.audience_id) {
      setStatus('Record commitment refused: select a confirmed claim; the frame will not invent destination state.', 'error');
      return;
    }
    var revision = Number(claim.expected_revision);
    if (!Number.isSafeInteger(revision) || revision < 1 || !String(claim.claim_id || '')) {
      setStatus('Record commitment refused: the confirmed claim lacks a safe id or revision.', 'error');
      return;
    }
    var exactInput = {
      claim_id: String(claim.claim_id),
      expected_revision: revision
    };
    stableIntentId('record_commitment', exactInput).then(function (requestId) {
      var payload = {
        request_id: requestId,
        claim_id: exactInput.claim_id,
        expected_revision: exactInput.expected_revision
      };
      node('claim-prepared-action').textContent = JSON.stringify({ action: 'record_commitment', input: payload }, null, 2);
      return launchAction('record_commitment', payload, requestId);
    }).catch(function (error) {
      setStatus('Record commitment refused: ' + (error && error.message ? error.message : error), 'error');
    });
  }

  function loadCommitments() {
    setStatus('Reading bounded commitment projections…', 'busy');
    return queryEntity(
      'commitment',
      ['term_id', 'status', 'term_text', 'audience_kind', 'audience_id', 'named_person', 'expected_revision', 'created_at'],
      [{ field: 'created_at', direction: 'ascending' }]
    ).then(function (rows) {
      state.commitments = rows;
      renderCommitments();
      state.loaded.commitments = true;
      setStatus('Loaded ' + rows.length + ' bounded commitment projection(s).', 'ok');
    });
  }

  function renderCommitments() {
    var body = node('commitments-body');
    body.textContent = '';
    node('commitments-count').textContent = String(state.commitments.length);
    if (!state.commitments.length) {
      var row = element('tr');
      var cell = element('td', 'empty', 'No synced commitments. Run sync_context from the native app action surface.');
      cell.colSpan = 6;
      row.appendChild(cell);
      body.appendChild(row);
      return;
    }
    state.commitments.forEach(function (term) {
      var row = element('tr');
      row.appendChild(element('td', null, term.term_text));
      row.appendChild(element('td', null, term.status));
      row.appendChild(element('td', null, makeMeta([term.audience_kind, term.audience_id])));
      row.appendChild(element('td', null, term.named_person));
      row.appendChild(element('td', null, term.expected_revision));
      var cell = element('td');
      var button = element('button', 'button', 'Submit confirmation');
      button.type = 'button';
      button.addEventListener('click', function () {
        prepareCommitmentConfirmation(term);
      });
      cell.appendChild(button);
      row.appendChild(cell);
      body.appendChild(row);
    });
  }

  function prepareCommitmentConfirmation(term) {
    var revision = Number(term.expected_revision);
    var exactInput = {
      commitment_id: String(term.term_id || ''),
      audience_kind: String(term.audience_kind || ''),
      audience_id: String(term.audience_id || ''),
      expected_revision: revision
    };
    if (!exactInput.commitment_id || !exactInput.audience_kind || !exactInput.audience_id ||
        !Number.isSafeInteger(revision) || revision < 1) {
      setStatus('Commitment confirmation refused: the projection lacks its full audience address or safe revision.', 'error');
      return;
    }
    stableIntentId('confirm_commitment', exactInput).then(function (requestId) {
      var payload = {
        request_id: requestId,
        commitment_id: exactInput.commitment_id,
        audience_kind: exactInput.audience_kind,
        audience_id: exactInput.audience_id,
        expected_revision: exactInput.expected_revision
      };
      return launchAction('confirm_commitment', payload, requestId);
    }).catch(function (error) {
      setStatus('Commitment confirmation refused: ' + (error && error.message ? error.message : error), 'error');
    });
  }

  function loadContext() {
    setStatus('Reading bounded correction and entity projections…', 'busy');
    return Promise.all([
      queryEntity('evidence_record', ['record_id', 'state', 'summary', 'created_at']),
      queryEntity('evidence_entity', ['entity_key', 'entity_kind', 'label'])
    ]).then(function (sets) {
      state.evidence = sets[0];
      state.entities = sets[1];
      renderSimpleRows('evidence-list', state.evidence, function (record) {
        return {
          title: record.summary || record.record_id,
          meta: makeMeta([record.state, record.created_at])
        };
      }, 'No correction-history projections are available.');
      renderSimpleRows('entities-list', state.entities, function (record) {
        return {
          title: record.label || record.entity_key,
          meta: makeMeta([record.entity_kind])
        };
      }, 'No entity-context projections are available.');
      state.loaded.context = true;
      setStatus('Loaded bounded correction and entity context.', 'ok');
    });
  }

  function loadActivity() {
    setStatus('Reading local decisions and signed receipt projections…', 'busy');
    return Promise.all([
      queryEntity('review_decision', ['target_id', 'decision', 'expected_revision', 'apply_state']),
      queryEntity('review_receipt', ['receipt_id', 'target_id', 'outcome', 'actor_ref', 'destination_revision', 'error_code'])
    ]).then(function (sets) {
      state.decisions = sets[0];
      state.receipts = sets[1];
      renderSimpleRows('decisions-list', state.decisions, function (record) {
        return {
          title: makeMeta([record.decision, record.target_id]),
          meta: makeMeta([record.apply_state, 'expected revision ' + record.expected_revision])
        };
      }, 'No package decision-ledger records are available.');
      renderSimpleRows('receipts-list', state.receipts, function (record) {
        return {
          title: makeMeta([record.outcome, record.target_id]),
          meta: makeMeta([record.actor_ref, record.receipt_id, 'destination revision ' + record.destination_revision, record.error_code])
        };
      }, 'No signed destination receipts are projected. Local decisions alone are not proof of apply.');
      state.loaded.activity = true;
      setStatus('Loaded local decisions and signed receipt projections.', 'ok');
    });
  }

  function renderSimpleRows(containerId, records, projection, emptyMessage) {
    var container = node(containerId);
    container.textContent = '';
    if (!records.length) {
      emptyInto(container, emptyMessage);
      return;
    }
    records.forEach(function (record) {
      var value = projection(record);
      var row = element('article', 'row');
      row.appendChild(element('strong', null, value.title));
      row.appendChild(element('small', null, value.meta));
      container.appendChild(row);
    });
  }

  function parseSpeakerMap(raw) {
    var speakers = Object.create(null);
    var lines = raw.split(/\r?\n/).filter(function (line) { return line.trim(); });
    if (!lines.length || lines.length > MAX_SPEAKERS) {
      throw new Error('speaker map must contain 1 to ' + MAX_SPEAKERS + ' entries');
    }
    lines.forEach(function (line) {
      var separator = line.indexOf('=');
      if (separator < 1) {
        throw new Error('every speaker-map line must use speaker-key = Named Person');
      }
      var key = line.slice(0, separator).trim();
      var name = line.slice(separator + 1).trim();
      if (!/^[A-Za-z0-9_.-]{1,64}$/.test(key) || !name || name.length > 256) {
        throw new Error('speaker keys or named people are invalid or too long');
      }
      if (Object.prototype.hasOwnProperty.call(speakers, key)) {
        throw new Error('speaker keys must be unique');
      }
      speakers[key] = name;
    });
    return speakers;
  }

  function parseUtterances(raw, speakers) {
    var lines = raw.split(/\r?\n/).filter(function (line) { return line.trim(); });
    if (!lines.length || lines.length > MAX_UTTERANCES) {
      throw new Error('utterances must contain 1 to ' + MAX_UTTERANCES + ' lines');
    }
    return lines.map(function (line, index) {
      var separator = line.indexOf('|');
      if (separator < 1) {
        throw new Error('utterance line ' + (index + 1) + ' must use speaker-key | exact words');
      }
      var key = line.slice(0, separator).trim();
      var text = line.slice(separator + 1).trim();
      if (!Object.prototype.hasOwnProperty.call(speakers, key)) {
        throw new Error('utterance line ' + (index + 1) + ' names an unmapped speaker key');
      }
      if (!text || text.length > 16000) {
        throw new Error('utterance line ' + (index + 1) + ' is empty or too long');
      }
      return { speaker: key, text: text };
    });
  }

  function byteLength(text) {
    return new window.TextEncoder().encode(text).length;
  }

  function prepareIngest(event) {
    event.preventDefault();
    try {
      var speakers = parseSpeakerMap(node('speaker-map').value);
      var utterances = parseUtterances(node('utterances').value, speakers);
      var transcriptText = utterances.map(function (utterance) {
        return '[' + utterance.speaker + '] ' + utterance.text;
      }).join('\n');
      if (byteLength(transcriptText) > MAX_TRANSCRIPT_BYTES) {
        throw new Error('structured transcript exceeds the 131072-byte ceiling');
      }
      var payload = {
        ingest_id: node('ingest-id').value.trim(),
        transcript_text: transcriptText,
        speaker_mapping_json: JSON.stringify({ speakers: speakers, utterances: utterances }),
        audience_kind: node('audience-kind').value.trim(),
        audience_id: node('audience-id').value.trim(),
        outwardness_reason: node('outwardness-reason').value.trim()
      };
      Object.keys(payload).forEach(function (key) {
        if (!String(payload[key]).trim()) {
          throw new Error(key + ' is required');
        }
      });
      node('prepared-action').textContent = JSON.stringify({ action: 'stage_ingest', input: payload }, null, 2);
      stableIntentId('stage_ingest', { ingest_id: payload.ingest_id }).then(function (intentId) {
        return launchAction('stage_ingest', payload, intentId);
      }).catch(function (error) {
        setStatus('Ingest action refused: ' + (error && error.message ? error.message : error), 'error');
      });
    } catch (error) {
      node('prepared-action').textContent = 'No action prepared.';
      setStatus('Ingest preparation refused: ' + (error && error.message ? error.message : error), 'error');
    }
  }

  function loadPanel(panelName, force) {
    if (!force && state.loaded[panelName]) {
      return Promise.resolve();
    }
    var loader = {
      claims: loadClaims,
      commitments: loadCommitments,
      context: loadContext,
      activity: loadActivity,
      ingest: function () {
        state.loaded.ingest = true;
        setStatus('Structured ingest form ready. Attribution remains manual.', 'ok');
        return Promise.resolve();
      }
    }[panelName];
    return loader().catch(function (error) {
      setStatus('View unavailable: ' + (error && error.message ? error.message : error), 'error');
    });
  }

  function showPanel(panelName) {
    state.activePanel = panelName;
    Array.prototype.forEach.call(window.document.querySelectorAll('.panel'), function (panel) {
      panel.hidden = panel.id !== 'panel-' + panelName;
    });
    Array.prototype.forEach.call(window.document.querySelectorAll('.tab'), function (tab) {
      tab.setAttribute('aria-selected', tab.dataset.panel === panelName ? 'true' : 'false');
    });
    loadPanel(panelName, false);
  }

  function installInteractions() {
    Array.prototype.forEach.call(window.document.querySelectorAll('.tab'), function (tab) {
      tab.addEventListener('click', function () { showPanel(tab.dataset.panel); });
    });
    node('refresh').addEventListener('click', function () {
      loadPanel(state.activePanel, true);
    });
    node('ingest-form').addEventListener('submit', prepareIngest);
    window.document.addEventListener('keydown', function (event) {
      if (state.activePanel !== 'claims' || event.altKey || event.ctrlKey || event.metaKey) {
        return;
      }
      var target = event.target;
      if (target && (target.tagName === 'INPUT' || target.tagName === 'TEXTAREA' || target.tagName === 'SELECT')) {
        return;
      }
      if (event.key === 'j' || event.key === 'J') {
        event.preventDefault();
        prepareClaimDecision('confirm_claim');
      } else if (event.key === 'k' || event.key === 'K') {
        event.preventDefault();
        prepareClaimDecision('reject_claim');
      }
    });
  }

  function boot() {
    state.installationId = installationIdFromLocation();
    if (!state.installationId) {
      var bootNotice = node('boot-notice');
      bootNotice.appendChild(element('p', 'notice', 'This document is not mounted at a reviewed scripted-surface address.'));
      return;
    }
    callBridge('contract_capabilities', {})
      .then(function () {
        node('boot-notice').remove();
        node('app').hidden = false;
        installInteractions();
        return loadPanel('claims', false);
      })
      .catch(function (error) {
        var bootNotice = node('boot-notice');
        bootNotice.appendChild(element('p', 'notice', 'Bridge refused: ' + (error && error.message ? error.message : error)));
      });
  }

  if (window.document.readyState === 'loading') {
    window.document.addEventListener('DOMContentLoaded', boot);
  } else {
    boot();
  }
})();
