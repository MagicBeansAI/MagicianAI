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

  return {
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
