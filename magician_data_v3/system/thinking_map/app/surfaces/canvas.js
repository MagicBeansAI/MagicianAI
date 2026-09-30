// Brainstorm canvas — the custom_surfaces_v1 reference surface script.
//
// Self-contained, dependency-free, read-only. The frame's only authority
// channel is the eight-operation bridge: this script posts bridge requests
// to the parent host (which alone holds authentication) and renders the
// replies. No fetch, no XMLHttpRequest, no WebSocket, no window.open, no
// eval; the kernel CSP would refuse them all anyway (connect-src 'none').
//
// Data path (the plan-2.5 pattern): the package's `sync_maps` workflow
// calls the reviewed `thinking_maps_data` reads and projects the owner's
// maps into this installation's entity store; this surface reads that
// store through `query_data` — the map library from `thinking_map_summary`
// and the synced boards from `thinking_map_snapshot`. Editing is out of
// scope; the mutation port is a separate re-open condition.
(function () {
  'use strict';

  var BRIDGE_CHANNEL = 'magician-surface-bridge';
  var BRIDGE_BUDGET = 24; // keep headroom under the kernel's 32-message session cap
  var PAGE_LIMIT = 25;

  var state = {
    installationId: null,
    sequence: 0,
    requestCounter: 0,
    messagesSent: 0,
    pending: Object.create(null),
    summaries: [],
    snapshots: {},
    selectedMapId: null,
    selectedNodeId: null,
    refreshedOnce: false
  };

  // ── Bridge client ────────────────────────────────────────────────────────

  function installationIdFromLocation() {
    // Web: the kernel-minted entry address is
    // /api/magician/v2/apps/installations/{installation_id}/custom-surface-v1/assets/{digest}/surfaces/canvas.html
    // iOS: the custom-scheme address is
    // magapp-surface://{installation_id}/{digest}/surfaces/canvas.html
    // In both forms the frame's own URL names its installation — the web
    // form in a path segment, the iOS form as the URL host. (Hosts are
    // case-lowered by parsers, so the installation id must be compared
    // case-insensitively when read from the host.)
    var parts = window.location.pathname.split('/');
    for (var i = 0; i < parts.length - 1; i += 1) {
      if (parts[i] === 'installations' && parts[i + 1]) {
        try {
          return decodeURIComponent(parts[i + 1]);
        } catch (error) {
          return null;
        }
      }
    }
    // iOS custom-scheme form: the host IS the installation id. WKWebView
    // lowercases hosts, but the installation id charset [A-Za-z0-9_.-]
    // makes the lowering ambiguous only for letters — resolve by
    // returning the raw host and letting the backend's case-sensitive
    // match decide (the session envelope carries the canonical id).
    if (window.location.protocol.indexOf('magapp-surface:') === 0 && window.location.hostname) {
      return window.location.hostname;
    }
    return null;
  }

  function callBridge(method, payload) {
    return new Promise(function (resolve, reject) {
      if (state.messagesSent >= BRIDGE_BUDGET) {
        reject(new Error('bridge message budget exhausted for this session'));
        return;
      }
      state.requestCounter += 1;
      state.sequence += 1;
      state.messagesSent += 1;
      var requestId = 'canvas-req-' + state.requestCounter;
      state.pending[requestId] = { resolve: resolve, reject: reject };
      // The parent host fills the session/installation authority envelope;
      // the frame contributes only the method, sequence, id and payload.
      window.parent.postMessage(
        {
          channel: BRIDGE_CHANNEL,
          request: {
            method: method,
            sequence: state.sequence,
            request_id: requestId,
            payload: payload
          }
        },
        '*'
      );
    });
  }

  window.addEventListener('message', function (event) {
    var data = event.data;
    if (!data || data.channel !== BRIDGE_CHANNEL || data.kind !== 'reply') {
      return;
    }
    var pending = state.pending[data.request_id];
    if (!pending) {
      return;
    }
    delete state.pending[data.request_id];
    if (typeof data.error === 'string' && data.error) {
      pending.reject(new Error(data.error));
    } else {
      pending.resolve(data.result);
    }
  });

  function queryEntity(entity, select, order) {
    var payload = {
      protocol_version: '1',
      source_installation_id: state.installationId,
      entity: entity,
      select: select,
      limit: PAGE_LIMIT,
      purpose: 'canvas'
    };
    if (order) {
      payload.order = order;
    }
    return callBridge('query_data', payload).then(function (page) {
      var rows = [];
      if (page && page.envelope && Array.isArray(page.envelope.value)) {
        rows = page.envelope.value;
      }
      return rows;
    });
  }

  // ── Loading ──────────────────────────────────────────────────────────────

  function loadLibrary() {
    return queryEntity(
      'thinking_map_summary',
      ['map_id', 'title', 'lifecycle', 'latest_revision', 'updated_at', 'synced_at'],
      [{ field: 'updated_at', direction: 'descending' }]
    ).then(function (rows) {
      state.summaries = rows.map(function (row) {
        return row.fields || {};
      });
    });
  }

  function loadSnapshots() {
    return queryEntity(
      'thinking_map_snapshot',
      ['map_id', 'title', 'lifecycle', 'revision', 'snapshot', 'synced_at']
    ).then(function (rows) {
      state.snapshots = {};
      rows.forEach(function (row) {
        var fields = row.fields || {};
        if (fields.map_id) {
          state.snapshots[fields.map_id] = fields;
        }
      });
    });
  }

  function defaultMapId() {
    var withSnapshot = null;
    var first = null;
    state.summaries.forEach(function (summary) {
      if (!first && summary.map_id) {
        first = summary.map_id;
      }
      if (!withSnapshot && summary.map_id && state.snapshots[summary.map_id]) {
        withSnapshot = summary.map_id;
      }
    });
    if (state.selectedMapId && state.snapshots[state.selectedMapId]) {
      return state.selectedMapId;
    }
    return withSnapshot || first || state.selectedMapId;
  }

  // ── Rendering ────────────────────────────────────────────────────────────

  function el(tag, className, text) {
    var node = window.document.createElement(tag);
    if (className) {
      node.className = className;
    }
    if (text !== undefined && text !== null) {
      node.textContent = String(text);
    }
    return node;
  }

  function svgTag(name) {
    return window.document.createElementNS('http://www.w3.org/2000/svg', name);
  }

  function setStatus(text, isError) {
    var status = window.document.getElementById('status');
    status.textContent = text;
    status.dataset.state = isError ? 'error' : 'idle';
  }

  function renderLibrary() {
    var hint = window.document.getElementById('library-hint');
    var list = window.document.getElementById('map-list');
    var syncHint = window.document.getElementById('sync-hint');
    list.textContent = '';
    syncHint.hidden = true;
    syncHint.textContent = '';

    if (!state.summaries.length) {
      hint.textContent =
        'No maps synced yet. Run the sync_maps action from the library view ' +
        'to pull a page of your thinking maps into this canvas.';
      return;
    }
    hint.hidden = true;

    state.summaries.forEach(function (summary) {
      if (!summary.map_id) {
        return;
      }
      var item = el('li');
      var button = el('button', 'map');
      button.type = 'button';
      button.appendChild(el('span', null, summary.title || summary.map_id));
      var meta = summary.lifecycle || '';
      if (summary.latest_revision !== undefined && summary.latest_revision !== null) {
        meta = (meta ? meta + ' · ' : '') + 'r' + summary.latest_revision;
      }
      if (!state.snapshots[summary.map_id]) {
        meta = (meta ? meta + ' · ' : '') + 'snapshot not synced';
      }
      var metaLine = el('span', 'meta', meta);
      button.appendChild(metaLine);
      if (summary.map_id === state.selectedMapId) {
        button.setAttribute('aria-current', 'true');
      }
      button.addEventListener('click', function () {
        state.selectedMapId = summary.map_id;
        state.selectedNodeId = null;
        renderLibrary();
        renderBoard();
        renderDetail();
      });
      item.appendChild(button);
      list.appendChild(item);
    });

    if (!state.snapshots[state.selectedMapId]) {
      syncHint.textContent =
        'This map\'s snapshot is not synced. Re-run sync_maps with map_id "' +
        state.selectedMapId + '" from the library view to load its board.';
      syncHint.hidden = false;
    }
  }

  function parseSnapshot(fields) {
    if (!fields || typeof fields.snapshot !== 'string') {
      return null;
    }
    try {
      var parsed = JSON.parse(fields.snapshot);
      if (parsed && typeof parsed === 'object' && parsed.nodes) {
        return parsed;
      }
      return null;
    } catch (error) {
      return null;
    }
  }

  function liveNodes(map) {
    var nodes = [];
    Object.keys(map.nodes).forEach(function (id) {
      var node = map.nodes[id];
      if (node && !node.tombstoned) {
        nodes.push(node);
      }
    });
    return nodes;
  }

  function liveEdges(map) {
    var edges = [];
    Object.keys(map.edges || {}).forEach(function (id) {
      var edge = map.edges[id];
      if (edge && !edge.tombstoned) {
        edges.push(edge);
      }
    });
    return edges;
  }

  // Positions: the map's own canvas coordinates when a node carries them;
  // otherwise a deterministic ring so unpositioned nodes stay visible.
  function positionsFor(nodes) {
    var positions = {};
    var radius = 150;
    var angle = 0;
    nodes.forEach(function (node) {
      if (
        node.position &&
        typeof node.position.x === 'number' &&
        typeof node.position.y === 'number'
      ) {
        positions[node.node_id] = { x: node.position.x, y: node.position.y, fixed: true };
      } else {
        positions[node.node_id] = {
          x: 480 + radius * Math.cos(angle),
          y: 300 + radius * Math.sin(angle),
          fixed: false
        };
        angle += (2 * Math.PI) / 12;
      }
    });
    return positions;
  }

  function renderBoard() {
    var board = window.document.getElementById('board');
    var title = window.document.getElementById('map-title');
    var revision = window.document.getElementById('map-revision');
    board.textContent = '';

    var fields = state.snapshots[state.selectedMapId];
    var map = parseSnapshot(fields);
    if (!map) {
      title.textContent = state.selectedMapId
        ? 'Snapshot not synced'
        : 'No map synced';
      revision.textContent = '';
      board.appendChild(
        el(
          'div',
          'boot-notice',
          state.selectedMapId
            ? 'This map\'s snapshot has not been synced into the entity store yet. Re-run sync_maps with its map_id, then refresh.'
            : 'Nothing to draw yet. Run the sync_maps action from the library view, then refresh here.'
        )
      );
      return;
    }

    title.textContent = map.title || map.map_id || state.selectedMapId;
    revision.textContent =
      map.lifecycle ? map.lifecycle + ' · revision ' + map.revision : 'revision ' + map.revision;

    var nodes = liveNodes(map);
    var edges = liveEdges(map);
    var positions = positionsFor(nodes);

    var minX = Infinity;
    var minY = Infinity;
    var maxX = -Infinity;
    var maxY = -Infinity;
    nodes.forEach(function (node) {
      var p = positions[node.node_id];
      minX = Math.min(minX, p.x);
      minY = Math.min(minY, p.y);
      maxX = Math.max(maxX, p.x);
      maxY = Math.max(maxY, p.y);
    });
    if (!nodes.length) {
      minX = 0;
      minY = 0;
      maxX = 320;
      maxY = 200;
    }

    var pad = 70;
    var width = Math.max(320, maxX - minX + 2 * pad);
    var height = Math.max(240, maxY - minY + 2 * pad);
    var offsetX = minX - pad;
    var offsetY = minY - pad;

    var svg = svgTag('svg');
    svg.setAttribute('width', String(width));
    svg.setAttribute('height', String(height));
    svg.setAttribute(
      'viewBox',
      offsetX + ' ' + offsetY + ' ' + width + ' ' + height
    );
    svg.setAttribute('role', 'img');
    svg.setAttribute(
      'aria-label',
      'Thinking map board for ' + (map.title || map.map_id || '')
    );

    edges.forEach(function (edge) {
      var from = positions[edge.from_node];
      var to = positions[edge.to_node];
      if (!from || !to) {
        return;
      }
      var line = svgTag('line');
      line.setAttribute('x1', String(from.x));
      line.setAttribute('y1', String(from.y));
      line.setAttribute('x2', String(to.x));
      line.setAttribute('y2', String(to.y));
      line.setAttribute('class', 'edge ' + (edge.kind || 'related_to'));
      svg.appendChild(line);
    });

    nodes.forEach(function (node) {
      var p = positions[node.node_id];
      var group = svgTag('g');
      group.setAttribute('class', 'node ' + (node.kind || 'idea'));

      var circle = svgTag('circle');
      circle.setAttribute('cx', String(p.x));
      circle.setAttribute('cy', String(p.y));
      circle.setAttribute('r', '26');
      if (node.node_id === state.selectedNodeId) {
        circle.setAttribute('class', 'selected');
      }
      circle.setAttribute('tabindex', '0');
      circle.setAttribute(
        'aria-label',
        (node.kind || 'node') + ': ' + (node.label || node.node_id || '')
      );
      circle.addEventListener('click', function () {
        state.selectedNodeId = node.node_id;
        renderBoard();
        renderDetail();
      });
      group.appendChild(circle);

      if (node.kind) {
        var kind = svgTag('text');
        kind.setAttribute('x', String(p.x));
        kind.setAttribute('y', String(p.y - 34));
        kind.setAttribute('class', 'kind');
        kind.textContent = node.kind;
        group.appendChild(kind);
      }

      var label = svgTag('text');
      label.setAttribute('x', String(p.x));
      label.setAttribute('y', String(p.y + 42));
      var text = node.label || node.node_id || '';
      if (text.length > 34) {
        text = text.slice(0, 33) + '…';
      }
      label.textContent = text;
      group.appendChild(label);

      svg.appendChild(group);
    });

    board.appendChild(svg);
  }

  function detailRow(list, term, value) {
    list.appendChild(el('dt', null, term));
    list.appendChild(el('dd', null, value === undefined || value === null ? '—' : value));
  }

  function renderDetail() {
    var empty = window.document.getElementById('detail-empty');
    var fields = window.document.getElementById('detail-fields');
    fields.textContent = '';
    var map = parseSnapshot(state.snapshots[state.selectedMapId]);
    var node = null;
    if (map && map.nodes && state.selectedNodeId) {
      var candidate = map.nodes[state.selectedNodeId];
      if (candidate && !candidate.tombstoned) {
        node = candidate;
      }
    }
    if (!node) {
      empty.hidden = false;
      fields.hidden = true;
      return;
    }
    empty.hidden = true;
    fields.hidden = false;
    detailRow(fields, 'Label', node.label);
    detailRow(fields, 'Kind', node.kind);
    detailRow(fields, 'State', node.epistemic_state);
    if (typeof node.confidence === 'number') {
      detailRow(fields, 'Confidence', node.confidence.toFixed(2));
    }
    detailRow(fields, 'Origin', node.assertion_origin);
    if (node.detail_markdown) {
      detailRow(fields, 'Detail', node.detail_markdown.split('\n')[0]);
    }
    detailRow(fields, 'Updated', node.updated_at);
  }

  function renderAll() {
    state.selectedMapId = defaultMapId();
    renderLibrary();
    renderBoard();
    renderDetail();
  }

  // ── Boot ─────────────────────────────────────────────────────────────────

  function removeBootNotice() {
    var notice = window.document.getElementById('boot-notice');
    if (notice && notice.parentNode) {
      notice.parentNode.removeChild(notice);
    }
  }

  function loadAndRender(isRefresh) {
    return loadLibrary()
      .then(loadSnapshots)
      .then(function () {
        renderAll();
        var refresh = window.document.getElementById('refresh');
        refresh.hidden = false;
        if (isRefresh) {
          setStatus(
            'Library refreshed: ' +
              state.summaries.length +
              ' map summary record(s), ' +
              Object.keys(state.snapshots).length +
              ' synced snapshot(s).'
          );
        } else {
          setStatus(
            state.summaries.length
              ? 'Loaded ' + state.summaries.length + ' map summary record(s).'
              : 'No map records yet — run sync_maps from the library view, then refresh.'
          );
        }
      })
      .catch(function (error) {
        renderAll();
        setStatus('Bridge read failed: ' + (error && error.message ? error.message : error), true);
      });
  }

  function boot() {
    // The boot notice stays until the bridge answers — SKILL.md and the
    // HTML describe it as the closed fallback when the kernel refuses the
    // sibling or the bridge, so it must survive a bridge refusal.
    state.installationId = installationIdFromLocation();
    if (!state.installationId) {
      setStatus('This document is not mounted at a scripted-surface asset address.', true);
      return;
    }
    callBridge('contract_capabilities', {})
      .then(function () {
      removeBootNotice();
        return loadAndRender(false);
      })
      .catch(function (error) {
        setStatus(
          'Bridge unavailable: ' + (error && error.message ? error.message : error),
          true
        );
      });

    window.document.getElementById('refresh').addEventListener('click', function () {
      loadAndRender(true);
    });
  }

  if (window.document.readyState === 'loading') {
    window.document.addEventListener('DOMContentLoaded', boot);
  } else {
    boot();
  }
})();
