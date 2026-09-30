/* ═══════════════════════════════════════════════════════════════
   Code Universe — 3D Spatial Graph Viewer
   ═══════════════════════════════════════════════════════════════ */
(function () {
  "use strict";

  // ── Kind → colour ────────────────────────────────────────
  // Two palettes: the original neon "Code Universe" set (great on
  // a pitch-black canvas, invisible on white) and a deeper, more
  // saturated set tuned for the light background. `KIND_COLORS` is
  // the "active" palette — `setTheme()` reassigns it.
  const KIND_COLORS_DARK = {
    crate:      "#ffffff",
    module:     "#ffd740",
    file:       "#64b5f6",
    function:   "#00e5ff",
    struct:     "#ff9100",
    enum:       "#b388ff",
    trait:      "#448aff",
    const:      "#69f0ae",
    type_alias: "#ff80ab",
    api_call:   "#ff5252",
    endpoint:   "#00e676",
    static:     "#78909c",
  };
  // Light-theme palette: ForceGraph3D applies Lambert-style lighting
  // to the sphere meshes, so the perceived colour on screen is lighter
  // than the hex value. We compensate by picking near-black saturated
  // tones so spheres still read as deep blue / red / green / etc. on
  // a near-white background.
  const KIND_COLORS_LIGHT = {
    crate:      "#0a0e1a",  // near-black so the (largest) crate spheres are anchor points
    module:     "#5d3a00",  // very dark amber
    file:       "#0d3b8a",  // very dark blue
    function:   "#003940",  // very dark teal
    struct:     "#7a2600",  // very dark burnt-orange
    enum:       "#3a0e7a",  // very dark purple
    trait:      "#0c1f4c",  // very dark navy
    const:      "#0e3a10",  // very dark forest green
    type_alias: "#5a0a31",  // very dark magenta
    api_call:   "#4d0000",  // very dark crimson
    endpoint:   "#0d4a12",  // very dark pine
    static:     "#15212b",  // very dark slate
  };
  // Pick the palette that matches the theme already applied to <html>
  // by the inline pre-paint script — keeps the first render correct.
  let KIND_COLORS =
    document.documentElement.getAttribute("data-theme") === "dark"
      ? KIND_COLORS_DARK
      : KIND_COLORS_LIGHT;

  // ── Kind → relative sphere size ──────────────────────────
  const KIND_VAL = {
    crate:      28,
    module:     10,
    file:       5,
    endpoint:   4.5,
    api_call:   4,
    trait:      3.5,
    struct:     2.8,
    enum:       2.2,
    type_alias: 1.8,
    function:   1,
    const:      0.9,
    static:     0.9,
  };

  // ── State ────────────────────────────────────────────────
  let graph       = null;
  let allNodes    = [];
  let allLinks    = [];
  const nodeMap   = new Map();
  let selectedId  = null;
  let glowMesh    = null;
  // Edges off by default: 260k+ edges murder framerate on first paint.
  // User can toggle via the Edges button (E key).
  let showEdges   = false;
  let isFlying    = false;
  // Kind-visibility filter: at 36k nodes ForceGraph3D's per-frame cost
  // makes the canvas nearly unresponsive. Default to coarse-grained
  // structure (crate + module) which is ~2k nodes; user opts into
  // file/symbol detail via the legend or "Show All Kinds" button.
  const DEFAULT_VISIBLE_KINDS = new Set(["crate", "module"]);
  let visibleKinds = new Set(DEFAULT_VISIBLE_KINDS);

  // ── Slash commands (built-ins + dynamic extension registry) ──────
  // Built-ins are the analyzer + core query commands. Anything else
  // registered by a codegraph extension (e.g. /skills, /tauri) is
  // discovered lazily by fetching /api/extensions on startup and
  // appended to SLASH_COMMANDS so the suggestion UI surfaces them
  // alongside built-ins.
  const COMMAND_RE = /^\/([a-zA-Z][a-zA-Z0-9-]*)\s*(.*)/i;
  const BUILTIN_COMMAND_NAMES = new Set([
    "how", "endpoints", "detail", "dead-code", "coverage", "flows",
  ]);
  const SLASH_COMMANDS = [
    { cmd: "/how",       arg: "<topic>",  desc: "Architecture explanation for a concept (approval, memory_tier, …)",
      kinds: ["function","struct","enum","trait","module","crate","endpoint","file"] },
    { cmd: "/endpoints", arg: "[crate]",  desc: "List API endpoints, optionally filtered by crate",
      kinds: ["crate"] },
    { cmd: "/detail",    arg: "<crate>",  desc: "Full blueprint of a crate (files, symbols, edges)",
      kinds: ["crate"] },
    { cmd: "/dead-code", arg: "[crate]",  desc: "Functions with no production callers (Tier A/C)",
      kinds: ["crate"] },
    { cmd: "/coverage",  arg: "[crate]",  desc: "Structural test-coverage buckets",
      kinds: ["crate"] },
    { cmd: "/flows",     arg: "<target>", desc: "Animated incoming flows into a function / file / module / crate",
      kinds: ["function","struct","enum","trait","module","crate","endpoint","file","api_call","type_alias","const","static"] },
  ];

  let _extSlashCache = null;
  let _extSlashLoading = null;
  function _fetchExtSlashCommands() {
    if (_extSlashCache) return Promise.resolve(_extSlashCache);
    if (_extSlashLoading) return _extSlashLoading;
    _extSlashLoading = fetch("/api/extensions")
      .then((r) => (r.ok ? r.json() : { extensions: [] }))
      .then((data) => {
        const all = [];
        (data.extensions || []).forEach((e) => {
          (e.slash_commands || []).forEach((spec) => all.push(spec));
        });
        _extSlashCache = all;
        // Mutate the live SLASH_COMMANDS list so the suggestion UI
        // (which captures it by reference inside initSearch) picks up
        // extension commands without a second registration pass.
        all.forEach((spec) => {
          if (!SLASH_COMMANDS.find((c) => c.cmd === spec.cmd)) {
            SLASH_COMMANDS.push({
              cmd: spec.cmd,
              arg: spec.arg || "",
              desc: spec.description || "",
              kinds: spec.kinds || null,
            });
          }
        });
        return all;
      })
      .catch(() => { _extSlashCache = []; return []; });
    return _extSlashLoading;
  }
  function lookupExtSlashCommand(cmdWithSlash) {
    if (!_extSlashCache) return null;
    return _extSlashCache.find((s) => s.cmd === cmdWithSlash) || null;
  }
  // Prime early so lookupExtSlashCommand is populated before first input.
  _fetchExtSlashCommands();

  // ── Boot ─────────────────────────────────────────────────
  document.addEventListener("DOMContentLoaded", boot);

  async function boot() {
    status("Fetching graph data\u2026");
    let raw;
    try {
      raw = await fetch("graph.json").then((r) => r.json());
    } catch (e) {
      status("Failed to load graph.json");
      return;
    }

    status(`Processing ${raw.nodes.length.toLocaleString()} nodes\u2026`);
    await frame();
    processData(raw);

    status("Computing spatial layout\u2026");
    await frame();
    computePositions();

    status("Rendering universe\u2026");
    await frame();
    initGraph();
    initSearch();
    initLegend();
    initControls();
    initCommandPanel();
    // Stars only on dark theme — `setTheme` will lazy-create them on
    // first switch-to-dark, so we don't pay the 5k-point upload cost
    // when the user stays in light mode.
    if (currentTheme() === "dark") addBackgroundStars();
    // Re-apply the active theme once the graph + scene exist so the
    // light-mode scene-light dimming kicks in on first paint (the
    // initial render uses ForceGraph3D defaults until we override).
    setTheme(currentTheme());

    // Stats
    const crates = new Set(allNodes.map((n) => n.crate).filter(Boolean));
    document.getElementById("graph-stats").textContent =
      `${allNodes.length.toLocaleString()} nodes  \u00b7  ` +
      `${allLinks.length.toLocaleString()} edges  \u00b7  ` +
      `${crates.size} crates`;

    // Fade loading screen
    const overlay = document.getElementById("loading-overlay");
    overlay.classList.add("fade-out");
    setTimeout(() => overlay.remove(), 1200);
  }

  // ── Data processing ──────────────────────────────────────
  function processData(raw) {
    allNodes = raw.nodes.map((n) => ({
      id:     n.id,
      kind:   n.kind,
      label:  n.label,
      path:   n.path  || "",
      crate:  n.crate || "",
      module: n.module || "",
      line:   n.line,
      method: n.method,
      // `route` is the HTTP path for endpoint nodes — needed so the
      // search box can prefix-match API paths (e.g. `/flows /api/foo`
      // resolves the endpoint by route, not label). Without this the
      // route lookups in renderArgSuggestions / the corpus search
      // silently return zero matches because n.route is undefined.
      route:  n.route,
      // Classification / rollup data (used by the info panel).
      test:             n.test,
      public:           n.public,
      visibility:       n.visibility,
      implements_trait: n.implements_trait,
      rollup:           n.rollup,
      // positions filled by computePositions
      x: 0, y: 0, z: 0,
      fx: 0, fy: 0, fz: 0,
    }));
    allNodes.forEach((n) => nodeMap.set(n.id, n));

    allLinks = raw.edges
      .filter((e) => nodeMap.has(e.from) && nodeMap.has(e.to))
      .map((e) => ({
        source: e.from,
        target: e.to,
        kind:   e.kind,
      }));
  }

  // ── Hierarchical spatial layout ──────────────────────────
  function computePositions() {
    // ---- group by crate ----------------------------------
    const byCrate = new Map();
    for (const n of allNodes) {
      const c = n.crate || "__root__";
      if (!byCrate.has(c)) byCrate.set(c, []);
      byCrate.get(c).push(n);
    }

    const crateNames = [...byCrate.keys()];
    const cratePos   = fibonacci(crateNames.length, 700);

    for (let ci = 0; ci < crateNames.length; ci++) {
      const cc   = cratePos[ci];
      const nodes = byCrate.get(crateNames[ci]);

      // ---- group by module inside crate ------------------
      const byMod = new Map();
      for (const n of nodes) {
        const m = n.module || n.id;
        if (!byMod.has(m)) byMod.set(m, []);
        byMod.get(m).push(n);
      }

      const modNames = [...byMod.keys()];
      const modR     = Math.max(160, Math.sqrt(modNames.length) * 42);
      const modPos   = fibonacci(modNames.length, modR);

      for (let mi = 0; mi < modNames.length; mi++) {
        const mc   = add3(cc, modPos[mi]);
        const mods = byMod.get(modNames[mi]);

        // ---- group by file inside module -----------------
        const byFile = new Map();
        for (const n of mods) {
          const f = n.path || n.id;
          if (!byFile.has(f)) byFile.set(f, []);
          byFile.get(f).push(n);
        }

        const fileNames = [...byFile.keys()];
        const fileR     = Math.max(50, Math.sqrt(fileNames.length) * 18);
        const filePos   = fibonacci(fileNames.length, fileR);

        for (let fi = 0; fi < fileNames.length; fi++) {
          const fc    = add3(mc, filePos[fi]);
          const fnodes = byFile.get(fileNames[fi]);

          for (const node of fnodes) {
            let p;
            if (node.kind === "crate")  p = cc;
            else if (node.kind === "module") p = mc;
            else if (node.kind === "file")   p = fc;
            else {
              // scatter symbols around their file centre
              const r     = 14 + seededRandom(node.id) * 22;
              const theta = seededRandom(node.id + "t") * Math.PI * 2;
              const phi   = Math.acos(2 * seededRandom(node.id + "p") - 1);
              p = {
                x: fc.x + r * Math.sin(phi) * Math.cos(theta),
                y: fc.y + r * Math.sin(phi) * Math.sin(theta),
                z: fc.z + r * Math.cos(phi),
              };
            }
            node.x = node.fx = p.x;
            node.y = node.fy = p.y;
            node.z = node.fz = p.z;
          }
        }
      }
    }
  }

  // ── 3-D graph initialisation ─────────────────────────────
  function initGraph() {
    const el = document.getElementById("3d-graph");

    // FastGraph: GPU-batched renderer (points + merged line buffers,
    // 2 draw calls total) instead of ForceGraph3D's per-node/per-link
    // scene objects — this is what makes full-graph flight smooth.
    graph = FastGraph({ controlType: "orbit" })(el)
      .graphData(filteredData())
      .backgroundColor(currentBgColor())
      .showNavInfo(false)
      // nodes
      .nodeRelSize(1)
      .nodeVal((n) => KIND_VAL[n.kind] || 1)
      .nodeColor((n) => (
        n.id === selectedId
          ? (currentTheme() === "light" ? "#000000" : "#ffffff")
          : (KIND_COLORS[n.kind] || (currentTheme() === "light" ? "#222" : "#555"))
      ))
      .nodeOpacity(currentTheme() === "light" ? 1.0 : 0.88)
      .nodeLabel((n) => `${n.label}  \u00b7  ${n.kind}`)
      // links — when a /flows visualization is active, only the
      // subgraph's edges show, each carrying directional particles
      // that visibly flow toward the target. Otherwise fall back to
      // the original selected-node behaviour.
      .linkVisibility((link) => {
        const src = typeof link.source === "object" ? link.source.id : link.source;
        const tgt = typeof link.target === "object" ? link.target.id : link.target;
        if (activeFlow) return activeFlow.edgeKeys.has(src + "::" + tgt);
        if (!showEdges) return false;
        if (!selectedId) return true;
        if (src === selectedId || tgt === selectedId) return true;
        return true;
      })
      .linkWidth((link) => {
        const src = typeof link.source === "object" ? link.source.id : link.source;
        const tgt = typeof link.target === "object" ? link.target.id : link.target;
        if (activeFlow && activeFlow.edgeKeys.has(src + "::" + tgt)) return 1.2;
        if (!selectedId) return 0;
        return (src === selectedId || tgt === selectedId) ? 1.0 : 0;
      })
      .linkOpacity(0.12)
      .linkColor((link) => {
        const src = typeof link.source === "object" ? link.source.id : link.source;
        const tgt = typeof link.target === "object" ? link.target.id : link.target;
        if (activeFlow && activeFlow.edgeKeys.has(src + "::" + tgt)) {
          return currentTheme() === "light" ? "#0a6cd0" : "#00e5ff";
        }
        if (!selectedId) return "#3a6a8a";
        if (src === selectedId || tgt === selectedId) {
          return currentTheme() === "light" ? "#0a6cd0" : "#00e5ff";
        }
        return "#1a2a35";
      })
      .linkDirectionalParticles((link) => {
        const src = typeof link.source === "object" ? link.source.id : link.source;
        const tgt = typeof link.target === "object" ? link.target.id : link.target;
        if (activeFlow && activeFlow.edgeKeys.has(src + "::" + tgt)) return 4;
        if (!selectedId) return 0;
        return (src === selectedId || tgt === selectedId) ? 2 : 0;
      })
      .linkDirectionalParticleWidth(2.0)
      .linkDirectionalParticleSpeed(0.012)
      .linkDirectionalParticleColor(() => (currentTheme() === "light" ? "#0a6cd0" : "#00e5ff"))
      // skip force simulation — positions are pre-computed
      .cooldownTicks(0)
      .warmupTicks(0)
      .d3AlphaDecay(1)
      .d3VelocityDecay(1)
      // interactions
      .onNodeClick(handleNodeClick)
      .onNodeHover(handleNodeHover)
      .onBackgroundClick(deselect);

    // camera
    const cam = graph.camera();
    cam.far  = 60000;
    cam.near = 0.5;
    cam.updateProjectionMatrix();

    // initial orbit
    setTimeout(() => {
      graph.cameraPosition({ x: 0, y: 600, z: 2200 }, { x: 0, y: 0, z: 0 }, 0);
    }, 60);
  }

  // ── Interactions ─────────────────────────────────────────
  function handleNodeClick(node) {
    if (!node) return;
    selectNode(node);
    flyTo(node);
  }

  function handleNodeHover(node) {
    document.body.style.cursor = node ? "pointer" : "default";
  }

  function selectNode(node) {
    selectedId = node.id;

    // info panel
    const panel   = document.getElementById("info-panel");
    const kindEl  = document.getElementById("info-kind");
    const labelEl = document.getElementById("info-label");
    const detEl   = document.getElementById("info-details");

    kindEl.textContent      = node.kind;
    kindEl.style.background = KIND_COLORS[node.kind] || "#555";
    kindEl.style.color      = luminance(KIND_COLORS[node.kind]) > 0.45 ? "#000" : "#fff";
    labelEl.textContent     = node.label;

    let html = "";
    if (node.path)   html += row("File",   node.path + (node.line ? ":" + node.line : ""));
    if (node.crate)  html += row("Crate",  node.crate);
    if (node.module && node.module !== node.crate) html += row("Module", node.module);
    if (node.method) html += row("Method", node.method);

    // Per-function flags (production / dead / test / untested classification).
    if (node.kind === "function") {
      const flags = [];
      if (node.test) flags.push("test");
      if (node.public) flags.push("public");
      if (node.visibility) flags.push("vis=" + node.visibility);
      if (node.implements_trait) flags.push("impl " + node.implements_trait);
      if (flags.length) html += row("Flags", flags.join(" · "));
    }

    // Hierarchical rollup for crate / module / file nodes.
    const ru = node.rollup;
    if (ru && typeof ru === "object") {
      const pct = (n, d) => (d > 0 ? Math.round((n / d) * 100) : 0);
      const prod = ru.prod_fns || 0;
      html += row(
        "Functions",
        ru.total_fns + " total · " + prod + " prod · " + ru.test_fns + " test · " + ru.public_fns + " public"
      );
      html += row(
        "Dead",
        ru.dead_fns + " / " + prod + " (" + pct(ru.dead_fns, prod) + "%)"
      );
      html += row(
        "Untested",
        ru.untested_fns + " / " + prod + " (" + pct(ru.untested_fns, prod) + "%)"
      );
      html += row(
        "Callers in",
        ru.prod_caller_edges.toLocaleString() + " prod · " + ru.test_caller_edges.toLocaleString() + " test"
      );
    }

    // Edge counts — broken down by kind so the user gets a high-signal
    // summary (e.g. `Outgoing: 12 (calls=10, contains=2)`) regardless
    // of the node's kind. Applies to crates/modules/files AND
    // individual functions/structs/endpoints.
    const outByKind = {};
    const inByKind = {};
    const neighbours = new Set();
    allLinks.forEach((l) => {
      const src = typeof l.source === "object" ? l.source.id : l.source;
      const tgt = typeof l.target === "object" ? l.target.id : l.target;
      const k = l.kind || "?";
      if (src === node.id) { outByKind[k] = (outByKind[k] || 0) + 1; neighbours.add(tgt); }
      if (tgt === node.id) { inByKind[k]  = (inByKind[k]  || 0) + 1; neighbours.add(src); }
    });
    const fmtBreakdown = (m) => {
      const parts = Object.entries(m).sort((a, b) => b[1] - a[1]).map((p) => p[0] + "=" + p[1]);
      return parts.length ? parts.join(", ") : "—";
    };
    const outTotal = Object.values(outByKind).reduce((a, b) => a + b, 0);
    const inTotal  = Object.values(inByKind).reduce((a, b) => a + b, 0);
    html += row("Outgoing", outTotal + " (" + fmtBreakdown(outByKind) + ")");
    html += row("Incoming", inTotal + " (" + fmtBreakdown(inByKind) + ")");
    if (neighbours.size > 0) {
      html += row("Unique neighbours", String(neighbours.size));
    }

    detEl.innerHTML = html;
    panel.classList.add("visible");

    // glow highlight
    addGlow(node);

    // re-colour to show selection + refresh links
    refreshGraph();
  }

  function deselect() {
    // Background click clears both selection state and any active
    // /flows overlay so users have a one-click "exit" from a flow view.
    clearFlowVisualization();
    selectedId = null;
    document.getElementById("info-panel").classList.remove("visible");
    removeGlow();
    refreshGraph();
  }

  function refreshGraph() {
    graph.nodeColor(graph.nodeColor());
    graph.linkColor(graph.linkColor());
    graph.linkWidth(graph.linkWidth());
    graph.linkVisibility(graph.linkVisibility());
    graph.linkDirectionalParticles(graph.linkDirectionalParticles());
  }

  // Compute the subset of nodes/links to render based on `visibleKinds`.
  // Edges are only kept when both endpoints survive the filter.
  function filteredData() {
    const nodes = allNodes.filter((n) => visibleKinds.has(n.kind));
    const idSet = new Set(nodes.map((n) => n.id));
    const links = allLinks.filter((l) => {
      const src = typeof l.source === "object" ? l.source.id : l.source;
      const tgt = typeof l.target === "object" ? l.target.id : l.target;
      return idSet.has(src) && idSet.has(tgt);
    });
    return { nodes, links };
  }

  function applyKindFilter() {
    if (!graph) return;
    graph.graphData(filteredData());
    updateStatsLine();
    updateLegendActive();
  }

  function updateStatsLine() {
    const total = allNodes.length;
    const shown = allNodes.reduce((acc, n) => acc + (visibleKinds.has(n.kind) ? 1 : 0), 0);
    const el = document.getElementById("graph-stats");
    if (el) {
      const crates = new Set(allNodes.map((n) => n.crate).filter(Boolean));
      el.textContent =
        `${shown.toLocaleString()}/${total.toLocaleString()} nodes  ·  ` +
        `${allLinks.length.toLocaleString()} edges  ·  ` +
        `${crates.size} crates`;
    }
  }

  function updateLegendActive() {
    document.querySelectorAll("#legend-items .legend-item").forEach((el) => {
      const kind = el.dataset.kind;
      if (!kind) return;
      el.classList.toggle("legend-item-active", visibleKinds.has(kind));
    });
  }

  function row(key, val) {
    return `<div class="detail-row"><span class="detail-key">${esc(key)}</span><span>${esc(val)}</span></div>`;
  }

  // ── Selection glow ───────────────────────────────────────
  function addGlow(node) {
    removeGlow();
    if (typeof THREE === "undefined") return;
    const size = Math.max((KIND_VAL[node.kind] || 1) * 1.4, 4);
    const geo  = new THREE.SphereGeometry(size, 24, 24);
    // Additive blending only works on dark backgrounds (it adds light
    // to whatever's behind). On light bg we need NormalBlending so the
    // halo actually subtracts brightness and shows up as a visible ring.
    const isLight = currentTheme() === "light";
    const fallback = isLight ? "#0a0e1a" : "#00dcff";
    const mat  = new THREE.MeshBasicMaterial({
      color:       new THREE.Color(KIND_COLORS[node.kind] || fallback),
      transparent: true,
      opacity:     isLight ? 0.35 : 0.18,
      blending:    isLight ? THREE.NormalBlending : THREE.AdditiveBlending,
      depthWrite:  false,
    });
    glowMesh = new THREE.Mesh(geo, mat);
    glowMesh.position.set(node.fx, node.fy, node.fz);
    graph.scene().add(glowMesh);

    // pulse loop
    (function pulse() {
      if (!glowMesh || !glowMesh.parent) return;
      const t = performance.now() * 0.0025;
      glowMesh.scale.setScalar(1 + 0.2 * Math.sin(t));
      glowMesh.material.opacity = 0.12 + 0.08 * Math.sin(t * 1.4);
      // FastGraph renders on demand — keep frames flowing while the
      // selection halo animates.
      if (graph && graph.wake) graph.wake();
      requestAnimationFrame(pulse);
    })();
  }

  function removeGlow() {
    if (!glowMesh) return;
    graph.scene().remove(glowMesh);
    glowMesh.geometry.dispose();
    glowMesh.material.dispose();
    glowMesh = null;
  }

  // ── Fly-to animation ────────────────────────────────────
  // Accepts an optional `onDone` callback fired the moment the tween
  // ends. The companion `flyToPromise` wraps that into an awaitable so
  // sequential path-walks (`flyThrough`) can chain hops cleanly.
  function flyTo(node, durationMs, onDone) {
    if (!node || isFlying) { if (onDone) onDone(); return; }

    const cam  = graph.camera();
    const ctrl = graph.controls();

    const dx   = cam.position.x - node.fx;
    const dy   = cam.position.y - node.fy;
    const dz   = cam.position.z - node.fz;
    const dist = Math.sqrt(dx * dx + dy * dy + dz * dz);

    const dur = durationMs || clamp(dist * 0.9, 700, 3200);
    const off = (KIND_VAL[node.kind] || 1) * 8 + 50;

    const endPos = {
      x: node.fx + off * 0.25,
      y: node.fy + off * 0.4,
      z: node.fz + off,
    };
    const endLook = { x: node.fx, y: node.fy, z: node.fz };

    const startPos  = { x: cam.position.x, y: cam.position.y, z: cam.position.z };
    const startLook = { x: ctrl.target.x,  y: ctrl.target.y,  z: ctrl.target.z };

    isFlying = true;
    document.getElementById("warp-overlay").classList.add("active");

    const t0 = performance.now();

    (function tick() {
      const t    = Math.min((performance.now() - t0) / dur, 1);
      const ease = easeInOutQuart(t);

      cam.position.x = lerp(startPos.x, endPos.x, ease);
      cam.position.y = lerp(startPos.y, endPos.y, ease);
      cam.position.z = lerp(startPos.z, endPos.z, ease);

      ctrl.target.x = lerp(startLook.x, endLook.x, ease);
      ctrl.target.y = lerp(startLook.y, endLook.y, ease);
      ctrl.target.z = lerp(startLook.z, endLook.z, ease);
      ctrl.update();

      if (t < 1) {
        requestAnimationFrame(tick);
      } else {
        isFlying = false;
        document.getElementById("warp-overlay").classList.remove("active");
        if (onDone) onDone();
      }
    })();
  }

  // Promise-shaped wrapper around `flyTo` so sequential walks can use
  // `await`. Resolves when the tween ends (or immediately if flyTo
  // refuses to start, e.g. another tween is already in flight).
  function flyToPromise(node, durationMs) {
    return new Promise((resolve) => flyTo(node, durationMs, resolve));
  }

  // Sequential fly-through: hop the camera node-by-node along a path
  // so the user reads "flowing into the searched node" instead of just
  // teleporting to the destination. Each hop awaits the previous tween
  // through `flyToPromise`, then dwells briefly so the eye can register
  // the stop before the next leg starts. Calls `selectNode` on each
  // hop so the visited node lights up as the camera arrives.
  async function flyThrough(nodeIds, perHopMs = 900, dwellMs = 220) {
    if (!Array.isArray(nodeIds) || nodeIds.length === 0) return;
    for (let i = 0; i < nodeIds.length; i++) {
      const n = nodeMap.get(nodeIds[i]);
      if (!n) continue;
      // First hop opens the chase — keep it short so we don't waste
      // time travelling from the user's current orbit to the leaf.
      // Intermediate + final hops use the full per-hop duration so the
      // direction of the flow reads visibly.
      const dur = i === 0 ? 550 : perHopMs;
      await flyToPromise(n, dur);
      // Light up the node we just arrived at — gives a per-hop marker
      // and updates the right-side info panel as the camera walks.
      selectNode(n);
      // Brief dwell so the user perceives discrete stops instead of a
      // smear; skip the dwell after the final hop.
      if (i < nodeIds.length - 1) {
        await new Promise((r) => setTimeout(r, dwellMs));
      }
    }
  }

  // Pan the camera along its own right / up axes — used by the arrow
  // keys when no node is selected (or when the user wants to look
  // around without jumping between nodes). The step scales with the
  // distance from the current orbit target so the response feels
  // similar at any zoom level.
  function panCamera(direction, scale) {
    if (!graph || typeof THREE === "undefined") return;
    const cam = graph.camera();
    const ctrl = graph.controls();
    if (!cam || !ctrl) return;
    const dist = cam.position.distanceTo(ctrl.target);
    const step = Math.max(20, dist * 0.08) * (scale || 1);
    const right = new THREE.Vector3().setFromMatrixColumn(cam.matrix, 0).normalize();
    const up = new THREE.Vector3().setFromMatrixColumn(cam.matrix, 1).normalize();
    let dx = 0, dy = 0;
    if (direction === "left")       dx = -step;
    else if (direction === "right") dx = step;
    else if (direction === "up")    dy = step;
    else if (direction === "down")  dy = -step;
    const delta = right.multiplyScalar(dx).add(up.multiplyScalar(dy));
    cam.position.add(delta);
    ctrl.target.add(delta);
    ctrl.update();
  }

  // Arrow-key navigation: hop from the selected node to one of its
  // neighbours via call / containment edges. Tracks a cycle cursor so
  // repeated presses in the same direction walk through multiple
  // candidates instead of bouncing between the same two nodes.
  const NAV_FLOW_KINDS = new Set([
    "calls", "handles", "implements", "calls_api", "targets_endpoint",
  ]);
  const NAV_HIER_KINDS = new Set(["contains", "defines"]);
  let navCycle = { nodeId: null, direction: null, list: [], idx: -1 };

  function navigateNeighbor(direction) {
    if (!selectedId) return;
    const collected = [];
    const isOutLike = direction === "right" || direction === "down";
    const kindFilter = direction === "up" || direction === "down"
      ? NAV_HIER_KINDS
      : null;  // null means "any flow kind, or anything in activeFlow"
    for (const l of allLinks) {
      const src = typeof l.source === "object" ? l.source.id : l.source;
      const tgt = typeof l.target === "object" ? l.target.id : l.target;
      const lk = l.kind || "";
      if (kindFilter) {
        if (!kindFilter.has(lk)) continue;
      } else if (activeFlow) {
        if (!activeFlow.edgeKeys.has(src + "::" + tgt)) continue;
      } else if (!NAV_FLOW_KINDS.has(lk)) {
        continue;
      }
      if (isOutLike && src === selectedId) collected.push(tgt);
      else if (!isOutLike && tgt === selectedId) collected.push(src);
    }
    const unique = [...new Set(collected)];
    if (!unique.length) return;
    if (navCycle.nodeId === selectedId && navCycle.direction === direction) {
      navCycle.idx = (navCycle.idx + 1) % unique.length;
    } else {
      navCycle = { nodeId: selectedId, direction, list: unique, idx: 0 };
    }
    const nextNode = nodeMap.get(unique[navCycle.idx]);
    if (!nextNode) return;
    selectNode(nextNode);
    flyTo(nextNode);
  }

  // BFS the active flow's adjacency map (built in `applyFlowVisualization`)
  // as an undirected graph to find a path from the flow target to
  // `endId`. Returned path starts at the target and ends at endId.
  // Empty array when endId isn't reachable inside the active subgraph.
  function flowPathToNode(endId) {
    if (!activeFlow || !activeFlow.nodeIds.has(endId)) return [];
    if (endId === activeFlow.target_id) return [activeFlow.target_id];
    const adj = activeFlow.adjacency || new Map();
    const queue = [[activeFlow.target_id]];
    const seen = new Set([activeFlow.target_id]);
    while (queue.length) {
      const path = queue.shift();
      const last = path[path.length - 1];
      if (last === endId) return path;
      for (const nxt of adj.get(last) || []) {
        if (seen.has(nxt)) continue;
        seen.add(nxt);
        queue.push([...path, nxt]);
      }
    }
    return [];
  }

  // ── /flows scene overlay ─────────────────────────────────
  // The active flow subgraph: target id + set of node ids + set of
  // "src::tgt" edge keys. linkVisibility / linkDirectionalParticles
  // accessors below consult these to:
  //   • surface only flow edges (everything else stays hidden)
  //   • shoot animated particles along each flow edge toward the target
  // Setting `activeFlow = null` (re-issue any other slash command or
  // clear by clicking the background) restores default rendering.
  let activeFlow = null;  // { target_id, direction, nodeIds:Set, edgeKeys:Set }

  function applyFlowVisualization(data) {
    if (!data || !data.target || !data.nodes) return;
    const nodeIds = new Set(data.nodes.map((n) => n.id));
    const edgeKeys = new Set((data.edges || []).map((e) => e.from + "::" + e.to));
    // Build an undirected adjacency map directly from the edges array.
    // We can't reliably split the "from::to" key back into its parts
    // because Rust-style node ids themselves contain `::`
    // (e.g. `magician::handlers::login`), so `indexOf("::")` lands
    // inside a node id and yields garbage. The adjacency map sidesteps
    // that entirely and is what `flowPathToNode` walks.
    const adjacency = new Map();      // undirected — used by click-driven walks
    const adjForward = new Map();     // s → [t]  (caller → callee), used to walk downstream from target
    const adjBackward = new Map();    // t → [s]  (callee → caller), used to walk upstream from target
    const link = (map, a, b) => {
      let list = map.get(a);
      if (!list) { list = []; map.set(a, list); }
      list.push(b);
    };
    (data.edges || []).forEach((e) => {
      if (!e || !e.from || !e.to) return;
      link(adjacency, e.from, e.to);
      link(adjacency, e.to, e.from);
      link(adjForward,  e.from, e.to);    // downstream walk from target
      link(adjBackward, e.to,   e.from);  // upstream walk from target
    });
    activeFlow = {
      target_id: data.target.id,
      direction: data.direction || "in",
      nodeIds: nodeIds,
      edgeKeys: edgeKeys,
      adjacency: adjacency,
      adjForward: adjForward,
      adjBackward: adjBackward,
    };
    // Expand `visibleKinds` so every node-kind in the subgraph renders
    // (the user may have filtered to crate+module by default).
    data.nodes.forEach((n) => visibleKinds.add(n.kind));
    applyKindFilter();
    // Re-evaluate link accessors so particles + colours pick up the
    // new activeFlow state.
    if (graph) {
      graph.linkVisibility(graph.linkVisibility());
      graph.linkColor(graph.linkColor());
      graph.linkWidth(graph.linkWidth());
      graph.linkDirectionalParticles(graph.linkDirectionalParticles());
    }
    // Walk each side of the flow that actually has nodes, so a "both"
    // direction request (the default for API paths) gets the full
    // call-graph tour: deepest upstream caller → handler → endpoint →
    // downstream calls → leaf. For "in" / "out" we only walk that one
    // side. Always settles at the target so the camera ends on the
    // searched item.
    const targetNode = nodeMap.get(data.target.id);
    const wantUp = activeFlow.direction === "in"  || activeFlow.direction === "both";
    const wantDown = activeFlow.direction === "out" || activeFlow.direction === "both";
    const upPath   = wantUp   ? deepestPathFrom(activeFlow.target_id, activeFlow.adjBackward) : [];
    const downPath = wantDown ? deepestPathFrom(activeFlow.target_id, activeFlow.adjForward)  : [];
    const segments = [];
    // Upstream leg: leaf → ... → target. (deepestPathFrom returns
    // [target, …, leaf]; reverse to flow inward.)
    if (upPath.length > 1) segments.push(upPath.slice().reverse());
    // Downstream leg: target → ... → leaf. The first segment already
    // ends at target, so this picks up where the upstream leg left off
    // and continues outward into callees.
    if (downPath.length > 1) {
      // Skip the leading target if we already arrived there via the
      // upstream leg, otherwise include it.
      segments.push(segments.length ? downPath.slice(1) : downPath);
    }
    if (!segments.length) {
      if (targetNode) flyTo(targetNode);
      return;
    }
    // Stitch segments into one flythrough so the dwell + selectNode
    // pacing stays consistent across the whole tour.
    const fullPath = [];
    for (const seg of segments) {
      for (const id of seg) {
        if (!fullPath.length || fullPath[fullPath.length - 1] !== id) {
          fullPath.push(id);
        }
      }
    }
    flyThrough(fullPath, 900);
  }

  // BFS the supplied directional adjacency map from `startId` and
  // return the path to the node that's furthest from start. Returns
  // [startId] when start has no neighbours.
  function deepestPathFrom(startId, adj) {
    if (!adj) return [startId];
    const queue = [[startId]];
    const seen = new Set([startId]);
    let deepest = [startId];
    while (queue.length) {
      const path = queue.shift();
      if (path.length > deepest.length) deepest = path;
      const last = path[path.length - 1];
      for (const nxt of adj.get(last) || []) {
        if (seen.has(nxt)) continue;
        seen.add(nxt);
        queue.push([...path, nxt]);
      }
    }
    return deepest;
  }

  function clearFlowVisualization() {
    if (!activeFlow) return;
    activeFlow = null;
    if (graph) {
      graph.linkVisibility(graph.linkVisibility());
      graph.linkColor(graph.linkColor());
      graph.linkWidth(graph.linkWidth());
      graph.linkDirectionalParticles(graph.linkDirectionalParticles());
    }
  }

  // ── Slash commands ───────────────────────────────────────
  // All user-controlled text is escaped via esc() before DOM insertion.
  function showCommandPanel(title, bodyHtml) {
    var panel = document.getElementById("command-panel");
    var titleEl = document.getElementById("command-title");
    var bodyEl = document.getElementById("command-body");
    if (!panel || !bodyEl) return;
    if (titleEl) titleEl.textContent = title;
    bodyEl.innerHTML = bodyHtml;
    panel.hidden = false;
  }

  function hideCommandPanel() {
    var panel = document.getElementById("command-panel");
    if (panel) panel.hidden = true;
  }

  function initCommandPanel() {
    var closeBtn = document.getElementById("command-close");
    if (closeBtn) closeBtn.addEventListener("click", hideCommandPanel);
  }

  async function fetchAndRenderCommand(mode, arg) {
    // Analyzer endpoints (/dead-code, /coverage, /flows) and the core
    // /how /endpoints /detail commands go through dedicated routes /
    // /api/query. Anything else is treated as extension-registered
    // and dispatched against its `http_route` from /api/extensions.
    var url, summary;
    if (mode === "dead-code") {
      var p = new URLSearchParams();
      if (arg && arg.trim()) p.set("crate", arg.trim());
      p.set("limit", "200");
      url = "/api/dead-code?" + p.toString();
      summary = "/dead-code " + (arg || "");
    } else if (mode === "coverage") {
      var p2 = new URLSearchParams();
      if (arg && arg.trim()) p2.set("crate", arg.trim());
      p2.set("limit", "200");
      url = "/api/test-coverage?" + p2.toString();
      summary = "/coverage " + (arg || "");
    } else if (mode === "flows") {
      var target = (arg || "").trim();
      if (!target) {
        showCommandPanel("/flows", '<div style="color:var(--text-dim)">Pass a node name: /flows &lt;target&gt;</div>');
        return;
      }
      // API paths get bidirectional traversal (callers → endpoint →
      // handler → its calls). Plain symbol names default to upstream.
      // Trace both upstream callers AND downstream callees regardless
      // of whether the target is an API path or a bare symbol — same
      // reasoning as in app.js: function-name targets were defaulting
      // to "in" and hiding the downstream call chain.
      var dir = "both";
      // Match the 2D viewer's 5-hop default so the same /flows query
      // produces the same call-chain depth in both views.
      var pf = new URLSearchParams({ target: target, hops: "5", direction: dir });
      url = "/api/flows?" + pf.toString();
      summary = "/flows " + target;
    } else if (BUILTIN_COMMAND_NAMES.has(mode)) {
      var pq = new URLSearchParams({ mode: mode });
      if (arg) pq.set("q", arg.trim());
      url = "/api/query?" + pq.toString();
      summary = "/" + mode + " " + (arg || "");
    } else {
      // Extension-registered command — wait for the cache to land if
      // the user beat us to it, then dispatch against its declared
      // http_route + arg_param.
      if (!_extSlashCache) {
        showCommandPanel("/" + mode, '<div style="color:var(--text-dim)">Loading\u2026</div>');
        await _fetchExtSlashCommands();
      }
      var extSpec = lookupExtSlashCommand("/" + mode);
      if (!extSpec) {
        showCommandPanel("Error", "<div>" + esc("Unknown command: /" + mode) + "</div>");
        return;
      }
      var pe = new URLSearchParams();
      var t = (arg || "").trim();
      if (t && extSpec.arg_param) pe.set(extSpec.arg_param, t);
      pe.set("limit", "300");
      url = extSpec.http_route + "?" + pe.toString();
      summary = "/" + mode + " " + (arg || "");
    }
    showCommandPanel(summary, '<div style="color:var(--text-dim)">Loading\u2026</div>');
    try {
      var resp = await fetch(url);
      if (!resp.ok) { showCommandPanel("Error", "<div>" + esc("Server returned " + resp.status) + "</div>"); return; }
      var data = await resp.json();
      if (data.error) { showCommandPanel("Error", "<div>" + esc(String(data.error)) + "</div>"); return; }
      renderCommandResult(mode, data);
    } catch (err) {
      showCommandPanel("Error", "<div>" + esc(String(err)) + "</div>");
    }
  }

  function renderCommandResult(mode, data) {
    var html = "";
    // Helper builders — all values go through esc()
    var sec = function(label) { return '<div class="cmd-section"><span class="cmd-label">' + esc(label) + "</span></div>"; };
    var item = function(text) { return '<div class="cmd-item">' + esc(text) + "</div>"; };
    var badge = function(text, cls) { return '<span class="cmd-badge ' + cls + '">' + esc(text) + "</span>"; };

    if (mode === "how") {
      html += sec("Topic: " + (data.topic || ""));
      var summary = data.summary || {};
      html += item("Scope: " + Object.entries(summary).map(function(e) { return e[1] + " " + e[0]; }).join(", "));
      var crateNames = Object.keys(data.crate_distribution || {});
      if (crateNames.length) html += item("Crates: " + crateNames.join(", "));
      var keyTypes = data.key_types || [];
      if (keyTypes.length) {
        html += sec("Key Types (" + keyTypes.length + ")");
        keyTypes.slice(0, 12).forEach(function(t) { html += item(t.kind + " " + t.label); });
        if (keyTypes.length > 12) html += item("+" + (keyTypes.length - 12) + " more");
      }
      var pFiles = data.primary_files || [];
      if (pFiles.length) { html += sec("Primary Files"); pFiles.slice(0, 6).forEach(function(f) { html += item(f.path.split("/").pop() + " (" + f.function_count + " fns)"); }); }
      var eps = data.endpoints || [];
      if (eps.length) { html += sec("Endpoints"); eps.forEach(function(e) { html += item(e.method + " " + e.route); }); }
      var cSections = Object.keys(data.contracts || {});
      if (cSections.length) { html += sec("Contracts"); cSections.forEach(function(s) { html += item(s + ": " + (data.contracts[s] || []).length + " items"); }); }
    } else if (mode === "endpoints") {
      html += sec("Endpoints: " + data.endpoint_count);
      (data.endpoints || []).forEach(function(ep) {
        var m = badge(ep.method, ep.method === "GET" ? "cmd-badge-green" : "cmd-badge-blue");
        html += '<div class="cmd-item">' + m + " " + esc(ep.route) + " \u2192 " + esc(ep.handler ? ep.handler.name : "?") + "</div>";
      });
    } else if (mode === "detail") {
      html += sec("Crate: " + (data.crate || ""));
      html += item(Object.entries(data.summary || {}).map(function(e) { return e[1] + " " + e[0]; }).join(", "));
      (data.files || []).forEach(function(f) { html += item(f); });
      Object.entries(data.symbols || {}).forEach(function(entry) {
        if (entry[1].length) {
          html += sec(entry[0] + " (" + entry[1].length + ")");
          entry[1].slice(0, 10).forEach(function(n) { html += item(n); });
          if (entry[1].length > 10) html += item("+" + (entry[1].length - 10) + " more");
        }
      });
    } else if (mode === "dead-code") {
      var tot = data.totals || {};
      var ncert = data.near_certain || 0;
      html += sec("Dead-code candidates: " + (Object.values(tot).reduce(function(a, b) { return a + b; }, 0)) + " (★ occ=1: " + ncert + ")");
      var cands = data.candidates || {};
      var renderList = function(label, list) {
        if (!list || !list.length) return;
        html += sec(label + " — " + list.length);
        list.slice(0, 50).forEach(function(c) {
          var tag = c.occurrences <= 1 ? "★ " : "  ";
          html += '<div class="cmd-item">' + esc(tag + c.name) + " <span class=\"cmd-badge cmd-badge-gray\">occ=" + c.occurrences + "</span> <span class=\"cmd-badge cmd-badge-gray\">tcalls=" + c.test_callers + "</span> " + esc(c.path + ":" + c.line) + "</div>";
        });
        if (list.length > 50) html += item("+" + (list.length - 50) + " more");
      };
      if (Array.isArray(cands)) {
        renderList("Candidates", cands);
      } else {
        renderList("Tier A — private, no prod callers", cands.A);
        renderList("Tier C — public, no prod callers", cands.C);
      }
    } else if (mode === "flows") {
      var fTotals = data.totals || {};
      var ft = data.target || {};
      html += sec("Flows " + (data.direction === "out" ? "out of" : "into") + " " + esc(ft.label || "?"));
      html += item("Target: " + (ft.kind || "?") + (ft.path ? "  ·  " + ft.path + (ft.line ? ":" + ft.line : "") : ""));
      var fNodes = (fTotals.nodes_returned != null ? fTotals.nodes_returned : fTotals.nodes) || 0;
      var fEdges = (fTotals.edges_returned != null ? fTotals.edges_returned : fTotals.edges) || 0;
      var fReach = fTotals.nodes_reachable != null ? fTotals.nodes_reachable : fNodes;
      html += item("Reach: " + fNodes + " nodes  ·  " + fEdges + " edges" + (fReach > fNodes ? "  (of " + fReach + " reachable)" : ""));
      html += item("Per-hop layers: " + ((data.layer_counts || []).join(" · ") || "—"));
      var groups = {};
      (data.nodes || []).forEach(function(n) {
        if (n.id === ft.id) return;
        (groups[n.hop || 0] = groups[n.hop || 0] || []).push(n);
      });
      Object.keys(groups).sort(function(a, b) { return Number(a) - Number(b); }).forEach(function(h) {
        html += sec("Hop " + h + " — " + groups[h].length);
        groups[h].slice(0, 30).forEach(function(n) {
          html += '<div class="cmd-item">' + esc(n.label || "?") + ' <span class="cmd-badge cmd-badge-gray">' + esc(n.kind || "?") + '</span> ' + esc(n.path ? n.path + (n.line ? ":" + n.line : "") : "") + '</div>';
        });
        if (groups[h].length > 30) html += item("+" + (groups[h].length - 30) + " more");
      });
      // Drive the 3D scene to actually visualise the flow.
      applyFlowVisualization(data);
    } else if (mode === "coverage") {
      var ctot = data.totals || {};
      html += sec("Coverage: " + (ctot.untested || 0) + " untested · " + (ctot.light || 0) + " light · " + (ctot.moderate || 0) + " moderate · " + (ctot.well || 0) + " well · " + (ctot.only_tested || 0) + " only-tested");
      var fns = data.functions || {};
      var renderBucket = function(label, list) {
        if (!list || !list.length) return;
        html += sec(label + " — " + list.length);
        list.slice(0, 50).forEach(function(f) {
          html += '<div class="cmd-item">' + esc(f.name) + " <span class=\"cmd-badge cmd-badge-gray\">prod=" + f.prod_callers + "</span> <span class=\"cmd-badge cmd-badge-gray\">tcalls=" + f.test_callers + "</span> " + esc(f.path + ":" + f.line) + "</div>";
        });
        if (list.length > 50) html += item("+" + (list.length - 50) + " more");
      };
      if (Array.isArray(fns)) {
        renderBucket("Functions", fns);
      } else {
        renderBucket("Untested (real gap)", fns.untested);
        renderBucket("Light (1 test caller)", fns.light);
        renderBucket("Moderate (2-5)", fns.moderate);
        renderBucket("Well (6+)", fns.well);
        renderBucket("Only-tested (no prod callers)", fns.only_tested);
      }
    } else {
      // Extension-registered command? Render purely from its declarative
      // `summary` spec — no per-mode branches needed.
      var extSpec = lookupExtSlashCommand("/" + mode);
      var sum = extSpec && extSpec.summary;
      if (sum) {
        (sum.header_fields || []).forEach(function(f) {
          var v = data[f.from];
          if (v === undefined || v === null) return;
          if (f.format === "kv" && typeof v === "object") {
            html += sec(f.label);
            Object.keys(v).sort().forEach(function(k) {
              html += item(k + ": " + v[k]);
            });
          } else {
            html += item(f.label + ": " + (typeof v === "object" ? JSON.stringify(v) : String(v)));
          }
        });
        var list = sum.list_field ? (data[sum.list_field] || []) : [];
        if (list.length) {
          html += sec(sum.list_field + " (" + list.length + ")");
          var template = sum.row_template || "{label}";
          list.slice(0, 80).forEach(function(row) {
            var line = template.replace(/\{(\w+)\}/g, function(_, key) {
              var rv = row[key];
              return (rv === undefined || rv === null) ? "?" : String(rv);
            });
            html += item(line);
          });
          if (list.length > 80) html += item("+" + (list.length - 80) + " more");
        }
      } else {
        html += item("(no renderer for /" + mode + ")");
      }
    }
    showCommandPanel("/" + mode + (data.topic ? " " + data.topic : data.crate ? " " + data.crate : ""), html);
  }

  // ── Search ───────────────────────────────────────────────
  function initSearch() {
    const input   = document.getElementById("search-input");
    const results = document.getElementById("search-results");
    let activeIdx = -1;

    // pre-built search corpus — includes `route` so users can find
    // endpoints by API path (e.g. typing `/api/foo` matches the
    // endpoint whose `n.route === "/api/foo"` even if the label is
    // `GET /api/foo`).
    const corpus = allNodes.map((n) => ({
      node: n,
      text: `${n.label}\t${n.id}\t${n.path}\t${n.module}\t${n.crate}\t${n.route || ""}`.toLowerCase(),
    }));

    const kindPriority = {
      crate: 0, module: 1, file: 2, endpoint: 3, trait: 4,
      struct: 5, enum: 6, api_call: 7, function: 8, const: 9,
      type_alias: 10, static: 11,
    };

    // Slash commands are triggered on Enter, not on every keystroke
    let pendingSlashCommand = false;

    // SLASH_COMMANDS is the module-level array seeded with built-ins and
    // mutated when /api/extensions resolves. We capture the live ref so
    // late-arriving extension commands appear in the suggestion palette
    // without re-initialising search.

    function clearResults() {
      while (results.firstChild) results.removeChild(results.firstChild);
    }

    function appendSlashRow(label, meta, onPick) {
      const row = document.createElement("div");
      row.className = "search-result-item slash-suggestion";
      const lbl = document.createElement("span");
      lbl.className = "result-label";
      lbl.textContent = label;
      const m = document.createElement("span");
      m.className = "result-meta";
      m.textContent = meta;
      row.appendChild(lbl);
      row.appendChild(m);
      row.addEventListener("click", onPick);
      results.appendChild(row);
    }

    function renderCommandList(prefix) {
      const matches = SLASH_COMMANDS.filter((c) => c.cmd.startsWith(prefix));
      clearResults();
      if (!matches.length) { results.hidden = true; return; }
      matches.forEach((m) => {
        appendSlashRow(
          m.cmd + (m.arg ? " " + m.arg : ""),
          m.desc,
          () => {
            input.value = m.cmd + (m.arg ? " " : "");
            // Trigger our handler so we transition into arg-suggestion mode.
            input.dispatchEvent(new Event("input", { bubbles: true }));
            input.focus();
          },
        );
      });
      results.hidden = false;
    }

    function renderArgSuggestions(cmdName, partial, kinds) {
      const p = partial.toLowerCase();
      const matches = [];
      if (p.startsWith("/")) {
        // API-path partial → match endpoint route (anywhere).
        for (const n of allNodes) {
          if (n.kind !== "endpoint") continue;
          const route = String(n.route || "").toLowerCase();
          if (route.includes(p)) matches.push(n);
          if (matches.length >= 25) break;
        }
        matches.sort((a, b) => {
          const ar = String(a.route || "").toLowerCase();
          const br = String(b.route || "").toLowerCase();
          const ap = ar.startsWith(p) ? 0 : 1;
          const bp = br.startsWith(p) ? 0 : 1;
          if (ap !== bp) return ap - bp;
          return ar.length - br.length;
        });
      } else {
        // Per-kind buckets so a flood of function matches doesn't
        // bury file / module / endpoint suggestions (functions are
        // 10x more numerous than any other kind in the graph).
        const kindSet = new Set(kinds);
        const buckets = {};
        kinds.forEach((k) => { buckets[k] = { pre: [], sub: [] }; });
        for (const n of allNodes) {
          if (!kindSet.has(n.kind)) continue;
          const lbl = String(n.label || "").toLowerCase();
          if (!lbl) continue;
          const b = buckets[n.kind];
          if (lbl.startsWith(p)) b.pre.push(n);
          else if (lbl.includes(p)) b.sub.push(n);
        }
        const shortest = (a, b) =>
          String(a.label || "").length - String(b.label || "").length;
        Object.values(buckets).forEach((b) => {
          b.pre.sort(shortest);
          b.sub.sort(shortest);
        });
        const order = [
          "endpoint", "file", "module", "crate",
          "struct", "enum", "trait", "type_alias",
          "function", "api_call", "const", "static",
        ];
        for (const k of order) {
          const b = buckets[k];
          if (b) matches.push(...b.pre.slice(0, 6));
        }
        for (const k of order) {
          const b = buckets[k];
          if (b) matches.push(...b.sub.slice(0, 4));
        }
      }

      clearResults();
      if (!matches.length) {
        appendSlashRow("(no matches)", "", () => {});
        results.hidden = false;
        return;
      }
      matches.slice(0, 20).forEach((n) => {
        const label = n.label || n.id;
        const meta = (n.kind || "?") + (n.path ? "  ·  " + n.path : "");
        // Endpoints: fill the input with just the route (not the
        // `METHOD /route` label) so /flows hits the API-path branch.
        const trigger = n.kind === "endpoint" && n.route ? n.route : label;
        appendSlashRow(label, meta, () => {
          input.value = cmdName + " " + trigger;
          results.hidden = true;
          input.focus();
        });
      });
      results.hidden = false;
    }

    function renderSlashSuggestions(raw) {
      const q = String(raw || "").toLowerCase();
      const spaceIdx = q.indexOf(" ");
      if (spaceIdx < 0) { renderCommandList(q); return; }
      const cmdName = q.slice(0, spaceIdx);
      const partial = q.slice(spaceIdx + 1).trim();
      const cmd = SLASH_COMMANDS.find((c) => c.cmd === cmdName);
      if (!cmd) { renderCommandList(q); return; }
      if (!cmd.kinds) { results.hidden = true; return; }
      if (!partial.length) { renderCommandList(cmdName); return; }
      renderArgSuggestions(cmdName, partial, cmd.kinds);
    }

    // Re-open suggestions when the user clicks back into the box with
    // slash-prefixed text still present — fixes the case where Enter
    // fires a command, the user clicks the input again, and types
    // expecting the palette but it stays closed because no input event
    // has fired yet.
    // Decide whether a slash-prefixed input is a command (e.g. `/skills`,
    // `/flows myfunc`) vs. an API route (e.g. `/api/foo`). A real
    // command has its first token equal-to or a prefix-of one of the
    // registered SLASH_COMMANDS entries; anything else is a route and
    // falls through to the normal corpus search (which now includes
    // `n.route` so endpoints surface from a route query).
    function looksLikeSlashCommand(q) {
      if (!q.startsWith("/")) return false;
      const firstTok = q.split(/\s+/)[0];
      return SLASH_COMMANDS.some((c) => c.cmd.startsWith(firstTok));
    }

    input.addEventListener("focus", () => {
      const raw = input.value;
      if (raw.startsWith("/") && looksLikeSlashCommand(raw.trim().toLowerCase())) {
        pendingSlashCommand = true;
        renderSlashSuggestions(raw.trim().toLowerCase());
      }
    });

    input.addEventListener("input", () => {
      const raw = input.value;
      const q = raw.trim().toLowerCase();

      // Slash-command palette: trigger ONLY if the prefix actually
      // matches a registered command. `/api/foo` doesn't, so it falls
      // through to normal search (which finds endpoints by route).
      if (raw.startsWith("/") && looksLikeSlashCommand(q)) {
        pendingSlashCommand = true;
        renderSlashSuggestions(q);
        return;
      }
      pendingSlashCommand = false;

      if (q.length < 2) { results.hidden = true; return; }

      const terms = q.split(/\s+/);
      let hits = corpus.filter((c) => terms.every((t) => c.text.includes(t)));

      // sort: when the query looks like an API path (`/…`), float
      // endpoints to the top — that's the only reason someone types a
      // leading slash without a command name. Otherwise: exact label
      // match first, then by kind priority.
      const routeQuery = q.startsWith("/");
      hits.sort((a, b) => {
        if (routeQuery) {
          const aEp = a.node.kind === "endpoint" ? 0 : 1;
          const bEp = b.node.kind === "endpoint" ? 0 : 1;
          if (aEp !== bEp) return aEp - bEp;
          if (aEp === 0) {
            const ar = String(a.node.route || "").toLowerCase();
            const br = String(b.node.route || "").toLowerCase();
            const ap = ar.startsWith(q) ? 0 : 1;
            const bp = br.startsWith(q) ? 0 : 1;
            if (ap !== bp) return ap - bp;
            return ar.length - br.length;
          }
        }
        const aEx = a.node.label.toLowerCase().includes(q) ? 0 : 1;
        const bEx = b.node.label.toLowerCase().includes(q) ? 0 : 1;
        if (aEx !== bEx) return aEx - bEx;
        return (kindPriority[a.node.kind] ?? 99) - (kindPriority[b.node.kind] ?? 99);
      });
      hits = hits.slice(0, 18);
      activeIdx = -1;

      if (!hits.length) {
        results.innerHTML =
          '<div class="search-result-item" style="color:var(--text-dim);pointer-events:none">No matches</div>';
        results.hidden = false;
        return;
      }

      results.innerHTML = hits
        .map(
          (h, i) =>
            `<div class="search-result-item" data-idx="${i}">` +
            `<span class="kind-dot" style="background:${KIND_COLORS[h.node.kind]};box-shadow:0 0 5px ${KIND_COLORS[h.node.kind]}"></span>` +
            `<span class="result-label">${highlight(h.node.label, q)}</span>` +
            `<span class="result-meta">${esc(h.node.kind)}</span>` +
            `</div>`
        )
        .join("");

      results.hidden = false;

      // click delegates
      results.querySelectorAll("[data-idx]").forEach((el) => {
        el.addEventListener("click", () => {
          const n = hits[+el.dataset.idx].node;
          selectNode(n);
          // If the user has an active /flows visualisation and the
          // picked node sits inside that subgraph, walk the camera
          // along the path target → ... → picked so they SEE the
          // flow lead to their search hit instead of jumping straight.
          if (activeFlow && activeFlow.nodeIds.has(n.id) && n.id !== activeFlow.target_id) {
            const pathFromTarget = flowPathToNode(n.id);
            if (pathFromTarget.length > 1) {
              flyThrough(pathFromTarget.slice().reverse(), 600);
            } else {
              flyTo(n);
            }
          } else {
            flyTo(n);
          }
          input.value = n.label;
          results.hidden = true;
        });
      });
    });

    // keyboard nav inside dropdown + slash command execution on Enter
    input.addEventListener("keydown", (e) => {
      if (e.key === "Enter") {
        e.preventDefault();
        // Fire slash command if pending — generic regex so any
        // extension-registered command (matched via lookupExtSlashCommand
        // inside fetchAndRenderCommand) works without listing it here.
        if (pendingSlashCommand) {
          const q = input.value.trim();
          const cmdMatch = q.match(COMMAND_RE);
          if (cmdMatch) {
            fetchAndRenderCommand(cmdMatch[1].toLowerCase(), cmdMatch[2] || "");
            // Clear the input + state so the next `/` opens a fresh
            // palette. Without this the stale `/flows myfunc` content
            // makes subsequent typing fall into arg-suggestion mode
            // for the previous command.
            input.value = "";
            pendingSlashCommand = false;
            results.hidden = true;
          }
          return;
        }
        // Otherwise pick search result
        const items = results.querySelectorAll("[data-idx]");
        if (items.length && !results.hidden) {
          const pick = activeIdx >= 0 ? items[activeIdx] : items[0];
          if (pick) pick.click();
        }
        return;
      }

      if (e.key === "Escape") {
        results.hidden = true;
        hideCommandPanel();
        input.blur();
        return;
      }

      const items = results.querySelectorAll("[data-idx]");
      if (!items.length || results.hidden) return;

      if (e.key === "ArrowDown") {
        e.preventDefault();
        activeIdx = Math.min(activeIdx + 1, items.length - 1);
        markActive(items, activeIdx);
      } else if (e.key === "ArrowUp") {
        e.preventDefault();
        activeIdx = Math.max(activeIdx - 1, 0);
        markActive(items, activeIdx);
      }
    });

    // close on outside click
    document.addEventListener("click", (e) => {
      if (!e.target.closest(".search-container")) results.hidden = true;
    });

    // global shortcuts
    document.addEventListener("keydown", (e) => {
      if (e.key === "/" && !isInput(e)) {
        e.preventDefault();
        input.focus();
        input.select();
      }
      if ((e.ctrlKey || e.metaKey) && e.key === "k") {
        e.preventDefault();
        input.focus();
        input.select();
      }
    });
  }

  function markActive(items, idx) {
    items.forEach((el, i) => el.classList.toggle("active", i === idx));
    if (items[idx]) items[idx].scrollIntoView({ block: "nearest" });
  }

  function highlight(text, query) {
    const i = text.toLowerCase().indexOf(query);
    if (i === -1) return esc(text);
    return (
      esc(text.slice(0, i)) +
      `<strong style="color:var(--accent)">${esc(text.slice(i, i + query.length))}</strong>` +
      esc(text.slice(i + query.length))
    );
  }

  // ── Legend ───────────────────────────────────────────────
  // Each kind row is a clickable visibility toggle. Two helper
  // buttons at the bottom (Show All, Crates+Modules) bypass per-kind
  // clicking. Built via DOM APIs (no innerHTML).
  function initLegend() {
    const counts = {};
    allNodes.forEach((n) => (counts[n.kind] = (counts[n.kind] || 0) + 1));
    const sorted = Object.entries(counts).sort((a, b) => b[1] - a[1]);

    const container = document.getElementById("legend-items");
    while (container.firstChild) container.removeChild(container.firstChild);

    for (const [k, c] of sorted) {
      const row = document.createElement("div");
      row.className = "legend-item";
      row.dataset.kind = k;
      row.title = "Click to toggle " + k + " nodes";

      const dot = document.createElement("span");
      dot.className = "legend-dot";
      const color = KIND_COLORS[k] || "#555";
      dot.style.background = color;
      dot.style.boxShadow = "0 0 5px " + color;

      const name = document.createElement("span");
      name.textContent = k;

      const count = document.createElement("span");
      count.className = "legend-count";
      count.textContent = c.toLocaleString();

      row.appendChild(dot);
      row.appendChild(name);
      row.appendChild(count);
      row.addEventListener("click", () => {
        if (visibleKinds.has(k)) visibleKinds.delete(k);
        else visibleKinds.add(k);
        applyKindFilter();
      });
      container.appendChild(row);
    }

    const actions = document.createElement("div");
    actions.className = "legend-actions";
    const showAllBtn = document.createElement("button");
    showAllBtn.type = "button";
    showAllBtn.className = "hud-btn legend-btn";
    showAllBtn.textContent = "Show All";
    showAllBtn.addEventListener("click", () => {
      visibleKinds = new Set(allNodes.map((n) => n.kind));
      applyKindFilter();
    });
    const resetBtn = document.createElement("button");
    resetBtn.type = "button";
    resetBtn.className = "hud-btn legend-btn";
    resetBtn.textContent = "Crates+Modules";
    resetBtn.addEventListener("click", () => {
      visibleKinds = new Set(DEFAULT_VISIBLE_KINDS);
      applyKindFilter();
    });
    actions.appendChild(showAllBtn);
    actions.appendChild(resetBtn);
    container.appendChild(actions);

    updateLegendActive();
    updateStatsLine();
  }

  // ── Controls ─────────────────────────────────────────────
  function initControls() {
    document.getElementById("btn-reset").addEventListener("click", () => {
      graph.cameraPosition({ x: 0, y: 600, z: 2200 }, { x: 0, y: 0, z: 0 }, 1400);
      deselect();
    });

    const edgeBtn = document.getElementById("btn-edges");
    edgeBtn.addEventListener("click", () => {
      showEdges = !showEdges;
      edgeBtn.classList.toggle("active", showEdges);
      graph.linkColor(graph.linkColor()); // re-evaluate accessor
    });

    const themeBtn = document.getElementById("btn-theme");
    if (themeBtn) {
      const syncThemeBtn = () => {
        const t = currentTheme();
        themeBtn.textContent = t === "light" ? "Theme: Light" : "Theme: Dark";
      };
      syncThemeBtn();
      themeBtn.addEventListener("click", () => {
        toggleTheme();
        syncThemeBtn();
      });
    }

    document.getElementById("info-close").addEventListener("click", deselect);

    document.addEventListener("keydown", (e) => {
      if (isInput(e)) return;
      if (e.key === "r" || e.key === "R") document.getElementById("btn-reset").click();
      if (e.key === "e" || e.key === "E") edgeBtn.click();
      if (e.key === "t" || e.key === "T") {
        toggleTheme();
        if (themeBtn) themeBtn.textContent =
          currentTheme() === "light" ? "Theme: Light" : "Theme: Dark";
      }
      if (e.key === "Escape") deselect();
      // While fly mode is engaged, WASD drives camera motion through the
      // dedicated fly loop (see initFlyMode). Skip the orbit-pan
      // shortcuts in that case so we don't double-apply movement.
      if (flyState.active) return;
      // Arrow-key behaviour (orbit mode):
      //   Plain arrow   → pan the camera (works any time).
      //   Shift+arrow   → hop to a graph neighbour of the selected
      //                   node (falls back to a fast pan when nothing
      //                   is selected).
      if (e.key === "ArrowRight" || e.key === "ArrowLeft" ||
          e.key === "ArrowUp"    || e.key === "ArrowDown") {
        e.preventDefault();
        const dir = e.key.replace("Arrow", "").toLowerCase();
        if (e.shiftKey && selectedId) {
          navigateNeighbor(dir);
        } else {
          panCamera(dir, e.shiftKey ? 2.4 : 1);
        }
      }
      // WASD: alternate camera pan in orbit mode.
      if (e.key === "w" || e.key === "W") { e.preventDefault(); panCamera("up", 1); }
      else if (e.key === "a" || e.key === "A") { e.preventDefault(); panCamera("left", 1); }
      else if (e.key === "s" || e.key === "S") { e.preventDefault(); panCamera("down", 1); }
      else if (e.key === "d" || e.key === "D") { e.preventDefault(); panCamera("right", 1); }
    });

    initFlyMode();
  }

  // ── FPS-style fly mode ───────────────────────────────────
  // Opt-in via the HUD "Fly" checkbox. When engaged:
  //   - Orbit controls disabled.
  //   - Click the canvas → request pointer lock; mouse movement rotates
  //     the camera (yaw/pitch like a first-person shooter).
  //   - WASD = forward / strafe-left / back / strafe-right in the
  //     direction the camera is looking. Space / Shift = up / down.
  //     Hold Shift while moving for a speed boost.
  //   - Esc releases pointer lock automatically; toggle off via the
  //     checkbox to restore orbit controls.
  const flyState = {
    active: false,
    yaw: 0,
    pitch: 0,
    keys: { forward: false, back: false, left: false, right: false, up: false, down: false, boost: false },
    rafId: 0,
    lastFrame: 0,
  };

  function initFlyMode() {
    const toggle = document.getElementById("btn-fly-mode");
    if (!toggle) return;
    toggle.addEventListener("change", () => {
      if (toggle.checked) enterFlyMode();
      else exitFlyMode();
      // Release focus from the checkbox so keyboard events flow to the
      // document — otherwise WASD goes to the focused <input> and our
      // global keydown handler skips them (and Space would re-toggle
      // the box). Also blur the wrapping <label> for good measure.
      try { toggle.blur(); } catch (_) {}
      const lbl = toggle.closest("label");
      if (lbl) { try { lbl.blur(); } catch (_) {} }
    });

    // mousemove (only when fly mode + pointer locked)
    document.addEventListener("mousemove", (e) => {
      if (!flyState.active) return;
      const dom = graph && graph.renderer && graph.renderer().domElement;
      if (!dom || document.pointerLockElement !== dom) return;
      const sens = 0.0022;
      flyState.yaw   -= e.movementX * sens;
      flyState.pitch -= e.movementY * sens;
      const max = Math.PI / 2 - 0.01;
      flyState.pitch = Math.max(-max, Math.min(max, flyState.pitch));
    });

    // Pointer lock auto-released on Esc by the browser; flip the
    // checkbox so the UI stays in sync.
    document.addEventListener("pointerlockchange", () => {
      const dom = graph && graph.renderer && graph.renderer().domElement;
      if (flyState.active && document.pointerLockElement !== dom) {
        // User pressed Esc — keep fly mode on but the keys won't fire
        // movement until they click the canvas again to re-lock.
      }
    });

    // WASD/space/shift state tracking while fly mode is active.
    document.addEventListener("keydown", (e) => {
      if (!flyState.active || isInput(e)) return;
      if (e.key === "w" || e.key === "W") { flyState.keys.forward = true; e.preventDefault(); }
      else if (e.key === "s" || e.key === "S") { flyState.keys.back = true; e.preventDefault(); }
      else if (e.key === "a" || e.key === "A") { flyState.keys.left = true; e.preventDefault(); }
      else if (e.key === "d" || e.key === "D") { flyState.keys.right = true; e.preventDefault(); }
      else if (e.key === " ") { flyState.keys.up = true; e.preventDefault(); }
      else if (e.key === "Shift") { flyState.keys.boost = true; }
      else if (e.key === "Control") { flyState.keys.down = true; }
    });
    document.addEventListener("keyup", (e) => {
      if (e.key === "w" || e.key === "W") flyState.keys.forward = false;
      else if (e.key === "s" || e.key === "S") flyState.keys.back = false;
      else if (e.key === "a" || e.key === "A") flyState.keys.left = false;
      else if (e.key === "d" || e.key === "D") flyState.keys.right = false;
      else if (e.key === " ") flyState.keys.up = false;
      else if (e.key === "Shift") flyState.keys.boost = false;
      else if (e.key === "Control") flyState.keys.down = false;
    });

    // Click canvas to acquire pointer lock while fly mode is on.
    const tryLock = () => {
      if (!flyState.active) return;
      const dom = graph && graph.renderer && graph.renderer().domElement;
      if (dom && document.pointerLockElement !== dom) {
        dom.requestPointerLock();
      }
    };
    const dom = graph && graph.renderer && graph.renderer().domElement;
    if (dom) dom.addEventListener("click", tryLock);
  }

  function enterFlyMode() {
    if (!graph || typeof THREE === "undefined") return;
    const cam = graph.camera();
    const ctrl = graph.controls();
    // Convert current orientation to yaw/pitch so we don't jolt the
    // view when switching modes.
    const euler = new THREE.Euler().setFromQuaternion(cam.quaternion, "YXZ");
    flyState.yaw = euler.y;
    flyState.pitch = euler.x;
    cam.rotation.order = "YXZ";
    if (ctrl) {
      // Disable user input on OrbitControls but let its `update()`
      // keep running — ForceGraph3D's render loop depends on it for
      // edge / particle / camera-matrix housekeeping each frame.
      // Patching update() to a no-op (the previous fix) caused all
      // edges to disappear in fly mode because the link accessors
      // never re-evaluated.
      //
      // Trick: instead of fighting OrbitControls, feed it a target
      // point that's always exactly in front of the camera along our
      // (yaw, pitch). update() then calls camera.lookAt(target),
      // which naturally orients the camera the way we want without
      // touching position. We update target every frame from the
      // flyLoop.
      ctrl.enabled = false;
      if (ctrl.enableDamping !== undefined) {
        flyState._origDamping = ctrl.enableDamping;
        ctrl.enableDamping = false;
      }
    }
    flyState.active = true;
    flyState.lastFrame = performance.now();
    document.body.classList.add("fly-mode-on");
    if (flyState.rafId) cancelAnimationFrame(flyState.rafId);
    flyLoop();
    // Try to acquire pointer lock right away so the user gets immediate
    // mouse-look. Browsers require this to be in a user-gesture handler
    // (the checkbox change counts) — if the gesture is too stale the
    // request will silently fail and the user can still click the
    // canvas to lock manually.
    const dom = graph && graph.renderer && graph.renderer().domElement;
    if (dom && document.pointerLockElement !== dom) {
      try { dom.requestPointerLock(); } catch (_) { /* fall back to click-to-lock */ }
    }
  }

  function exitFlyMode() {
    flyState.active = false;
    Object.keys(flyState.keys).forEach((k) => { flyState.keys[k] = false; });
    if (flyState.rafId) cancelAnimationFrame(flyState.rafId);
    flyState.rafId = 0;
    const ctrl = graph && graph.controls && graph.controls();
    if (ctrl) {
      if (flyState._origDamping !== undefined && ctrl.enableDamping !== undefined) {
        ctrl.enableDamping = flyState._origDamping;
        flyState._origDamping = undefined;
      }
      // Place the orbit pivot in front of the camera so the next
      // mouse-drag rotates around what the user was looking at,
      // not the world origin.
      const cam = graph.camera();
      const forward = new THREE.Vector3(0, 0, -1)
        .applyEuler(new THREE.Euler(flyState.pitch, flyState.yaw, 0, "YXZ"));
      const targetDist = 400;
      ctrl.target.copy(cam.position.clone().add(forward.multiplyScalar(targetDist)));
      ctrl.enabled = true;
      ctrl.update();
    }
    document.body.classList.remove("fly-mode-on");
    if (document.pointerLockElement) document.exitPointerLock();
    const cb = document.getElementById("btn-fly-mode");
    if (cb) cb.checked = false;
  }

  function flyLoop() {
    if (!flyState.active) return;
    const now = performance.now();
    const dt = Math.min(0.1, (now - flyState.lastFrame) / 1000);
    flyState.lastFrame = now;

    const cam = graph.camera();
    const ctrl = graph.controls && graph.controls();

    // Movement speed scales with how far we are from the origin so the
    // sim feels right whether we're inside a crate cluster or far out.
    const distFromOrigin = cam.position.length();
    const baseSpeed = Math.max(180, distFromOrigin * 0.6);
    const speed = baseSpeed * (flyState.keys.boost ? 3.0 : 1.0) * dt;

    const forwardDir = new THREE.Vector3(0, 0, -1)
      .applyEuler(new THREE.Euler(flyState.pitch, flyState.yaw, 0, "YXZ"));
    const rightDir = new THREE.Vector3(1, 0, 0)
      .applyEuler(new THREE.Euler(0, flyState.yaw, 0, "YXZ"));

    if (flyState.keys.forward) cam.position.add(forwardDir.clone().multiplyScalar(speed));
    if (flyState.keys.back)    cam.position.add(forwardDir.clone().multiplyScalar(-speed));
    if (flyState.keys.right)   cam.position.add(rightDir.clone().multiplyScalar(speed));
    if (flyState.keys.left)    cam.position.add(rightDir.clone().multiplyScalar(-speed));
    if (flyState.keys.up)      cam.position.y += speed;
    if (flyState.keys.down)    cam.position.y -= speed;

    // Drive OrbitControls.target to a point exactly one "look-distance"
    // ahead of the camera along (yaw, pitch). Every frame `update()`
    // calls `camera.lookAt(target)` which orients us in that direction
    // WITHOUT moving the camera (offset is preserved by spherical math).
    // This is what lets us keep OrbitControls' update() running — which
    // is what ForceGraph3D needs for its link / particle accessors to
    // stay live — while still getting FPS-style mouse-look + WASD.
    if (ctrl) {
      const lookDist = 100;
      ctrl.target.copy(cam.position.clone().add(forwardDir.clone().multiplyScalar(lookDist)));
    }

    flyState.rafId = requestAnimationFrame(flyLoop);
  }

  // ── Theme handling ────────────────────────────────────────
  // Light theme is the default — set in `graph3d.html`. Toggle saves
  // user choice to localStorage so it persists across reloads.
  let starsPoints = null;

  function currentTheme() {
    return document.documentElement.getAttribute("data-theme") || "light";
  }

  function currentBgColor() {
    // Match the CSS `--void` var: light=#f5f7fa, dark=#000005.
    return currentTheme() === "light" ? "#f5f7fa" : "#000005";
  }

  function setTheme(theme) {
    const t = theme === "dark" ? "dark" : "light";
    document.documentElement.setAttribute("data-theme", t);
    try { localStorage.setItem("codegraph3d-theme", t); } catch (e) { /* ignore */ }
    // Swap the active kind-color palette so spheres are legible on
    // whichever background is now showing.
    KIND_COLORS = t === "dark" ? KIND_COLORS_DARK : KIND_COLORS_LIGHT;
    if (graph) {
      graph.backgroundColor(currentBgColor());
      // Re-evaluate accessors → picks up the new KIND_COLORS table.
      graph.nodeColor(graph.nodeColor());
      graph.nodeOpacity(t === "light" ? 1.0 : 0.88);
      graph.linkColor(graph.linkColor());
      // Dim the scene's default ambient + directional lights in light
      // mode — ForceGraph3D ships them tuned for a black canvas, which
      // washes deep tones out on white. Cut intensity to ~30% so the
      // configured KIND_COLORS read at near their hex value.
      const lightFactor = t === "light" ? 0.35 : 1.0;
      try {
        graph.scene().traverse((obj) => {
          if (obj && obj.isLight) {
            if (obj.userData._origIntensity === undefined) {
              obj.userData._origIntensity = obj.intensity;
            }
            obj.intensity = obj.userData._origIntensity * lightFactor;
          }
        });
      } catch (e) { /* scene not ready */ }
    }
    // Stars belong to the dark "Code Universe" aesthetic — pull them
    // off the scene in light mode (they read as smudges on white).
    if (t === "dark") {
      if (!starsPoints) addBackgroundStars();
      else starsPoints.visible = true;
    } else if (starsPoints) {
      starsPoints.visible = false;
    }
    // Rebuild the legend so its dots use the new palette.
    if (allNodes.length) initLegend();
  }

  function toggleTheme() {
    setTheme(currentTheme() === "dark" ? "light" : "dark");
  }

  // ── Background star field ────────────────────────────────
  function addBackgroundStars() {
    if (typeof THREE === "undefined") return; // guard: THREE not loaded
    const N   = 5000;
    const pos = new Float32Array(N * 3);
    const col = new Float32Array(N * 3);

    for (let i = 0; i < N; i++) {
      const r     = 5000 + Math.random() * 12000;
      const theta = Math.random() * Math.PI * 2;
      const phi   = Math.acos(2 * Math.random() - 1);

      pos[i * 3]     = r * Math.sin(phi) * Math.cos(theta);
      pos[i * 3 + 1] = r * Math.sin(phi) * Math.sin(theta);
      pos[i * 3 + 2] = r * Math.cos(phi);

      const b = 0.35 + Math.random() * 0.65;
      const h = Math.random();
      col[i * 3]     = b * (h > 0.75 ? 0.82 : 1);
      col[i * 3 + 1] = b * (h > 0.85 ? 0.88 : 1);
      col[i * 3 + 2] = b;
    }

    const geo = new THREE.BufferGeometry();
    geo.setAttribute("position", new THREE.BufferAttribute(pos, 3));
    geo.setAttribute("color",    new THREE.BufferAttribute(col, 3));

    const mat = new THREE.PointsMaterial({
      size:            1.6,
      vertexColors:    true,
      transparent:     true,
      opacity:         0.55,
      sizeAttenuation: false,
      depthWrite:      false,
    });

    starsPoints = new THREE.Points(geo, mat);
    graph.scene().add(starsPoints);
  }

  // ── Utilities ────────────────────────────────────────────
  function fibonacci(n, radius) {
    if (n <= 0) return [];
    if (n === 1) return [{ x: 0, y: 0, z: 0 }];
    const pts = [];
    const ga  = Math.PI * (3 - Math.sqrt(5));
    for (let i = 0; i < n; i++) {
      const y  = 1 - (i / (n - 1)) * 2;
      const rr = Math.sqrt(1 - y * y);
      const th = ga * i;
      pts.push({ x: Math.cos(th) * rr * radius, y: y * radius, z: Math.sin(th) * rr * radius });
    }
    return pts;
  }

  function add3(a, b) { return { x: a.x + b.x, y: a.y + b.y, z: a.z + b.z }; }
  function lerp(a, b, t) { return a + (b - a) * t; }
  function clamp(v, lo, hi) { return Math.max(lo, Math.min(hi, v)); }
  function easeInOutQuart(t) { return t < 0.5 ? 8 * t * t * t * t : 1 - Math.pow(-2 * t + 2, 4) / 2; }
  function frame() { return new Promise((r) => requestAnimationFrame(r)); }

  function seededRandom(seed) {
    let h = 0;
    for (let i = 0; i < seed.length; i++) {
      h  = ((h << 5) - h + seed.charCodeAt(i)) | 0;
    }
    const x = Math.sin(h) * 10000;
    return x - Math.floor(x);
  }

  function luminance(hex) {
    if (!hex || hex[0] !== "#") return 0;
    const r = parseInt(hex.slice(1, 3), 16) / 255;
    const g = parseInt(hex.slice(3, 5), 16) / 255;
    const b = parseInt(hex.slice(5, 7), 16) / 255;
    return 0.299 * r + 0.587 * g + 0.114 * b;
  }

  function esc(s) {
    return String(s)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;");
  }

  function isInput(e) {
    const tgt = e.target;
    if (!tgt) return false;
    const tag = (tgt.tagName || "").toLowerCase();
    if (tag === "textarea" || tag === "select") return true;
    if (tag !== "input") return false;
    // Only treat *text-entry* inputs as "input" for keystroke-shadowing
    // purposes. Checkboxes / radios / range sliders / buttons sit on
    // the HUD (e.g. the Fly toggle) and should NOT swallow WASD or
    // arrow-key navigation when they happen to hold focus.
    const t = String(tgt.type || "text").toLowerCase();
    const TEXTY = new Set([
      "text", "search", "email", "url", "tel", "password", "number",
      "date", "datetime-local", "month", "time", "week",
    ]);
    return TEXTY.has(t);
  }

  function status(msg) {
    const el = document.getElementById("loading-status");
    if (el) el.textContent = msg;
  }
})();
