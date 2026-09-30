/*
 * Plane View — the whole workspace as one GPU-batched 2D canvas.
 *
 * The Cytoscape explorer pays a JS-object cost per element; at 100k+
 * mounted elements every frame and every interaction walks that model.
 * This page draws the same graph with FastGraph (one point cloud + one
 * merged line buffer, 2 draw calls) under an orthographic camera, so
 * pan/zoom/click are O(visible pixels), not O(graph).
 *
 * Data: graph_ui.json (all nodes + non-test edges). Layout: the
 * deterministic hierarchical map (crates → modules → files → symbols)
 * computed once at boot. Interactions: hover label, click inspector,
 * search-and-fly, Explorer deep links.
 */
(function () {
  "use strict";

  var KIND_COLORS_DARK = {
    crate: "#dcac32", module: "#4aa3dd", file: "#7ee0a3", function: "#9aa6b8",
    struct: "#c98fdd", enum: "#e0a04f", trait: "#45c4b0", endpoint: "#f6d365",
    api_call: "#f28b6e", skill: "#6ea8ff", section: "#59636e",
  };
  var KIND_COLORS_LIGHT = {
    crate: "#a87f16", module: "#2b7cb8", file: "#2f8f57", function: "#5d6b84",
    struct: "#8759d1", enum: "#b45309", trait: "#0d9488", endpoint: "#d49b11",
    api_call: "#c2543a", skill: "#2563eb", section: "#8b95a6",
  };
  var KIND_SIZE = {
    crate: 30, module: 13, file: 7, endpoint: 5, api_call: 5, trait: 4.5,
    struct: 4, skill: 4, enum: 3.5,
  };

  var graph = null;
  var graphIndex = new Map();
  var degreeById = new Map();
  var positions = null;
  var layoutBounds = null;
  var fg = null;
  var selectedId = null;
  var flyAnim = 0;
  // Expansion state: GPU-visibility semantics — data stays mounted,
  // expansion lights up subtrees (color/size buffers) instead of
  // mounting elements.
  var parentOf = new Map();
  var childrenOf = new Map();
  var subtreeOf = new Map();
  var expanded = new Set();
  var showAll = false;
  var litSet = new Set();
  // Search-match overlay: matches + their ancestors stay lit, everything
  // else dims — the explorer's search-slice behaviour on GPU colors.
  var searchMatches = null; // Set of node ids or null
  var disabledKinds = new Set();
  var adjacency = null; // Map<id, Array<id>> over visible edges (lazy)
  var blastSet = null; // Set of node ids currently blast-highlighted
  var blastOrigin = null;
  var impactSet = null; // Set of impacted node ids from git impact

  function $(id) { return document.getElementById(id); }

  function theme() {
    return document.body.dataset.theme === "light" ? "light" : "dark";
  }
  function colors() {
    return theme() === "light" ? KIND_COLORS_LIGHT : KIND_COLORS_DARK;
  }

  // ── Expansion indexes ────────────────────────────────────────────
  function buildExpansionIndexes() {
    graph.edges.forEach(function (e) {
      if (e.kind !== "contains" && e.kind !== "defines") return;
      if (!graphIndex.has(e.from) || !graphIndex.has(e.to)) return;
      if (!parentOf.has(e.to)) parentOf.set(e.to, e.from);
      if (!childrenOf.has(e.from)) childrenOf.set(e.from, []);
      childrenOf.get(e.from).push(e.to);
    });
  }

  function computeSubtree(rootId) {
    if (subtreeOf.has(rootId)) return subtreeOf.get(rootId);
    var out = [];
    var stack = [rootId];
    while (stack.length) {
      var id = stack.pop();
      out.push(id);
      var kids = childrenOf.get(id);
      if (kids) {
        for (var i = 0; i < kids.length; i++) stack.push(kids[i]);
      }
    }
    subtreeOf.set(rootId, out);
    return out;
  }

  var CONTAINER_KINDS = new Set(["crate", "module", "file"]);
  var VISIBLE_ADJ_KINDS = new Set([
    "calls", "handles", "calls_api", "targets_endpoint", "implements", "references",
  ]);
  var BASE_KINDS = new Set(["crate", "module"]);

  function computeLit() {
    litSet = new Set();
    var unrestricted = showAll;
    if (searchMatches) {
      // Search slice: matches plus ancestry.
      searchMatches.forEach(function (id) {
        litSet.add(id);
        var a = parentOf.get(id);
        var guard = 0;
        while (a && guard++ < 8) {
          litSet.add(a);
          a = parentOf.get(a);
        }
      });
      return;
    }
    if (unrestricted) {
      graph.nodes.forEach(function (n) { litSet.add(n.id); });
      return;
    }
    graph.nodes.forEach(function (n) {
      if (BASE_KINDS.has(n.kind)) litSet.add(n.id);
    });
    expanded.forEach(function (id) {
      computeSubtree(id).forEach(function (descendant) {
        litSet.add(descendant);
      });
    });
    if (blastSet) {
      blastSet.forEach(function (id) { litSet.add(id); });
    }
    if (impactSet) {
      impactSet.forEach(function (id) { litSet.add(id); });
    }
  }

  function buildAdjacency() {
    if (adjacency) return;
    adjacency = new Map();
    graph.edges.forEach(function (e) {
      if (!VISIBLE_ADJ_KINDS.has(e.kind)) return;
      if (!adjacency.has(e.from)) adjacency.set(e.from, []);
      adjacency.get(e.from).push(e.to);
      if (!adjacency.has(e.to)) adjacency.set(e.to, []);
      adjacency.get(e.to).push(e.from);
    });
  }

  function blastFrom(originId, hops, cap) {
    buildAdjacency();
    var seen = new Set([originId]);
    var frontier = [originId];
    var out = [originId];
    for (var h = 0; h < hops && frontier.length; h++) {
      var next = [];
      for (var i = 0; i < frontier.length; i++) {
        var neighbors = adjacency.get(frontier[i]) || [];
        for (var j = 0; j < neighbors.length; j++) {
          var nb = neighbors[j];
          if (seen.has(nb)) continue;
          seen.add(nb);
          out.push(nb);
          next.push(nb);
          if (out.length >= (cap || 4000)) {
            frontier = [];
            return out;
          }
        }
      }
      frontier = next;
    }
    return out;
  }

  function isLit(id) {
    return litSet.has(id);
  }

  function dimColor(hex, bg, amount) {
    // Blend a lit color toward the background by `amount` (0=lit).
    var c = new THREE.Color(hex);
    var b = new THREE.Color(bg);
    c.r = c.r + (b.r - c.r) * amount;
    c.g = c.g + (b.g - c.g) * amount;
    c.b = c.b + (b.b - c.b) * amount;
    return "#" + c.getHexString();
  }

  function applyExpansionColors() {
    if (!fg) return;
    var bg = theme() === "light" ? "#f7f8fa" : "#0d1117";
    var palette = colors();
    var dimNode = theme() === "light" ? 0.86 : 0.84;
    var dimEdge = theme() === "light" ? 0.94 : 0.93;
    var accentBlast = theme() === "light" ? "#d0342c" : "#ff6b5e";
    var accentMatch = theme() === "light" ? "#b8860b" : "#ffd257";
    var accentImpact = theme() === "light" ? "#8b2f9e" : "#c77dff";
    fg.nodeColor(function (n) {
      if (n.id === selectedId) {
        return theme() === "light" ? "#000000" : "#ffffff";
      }
      if (blastSet && blastSet.has(n.id) && n.id === blastOrigin) {
        return theme() === "light" ? "#7a1fa2" : "#ff9d00";
      }
      if (impactSet && impactSet.has(n.id)) {
        return accentImpact;
      }
      if (blastSet && blastSet.has(n.id)) {
        return accentBlast;
      }
      if (searchMatches && searchMatches.has(n.id)) {
        return accentMatch;
      }
      if (disabledKinds.has(n.kind)) {
        return bg;
      }
      var base = palette[n.kind] || (theme() === "light" ? "#5d6b84" : "#8d99ad");
      return isLit(n.id) ? base : dimColor(base, bg, dimNode);
    });
    fg.linkColor(function (link) {
      if (disabledKinds.has(graphIndex.get(link.source) && graphIndex.get(link.source).kind) ||
          disabledKinds.has(graphIndex.get(link.target) && graphIndex.get(link.target).kind)) {
        return bg;
      }
      var lit = isLit(link.source) && isLit(link.target);
      var hot = (blastSet && blastSet.has(link.source) && blastSet.has(link.target)) ||
                (impactSet && impactSet.has(link.source) && impactSet.has(link.target));
      if (hot) return lit ? accentBlast : dimColor(accentBlast, bg, dimEdge);
      var base = theme() === "light" ? "#9db4d0" : "#3a5a7a";
      return lit ? base : dimColor(base, bg, dimEdge);
    });
    fg.nodeVal(function (n) {
      var base = KIND_SIZE[n.kind] || 2.4;
      var deg = degreeById.get(n.id) || 0;
      var emphasized =
        (searchMatches && searchMatches.has(n.id)) ||
        (blastSet && blastSet.has(n.id)) ||
        (impactSet && impactSet.has(n.id));
      var scale = emphasized ? 1.6 : isLit(n.id) ? 1 : 0.45;
      return base * (1 + Math.min(0.8, Math.log2(1 + deg) / 8)) * scale;
    });
    fg.refreshSizes();
  }

  function refreshView() {
    computeLit();
    applyExpansionColors();
    updateLegend();
  }

  function toggleExpand(node) {
    if (!CONTAINER_KINDS.has(node.kind)) return false;
    if (expanded.has(node.id)) {
      expanded.delete(node.id);
    } else {
      expanded.add(node.id);
    }
    computeLit();
    applyExpansionColors();
    return true;
  }

  function flyToSubtree(node) {
    var ids = computeSubtree(node.id);
    var rect = nodeRect(ids.slice(0, 4000));
    if (!rect) return;
    var cam = fg.camera();
    var w = window.innerWidth;
    var h = window.innerHeight - 58;
    var baseHalfH = (cam.top - cam.bottom) / 2 / cam.zoom;
    var spanY = Math.max(400, rect.maxY - rect.minY) * 1.7;
    var spanX = Math.max(400, rect.maxX - rect.minX) * 1.7;
    var zoom = Math.min(
      (h / 2 / baseHalfH) * (h / spanY),
      (w / 2 / baseHalfH) * (w / spanX)
    );
    flyCamera({
      x: (rect.minX + rect.maxX) / 2,
      y: (rect.minY + rect.maxY) / 2,
      zoom: Math.max(0.05, Math.min(40, zoom)),
    }, 650);
  }

  // ── Layout: size-aware hierarchical Vogel placement ──────────────
  function vogel(i, spacing) {
    var r = spacing * Math.sqrt(i + 0.5);
    var t = i * 2.39996323;
    return { x: r * Math.cos(t), y: r * Math.sin(t) };
  }

  function computeLayout() {
    var byCrate = new Map();
    graph.nodes.forEach(function (n) {
      var c = n.crate || "__root__";
      if (!byCrate.has(c)) byCrate.set(c, []);
      byCrate.get(c).push(n);
    });
    positions = new Map();
    var minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
    var track = function (x, y) {
      if (x < minX) minX = x; if (x > maxX) maxX = x;
      if (y < minY) minY = y; if (y > maxY) maxY = y;
    };

    var crateNames = Array.from(byCrate.keys());
    var crateSpacing = 2200;
    crateNames.forEach(function (crateName, ci) {
      var cc = vogel(ci, crateSpacing);
      var nodes = byCrate.get(crateName);

      var byModule = new Map();
      nodes.forEach(function (n) {
        var m = n.module || n.id;
        if (!byModule.has(m)) byModule.set(m, []);
        byModule.get(m).push(n);
      });
      var moduleKeys = Array.from(byModule.keys());
      var moduleSpacing = Math.max(420, 46 * Math.sqrt(moduleKeys.length));
      moduleKeys.forEach(function (moduleKey, mi) {
        var mv = vogel(mi, moduleSpacing);
        var mc = { x: cc.x + mv.x, y: cc.y + mv.y };

        var byFile = new Map();
        byModule.get(moduleKey).forEach(function (n) {
          var f = n.path || n.id;
          if (!byFile.has(f)) byFile.set(f, []);
          byFile.get(f).push(n);
        });
        var fileKeys = Array.from(byFile.keys());
        var fileSpacing = Math.max(90, 15 * Math.sqrt(fileKeys.length));
        fileKeys.forEach(function (fileKey, fi) {
          var fv = vogel(fi, fileSpacing);
          var fx = mc.x + fv.x, fy = mc.y + fv.y;
          var fileNodes = byFile.get(fileKey);
          var symbolSpacing = Math.max(26, 2.6 * Math.sqrt(fileNodes.length));
          fileNodes.forEach(function (n, si) {
            var p;
            if (n.kind === "crate") p = { x: cc.x, y: cc.y };
            else if (n.kind === "module") p = { x: mc.x, y: mc.y };
            else if (n.kind === "file") p = { x: fx, y: fy };
            else {
              var sv = vogel(si, symbolSpacing);
              p = { x: fx + sv.x, y: fy + sv.y };
            }
            positions.set(n.id, p);
            track(p.x, p.y);
          });
        });
      });
    });
    layoutBounds = { minX: minX, minY: minY, maxX: maxX, maxY: maxY };
  }

  // ── Camera helpers (orthographic fit + tweened fly) ──────────────
  function fitToView(rect, ms) {
    if (!layoutBounds || !fg) return;
    var cam = fg.camera();
    var b = layoutBounds;
    var w = window.innerWidth;
    var h = window.innerHeight - 58;
    var pad = 0.08;
    var spanY = Math.max(1, b.maxY - b.minY) * (1 + pad * 2);
    var spanX = Math.max(1, b.maxX - b.minX) * (1 + pad * 2);
    // Ortho frustum height is fixed; fit via zoom (zoom>1 = tighter).
    var baseHalfH = (cam.top - cam.bottom) / 2 / cam.zoom;
    var zoomY = (h / 2 / baseHalfH) * (spanY ? h / spanY : 1);
    var zoomX = (w / 2 / baseHalfH) * (spanX ? w / spanX : 1);
    var zoomFit = Math.min(zoomY, zoomX);
    var target = {
      x: rect ? (rect.minX + rect.maxX) / 2 : (b.minX + b.maxX) / 2,
      y: rect ? (rect.minY + rect.maxY) / 2 : (b.minY + b.maxY) / 2,
      zoom: rect
        ? Math.min(
            zoomFit,
            (h / spanY_half(rect)) || zoomFit
          )
        : zoomFit,
    };
    if (rect) {
      var rectSpanY = Math.max(1, rect.maxY - rect.minY) * 1.6;
      var rectSpanX = Math.max(1, rect.maxX - rect.minX) * 1.6;
      target.zoom = Math.min(
        (h / 2 / baseHalfH) * (h / rectSpanY),
        (w / 2 / baseHalfH) * (w / rectSpanX)
      );
    }
    flyCamera(target, ms || 0);
    function spanY_half(r) { return Math.max(1, (r.maxY - r.minY) / 2); }
  }

  function flyCamera(target, ms) {
    var cam = fg.camera();
    cancelAnimationFrame(flyAnim);
    if (!ms) {
      cam.position.x = target.x;
      cam.position.y = target.y;
      cam.zoom = target.zoom;
      cam.updateProjectionMatrix();
      fg.wake();
      return;
    }
    var from = { x: cam.position.x, y: cam.position.y, z: cam.zoom };
    var t0 = performance.now();
    (function step() {
      var t = Math.min(1, (performance.now() - t0) / ms);
      var e = 1 - Math.pow(1 - t, 3);
      cam.position.x = from.x + (target.x - from.x) * e;
      cam.position.y = from.y + (target.y - from.y) * e;
      cam.zoom = from.z * (target.zoom / from.z);
      cam.zoom = Math.max(0.02, Math.min(400, from.z * Math.pow(target.zoom / from.z, e)));
      cam.updateProjectionMatrix();
      fg.wake();
      if (t < 1) flyAnim = requestAnimationFrame(step);
    })();
  }

  function nodeRect(nodeIds) {
    var r = null;
    nodeIds.forEach(function (id) {
      var p = positions.get(id);
      if (!p) return;
      if (!r) r = { minX: p.x, maxX: p.x, minY: p.y, maxY: p.y };
      else {
        r.minX = Math.min(r.minX, p.x); r.maxX = Math.max(r.maxX, p.x);
        r.minY = Math.min(r.minY, p.y); r.maxY = Math.max(r.maxY, p.y);
      }
    });
    return r;
  }

  // ── Inspector ────────────────────────────────────────────────────
  function showInspector(node) {
    selectedId = node.id;
    $("i-kind").textContent = node.kind || "node";
    $("i-label").textContent = node.label || node.id;
    $("i-path").textContent = node.path ? node.path + (node.line ? ":" + node.line : "") : (node.module || "");
    var facts = $("i-facts");
    facts.textContent = "";
    var add = function (k, v) {
      var dt = document.createElement("dt"); dt.textContent = k;
      var dd = document.createElement("dd"); dd.textContent = v;
      facts.appendChild(dt); facts.appendChild(dd);
    };
    if (node.crate) add("Crate", node.crate);
    if (node.module && node.module !== node.crate) add("Module", node.module);
    var deg = degreeById.get(node.id) || 0;
    add("Degree", String(deg));
    add("Graph id", node.id);
    $("i-explorer").href = "explorer.html?sel=" + encodeURIComponent(node.id) +
      "&q=" + encodeURIComponent(node.label || "");
    $("inspector").classList.add("visible");
  }

  function hideInspector() {
    selectedId = null;
    $("inspector").classList.remove("visible");
  }

  // ── Search ───────────────────────────────────────────────────────
  function fuzzy(needle, hay) {
    var n = needle.toLowerCase(), h = String(hay || "").toLowerCase();
    if (!n) return 0;
    var hi = 0, score = 0, streak = 0;
    for (var i = 0; i < n.length; i++) {
      var j = h.indexOf(n[i], hi);
      if (j < 0) return null;
      streak = j === hi ? streak + 1 : 1;
      score += 1 + streak * 2;
      if (j === 0 || /[^a-z0-9]/.test(h[j - 1] || " ")) score += 2;
      hi = j + 1;
    }
    return score;
  }

  function clearSearchSlice() {
    searchMatches = null;
    refreshView();
  }

  function runSearch(q) {
    var box = $("results");
    if (!q || q.length < 2) {
      box.hidden = true;
      box.textContent = "";
      if (searchMatches) clearSearchSlice();
      return;
    }
    var scored = [];
    graph.nodes.forEach(function (n) {
      if (scored.length > 4000 && scored[4000] && n.kind !== "crate") return;
      var s = fuzzy(q, n.label);
      if (s === null && n.path) s = null; // keep label-only matching fast
      if (s !== null) scored.push({ n: n, s: s });
    });
    scored.sort(function (a, b) { return b.s - a.s; });
    // Search-slice overlay: light the top 400 matches + ancestry.
    var matchIds = new Set();
    scored.slice(0, 400).forEach(function (item) { matchIds.add(item.n.id); });
    searchMatches = matchIds.size > 0 ? matchIds : null;
    refreshView();
    scored = scored.slice(0, 20);
    box.textContent = "";
    if (!scored.length) { box.hidden = true; return; }
    scored.forEach(function (item) {
      var row = document.createElement("div");
      row.className = "result";
      var left = document.createElement("span");
      left.textContent = item.n.label;
      var right = document.createElement("span");
      right.className = "kind";
      right.textContent = item.n.kind + (item.n.crate ? " · " + item.n.crate : "");
      row.appendChild(left); row.appendChild(right);
      row.addEventListener("click", function () {
        box.hidden = true;
        var p = positions.get(item.n.id);
        if (!p) return;
        showInspector(item.n);
        // Zoom to a comfortable local view around the node.
        var cam = fg.camera();
        var baseHalfH = (cam.top - cam.bottom) / 2 / cam.zoom;
        var targetZoom = Math.max(0.3, Math.min(3, 900 / Math.max(1, baseHalfH * 2)));
        flyCamera({ x: p.x, y: p.y, zoom: targetZoom }, 550);
      });
      box.appendChild(row);
    });
    box.hidden = false;
  }

  var legendEl = null;

  function setStatus(text) {
    var el2 = $("status");
    if (el2) el2.textContent = text;
  }

  function updateLegend() {
    if (!legendEl || !graph) return;
    var counts = new Map();
    graph.nodes.forEach(function (n) {
      counts.set(n.kind, (counts.get(n.kind) || 0) + 1);
    });
    var kinds = Array.from(counts.keys()).sort(function (a, b) { return counts.get(b) - counts.get(a); });
    legendEl.textContent = "";
    kinds.forEach(function (kind) {
      var chip = document.createElement("button");
      chip.className = "chip" + (disabledKinds.has(kind) ? " off" : "");
      chip.title = "Toggle " + kind + " nodes";
      var dot = document.createElement("span");
      dot.className = "dot";
      dot.style.background = colors()[kind] || "#8d99ad";
      var txt = document.createElement("span");
      txt.textContent = kind + " " + (counts.get(kind) || 0).toLocaleString();
      chip.appendChild(dot);
      chip.appendChild(txt);
      chip.addEventListener("click", function () {
        if (disabledKinds.has(kind)) disabledKinds.delete(kind);
        else disabledKinds.add(kind);
        refreshView();
      });
      legendEl.appendChild(chip);
    });
  }

  // ── Boot ─────────────────────────────────────────────────────────
  function boot() {
    var stored = null;
    try { stored = localStorage.getItem("codegraph.theme"); } catch (_e) {}
    document.body.dataset.theme = stored === "light" ? "light" : "dark";
    $("theme").addEventListener("click", function () {
      var next = theme() === "dark" ? "light" : "dark";
      document.body.dataset.theme = next;
      try { localStorage.setItem("codegraph.theme", next); } catch (_e) {}
      if (fg) {
        fg.backgroundColor(next === "light" ? "#f7f8fa" : "#0d1117");
        fg.nodeColor(function (n) {
          return n.id === selectedId
            ? (next === "light" ? "#000000" : "#ffffff")
            : (colors()[n.kind] || (next === "light" ? "#5d6b84" : "#8d99ad"));
        });
        fg.nodeOpacity(next === "light" ? 1.0 : 0.9);
      }
    });
    $("inspector-close").addEventListener("click", hideInspector);
    var showAllBtn = $("show-all");
    if (showAllBtn) {
      var syncShowAll = function () {
        showAllBtn.textContent = showAll ? "Focus Mode" : "Show All";
        showAllBtn.title = showAll
          ? "Collapse to crates + modules and expanded branches"
          : "Light up every node (GPU visibility — nothing remounts)";
      };
      showAllBtn.addEventListener("click", function () {
        showAll = !showAll;
        try { localStorage.setItem("plane.showAll", showAll ? "1" : "0"); } catch (_e2) {}
        computeLit();
        applyExpansionColors();
        syncShowAll();
      });
      syncShowAll();
    }
    var blastBtn = $("blast");
    if (blastBtn) {
      blastBtn.addEventListener("click", function () {
        if (!selectedId) {
          setStatus("Select a node first, then Blast.");
          return;
        }
        if (blastSet) {
          blastSet = null;
          blastOrigin = null;
          refreshView();
          return;
        }
        var hops = 2;
        var hopsSel = $("blast-hops");
        if (hopsSel) hops = Number(hopsSel.value) || 2;
        blastOrigin = selectedId;
        blastSet = new Set(blastFrom(selectedId, hops, 4000));
        refreshView();
        setStatus("Blast: " + blastSet.size.toLocaleString() + " nodes within " + hops + " hops (click again to clear).");
      });
    }
    var impactBtn = $("impact");
    if (impactBtn) {
      impactBtn.addEventListener("click", function () {
        if (impactSet) {
          impactSet = null;
          refreshView();
          return;
        }
        var base = $("impact-base") ? $("impact-base").value : "origin/master";
        setStatus("Running git impact against " + base + "…");
        fetch("/api/git/impact", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ base_ref: base, compare_mode: "workspace" }),
        })
          .then(function (r) { return r.json().then(function (j) { return { ok: r.ok, j: j }; }); })
          .then(function (res) {
            if (!res.ok) throw new Error(res.j.message || ("HTTP " + res.ok));
            var changed = res.j.changed_files || [];
            var fileNode = new Map();
            graph.nodes.forEach(function (n) {
              if (n.kind === "file" && n.path) fileNode.set(n.path, n.id);
            });
            var ids = new Set();
            changed.forEach(function (path) {
              var fid = fileNode.get(path);
              if (fid) {
                ids.add(fid);
                computeSubtree(fid).forEach(function (d) { ids.add(d); });
              }
            });
            if (ids.size === 0) {
              setStatus("Impact: no changed files matched the graph (or no changes vs base).");
              return;
            }
            impactSet = ids;
            refreshView();
            setStatus("Impact: " + ids.size.toLocaleString() + " nodes under " +
              changed.length + " changed files (click again to clear).");
          })
          .catch(function (err) {
            setStatus("Impact failed: " + err.message);
          });
      });
    }
    var collapseBtn = $("collapse-all");
    if (collapseBtn) {
      collapseBtn.addEventListener("click", function () {
        expanded.clear();
        blastSet = null;
        blastOrigin = null;
        impactSet = null;
        searchMatches = null;
        var box2 = $("search");
        if (box2) box2.value = "";
        refreshView();
      });
    }
    legendEl = $("legend");
    window.addEventListener("keydown", function (e) {
      if (e.key === "/" && document.activeElement !== $("search")) {
        e.preventDefault(); $("search").focus();
      } else if (e.key === "Escape") {
        $("results").hidden = true; hideInspector();
      } else if (e.key === "0") {
        fitToView(null, 600);
      }
    });
    var timer = 0;
    $("search").addEventListener("input", function () {
      clearTimeout(timer);
      timer = setTimeout(function () { runSearch($("search").value.trim()); }, 150);
    });

    fetch("graph_ui.json", { cache: "no-store" })
      .then(function (r) {
        if (!r.ok) throw new Error("HTTP " + r.status);
        return r.json();
      })
      .then(function (data) {
        graph = data;
        $("loading-text").textContent = "Indexing " + graph.nodes.length.toLocaleString() + " nodes…";
        return new Promise(function (res) { setTimeout(res, 30); });
      })
      .then(function () {
        graph.nodes.forEach(function (n) { graphIndex.set(n.id, n); });
        graph.edges.forEach(function (e) {
          degreeById.set(e.from, (degreeById.get(e.from) || 0) + 1);
          degreeById.set(e.to, (degreeById.get(e.to) || 0) + 1);
        });
        buildExpansionIndexes();
        try {
          showAll = localStorage.getItem("plane.showAll") === "1";
        } catch (_e) {}
        computeLit();
        computeLayout();
        $("loading-text").textContent = "Rendering…";
        return new Promise(function (res) { setTimeout(res, 30); });
      })
      .then(function () {
        var cyData = {
          nodes: graph.nodes.map(function (n) {
            var p = positions.get(n.id) || { x: 0, y: 0 };
            return { id: n.id, kind: n.kind, label: n.label, x: p.x, y: p.y, z: 0 };
          }),
          links: graph.edges.map(function (e) {
            return { source: e.from, target: e.to, kind: e.kind };
          }),
        };
        fg = FastGraph({ camera: "ortho" })($("plane"));
        fg.backgroundColor(theme() === "light" ? "#f7f8fa" : "#0d1117")
          .graphData(cyData)
          .nodeOpacity(theme() === "light" ? 1.0 : 0.9)
          .nodeLabel(function (n) {
            var real = graphIndex.get(n.id);
            if (!real) return n.id;
            var mark = expanded.has(n.id) ? " ▾" : CONTAINER_KINDS.has(real.kind) ? " ▸" : "";
            return real.label + " · " + real.kind + mark;
          })
          .linkOpacity(0.28)
          .onNodeClick(function (n) {
            var real = graphIndex.get(n.id) || n;
            showInspector(real);
            if (CONTAINER_KINDS.has(real.kind)) {
              toggleExpand(real);
              if (expanded.has(real.id)) flyToSubtree(real);
            }
          })
          .onBackgroundClick(function () { hideInspector(); });
        applyExpansionColors();

        // Ortho frustum: set world height so the full map fits at zoom 1.
        var cam = fg.camera();
        var b = layoutBounds;
        var spanY = Math.max(1, b.maxY - b.minY) * 1.1;
        cam.top = spanY / 2; cam.bottom = -spanY / 2;
        cam.position.x = (b.minX + b.maxX) / 2;
        cam.position.y = (b.minY + b.maxY) / 2;
        cam.zoom = 1;
        cam.updateProjectionMatrix();
        fg.wake();

        // Deep links: ?q= seeds search; ?sel= selects/flies (and expands
        // ancestry so the node is lit); ?all=1 forces Show All.
        (function applyDeepLinks() {
          var params = new URLSearchParams(window.location.search);
          var q = params.get("q") || "";
          var sel = params.get("sel") || "";
          var all = params.get("all") === "1";
          if (all && !showAll) {
            showAll = true;
            computeLit();
            applyExpansionColors();
          }
          if (q) {
            $("search").value = q;
            runSearch(q.trim());
          }
          if (sel && graphIndex.has(sel)) {
            var node = graphIndex.get(sel);
            if (!showAll) {
              var ancestor = parentOf.get(sel);
              var guard = 0;
              while (ancestor && guard++ < 8) {
                if (CONTAINER_KINDS.has(graphIndex.get(ancestor).kind)) {
                  expanded.add(ancestor);
                }
                ancestor = parentOf.get(ancestor);
              }
              computeLit();
              applyExpansionColors();
            }
            showInspector(node);
            var pos = positions.get(sel);
            if (pos) {
              var cam2 = fg.camera();
              var baseHalfH2 = (cam2.top - cam2.bottom) / 2 / cam2.zoom;
              flyCamera({
                x: pos.x, y: pos.y,
                zoom: Math.max(0.3, Math.min(3, 900 / Math.max(1, baseHalfH2 * 2))),
              }, 700);
            }
          }
        })();

        var crates = 0;
        graph.nodes.forEach(function (n) { if (n.kind === "crate") crates++; });
        $("meta").textContent =
          graph.nodes.length.toLocaleString() + " nodes · " +
          graph.edges.length.toLocaleString() + " edges · " + crates + " crates";
        var loading = $("loading");
        loading.classList.add("spin");
        loading.style.transition = "opacity .5s";
        loading.style.opacity = "0";
        setTimeout(function () { loading.remove(); }, 600);
      })
      .catch(function (err) {
        $("loading-text").innerHTML = "Failed to load graph_ui.json: " + err.message;
      });
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", boot);
  } else {
    boot();
  }
})();
