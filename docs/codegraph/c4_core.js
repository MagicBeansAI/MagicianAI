/*
 * C4 Architecture Canvas — pure helpers (camera math, culling, URL state,
 * fuzzy search). No DOM access: shared between the browser canvas
 * (docs/codegraph/c4.js loads it via <script>) and node --test
 * (docs/codegraph/test/c4_core.test.mjs requires it).
 */
(function (root, factory) {
  if (typeof module === "object" && module.exports) {
    module.exports = factory();
  } else {
    root.C4Core = factory();
  }
})(typeof self !== "undefined" ? self : this, function () {
  "use strict";

  // Semantic zoom ladder: band name -> upper scale bound (exclusive).
  // Thresholds sit where the band's inverse-scaled LOD (c4.css) reads
  // comfortably: the runtime overview frames its ~5.6k-wide world layer
  // at z ~ 0.2, areas browsing lives around 0.4-1, code around 1.3-2.5.
  var BANDS = [
    { name: "runtime", max: 0.35 },
    { name: "areas", max: 1.2 },
    { name: "code", max: 3 },
    { name: "detail", max: Infinity },
  ];

  var SCALE_MIN = 0.02;
  var SCALE_MAX = 8;

  function bandForScale(scale) {
    for (var i = 0; i < BANDS.length; i++) {
      if (scale < BANDS[i].max) {
        return BANDS[i].name;
      }
    }
    return BANDS[BANDS.length - 1].name;
  }

  function clampScale(scale, min, max) {
    var lo = typeof min === "number" ? min : SCALE_MIN;
    var hi = typeof max === "number" ? max : SCALE_MAX;
    return Math.min(hi, Math.max(lo, scale));
  }

  // Camera: {x, y} = world coordinates at the viewport center, z = scale.
  function worldToScreen(wx, wy, cam, view) {
    return [(wx - cam.x) * cam.z + view.w / 2, (wy - cam.y) * cam.z + view.h / 2];
  }

  function screenToWorld(sx, sy, cam, view) {
    return [(sx - view.w / 2) / cam.z + cam.x, (sy - view.h / 2) / cam.z + cam.y];
  }

  function boxesIntersect(a, b, margin) {
    var m = margin || 0;
    return (
      a.x - m < b.x + b.w &&
      a.x + a.w + m > b.x &&
      a.y - m < b.y + b.h &&
      a.y + a.h + m > b.y
    );
  }

  function cullRects(rects, viewport, margin) {
    var kept = [];
    for (var i = 0; i < rects.length; i++) {
      if (boxesIntersect(viewport, rects[i], margin)) {
        kept.push(rects[i]);
      }
    }
    return kept;
  }

  var DEFAULT_STATE = { x: -470, y: -170, z: 0.2, sel: "", exp: [] };

  function encodeUrlState(state) {
    var params = [];
    params.push("x=" + Math.round(state.x * 100) / 100);
    params.push("y=" + Math.round(state.y * 100) / 100);
    params.push("z=" + Math.round(state.z * 10000) / 10000);
    if (state.sel) {
      params.push("sel=" + encodeURIComponent(state.sel));
    }
    if (state.exp && state.exp.length) {
      params.push("exp=" + encodeURIComponent(state.exp.join(",")));
    }
    return params.join("&");
  }

  function decodeUrlState(str) {
    var state = {
      x: DEFAULT_STATE.x,
      y: DEFAULT_STATE.y,
      z: DEFAULT_STATE.z,
      sel: DEFAULT_STATE.sel,
      exp: [],
    };
    if (!str) {
      return state;
    }
    var query = str.charAt(0) === "?" ? str.slice(1) : str;
    var parts = query.split("&");
    for (var i = 0; i < parts.length; i++) {
      var eq = parts[i].indexOf("=");
      if (eq < 0) {
        continue;
      }
      var key = parts[i].slice(0, eq);
      var value = decodeURIComponent(parts[i].slice(eq + 1));
      if (key === "x" || key === "y" || key === "z") {
        var num = parseFloat(value);
        if (!isNaN(num)) {
          state[key] = num;
        }
      } else if (key === "sel") {
        state.sel = value;
      } else if (key === "exp") {
        state.exp = value ? value.split(",").filter(Boolean) : [];
      }
    }
    state.z = clampScale(state.z);
    return state;
  }

  // Subsequence fuzzy score: higher is better. Consecutive characters and
  // word-start matches earn bonuses; null when needle is not a subsequence.
  function fuzzyScore(needle, haystack) {
    var n = String(needle || "").toLowerCase();
    var h = String(haystack || "").toLowerCase();
    if (!n) {
      return 0;
    }
    var score = 0;
    var hi = 0;
    var streak = 0;
    for (var i = 0; i < n.length; i++) {
      var ch = n[i];
      var found = -1;
      for (var j = hi; j < h.length; j++) {
        if (h[j] === ch) {
          found = j;
          break;
        }
      }
      if (found < 0) {
        return null;
      }
      streak = found === hi ? streak + 1 : 1;
      score += 1 + streak * 2;
      if (found === 0 || /[^a-z0-9]/.test(h[found - 1] || " ")) {
        score += 2; // word-start bonus
      }
      hi = found + 1;
    }
    return score;
  }

  // ---- Orthogonal edge routing ---------------------------------------------
  //
  // Routes an edge between two boxes around `obstacles` on a sparse grid
  // whose lines are the inflated obstacle borders, the corridor midlines
  // between them, and the endpoints themselves. A* minimises length plus a
  // bend penalty. The search starts and ends at (offset) box centres —
  // endpoints are not obstacles — and the result is clipped to the two box
  // borders, so the polyline leaves and enters each box exactly once.
  // Returns [[x, y], ...] (>= 2 points), or null when no route exists.

  function insideStrict(r, x, y) {
    return x > r.x && x < r.x + r.w && y > r.y && y < r.y + r.h;
  }

  function uniqSorted(values) {
    values.sort(function (a, b) { return a - b; });
    var out = [];
    for (var i = 0; i < values.length; i++) {
      if (!out.length || values[i] - out[out.length - 1] > 1) {
        out.push(values[i]);
      }
    }
    return out;
  }

  function withMidlines(lines) {
    var out = lines.slice();
    for (var i = 0; i + 1 < lines.length; i++) {
      out.push((lines[i] + lines[i + 1]) / 2);
    }
    return uniqSorted(out);
  }

  // Clip the polyline so it starts on `rect`'s border: drop points while
  // the path is still inside, then cut the exiting segment at the border.
  function clipStart(points, rect) {
    var i = 0;
    while (i + 1 < points.length && insideStrict(rect, points[i + 1][0], points[i + 1][1])) {
      i++;
    }
    if (i + 1 >= points.length) {
      return points.slice(i);
    }
    var a = points[i];
    var b = points[i + 1];
    var p;
    if (a[1] === b[1]) {
      p = [b[0] > a[0] ? rect.x + rect.w : rect.x, a[1]];
    } else {
      p = [a[0], b[1] > a[1] ? rect.y + rect.h : rect.y];
    }
    return [p].concat(points.slice(i + 1));
  }

  function simplify(points) {
    var out = [];
    for (var i = 0; i < points.length; i++) {
      var p = points[i];
      var prev = out[out.length - 1];
      if (prev && prev[0] === p[0] && prev[1] === p[1]) {
        continue;
      }
      if (out.length >= 2) {
        var q = out[out.length - 2];
        if ((q[0] === prev[0] && prev[0] === p[0]) || (q[1] === prev[1] && prev[1] === p[1])) {
          out[out.length - 1] = p;
          continue;
        }
      }
      out.push(p);
    }
    return out;
  }

  function routeOrthogonal(from, to, obstacles, opts) {
    var o = opts || {};
    var margin = o.margin == null ? 40 : o.margin;
    var bendCost = o.bendCost == null ? 400 : o.bendCost;
    var off = o.offset || 0;
    var inflated = obstacles.map(function (r) {
      return { x: r.x - margin, y: r.y - margin, w: r.w + margin * 2, h: r.h + margin * 2 };
    });
    var sx = from.x + from.w / 2 + off;
    var sy = from.y + from.h / 2 + off;
    var tx = to.x + to.w / 2 + off;
    var ty = to.y + to.h / 2 + off;

    var xsRaw = [sx, tx];
    var ysRaw = [sy, ty];
    inflated.forEach(function (r) {
      xsRaw.push(r.x, r.x + r.w);
      ysRaw.push(r.y, r.y + r.h);
    });
    var xs = withMidlines(uniqSorted(xsRaw));
    var ys = withMidlines(uniqSorted(ysRaw));
    var nx = xs.length;
    var ny = ys.length;

    function blockedPoint(x, y) {
      for (var i = 0; i < inflated.length; i++) {
        if (insideStrict(inflated[i], x, y)) {
          return true;
        }
      }
      return false;
    }
    var blocked = new Uint8Array(nx * ny);
    for (var yi = 0; yi < ny; yi++) {
      for (var xi = 0; xi < nx; xi++) {
        blocked[yi * nx + xi] = blockedPoint(xs[xi], ys[yi]) ? 1 : 0;
      }
    }
    function idxOf(arr, v) {
      var best = 0;
      for (var i = 1; i < arr.length; i++) {
        if (Math.abs(arr[i] - v) < Math.abs(arr[best] - v)) best = i;
      }
      return best;
    }
    var start = idxOf(ys, sy) * nx + idxOf(xs, sx);
    var goal = idxOf(ys, ty) * nx + idxOf(xs, tx);
    blocked[start] = 0;
    blocked[goal] = 0;

    // State = cell * 2 + axis (0 horizontal, 1 vertical) of the arriving move.
    var size = nx * ny * 2;
    var gScore = new Float64Array(size).fill(Infinity);
    var came = new Int32Array(size).fill(-1);
    var closed = new Uint8Array(size);
    var heap = [];
    function push(f, s) {
      heap.push([f, s]);
      var i = heap.length - 1;
      while (i > 0) {
        var p = (i - 1) >> 1;
        if (heap[p][0] <= heap[i][0]) break;
        var t = heap[p]; heap[p] = heap[i]; heap[i] = t;
        i = p;
      }
    }
    function pop() {
      var top = heap[0];
      var last = heap.pop();
      if (heap.length) {
        heap[0] = last;
        var i = 0;
        for (;;) {
          var l = i * 2 + 1, r = l + 1, m = i;
          if (l < heap.length && heap[l][0] < heap[m][0]) m = l;
          if (r < heap.length && heap[r][0] < heap[m][0]) m = r;
          if (m === i) break;
          var t = heap[m]; heap[m] = heap[i]; heap[i] = t;
          i = m;
        }
      }
      return top;
    }
    var gx = goal % nx;
    var gy = Math.floor(goal / nx);
    function h(cell) {
      return Math.abs(xs[cell % nx] - xs[gx]) + Math.abs(ys[Math.floor(cell / nx)] - ys[gy]);
    }
    [0, 1].forEach(function (axis) {
      gScore[start * 2 + axis] = 0;
      push(h(start), start * 2 + axis);
    });
    var found = -1;
    while (heap.length) {
      var cur = pop()[1];
      if (closed[cur]) continue;
      closed[cur] = 1;
      var cell = cur >> 1;
      var axis = cur & 1;
      if (cell === goal) { found = cur; break; }
      var cx = cell % nx;
      var cy = Math.floor(cell / nx);
      var moves = [[cx - 1, cy, 0], [cx + 1, cy, 0], [cx, cy - 1, 1], [cx, cy + 1, 1]];
      for (var k = 0; k < 4; k++) {
        var mx = moves[k][0], my = moves[k][1], maxis = moves[k][2];
        if (mx < 0 || my < 0 || mx >= nx || my >= ny) continue;
        var ncell = my * nx + mx;
        if (blocked[ncell]) continue;
        // Midpoint test catches an obstacle sitting between two free lines.
        if (blockedPoint((xs[cx] + xs[mx]) / 2, (ys[cy] + ys[my]) / 2)) continue;
        var step = Math.abs(xs[mx] - xs[cx]) + Math.abs(ys[my] - ys[cy]);
        var g = gScore[cur] + step + (maxis !== axis ? bendCost : 0);
        var ns = ncell * 2 + maxis;
        if (g < gScore[ns]) {
          gScore[ns] = g;
          came[ns] = cur;
          push(g + h(ncell), ns);
        }
      }
    }
    if (found < 0) {
      return null;
    }
    var pts = [];
    for (var s = found; s >= 0; s = came[s]) {
      var c = s >> 1;
      pts.push([xs[c % nx], ys[Math.floor(c / nx)]]);
    }
    pts.reverse();
    pts = simplify(pts);
    pts = clipStart(pts, from);
    pts = clipStart(pts.slice().reverse(), to).reverse();
    return simplify(pts);
  }

  // Point and axis of the longest segment's midpoint — where a label reads
  // best and is least likely to sit on a bend.
  function labelAnchor(points) {
    var best = null;
    var bestLen = -1;
    for (var i = 0; i + 1 < points.length; i++) {
      var a = points[i], b = points[i + 1];
      var len = Math.abs(b[0] - a[0]) + Math.abs(b[1] - a[1]);
      if (len > bestLen) {
        bestLen = len;
        best = [(a[0] + b[0]) / 2, (a[1] + b[1]) / 2];
      }
    }
    return best || points[0];
  }

  return {
    routeOrthogonal: routeOrthogonal,
    labelAnchor: labelAnchor,
    BANDS: BANDS,
    SCALE_MIN: SCALE_MIN,
    SCALE_MAX: SCALE_MAX,
    bandForScale: bandForScale,
    clampScale: clampScale,
    worldToScreen: worldToScreen,
    screenToWorld: screenToWorld,
    boxesIntersect: boxesIntersect,
    cullRects: cullRects,
    encodeUrlState: encodeUrlState,
    decodeUrlState: decodeUrlState,
    fuzzyScore: fuzzyScore,
  };
});
