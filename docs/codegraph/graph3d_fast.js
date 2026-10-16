/*
 * FastGraph — a GPU-batched drop-in renderer core for graph3d.js.
 *
 * Replaces ForceGraph3D's scene management for large graphs. Instead of
 * one Three.js object per node and per link (115k + 1.46M objects, each
 * walked every frame), everything renders in a handful of draw calls:
 *
 *   nodes       → one THREE.Points      (custom shader, per-vertex size/color)
 *   edges       → one THREE.LineSegments (per-vertex colors, built lazily)
 *   flow edges  → one small LineSegments (rebuilt when a /flows view is on)
 *   particles   → one small Points       (selection / flow directionality)
 *
 * Game-style behaviour, adapted for a point-cloud world:
 *   - clip-space culling: the GPU rejects whatever the camera cannot see;
 *     there are no per-node JS objects to walk, so the frame cost is
 *     independent of graph size.
 *   - on-demand rendering: frames render only when the camera moves, a
 *     tween/pulse/particle animates, or something explicitly calls wake().
 *   - pixelRatio capped at 2 to stop retina overdraw.
 *
 * The public surface mirrors the subset of the ForceGraph3D API that
 * graph3d.js uses (chainable accessors, camera()/controls()/scene()/
 * renderer()/cameraPosition()/graphData()), so the rest of that file —
 * selection, flows, fly mode, legend filters, themes — keeps working.
 *
 * Edge styling lambdas (linkVisibility/linkColor/linkWidth/particles) are
 * re-evaluated in coalesced, frame-chunked passes instead of every engine
 * tick; with the color-string cache the whole 1.46M-edge repaint is a few
 * frames of background work rather than a stall.
 */
(function (root) {
  "use strict";

  var COLOR_CACHE = new Map();
  function cachedColor(css) {
    var c = COLOR_CACHE.get(css);
    if (!c) {
      c = new THREE.Color(css);
      COLOR_CACHE.set(css, c);
    }
    return c;
  }

  var NODES_VERT = [
    "attribute float aSize;",
    "attribute vec3 aColor;",
    "varying vec3 vColor;",
    "uniform float uProj;",  // device-px perspective factor: h / (2*tan(fov/2))
    "uniform float uScale;", // global size boost (nodeRelSize * 3)
    "void main() {",
    "  vColor = aColor;",
    "  vec4 mv = modelViewMatrix * vec4(position, 1.0);",
    "  float d = max(1.0, -mv.z);",
    // Proportions follow the old renderer's geometry (radius = cbrt(val)
    // perspective-projected) with a global boost so the default overview
    // reads as visible dots instead of a subpixel starfield.
    "  float radius = pow(aSize, 0.3333) * uScale;",
    "  gl_PointSize = clamp(radius * uProj / d, 3.5, 60.0);",
    "  gl_Position = projectionMatrix * mv;",
    "}",
  ].join("\n");

  var NODES_FRAG = [
    // Fake-sphere shading: interpret the point sprite as a lit ball
    // (normal from sprite coords, fixed headlight) so large kinds read
    // as spheres up close instead of flat colored circles.
    "varying vec3 vColor;",
    "uniform float uOpacity;",
    "void main() {",
    "  vec2 c = gl_PointCoord * 2.0 - 1.0;",
    "  float r2 = dot(c, c);",
    "  if (r2 > 1.0) discard;",
    "  vec3 n = vec3(c.x, -c.y, sqrt(max(0.0, 1.0 - r2)));",
    "  vec3 L = normalize(vec3(0.4, 0.55, 0.75));",
    "  float diff = 0.42 + 0.58 * max(dot(n, L), 0.0);",
    "  float spec = pow(max(dot(reflect(-L, n), vec3(0.0, 0.0, 1.0)), 0.0), 28.0) * 0.45;",
    "  float rim = 1.0 - smoothstep(0.9, 1.0, sqrt(r2));",
    "  vec3 col = vColor * diff + vec3(spec);",
    "  gl_FragColor = vec4(col, uOpacity * rim);",
    "}",
  ].join("\n");

  var PARTICLE_VERT = [
    "attribute vec3 aColor;",
    "varying vec3 vColor;",
    "uniform float uPx;",
    "void main() {",
    "  vColor = aColor;",
    "  vec4 mv = modelViewMatrix * vec4(position, 1.0);",
    "  gl_PointSize = clamp(700.0 / max(1.0, -mv.z), 3.0, 14.0) * uPx;",
    "  gl_Position = projectionMatrix * mv;",
    "}",
  ].join("\n");

  function FastGraph(_opts) {
    var container = null;

    // ── scene basics ────────────────────────────────────────
    var renderer = null;
    var scene = new THREE.Scene();
    var useOrtho = _opts && _opts.camera === "ortho";
    var camera = useOrtho
      ? new THREE.OrthographicCamera(-1000, 1000, 600, -600, 0.1, 100000)
      : new THREE.PerspectiveCamera(60, 1, 0.5, 60000);
    if (useOrtho) {
      camera.position.set(0, 0, 10000);
    } else {
      camera.position.set(0, 0, 2200);
    }

    var nodesMesh = null;      // THREE.Points — all nodes
    var edgeMesh = null;       // THREE.LineSegments — all edges (lazy)
    var flowMesh = null;       // THREE.LineSegments — active /flows subset
    var particleMesh = null;   // THREE.Points — directional particles
    var tooltip = null;
    var fpsEl = null;

    var nodes = [];
    var links = [];
    var nodeIdToIndex = new Map();
    var edgeColorsDirty = true;
    var edgeChunkCursor = 0;   // chunked repaint cursor
    var edgeColorsPending = false;
    var nodeColorsPending = false;

    var fns = {
      nodeVal: function () { return 1; },
      nodeColor: function () { return "#cccccc"; },
      nodeLabel: function (n) { return n.label; },
      linkVisibility: function () { return true; },
      linkWidth: function () { return 0; },
      linkColor: function () { return "#3a6a8a"; },
      linkDirectionalParticles: function () { return 0; },
      linkDirectionalParticleColor: function () { return "#00e5ff"; },
    };
    var cfg = {
      nodeRelSize: 1,
      nodeOpacity: 0.9,
      linkOpacity: 0.12,
      particleWidth: 2,
      particleSpeed: 0.012,
      bgColor: "#000005",
    };

    // particles state: [{edge, t}] with positions written each frame
    var particleAnims = [];
    var particleColor = "#00e5ff";

    // ── on-demand render loop ───────────────────────────────
    var dirty = true;
    var lastCamKey = "";
    var rafId = 0;
    var frames = 0;
    var fpsT0 = 0;

    function wake() {
      dirty = true;
    }

    function camKey() {
      var p = camera.position;
      var t = controls.target;
      return p.x.toFixed(2) + "," + p.y.toFixed(2) + "," + p.z.toFixed(2) +
        "," + t.x.toFixed(2) + "," + t.y.toFixed(2) + "," + t.z.toFixed(2);
    }

    function loop() {
      rafId = requestAnimationFrame(loop);
      controls.update();
      if (particleAnims.length) animateParticles();
      var key = camKey();
      if (key !== lastCamKey) {
        lastCamKey = key;
        dirty = true;
      }
      if (!dirty) {
        trackFps(false);
        return;
      }
      dirty = false;
      renderer.render(scene, camera);
      trackFps(true);
    }

    function trackFps(rendered) {
      frames += 1;
      var now = performance.now();
      if (!fpsT0) fpsT0 = now;
      if (now - fpsT0 >= 500) {
        var fps = Math.round(frames * 1000 / (now - fpsT0));
        if (fpsEl) {
          var info = renderer.info.render;
          fpsEl.textContent = fps + " fps · " + info.calls + " draw calls · " +
            (rendered ? "rendering" : "idle");
        }
        frames = 0;
        fpsT0 = now;
      }
    }

    // ── buffers ─────────────────────────────────────────────

    function disposeMesh(mesh) {
      if (!mesh) return;
      scene.remove(mesh);
      mesh.geometry.dispose();
      if (mesh.material.dispose) mesh.material.dispose();
    }

    function buildNodes() {
      disposeMesh(nodesMesh);
      nodeIdToIndex.clear();
      var n = nodes.length;
      var positions = new Float32Array(n * 3);
      var colors = new Float32Array(n * 3);
      var sizes = new Float32Array(n);
      for (var i = 0; i < n; i++) {
        var node = nodes[i];
        nodeIdToIndex.set(node.id, i);
        positions[i * 3] = node.x || 0;
        positions[i * 3 + 1] = node.y || 0;
        positions[i * 3 + 2] = node.z || 0;
        sizes[i] = Number(fns.nodeVal(node)) || 1;
        var c = cachedColor(String(fns.nodeColor(node)));
        colors[i * 3] = c.r;
        colors[i * 3 + 1] = c.g;
        colors[i * 3 + 2] = c.b;
      }
      var geo = new THREE.BufferGeometry();
      geo.setAttribute("position", new THREE.BufferAttribute(positions, 3));
      geo.setAttribute("aColor", new THREE.BufferAttribute(colors, 3));
      geo.setAttribute("aSize", new THREE.BufferAttribute(sizes, 1));
      var mat = new THREE.ShaderMaterial({
        vertexShader: NODES_VERT,
        fragmentShader: NODES_FRAG,
        transparent: true,
        uniforms: {
          uProj: { value: projFactor() },
          uScale: { value: cfg.nodeRelSize * 3 },
          uOpacity: { value: cfg.nodeOpacity },
        },
      });
      nodesMesh = new THREE.Points(geo, mat);
      nodesMesh.frustumCulled = false; // one global cloud: the GPU clips it
      scene.add(nodesMesh);
    }

    // Device-pixel perspective factor: how many pixels one world unit of
    // radius spans at distance 1 for this viewport and fov.
    function projFactor() {
      var fovRad = (camera.fov || 60) * Math.PI / 180;
      var h = renderer ? renderer.domElement.height : 720;
      return h / (2 * Math.tan(fovRad / 2));
    }

    function updateNodeColors() {
      if (!nodesMesh) return;
      var attr = nodesMesh.geometry.attributes.aColor;
      var arr = attr.array;
      for (var i = 0; i < nodes.length; i++) {
        var c = cachedColor(String(fns.nodeColor(nodes[i])));
        arr[i * 3] = c.r;
        arr[i * 3 + 1] = c.g;
        arr[i * 3 + 2] = c.b;
      }
      attr.needsUpdate = true;
      wake();
    }

    // Edges are built lazily: they default to hidden and building the
    // 2.9M-vertex buffer is real work, so skip it until first needed.
    function ensureEdgeMesh() {
      if (edgeMesh) return true;
      var pos = nodesMesh.geometry.attributes.position.array;
      var m = links.length;
      var edgePos = new Float32Array(m * 6);
      var edgeCol = new Float32Array(m * 6);
      var ok = true;
      for (var i = 0; i < m; i++) {
        var l = links[i];
        var a = nodeIdToIndex.get(l.source);
        var b = nodeIdToIndex.get(l.target);
        if (a === undefined || b === undefined) { ok = false; break; }
        edgePos[i * 6] = pos[a * 3];
        edgePos[i * 6 + 1] = pos[a * 3 + 1];
        edgePos[i * 6 + 2] = pos[a * 3 + 2];
        edgePos[i * 6 + 3] = pos[b * 3];
        edgePos[i * 6 + 4] = pos[b * 3 + 1];
        edgePos[i * 6 + 5] = pos[b * 3 + 2];
      }
      if (!ok) return false;
      var geo = new THREE.BufferGeometry();
      geo.setAttribute("position", new THREE.BufferAttribute(edgePos, 3));
      geo.setAttribute("color", new THREE.BufferAttribute(edgeCol, 3));
      var mat = new THREE.LineBasicMaterial({
        vertexColors: true,
        transparent: true,
        opacity: cfg.linkOpacity,
      });
      edgeMesh = new THREE.LineSegments(geo, mat);
      edgeMesh.frustumCulled = false;
      edgeMesh.visible = false;
      scene.add(edgeMesh);
      return true;
    }

    // Chunked edge repaint: evaluates the styling lambdas across frames so
    // a selection change never stalls the main thread.
    function edgeChunk() {
      if (!edgeMesh || edgeChunkCursor >= links.length) {
        edgeColorsPending = false;
        rebuildParticles();
        wake();
        return;
      }
      var colArr = edgeMesh.geometry.attributes.color.array;
      var end = Math.min(links.length, edgeChunkCursor + 280000);
      for (var i = edgeChunkCursor; i < end; i++) {
        var l = links[i];
        var c;
        try {
          c = fns.linkVisibility(l) === false
            ? BLACK
            : cachedColor(String(fns.linkColor(l)));
        } catch (_err) {
          c = BLACK;
        }
        colArr[i * 6] = c.r;
        colArr[i * 6 + 1] = c.g;
        colArr[i * 6 + 2] = c.b;
        colArr[i * 6 + 3] = c.r;
        colArr[i * 6 + 4] = c.g;
        colArr[i * 6 + 5] = c.b;
      }
      edgeChunkCursor = end;
      edgeMesh.geometry.attributes.color.needsUpdate = true;
      wake();
      requestAnimationFrame(edgeChunk);
    }

    var BLACK = new THREE.Color("#000000");

    function scheduleEdgeRestyle() {
      if (!edgeMesh) return;
      edgeChunkCursor = 0;
      if (edgeColorsPending) return;
      edgeColorsPending = true;
      requestAnimationFrame(edgeChunk);
    }

    function updateEdgeMode() {
      // Probe the visibility lambda on a representative slice to learn
      // the current mode cheaply (all-on vs flow-subset vs all-off).
      var probe = links.length ? links[Math.floor(links.length / 2)] : null;
      var visible = probe ? fns.linkVisibility(probe) !== false : true;
      if (!visible) {
        // Flow mode (or off): only a subset is visible — rebuild the small
        // flow buffer instead of restyling 1.46M edges.
        rebuildFlowMesh();
        if (edgeMesh) edgeMesh.visible = false;
        return;
      }
      if (!ensureEdgeMesh()) return;
      edgeMesh.visible = true;
      disposeMesh(flowMesh);
      flowMesh = null;
      scheduleEdgeRestyle();
    }

    function rebuildFlowMesh() {
      disposeMesh(flowMesh);
      flowMesh = null;
      var pos = nodesMesh ? nodesMesh.geometry.attributes.position.array : null;
      if (!pos) return;
      var pts = [];
      var cols = [];
      var flowColor = cachedColor(String(fns.linkDirectionalParticleColor(links[0] || {})));
      var defaultEdge = cachedColor(String(fns.linkColor(links[0] || {})));
      for (var i = 0; i < links.length; i++) {
        var l = links[i];
        var vis = false;
        try { vis = fns.linkVisibility(l) !== false; } catch (_err) { vis = false; }
        if (!vis) continue;
        var a = nodeIdToIndex.get(l.source);
        var b = nodeIdToIndex.get(l.target);
        if (a === undefined || b === undefined) continue;
        var c = defaultEdge;
        try { c = cachedColor(String(fns.linkColor(l))); } catch (_err) { /* keep */ }
        pts.push(pos[a * 3], pos[a * 3 + 1], pos[a * 3 + 2], pos[b * 3], pos[b * 3 + 1], pos[b * 3 + 2]);
        cols.push(c.r, c.g, c.b, c.r, c.g, c.b);
      }
      if (!pts.length) return;
      var geo = new THREE.BufferGeometry();
      geo.setAttribute("position", new THREE.Float32BufferAttribute(pts, 3));
      geo.setAttribute("color", new THREE.Float32BufferAttribute(cols, 3));
      var mat = new THREE.LineBasicMaterial({
        vertexColors: true,
        transparent: true,
        opacity: Math.max(cfg.linkOpacity, 0.5),
      });
      flowMesh = new THREE.LineSegments(geo, mat);
      flowMesh.frustumCulled = false;
      scene.add(flowMesh);
    }

    var lastParticleTotal = 0;

    function rebuildParticles() {
      // Restyles (selection/expansion recolors) re-run this pass; the
      // full-link scan is only worth it when particles were live.
      if (lastParticleTotal === 0 && !particleMesh) {
        var quickTotal = 0;
        for (var qi = 0; qi < links.length && quickTotal < 1; qi++) {
          try {
            quickTotal = Number(fns.linkDirectionalParticles(links[qi])) || 0;
          } catch (_qerr) { quickTotal = 0; }
        }
        if (quickTotal === 0) return;
      }
      particleAnims = [];
      disposeMesh(particleMesh);
      particleMesh = null;
      var pos = nodesMesh ? nodesMesh.geometry.attributes.position.array : null;
      if (!pos) return;
      var total = 0;
      for (var i = 0; i < links.length && total < 4000; i++) {
        var count = 0;
        try { count = Number(fns.linkDirectionalParticles(links[i])) || 0; } catch (_err) { count = 0; }
        if (count <= 0) continue;
        var a = nodeIdToIndex.get(links[i].source);
        var b = nodeIdToIndex.get(links[i].target);
        if (a === undefined || b === undefined) continue;
        for (var p = 0; p < count && total < 4000; p++) {
          particleAnims.push({
            a: a, b: b,
            t: Math.random(),
            speed: cfg.particleSpeed,
          });
          total++;
        }
      }
      lastParticleTotal = particleAnims.length;
      if (!particleAnims.length) return;
      var positions = new Float32Array(particleAnims.length * 3);
      var colors = new Float32Array(particleAnims.length * 3);
      var c = cachedColor(String(fns.linkDirectionalParticleColor(links[0] || {})));
      for (var k = 0; k < particleAnims.length; k++) {
        colors[k * 3] = c.r;
        colors[k * 3 + 1] = c.g;
        colors[k * 3 + 2] = c.b;
      }
      var geo = new THREE.BufferGeometry();
      geo.setAttribute("position", new THREE.BufferAttribute(positions, 3));
      geo.setAttribute("aColor", new THREE.BufferAttribute(colors, 3));
      var mat = new THREE.ShaderMaterial({
        vertexShader: PARTICLE_VERT,
        fragmentShader: NODES_FRAG,
        transparent: true,
        blending: THREE.AdditiveBlending,
        depthWrite: false,
        uniforms: {
          uPx: { value: renderer.getPixelRatio() },
          uOpacity: { value: 1.0 },
        },
      });
      particleMesh = new THREE.Points(geo, mat);
      particleMesh.frustumCulled = false;
      scene.add(particleMesh);
      animateParticles();
    }

    function animateParticles() {
      if (!particleMesh) return;
      var posArr = particleMesh.geometry.attributes.position.array;
      var nodesPos = nodesMesh.geometry.attributes.position.array;
      for (var i = 0; i < particleAnims.length; i++) {
        var p = particleAnims[i];
        p.t += p.speed;
        if (p.t > 1) p.t -= 1;
        var a = p.a * 3;
        var b = p.b * 3;
        posArr[i * 3] = nodesPos[a] + (nodesPos[b] - nodesPos[a]) * p.t;
        posArr[i * 3 + 1] = nodesPos[a + 1] + (nodesPos[b + 1] - nodesPos[a + 1]) * p.t;
        posArr[i * 3 + 2] = nodesPos[a + 2] + (nodesPos[b + 2] - nodesPos[a + 2]) * p.t;
      }
      particleMesh.geometry.attributes.position.needsUpdate = true;
      wake();
    }

    // ── picking ─────────────────────────────────────────────

    var raycaster = new THREE.Raycaster();
    var pointer = new THREE.Vector2();
    var hoverNode = null;
    var lastPickAt = 0;

    function pickNode(clientX, clientY) {
      if (!nodesMesh || !container) return null;
      var rect = container.getBoundingClientRect();
      pointer.x = ((clientX - rect.left) / rect.width) * 2 - 1;
      pointer.y = -((clientY - rect.top) / rect.height) * 2 + 1;
      raycaster.setFromCamera(pointer, camera);
      if (camera.isOrthographicCamera) {
        // Screen-anchored pick radius: 8 device px in world units.
        var wpx = renderer.domElement.clientWidth || 1;
        raycaster.params.Points.threshold = ((camera.right - camera.left) / wpx) * 8;
      } else {
        var dist = camera.position.length();
        raycaster.params.Points.threshold = Math.max(1.5, dist * 0.006);
      }
      var hits = raycaster.intersectObject(nodesMesh);
      if (!hits.length) return null;
      return nodes[hits[0].index] || null;
    }

    function showTooltip(node, clientX, clientY) {
      if (!tooltip) return;
      if (!node) {
        tooltip.style.display = "none";
        return;
      }
      var text = "";
      try { text = String(fns.nodeLabel(node)); } catch (_err) { text = node.label; }
      tooltip.textContent = text;
      tooltip.style.display = "block";
      tooltip.style.left = (clientX + 14) + "px";
      tooltip.style.top = (clientY + 10) + "px";
    }

    function bindPointer() {
      var dom = renderer.domElement;
      var downAt = null;

      dom.addEventListener("pointerdown", function (e) {
        downAt = { x: e.clientX, y: e.clientY };
      });

      dom.addEventListener("pointerup", function (e) {
        if (!downAt) return;
        var moved = Math.abs(e.clientX - downAt.x) + Math.abs(e.clientY - downAt.y);
        downAt = null;
        if (moved > 5) return; // it was a drag
        var node = pickNode(e.clientX, e.clientY);
        if (node && api.onNodeClickFn) api.onNodeClickFn(node);
        else if (!node && api.onBackgroundClickFn) api.onBackgroundClickFn();
      });

      dom.addEventListener("mousemove", function (e) {
        var now = performance.now();
        if (now - lastPickAt < 60) return;
        lastPickAt = now;
        if (document.pointerLockElement === dom) {
          showTooltip(null);
          return;
        }
        var node = pickNode(e.clientX, e.clientY);
        if (node !== hoverNode) {
          hoverNode = node;
          if (api.onNodeHoverFn) api.onNodeHoverFn(node);
          dom.style.cursor = node ? "pointer" : "default";
        }
        showTooltip(node, e.clientX, e.clientY);
      });

      dom.addEventListener("mouseleave", function () {
        hoverNode = null;
        showTooltip(null);
        if (api.onNodeHoverFn) api.onNodeHoverFn(null);
      });
    }

    // ── orbit controls (OrbitControls-compatible subset) ─────

    // Pan-dominant 2D navigation for the orthographic plane mode:
    // left-drag pans, wheel zooms toward the cursor, no orbit.
    function buildOrthoControls() {
      var oDrag = null;
      function bindOrtho() {
        var dom = renderer.domElement;
        dom.addEventListener("contextmenu", function (e) { e.preventDefault(); });
        dom.addEventListener("pointerdown", function (e) {
          oDrag = { x: e.clientX, y: e.clientY };
        });
        window.addEventListener("pointerup", function () { oDrag = null; });
        window.addEventListener("pointermove", function (e) {
          if (!oDrag || !ortho.enabled) return;
          var dx = e.clientX - oDrag.x;
          var dy = e.clientY - oDrag.y;
          oDrag.x = e.clientX;
          oDrag.y = e.clientY;
          var worldPerPx = (camera.right - camera.left) / (renderer.domElement.clientWidth || 1);
          camera.position.x -= dx * worldPerPx;
          camera.position.y += dy * worldPerPx;
          wake();
        });
        dom.addEventListener("wheel", function (e) {
          if (!ortho.enabled) return;
          e.preventDefault();
          var rect = container.getBoundingClientRect();
          var px = e.clientX - rect.left;
          var py = e.clientY - rect.top;
          var halfW = (camera.right - camera.left) / 2;
          var halfH = (camera.top - camera.bottom) / 2;
          var wx = camera.position.x + (px / rect.width - 0.5) * 2 * halfW;
          var wy = camera.position.y - (py / rect.height - 0.5) * 2 * halfH;
          var factor = Math.exp(-e.deltaY * 0.0012);
          camera.zoom = Math.max(0.02, Math.min(400, camera.zoom * factor));
          camera.updateProjectionMatrix();
          var halfW2 = (camera.right - camera.left) / 2;
          var halfH2 = (camera.top - camera.bottom) / 2;
          var wx2 = camera.position.x + (px / rect.width - 0.5) * 2 * halfW2;
          var wy2 = camera.position.y - (py / rect.height - 0.5) * 2 * halfH2;
          camera.position.x += wx - wx2;
          camera.position.y += wy - wy2;
          wake();
        }, { passive: false });
      }
      var ortho = {
        target: { x: 0, y: 0, z: 0 },
        enabled: true,
        enableDamping: false,
        update: function () {
          camera.lookAt(camera.position.x, camera.position.y, 0);
          wake();
        },
        _bind: bindOrtho,
      };
      return ortho;
    }

    var controls = useOrtho ? buildOrthoControls() : (function () {
      var target = new THREE.Vector3(0, 0, 0);
      var spherical = { radius: 2200, theta: 0, phi: Math.PI / 2 };
      var vel = { theta: 0, phi: 0, radius: 0, panX: 0, panY: 0 };
      var state = {
        target: target,
        enabled: true,
        enableDamping: true,
        dampingFactor: 0.12,
        rotateSpeed: 0.005,
        zoomSpeed: 0.0012,
        panSpeed: 1.1,
      };
      var drag = null;

      function syncFromCamera() {
        var offset = new THREE.Vector3().subVectors(camera.position, target);
        spherical.radius = Math.max(1, offset.length());
        spherical.theta = Math.atan2(offset.x, offset.z);
        spherical.phi = Math.acos(Math.max(-1, Math.min(1, offset.y / spherical.radius)));
      }

      function applyToCamera() {
        var sinPhi = Math.sin(spherical.phi);
        camera.position.set(
          target.x + spherical.radius * sinPhi * Math.sin(spherical.theta),
          target.y + spherical.radius * Math.cos(spherical.phi),
          target.z + spherical.radius * sinPhi * Math.cos(spherical.theta)
        );
      }

      function update() {
        if (state.enabled) {
          syncFromCamera();
          spherical.theta += vel.theta;
          spherical.phi = Math.max(0.05, Math.min(Math.PI - 0.05, spherical.phi + vel.phi));
          spherical.radius = Math.max(2, spherical.radius * (1 + vel.radius));
          if (state.enableDamping) {
            vel.theta *= 1 - state.dampingFactor;
            vel.phi *= 1 - state.dampingFactor;
            vel.radius *= 1 - state.dampingFactor;
            vel.panX *= 1 - state.dampingFactor;
            vel.panY *= 1 - state.dampingFactor;
            if (Math.abs(vel.theta) < 1e-7) vel.theta = 0;
            if (Math.abs(vel.phi) < 1e-7) vel.phi = 0;
            if (Math.abs(vel.radius) < 1e-7) vel.radius = 0;
          } else {
            vel.theta = vel.phi = vel.radius = 0;
          }
          applyToCamera();
        }
        camera.lookAt(target);
      }

      function bind() {
        var dom = renderer.domElement;
        dom.addEventListener("contextmenu", function (e) { e.preventDefault(); });
        dom.addEventListener("pointerdown", function (e) {
          drag = { button: e.button, x: e.clientX, y: e.clientY, ctrl: e.ctrlKey };
        });
        window.addEventListener("pointerup", function () { drag = null; });
        window.addEventListener("pointermove", function (e) {
          if (!drag || !state.enabled) return;
          var dx = e.clientX - drag.x;
          var dy = e.clientY - drag.y;
          drag.x = e.clientX;
          drag.y = e.clientY;
          if (drag.button === 0 && !drag.ctrl) {
            // Direct manipulation for immediate response; the stored
            // velocity becomes release-inertia that update() damps out.
            syncFromCamera();
            spherical.theta += -dx * state.rotateSpeed;
            spherical.phi = Math.max(0.05, Math.min(Math.PI - 0.05, spherical.phi + -dy * state.rotateSpeed));
            applyToCamera();
            camera.lookAt(target);
            vel.theta = -dx * state.rotateSpeed * 0.25;
            vel.phi = -dy * state.rotateSpeed * 0.25;
            wake();
          } else if (drag.button === 2 || drag.ctrl) {
            // pan: shift target along the camera's right/up axes
            var dist = camera.position.distanceTo(target);
            var right = new THREE.Vector3().setFromMatrixColumn(camera.matrix, 0);
            var up = new THREE.Vector3().setFromMatrixColumn(camera.matrix, 1);
            target.add(right.multiplyScalar(-dx * dist * 0.0011 * state.panSpeed));
            target.add(up.multiplyScalar(dy * dist * 0.0011 * state.panSpeed));
            wake();
          }
        });
        dom.addEventListener("wheel", function (e) {
          if (!state.enabled) return;
          e.preventDefault();
          vel.radius += Math.sign(e.deltaY) * state.zoomSpeed * Math.min(3, Math.abs(e.deltaY) / 100);
        }, { passive: false });
      }

      return {
        target: target,
        get enabled() { return state.enabled; },
        set enabled(v) { state.enabled = v; },
        get enableDamping() { return state.enableDamping; },
        set enableDamping(v) { state.enableDamping = v; },
        dampingFactor: state.dampingFactor,
        update: update,
        syncFromCamera: syncFromCamera,
        _bind: bind,
        _state: state,
      };
    })();

    // ── public API (ForceGraph3D-compatible subset) ──────────

    var camTween = 0;

    function cameraPosition(pos, lookAt, ms) {
      var to = pos || {};
      if (!ms) {
        camera.position.set(to.x || 0, to.y || 0, to.z || 0);
        if (lookAt) {
          controls.target.set(lookAt.x || 0, lookAt.y || 0, lookAt.z || 0);
        }
        camera.lookAt(controls.target);
        wake();
        return;
      }
      cancelAnimationFrame(camTween);
      var from = camera.position.clone();
      var fromLook = controls.target.clone();
      var toLook = lookAt ? new THREE.Vector3(lookAt.x || 0, lookAt.y || 0, lookAt.z || 0) : fromLook.clone();
      var toVec = new THREE.Vector3(to.x || 0, to.y || 0, to.z || 0);
      var t0 = performance.now();
      (function step() {
        var t = Math.min(1, (performance.now() - t0) / ms);
        var ease = 1 - Math.pow(1 - t, 3);
        camera.position.lerpVectors(from, toVec, ease);
        controls.target.lerpVectors(fromLook, toLook, ease);
        camera.lookAt(controls.target);
        wake();
        if (t < 1) camTween = requestAnimationFrame(step);
      })();
    }

    function graphData(data) {
      nodes = (data && data.nodes) || [];
      links = (data && data.links) || [];
      nodeIdToIndex.clear();
      disposeMesh(edgeMesh); edgeMesh = null;
      disposeMesh(flowMesh); flowMesh = null;
      disposeMesh(particleMesh); particleMesh = null;
      particleAnims = [];
      buildNodes();
      updateEdgeMode();
      wake();
      return api;
    }

    function resize() {
      if (!container) return;
      // The container is a fullscreen fixed element (#3d-graph). Take the
      // max of the measured and window dimensions: a measurement taken
      // during a transient pre-layout pass otherwise pins the canvas to
      // the THREE default 300x150 and squashes the whole view.
      var w = Math.max(container.clientWidth || 0, window.innerWidth);
      var h = Math.max(container.clientHeight || 0, window.innerHeight);
      renderer.setSize(w, h);
      if (camera.isOrthographicCamera) {
        var halfH = (camera.top - camera.bottom) / 2;
        var halfW = halfH * (w / h);
        camera.left = -halfW;
        camera.right = halfW;
      } else {
        camera.aspect = w / h;
      }
      camera.updateProjectionMatrix();
      if (nodesMesh) {
        nodesMesh.material.uniforms.uProj.value = projFactor();
      }
      wake();
    }

    var api = {
      onNodeClickFn: null,
      onNodeHoverFn: null,
      onBackgroundClickFn: null,

      graphData: graphData,
      camera: function () { return camera; },
      controls: function () { return controls; },
      renderer: function () { return renderer; },
      scene: function () { return scene; },
      cameraPosition: function (pos, lookAt, ms) { cameraPosition(pos, lookAt, ms); return api; },
      wake: wake,
      refresh: function () {
        updateNodeColors();
        updateEdgeMode();
        return api;
      },
      refreshSizes: function () {
        if (!nodesMesh) return api;
        var attr = nodesMesh.geometry.attributes.aSize;
        for (var i = 0; i < nodes.length; i++) {
          attr.array[i] = Number(fns.nodeVal(nodes[i])) || 1;
        }
        attr.needsUpdate = true;
        wake();
        return api;
      },

      // chainable config setters
      backgroundColor: function (c) { cfg.bgColor = c; renderer.setClearColor(new THREE.Color(c), 1); return api; },
      showNavInfo: function () { return api; },
      nodeRelSize: function (v) {
        cfg.nodeRelSize = v;
        if (nodesMesh) nodesMesh.material.uniforms.uScale.value = v * 3;
        return api;
      },
      nodeVal: function (fn) { fns.nodeVal = fn; return api; },
      nodeColor: function (fn) {
        if (typeof fn === "function") fns.nodeColor = fn;
        else if (typeof fn === "undefined") { /* getter-style no-op call */ }
        scheduleNodeRefresh();
        return api;
      },
      nodeOpacity: function (v) {
        cfg.nodeOpacity = v;
        if (nodesMesh) nodesMesh.material.uniforms.uOpacity.value = v;
        return api;
      },
      nodeLabel: function (fn) { fns.nodeLabel = fn; return api; },
      linkVisibility: function (fn) { if (typeof fn === "function") fns.linkVisibility = fn; scheduleEdgeMode(); return api; },
      linkWidth: function (fn) { if (typeof fn === "function") fns.linkWidth = fn; return api; },
      linkOpacity: function (v) {
        cfg.linkOpacity = v;
        if (edgeMesh) edgeMesh.material.opacity = v;
        if (flowMesh) flowMesh.material.opacity = Math.max(v, 0.5);
        return api;
      },
      linkColor: function (fn) { if (typeof fn === "function") fns.linkColor = fn; scheduleEdgeRestyle(); return api; },
      linkDirectionalParticles: function (fn) { if (typeof fn === "function") fns.linkDirectionalParticles = fn; scheduleParticleRebuild(); return api; },
      linkDirectionalParticleWidth: function (v) { cfg.particleWidth = v; return api; },
      linkDirectionalParticleSpeed: function (v) { cfg.particleSpeed = v; return api; },
      linkDirectionalParticleColor: function (fn) { if (typeof fn === "function") fns.linkDirectionalParticleColor = fn; return api; },
      onNodeClick: function (fn) { api.onNodeClickFn = fn; return api; },
      onNodeHover: function (fn) { api.onNodeHoverFn = fn; return api; },
      onBackgroundClick: function (fn) { api.onBackgroundClickFn = fn; return api; },
      cooldownTicks: function () { return api; },
      warmupTicks: function () { return api; },
      d3AlphaDecay: function () { return api; },
      d3VelocityDecay: function () { return api; },

      _fpsEl: function () { return fpsEl; },
    };

    function scheduleNodeRefresh() {
      if (nodeColorsPending) return;
      nodeColorsPending = true;
      requestAnimationFrame(function () {
        nodeColorsPending = false;
        updateNodeColors();
      });
    }

    var edgeModeTimer = 0;
    function scheduleEdgeMode() {
      clearTimeout(edgeModeTimer);
      edgeModeTimer = setTimeout(updateEdgeMode, 0);
    }

    var particleTimer = 0;
    function scheduleParticleRebuild() {
      clearTimeout(particleTimer);
      particleTimer = setTimeout(rebuildParticles, 0);
    }

    // ── init ─────────────────────────────────────────────────

    return function (el) {
      container = el;
      renderer = new THREE.WebGLRenderer({
        antialias: window.devicePixelRatio < 1.5,
        powerPreference: "high-performance",
      });
      renderer.setPixelRatio(Math.min(window.devicePixelRatio || 1, 2));
      renderer.setClearColor(new THREE.Color(cfg.bgColor), 1);
      el.appendChild(renderer.domElement);

      tooltip = document.createElement("div");
      tooltip.className = "graph-tooltip";
      tooltip.style.display = "none";
      tooltip.style.position = "absolute";
      tooltip.style.pointerEvents = "none";
      tooltip.style.zIndex = "50";
      document.body.appendChild(tooltip);

      fpsEl = document.createElement("div");
      fpsEl.className = "fastgraph-fps";
      document.body.appendChild(fpsEl);

      controls._bind();
      bindPointer();
      if (typeof ResizeObserver !== "undefined") {
        new ResizeObserver(resize).observe(el);
      }
      window.addEventListener("resize", resize);
      resize();
      // Late-layout safety net: re-measure after the first paint settles.
      setTimeout(resize, 300);
      setTimeout(resize, 1200);
      rafId = requestAnimationFrame(loop);
      return api;
    };
  }

  root.FastGraph = FastGraph;
})(typeof window !== "undefined" ? window : this);
