/*
 * C4 Architecture Canvas — view logic.
 *
 * Loads the merged c4.json (curated architecture + code ladder) and renders
 * an infinite zoomable canvas:
 *   band "runtime" — the human runtime architecture (processes, clients,
 *                    externals, actors, connections)
 *   band "areas"   — feature areas inside systems (zoom into the magician)
 *   band "code"    — the tiered implementation map (crates -> modules)
 *   band "detail"  — lazy files/symbols via /api/c4/slice, falling back to
 *                    deep links into the 2D explorer when unserved.
 *
 * Never fetches graph.json (436 MB); c4.json is the only required artifact.
 */
(function () {
  "use strict";

  var C4Core = window.C4Core;

  // ---- Layout constants (world units) ----------------------------------------
  //
  // Band-0 geometry is lane-organized for the C4 composition:
  //   actors (top-left) · clients column · supervised stack boundary
  //   (center) · peer service (below) · externals column (right).

  var RUNTIME_LAYOUT = {
    "actor:user": { x: -3260, y: -1660, w: 340, h: 200 },
    "actor:operator": { x: -2860, y: -1660, w: 340, h: 200 },
    "runtime:web": { x: -2480, y: -1560, w: 480, h: 200 },
    "runtime:desktop": { x: -2480, y: -1310, w: 480, h: 220 },
    "runtime:ios": { x: -2480, y: -1040, w: 480, h: 200 },
    "runtime:android": { x: -2480, y: -790, w: 480, h: 200 },
    "runtime:esp32": { x: -2480, y: -540, w: 480, h: 180 },
    "runtime:bots": { x: -2480, y: -310, w: 480, h: 200 },
    "runtime:extension": { x: -2480, y: -60, w: 480, h: 180 },
    "runtime:supervisor": { x: -1140, y: -1520, w: 700, h: 280 },
    "runtime:magician": { x: -1140, y: -1180, w: 1780, h: 2120 },
    "runtime:magicutor": { x: 720, y: -1180, w: 600, h: 290 },
    "runtime:fluidaudio": { x: 720, y: -830, w: 600, h: 180 },
    "runtime:silverbullet": { x: -1140, y: 1220, w: 600, h: 180 },
    "external:llm-providers": { x: 1900, y: -1560, w: 420, h: 150 },
    "external:ollama": { x: 1900, y: -1370, w: 420, h: 130 },
    "external:cloudflare": { x: 1900, y: -1180, w: 420, h: 130 },
    "external:gmail": { x: 1900, y: -990, w: 420, h: 120 },
    "external:google-calendar": { x: 1900, y: -820, w: 420, h: 120 },
    "external:telegram": { x: 1900, y: -650, w: 420, h: 120 },
    "external:whatsapp-cloud": { x: 1900, y: -480, w: 420, h: 120 },
    "external:apple-reminders": { x: 1900, y: -310, w: 420, h: 120 },
    "external:container-runtime": { x: 1900, y: -140, w: 420, h: 130 },
  };

  // Lanes: section headers and the supervised-stack boundary.
  var LANES = [
    { label: "People", x: -3260, y: -1740, w: 740 },
    { label: "Clients & Surfaces", x: -2480, y: -1660, w: 480 },
    { label: "External Systems", x: 1900, y: -1660, w: 420 },
  ];
  var STACK_BOUNDARY = { x: -1280, y: -1600, w: 2740, h: 2860, label: "Magician Runtime", sub: "supervised local stack" };

  // Per-node glyphs for the human-readable diagram.
  var NODE_ICONS = {
    "actor:user": "👤", "actor:operator": "🛠️",
    "runtime:web": "🌐", "runtime:desktop": "🖥️", "runtime:ios": "📱",
    "runtime:android": "🤖", "runtime:esp32": "🎧", "runtime:bots": "💬",
    "runtime:extension": "🧩", "runtime:supervisor": "🛡️", "runtime:magician": "✨",
    "runtime:magicutor": "🔭", "runtime:fluidaudio": "🎚️", "runtime:silverbullet": "📝",
    "external:llm-providers": "🧠", "external:ollama": "🦙", "external:cloudflare": "☁️",
    "external:gmail": "✉️", "external:google-calendar": "📅", "external:telegram": "✈️",
    "external:whatsapp-cloud": "🟢", "external:apple-reminders": "☑️", "external:container-runtime": "📦",
    "area:agentic-loop": "🔁", "area:delegation-agents": "🧑‍🚀", "area:tasks-artifacts": "🗂️",
    "area:chat-lane": "💬", "area:skills": "🧩", "area:resource-authority": "🏛️",
    "area:memory": "🧲", "area:comms-channels": "📡", "area:attention-hitl": "🔔",
    "area:app-platform": "🧱", "area:device-bridge": "🔗", "area:voice-media": "🎙️",
    "area:coding-engines": "⌨️", "area:llm-dispatch": "🚦", "area:observation": "👁️",
    "area:identity-auth": "🔐",
  };

  // Edge color categories: control (amber), data (blue), ai (violet),
  // external (slate), user (teal).
  var EDGE_CATEGORY = {
    supervises: "ctrl", manages: "ctrl", operates: "ctrl", proxies: "ctrl",
    hosts: "ctrl", owns: "ctrl", peers: "ctrl",
    consumes: "data", invokes: "data", bridges: "data", tunnels: "data",
    attaches: "data", posts: "data", opens: "data", probes: "data", calls: "data",
    dispatches: "ai", embeds: "ai",
    ingests: "ext", exchanges: "ext", creates: "ext",
    uses: "user",
  };

  // Feature-area grid inside the magician box.
  var AREA_GRID = { x: -1100, y: -1090, cols: 3, cellW: 560, cellH: 316, gap: 20 };

  // Implementation ladder lives far below the runtime layer.
  var LADDER_ORIGIN = { x: -2000, y: 2000, rowStep: 340, baseW: 6400 };
  var CRATE = { w: 330, h: 170, gapX: 26, gapY: 120, maxRowW: 6400 };
  var MODULE = { cols: 5, cellW: 170, cellH: 92, gap: 12 };

  var CULL_MARGIN = 600;

  // Edge-layer geometry: a group translated by SVG_ORIGIN lets path data
  // stay in world coordinates while the SVG element covers the whole map.
  var SVG_ORIGIN = { x: -5000, y: -4000 };
  var SVG_SIZE = { w: 10000, h: 9000 };

  // ---- State -------------------------------------------------------------------

  var state = {
    data: null,
    cam: { x: 0, y: -200, z: 0.06 },
    sel: "",
    selEdge: null,
    exp: new Set(),
    boxes: new Map(),       // id -> {id, kind, label, x, y, w, h, el, parentId, childrenBand}
    edgeEls: new Map(),     // edgeIndex -> {path, arrow, group}
    searchIndex: [],
    animHandle: 0,
    urlTimer: 0,
    framePending: false,
    drag: null,
  };

  // ---- Small helpers ------------------------------------------------------------

  function el(id) {
    return document.getElementById(id);
  }

  function viewportSize() {
    var vp = el("c4-viewport");
    return { w: vp.clientWidth, h: vp.clientHeight };
  }

  function viewportWorldRect() {
    var view = viewportSize();
    var cam = state.cam;
    var halfW = view.w / 2 / cam.z;
    var halfH = view.h / 2 / cam.z;
    return { x: cam.x - halfW, y: cam.y - halfH, w: halfW * 2, h: halfH * 2 };
  }

  var activeBand = null;
  function setBandBadge(band) {
    var chip = el("c4-band");
    if (chip) {
      chip.textContent = band;
    }
    document.body.dataset.band = band;
    if (band !== activeBand) {
      activeBand = band;
      // Band CSS changes edge-label font sizes; re-measure the chips.
      if (state.data) {
        redrawEdges();
      }
    }
  }

  function applyCamera() {
    var cam = state.cam;
    var view = viewportSize();
    var tx = view.w / 2 - cam.x * cam.z;
    var ty = view.h / 2 - cam.y * cam.z;
    el("c4-world").style.transform =
      "translate(" + tx + "px," + ty + "px) scale(" + cam.z + ")";
    // The dot grid lives on the viewport and tracks the camera so it reads
    // as one continuous infinite surface. Clamped so extreme zooms never
    // produce a moiré (too dense) or an empty void (too sparse).
    var vp = el("c4-viewport");
    var cell = Math.min(160, Math.max(16, 28 * cam.z));
    vp.style.backgroundSize = cell + "px " + cell + "px";
    vp.style.backgroundPosition = tx + "px " + ty + "px";
    setBandBadge(C4Core.bandForScale(cam.z));
    scheduleCulling();
    drawMinimap();
    scheduleUrlSync();
  }

  function scheduleUrlSync() {
    clearTimeout(state.urlTimer);
    state.urlTimer = setTimeout(function () {
      var query = C4Core.encodeUrlState({
        x: state.cam.x,
        y: state.cam.y,
        z: state.cam.z,
        sel: state.sel,
        exp: Array.from(state.exp),
      });
      history.replaceState(null, "", "?" + query);
    }, 250);
  }

  // ---- Box management -------------------------------------------------------------

  function registerBox(def) {
    state.boxes.set(def.id, def);
  }

  // Modules live inside their crate element with crate-relative coords;
  // everything else uses world coords directly.
  function worldRect(def) {
    if (def.container) {
      return { x: def.container.x + def.x, y: def.container.y + def.y, w: def.w, h: def.h };
    }
    return def;
  }

  function boxElement(def) {
    if (def.el) {
      return def.el;
    }
    var node = document.createElement("div");
    node.className = "c4-box";
    node.dataset.kind = def.kind;
    node.dataset.id = def.id;
    node.style.left = def.x + "px";
    node.style.top = def.y + "px";
    node.style.width = def.w + "px";
    node.style.height = def.h + "px";

    var head = document.createElement("div");
    head.className = "c4-box-head";
    if (def.icon) {
      var icon = document.createElement("span");
      icon.className = "c4-box-icon";
      icon.textContent = def.icon;
      head.appendChild(icon);
    }
    var label = document.createElement("div");
    label.className = "c4-box-label";
    label.textContent = def.label;
    head.appendChild(label);
    if (def.port) {
      var port = document.createElement("span");
      port.className = "c4-box-port";
      port.textContent = ":" + def.port;
      head.appendChild(port);
    }
    node.appendChild(head);

    if (def.summary) {
      var summary = document.createElement("div");
      summary.className = "c4-box-summary";
      summary.textContent = def.summary;
      node.appendChild(summary);
    }
    if (def.stats) {
      var stats = document.createElement("div");
      stats.className = "c4-box-stats";
      stats.textContent = statsLine(def.stats, def.kind);
      node.appendChild(stats);
    }
    if (def.expandable) {
      var hint = document.createElement("span");
      hint.className = "c4-expand-hint";
      hint.textContent = " dbl-click to expand";
      node.appendChild(hint);
    }
    if (def.chips) {
      var row = document.createElement("div");
      row.className = "c4-chip-row";
      def.chips.forEach(function (chip) {
        var chipEl = document.createElement("span");
        chipEl.className = "c4-chip";
        chipEl.textContent = chip.label;
        chipEl.title = chip.title || chip.label;
        chipEl.dataset.flyRef = chip.flyRef || "";
        row.appendChild(chipEl);
      });
      node.appendChild(row);
    }

    el("c4-world").appendChild(node);

    node.addEventListener("mouseenter", function () {
      if (!state.drag) applyIsolation(def.id, null);
    });
    node.addEventListener("mouseleave", function () {
      applyIsolation(null, null);
    });

    def.el = node;
    return node;
  }

  function statsLine(stats, kind) {
    if (kind === "crate") {
      return (
        stats.symbols.toLocaleString() + " symbols · " +
        stats.modules.toLocaleString() + " modules" +
        (stats.endpoints ? " · " + stats.endpoints + " endpoints" : "")
      );
    }
    if (kind === "module") {
      return stats.symbols.toLocaleString() + " symbols";
    }
    var parts = [];
    if (stats.symbols) {
      parts.push(stats.symbols.toLocaleString() + " symbols");
    }
    if (stats.modules) {
      parts.push(stats.modules.toLocaleString() + " modules");
    }
    if (stats.endpoints) {
      parts.push(stats.endpoints + " endpoints");
    }
    return parts.join(" · ");
  }

  function updateBoxChildrenVisibility() {
    state.boxes.forEach(function (def) {
      if (!def.el) {
        return;
      }
      var visibleByExpansion = !def.parentId || state.exp.has(def.parentId);
      if (visibleByExpansion) {
        def.el.classList.remove("is-hidden-exp");
      } else {
        def.el.classList.add("is-hidden-exp");
      }
    });
    scheduleCulling();
  }

  // ---- Runtime layer (band 0/1) ------------------------------------------------------

  function renderCuratedLayer() {
    var data = state.data;
    var byId = {};
    data.nodes.forEach(function (n) { byId[n.id] = n; });
    data.externals.forEach(function (n) { byId[n.id] = n; });
    data.actors.forEach(function (n) { byId[n.id] = n; });

    renderSceneFurniture();

    Object.keys(RUNTIME_LAYOUT).forEach(function (id) {
      var node = byId[id];
      if (!node) {
        return;
      }
      var rect = RUNTIME_LAYOUT[id];
      var kind = id.split(":")[0];
      registerBox({
        id: id,
        kind: kind === "external" ? "external" : kind === "actor" ? "actor" : "runtime",
        label: node.label,
        summary: node.summary,
        icon: NODE_ICONS[id] || null,
        port: node.runs && node.runs.port ? String(node.runs.port) : null,
        expandable: id === "runtime:magician",
        x: rect.x, y: rect.y, w: rect.w, h: rect.h,
      });
      boxElement(state.boxes.get(id));
    });

    // Feature areas inside the magician box.
    var areas = data.nodes.filter(function (n) {
      return n.id.indexOf("area:") === 0 && n.parent === "runtime:magician";
    });
    areas.forEach(function (area, index) {
      var col = index % AREA_GRID.cols;
      var row = Math.floor(index / AREA_GRID.cols);
      var x = AREA_GRID.x + col * (AREA_GRID.cellW + AREA_GRID.gap);
      var y = AREA_GRID.y + row * (AREA_GRID.cellH + AREA_GRID.gap);
      var chips = (area.code_refs_resolved || []).slice(0, 4).map(function (ref) {
        return {
          label: ref.label.length > 26 ? ref.label.slice(0, 24) + "…" : ref.label,
          title: ref.id,
          flyRef: ref.id,
        };
      });
      registerBox({
        id: area.id,
        kind: "area",
        label: area.label,
        summary: area.summary,
        icon: NODE_ICONS[area.id] || null,
        stats: area.stats,
        parentId: "runtime:magician",
        chips: chips,
        x: x, y: y, w: AREA_GRID.cellW, h: AREA_GRID.cellH,
      });
      boxElement(state.boxes.get(area.id));
    });
  }

  // Lane headers and the supervised-stack boundary — the diagram furniture
  // that makes band 0 read as a composed architecture view.
  function renderSceneFurniture() {
    var world = el("c4-world");

    var boundary = document.createElement("div");
    boundary.className = "c4-boundary";
    boundary.style.left = STACK_BOUNDARY.x + "px";
    boundary.style.top = STACK_BOUNDARY.y + "px";
    boundary.style.width = STACK_BOUNDARY.w + "px";
    boundary.style.height = STACK_BOUNDARY.h + "px";
    var boundaryLabel = document.createElement("div");
    boundaryLabel.className = "c4-boundary-label";
    boundaryLabel.textContent = STACK_BOUNDARY.label;
    var boundarySub = document.createElement("div");
    boundarySub.className = "c4-boundary-sub";
    boundarySub.textContent = STACK_BOUNDARY.sub;
    boundary.appendChild(boundaryLabel);
    boundary.appendChild(boundarySub);
    world.appendChild(boundary);

    LANES.forEach(function (lane) {
      var label = document.createElement("div");
      label.className = "c4-lane-label";
      label.textContent = lane.label;
      label.style.left = lane.x + "px";
      label.style.top = lane.y + "px";
      label.style.width = lane.w + "px";
      world.appendChild(label);
    });
  }

  // ---- Implementation ladder (band 2) --------------------------------------------------

  function renderLadder() {
    var data = state.data;
    var ladder = document.createElement("div");
    ladder.className = "c4-ladder";
    ladder.id = "c4-ladder";
    el("c4-world").appendChild(ladder);

    // Section title: keeps the implementation map discoverable as a
    // distinct layer when it is ghosted at the overview band.
    var title = document.createElement("div");
    title.className = "c4-ladder-title";
    title.textContent = "Implementation Map — tiers → crates → modules · zoom in to browse";
    title.style.left = LADDER_ORIGIN.x + "px";
    title.style.top = (LADDER_ORIGIN.y - 170) + "px";
    title.style.width = LADDER_ORIGIN.baseW + "px";
    ladder.appendChild(title);

    data.code.tiers.forEach(function (tier, tierIndex) {
      var tierRow = document.createElement("div");
      tierRow.className = "c4-box";
      tierRow.dataset.kind = "tier";
      tierRow.dataset.id = "tier:" + tier.id;
      tierRow.style.left = LADDER_ORIGIN.x + "px";
      tierRow.style.width = LADDER_ORIGIN.baseW + "px";
      var tierLabel = document.createElement("div");
      tierLabel.className = "c4-box-label";
      tierLabel.textContent = tier.label + " — " + tier.crates.length + " crates";
      tierRow.appendChild(tierLabel);
      ladder.appendChild(tierRow);
      // Tier rows are registered so layoutCrates can reposition them when
      // an expanded crate forces its tier (and the ones below) taller.
      registerBox({
        id: "tier:" + tier.id,
        kind: "tier",
        label: tier.label,
        x: LADDER_ORIGIN.x,
        y: LADDER_ORIGIN.y + tierIndex * LADDER_ORIGIN.rowStep,
        w: LADDER_ORIGIN.baseW,
        h: LADDER_ORIGIN.rowStep - 40,
        el: tierRow,
      });

      tier.crates.forEach(function (crate, crateIndex) {
        var def = {
          id: crate.id,
          kind: "crate",
          label: crate.label,
          summary: crate.description,
          stats: crate,
          tech: crate.tech,
          deps: crate.deps,
          modules: crate.modules_list || [],
          tierIndex: tierIndex,
          crateIndex: crateIndex,
          expandable: crate.modules_list && crate.modules_list.length > 0,
          x: 0, y: 0, w: CRATE.w, h: CRATE.h,
        };
        registerBox(def);
        boxElement(def);
        ladder.appendChild(def.el);
      });
    });

    layoutCrates();
  }

  // Adaptive grid shape: big module counts grow wider (square-ish) instead
  // of an unbounded single column stack, capped so one crate never eats
  // the whole canvas.
  function crateGridCols(count) {
    return Math.max(3, Math.min(12, Math.ceil(Math.sqrt(Math.max(1, count) * 1.5))));
  }

  function layoutCrates() {
    var tierTop = LADDER_ORIGIN.y;
    state.data.code.tiers.forEach(function (tier) {
      var tierDef = state.boxes.get("tier:" + tier.id);
      var rowY = tierTop + 56;
      var cursorX = LADDER_ORIGIN.x + 20;
      var rowH = 0;
      tier.crates.forEach(function (crate) {
        var def = state.boxes.get(crate.id);
        if (!def || !def.el) {
          return;
        }
        var w = state.exp.has(crate.id) ? expandedCrateWidth(def) : CRATE.w;
        var h = state.exp.has(crate.id) ? expandedCrateHeight(def) : CRATE.h;
        if (cursorX + w > LADDER_ORIGIN.x + LADDER_ORIGIN.baseW - 20) {
          cursorX = LADDER_ORIGIN.x + 20;
          rowY += rowH + CRATE.gapY;
          rowH = 0;
        }
        def.x = cursorX;
        def.y = rowY;
        def.w = w;
        def.h = h;
        def.el.style.left = def.x + "px";
        def.el.style.top = def.y + "px";
        def.el.style.width = def.w + "px";
        def.el.style.height = def.h + "px";
        cursorX += w + CRATE.gapX;
        rowH = Math.max(rowH, h);
      });
      var tierHeight = Math.max(LADDER_ORIGIN.rowStep, (rowY + rowH) - tierTop + 20);
      if (tierDef && tierDef.el) {
        tierDef.y = tierTop;
        tierDef.h = tierHeight - 40;
        tierDef.el.style.top = tierDef.y + "px";
        tierDef.el.style.height = tierDef.h + "px";
      }
      tierTop += tierHeight + 60;
    });
    computeBounds();
    scheduleCulling();
    drawMinimap();
  }

  function expandedCrateWidth(def) {
    var cols = crateGridCols(def.modules.length);
    return cols * (MODULE.cellW + MODULE.gap) + 24;
  }

  function expandedCrateHeight(def) {
    var cols = crateGridCols(def.modules.length);
    var rows = Math.ceil(def.modules.length / cols);
    return 60 + rows * (MODULE.cellH + MODULE.gap) + 16;
  }

  function renderModules(crateId) {
    var def = state.boxes.get(crateId);
    if (!def || def.moduleEls) {
      return;
    }
    def.moduleEls = [];
    var cols = crateGridCols(def.modules.length);
    def.modules.forEach(function (module, index) {
      var col = index % cols;
      var row = Math.floor(index / cols);
      var moduleDef = {
        id: module.id,
        kind: "module",
        label: module.label.length > 24 ? module.label.slice(0, 22) + "…" : module.label,
        summary: null,
        stats: { symbols: module.symbols },
        parentId: crateId,
        container: def,
        sliceTarget: { crate: crateId.replace("crate::", ""), moduleId: module.id },
        x: 12 + col * (MODULE.cellW + MODULE.gap),
        y: 60 + row * (MODULE.cellH + MODULE.gap),
        w: MODULE.cellW,
        h: MODULE.cellH,
      };
      registerBox(moduleDef);
      var node = boxElement(moduleDef);
      def.el.appendChild(node);
      def.moduleEls.push(moduleDef);
    });
    ensureSliceRow(def);
  }

  function unmountModules(crateId) {
    var def = state.boxes.get(crateId);
    if (!def || !def.moduleEls) {
      return;
    }
    def.moduleEls.forEach(function (moduleDef) {
      if (moduleDef.el && moduleDef.el.parentNode) {
        moduleDef.el.parentNode.removeChild(moduleDef.el);
      }
      state.boxes.delete(moduleDef.id);
    });
    if (def.sliceRow && def.sliceRow.parentNode) {
      def.sliceRow.parentNode.removeChild(def.sliceRow);
    }
    def.sliceRow = null;
    def.moduleEls = null;
    state.sliceLoaded && state.sliceLoaded.delete(crateId);
  }

  // ---- Lazy detail band (slice API / explorer fallback) ---------------------------------

  function ensureSliceRow(crateDef) {
    // Placeholder row; populated by loadSlices when the band allows.
  }

  function loadSlices(crateDef) {
    if (!crateDef.moduleEls || (state.sliceLoaded && state.sliceLoaded.has(crateDef.id))) {
      return;
    }
    if (!state.sliceLoaded) {
      state.sliceLoaded = new Set();
    }
    state.sliceLoaded.add(crateDef.id);
    crateDef.moduleEls.forEach(function (moduleDef) {
      var target = moduleDef.sliceTarget;
      if (!target) {
        return;
      }
      fetch(
        "/api/c4/slice?crate=" + encodeURIComponent(target.crate) +
        "&parent=" + encodeURIComponent(target.moduleId) +
        "&depth=1"
      )
        .then(function (res) {
          if (!res.ok) {
            throw new Error("slice api " + res.status);
          }
          return res.json();
        })
        .then(function (payload) {
          renderSliceChips(moduleDef, payload, false);
        })
        .catch(function () {
          // Static mode (no dev server): offer explorer deep links instead.
          renderSliceChips(moduleDef, null, true);
        });
    });
  }

  function renderSliceChips(moduleDef, payload, fallback) {
    if (!moduleDef.el || moduleDef.sliceRendered) {
      return;
    }
    moduleDef.sliceRendered = true;
    var row = document.createElement("div");
    row.className = "c4-slice-row";
    // Labels are stripped of the glyph prefix and truncation ellipsis so
    // the explorer's search box gets a clean query.
    var chipLabel = function (text) {
      return String(text || "").replace(/^[^A-Za-z0-9_]+/, "").replace(/…$/, "").trim();
    };
    var moduleLabel = chipLabel(moduleDef.label);
    var items = [];
    if (fallback) {
      items.push({
        text: "Open in Explorer ↗",
        href: explorerLink(moduleDef.id, moduleLabel, "module"),
      });
    } else {
      (payload.files || []).slice(0, 3).forEach(function (file) {
        items.push({ text: "📄 " + file.label, href: explorerLink(file.id, file.label, "file") });
      });
      (payload.symbols || []).slice(0, 5).forEach(function (symbol) {
        items.push({ text: symbol.label, href: explorerLink(symbol.id, symbol.label, "symbol") });
      });
      if (payload.truncated) {
        items.push({ text: "+" + payload.truncated + " more…", href: explorerLink(moduleDef.id, moduleLabel, "module") });
      }
      if (!items.length) {
        items.push({ text: "Open in Explorer ↗", href: explorerLink(moduleDef.id, moduleLabel, "module") });
      }
    }
    items.forEach(function (item) {
      var link = document.createElement("a");
      link.className = "c4-slice-chip";
      link.textContent = item.text;
      link.href = item.href;
      link.target = "_blank";
      link.rel = "noopener";
      link.addEventListener("click", function (event) {
        event.stopPropagation();
      });
      row.appendChild(link);
    });
    moduleDef.el.appendChild(row);
  }

  // Explorer deep link: selection takes the node id, `xm` takes the same
  // id for modules so the module actually expands (it is an id-list param,
  // not a flag), and `q` seeds the search box so the target is visible
  // even when progressive expansion would otherwise leave it offscreen.
  function explorerLink(graphId, label, kind) {
    var params = ["sel=" + encodeURIComponent(graphId)];
    if (kind === "module") {
      params.push("xm=" + encodeURIComponent(graphId));
    }
    if (label) {
      params.push("q=" + encodeURIComponent(label));
    }
    return "explorer.html?" + params.join("&");
  }

  // ---- Edges -----------------------------------------------------------------------------

  function renderEdges() {
    var svg = el("c4-edge-svg");
    // The SVG must physically cover the whole world: Chromium clips
    // hit-testing to the SVG viewport, so a tiny SVG makes edges and
    // labels unclickable even with overflow:visible.
    svg.style.left = SVG_ORIGIN.x + "px";
    svg.style.top = SVG_ORIGIN.y + "px";
    svg.setAttribute("width", String(SVG_SIZE.w));
    svg.setAttribute("height", String(SVG_SIZE.h));

    var defs = document.createElementNS("http://www.w3.org/2000/svg", "defs");
    ["ctrl", "data", "ai", "ext", "user"].forEach(function (category) {
      var marker = document.createElementNS("http://www.w3.org/2000/svg", "marker");
      marker.setAttribute("id", "c4-arrow-" + category);
      marker.setAttribute("viewBox", "0 0 10 10");
      marker.setAttribute("refX", "9");
      marker.setAttribute("refY", "5");
      marker.setAttribute("markerWidth", "7");
      marker.setAttribute("markerHeight", "7");
      marker.setAttribute("orient", "auto-start-reverse");
      var path = document.createElementNS("http://www.w3.org/2000/svg", "path");
      path.setAttribute("d", "M 0 0 L 10 5 L 0 10 z");
      path.setAttribute("class", "c4-edge-arrow " + category);
      marker.appendChild(path);
      defs.appendChild(marker);
    });
    svg.appendChild(defs);

    var group = document.createElementNS("http://www.w3.org/2000/svg", "g");
    group.setAttribute("transform", "translate(" + -SVG_ORIGIN.x + "," + -SVG_ORIGIN.y + ")");
    svg.appendChild(group);

    state.data.edges.forEach(function (edge, index) {
      var fromDef = state.boxes.get(edge.from);
      var toDef = state.boxes.get(edge.to);
      if (!fromDef || !toDef) {
        return;
      }
      var category = EDGE_CATEGORY[edge.kind] || "data";
      var line = document.createElementNS("http://www.w3.org/2000/svg", "path");
      line.setAttribute("class", "c4-edge-line " + category + (edge.trigger ? " triggered" : ""));
      line.setAttribute("marker-end", "url(#c4-arrow-" + category + ")");
      group.appendChild(line);

      var chip = document.createElementNS("http://www.w3.org/2000/svg", "rect");
      chip.setAttribute("class", "c4-edge-chip");
      group.appendChild(chip);

      var label = document.createElementNS("http://www.w3.org/2000/svg", "text");
      label.setAttribute("class", "c4-edge-label");
      label.setAttribute("text-anchor", "middle");
      label.textContent = edge.kind + (edge.transport ? " · " + edge.transport : "");
      group.appendChild(label);

      // Edge selection is resolved on viewport pointerup (see
      // resolvePointerHit): click/dblclick retargeting under pointer
      // capture is unreliable across browsers. Hover stays native.
      line.dataset.edgeIndex = String(index);
      label.dataset.edgeIndex = String(index);
      [line, label].forEach(function (target) {
        target.addEventListener("mouseenter", function () {
          line.classList.add("hovered");
          applyIsolation(null, index);
        });
        target.addEventListener("mouseleave", function () {
          line.classList.remove("hovered");
          applyIsolation(null, null);
        });
      });

      state.edgeEls.set(index, { line: line, chip: chip, label: label, edge: edge, from: edge.from, to: edge.to });
    });

    redrawEdges();
  }

  // Anchor edges on box borders (not centers) so lines do not run under
  // the boxes they connect. Returns [x, y] on rect's border along the ray
  // from rect center toward the other center.
  function borderAnchor(rect, other) {
    var cx = rect.x + rect.w / 2;
    var cy = rect.y + rect.h / 2;
    var ox = other.x + other.w / 2;
    var oy = other.y + other.h / 2;
    var dx = ox - cx;
    var dy = oy - cy;
    if (dx === 0 && dy === 0) {
      return [cx, cy];
    }
    var t = Infinity;
    if (dx !== 0) {
      t = Math.min(t, (dx > 0 ? rect.w / 2 : -rect.w / 2) / dx);
    }
    if (dy !== 0) {
      t = Math.min(t, (dy > 0 ? rect.h / 2 : -rect.h / 2) / dy);
    }
    return [cx + dx * t, cy + dy * t];
  }

  function redrawEdges() {
    state.edgeEls.forEach(function (entry) {
      var fromDef = state.boxes.get(entry.from);
      var toDef = state.boxes.get(entry.to);
      if (!fromDef || !toDef) {
        return;
      }
      var a = borderAnchor(fromDef, toDef);
      var b = borderAnchor(toDef, fromDef);
      var x1 = a[0], y1 = a[1];
      var x2 = b[0], y2 = b[1];
      var mx = (x1 + x2) / 2;
      var my = (y1 + y2) / 2;

      // Orthogonal (taxi-cab) routing
      var d = "";
      var r = 16; // Corner radius threshold

      if (Math.abs(x2 - x1) > Math.abs(y2 - y1)) {
        // Horizontal dominance
        var rX = (x2 > x1 ? r : -r);
        var rY = (y2 > y1 ? r : -r);
        if (Math.abs(x2 - x1) < r*2 || Math.abs(y2 - y1) < r*2) {
          d = "M " + x1 + " " + y1 + " L " + mx + " " + y1 + " L " + mx + " " + y2 + " L " + x2 + " " + y2;
        } else {
          d = "M " + x1 + " " + y1 +
              " L " + (mx - rX) + " " + y1 +
              " Q " + mx + " " + y1 + " " + mx + " " + (y1 + rY) +
              " L " + mx + " " + (y2 - rY) +
              " Q " + mx + " " + y2 + " " + (mx + rX) + " " + y2 +
              " L " + x2 + " " + y2;
        }
      } else {
        // Vertical dominance
        var rX = (x2 > x1 ? r : -r);
        var rY = (y2 > y1 ? r : -r);
        if (Math.abs(x2 - x1) < r*2 || Math.abs(y2 - y1) < r*2) {
          d = "M " + x1 + " " + y1 + " L " + x1 + " " + my + " L " + x2 + " " + my + " L " + x2 + " " + y2;
        } else {
          d = "M " + x1 + " " + y1 +
              " L " + x1 + " " + (my - rY) +
              " Q " + x1 + " " + my + " " + (x1 + rX) + " " + my +
              " L " + (x2 - rX) + " " + my +
              " Q " + x2 + " " + my + " " + x2 + " " + (my + rY) +
              " L " + x2 + " " + y2;
        }
      }
      entry.line.setAttribute("d", d);
      entry.label.setAttribute("x", mx);
      entry.label.setAttribute("y", my - 8);
      // Size the label chip from the measured text box.
      try {
        var bb = entry.label.getBBox();
        var padX = 10;
        var padY = 5;
        entry.chip.setAttribute("x", bb.x - padX);
        entry.chip.setAttribute("y", bb.y - padY);
        entry.chip.setAttribute("width", bb.width + padX * 2);
        entry.chip.setAttribute("height", bb.height + padY * 2);
        entry.chip.setAttribute("rx", 6);
      } catch (_err) { /* getBBox before layout — chip omitted */ }
    });
  }

  // ---- Selection / inspector ----------------------------------------------------------------

  function nodeById(id) {
    if (!state.data) {
      return null;
    }
    var pools = [state.data.nodes, state.data.externals, state.data.actors];
    for (var i = 0; i < pools.length; i++) {
      for (var j = 0; j < pools[i].length; j++) {
        if (pools[i][j].id === id) {
          return pools[i][j];
        }
      }
    }
    return null;
  }

  function crateById(id) {
    if (!state.data) {
      return null;
    }
    var tiers = state.data.code.tiers;
    for (var i = 0; i < tiers.length; i++) {
      for (var j = 0; j < tiers[i].crates.length; j++) {
        if (tiers[i].crates[j].id === id) {
          return tiers[i].crates[j];
        };
      }
    }
    return null;
  }

  function select(id) {
    state.sel = id;
    state.selEdge = null;
    state.boxes.forEach(function (def) {
      if (def.el) {
        def.el.classList.toggle("selected", def.id === id);
      }
    });
    state.edgeEls.forEach(function (entry) {
      entry.line.classList.remove("selected");
    });
    renderInspectorForNode(id);
    renderBreadcrumbs(id);
    scheduleUrlSync();
  }

  function selectEdge(index) {
    var entry = state.edgeEls.get(index);
    if (!entry) {
      return;
    }
    state.sel = "";
    state.selEdge = index;
    state.boxes.forEach(function (def) {
      if (def.el) {
        def.el.classList.remove("selected");
      }
    });
    state.edgeEls.forEach(function (other, otherIndex) {
      other.line.classList.toggle("selected", otherIndex === index);
    });
    renderInspectorForEdge(entry.edge);
    renderBreadcrumbs(null);
    scheduleUrlSync();
  }

  function clearSelection() {
    state.sel = "";
    state.selEdge = null;
    state.boxes.forEach(function (def) {
      if (def.el) {
        def.el.classList.remove("selected");
      }
    });
    state.edgeEls.forEach(function (entry) {
      entry.line.classList.remove("selected");
    });
    el("c4-inspector").hidden = true;
    renderBreadcrumbs(null);
    scheduleUrlSync();
  }

  function factRow(dl, term, value) {
    if (!value) {
      return;
    }
    var dt = document.createElement("dt");
    dt.textContent = term;
    var dd = document.createElement("dd");
    dd.textContent = value;
    dl.appendChild(dt);
    dl.appendChild(dd);
  }

  function renderInspectorForNode(id) {
    var panel = el("c4-inspector");
    var node = nodeById(id);
    var crate = node ? null : crateById(id);
    var moduleBox = state.boxes.get(id);

    if (!node && !crate && !moduleBox) {
      panel.hidden = true;
      return;
    }

    el("c4-inspector-kind").textContent = node
      ? node.id.split(":")[0]
      : crate
        ? "crate"
        : moduleBox
          ? moduleBox.kind
          : "";
    el("c4-inspector-title").textContent = node
      ? node.label
      : crate
        ? crate.label
        : moduleBox.label;
    el("c4-inspector-summary").textContent = node ? node.summary : crate ? crate.description || "" : "";
    var detail = el("c4-inspector-detail");
    detail.textContent = node && node.detail ? node.detail : "";

    var facts = el("c4-inspector-facts");
    facts.textContent = "";
    if (node) {
      factRow(facts, "Port", node.runs && node.runs.port ? ":" + node.runs.port : null);
      factRow(facts, "Doc", node.doc);
      if (node.stats) {
        factRow(facts, "Symbols", node.stats.symbols ? node.stats.symbols.toLocaleString() : null);
        factRow(facts, "Modules", node.stats.modules ? String(node.stats.modules) : null);
        factRow(facts, "Endpoints", node.stats.endpoints ? String(node.stats.endpoints) : null);
      }
    } else if (crate) {
      factRow(facts, "Tech", crate.tech);
      factRow(facts, "Deps", crate.deps.length ? crate.deps.map(function (d) { return d.replace("crate::", ""); }).join(", ") : null);
      factRow(facts, "Symbols", crate.symbols ? crate.symbols.toLocaleString() : null);
      factRow(facts, "Modules", crate.modules ? String(crate.modules) : null);
      factRow(facts, "Endpoints", crate.endpoints ? String(crate.endpoints) : null);
    } else if (moduleBox) {
      factRow(facts, "Graph id", moduleBox.id);
    }

    var links = el("c4-inspector-links");
    links.textContent = "";
    if (node && node.doc) {
      var docLink = document.createElement("a");
      // Served by the dev server's /docs/<path> viewer route — the static
      // docroot is docs/codegraph/, so "../"-style links 404.
      docLink.href = "/docs/" + node.doc;
      docLink.target = "_blank";
      docLink.rel = "noopener";
      docLink.textContent = "Open doc: " + node.doc;
      links.appendChild(docLink);
    }
    var refs = node ? node.code_refs_resolved || [] : crate ? [{ id: crate.id, label: crate.label }] : [];
    refs.forEach(function (ref) {
      var link = document.createElement("a");
      link.className = "c4-ref-link";
      link.textContent = "⤷ " + ref.label + " (" + ref.kind + ")";
      link.addEventListener("click", function () {
        flyToCodeRef(ref.id);
      });
      links.appendChild(link);
      var explorer = document.createElement("a");
      explorer.href = explorerLink(ref.id, ref.label, ref.kind);
      explorer.target = "_blank";
      explorer.rel = "noopener";
      explorer.textContent = "   open " + ref.label + " in Explorer ↗";
      links.appendChild(explorer);
    });
    panel.hidden = false;
  }

  function renderInspectorForEdge(edge) {
    var panel = el("c4-inspector");
    el("c4-inspector-kind").textContent = "connection";
    el("c4-inspector-title").textContent =
      labelFor(edge.from) + " → " + labelFor(edge.to);
    el("c4-inspector-summary").textContent = edge.summary || "";
    el("c4-inspector-detail").textContent = "";
    var facts = el("c4-inspector-facts");
    facts.textContent = "";
    factRow(facts, "Kind", edge.kind);
    factRow(facts, "Transport", edge.transport);
    factRow(facts, "Trigger", edge.trigger);
    var links = el("c4-inspector-links");
    links.textContent = "";
    var a = document.createElement("a");
    a.textContent = "Zoom to source: " + labelFor(edge.from);
    a.addEventListener("click", function () { flyToNode(edge.from); });
    links.appendChild(a);
    var b = document.createElement("a");
    b.textContent = "Zoom to target: " + labelFor(edge.to);
    b.addEventListener("click", function () { flyToNode(edge.to); });
    links.appendChild(b);
    panel.hidden = false;
  }

  function labelFor(id) {
    var node = nodeById(id);
    if (node) {
      return node.label;
    }
    var def = state.boxes.get(id);
    return def ? def.label : id;
  }

  function renderBreadcrumbs(id) {
    var nav = el("c4-breadcrumbs");
    nav.textContent = "";
    var chain = [];
    var current = id;
    var guard = 0;
    while (current && guard++ < 10) {
      var node = nodeById(current);
      if (!node) {
        break;
      }
      chain.unshift(node);
      current = node.parent;
    }
    var systemLink = document.createElement("a");
    systemLink.textContent = state.data.system.label;
    systemLink.addEventListener("click", function () {
      flyToRect({ x: -3260, y: -1740, w: 5580, h: 3140 });
    });
    nav.appendChild(systemLink);
    chain.forEach(function (node) {
      var sep = document.createElement("span");
      sep.className = "sep";
      sep.textContent = "›";
      nav.appendChild(sep);
      var link = document.createElement("a");
      link.textContent = node.label;
      link.addEventListener("click", function () {
        flyToNode(node.id);
        select(node.id);
      });
      nav.appendChild(link);
    });
  }

  // ---- Expansion -----------------------------------------------------------------------------

  function toggleExpand(id) {
    if (state.exp.has(id)) {
      state.exp.delete(id);
      if (id.indexOf("crate::") === 0) {
        unmountModules(id);
      }
    } else {
      state.exp.add(id);
      if (id.indexOf("crate::") === 0) {
        renderModules(id);
      }
    }
    if (id.indexOf("crate::") === 0) {
      layoutCrates();
    }
    updateBoxChildrenVisibility();
    scheduleUrlSync();
  }

  function ensureExpanded(id) {
    if (!state.exp.has(id)) {
      state.exp.add(id);
      if (id.indexOf("crate::") === 0) {
        renderModules(id);
        layoutCrates();
      }
      updateBoxChildrenVisibility();
    }
  }

  // ---- Camera actions --------------------------------------------------------------------------

  function flyToRect(rect, targetScale) {
    var view = viewportSize();
    var pad = 260;
    var fit = Math.min(
      view.w / (rect.w + pad * 2),
      view.h / (rect.h + pad * 2)
    );
    var endScale = C4Core.clampScale(targetScale || fit);
    var endX = rect.x + rect.w / 2;
    var endY = rect.y + rect.h / 2;
    animateCameraTo(endX, endY, endScale);
  }

  function flyToNode(id) {
    var def = state.boxes.get(id);
    if (def) {
      flyToRect(worldRect(def));
      return;
    }
    var node = nodeById(id);
    if (node && RUNTIME_LAYOUT[id]) {
      flyToRect(RUNTIME_LAYOUT[id]);
    }
  }

  function flyToCodeRef(graphId) {
    // Expand the ladder context around a crate/module reference and fly there.
    if (graphId.indexOf("crate::") === 0) {
      ensureExpanded(graphId);
      var crateDef = state.boxes.get(graphId);
      if (crateDef) {
        flyToRect({ x: crateDef.x, y: crateDef.y, w: crateDef.w, h: crateDef.h }, 1.3);
        return;
      }
    }
    var moduleId = graphId;
    var crateName = moduleId.replace("module::", "").split("::")[0];
    var crateId = "crate::" + crateName;
    ensureExpanded(crateId);
    var moduleDef = state.boxes.get(moduleId);
    if (moduleDef) {
      flyToRect(worldRect(moduleDef), 1.4);
    } else {
      flyToNode(crateId);
    }
  }

  function animateCameraTo(x, y, z) {
    cancelAnimationFrame(state.animHandle);
    var start = { x: state.cam.x, y: state.cam.y, z: state.cam.z };
    var t0 = performance.now();
    var duration = 520;
    function step(now) {
      var t = Math.min(1, (now - t0) / duration);
      var ease = 1 - Math.pow(1 - t, 3);
      state.cam.x = start.x + (x - start.x) * ease;
      state.cam.y = start.y + (y - start.y) * ease;
      state.cam.z = start.z * Math.pow(z / start.z, ease);
      applyCamera();
      if (t < 1) {
        state.animHandle = requestAnimationFrame(step);
      }
    }
    state.animHandle = requestAnimationFrame(step);
  }

  // ---- Culling -----------------------------------------------------------------------------------

  function scheduleCulling() {
    if (state.framePending) {
      return;
    }
    state.framePending = true;
    requestAnimationFrame(function () {
      state.framePending = false;
      runCulling();
      if (C4Core.bandForScale(state.cam.z) === "detail") {
        triggerSliceLoads();
      }
    });
  }

  function runCulling() {
    var viewport = viewportWorldRect();
    state.boxes.forEach(function (def) {
      if (!def.el) {
        return;
      }
      if (def.el.classList.contains("is-hidden-exp")) {
        return;
      }
      var visible = C4Core.boxesIntersect(viewport, worldRect(def), CULL_MARGIN);
      def.el.classList.toggle("is-culled", !visible);
    });
  }

  function triggerSliceLoads() {
    state.boxes.forEach(function (def) {
      if (def.kind === "crate" && def.moduleEls && state.exp.has(def.id)) {
        loadSlices(def);
      }
    });
  }

  // ---- Minimap -------------------------------------------------------------------------------------

  function drawMinimap() {
    var canvas = el("c4-minimap");
    if (!canvas || !state.data) {
      return;
    }
    var ctx = canvas.getContext("2d");
    var bounds = minimapBounds();
    var sx = canvas.width / bounds.w;
    var sy = canvas.height / bounds.h;
    var s = Math.min(sx, sy);
    var ox = (canvas.width - bounds.w * s) / 2 - bounds.x * s;
    var oy = (canvas.height - bounds.h * s) / 2 - bounds.y * s;

    ctx.clearRect(0, 0, canvas.width, canvas.height);
    var colors = {
      runtime: getCSS("--c4-runtime-border"),
      area: getCSS("--c4-area-border"),
      external: getCSS("--c4-external-border"),
      actor: getCSS("--c4-actor-border"),
      crate: getCSS("--c4-crate-border"),
      module: getCSS("--c4-module-border"),
      tier: getCSS("--c4-border"),
    };
    state.boxes.forEach(function (def) {
      if (!def.el || def.el.classList.contains("is-hidden-exp") || def.parentId) {
        return;
      }
      ctx.fillStyle = colors[def.kind] || colors.tier;
      ctx.globalAlpha = def.kind === "tier" ? 0.25 : 0.8;
      ctx.fillRect(ox + def.x * s, oy + def.y * s, Math.max(2, def.w * s), Math.max(2, def.h * s));
    });
    ctx.globalAlpha = 1;
    var viewport = viewportWorldRect();
    ctx.strokeStyle = getCSS("--c4-accent");
    ctx.lineWidth = 1.5;
    ctx.strokeRect(ox + viewport.x * s, oy + viewport.y * s, viewport.w * s, viewport.h * s);

    if (canvas._hasPointerEvents) return; // Only bind once
    canvas._hasPointerEvents = true;

    function moveCameraFromMinimap(event) {
      var rect = canvas.getBoundingClientRect();
      var b = minimapBounds();
      var sx_ = canvas.width / b.w;
      var sy_ = canvas.height / b.h;
      var s_ = Math.min(sx_, sy_);
      var ox_ = (canvas.width - b.w * s_) / 2 - b.x * s_;
      var oy_ = (canvas.height - b.h * s_) / 2 - b.y * s_;

      var wx = (event.clientX - rect.left - ox_) / s_;
      var wy = (event.clientY - rect.top - oy_) / s_;
      state.cam.x = wx;
      state.cam.y = wy;
      applyCamera();
    }

    var isDraggingMinimap = false;
    canvas.addEventListener("pointerdown", function (event) {
      isDraggingMinimap = true;
      canvas.setPointerCapture(event.pointerId);
      moveCameraFromMinimap(event);
      event.preventDefault(); // prevent scroll/pan
    });
    canvas.addEventListener("pointermove", function (event) {
      if (!isDraggingMinimap) return;
      moveCameraFromMinimap(event);
    });
    function endDrag(event) {
      isDraggingMinimap = false;
      canvas.releasePointerCapture(event.pointerId);
    }
    canvas.addEventListener("pointerup", endDrag);
    canvas.addEventListener("pointercancel", endDrag);
  }

  function minimapBounds() {
    if (state.bounds) {
      return state.bounds;
    }
    return { x: -3500, y: -2100, w: 5900, h: 4700 };
  }

  // Content-derived world bounds so the minimap tracks the ladder when
  // expanded crates stretch it far beyond the static estimate.
  function computeBounds() {
    var minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
    state.boxes.forEach(function (def) {
      if (!def.el) return;
      if (def.parentId && !state.exp.has(def.parentId)) return;
      var r = worldRect(def);
      minX = Math.min(minX, r.x);
      minY = Math.min(minY, r.y);
      maxX = Math.max(maxX, r.x + r.w);
      maxY = Math.max(maxY, r.y + r.h);
    });
    if (minX === Infinity) return;
    var m = 400;
    state.bounds = { x: minX - m, y: minY - m, w: (maxX - minX) + m * 2, h: (maxY - minY) + m * 2 };
  }

  function getCSS(name) {
    return getComputedStyle(document.body).getPropertyValue(name).trim() || "#888";
  }

  // ---- Search -----------------------------------------------------------------------------------------

  function buildSearchIndex() {
    var index = [];
    function push(label, kind, id, fly) {
      index.push({ label: label, kind: kind, id: id, fly: fly });
    }
    state.data.nodes.forEach(function (node) {
      push(node.label, node.id.split(":")[0], node.id, function () {
        if (node.parent) {
          ensureExpanded(node.parent);
        }
        flyToNode(node.id);
        select(node.id);
      });
    });
    state.data.externals.forEach(function (node) {
      push(node.label, "external", node.id, function () { flyToNode(node.id); select(node.id); });
    });
    state.data.actors.forEach(function (node) {
      push(node.label, "actor", node.id, function () { flyToNode(node.id); select(node.id); });
    });
    state.data.code.tiers.forEach(function (tier) {
      tier.crates.forEach(function (crate) {
        push(crate.label, "crate · " + tier.id, crate.id, function () {
          flyToCodeRef(crate.id);
          select(crate.id);
        });
        (crate.modules_list || []).forEach(function (module) {
          push(module.label, "module · " + crate.label, module.id, function () {
            flyToCodeRef(module.id);
          });
        });
      });
    });
    state.searchIndex = index;
  }

  var searchActiveIndex = -1;

  function runSearch(query) {
    var results = el("c4-search-results");
    if (!query || query.trim().length < 2) {
      results.hidden = true;
      results.textContent = "";
      return;
    }
    var scored = [];
    state.searchIndex.forEach(function (entry) {
      var score = C4Core.fuzzyScore(query, entry.label);
      if (score !== null) {
        scored.push({ entry: entry, score: score });
      }
    });
    scored.sort(function (a, b) { return b.score - a.score; });
    scored = scored.slice(0, 12);

    results.textContent = "";
    if (!scored.length) {
      results.hidden = true;
      return;
    }
    scored.forEach(function (item, index) {
      var row = document.createElement("div");
      row.className = "c4-search-item" + (index === 0 ? " active" : "");
      var label = document.createElement("span");
      label.textContent = item.entry.label;
      var kind = document.createElement("span");
      kind.className = "kind";
      kind.textContent = item.entry.kind;
      row.appendChild(label);
      row.appendChild(kind);
      row.addEventListener("click", function () {
        commitSearch(item.entry);
      });
      results.appendChild(row);
    });
    searchActiveIndex = 0;
    results.hidden = false;
  }

  function commitSearch(entry) {
    el("c4-search-results").hidden = true;
    entry.fly();
  }

  // ---- Theme -------------------------------------------------------------------------------------------

  function applyTheme(theme) {
    var next = theme === "light" ? "light" : "dark";
    document.body.dataset.theme = next;
    try {
      window.localStorage.setItem("codegraph.theme", next);
    } catch (_err) { /* private mode */ }
    drawMinimap();
  }

  function initTheme() {
    var stored = null;
    try {
      stored = window.localStorage.getItem("codegraph.theme");
    } catch (_err) { /* private mode */ }
    document.body.dataset.theme = stored === "light" ? "light" : "dark";

    el("c4-zoom-in").addEventListener("click", function() { animateCameraTo(state.cam.x, state.cam.y, C4Core.clampScale(state.cam.z * 1.5, 0.02, 4)); });
    el("c4-zoom-out").addEventListener("click", function() { animateCameraTo(state.cam.x, state.cam.y, C4Core.clampScale(state.cam.z / 1.5, 0.02, 4)); });
    el("c4-zoom-fit").addEventListener("click", function() {
      var minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
      state.boxes.forEach(function(def) {
        if (!def.el || def.kind === "tier") return;
        var r = worldRect(def);
        if (r.x < minX) minX = r.x;
        if (r.y < minY) minY = r.y;
        if (r.x + r.w > maxX) maxX = r.x + r.w;
        if (r.y + r.h > maxY) maxY = r.y + r.h;
      });
      if (minX !== Infinity) {
        flyToRect({x: minX, y: minY, w: maxX - minX, h: maxY - minY});
      }
    });

    el("c4-theme-toggle").addEventListener("click", function () {
      applyTheme(document.body.dataset.theme === "dark" ? "light" : "dark");
    });
  }

  // ---- Input bindings --------------------------------------------------------------------------------------

  function bindInput() {
    var vp = el("c4-viewport");

    vp.addEventListener("wheel", function (event) {
      event.preventDefault();
      cancelAnimationFrame(state.animHandle);
      var view = viewportSize();
      var world = C4Core.screenToWorld(event.offsetX, event.offsetY, state.cam, view);
      var factor = Math.exp(-event.deltaY * 0.0016);
      var newZ = C4Core.clampScale(state.cam.z * factor);
      state.cam.z = newZ;
      state.cam.x = world[0] - (event.offsetX - view.w / 2) / newZ;
      state.cam.y = world[1] - (event.offsetY - view.h / 2) / newZ;
      applyCamera();
    }, { passive: false });

    vp.addEventListener("pointerdown", function (event) {
      if (event.button !== 0) {
        return;
      }
      cancelAnimationFrame(state.animHandle);
      state.drag = { x: event.clientX, y: event.clientY, moved: false, captured: false };
      state.dragMoved = false;
      vp.classList.add("dragging");
    });
    vp.addEventListener("pointermove", function (event) {
      if (!state.drag) {
        return;
      }
      var dx = event.clientX - state.drag.x;
      var dy = event.clientY - state.drag.y;
      if (Math.abs(dx) + Math.abs(dy) > 4) {
        state.drag.moved = true;
        state.dragMoved = true;
        // Capture only once a real drag starts: capturing on pointerdown
        // would retarget click/dblclick to the viewport and swallow box
        // selection entirely.
        if (!state.drag.captured) {
          state.drag.captured = true;
          try {
            vp.setPointerCapture(event.pointerId);
          } catch (_err) { /* capture is best-effort */ }
        }
      }
      state.drag.x = event.clientX;
      state.drag.y = event.clientY;
      state.cam.x -= dx / state.cam.z;
      state.cam.y -= dy / state.cam.z;
      applyCamera();
    });
    // Selection is resolved manually on pointerup. Relying on click/dblclick
    // events is fragile once pointer capture engages mid-drag (events
    // retarget to the capture element), so hit-testing happens here for
    // boxes, edges, chips, and background consistently.
    var lastTap = { id: null, time: 0 };


  // ---- Hover Isolation --------------------------------------------------------

  function applyIsolation(focusBoxId, focusEdgeIndex) {
    if (!focusBoxId && focusEdgeIndex === null) {
      document.body.classList.remove("is-isolating");
      document.querySelectorAll(".is-highlighted").forEach(function(n) { n.classList.remove("is-highlighted"); });
      return;
    }
    document.body.classList.add("is-isolating");

    var highlightIds = new Set();
    var highlightEdges = new Set();

    if (focusBoxId) {
      highlightIds.add(focusBoxId);
      // Include parents
      var def = state.boxes.get(focusBoxId);
      while (def && def.parentId) {
        highlightIds.add(def.parentId);
        def = state.boxes.get(def.parentId);
      }

      state.edgeEls.forEach(function(entry, index) {
        if (entry.from === focusBoxId || entry.to === focusBoxId) {
          highlightEdges.add(index);
          highlightIds.add(entry.from);
          highlightIds.add(entry.to);
        }
      });
    } else if (focusEdgeIndex !== null) {
      highlightEdges.add(focusEdgeIndex);
      var entry = state.edgeEls.get(focusEdgeIndex);
      if (entry) {
        highlightIds.add(entry.from);
        highlightIds.add(entry.to);
      }
    }

    state.boxes.forEach(function(def) {
      if (def.el) {
        if (highlightIds.has(def.id) || def.kind === "tier") {
          def.el.classList.add("is-highlighted");
        } else {
          def.el.classList.remove("is-highlighted");
        }
      }
    });

    state.edgeEls.forEach(function(entry, index) {
      if (highlightEdges.has(index)) {
        entry.line.classList.add("is-highlighted");
        entry.chip.classList.add("is-highlighted");
        entry.label.classList.add("is-highlighted");
      } else {
        entry.line.classList.remove("is-highlighted");
        entry.chip.classList.remove("is-highlighted");
        entry.label.classList.remove("is-highlighted");
      }
    });
  }

  function resolvePointerHit(event) {
      var hit = document.elementFromPoint(event.clientX, event.clientY);
      if (!hit) {
        return;
      }
      if (hit.tagName === "A") {
        return; // native links (explorer deep links) handle themselves
      }
      var chip = hit.closest ? hit.closest(".c4-chip") : null;
      if (chip && chip.dataset.flyRef) {
        flyToCodeRef(chip.dataset.flyRef);
        return;
      }
      var edgeTarget = hit.closest ? hit.closest("[data-edge-index]") : null;
      if (edgeTarget) {
        selectEdge(parseInt(edgeTarget.dataset.edgeIndex, 10));
        return;
      }
      var box = hit.closest ? hit.closest(".c4-box") : null;
      if (box && box.dataset.id) {
        var now = performance.now();
        var isDoubleTap = lastTap.id === box.dataset.id && now - lastTap.time < 400;
        lastTap = { id: box.dataset.id, time: now };
        var def = state.boxes.get(box.dataset.id);
        if (isDoubleTap && def && def.expandable) {
          toggleExpand(box.dataset.id);
        }
        select(box.dataset.id);
        return;
      }
      lastTap = { id: null, time: 0 };
      clearSelection();
    }

    function endDrag(event) {
      var moved = state.drag ? state.drag.moved : false;
      state.drag = null;
      state.dragMoved = false;
      vp.classList.remove("dragging");
      if (!moved && event && event.type === "pointerup") {
        resolvePointerHit(event);
      }
    }
    vp.addEventListener("pointerup", endDrag);
    vp.addEventListener("pointercancel", endDrag);

    window.addEventListener("keydown", function (event) {
      if (event.key === "/" && document.activeElement !== el("c4-search")) {
        event.preventDefault();
        el("c4-search").focus();
      } else if (event.key === "Escape") {
        el("c4-search-results").hidden = true;
        clearSelection();
      } else if (event.key === "+" || event.key === "=") {
        state.cam.z = C4Core.clampScale(state.cam.z * 1.3);
        applyCamera();
      } else if (event.key === "-") {
        state.cam.z = C4Core.clampScale(state.cam.z / 1.3);
        applyCamera();
      } else if (event.key === "0") {
        animateCameraTo(-470, -170, 0.2);
      }
    });

    var search = el("c4-search");
    var searchTimer = 0;
    // The search backdrop dims the canvas while results are open; clicking
    // it dismisses them (it sits below the search surfaces in z-order).
    var searchBackdrop = document.querySelector(".c4-search-backdrop");
    if (searchBackdrop) {
      searchBackdrop.addEventListener("click", function () {
        el("c4-search-results").hidden = true;
        search.blur();
      });
    }
    search.addEventListener("input", function () {
      clearTimeout(searchTimer);
      searchTimer = setTimeout(function () {
        runSearch(search.value.trim());
      }, 120);
    });
    search.addEventListener("keydown", function (event) {
      var results = el("c4-search-results").querySelectorAll(".c4-search-item");
      if (event.key === "Enter") {
        event.preventDefault();
        if (searchActiveIndex >= 0 && results[searchActiveIndex]) {
          results[searchActiveIndex].click();
        }
      } else if (event.key === "ArrowDown" || event.key === "ArrowUp") {
        event.preventDefault();
        if (!results.length) {
          return;
        }
        results[searchActiveIndex] && results[searchActiveIndex].classList.remove("active");
        searchActiveIndex = event.key === "ArrowDown"
          ? Math.min(results.length - 1, searchActiveIndex + 1)
          : Math.max(0, searchActiveIndex - 1);
        results[searchActiveIndex].classList.add("active");
      }
    });

    el("c4-inspector-close").addEventListener("click", clearSelection);
  }

  // ---- Boot ---------------------------------------------------------------------------------------------------

  function showSplash(message) {
    var viewport = el("c4-viewport");
    var splash = document.createElement("div");
    splash.className = "c4-splash";
    splash.innerHTML = "<strong>" + message + "</strong>";
    var code = document.createElement("div");
    code.innerHTML = "Run <code>make graph-index</code> to generate it, then serve with <code>make graph-serve</code>.";
    splash.appendChild(code);
    viewport.appendChild(splash);
  }

  function checkStaleness() {
    fetch("stats.json", { cache: "no-store" })
      .then(function (res) { return res.ok ? res.json() : null; })
      .then(function (stats) {
        if (!stats || !state.data) {
          return;
        }
        if (String(stats.generated_at || "") > String(state.data.generated_at || "")) {
          var meta = el("c4-meta");
          meta.innerHTML = '<span class="stale">c4 index stale — run make graph-index</span>';
        } else {
          el("c4-meta").textContent =
            state.data.commit ? "commit " + state.data.commit : "up to date";
        }
      })
      .catch(function () {
        el("c4-meta").textContent = "";
      });
  }

  function boot() {
    initTheme();
    bindInput();

    fetch("c4.json", { cache: "no-store" })
      .then(function (res) {
        if (!res.ok) {
          throw new Error("HTTP " + res.status);
        }
        return res.json();
      })
      .then(function (data) {
        state.data = data;

        var urlState = C4Core.decodeUrlState(window.location.search);
        state.cam = { x: urlState.x, y: urlState.y, z: urlState.z };
        state.exp = new Set(urlState.exp);

        renderCuratedLayer();
        renderLadder();
        renderEdges();
        buildSearchIndex();

        // Restore expansions (mounts module grids).
        Array.from(state.exp).forEach(function (id) {
          if (id.indexOf("crate::") === 0) {
            renderModules(id);
          }
        });
        layoutCrates();
        updateBoxChildrenVisibility();
        if (urlState.sel) {
          select(urlState.sel);
        }
        applyCamera();
        checkStaleness();
      })
      .catch(function (err) {
        el("c4-meta").textContent = "Error: " + err.message;
        showSplash("Could not load c4.json");
      });
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", boot);
  } else {
    boot();
  }
})();
