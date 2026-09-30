/* global cytoscape, monaco, require */

const state = {
  graph: null,
  graphIndex: null,
  cy: null,
  selectedNodeId: null,
  renderMode: "progressive",
  searchQuery: "",
  kindFilter: "all",
  searchDebounceHandle: null,
  searchTotalMatches: 0,
  searchShownMatches: 0,
  searchTruncated: false,
  renderedNodeCount: 0,
  renderedEdgeCount: 0,
  renderedFunctionCount: 0,
  graphControlsBound: false,
  expandedModuleNodes: new Set(),
  expandedFileNodes: new Set(),
  expandedSymbolNodes: new Set(),
  expandedRelationNodes: new Set(),
  expansionLevelByNode: new Map(),
  globalRepelFrameHandle: null,
  globalRepelSettleFrames: 0,
  stagedRevealTimeoutHandle: null,
  stagedRevealSession: null,
  revealIntervalMs: 300,
  fitTightness: 50,
  nodeTooltipHideHandle: null,
  nodeTooltipShowHandle: null,
  makeTargets: [],
  makeRunnerAvailable: false,
  isRunningMake: false,
  settingsPanelOpen: false,
  detailsPanelOpen: true,
  detailsPanelWidth: 440,
  detailsActiveTab: "details",
  detailsResizeSession: null,
  theme: "light",
  fullLoadTimeoutHandle: null,
  fullLoadSession: null,
  fullLoadIncludeAll: false,
  mermaidRenderToken: 0,
  latestMermaidSource: "",
  mermaidAliasByGraphNodeId: new Map(),
  mermaidGraphNodeIdByAlias: new Map(),
  mermaidFocusedNodeId: null,
  mermaidFocusedEdge: null,
  mermaidFocusFlashTimeoutHandle: null,
  pendingMermaidFocusSync: null,
  graphFocusFlashTimeoutHandle: null,
  graphFocusFlashIntervalHandle: null,
  searchSplitAutoOpen: true,
  mermaidSplitOpen: false,
  mermaidModalOpen: false,
  mermaidZoom: 1,
  mermaidSourceMode: "auto",
  gitRefs: [],
  gitCurrentBranch: "",
  gitDefaultBaseRef: "",
  impactApiAvailable: false,
  impactLoading: false,
  impactActive: false,
  impactBaseRef: "",
  impactCompareMode: "workspace",
  impactHopCount: 2,
  impactChangedFiles: [],
  impactChangedFileNodeIds: new Set(),
  impactNodeIds: new Set(),
  impactVisibleNodeIds: new Set(),
  impactRevealSession: null,
  impactRevealTimeoutHandle: null,
  deferInsightsActive: false,
  insightsRefreshTimeoutHandle: null,
  insightsRefreshIdleHandle: null,
  insightsRefreshToken: 0,
  viewportCullFrameHandle: null,
  viewportCullTimerHandle: null,
  viewportCullLastAt: 0,
  viewportCullingActive: false,
  transitionLodActive: false,
  blastLensActive: false,
  blastCenterNodeId: null,
  blastHopCount: 2,
  blastNodeCap: 220,
  blastIgnoreStructureHops: true,
  blastShowCrate: true,
  blastShowModule: true,
  blastShowFile: true,
  blastLastSummary: null,
  liveImpactSeedNodeIds: new Set(),
  liveImpactUpstreamIds: new Set(),
  liveImpactDownstreamIds: new Set(),
  liveImpactChangedSymbolCount: 0,
  editorPath: "",
  editorMtimeMs: 0,
  editorBaseContent: "",
  editorDirty: false,
  editorLoading: false,
  editorSaving: false,
  editorChangeDebounceHandle: null,
  editorPollHandle: null,
  editorAutoRefresh: true,
  editorExternalChangeDetected: false,
  editorMonacoReady: false,
  editorMonacoInstance: null,
  editorMonacoModel: null,
  editorSuppressModelChange: false,
  flowSources: [],
  flowSourceId: "",
  flowMaxHops: 6,
  flowOnlyOutbound: false,
  flowOnlyCrossCrate: false,
  flowIncludeContainers: true,
  flowSourcesLoading: false,
  flowRunning: false,
  flowDeltaRunning: false,
  flowLastResult: null,
  flowLastView: null,
  flowLastDelta: null,
  flowGraphFocusActive: false,
  flowVisibleNodeIds: new Set(),
  flowTraceNodeIds: new Set(),
  flowVisibleTruncated: false,
  flowPlaybackSession: null,
  flowPlaybackTimeoutHandle: null,
  flowPlaybackPeakTimeoutHandle: null,
  guideTraceDirection: "downstream",
  guideSelectedTraceKey: "",
  guideSelectedSourceId: "",
  guideAutoRebuild: true,
  guideLastModel: null,
  guideArtifactPollHandle: null,
  guideArtifactSignature: "",
  guideArtifactsDirty: false,
  guideWatchBusy: false,
  guideOwnerRuleIndex: null,
  guideOwnerRuleLoadAttempted: false,
  activeInsightsMode: "none",
  optionsRowsCollapsed: false,
  infoRowsCollapsed: false,
};

const FALLBACK_MAKE_TARGETS = [
  "graph-index",
  "graph-check",
];

const ROOT_PROGRESSIVE_KINDS = new Set(["crate", "module"]);
const CONTAINER_KINDS = new Set(["crate", "module", "file"]);
const SYMBOL_KINDS = new Set([
  "function",
  "struct",
  "enum",
  "trait",
  "const",
  "static",
  "type_alias",
  "endpoint",
  "api_call",
]);
const VISIBLE_EDGE_KINDS = new Set([
  "contains",
  "defines",
  "depends_on",
  "references",
  "calls",
  "handles",
  "calls_api",
  "targets_endpoint",
  "accepts_payload",
]);
// Full progressive load mounts the STRUCTURAL map only: mounting the
// 1.2M call/test edges alongside 115k nodes exceeds what a Cytoscape
// canvas can hold interactively. Call-graph detail stays available
// through search / impact / blast lens, which are bounded slices.
const FULL_LOAD_EDGE_KINDS = new Set([
  "contains",
  "defines",
  "depends_on",
  "references",
  "implements",
]);
const FULL_GRAPH_ENABLE_NODE_LIMIT = 100;
const MODULE_EXPAND_LIMIT = 320;
const FILE_EXPAND_LIMIT = 260;
const SYMBOL_EXPAND_LIMIT = 680;
const RELATION_EXPAND_LIMIT = 260;
const SEARCH_MATCH_LIMIT = 700;
const SEARCH_VISIBLE_LIMIT = 1400;
// Debounce window between the last keystroke in the search box and
// the actual applySearch call. Higher value gives the user more time
// to finish typing a multi-word query without the graph rebuilding
// mid-word; lower value feels snappier for short queries. 600 ms is
// roughly one comfortable "pause-before-thinking" gap.
const SEARCH_DEBOUNCE_MS = 600;
const EXPANSION_HIGHLIGHT_NODE_LIMIT = 900;
const EXPANSION_REVEAL_INTERVAL_MS = 300;
const REVEAL_SPEED_STEP_MS = 20;
const FULL_LOAD_MIN_SPEED_MS = 20;
const REVEAL_SPEED_STORAGE_KEY = "codegraph.reveal_speed_ms";
const FIT_TIGHTNESS_STORAGE_KEY = "codegraph.fit_tightness";
const SEARCH_SPLIT_AUTO_OPEN_STORAGE_KEY = "codegraph.search_split_auto_open";
const THEME_STORAGE_KEY = "codegraph.theme";
const MERMAID_SOURCE_MODE_STORAGE_KEY = "codegraph.mermaid_source_mode";
const CONTROLS_OPTIONS_COLLAPSED_STORAGE_KEY = "codegraph.controls_options_collapsed";
const CONTROLS_INFO_COLLAPSED_STORAGE_KEY = "codegraph.controls_info_collapsed";
const DETAILS_PANEL_OPEN_STORAGE_KEY = "codegraph.details_panel_open";
const DETAILS_PANEL_WIDTH_STORAGE_KEY = "codegraph.details_panel_width";
const DETAILS_ACTIVE_TAB_STORAGE_KEY = "codegraph.details_active_tab";
const BLAST_IGNORE_STRUCTURE_HOPS_STORAGE_KEY = "codegraph.blast_ignore_structure_hops";
const BLAST_SHOW_CRATE_STORAGE_KEY = "codegraph.blast_show_crate";
const BLAST_SHOW_MODULE_STORAGE_KEY = "codegraph.blast_show_module";
const BLAST_SHOW_FILE_STORAGE_KEY = "codegraph.blast_show_file";
const GUIDE_TRACE_DIRECTION_STORAGE_KEY = "codegraph.guide_trace_direction";
const GUIDE_AUTO_REBUILD_STORAGE_KEY = "codegraph.guide_auto_rebuild";
const URL_QUERY_SEARCH_KEY = "q";
const URL_QUERY_KIND_KEY = "kind";
const URL_QUERY_SPLIT_KEY = "split";
const URL_QUERY_MODAL_KEY = "modal";
const URL_QUERY_SELECTED_NODE_KEY = "sel";
const URL_QUERY_EXPANDED_MODULES_KEY = "xm";
const URL_QUERY_EXPANDED_FILES_KEY = "xf";
const URL_QUERY_EXPANDED_SYMBOLS_KEY = "xs";
const URL_QUERY_EXPANDED_RELATIONS_KEY = "xr";
const URL_QUERY_IMPACT_ACTIVE_KEY = "impact";
const URL_QUERY_IMPACT_BASE_KEY = "ibase";
const URL_QUERY_IMPACT_COMPARE_KEY = "icmp";
const URL_QUERY_IMPACT_HOPS_KEY = "ihops";
const URL_QUERY_FLOW_SOURCE_KEY = "fsrc";
const URL_QUERY_FLOW_HOPS_KEY = "fhops";
const URL_QUERY_FLOW_OUTBOUND_KEY = "fout";
const URL_QUERY_FLOW_CROSS_CRATE_KEY = "fxc";
const URL_QUERY_FLOW_CONTAINERS_KEY = "fcont";
const URL_EXPANSION_LIST_LIMIT = 120;
const IMPACT_HOP_MIN = 1;
const IMPACT_HOP_MAX = 3;
const IMPACT_NODE_CAP = 3200;
const SEARCH_INSIGHTS_NODE_LIMIT = 48;
const SEARCH_INSIGHTS_EDGE_LIMIT = 120;
const MERMAID_ZOOM_MIN = 0.4;
const MERMAID_ZOOM_MAX = 2.8;
const MERMAID_ZOOM_STEP = 0.2;
const MERMAID_WHEEL_ZOOM_IN_FACTOR = 1.08;
const MERMAID_WHEEL_ZOOM_OUT_FACTOR = 0.92;
const MERMAID_FOCUS_MIN_ZOOM = 0.9;
const MERMAID_FOCUS_CENTER_BLEND = 0.86;
const AUTO_OPEN_MERMAID_SPLIT_MIN_WIDTH = 1280;
const MERMAID_INTERACTIVE_CONTAINER_IDS = [
  "search-mermaid-diagram-large",
  "search-mermaid-diagram-modal",
  // /flows overlay body — same wheel-zoom + drag-pan UX as the split /
  // modal containers so deep flows (20+ layers) remain readable.
  "flow-canvas-body",
];
const MERMAID_RENDER_CONTAINER_IDS = [
  "search-mermaid-diagram",
  "search-mermaid-diagram-large",
  "search-mermaid-diagram-modal",
  "flow-canvas-body",
];
const GLOBAL_REPEL_RADIUS = 72;
const GLOBAL_REPEL_STRENGTH = 0.26;
const GLOBAL_REPEL_SETTLE_FRAMES_BASE = 90;
const GLOBAL_REPEL_SETTLE_FRAMES_MAX = 220;
const GLOBAL_REPEL_SETTLE_FRAMES_PER_200_NODES = 8;
const GLOBAL_REPEL_MAX_NODES = 1700;
const GLOBAL_REPEL_CONTINUE_DELTA = 0.4;
const GLOBAL_REPEL_CONTINUE_MIN_FRAMES = 22;
const NODE_SIZE_DEGREE_EXPONENT = 1.3;
const GRAPH_MERMAID_FOCUS_MIN_ZOOM = 0.55;
const GRAPH_MERMAID_FOCUS_MAX_ZOOM = 1.2;
const FOCUS_FLASH_DURATION_MS = 2800;
const FOCUS_FLASH_PULSE_MS = 220;
const SVG_NS = "http://www.w3.org/2000/svg";
const MERMAID_FOCUS_RING_EXTRA_RADIUS = 18;
const INSIGHTS_DEBOUNCE_MS = 140;
const INSIGHTS_IDLE_TIMEOUT_MS = 220;
const INSIGHTS_IMMEDIATE_NODE_LIMIT = 360;
const VIEWPORT_CULL_NODE_THRESHOLD = 380;
const VIEWPORT_CULL_EDGE_THRESHOLD = 820;
const VIEWPORT_CULL_MARGIN_PX = 180;
const VIEWPORT_CULL_THROTTLE_MS = 90;
// Below this zoom, edges are subpixel noise on large graphs — hide them
// wholesale instead of paying per-edge visibility math during pans.
const EDGE_CULL_MIN_ZOOM = 0.09;
// Edge span LOD: an edge draws only while its on-screen span is
// readable and useful — shorter than EDGE_SPAN_MIN_PX is arrowhead noise,
// longer than ~45% of the viewport is hierarchy spaghetti. Gating on
// world length (span = length * zoom) adapts per edge as you zoom.
const EDGE_SPAN_MIN_PX = 6;
const EDGE_SPAN_VIEWPORT_FRACTION = 0.45;
// Below this zoom on large mounts, node labels are unreadable specks —
// rendering 100k text glyphs dominates every overview frame.
const LABEL_MIN_ZOOM = 0.12;
const TRANSITION_LOD_NODE_THRESHOLD = 420;
const REVEAL_FRAME_BUDGET_MS = 9;
const BLAST_HOP_MIN = 1;
const BLAST_HOP_MAX = 8;
const BLAST_NODE_CAP_MIN = 80;
const BLAST_NODE_CAP_MAX = 1000;
const BLAST_LAYOUT_X_STEP = 300;
const BLAST_LAYOUT_Y_STEP = 72;
const LIVE_IMPACT_HOP_COUNT = 2;
const LIVE_IMPACT_NODE_CAP = 420;
const EDITOR_CHANGE_DEBOUNCE_MS = 650;
const EDITOR_POLL_INTERVAL_MS = 2200;
const EDITOR_IMPACT_TOKEN_MAX = 80;
const DETAILS_PANEL_MIN_WIDTH = 320;
const DETAILS_PANEL_MAX_WIDTH = 1040;
const FLOW_HOPS_MIN = 2;
const FLOW_HOPS_MAX = 12;
const FLOW_VISIBLE_NODE_CAP = 2600;
const FLOW_PLAYBACK_TRACE_LIMIT = 12;
const FLOW_PLAYBACK_STEP_MIN_MS = 60;
const FLOW_PLAYBACK_STEP_MAX_MS = 420;
const FLOW_PLAYBACK_PEAK_RATIO = 0.42;
const FLOW_INSIGHTS_NODE_LIMIT = 120;
const FLOW_INSIGHTS_EDGE_LIMIT = 220;
const FLOW_INSIGHTS_TRACE_LIMIT = 24;
const GUIDE_ARTIFACT_WATCH_INTERVAL_MS = 3200;
const GUIDE_ARTIFACT_WATCH_PATHS = [
  "docs/codegraph/graph.json",
  "docs/codegraph/payload_profiles.json",
  "docs/codegraph/stats.json",
  "docs/codegraph/c4.json",
];
const GUIDE_STEP_NOTE_TRUNCATE = 120;

const KIND_STYLE_MAP_LIGHT = {
  crate: { fill: "#0b7a5a", border: "#065f46", text: "#f4fffa" },
  module: { fill: "#4f8d78", border: "#2f6c57", text: "#102219" },
  file: { fill: "#6f9ec0", border: "#4f7592", text: "#102219" },
  function: { fill: "#8b9ef0", border: "#5f72d5", text: "#11204d" },
  struct: { fill: "#7fbf80", border: "#4f9451", text: "#12381a" },
  enum: { fill: "#f3b36a", border: "#cf8429", text: "#4f2c06" },
  trait: { fill: "#be9ee8", border: "#8b63c2", text: "#35165e" },
  const: { fill: "#b9a7db", border: "#8d78b5", text: "#2d1a4d" },
  static: { fill: "#d7a8bf", border: "#ae7893", text: "#4d1632" },
  type_alias: { fill: "#82c7d7", border: "#4f97a9", text: "#103746" },
  endpoint: { fill: "#f6d365", border: "#d49b11", text: "#4d3300" },
  api_call: { fill: "#f59f9f", border: "#d46565", text: "#4b1111" },
};

const KIND_STYLE_MAP_DARK = {
  crate: { fill: "#0f8f6a", border: "#35b58d", text: "#edfff8" },
  module: { fill: "#2f725c", border: "#5fa087", text: "#e6f8f0" },
  file: { fill: "#2d5e7f", border: "#5c93b9", text: "#e8f4ff" },
  function: { fill: "#405cb3", border: "#7f98f5", text: "#e9efff" },
  struct: { fill: "#2f7b47", border: "#63b07a", text: "#e9ffef" },
  enum: { fill: "#915a1a", border: "#d38c3a", text: "#fff1df" },
  trait: { fill: "#65439c", border: "#a786e1", text: "#f4ebff" },
  const: { fill: "#6a5a93", border: "#a898d2", text: "#f4edff" },
  static: { fill: "#7a4962", border: "#c08dab", text: "#ffeaf5" },
  type_alias: { fill: "#2d6f78", border: "#6fbecb", text: "#e7fcff" },
  endpoint: { fill: "#8f6b11", border: "#e0b445", text: "#fff4d5" },
  api_call: { fill: "#8f3535", border: "#e58888", text: "#ffe9e9" },
};

const GRAPH_THEME_LIGHT = {
  defaultNodeText: "#102219",
  edgeLine: "#b7c5bd",
  edgeArrow: "#b7c5bd",
  matchedBorder: "#ef9b20",
  expansionOriginBorder: "#8a4f00",
  expansionOriginFill: "#ffd60a",
  expansionOriginText: "#3a2200",
  expansionOriginShadow: "#8a4f00",
  expansionNeighborFill: "#ffb366",
  expansionNeighborText: "#472100",
  expansionEdgeOutgoing: "#e85d04",
  expansionEdgeIncoming: "#ff9f1c",
  mermaidFocusNodeBorder: "#ff006e",
  mermaidFocusNodeShadow: "#ff4fa1",
  mermaidFocusEdge: "#0077ff",
  flowTraceSourceBorder: "#f4b400",
  flowTraceSourceGlow: "#ffcc4d",
  flowTraceNodeBorder: "#f97316",
  flowTraceSinkBorder: "#ef4444",
  flowTraceEdge: "#f59e0b",
  flowPlaybackNodeBorder: "#34d399",
  flowPlaybackNodeGlow: "#10b981",
  flowPlaybackEdge: "#22c55e",
};

const GRAPH_THEME_DARK = {
  defaultNodeText: "#e7f3ec",
  edgeLine: "#4f6a5d",
  edgeArrow: "#4f6a5d",
  matchedBorder: "#ffbf47",
  expansionOriginBorder: "#ffd166",
  expansionOriginFill: "#8d6b00",
  expansionOriginText: "#fff7d6",
  expansionOriginShadow: "#ffcf5c",
  expansionNeighborFill: "#7a3e00",
  expansionNeighborText: "#ffe9d1",
  expansionEdgeOutgoing: "#ff9f43",
  expansionEdgeIncoming: "#ffd166",
  mermaidFocusNodeBorder: "#ffd400",
  mermaidFocusNodeShadow: "#ffe986",
  mermaidFocusEdge: "#5cc8ff",
  flowTraceSourceBorder: "#ffd54d",
  flowTraceSourceGlow: "#ffe082",
  flowTraceNodeBorder: "#ffb74d",
  flowTraceSinkBorder: "#ff8a80",
  flowTraceEdge: "#ffb74d",
  flowPlaybackNodeBorder: "#6ee7b7",
  flowPlaybackNodeGlow: "#34d399",
  flowPlaybackEdge: "#6ee7b7",
};

function el(id) {
  return document.getElementById(id);
}

function normalize(str) {
  return String(str || "").toLowerCase();
}

function escapeHtml(value) {
  return String(value ?? "")
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#39;");
}

function readLocalStorage(key) {
  try {
    return window.localStorage.getItem(key);
  } catch (_err) {
    return null;
  }
}

function writeLocalStorage(key, value) {
  try {
    window.localStorage.setItem(key, value);
  } catch (_err) {
    // Ignore storage write failures (private mode, policy restrictions).
  }
}

function normalizeKindFilter(value) {
  const normalized = normalize(value);
  return normalized.length > 0 ? normalized : "all";
}

function parseUrlBoolean(value) {
  if (value === null || value === undefined) return null;
  const normalized = normalize(value);
  if (normalized === "1" || normalized === "true" || normalized === "yes") return true;
  if (normalized === "0" || normalized === "false" || normalized === "no") return false;
  return null;
}

function parseNodeIdList(value) {
  if (!value) return [];
  return String(value)
    .split(",")
    .map((entry) => entry.trim())
    .filter((entry) => entry.length > 0);
}

function normalizeImpactCompareMode(value) {
  const normalized = normalize(value);
  return normalized === "head" ? "head" : "workspace";
}

function clampImpactHops(value) {
  if (value === null || value === undefined) return 2;
  if (typeof value === "string" && value.trim().length === 0) return 2;
  const parsed = Number(value);
  if (!Number.isFinite(parsed)) return 2;
  return Math.max(IMPACT_HOP_MIN, Math.min(IMPACT_HOP_MAX, Math.round(parsed)));
}

function clampFlowHops(value) {
  if (value === null || value === undefined) return 6;
  if (typeof value === "string" && value.trim().length === 0) return 6;
  const parsed = Number(value);
  if (!Number.isFinite(parsed)) return 6;
  const stepped = Math.round(parsed / 2) * 2;
  return Math.max(FLOW_HOPS_MIN, Math.min(FLOW_HOPS_MAX, stepped));
}

function normalizeMermaidSourceMode(value) {
  const normalized = String(value || "").trim().toLowerCase();
  if (normalized === "flow") return "flow";
  if (normalized === "search") return "search";
  return "auto";
}

function clampBlastHops(value) {
  const parsed = Number(value);
  if (!Number.isFinite(parsed)) return 2;
  return Math.max(BLAST_HOP_MIN, Math.min(BLAST_HOP_MAX, Math.round(parsed)));
}

function clampBlastNodeCap(value) {
  const parsed = Number(value);
  if (!Number.isFinite(parsed)) return 220;
  return Math.max(BLAST_NODE_CAP_MIN, Math.min(BLAST_NODE_CAP_MAX, Math.round(parsed)));
}

function parseStoredBoolean(rawValue, fallback = false) {
  if (rawValue === null || rawValue === undefined) return fallback;
  const normalized = normalize(rawValue);
  if (normalized === "1" || normalized === "true" || normalized === "yes") return true;
  if (normalized === "0" || normalized === "false" || normalized === "no") return false;
  return fallback;
}

function normalizeDetailsTab(rawValue) {
  const normalized = normalize(rawValue);
  if (normalized === "guide") return "guide";
  if (normalized === "mermaid") return "mermaid";
  if (normalized === "editor") return "editor";
  // The Flow tab UI was retired in favor of the `/flows` slash
  // command. Any stored preference / programmatic request for it
  // gracefully falls through to the Details tab so the panel
  // doesn't end up pointing at a non-existent target.
  return "details";
}

function normalizeGuideTraceDirection(value) {
  const normalized = String(value || "").trim().toLowerCase();
  if (normalized === "upstream") return "upstream";
  if (normalized === "both") return "both";
  return "downstream";
}

function detailsPanelMaxWidth() {
  const viewportCap = typeof window !== "undefined"
    ? Math.floor(window.innerWidth * 0.88)
    : DETAILS_PANEL_MAX_WIDTH;
  return Math.max(
    DETAILS_PANEL_MIN_WIDTH,
    Math.min(DETAILS_PANEL_MAX_WIDTH, viewportCap)
  );
}

function clampDetailsPanelWidth(value) {
  const parsed = Number(value);
  if (!Number.isFinite(parsed)) {
    return Math.min(440, detailsPanelMaxWidth());
  }
  const maxWidth = detailsPanelMaxWidth();
  return Math.max(DETAILS_PANEL_MIN_WIDTH, Math.min(maxWidth, Math.round(parsed)));
}

function serializeNodeIdCollection(collection) {
  if (!collection || typeof collection.size !== "number" || collection.size <= 0) return "";
  if (collection.size > URL_EXPANSION_LIST_LIMIT) return "";
  return Array.from(collection).sort().join(",");
}

function sanitizeNodeIdList(rawIds, allowedKinds = null) {
  const index = state.graphIndex;
  if (!index || !Array.isArray(rawIds) || rawIds.length === 0) return [];

  const deduped = [];
  const seen = new Set();
  for (const nodeId of rawIds) {
    const id = String(nodeId || "").trim();
    if (!id || seen.has(id)) continue;
    const node = index.nodeById.get(id);
    if (!node) continue;
    if (allowedKinds && !allowedKinds.has(node.kind)) continue;
    seen.add(id);
    deduped.push(id);
  }
  return deduped;
}

function restoreExpansionLevelsFromSets() {
  const index = state.graphIndex;
  state.expansionLevelByNode.clear();
  if (!index) return;

  const sourceIds = new Set([
    ...state.expandedModuleNodes,
    ...state.expandedFileNodes,
    ...state.expandedSymbolNodes,
  ]);

  for (const nodeId of sourceIds) {
    const node = index.nodeById.get(nodeId);
    if (!node) continue;
    let level = 0;
    if (state.expandedSymbolNodes.has(nodeId)) {
      level = node.kind === "module" ? 2 : 1;
    } else if (state.expandedFileNodes.has(nodeId) || state.expandedModuleNodes.has(nodeId)) {
      level = 1;
    }
    if (level > 0) {
      state.expansionLevelByNode.set(nodeId, level);
    }
  }
}

function restoreClickExpansionStateFromUrl(urlViewState) {
  if (!urlViewState || !state.graphIndex) return;

  const moduleKinds = new Set(["crate", "module"]);
  const fileKinds = new Set(["crate", "module"]);
  const symbolKinds = new Set(["crate", "module", "file"]);

  const modules = sanitizeNodeIdList(urlViewState.expandedModuleIds, moduleKinds);
  const files = sanitizeNodeIdList(urlViewState.expandedFileIds, fileKinds);
  const symbols = sanitizeNodeIdList(urlViewState.expandedSymbolIds, symbolKinds);
  const relations = sanitizeNodeIdList(urlViewState.expandedRelationIds, null);
  const selected = sanitizeNodeIdList(
    urlViewState.selectedNodeId ? [urlViewState.selectedNodeId] : [],
    null
  )[0] || null;

  state.expandedModuleNodes.clear();
  modules.forEach((id) => state.expandedModuleNodes.add(id));
  state.expandedFileNodes.clear();
  files.forEach((id) => state.expandedFileNodes.add(id));
  state.expandedSymbolNodes.clear();
  symbols.forEach((id) => state.expandedSymbolNodes.add(id));
  state.expandedRelationNodes.clear();
  relations.forEach((id) => state.expandedRelationNodes.add(id));
  state.selectedNodeId = selected;

  restoreExpansionLevelsFromSets();
}

function canAutoOpenMermaidSplit() {
  return typeof window !== "undefined" && window.innerWidth >= AUTO_OPEN_MERMAID_SPLIT_MIN_WIDTH;
}

function readUrlViewState() {
  try {
    const currentUrl = new URL(window.location.href);
    return {
      searchQuery: currentUrl.searchParams.get(URL_QUERY_SEARCH_KEY) || "",
      kindFilter: normalizeKindFilter(currentUrl.searchParams.get(URL_QUERY_KIND_KEY) || "all"),
      mermaidSplitOpen: parseUrlBoolean(currentUrl.searchParams.get(URL_QUERY_SPLIT_KEY)),
      mermaidModalOpen: parseUrlBoolean(currentUrl.searchParams.get(URL_QUERY_MODAL_KEY)),
      selectedNodeId: currentUrl.searchParams.get(URL_QUERY_SELECTED_NODE_KEY) || "",
      expandedModuleIds: parseNodeIdList(currentUrl.searchParams.get(URL_QUERY_EXPANDED_MODULES_KEY)),
      expandedFileIds: parseNodeIdList(currentUrl.searchParams.get(URL_QUERY_EXPANDED_FILES_KEY)),
      expandedSymbolIds: parseNodeIdList(currentUrl.searchParams.get(URL_QUERY_EXPANDED_SYMBOLS_KEY)),
      expandedRelationIds: parseNodeIdList(currentUrl.searchParams.get(URL_QUERY_EXPANDED_RELATIONS_KEY)),
      impactActive: parseUrlBoolean(currentUrl.searchParams.get(URL_QUERY_IMPACT_ACTIVE_KEY)),
      impactBaseRef: currentUrl.searchParams.get(URL_QUERY_IMPACT_BASE_KEY) || "",
      impactCompareMode: normalizeImpactCompareMode(
        currentUrl.searchParams.get(URL_QUERY_IMPACT_COMPARE_KEY) || "workspace"
      ),
      impactHopCount: clampImpactHops(currentUrl.searchParams.get(URL_QUERY_IMPACT_HOPS_KEY)),
      flowSourceId: currentUrl.searchParams.get(URL_QUERY_FLOW_SOURCE_KEY) || "",
      flowMaxHops: clampFlowHops(currentUrl.searchParams.get(URL_QUERY_FLOW_HOPS_KEY)),
      flowOnlyOutbound: parseUrlBoolean(currentUrl.searchParams.get(URL_QUERY_FLOW_OUTBOUND_KEY)),
      flowOnlyCrossCrate: parseUrlBoolean(currentUrl.searchParams.get(URL_QUERY_FLOW_CROSS_CRATE_KEY)),
      flowIncludeContainers: parseUrlBoolean(currentUrl.searchParams.get(URL_QUERY_FLOW_CONTAINERS_KEY)),
    };
  } catch (_err) {
    return {
      searchQuery: "",
      kindFilter: "all",
      mermaidSplitOpen: null,
      mermaidModalOpen: null,
      selectedNodeId: "",
      expandedModuleIds: [],
      expandedFileIds: [],
      expandedSymbolIds: [],
      expandedRelationIds: [],
      impactActive: null,
      impactBaseRef: "",
      impactCompareMode: "workspace",
      impactHopCount: 2,
      flowSourceId: "",
      flowMaxHops: 6,
      flowOnlyOutbound: null,
      flowOnlyCrossCrate: null,
      flowIncludeContainers: null,
    };
  }
}

function writeUrlViewState() {
  if (!window.history || typeof window.history.replaceState !== "function") return;

  const currentUrl = new URL(window.location.href);
  const nextQuery = state.searchQuery.trim();
  const nextKind = normalizeKindFilter(state.kindFilter);
  const selectedNodeId = String(state.selectedNodeId || "").trim();
  const expandedModules = serializeNodeIdCollection(state.expandedModuleNodes);
  const expandedFiles = serializeNodeIdCollection(state.expandedFileNodes);
  const expandedSymbols = serializeNodeIdCollection(state.expandedSymbolNodes);
  const expandedRelations = serializeNodeIdCollection(state.expandedRelationNodes);
  const impactBaseRef = String(state.impactBaseRef || "").trim();
  const impactCompareMode = normalizeImpactCompareMode(state.impactCompareMode);
  const impactHopCount = clampImpactHops(state.impactHopCount);
  const hasExpansionState =
    selectedNodeId.length > 0 ||
    expandedModules.length > 0 ||
    expandedFiles.length > 0 ||
    expandedSymbols.length > 0 ||
    expandedRelations.length > 0;
  const hasInsightState = nextQuery.length > 0 || state.impactActive;

  if (nextQuery.length > 0) {
    currentUrl.searchParams.set(URL_QUERY_SEARCH_KEY, nextQuery);
  } else {
    currentUrl.searchParams.delete(URL_QUERY_SEARCH_KEY);
  }

  if (nextKind !== "all") {
    currentUrl.searchParams.set(URL_QUERY_KIND_KEY, nextKind);
  } else {
    currentUrl.searchParams.delete(URL_QUERY_KIND_KEY);
  }

  if (hasInsightState) {
    currentUrl.searchParams.set(URL_QUERY_SPLIT_KEY, state.mermaidSplitOpen ? "1" : "0");
    currentUrl.searchParams.set(URL_QUERY_MODAL_KEY, state.mermaidModalOpen ? "1" : "0");
  } else {
    currentUrl.searchParams.delete(URL_QUERY_SPLIT_KEY);
    currentUrl.searchParams.delete(URL_QUERY_MODAL_KEY);
  }

  if (hasExpansionState) {
    if (selectedNodeId.length > 0) {
      currentUrl.searchParams.set(URL_QUERY_SELECTED_NODE_KEY, selectedNodeId);
    } else {
      currentUrl.searchParams.delete(URL_QUERY_SELECTED_NODE_KEY);
    }
    if (expandedModules.length > 0) {
      currentUrl.searchParams.set(URL_QUERY_EXPANDED_MODULES_KEY, expandedModules);
    } else {
      currentUrl.searchParams.delete(URL_QUERY_EXPANDED_MODULES_KEY);
    }
    if (expandedFiles.length > 0) {
      currentUrl.searchParams.set(URL_QUERY_EXPANDED_FILES_KEY, expandedFiles);
    } else {
      currentUrl.searchParams.delete(URL_QUERY_EXPANDED_FILES_KEY);
    }
    if (expandedSymbols.length > 0) {
      currentUrl.searchParams.set(URL_QUERY_EXPANDED_SYMBOLS_KEY, expandedSymbols);
    } else {
      currentUrl.searchParams.delete(URL_QUERY_EXPANDED_SYMBOLS_KEY);
    }
    if (expandedRelations.length > 0) {
      currentUrl.searchParams.set(URL_QUERY_EXPANDED_RELATIONS_KEY, expandedRelations);
    } else {
      currentUrl.searchParams.delete(URL_QUERY_EXPANDED_RELATIONS_KEY);
    }
  } else {
    currentUrl.searchParams.delete(URL_QUERY_SELECTED_NODE_KEY);
    currentUrl.searchParams.delete(URL_QUERY_EXPANDED_MODULES_KEY);
    currentUrl.searchParams.delete(URL_QUERY_EXPANDED_FILES_KEY);
    currentUrl.searchParams.delete(URL_QUERY_EXPANDED_SYMBOLS_KEY);
    currentUrl.searchParams.delete(URL_QUERY_EXPANDED_RELATIONS_KEY);
  }

  const keepImpactParams = state.impactActive === true;
  if (keepImpactParams) {
    if (impactBaseRef.length > 0) {
      currentUrl.searchParams.set(URL_QUERY_IMPACT_BASE_KEY, impactBaseRef);
    } else {
      currentUrl.searchParams.delete(URL_QUERY_IMPACT_BASE_KEY);
    }
    if (impactCompareMode !== "workspace") {
      currentUrl.searchParams.set(URL_QUERY_IMPACT_COMPARE_KEY, impactCompareMode);
    } else {
      currentUrl.searchParams.delete(URL_QUERY_IMPACT_COMPARE_KEY);
    }
    if (impactHopCount !== 2) {
      currentUrl.searchParams.set(URL_QUERY_IMPACT_HOPS_KEY, String(impactHopCount));
    } else {
      currentUrl.searchParams.delete(URL_QUERY_IMPACT_HOPS_KEY);
    }
    if (state.impactActive) {
      currentUrl.searchParams.set(URL_QUERY_IMPACT_ACTIVE_KEY, "1");
    } else {
      currentUrl.searchParams.delete(URL_QUERY_IMPACT_ACTIVE_KEY);
    }
  } else {
    currentUrl.searchParams.delete(URL_QUERY_IMPACT_BASE_KEY);
    currentUrl.searchParams.delete(URL_QUERY_IMPACT_COMPARE_KEY);
    currentUrl.searchParams.delete(URL_QUERY_IMPACT_HOPS_KEY);
    currentUrl.searchParams.delete(URL_QUERY_IMPACT_ACTIVE_KEY);
  }

  const flowSourceId = String(state.flowSourceId || "").trim();
  const defaultFlowSourceId = state.flowSources.length > 0
    ? String(state.flowSources[0]?.id || "").trim()
    : "";
  const flowSourceIsDefault = defaultFlowSourceId.length > 0 && flowSourceId === defaultFlowSourceId;
  const flowHops = clampFlowHops(state.flowMaxHops);
  const flowOnlyOutbound = state.flowOnlyOutbound === true;
  const flowOnlyCrossCrate = state.flowOnlyCrossCrate === true;
  const flowIncludeContainers = state.flowIncludeContainers !== false;
  const flowHasLiveState =
    state.flowLastResult !== null ||
    state.flowLastDelta !== null ||
    state.flowGraphFocusActive === true;
  const keepFlowParams =
    (flowSourceId.length > 0 && (!flowSourceIsDefault || flowHasLiveState)) ||
    flowHops !== 6 ||
    flowOnlyOutbound ||
    flowOnlyCrossCrate ||
    !flowIncludeContainers;

  if (keepFlowParams) {
    if (flowSourceId.length > 0) {
      currentUrl.searchParams.set(URL_QUERY_FLOW_SOURCE_KEY, flowSourceId);
    } else {
      currentUrl.searchParams.delete(URL_QUERY_FLOW_SOURCE_KEY);
    }
    if (flowHops !== 6) {
      currentUrl.searchParams.set(URL_QUERY_FLOW_HOPS_KEY, String(flowHops));
    } else {
      currentUrl.searchParams.delete(URL_QUERY_FLOW_HOPS_KEY);
    }
    if (flowOnlyOutbound) {
      currentUrl.searchParams.set(URL_QUERY_FLOW_OUTBOUND_KEY, "1");
    } else {
      currentUrl.searchParams.delete(URL_QUERY_FLOW_OUTBOUND_KEY);
    }
    if (flowOnlyCrossCrate) {
      currentUrl.searchParams.set(URL_QUERY_FLOW_CROSS_CRATE_KEY, "1");
    } else {
      currentUrl.searchParams.delete(URL_QUERY_FLOW_CROSS_CRATE_KEY);
    }
    if (!flowIncludeContainers) {
      currentUrl.searchParams.set(URL_QUERY_FLOW_CONTAINERS_KEY, "0");
    } else {
      currentUrl.searchParams.delete(URL_QUERY_FLOW_CONTAINERS_KEY);
    }
  } else {
    currentUrl.searchParams.delete(URL_QUERY_FLOW_SOURCE_KEY);
    currentUrl.searchParams.delete(URL_QUERY_FLOW_HOPS_KEY);
    currentUrl.searchParams.delete(URL_QUERY_FLOW_OUTBOUND_KEY);
    currentUrl.searchParams.delete(URL_QUERY_FLOW_CROSS_CRATE_KEY);
    currentUrl.searchParams.delete(URL_QUERY_FLOW_CONTAINERS_KEY);
  }

  const nextHref = `${currentUrl.pathname}${currentUrl.search}${currentUrl.hash}`;
  const activeHref = `${window.location.pathname}${window.location.search}${window.location.hash}`;
  if (nextHref !== activeHref) {
    window.history.replaceState(null, "", nextHref);
  }
}

function clampRevealSpeedMs(value) {
  const parsed = Number(value);
  if (!Number.isFinite(parsed)) {
    return EXPANSION_REVEAL_INTERVAL_MS;
  }
  return Math.max(0, Math.min(1000, Math.round(parsed / REVEAL_SPEED_STEP_MS) * REVEAL_SPEED_STEP_MS));
}

function formatRevealSpeed(ms) {
  return ms <= 0 ? "Instant" : `${ms}ms`;
}

function clampFitTightness(value) {
  const parsed = Number(value);
  if (!Number.isFinite(parsed)) return 50;
  return Math.max(0, Math.min(100, Math.round(parsed)));
}

function formatFitTightness(value) {
  return `${clampFitTightness(value)}%`;
}

function initializeRevealSpeedSetting() {
  const raw = readLocalStorage(REVEAL_SPEED_STORAGE_KEY);
  if (raw === null) {
    state.revealIntervalMs = EXPANSION_REVEAL_INTERVAL_MS;
    return;
  }
  state.revealIntervalMs = clampRevealSpeedMs(raw);
}

function initializeFitTightnessSetting() {
  const raw = readLocalStorage(FIT_TIGHTNESS_STORAGE_KEY);
  if (raw === null) {
    state.fitTightness = 50;
    return;
  }
  state.fitTightness = clampFitTightness(raw);
}

function initializeSearchSplitAutoOpenSetting() {
  const raw = readLocalStorage(SEARCH_SPLIT_AUTO_OPEN_STORAGE_KEY);
  if (raw === null) {
    state.searchSplitAutoOpen = true;
    return;
  }
  const normalized = normalize(raw);
  state.searchSplitAutoOpen = !(normalized === "0" || normalized === "false" || normalized === "no");
}

function initializeMermaidSourceModeSetting() {
  const raw = readLocalStorage(MERMAID_SOURCE_MODE_STORAGE_KEY);
  state.mermaidSourceMode = normalizeMermaidSourceMode(raw || "auto");
}

function initializeControlsCollapseSettings() {
  const optionsRaw = readLocalStorage(CONTROLS_OPTIONS_COLLAPSED_STORAGE_KEY);
  const infoRaw = readLocalStorage(CONTROLS_INFO_COLLAPSED_STORAGE_KEY);
  state.optionsRowsCollapsed = parseStoredBoolean(optionsRaw, false);
  state.infoRowsCollapsed = parseStoredBoolean(infoRaw, false);
}

function initializeDetailsPanelSettings() {
  state.detailsPanelOpen = parseStoredBoolean(
    readLocalStorage(DETAILS_PANEL_OPEN_STORAGE_KEY),
    true
  );
  state.detailsPanelWidth = clampDetailsPanelWidth(
    readLocalStorage(DETAILS_PANEL_WIDTH_STORAGE_KEY)
  );
  state.detailsActiveTab = normalizeDetailsTab(
    readLocalStorage(DETAILS_ACTIVE_TAB_STORAGE_KEY)
  );
}

function initializeBlastSettings() {
  state.blastIgnoreStructureHops = parseStoredBoolean(
    readLocalStorage(BLAST_IGNORE_STRUCTURE_HOPS_STORAGE_KEY),
    true
  );
  state.blastShowCrate = parseStoredBoolean(readLocalStorage(BLAST_SHOW_CRATE_STORAGE_KEY), true);
  state.blastShowModule = parseStoredBoolean(readLocalStorage(BLAST_SHOW_MODULE_STORAGE_KEY), true);
  state.blastShowFile = parseStoredBoolean(readLocalStorage(BLAST_SHOW_FILE_STORAGE_KEY), true);
}

function initializeGuideSettings() {
  state.guideTraceDirection = normalizeGuideTraceDirection(
    readLocalStorage(GUIDE_TRACE_DIRECTION_STORAGE_KEY) || "downstream"
  );
  state.guideAutoRebuild = parseStoredBoolean(
    readLocalStorage(GUIDE_AUTO_REBUILD_STORAGE_KEY),
    true
  );
}

function applyControlsRowCollapseState() {
  const optionsRows = [
    el("controls-options-impact"),
    el("controls-options-blast"),
  ];
  const infoRows = [
    el("controls-info-impact"),
    el("controls-info-blast"),
    el("controls-info-top"),
    el("controls-info-status"),
  ];
  optionsRows.forEach((row) => {
    if (!row) return;
    row.hidden = state.optionsRowsCollapsed;
  });
  infoRows.forEach((row) => {
    if (!row) return;
    row.hidden = state.infoRowsCollapsed;
  });
}

function syncControlsCollapseToggles() {
  const optionsButton = el("toggle-options-rows");
  const infoButton = el("toggle-info-rows");
  if (optionsButton) {
    optionsButton.textContent = state.optionsRowsCollapsed ? "Show Options" : "Hide Options";
    optionsButton.setAttribute("aria-expanded", state.optionsRowsCollapsed ? "false" : "true");
  }
  if (infoButton) {
    infoButton.textContent = state.infoRowsCollapsed ? "Show Tips" : "Hide Tips";
    infoButton.setAttribute("aria-expanded", state.infoRowsCollapsed ? "false" : "true");
  }
}

function setOptionsRowsCollapsed(collapsed, options = {}) {
  const next = collapsed === true;
  if (state.optionsRowsCollapsed === next) return;
  state.optionsRowsCollapsed = next;
  applyControlsRowCollapseState();
  syncControlsCollapseToggles();
  if (options.persist !== false) {
    writeLocalStorage(CONTROLS_OPTIONS_COLLAPSED_STORAGE_KEY, next ? "1" : "0");
  }
}

function setInfoRowsCollapsed(collapsed, options = {}) {
  const next = collapsed === true;
  if (state.infoRowsCollapsed === next) return;
  state.infoRowsCollapsed = next;
  applyControlsRowCollapseState();
  syncControlsCollapseToggles();
  if (options.persist !== false) {
    writeLocalStorage(CONTROLS_INFO_COLLAPSED_STORAGE_KEY, next ? "1" : "0");
  }
}

function scheduleGraphResizeAfterPanelChange(options = {}) {
  if (!state.cy) return;
  const cy = state.cy;
  const fitGraph = options.fitGraph === true;
  const resizeCy = () => {
    if (state.cy !== cy) return;
    cy.resize();
    if (fitGraph) {
      fitGraphToCurrentContext();
    }
  };
  window.requestAnimationFrame(() => {
    resizeCy();
    window.requestAnimationFrame(resizeCy);
  });
  window.setTimeout(resizeCy, 120);
}

function applyDetailsPanelState(options = {}) {
  const panel = el("details");
  const mainToggle = el("toggle-right-panel");
  const inlineToggle = el("toggle-right-panel-inline");
  if (!panel) return;

  state.detailsPanelWidth = clampDetailsPanelWidth(state.detailsPanelWidth);
  state.detailsActiveTab = normalizeDetailsTab(state.detailsActiveTab);
  const isOpen = state.detailsPanelOpen === true;

  panel.style.width = `${state.detailsPanelWidth}px`;
  panel.classList.toggle("panel-collapsed", !isOpen);
  panel.setAttribute("aria-hidden", isOpen ? "false" : "true");

  if (mainToggle) {
    mainToggle.textContent = isOpen ? "Hide Panel" : "Show Panel";
    mainToggle.setAttribute("aria-expanded", isOpen ? "true" : "false");
  }
  if (inlineToggle) {
    inlineToggle.textContent = isOpen ? "Hide" : "Show";
    inlineToggle.setAttribute("aria-expanded", isOpen ? "true" : "false");
  }

  const tabNames = ["details", "flow", "guide", "mermaid", "editor"];
  for (const tabName of tabNames) {
    const tabButton = el(`details-tab-btn-${tabName}`);
    const tabContent = el(`details-tab-${tabName}`);
    const active = tabName === state.detailsActiveTab;
    if (tabButton) {
      tabButton.dataset.active = active ? "true" : "false";
      tabButton.setAttribute("aria-pressed", active ? "true" : "false");
    }
    if (tabContent) {
      tabContent.hidden = !active;
    }
  }

  if (options.persist !== false) {
    writeLocalStorage(DETAILS_PANEL_OPEN_STORAGE_KEY, isOpen ? "1" : "0");
    writeLocalStorage(DETAILS_PANEL_WIDTH_STORAGE_KEY, String(state.detailsPanelWidth));
    writeLocalStorage(DETAILS_ACTIVE_TAB_STORAGE_KEY, state.detailsActiveTab);
  }

  if (options.resizeGraph !== false) {
    scheduleGraphResizeAfterPanelChange({ fitGraph: options.fitGraph === true });
  }
}

function setDetailsPanelOpen(nextOpen, options = {}) {
  const normalized = nextOpen === true;
  if (state.detailsPanelOpen === normalized) {
    if (options.syncUi === true) {
      applyDetailsPanelState({
        persist: options.persist !== false,
        resizeGraph: options.resizeGraph !== false,
      });
    }
    return;
  }
  state.detailsPanelOpen = normalized;
  applyDetailsPanelState({
    persist: options.persist !== false,
    resizeGraph: options.resizeGraph !== false,
  });
}

function setDetailsPanelTab(nextTab, options = {}) {
  const normalized = normalizeDetailsTab(nextTab);
  if (options.ensureOpen === true && !state.detailsPanelOpen) {
    state.detailsPanelOpen = true;
  }
  if (state.detailsActiveTab !== normalized) {
    state.detailsActiveTab = normalized;
  }
  applyDetailsPanelState({
    persist: options.persist !== false,
    resizeGraph: options.resizeGraph !== false,
  });
}

function startDetailsPanelResize(event) {
  if (!event || event.button !== 0) return;
  if (!state.detailsPanelOpen) return;

  const panel = el("details");
  if (!panel) return;

  event.preventDefault();
  const startWidth = panel.getBoundingClientRect().width || state.detailsPanelWidth;
  state.detailsResizeSession = {
    startX: event.clientX,
    startWidth,
    frameHandle: null,
    pendingWidth: null,
  };
  panel.classList.add("resizing");
  document.body.classList.add("details-resizing");

  const flushPendingResize = () => {
    const session = state.detailsResizeSession;
    if (!session || session.pendingWidth === null) return;
    const nextWidth = clampDetailsPanelWidth(session.pendingWidth);
    state.detailsPanelWidth = nextWidth;
    panel.style.width = `${nextWidth}px`;
    session.pendingWidth = null;
    session.frameHandle = null;
    if (state.cy) {
      state.cy.resize();
    }
  };

  const handleMove = (moveEvent) => {
    const session = state.detailsResizeSession;
    if (!session) return;
    const delta = session.startX - moveEvent.clientX;
    session.pendingWidth = session.startWidth + delta;
    if (session.frameHandle === null) {
      session.frameHandle = window.requestAnimationFrame(flushPendingResize);
    }
  };

  const handleStop = () => {
    const session = state.detailsResizeSession;
    if (!session) return;
    if (session.frameHandle !== null) {
      window.cancelAnimationFrame(session.frameHandle);
    }
    flushPendingResize();
    state.detailsResizeSession = null;
    panel.classList.remove("resizing");
    document.body.classList.remove("details-resizing");
    writeLocalStorage(DETAILS_PANEL_WIDTH_STORAGE_KEY, String(state.detailsPanelWidth));
    scheduleGraphResizeAfterPanelChange({ fitGraph: false });
    window.removeEventListener("mousemove", handleMove);
    window.removeEventListener("mouseup", handleStop);
    window.removeEventListener("blur", handleStop);
  };

  window.addEventListener("mousemove", handleMove);
  window.addEventListener("mouseup", handleStop);
  window.addEventListener("blur", handleStop);
}

function syncRevealSpeedControl() {
  const slider = el("reveal-speed");
  const value = el("reveal-speed-value");
  const inline = el("reveal-speed-inline");
  if (!slider) return;
  state.revealIntervalMs = clampRevealSpeedMs(state.revealIntervalMs);
  const formatted = formatRevealSpeed(state.revealIntervalMs);
  slider.value = String(state.revealIntervalMs);
  if (value) {
    value.textContent = formatted;
  }
  if (inline) {
    inline.textContent = `Reveal: ${formatted}`;
  }
}

function syncFitTightnessControl() {
  const slider = el("fit-tightness");
  const value = el("fit-tightness-value");
  state.fitTightness = clampFitTightness(state.fitTightness);
  if (slider) {
    slider.value = String(state.fitTightness);
  }
  if (value) {
    value.textContent = formatFitTightness(state.fitTightness);
  }
}

function currentKindStyleMap() {
  return state.theme === "dark" ? KIND_STYLE_MAP_DARK : KIND_STYLE_MAP_LIGHT;
}

function currentGraphTheme() {
  return state.theme === "dark" ? GRAPH_THEME_DARK : GRAPH_THEME_LIGHT;
}

function syncThemeControl() {
  const themeToggle = el("theme-toggle");
  if (!themeToggle) return;
  const isDark = state.theme === "dark";
  themeToggle.textContent = isDark ? "Theme: Dark" : "Theme: Light";
  themeToggle.setAttribute("aria-pressed", isDark ? "true" : "false");
}

function applyTheme(theme, options = {}) {
  const persist = options.persist !== false;
  const rerender = options.rerender !== false;
  const preserveViewport = options.preserveViewport !== false;
  const nextTheme = theme === "dark" ? "dark" : "light";

  state.theme = nextTheme;
  document.body.dataset.theme = nextTheme;
  syncThemeControl();
  if (state.editorMonacoReady && typeof monaco !== "undefined" && monaco?.editor) {
    monaco.editor.setTheme(nextTheme === "dark" ? "vs-dark" : "vs");
  }
  if (persist) {
    writeLocalStorage(THEME_STORAGE_KEY, nextTheme);
  }
  if (rerender && state.graph && state.cy) {
    renderGraph(state.renderMode, {
      preserveViewport,
      focusSearch: true,
    });
  }
}

function initializeThemeSetting() {
  const stored = readLocalStorage(THEME_STORAGE_KEY);
  if (stored === "light" || stored === "dark") {
    applyTheme(stored, { persist: false, rerender: false });
    return;
  }
  const prefersDark =
    typeof window.matchMedia === "function" &&
    window.matchMedia("(prefers-color-scheme: dark)").matches;
  applyTheme(prefersDark ? "dark" : "light", { persist: false, rerender: false });
}

function mapSetAdd(map, key, value) {
  let bucket = map.get(key);
  if (!bucket) {
    bucket = new Set();
    map.set(key, bucket);
  }
  bucket.add(value);
}

function mapListPush(map, key, value) {
  let bucket = map.get(key);
  if (!bucket) {
    bucket = [];
    map.set(key, bucket);
  }
  bucket.push(value);
}

function compareNodeLabels(nodeById, aId, bId) {
  const a = nodeById.get(aId);
  const b = nodeById.get(bId);
  const aLabel = normalize(a?.label || aId);
  const bLabel = normalize(b?.label || bId);
  if (aLabel < bLabel) return -1;
  if (aLabel > bLabel) return 1;
  return 0;
}

function hashString(value) {
  let hash = 0;
  const text = String(value || "");
  for (let index = 0; index < text.length; index += 1) {
    hash = ((hash << 5) - hash + text.charCodeAt(index)) | 0;
  }
  return hash >>> 0;
}

function deterministicOffset(nodeId) {
  const hash = hashString(nodeId);
  const angle = (hash % 360) * (Math.PI / 180);
  const radius = 26 + ((hash >>> 8) % 52);
  return {
    x: Math.cos(angle) * radius,
    y: Math.sin(angle) * radius,
  };
}

function currentFitPaddingProfile() {
  const base = state.mermaidSplitOpen
    ? {
        reset: 44,
        all: 56,
        matched: 68,
        focused: 88,
        selectedNeighborhood: 80,
      }
    : {
        reset: 48,
        all: 62,
        matched: 74,
        focused: 92,
        selectedNeighborhood: 86,
      };

  // 0 => looser fit (more margin), 100 => tighter fit (less margin).
  const tightness = clampFitTightness(state.fitTightness);
  const multiplier = 1.35 - (tightness / 100) * 0.65;
  const scale = (value) => Math.max(12, Math.round(value * multiplier));
  return {
    reset: scale(base.reset),
    all: scale(base.all),
    matched: scale(base.matched),
    focused: scale(base.focused),
    selectedNeighborhood: scale(base.selectedNeighborhood),
  };
}

function fitGraphToCurrentContext(cy = state.cy) {
  if (!cy) return;
  const query = state.searchQuery.trim();
  const matched = cy.nodes(".matched");
  const padding = currentFitPaddingProfile();
  if (query.length > 0 && matched.length > 0 && matched.length <= 180) {
    // Include each matched node's closed neighborhood so the call /
    // contains fan-out we just pulled into the slice actually fits
    // on screen. Previously the fit zoomed onto matched nodes only,
    // pushing the 30 outgoing neighbours of a function search off the
    // viewport — they were in the graph but invisible without manual
    // panning.
    const matchedScope = matched.closedNeighborhood().union(matched);
    cy.fit(matchedScope, padding.matched);
    return;
  }

  if (state.selectedNodeId) {
    const selected = cy.$id(state.selectedNodeId);
    if (selected && !selected.empty()) {
      cy.fit(selected.closedNeighborhood().union(selected), padding.selectedNeighborhood);
      return;
    }
  }

  const nodes = cy.nodes();
  if (nodes.length > 0) {
    cy.fit(nodes, padding.all);
  }
}

function buildGraphIndex(graph) {
  const nodeById = new Map();
  const degreeById = new Map();
  const fileNodeIdByPath = new Map();
  const fileChildSets = new Map();
  const moduleChildSets = new Map();
  const symbolChildSets = new Map();
  const symbolNodeIdsByPath = new Map();
  const nodeIdsByNormalizedLabel = new Map();
  const relationNeighborSetsByNode = new Map();
  const outgoingVisibleSetsByNode = new Map();
  const incomingVisibleSetsByNode = new Map();
  const outgoingVisibleEdgeListByNode = new Map();
  const incomingVisibleEdgeListByNode = new Map();
  const parentSetsByChild = new Map();
  const crateNodeIds = [];
  let totalFunctions = 0;

  for (const node of graph.nodes) {
    nodeById.set(node.id, node);
    if (node.kind === "file" && node.path) {
      fileNodeIdByPath.set(String(node.path), node.id);
    }
    if (SYMBOL_KINDS.has(node.kind) && node.path) {
      mapSetAdd(symbolNodeIdsByPath, String(node.path), node.id);
    }
    const normalizedLabel = normalize(node.label || "");
    if (normalizedLabel.length > 0) {
      mapSetAdd(nodeIdsByNormalizedLabel, normalizedLabel, node.id);
    }
    if (node.kind === "crate") {
      crateNodeIds.push(node.id);
    }
    if (node.kind === "function") {
      totalFunctions += 1;
    }
  }

  for (const edge of graph.edges) {
    if (!nodeById.has(edge.from) || !nodeById.has(edge.to)) continue;
    degreeById.set(edge.from, (degreeById.get(edge.from) || 0) + 1);
    degreeById.set(edge.to, (degreeById.get(edge.to) || 0) + 1);

    if (VISIBLE_EDGE_KINDS.has(edge.kind)) {
      mapSetAdd(relationNeighborSetsByNode, edge.from, edge.to);
      mapSetAdd(relationNeighborSetsByNode, edge.to, edge.from);
      mapSetAdd(outgoingVisibleSetsByNode, edge.from, edge.to);
      mapSetAdd(incomingVisibleSetsByNode, edge.to, edge.from);
      mapListPush(outgoingVisibleEdgeListByNode, edge.from, {
        neighborId: edge.to,
        kind: edge.kind,
      });
      mapListPush(incomingVisibleEdgeListByNode, edge.to, {
        neighborId: edge.from,
        kind: edge.kind,
      });
    }

    const childNode = nodeById.get(edge.to);
    if (edge.kind === "contains") {
      if (childNode.kind === "file") {
        mapSetAdd(fileChildSets, edge.from, edge.to);
      } else if (childNode.kind === "module") {
        mapSetAdd(moduleChildSets, edge.from, edge.to);
      } else if (SYMBOL_KINDS.has(childNode.kind)) {
        mapSetAdd(symbolChildSets, edge.from, edge.to);
      }
      mapSetAdd(parentSetsByChild, edge.to, edge.from);
    } else if (edge.kind === "defines") {
      if (SYMBOL_KINDS.has(childNode.kind)) {
        mapSetAdd(symbolChildSets, edge.from, edge.to);
      }
      mapSetAdd(parentSetsByChild, edge.to, edge.from);
    }
  }

  const sortByLabel = (aId, bId) => compareNodeLabels(nodeById, aId, bId);
  const sortByDegreeThenLabel = (aId, bId) => {
    const delta = (degreeById.get(bId) || 0) - (degreeById.get(aId) || 0);
    if (delta !== 0) return delta;
    return compareNodeLabels(nodeById, aId, bId);
  };

  const finalizeMap = (setMap, sortFn) => {
    const result = new Map();
    for (const [key, values] of setMap.entries()) {
      const sorted = Array.from(values).sort(sortFn);
      result.set(key, sorted);
    }
    return result;
  };

  const sortedCrateNodeIds = crateNodeIds.sort(sortByLabel);
  const rootNodeIds = sortedCrateNodeIds.slice();

  return {
    nodeById,
    degreeById,
    fileNodeIdByPath,
    crateNodeIds: sortedCrateNodeIds,
    rootNodeIds,
    fileChildrenByParent: finalizeMap(fileChildSets, sortByLabel),
    moduleChildrenByParent: finalizeMap(moduleChildSets, sortByLabel),
    symbolChildrenByParent: finalizeMap(symbolChildSets, sortByDegreeThenLabel),
    symbolNodeIdsByPath: finalizeMap(symbolNodeIdsByPath, sortByDegreeThenLabel),
    nodeIdsByNormalizedLabel: finalizeMap(nodeIdsByNormalizedLabel, sortByDegreeThenLabel),
    relationNeighborsByNode: finalizeMap(relationNeighborSetsByNode, sortByDegreeThenLabel),
    outgoingVisibleByNode: finalizeMap(outgoingVisibleSetsByNode, sortByDegreeThenLabel),
    incomingVisibleByNode: finalizeMap(incomingVisibleSetsByNode, sortByDegreeThenLabel),
    outgoingVisibleEdgesByNode: outgoingVisibleEdgeListByNode,
    incomingVisibleEdgesByNode: incomingVisibleEdgeListByNode,
    parentsByChild: finalizeMap(parentSetsByChild, sortByLabel),
    totalFunctions,
  };
}

function nodeMatchesQuery(node, queryNormalized) {
  return (
    normalize(node.label).includes(queryNormalized) ||
    normalize(node.id).includes(queryNormalized) ||
    normalize(node.path).includes(queryNormalized) ||
    normalize(node.kind).includes(queryNormalized)
  );
}

function hideNodeTooltip() {
  const tooltip = el("node-tooltip");
  if (!tooltip) return;
  tooltip.classList.remove("visible");
}

function clearNodeTooltipTimers() {
  if (state.nodeTooltipHideHandle !== null) {
    window.clearTimeout(state.nodeTooltipHideHandle);
    state.nodeTooltipHideHandle = null;
  }
  if (state.nodeTooltipShowHandle !== null) {
    window.clearTimeout(state.nodeTooltipShowHandle);
    state.nodeTooltipShowHandle = null;
  }
}

function placeNodeTooltip(evt) {
  const tooltip = el("node-tooltip");
  if (!tooltip || !evt) return;

  const x = Number(evt.clientX || 0) + 14;
  const y = Number(evt.clientY || 0) + 14;
  tooltip.style.left = `${x}px`;
  tooltip.style.top = `${y}px`;
}

function nodeApiDescriptor(nodeData) {
  if (!nodeData || typeof nodeData !== "object") return "";
  const kind = String(nodeData.kind || "");
  if (kind === "endpoint") {
    const method = String(nodeData.method || "").trim();
    const route = String(nodeData.route || "").trim();
    const handler = String(nodeData.handler || "").trim();
    const head = `${method} ${route}`.trim() || String(nodeData.label || "").trim();
    if (handler.length > 0) {
      return `${head} | handler:${handler}`;
    }
    return head;
  }
  if (kind === "api_call") {
    const method = String(nodeData.method || "").trim();
    const target = String(nodeData.target || "").trim();
    const caller = String(nodeData.caller || "").trim();
    const base = target.length > 0
      ? `${method} ${target}`.trim()
      : String(nodeData.label || "").trim();
    if (caller.length > 0) {
      return `${base} | via:${caller}`;
    }
    return base;
  }
  return "";
}

function showNodeTooltip(node, evt) {
  const tooltip = el("node-tooltip");
  if (!tooltip || !node) return;

  const legend = node.data("legend");
  const kind = node.data("kind");
  const label = node.data("full_label") || node.data("label") || node.id();
  const descriptor = nodeApiDescriptor(node.data());
  const path = String(node.data("path") || "").trim();
  const line = Number(node.data("line") || 0);
  const location = path.length > 0
    ? `${path}${line > 0 ? `:${String(line)}` : ""}`
    : "";
  const extraParts = [descriptor, location].filter((part) => String(part).trim().length > 0);
  tooltip.innerHTML =
    `<div class="node-tooltip-title">${escapeHtml(label)}</div>` +
    `<div class="node-tooltip-meta">#${escapeHtml(legend)} · ${escapeHtml(kind)}</div>` +
    (extraParts.length > 0
      ? `<div class="node-tooltip-extra">${escapeHtml(extraParts.join(" • "))}</div>`
      : "");
  placeNodeTooltip(evt);
  tooltip.classList.add("visible");
}

function pointerEventFromNode(node) {
  if (!node) return null;
  const cy = node.cy();
  const container = cy?.container?.();
  if (!container) return null;

  const rect = container.getBoundingClientRect();
  const rendered = node.renderedPosition();
  return {
    clientX: rect.left + rendered.x,
    clientY: rect.top + rendered.y,
  };
}

function showTransientNodeTooltip(node, evt, durationMs = 1200) {
  if (!node) return;
  const pointer = evt || pointerEventFromNode(node);
  showNodeTooltip(node, pointer);
  if (state.nodeTooltipHideHandle !== null) {
    window.clearTimeout(state.nodeTooltipHideHandle);
  }
  state.nodeTooltipHideHandle = window.setTimeout(() => {
    state.nodeTooltipHideHandle = null;
    hideNodeTooltip();
  }, durationMs);
}

function scheduleTransientNodeTooltip(nodeId, delayMs = 120, durationMs = 1200) {
  if (!nodeId) return;
  if (state.nodeTooltipShowHandle !== null) {
    window.clearTimeout(state.nodeTooltipShowHandle);
  }
  state.nodeTooltipShowHandle = window.setTimeout(() => {
    state.nodeTooltipShowHandle = null;
    const cy = state.cy;
    if (!cy) return;
    const node = cy.$id(nodeId);
    if (!node || node.empty()) return;
    showTransientNodeTooltip(node, null, durationMs);
  }, Math.max(0, delayMs));
}

function renderMeta(graph) {
  const meta = el("graph-meta");
  if (!meta) return;
  const commit =
    typeof graph.commit === "string" && graph.commit.length > 0
      ? graph.commit.slice(0, 12)
      : "unknown";
  const modeLabel = state.renderMode;
  const totalFunctions = state.graphIndex?.totalFunctions || 0;
  const searchPart = state.searchQuery.trim().length > 0
    ? ` search_matches=${state.searchShownMatches}/${state.searchTotalMatches}${state.searchTruncated ? "+" : ""}`
    : "";
  const impactPart = state.impactActive
    ? ` impact_nodes=${state.impactNodeIds.size} changed_files=${state.impactChangedFiles.length}`
    : "";
  meta.textContent =
    `workspace=${graph.workspace} commit=${commit} mode=${modeLabel} ` +
    `shown_nodes=${state.renderedNodeCount}/${graph.nodes.length} ` +
    `shown_edges=${state.renderedEdgeCount}/${graph.edges.length} ` +
    `shown_functions=${state.renderedFunctionCount}/${totalFunctions}` +
    searchPart +
    impactPart;
}

function countKindsForIds(nodeIds) {
  const counts = new Map();
  const index = state.graphIndex;
  if (!index) return counts;
  for (const nodeId of nodeIds) {
    const kind = index.nodeById.get(nodeId)?.kind || "unknown";
    counts.set(kind, (counts.get(kind) || 0) + 1);
  }
  return counts;
}

function summarizeKindCounts(countMap, maxKinds = 3) {
  const parts = Array.from(countMap.entries())
    .sort((a, b) => b[1] - a[1])
    .slice(0, maxKinds)
    .map(([kind, count]) => `${count} ${kind}${count === 1 ? "" : "s"}`);
  return parts.join(", ");
}

function describeExpansionAction(action, kind) {
  if (action === "expand_modules") return "expanded modules for";
  if (action === "expand_files") return "expanded files for";
  if (action === "expand_symbols") return "expanded symbols for";
  if (action === "expand_relations") return "expanded related nodes for";
  if (action === "collapse_relations") return "collapsed related nodes for";
  if (action === "collapse") return "collapsed branch for";
  return `updated ${kind || "node"}`;
}

function revealPriorityForKind(kind) {
  if (kind === "crate") return 0;
  if (kind === "module") return 1;
  if (kind === "file") return 2;
  if (SYMBOL_KINDS.has(kind)) return 3;
  return 9;
}

function sortNodeIdsForReveal(nodeIds) {
  const index = state.graphIndex;
  if (!index) return Array.from(nodeIds);
  return Array.from(nodeIds).sort((aId, bId) => {
    const aKind = index.nodeById.get(aId)?.kind || "";
    const bKind = index.nodeById.get(bId)?.kind || "";
    const kindDelta = revealPriorityForKind(aKind) - revealPriorityForKind(bKind);
    if (kindDelta !== 0) return kindDelta;
    return compareNodeLabels(index.nodeById, aId, bId);
  });
}

function buildExpansionContext(beforeVisible, afterVisible, sourceNodeId, actionInfo) {
  const addedNodeIds = [];
  const removedNodeIds = [];

  for (const nodeId of afterVisible) {
    if (!beforeVisible.has(nodeId)) {
      addedNodeIds.push(nodeId);
    }
  }
  for (const nodeId of beforeVisible) {
    if (!afterVisible.has(nodeId)) {
      removedNodeIds.push(nodeId);
    }
  }

  const addedKinds = countKindsForIds(addedNodeIds);
  const removedKinds = countKindsForIds(removedNodeIds);
  const orderedAddedNodeIds = sortNodeIdsForReveal(addedNodeIds);
  const orderedRemovedNodeIds = sortNodeIdsForReveal(removedNodeIds);

  return {
    sourceNodeId,
    action: actionInfo.action,
    sourceKind: actionInfo.kind,
    addedNodeIds: orderedAddedNodeIds,
    removedNodeIds: orderedRemovedNodeIds,
    addedCount: addedNodeIds.length,
    removedCount: removedNodeIds.length,
    addedSummary: summarizeKindCounts(addedKinds),
    removedSummary: summarizeKindCounts(removedKinds),
    beforeCount: beforeVisible.size,
    afterCount: afterVisible.size,
    timestampMs: Date.now(),
  };
}

function setExpansionStatusText(message, tone = "muted") {
  const status = el("expansion-status");
  if (!status) return;
  status.textContent = message;
  status.dataset.tone = tone;
}

function setExpansionStatus(context, tone = "grow") {
  const status = el("expansion-status");
  if (!status) return;
  if (!context) {
    setExpansionStatusText("No expansion yet.", "muted");
    return;
  }

  const sourceLabel = state.graphIndex?.nodeById.get(context.sourceNodeId)?.label || context.sourceNodeId;
  const actionLabel = describeExpansionAction(context.action, context.sourceKind);
  const diffs = [];
  if (context.addedCount > 0) {
    diffs.push(`+${context.addedCount}${context.addedSummary ? ` (${context.addedSummary})` : ""}`);
  }
  if (context.removedCount > 0) {
    diffs.push(`-${context.removedCount}${context.removedSummary ? ` (${context.removedSummary})` : ""}`);
  }
  if (diffs.length === 0) {
    diffs.push("no visibility change");
  }

  status.textContent = `${actionLabel} "${sourceLabel}" • ${diffs.join(" • ")} • now ${context.afterCount} shown`;
  status.dataset.tone = tone;
}

function detailsHtml(nodeData) {
  const viewData = { ...nodeData };
  if (viewData.full_label) {
    viewData.label = viewData.full_label;
  }
  const lines = [];
  for (const key of [
    "id",
    "kind",
    "label",
    "method",
    "route",
    "handler",
    "target",
    "crate",
    "module",
    "path",
    "line",
    "version",
  ]) {
    if (viewData[key] !== undefined) {
      lines.push(`<div class="kv"><strong>${key}</strong>: ${String(viewData[key])}</div>`);
    }
  }
  // Per-function classification (production / dead / test / untested).
  if (nodeData.kind === "function") {
    const flags = [];
    if (nodeData.test) flags.push("test");
    if (nodeData.public) flags.push("public");
    if (nodeData.visibility) flags.push(`vis=${nodeData.visibility}`);
    if (nodeData.implements_trait) flags.push(`impl ${nodeData.implements_trait}`);
    if (flags.length) {
      lines.push(`<div class="kv"><strong>flags</strong>: ${flags.join(" · ")}</div>`);
    }
  }
  // Hierarchical rollup for crate / module / file nodes.
  const rollup = nodeData.rollup;
  if (rollup && typeof rollup === "object") {
    const pct = (n, d) => (d > 0 ? Math.round((n / d) * 100) : 0);
    const prod = rollup.prod_fns || 0;
    lines.push(`<h3 class="rollup-h">Rollup</h3>`);
    lines.push(`<div class="rollup">`);
    lines.push(`<div class="rollup-row"><span>Functions</span><span><strong>${rollup.total_fns}</strong> total · ${prod} prod · ${rollup.test_fns} test · ${rollup.public_fns} public</span></div>`);
    lines.push(`<div class="rollup-row"><span>Dead (no prod callers)</span><span><strong>${rollup.dead_fns}</strong> / ${prod} (${pct(rollup.dead_fns, prod)}%)</span></div>`);
    lines.push(`<div class="rollup-row"><span>Untested (no test callers)</span><span><strong>${rollup.untested_fns}</strong> / ${prod} (${pct(rollup.untested_fns, prod)}%)</span></div>`);
    lines.push(`<div class="rollup-row"><span>Caller edges in</span><span>${rollup.prod_caller_edges.toLocaleString()} prod · ${rollup.test_caller_edges.toLocaleString()} test</span></div>`);
    lines.push(`</div>`);
  }
  return lines.join("\n");
}

function setDetails(node) {
  const body = el("details-body");
  if (!node) {
    body.innerHTML = "Select a node to inspect.";
    return;
  }

  const outEdges = node.outgoers("edge").map((e) => e.data());
  const inEdges = node.incomers("edge").map((e) => e.data());
  // Bucket by edge kind so the panel shows a high-signal summary
  // (`calls 12, contains 1`) instead of a 60-line raw JSON dump.
  const bucketEdges = (edges) => {
    const m = {};
    edges.forEach((e) => {
      const k = e.kind || e.type || "?";
      m[k] = (m[k] || 0) + 1;
    });
    return Object.entries(m).sort((a, b) => b[1] - a[1]);
  };
  const pillFor = (k, n) =>
    '<span class="edge-pill">' + escapeHtml(String(k)) + ' <strong>' + n + '</strong></span>';
  const pillsFor = (edges) => {
    const pairs = bucketEdges(edges);
    if (!pairs.length) return '<span class="edge-pill edge-pill-muted">(none)</span>';
    return pairs.map((p) => pillFor(p[0], p[1])).join(" ");
  };

  body.innerHTML = [
    detailsHtml(node.data()),
    '<h3>Outgoing edges (' + outEdges.length + ')</h3>',
    '<div class="edge-pills">' + pillsFor(outEdges) + '</div>',
    '<h3>Incoming edges (' + inEdges.length + ')</h3>',
    '<div class="edge-pills">' + pillsFor(inEdges) + '</div>',
    '<details class="edge-raw"><summary>Raw outgoing JSON</summary><pre>'
      + escapeHtml(JSON.stringify(outEdges.slice(0, 60), null, 2)) + '</pre></details>',
    '<details class="edge-raw"><summary>Raw incoming JSON</summary><pre>'
      + escapeHtml(JSON.stringify(inEdges.slice(0, 60), null, 2)) + '</pre></details>',
  ].join("\n");

  const nodeData = node.data();
  suggestEditorPathFromNode(nodeData);
  if (nodeData.kind === "endpoint") {
    setFlowSource(nodeData.id, { ensureOption: true, setPayload: false });
  }
}

// Render the details pane straight from the raw graph index (no
// cytoscape lookup required). Used when a Mermaid click lands on a
// node that the live cy graph doesn't currently expose — e.g. nodes
// inside a /flows subgraph or behind progressive filtering. Mirrors
// the layout of `setDetails(cyNode)` so the user gets the same view.
function setDetailsFromGraphNodeId(nodeId) {
  const body = el("details-body");
  if (!body) return false;
  const index = state.graphIndex;
  const node = index && index.nodeById.get(nodeId);
  if (!node) return false;

  const outByKind = {};
  const inByKind = {};
  if (state.graph && Array.isArray(state.graph.edges)) {
    for (const edge of state.graph.edges) {
      const k = edge.kind || "?";
      if (edge.from === nodeId) outByKind[k] = (outByKind[k] || 0) + 1;
      if (edge.to === nodeId) inByKind[k] = (inByKind[k] || 0) + 1;
    }
  }
  const totalOut = Object.values(outByKind).reduce((a, b) => a + b, 0);
  const totalIn = Object.values(inByKind).reduce((a, b) => a + b, 0);
  const pillFor = (k, n) =>
    '<span class="edge-pill">' + escapeHtml(String(k)) + ' <strong>' + n + '</strong></span>';
  const pillsFor = (m) => {
    const pairs = Object.entries(m).sort((a, b) => b[1] - a[1]);
    if (!pairs.length) return '<span class="edge-pill edge-pill-muted">(none)</span>';
    return pairs.map((p) => pillFor(p[0], p[1])).join(" ");
  };

  body.innerHTML = [
    detailsHtml(node),
    '<h3>Outgoing edges (' + totalOut + ')</h3>',
    '<div class="edge-pills">' + pillsFor(outByKind) + '</div>',
    '<h3>Incoming edges (' + totalIn + ')</h3>',
    '<div class="edge-pills">' + pillsFor(inByKind) + '</div>',
    '<div class="search-insights-muted" style="margin-top:8px">Node not present in current cytoscape view &mdash; details rendered from raw graph index.</div>',
  ].join("\n");
  return true;
}

function normalizeRepoRelativePath(inputPath) {
  return String(inputPath || "")
    .trim()
    .replaceAll("\\", "/")
    .replace(/^\.\/+/, "")
    .replace(/^\/+/, "");
}

function lookupFileNodeIdForPath(inputPath) {
  const index = state.graphIndex;
  if (!index) return null;
  const normalizedPath = normalizeRepoRelativePath(inputPath);
  if (!normalizedPath) return null;
  if (index.fileNodeIdByPath.has(normalizedPath)) {
    return index.fileNodeIdByPath.get(normalizedPath);
  }
  for (const [path, nodeId] of index.fileNodeIdByPath.entries()) {
    if (normalizeRepoRelativePath(path) === normalizedPath) {
      return nodeId;
    }
  }
  return null;
}

function setEditorStatus(message, tone = "muted") {
  const status = el("editor-status");
  if (!status) return;
  status.textContent = message;
  status.dataset.tone = tone;
}

function syncEditorControls() {
  const loadButton = el("editor-load-file");
  const saveButton = el("editor-save-file");
  const refreshButton = el("editor-refresh-file");
  const pathInput = el("editor-file-path");
  const autoRefresh = el("editor-auto-refresh");
  const hasPath = normalizeRepoRelativePath(state.editorPath).length > 0;

  if (pathInput && !state.editorLoading && !state.editorSaving && state.editorPath.length > 0) {
    pathInput.value = state.editorPath;
  }
  if (loadButton) {
    loadButton.disabled = state.editorLoading || state.editorSaving;
    loadButton.textContent = state.editorLoading ? "Loading..." : "Load";
  }
  if (saveButton) {
    saveButton.disabled =
      !state.editorMonacoReady ||
      !hasPath ||
      !state.editorDirty ||
      state.editorLoading ||
      state.editorSaving;
    saveButton.textContent = state.editorSaving ? "Saving..." : "Save";
  }
  if (refreshButton) {
    refreshButton.disabled = !hasPath || state.editorLoading || state.editorSaving;
  }
  if (autoRefresh) {
    autoRefresh.checked = state.editorAutoRefresh;
    autoRefresh.disabled = state.editorLoading || state.editorSaving;
  }
}

function setFlowStatus(message, tone = "muted") {
  const status = el("flow-status");
  if (!status) return;
  status.textContent = message;
  status.dataset.tone = tone;
}

function setGuideStatus(message, tone = "muted") {
  const status = el("guide-status");
  if (!status) return;
  status.textContent = message;
  status.dataset.tone = tone;
}

function setGuideArtifactsDirty(dirty) {
  const stale = el("guide-artifact-stale");
  state.guideArtifactsDirty = dirty === true;
  if (stale) {
    stale.hidden = !state.guideArtifactsDirty;
  }
}

function describeGuideEdgeTransition(edgeKind) {
  const normalized = String(edgeKind || "").trim();
  switch (normalized) {
    case "handles":
      return "HTTP endpoint hands control to handler logic.";
    case "calls":
      return "Function call transfer inside code path.";
    case "calls_api":
      return "Outbound API boundary crossed.";
    case "targets_endpoint":
      return "Targets another endpoint.";
    case "accepts_payload":
      return "Payload enters downstream contract surface.";
    case "references":
      return "Reference-level dependency hop.";
    case "depends_on":
      return "Compile/runtime dependency hop.";
    default:
      return normalized.length > 0 ? normalized : "Control moved to next node.";
  }
}

function buildGuideTraceMermaid(model) {
  if (!model?.selectedTrace?.trace || !Array.isArray(model.selectedTrace.trace.nodes)) {
    return "flowchart LR\n  no_trace[\"No trace selected\"]";
  }
  const trace = model.selectedTrace.trace;
  const lines = ["flowchart LR"];
  const nodeIds = [];
  trace.nodes.forEach((node, index) => {
    const alias = `s${index + 1}`;
    nodeIds.push(alias);
    const label = escapeMermaidLabel(
      truncateNodeLabel(String(node?.label || node?.id || "unknown"), 88)
    );
    lines.push(`  ${alias}["${label}"]`);
  });
  for (let index = 0; index < nodeIds.length - 1; index += 1) {
    const edgeKind = String(trace.edge_kinds?.[index] || "calls");
    lines.push(`  ${nodeIds[index]} -->|${escapeMermaidLabel(edgeKind)}| ${nodeIds[index + 1]}`);
  }
  return lines.join("\n");
}

function shouldGuideAutoRebuild() {
  return state.guideAutoRebuild === true;
}

function summarizeGuideKinds(nodes) {
  const kindCounts = new Map();
  (Array.isArray(nodes) ? nodes : []).forEach((node) => {
    const kind = String(node?.kind || "unknown");
    kindCounts.set(kind, (kindCounts.get(kind) || 0) + 1);
  });
  return summarizeKindCounts(kindCounts, 6) || "none";
}

function collectGuideTraceNodes(traces) {
  const nodeMap = new Map();
  (Array.isArray(traces) ? traces : []).forEach((trace) => {
    (Array.isArray(trace?.nodes) ? trace.nodes : []).forEach((node) => {
      const nodeId = String(node?.id || "").trim();
      if (!nodeId || nodeMap.has(nodeId)) return;
      nodeMap.set(nodeId, {
        id: nodeId,
        kind: String(node?.kind || "unknown"),
        label: String(node?.label || nodeId),
        crate: String(node?.crate || ""),
        module: String(node?.module || ""),
        path: String(node?.path || ""),
      });
    });
  });
  return Array.from(nodeMap.values());
}

function pathPatternToPrefix(pattern) {
  const text = String(pattern || "").trim();
  if (!text) return "";
  const wildcardIndex = text.search(/[*?\[]/);
  const prefix = wildcardIndex >= 0 ? text.slice(0, wildcardIndex) : text;
  return normalizeRepoRelativePath(prefix);
}

function buildGuideOwnerRuleIndexFromRules(rawRules) {
  const rules = Array.isArray(rawRules) ? rawRules : [];
  const indexed = [];
  rules.forEach((rule) => {
    const name = String(rule?.name || "").trim();
    if (!name) return;
    const codePatterns = Array.isArray(rule?.code_patterns) ? rule.code_patterns : [];
    const prefixes = codePatterns
      .map((pattern) => pathPatternToPrefix(pattern))
      .filter((prefix) => prefix.length > 0);
    if (prefixes.length === 0) return;
    indexed.push({
      name,
      prefixes,
      docPatterns: Array.isArray(rule?.doc_patterns)
        ? rule.doc_patterns.map((value) => String(value || "")).filter((value) => value.length > 0)
        : [],
    });
  });
  return indexed;
}

async function ensureGuideOwnerRuleIndexLoaded() {
  if (state.guideOwnerRuleLoadAttempted) {
    return state.guideOwnerRuleIndex;
  }
  state.guideOwnerRuleLoadAttempted = true;
  try {
    const response = await fetch("/api/file/read?path=docs/docs_guard_rules.json", {
      cache: "no-store",
    });
    if (!response.ok) {
      throw new Error(`rules http ${response.status}`);
    }
    const payload = await response.json().catch(() => ({}));
    const content = String(payload?.content || "");
    if (!content.trim()) {
      throw new Error("rules content empty");
    }
    const parsed = JSON.parse(content);
    state.guideOwnerRuleIndex = buildGuideOwnerRuleIndexFromRules(parsed?.rules);
    return state.guideOwnerRuleIndex;
  } catch (_err) {
    state.guideOwnerRuleIndex = [];
    return state.guideOwnerRuleIndex;
  }
}

function findOwnerRuleForPath(path, ownerRuleIndex) {
  const normalizedPath = normalizeRepoRelativePath(path);
  if (!normalizedPath) return null;
  const rules = Array.isArray(ownerRuleIndex) ? ownerRuleIndex : [];
  let best = null;
  let bestPrefixLength = -1;
  rules.forEach((rule) => {
    (rule.prefixes || []).forEach((prefix) => {
      if (!prefix) return;
      if (!normalizedPath.startsWith(prefix)) return;
      if (prefix.length > bestPrefixLength) {
        bestPrefixLength = prefix.length;
        best = rule;
      }
    });
  });
  return best;
}

function inferGuideOwners(nodes, source, ownerRuleIndex) {
  const ownerMap = new Map();
  (Array.isArray(nodes) ? nodes : []).forEach((node) => {
    const nodePath = normalizeRepoRelativePath(node?.path);
    if (!nodePath) return;
    const rule = findOwnerRuleForPath(nodePath, ownerRuleIndex);
    if (rule) {
      const key = `rule:${rule.name}`;
      if (!ownerMap.has(key)) {
        ownerMap.set(key, {
          crate: String(node?.crate || ""),
          owner: `component:${rule.name}`,
          reason: `mapped via docs_guard rule ${rule.name}`,
          docs: (rule.docPatterns || []).slice(0, 4),
        });
      }
      return;
    }
    const crate = String(node?.crate || "").trim();
    if (!crate) return;
    const key = `crate:${crate}`;
    if (ownerMap.has(key)) return;
    ownerMap.set(key, {
      crate,
      owner: `@${crate}-owners`,
      reason: "inferred from crate path",
      docs: [],
    });
  });

  if (ownerMap.size === 0) {
    const sourceRoute = String(source?.route || "").trim();
    const sourceMethod = String(source?.method || "").trim().toLowerCase();
    if (sourceMethod.length > 0 || sourceRoute.length > 0) {
      ownerMap.set("fallback:api", {
        crate: "api-platform",
        owner: "@api-platform-owners",
        reason: "fallback from source endpoint metadata",
        docs: [],
      });
    }
  }

  return Array.from(ownerMap.values()).slice(0, 12);
}

function buildGuideRollbackHints(summary) {
  const hints = [];
  if (summary.highFindingCount > 0) {
    hints.push("Gate the source handler behind a temporary validation/feature flag while fixing high-risk paths.");
  }
  if (summary.mediumFindingCount > 0) {
    hints.push("Roll back newly added call edges first (highest-depth traces) before broad source rollback.");
  }
  if (summary.downstreamApiCount > 0) {
    hints.push("Temporarily disable outbound API dispatch on impacted traces or switch to a safe/mock endpoint.");
  }
  if (summary.payloadTags.includes("secret") || summary.payloadTags.includes("pii")) {
    hints.push("Force sanitization/redaction at handler boundary before re-enabling outbound integrations.");
  }
  if (summary.touchedCrateCount > 1) {
    hints.push("Rollback touched crates in reverse dependency order (leaf crates first, shared crates last).");
  }
  if (summary.lowConfidenceTraceCount > 0) {
    hints.push("Add targeted tests for low-confidence traces before restoring full traffic.");
  }
  hints.push("Run `Flow -> Run Delta vs Base` after rollback and verify high/medium findings do not increase.");
  return hints;
}

function buildGuideFailureModes(model) {
  const modes = [];
  if (model.downstreamApiCount > 0) {
    modes.push({
      severity: "high",
      mode: "Outbound integration failure",
      detail: "API sinks are reachable from this source. Network/auth/rate-limit failures can break end-to-end execution.",
    });
  }
  if (model.payloadTags.includes("secret") || model.payloadTags.includes("pii")) {
    modes.push({
      severity: "high",
      mode: "Sensitive payload egress risk",
      detail: "Payload tags include secret/PII and traces hit outbound or cross-boundary nodes.",
    });
  }
  if (model.lowConfidenceTraceCount > 0) {
    modes.push({
      severity: "medium",
      mode: "Static linkage uncertainty",
      detail: `${model.lowConfidenceTraceCount} selected traces have low confidence and may hide true runtime behavior.`,
    });
  }
  if (model.crossCrateTraceCount > 0) {
    modes.push({
      severity: "medium",
      mode: "Cross-crate coupling",
      detail: `${model.crossCrateTraceCount} traces cross crate boundaries and may require coordinated deploy/rollback.`,
    });
  }
  if (modes.length === 0) {
    modes.push({
      severity: "low",
      mode: "No major static failure hotspots",
      detail: "Current flow slice shows no high-confidence structural hazards.",
    });
  }
  return modes;
}

function buildGuideDependencies(view) {
  const upstreamHeads = Array.from(
    new Set(
      (view?.upstream || [])
        .map((trace) => trace?.nodes?.[0])
        .filter(Boolean)
        .map((node) => ({
          id: String(node.id || ""),
          label: String(node.label || node.id || "unknown"),
          kind: String(node.kind || "unknown"),
          crate: String(node.crate || ""),
          path: String(node.path || ""),
        }))
        .map((node) => JSON.stringify(node))
    )
  ).map((value) => JSON.parse(value));

  const downstreamSinks = Array.from(
    new Set(
      (view?.downstream || [])
        .map((trace) => {
          const nodes = Array.isArray(trace?.nodes) ? trace.nodes : [];
          return nodes.length > 0 ? nodes[nodes.length - 1] : null;
        })
        .filter(Boolean)
        .map((node) => ({
          id: String(node.id || ""),
          label: String(node.label || node.id || "unknown"),
          kind: String(node.kind || "unknown"),
          crate: String(node.crate || ""),
          path: String(node.path || ""),
          target: String(node.target || ""),
          method: String(node.method || ""),
          route: String(node.route || ""),
        }))
        .map((node) => JSON.stringify(node))
    )
  ).map((value) => JSON.parse(value));

  return {
    upstreamHeads,
    downstreamSinks,
  };
}

function buildGuideStepModels(model) {
  if (!model?.selectedTrace?.trace) return [];
  const trace = model.selectedTrace.trace;
  const nodes = Array.isArray(trace.nodes) ? trace.nodes : [];
  if (nodes.length === 0) return [];
  const edgeKinds = Array.isArray(trace.edge_kinds) ? trace.edge_kinds : [];
  let tagState = new Set(Array.isArray(model.payloadTags) ? model.payloadTags : []);

  const steps = nodes.map((node, index) => {
    const edgeKind = index === 0 ? "source" : String(edgeKinds[index - 1] || "calls");
    const beforeTags = Array.from(tagState.values()).sort();
    const label = String(node?.label || node?.id || "unknown");
    const lowerLabel = label.toLowerCase();
    const notes = [];

    if (edgeKind === "accepts_payload") {
      tagState.add("accepted_payload");
      notes.push("payload accepted by downstream contract");
    }
    if (edgeKind === "calls_api" || String(node?.kind || "") === "api_call") {
      tagState.add("outbound_request");
      tagState.add("egress");
      notes.push("outbound boundary reached");
    }
    if (edgeKind === "targets_endpoint" || String(node?.kind || "") === "endpoint") {
      tagState.add("endpoint_boundary");
      notes.push("endpoint boundary transition");
    }
    if (String(node?.kind || "") === "function") {
      tagState.add("code_execution");
    }
    const sanitizerHit = (trace.sanitizer_hits || []).find((token) => {
      const normalized = String(token || "").toLowerCase();
      return normalized.length > 0 && (lowerLabel.includes(normalized) || String(node?.id || "").toLowerCase().includes(normalized));
    });
    if (sanitizerHit) {
      tagState.delete("secret");
      tagState.delete("pii");
      tagState.add("sanitized");
      notes.push(`sanitizer signal: ${sanitizerHit}`);
    }

    const afterTags = Array.from(tagState.values()).sort();
    const noteText = notes.length > 0
      ? notes.join("; ")
      : describeGuideEdgeTransition(edgeKind);
    return {
      index,
      nodeId: String(node?.id || ""),
      nodeKind: String(node?.kind || "unknown"),
      nodeLabel: label,
      edgeKind,
      beforeTags,
      afterTags,
      note: truncateNodeLabel(noteText, GUIDE_STEP_NOTE_TRUNCATE),
    };
  });
  return steps;
}

function buildGuideTraceOptions(view, direction) {
  const downstream = Array.isArray(view?.downstream) ? view.downstream : [];
  const upstream = Array.isArray(view?.upstream) ? view.upstream : [];
  const options = [];
  const normalizedDirection = normalizeGuideTraceDirection(direction);
  const includeDownstream = normalizedDirection === "downstream" || normalizedDirection === "both";
  const includeUpstream = normalizedDirection === "upstream" || normalizedDirection === "both";

  const pushTrace = (trace, traceDirection, localIndex, globalTraceIndex) => {
    if (!trace || !Array.isArray(trace.nodes) || trace.nodes.length === 0) return;
    const focusNode = traceDirection === "upstream"
      ? trace.nodes[0]
      : trace.nodes[trace.nodes.length - 1];
    const focusLabel = String(focusNode?.label || focusNode?.id || "unknown");
    const confidenceLabel = String(trace.confidence_label || "unknown");
    const hops = Math.max(0, trace.nodes.length - 1);
    const directionPrefix = traceDirection === "upstream" ? "Up" : "Down";
    const traceLabel =
      `${directionPrefix} #${localIndex + 1} • ${truncateNodeLabel(focusLabel, 56)} ` +
      `• hops:${hops} • conf:${confidenceLabel}`;
    options.push({
      key: `${traceDirection}:${localIndex}`,
      direction: traceDirection,
      localIndex,
      globalTraceIndex,
      label: traceLabel,
      focusLabel,
      hops,
      confidenceLabel,
      trace,
    });
  };

  if (includeDownstream) {
    downstream.forEach((trace, index) => {
      pushTrace(trace, "downstream", index, index);
    });
  }
  if (includeUpstream) {
    upstream.forEach((trace, index) => {
      pushTrace(trace, "upstream", index, downstream.length + index);
    });
  }
  return options;
}

function buildGuideRunbookMarkdown(model) {
  if (!model) {
    return "# Auto Runbook\n\nNo guide model available.\n";
  }
  const mermaidSource = buildGuideTraceMermaid(model);
  const lines = [
    `# Auto Runbook - ${String(model.sourceLabel || model.sourceId || "unknown source")}`,
    "",
    `- Generated: ${new Date().toISOString()}`,
    `- Source ID: \`${String(model.sourceId || "")}\``,
    `- Method/Route: \`${String(model.sourceMethod || "")} ${String(model.sourceRoute || "")}\``,
    `- Trace set: ${model.traceDirection}`,
    `- Selected trace: ${model.selectedTrace ? model.selectedTrace.label : "n/a"}`,
    `- Flow hops: ${String(clampFlowHops(state.flowMaxHops))}`,
    `- Trace confidence summary: high=${model.traceConfidenceSummary.high}, medium=${model.traceConfidenceSummary.medium}, low=${model.traceConfidenceSummary.low}`,
    "",
    "## Diagram (Mermaid)",
    "```mermaid",
    mermaidSource,
    "```",
    "",
    "## Guided Explain",
  ];

  if (!model.selectedTrace) {
    lines.push("- No trace available for current filters.");
  } else {
    model.stepModels.forEach((step) => {
      lines.push(
        `${step.index + 1}. [${step.nodeKind}] ${step.nodeLabel} (edge: ${step.edgeKind})`,
        `   - ${step.note}`,
        `   - tags: ${step.beforeTags.join(", ") || "none"} -> ${step.afterTags.join(", ") || "none"}`
      );
    });
  }

  lines.push("", "## Payload Evolution");
  lines.push(`- Input tags: ${model.payloadTags.length > 0 ? model.payloadTags.join(", ") : "none"}`);
  if (model.selectedTrace) {
    const score = Number(model.selectedTrace.trace.confidence_score || 0);
    lines.push(
      `- Trace confidence: ${String(model.selectedTrace.trace.confidence_label || "unknown")} (${score.toFixed(3)})`
    );
    const sanitizerHits = Array.isArray(model.selectedTrace.trace.sanitizer_hits)
      ? model.selectedTrace.trace.sanitizer_hits
      : [];
    lines.push(`- Sanitizer indicators: ${sanitizerHits.length > 0 ? sanitizerHits.join(", ") : "none"}`);
  }

  lines.push("", "## Dependencies");
  lines.push(`- Touched crates: ${model.touchedCrateCount} (${model.touchedCrates.join(", ") || "none"})`);
  lines.push(`- Upstream dependency heads: ${model.dependencies.upstreamHeads.length}`);
  model.dependencies.upstreamHeads.slice(0, 16).forEach((node) => {
    lines.push(`  - [${node.kind}] ${node.label}`);
  });
  lines.push(`- Downstream sinks: ${model.dependencies.downstreamSinks.length}`);
  model.dependencies.downstreamSinks.slice(0, 16).forEach((node) => {
    const targetHint = node.target ? ` target=${node.target}` : "";
    const routeHint = node.route ? ` route=${node.route}` : "";
    lines.push(`  - [${node.kind}] ${node.label}${targetHint}${routeHint}`);
  });

  lines.push("", "## Failure Modes");
  model.failureModes.forEach((failure) => {
    lines.push(`- [${failure.severity}] ${failure.mode}: ${failure.detail}`);
  });
  lines.push("", "## Findings");
  if (model.findings.length === 0) {
    lines.push("- None");
  } else {
    model.findings.slice(0, 20).forEach((finding) => {
      lines.push(`- [${String(finding.severity || "info")}] ${String(finding.message || "").trim()}`);
    });
  }

  lines.push("", "## Rollback Hints");
  model.rollbackHints.forEach((hint) => {
    lines.push(`- ${hint}`);
  });

  lines.push("", "## Owners (Inferred)");
  if (model.owners.length === 0) {
    lines.push("- No owners inferred from current trace data.");
  } else {
    model.owners.forEach((owner) => {
      const docsPart = Array.isArray(owner.docs) && owner.docs.length > 0
        ? ` docs=${owner.docs.join(", ")}`
        : "";
      lines.push(`- ${owner.owner} (${owner.reason}; crate=${owner.crate || "n/a"}${docsPart})`);
    });
  }

  lines.push(
    "",
    "## Regeneration",
    "- Re-run `Flow -> Run Flow` (or `Run Delta vs Base`) after code changes.",
    "- Rebuild this runbook in the `Guide` tab and compare diffs."
  );
  return `${lines.join("\n")}\n`;
}

function buildGuideModel() {
  const result = state.flowLastResult;
  const view = state.flowLastView;
  if (!result || !view) return null;
  const traceDirection = normalizeGuideTraceDirection(state.guideTraceDirection);
  const traceOptions = buildGuideTraceOptions(view, traceDirection);
  const selectedTrace = traceOptions.find((option) => option.key === state.guideSelectedTraceKey)
    || traceOptions[0]
    || null;
  state.guideSelectedTraceKey = selectedTrace ? selectedTrace.key : "";

  const allTraces = [...(view.downstream || []), ...(view.upstream || [])];
  const allNodes = collectGuideTraceNodes(allTraces);
  const touchedCrates = Array.from(
    new Set(
      allNodes
        .map((node) => String(node.crate || "").trim())
        .filter((crate) => crate.length > 0)
    )
  ).slice(0, 16);
  const findings = Array.isArray(result.findings) ? result.findings : [];
  const downstreamSinkNodes = (view.downstream || [])
    .map((trace) => {
      const nodes = Array.isArray(trace?.nodes) ? trace.nodes : [];
      return nodes.length > 0 ? nodes[nodes.length - 1] : null;
    })
    .filter(Boolean);
  const downstreamSinkLabels = Array.from(
    new Set(
      downstreamSinkNodes.map((node) => String(node.label || node.id || "unknown"))
    )
  );
  const downstreamApiCount = downstreamSinkNodes
    .filter((node) => String(node?.kind || "") === "api_call")
    .length;
  const traceConfidenceSummary = { high: 0, medium: 0, low: 0 };
  traceOptions.forEach((option) => {
    const label = String(option?.trace?.confidence_label || "unknown");
    if (label === "high" || label === "medium" || label === "low") {
      traceConfidenceSummary[label] += 1;
    }
  });
  const lowConfidenceTraceCount = traceConfidenceSummary.low;
  const crossCrateTraceCount = traceOptions
    .filter((option) => flowTraceCrateCount(option.trace) > 1)
    .length;
  const dependencies = buildGuideDependencies(view);
  const owners = inferGuideOwners(allNodes, result.source || {}, state.guideOwnerRuleIndex);
  const payloadTags = Array.isArray(result.tags)
    ? result.tags.map((tag) => String(tag || "").trim()).filter((tag) => tag.length > 0)
    : [];

  const model = {
    sourceId: String(result?.source?.id || state.flowSourceId || ""),
    sourceLabel: String(result?.source?.label || result?.source?.id || state.flowSourceId || "unknown"),
    sourceMethod: String(result?.source?.method || ""),
    sourceRoute: String(result?.source?.route || ""),
    traceDirection,
    traceOptions,
    selectedTrace,
    findings,
    highFindingCount: findings.filter((finding) => String(finding?.severity || "") === "high").length,
    mediumFindingCount: findings.filter((finding) => String(finding?.severity || "") === "medium").length,
    touchedCrateCount: touchedCrates.length,
    touchedCrates,
    touchedKindSummary: summarizeGuideKinds(allNodes),
    downstreamSinkLabels,
    downstreamApiCount,
    payloadTags,
    traceConfidenceSummary,
    lowConfidenceTraceCount,
    crossCrateTraceCount,
    dependencies,
    owners,
    rollbackHints: [],
    failureModes: [],
    stepModels: [],
    savePath: "",
    runbookMarkdown: "",
  };
  model.stepModels = buildGuideStepModels(model);
  model.failureModes = buildGuideFailureModes(model);
  model.rollbackHints = buildGuideRollbackHints(model);
  const slugBase = String(
    [model.sourceMethod, model.sourceRoute]
      .filter((value) => String(value || "").trim().length > 0)
      .join("-")
    || model.sourceId
    || "source"
  )
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 48) || "source";
  const slugSuffix = shortStableHexDigest(model.sourceId || `${model.sourceMethod}:${model.sourceRoute}`);
  const slug = `${slugBase}-${slugSuffix}`;
  model.savePath = `docs/codegraph/runbooks/runbook-${slug}.md`;
  model.runbookMarkdown = buildGuideRunbookMarkdown(model);
  return model;
}

function renderGuidePanel(model) {
  const directionSelect = el("guide-trace-direction");
  const traceSelect = el("guide-trace-select");
  const summary = el("guide-summary");
  const steps = el("guide-steps");
  const payload = el("guide-payload");
  const runbookOutput = el("guide-runbook-output");
  if (!directionSelect || !traceSelect || !summary || !steps || !payload || !runbookOutput) {
    return;
  }

  directionSelect.value = normalizeGuideTraceDirection(state.guideTraceDirection);
  state.guideLastModel = model;

  traceSelect.innerHTML = "";
  const traceOptions = Array.isArray(model?.traceOptions) ? model.traceOptions : [];
  if (traceOptions.length === 0) {
    const option = document.createElement("option");
    option.value = "";
    option.textContent = "No traces available";
    traceSelect.appendChild(option);
    traceSelect.value = "";
  } else {
    traceOptions.forEach((optionData) => {
      const option = document.createElement("option");
      option.value = optionData.key;
      option.textContent = optionData.label;
      traceSelect.appendChild(option);
    });
    const hasSelected = traceOptions.some((option) => option.key === state.guideSelectedTraceKey);
    if (!hasSelected) {
      state.guideSelectedTraceKey = traceOptions[0].key;
    }
    traceSelect.value = state.guideSelectedTraceKey;
  }

  if (!model) {
    summary.innerHTML = `<div class="guide-muted">No guide model yet. Run flow simulation or click Build Guide.</div>`;
    steps.innerHTML = `<div class="guide-muted">Build guide to render step cards.</div>`;
    payload.innerHTML = `<div class="guide-muted">Payload evolution appears after guide build.</div>`;
    runbookOutput.textContent = "Runbook markdown appears here after guide build.";
    setGuideStatus("Guide idle. Run flow simulation or click Build Guide.", "muted");
    syncGuideControls();
    return;
  }

  summary.innerHTML = `
    <div class="guide-summary-grid">
      <div class="guide-metric">
        <span class="guide-metric-label">Source</span>
        <span class="guide-metric-value">${escapeHtml(truncateNodeLabel(model.sourceLabel, 90))}</span>
      </div>
      <div class="guide-metric">
        <span class="guide-metric-label">Trace Set</span>
        <span class="guide-metric-value">${escapeHtml(model.traceDirection)} (${model.traceOptions.length})</span>
      </div>
      <div class="guide-metric">
        <span class="guide-metric-label">Risk Findings</span>
        <span class="guide-metric-value">high:${model.highFindingCount} medium:${model.mediumFindingCount}</span>
      </div>
      <div class="guide-metric">
        <span class="guide-metric-label">Trace Confidence</span>
        <span class="guide-metric-value">high:${model.traceConfidenceSummary.high} medium:${model.traceConfidenceSummary.medium} low:${model.traceConfidenceSummary.low}</span>
      </div>
      <div class="guide-metric">
        <span class="guide-metric-label">Touched Crates</span>
        <span class="guide-metric-value">${model.touchedCrateCount} (${escapeHtml(model.touchedCrates.join(", ") || "none")})</span>
      </div>
      <div class="guide-metric">
        <span class="guide-metric-label">Downstream Sinks</span>
        <span class="guide-metric-value">${model.downstreamSinkLabels.length} (api:${model.downstreamApiCount})</span>
      </div>
      <div class="guide-metric">
        <span class="guide-metric-label">Kinds</span>
        <span class="guide-metric-value">${escapeHtml(model.touchedKindSummary)}</span>
      </div>
      <div class="guide-metric">
        <span class="guide-metric-label">Cross-Crate Traces</span>
        <span class="guide-metric-value">${model.crossCrateTraceCount}</span>
      </div>
      <div class="guide-metric">
        <span class="guide-metric-label">Runbook Path</span>
        <span class="guide-metric-value">${escapeHtml(model.savePath)}</span>
      </div>
    </div>
  `;

  if (!model.selectedTrace) {
    steps.innerHTML = `<div class="guide-muted">No trace available for current direction/filter set.</div>`;
  } else {
    const selected = model.selectedTrace;
    steps.innerHTML = selected.trace.nodes.map((node, index) => {
      const stepModel = model.stepModels[index] || null;
      const edgeKind = stepModel ? stepModel.edgeKind : (
        index === 0
          ? "source"
          : String(selected.trace.edge_kinds[index - 1] || "calls")
      );
      const kind = String(node?.kind || "unknown");
      const safeKind = kind.replace(/[^a-zA-Z0-9_-]/g, "-");
      const nodeId = String(node?.id || "");
      const label = truncateNodeLabel(String(node?.label || nodeId || "unknown"), 88);
      const note = stepModel ? stepModel.note : describeGuideEdgeTransition(edgeKind);
      return `
        <button
          type="button"
          class="guide-step-card"
          data-guide-node-id="${escapeHtml(nodeId)}"
          data-guide-trace-index="${String(selected.globalTraceIndex)}"
          data-guide-step-index="${String(index)}"
          title="${escapeHtml(kind)}"
        >
          <span class="guide-step-head">
            <span class="guide-step-index">Step ${index + 1}</span>
            <span class="guide-step-label">${escapeHtml(label)}</span>
            <span class="guide-step-note">${escapeHtml(note)}</span>
          </span>
          <span class="guide-step-meta">
            <span class="guide-badge guide-badge-kind-${escapeHtml(safeKind)}">${escapeHtml(kind)}</span>
            <span class="guide-badge">${escapeHtml(edgeKind)}</span>
          </span>
        </button>
      `;
    }).join("\n");
  }

  const tagChips = model.payloadTags.length > 0
    ? model.payloadTags.map((tag) => `<span class="guide-chip">${escapeHtml(tag)}</span>`).join("")
    : `<span class="guide-muted">none</span>`;
  const selectedTrace = model.selectedTrace;
  const sanitizerHits = selectedTrace && Array.isArray(selectedTrace.trace.sanitizer_hits)
    ? selectedTrace.trace.sanitizer_hits
    : [];
  const sanitizerChips = sanitizerHits.length > 0
    ? sanitizerHits.map((token) => `<span class="guide-chip">${escapeHtml(String(token || ""))}</span>`).join("")
    : `<span class="guide-muted">none</span>`;
  payload.innerHTML = `
    <div class="guide-payload-row"><strong>Input tags:</strong> ${tagChips}</div>
    <div class="guide-payload-row">
      <strong>Trace confidence:</strong>
      ${selectedTrace
        ? `<span class="guide-chip">${escapeHtml(String(selectedTrace.trace.confidence_label || "unknown"))}</span>`
        : `<span class="guide-muted">n/a</span>`}
    </div>
    <div class="guide-payload-row"><strong>Sanitizer indicators:</strong> ${sanitizerChips}</div>
    <div class="guide-payload-row"><strong>Upstream dependency heads:</strong> ${escapeHtml(model.dependencies.upstreamHeads.slice(0, 8).map((node) => node.label).join(", ") || "none")}</div>
    <div class="guide-payload-row"><strong>Downstream sink heads:</strong> ${escapeHtml(model.dependencies.downstreamSinks.slice(0, 8).map((node) => node.label).join(", ") || "none")}</div>
    <div class="guide-payload-row"><strong>Step evolution:</strong> ${escapeHtml(model.stepModels.map((step) => `${step.index + 1}:${step.afterTags.join("|") || "none"}`).join(" ; ") || "none")}</div>
  `;

  runbookOutput.textContent = model.runbookMarkdown;
  setGuideStatus(
    model.selectedTrace
      ? `Guide ready: ${model.selectedTrace.label}`
      : "Guide ready, but no trace matched current direction filters.",
    model.selectedTrace ? "ok" : "warn"
  );
  syncGuideControls();
}

function syncGuideControls() {
  const sourceSelect = el("guide-source-select");
  const directionSelect = el("guide-trace-direction");
  const traceSelect = el("guide-trace-select");
  const runSourceButton = el("guide-run-source");
  const buildButton = el("guide-build");
  const playButton = el("guide-play-trace");
  const openFlowButton = el("guide-open-flow");
  const exportButton = el("guide-export-md");
  const copyButton = el("guide-copy-md");
  const saveButton = el("guide-save-md");
  const autoRebuildToggle = el("guide-auto-rebuild");
  const hasSource = String(state.flowSourceId || "").trim().length > 0;
  const busy = state.flowRunning || state.flowDeltaRunning;
  const model = state.guideLastModel;
  const hasTrace = Boolean(model?.selectedTrace);
  const hasRunbook = Boolean(model?.runbookMarkdown && model.runbookMarkdown.trim().length > 0);

  if (sourceSelect) {
    const selectedId = String(state.flowSourceId || "").trim();
    sourceSelect.innerHTML = "";
    if (state.flowSources.length === 0) {
      const option = document.createElement("option");
      option.value = "";
      option.textContent = state.flowSourcesLoading ? "Loading sources..." : "No sources";
      sourceSelect.appendChild(option);
      sourceSelect.value = "";
      state.guideSelectedSourceId = "";
    } else {
      state.flowSources.forEach((source) => {
        const option = document.createElement("option");
        option.value = source.id;
        const routePart = source.route ? source.route : source.id;
        const methodPart = source.method ? `${source.method} ` : "";
        option.textContent = `${methodPart}${routePart}`;
        sourceSelect.appendChild(option);
      });
      const hasSelected = state.flowSources.some((source) => source.id === selectedId);
      const finalId = hasSelected ? selectedId : state.flowSources[0].id;
      sourceSelect.value = finalId;
      state.guideSelectedSourceId = finalId;
      if (!selectedId) {
        state.flowSourceId = finalId;
      }
    }
    sourceSelect.disabled = busy || state.flowSourcesLoading;
  }
  if (directionSelect) {
    directionSelect.value = normalizeGuideTraceDirection(state.guideTraceDirection);
    directionSelect.disabled = busy;
  }
  if (traceSelect) {
    traceSelect.disabled = busy || !model || (model.traceOptions || []).length === 0;
  }
  if (runSourceButton) {
    runSourceButton.disabled = busy || !hasSource || state.flowSourcesLoading;
    runSourceButton.textContent = busy ? "Running..." : "Run Source";
  }
  if (buildButton) {
    buildButton.disabled = busy || !hasSource;
    buildButton.textContent = busy ? "Running..." : "Build Guide";
  }
  if (playButton) {
    playButton.disabled = busy || !hasTrace;
  }
  if (openFlowButton) {
    openFlowButton.disabled = false;
  }
  if (exportButton) {
    exportButton.disabled = busy || !hasRunbook;
  }
  if (copyButton) {
    copyButton.disabled = busy || !hasRunbook;
    if (copyButton.textContent.trim().length === 0) {
      copyButton.textContent = "Copy Runbook";
    }
  }
  if (saveButton) {
    saveButton.disabled = busy || !hasRunbook;
  }
  if (autoRebuildToggle) {
    autoRebuildToggle.checked = shouldGuideAutoRebuild();
    autoRebuildToggle.disabled = busy;
  }
}

function refreshGuideFromFlowState(options = {}) {
  if (options.resetSelection === true) {
    state.guideSelectedTraceKey = "";
  }
  const model = buildGuideModel();
  renderGuidePanel(model);
  if (model && options.markFresh === true) {
    setGuideArtifactsDirty(false);
  }
  return model;
}

async function runGuideBuild(options = {}) {
  const openGuideTab = options.openGuideTab !== false;
  await ensureGuideOwnerRuleIndexLoaded();
  if (!state.flowLastResult || !state.flowLastView) {
    setGuideStatus("No flow result yet; running flow simulation first...", "muted");
    const ok = await runFlowSimulation();
    if (!ok) {
      setGuideStatus("Guide build failed because flow simulation failed.", "error");
      syncGuideControls();
      return false;
    }
  }
  const model = refreshGuideFromFlowState({ markFresh: true });
  if (!model) {
    setGuideStatus("Guide build failed: no flow traces available.", "warn");
    return false;
  }
  if (openGuideTab) {
    setDetailsPanelTab("guide", {
      ensureOpen: true,
      persist: true,
      resizeGraph: false,
    });
  }
  setGuideStatus(`Guide rebuilt for ${model.sourceLabel}.`, "ok");
  return true;
}

async function runGuideForSelectedSource(options = {}) {
  const sourceId = String(state.guideSelectedSourceId || state.flowSourceId || "").trim();
  if (!sourceId) {
    setGuideStatus("Choose a source first.", "warn");
    return false;
  }
  if (String(state.flowSourceId || "") !== sourceId) {
    setFlowSource(sourceId, { setPayload: false, ensureOption: true });
  }
  setGuideStatus("Running flow simulation for selected source...", "muted");
  const flowOk = await runFlowSimulation();
  if (!flowOk) {
    setGuideStatus("Guide run failed because flow simulation failed.", "error");
    return false;
  }
  return runGuideBuild({
    openGuideTab: options.openGuideTab !== false,
  });
}

function playGuideTrace(options = {}) {
  const model = state.guideLastModel || buildGuideModel();
  if (!model || !model.selectedTrace || !state.flowLastView) {
    setGuideStatus("Build guide first to play a trace.", "warn");
    return false;
  }
  const stepIndex = Number.isFinite(options.stepIndex)
    ? Math.max(0, Math.floor(options.stepIndex))
    : 0;
  startFlowPlayback(state.flowLastView, String(state.flowSourceId || model.sourceId || ""), {
    traceIndex: model.selectedTrace.globalTraceIndex,
    stepIndex,
    singleTrace: true,
  });
  setGuideStatus(`Playing ${model.selectedTrace.label} from step ${stepIndex + 1}.`, "ok");
  return true;
}

function focusGuideNodeInGraph(nodeId, traceIndex, stepIndex = 0) {
  if (!state.cy) return false;
  const normalizedNodeId = String(nodeId || "").trim();
  if (!normalizedNodeId) return false;
  const node = state.cy.$id(normalizedNodeId);
  if (!node || node.empty()) return false;

  state.selectedNodeId = normalizedNodeId;
  node.select();
  setDetails(node);
  syncBlastControls();
  writeUrlViewState();
  const focusElements = node.closedNeighborhood().union(node);
  const padding = currentFitPaddingProfile();
  state.cy.animate({
    fit: { eles: focusElements, padding: padding.selectedNeighborhood },
    duration: 240,
    easing: "ease-out-cubic",
  });
  scheduleViewportCulling(state.cy);

  if (Number.isFinite(traceIndex) && state.flowLastView) {
    startFlowPlayback(state.flowLastView, String(state.flowSourceId || ""), {
      traceIndex: Math.max(0, Math.floor(traceIndex)),
      stepIndex: Math.max(0, Math.floor(stepIndex)),
      singleTrace: true,
    });
  }
  return true;
}

function exportGuideRunbookMarkdown() {
  const model = state.guideLastModel;
  if (!model || !model.runbookMarkdown) {
    setGuideStatus("Build guide first to export runbook.", "warn");
    return false;
  }
  const filename = String(model.savePath || "docs/codegraph/runbooks/runbook-source.md")
    .split("/")
    .pop() || "runbook-source.md";
  downloadTextFile(
    filename,
    model.runbookMarkdown,
    "text/markdown;charset=utf-8"
  );
  setGuideStatus("Runbook markdown exported.", "ok");
  return true;
}

async function copyGuideRunbookMarkdown() {
  const model = state.guideLastModel;
  const button = el("guide-copy-md");
  if (!model || !model.runbookMarkdown) {
    setGuideStatus("Build guide first to copy runbook.", "warn");
    return false;
  }
  try {
    await navigator.clipboard.writeText(model.runbookMarkdown);
    if (button) {
      button.textContent = "Copied";
      window.setTimeout(() => {
        if (button) {
          button.textContent = "Copy Runbook";
        }
      }, 900);
    }
    setGuideStatus("Runbook markdown copied to clipboard.", "ok");
    return true;
  } catch (_err) {
    if (button) {
      button.textContent = "Copy failed";
      window.setTimeout(() => {
        if (button) {
          button.textContent = "Copy Runbook";
        }
      }, 1200);
    }
    setGuideStatus("Clipboard copy failed in this browser.", "error");
    return false;
  }
}

async function saveGuideRunbookInRepo() {
  const model = state.guideLastModel;
  if (!model || !model.runbookMarkdown) {
    setGuideStatus("Build guide first to save runbook.", "warn");
    return false;
  }
  const savePath = String(model.savePath || "").trim();
  if (!savePath) {
    setGuideStatus("Runbook save path missing.", "error");
    return false;
  }
  try {
    const response = await fetch("/api/file/write", {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
      },
      body: JSON.stringify({
        path: savePath,
        content: model.runbookMarkdown,
      }),
    });
    const payload = await response.json().catch(() => ({}));
    if (!response.ok) {
      const message = typeof payload.message === "string"
        ? payload.message
        : `Save failed (HTTP ${response.status})`;
      throw new Error(message);
    }
    setGuideStatus(`Runbook saved to ${String(payload.path || savePath)}.`, "ok");
    return true;
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    setGuideStatus(`Runbook save failed: ${message}`, "error");
    return false;
  }
}

async function fetchGuideArtifactSignature() {
  const parts = [];
  for (const path of GUIDE_ARTIFACT_WATCH_PATHS) {
    try {
      const response = await fetch(`/api/file/stat?path=${encodeURIComponent(path)}`, {
        cache: "no-store",
      });
      if (!response.ok) {
        parts.push(`${path}:missing`);
        continue;
      }
      const payload = await response.json().catch(() => ({}));
      const mtimeMs = Number(payload.mtime_ms);
      const sizeBytes = Number(payload.size_bytes);
      const mtimePart = Number.isFinite(mtimeMs) ? String(Math.round(mtimeMs)) : "0";
      const sizePart = Number.isFinite(sizeBytes) ? String(Math.round(sizeBytes)) : "0";
      parts.push(`${path}:${mtimePart}:${sizePart}`);
    } catch (_err) {
      parts.push(`${path}:error`);
    }
  }
  return parts.join("|");
}

async function pollGuideArtifacts() {
  if (state.guideWatchBusy) return;
  state.guideWatchBusy = true;
  try {
    const signature = await fetchGuideArtifactSignature();
    if (!signature) return;
    let signatureChanged = false;
    if (!state.guideArtifactSignature) {
      state.guideArtifactSignature = signature;
      setGuideArtifactsDirty(false);
      return;
    }
    if (signature !== state.guideArtifactSignature) {
      state.guideArtifactSignature = signature;
      signatureChanged = true;
      setGuideArtifactsDirty(true);
    }

    if (signatureChanged && !shouldGuideAutoRebuild()) {
      setGuideStatus("Codegraph artifacts changed. Guide is stale; click Build Guide.", "warn");
      return;
    }
    if (!state.guideArtifactsDirty || !shouldGuideAutoRebuild()) return;
    if (!state.flowSourceId) return;
    if (state.flowRunning || state.flowDeltaRunning) return;

    const activeTab = normalizeDetailsTab(state.detailsActiveTab);
    if (activeTab !== "guide" && activeTab !== "flow") {
      if (signatureChanged) {
        setGuideStatus("Codegraph artifacts changed. Auto-rebuild pending until Guide/Flow tab is active.", "warn");
      }
      return;
    }

    setGuideStatus(
      signatureChanged
        ? "Codegraph artifacts changed. Auto rebuilding flow + runbook..."
        : "Auto rebuilding stale guide + runbook...",
      "muted"
    );
    const ok = await runFlowSimulation();
    if (ok) {
      await runGuideBuild({ openGuideTab: false });
      setGuideArtifactsDirty(false);
    }
  } finally {
    state.guideWatchBusy = false;
    syncGuideControls();
  }
}

function ensureGuideArtifactWatcher() {
  if (state.guideArtifactPollHandle !== null) return;
  state.guideArtifactPollHandle = window.setInterval(() => {
    void pollGuideArtifacts();
  }, GUIDE_ARTIFACT_WATCH_INTERVAL_MS);
  void pollGuideArtifacts();
}

function setFlowTimelineMessage(message) {
  const timeline = el("flow-timeline");
  if (!timeline) return;
  timeline.innerHTML = `<div class="flow-timeline-muted">${escapeHtml(message)}</div>`;
}

function clearFlowPlaybackRowHighlight() {
  const timeline = el("flow-timeline");
  if (!timeline) return;
  timeline.querySelectorAll(".flow-trace-row-active").forEach((row) => {
    row.classList.remove("flow-trace-row-active");
  });
}

function setFlowPlaybackRowHighlight(traceIndex) {
  const timeline = el("flow-timeline");
  if (!timeline) return;
  clearFlowPlaybackRowHighlight();
  if (!Number.isFinite(traceIndex)) return;
  const row = timeline.querySelector(`[data-flow-trace-index="${String(Math.max(0, Math.floor(traceIndex)))}"]`);
  if (row) {
    row.classList.add("flow-trace-row-active");
    row.scrollIntoView({ block: "nearest", behavior: "smooth" });
  }
}

function clearFlowPlaybackClasses(cy = state.cy) {
  if (!cy) return;
  cy.nodes().removeClass("flow-play-node flow-play-node-peak");
  cy.edges().removeClass("flow-play-edge flow-play-edge-peak");
}

function stopFlowPlayback() {
  if (state.flowPlaybackTimeoutHandle !== null) {
    window.clearTimeout(state.flowPlaybackTimeoutHandle);
    state.flowPlaybackTimeoutHandle = null;
  }
  if (state.flowPlaybackPeakTimeoutHandle !== null) {
    window.clearTimeout(state.flowPlaybackPeakTimeoutHandle);
    state.flowPlaybackPeakTimeoutHandle = null;
  }
  state.flowPlaybackSession = null;
  clearFlowPlaybackClasses();
  clearMermaidFlowPlaybackClasses();
  clearFlowPlaybackRowHighlight();
  updateLoadingControls();
}

function flowPlaybackDelayMs() {
  const revealDelay = stagedRevealDelayMs();
  if (revealDelay <= 0) {
    return FLOW_PLAYBACK_STEP_MIN_MS;
  }
  return Math.max(FLOW_PLAYBACK_STEP_MIN_MS, Math.min(FLOW_PLAYBACK_STEP_MAX_MS, revealDelay));
}

function buildFlowPlaybackQueue(view, options = {}) {
  if (!view) return [];
  const allTraces = [...(view.downstream || []), ...(view.upstream || [])].slice(0, FLOW_PLAYBACK_TRACE_LIMIT);
  if (allTraces.length === 0) return [];

  const requestedTraceIndex = Number.isFinite(options.traceIndex)
    ? Math.max(0, Math.floor(options.traceIndex))
    : null;
  const requestedStepIndex = Number.isFinite(options.stepIndex)
    ? Math.max(0, Math.floor(options.stepIndex))
    : 0;
  const singleTrace = options.singleTrace === true;

  const queue = [];
  allTraces.forEach((trace, traceIndex) => {
    if (requestedTraceIndex !== null && traceIndex !== requestedTraceIndex) {
      return;
    }
    const nodeIds = Array.isArray(trace?.node_ids) ? trace.node_ids : [];
    const startAt = requestedTraceIndex === traceIndex ? requestedStepIndex : 0;
    for (let idx = startAt; idx < nodeIds.length; idx += 1) {
      const nodeId = String(nodeIds[idx] || "").trim();
      if (!nodeId) continue;
      const prevId = idx > 0 ? String(nodeIds[idx - 1] || "").trim() : "";
      queue.push({
        traceIndex,
        stepIndex: idx,
        nodeId,
        fromId: prevId,
        toId: nodeId,
      });
    }
    if (singleTrace && requestedTraceIndex === traceIndex) {
      return;
    }
  });
  return queue;
}

function applyFlowPlaybackStep(cy, step) {
  if (!cy || !step) return;
  clearFlowPlaybackClasses(cy);
  setFlowPlaybackRowHighlight(step.traceIndex);

  const node = cy.$id(step.nodeId);
  if (node && !node.empty()) {
    node.addClass("flow-play-node");
  }

  if (step.fromId && step.toId) {
    let edge = cy.edges().filter((item) => item.source().id() === step.fromId && item.target().id() === step.toId);
    if (edge.length === 0) {
      edge = cy.edges().filter((item) => item.source().id() === step.toId && item.target().id() === step.fromId);
    }
    if (edge.length > 0) {
      edge.addClass("flow-play-edge");
    }
  }

  if (state.flowPlaybackSession) {
    state.flowPlaybackSession.currentStep = step;
  }
  applyMermaidFlowPlaybackStep(step, {
    peak: false,
    autoCenter: true,
  });

  if (state.flowPlaybackPeakTimeoutHandle !== null) {
    window.clearTimeout(state.flowPlaybackPeakTimeoutHandle);
    state.flowPlaybackPeakTimeoutHandle = null;
  }
  const peakDelay = Math.max(26, Math.round((state.flowPlaybackSession?.delayMs || FLOW_PLAYBACK_STEP_MIN_MS) * FLOW_PLAYBACK_PEAK_RATIO));
  state.flowPlaybackPeakTimeoutHandle = window.setTimeout(() => {
    if (!state.flowPlaybackSession || state.flowPlaybackSession.cy !== cy) return;
    node.addClass("flow-play-node-peak");
    cy.edges(".flow-play-edge").addClass("flow-play-edge-peak");
    applyMermaidFlowPlaybackStep(step, {
      peak: true,
      autoCenter: false,
    });
  }, peakDelay);
}

function scheduleFlowPlaybackTick(delayMs) {
  if (state.flowPlaybackTimeoutHandle !== null) {
    window.clearTimeout(state.flowPlaybackTimeoutHandle);
    state.flowPlaybackTimeoutHandle = null;
  }
  if (!state.flowPlaybackSession) return;
  const delay = Math.max(0, Number(delayMs) || 0);
  state.flowPlaybackTimeoutHandle = window.setTimeout(() => {
    state.flowPlaybackTimeoutHandle = null;
    runFlowPlaybackTick();
  }, delay);
}

function runFlowPlaybackTick() {
  const session = state.flowPlaybackSession;
  if (!session) return;
  const { cy, queue, delayMs } = session;
  if (!cy || state.cy !== cy) {
    stopFlowPlayback();
    return;
  }
  if (session.cursor >= queue.length) {
    stopFlowPlayback();
    return;
  }

  const step = queue[session.cursor];
  applyFlowPlaybackStep(cy, step);
  session.cursor += 1;
  if (session.cursor >= queue.length) {
    scheduleFlowPlaybackTick(Math.max(200, delayMs));
    return;
  }
  scheduleFlowPlaybackTick(delayMs);
}

function startFlowPlayback(view, sourceId = "", options = {}) {
  const cy = state.cy;
  if (!cy || !view) return;
  const queue = buildFlowPlaybackQueue(view, options);
  if (queue.length === 0) {
    stopFlowPlayback();
    return;
  }
  stopFlowPlayback();

  const sourceNodeId = String(sourceId || state.flowSourceId || "").trim();
  const delayMs = flowPlaybackDelayMs();
  state.flowPlaybackSession = {
    cy,
    sourceNodeId,
    queue,
    cursor: 0,
    delayMs,
    currentStep: null,
  };
  updateLoadingControls();
  scheduleFlowPlaybackTick(0);
}

function clearFlowGraphFocus() {
  state.flowGraphFocusActive = false;
  state.flowVisibleNodeIds = new Set();
  state.flowTraceNodeIds = new Set();
  state.flowVisibleTruncated = false;
  stopFlowPlayback();
}

function collectFlowTraceNodeIds(view, sourceId = "") {
  const traceNodeIds = new Set();
  const sourceNodeId = String(sourceId || state.flowSourceId || "").trim();
  if (sourceNodeId.length > 0) {
    traceNodeIds.add(sourceNodeId);
  }
  const traces = [...(view?.downstream || []), ...(view?.upstream || [])];
  traces.forEach((trace) => {
    const nodeIds = Array.isArray(trace?.node_ids) ? trace.node_ids : [];
    nodeIds.forEach((nodeId) => {
      const id = String(nodeId || "").trim();
      if (id.length > 0) {
        traceNodeIds.add(id);
      }
    });
  });
  return traceNodeIds;
}

function buildFlowGraphFocusContext(view, sourceId = "") {
  const traceNodeIds = collectFlowTraceNodeIds(view, sourceId);
  if (traceNodeIds.size === 0) {
    return null;
  }
  const visibleNodeIds = new Set(traceNodeIds);
  addAncestorParents(visibleNodeIds, FLOW_VISIBLE_NODE_CAP);
  return {
    traceNodeIds,
    visibleNodeIds,
    truncated: visibleNodeIds.size >= FLOW_VISIBLE_NODE_CAP,
  };
}

function applyFlowGraphFocus(view, sourceId = "") {
  const focusContext = buildFlowGraphFocusContext(view, sourceId);
  if (!focusContext) {
    clearFlowGraphFocus();
    return { rerendered: false, revealNodeIds: [], blockedByMode: false };
  }

  state.flowGraphFocusActive = true;
  state.flowTraceNodeIds = focusContext.traceNodeIds;
  state.flowVisibleNodeIds = focusContext.visibleNodeIds;
  state.flowVisibleTruncated = focusContext.truncated;

  const queryActive = state.searchQuery.trim().length > 0;
  if (state.impactActive || state.blastLensActive || queryActive) {
    return { rerendered: false, revealNodeIds: [], blockedByMode: true };
  }

  const previousVisible = state.cy
    ? new Set(state.cy.nodes().map((node) => node.id()))
    : new Set();
  const revealNodeIds = sortNodeIdsForReveal(
    Array.from(focusContext.visibleNodeIds).filter((nodeId) => !previousVisible.has(nodeId))
  );

  stopFlowPlayback();
  renderGraph("progressive", {
    preserveViewport: false,
    focusSearch: false,
    stagedRevealNodeIds: revealNodeIds,
    deferInsights: revealNodeIds.length > 0,
    onStagedRevealComplete: () => {
      startFlowPlayback(view, sourceId);
    },
  });
  if (revealNodeIds.length === 0) {
    startFlowPlayback(view, sourceId);
  }
  return { rerendered: true, revealNodeIds, blockedByMode: false };
}

function flowNodeCrate(node) {
  if (!node || typeof node !== "object") return "";
  const kind = String(node.kind || "");
  if (kind === "crate") {
    return String(node.label || node.id || "");
  }
  const crate = String(node.crate || "").trim();
  if (crate.length > 0) return crate;
  const module = String(node.module || "").trim();
  if (module.includes("::")) {
    return module.split("::")[0];
  }
  const path = String(node.path || "").trim();
  if (path.includes("/")) {
    return path.split("/", 1)[0];
  }
  return "";
}

function flowTraceCrateCount(trace) {
  const crateSet = new Set();
  const nodes = Array.isArray(trace?.nodes) ? trace.nodes : [];
  nodes.forEach((node) => {
    const crate = flowNodeCrate(node);
    if (crate) crateSet.add(crate);
  });
  return crateSet.size;
}

function shortStableHexDigest(input) {
  const text = String(input || "");
  let hash = 2166136261;
  for (let index = 0; index < text.length; index += 1) {
    hash ^= text.charCodeAt(index);
    hash = Math.imul(hash, 16777619);
  }
  return (hash >>> 0).toString(16).padStart(8, "0");
}

function sanitizeFlowTrace(trace, direction) {
  const nodeIds = Array.isArray(trace?.node_ids) ? trace.node_ids.map((value) => String(value || "")) : [];
  const nodes = Array.isArray(trace?.nodes) ? trace.nodes : [];
  if (nodeIds.length === 0 || nodes.length === 0) {
    return null;
  }

  let zipped = nodeIds.map((id, index) => ({
    id,
    node: nodes[index] || { id, label: id, kind: "unknown" },
  }));

  if (!state.flowIncludeContainers) {
    zipped = zipped.filter((entry, index) => {
      if (index === 0 || index === nodeIds.length - 1) return true;
      const kind = String(entry.node?.kind || "");
      return !CONTAINER_KINDS.has(kind);
    });
  }

  if (zipped.length === 0) return null;

  const deduped = [];
  zipped.forEach((entry) => {
    const previous = deduped[deduped.length - 1];
    if (previous && previous.id === entry.id) {
      return;
    }
    deduped.push(entry);
  });
  if (deduped.length === 0) return null;

  const traceView = {
    direction,
    node_ids: deduped.map((entry) => entry.id),
    nodes: deduped.map((entry) => entry.node),
    edge_kinds: Array.isArray(trace?.edge_kinds) ? trace.edge_kinds : [],
    depth: Number(trace?.depth || Math.max(0, deduped.length - 1)),
    confidence_score: Number(trace?.confidence_score || 0),
    confidence_label: String(trace?.confidence_label || "unknown"),
    sanitizer_hits: Array.isArray(trace?.sanitizer_hits)
      ? trace.sanitizer_hits.map((value) => String(value || "").trim()).filter((value) => value.length > 0)
      : [],
  };
  return traceView;
}

function buildFlowResultView(result) {
  const downstreamRaw = Array.isArray(result?.traces) ? result.traces : [];
  const upstreamRaw = Array.isArray(result?.upstream_traces) ? result.upstream_traces : [];

  const filterTrace = (trace, direction) => {
    const sanitized = sanitizeFlowTrace(trace, direction);
    if (!sanitized) return null;

    const sink = sanitized.nodes[sanitized.nodes.length - 1] || {};
    const sinkKind = String(sink.kind || "");
    if (direction === "downstream" && state.flowOnlyOutbound && sinkKind !== "api_call") {
      return null;
    }
    if (state.flowOnlyCrossCrate && flowTraceCrateCount(sanitized) < 2) {
      return null;
    }
    return sanitized;
  };

  const downstream = downstreamRaw
    .map((trace) => filterTrace(trace, "downstream"))
    .filter((trace) => trace !== null);
  const upstream = state.flowOnlyOutbound
    ? []
    : upstreamRaw
      .map((trace) => filterTrace(trace, "upstream"))
      .filter((trace) => trace !== null);

  return {
    downstream,
    upstream,
    totalDownstream: downstreamRaw.length,
    totalUpstream: upstreamRaw.length,
  };
}

function flowTraceToHtml(trace, index) {
  const directionLabel = trace.direction === "upstream" ? "Upstream" : "Downstream";
  const steps = trace.nodes.map((node, stepIndex) => {
    const kind = String(node?.kind || "unknown");
    const label = String(node?.label || node?.id || "unknown");
    const nodeId = String(node?.id || "");
    const stepClass = `flow-step-${kind.replace(/[^a-zA-Z0-9_-]/g, "-")}`;
    return `<button type="button" class="flow-step ${stepClass}" data-flow-node-id="${escapeHtml(nodeId)}" data-flow-trace-index="${String(index)}" data-flow-step-index="${String(stepIndex)}" title="${escapeHtml(kind)}">${escapeHtml(label)}</button>`;
  }).join('<span class="flow-step-arrow">→</span>');
  const crateCount = flowTraceCrateCount(trace);
  const crossCrateBadge = crateCount > 1
    ? `<span class="flow-trace-badge flow-trace-badge-cross">cross-crate:${crateCount}</span>`
    : "";
  return `
    <div class="flow-trace-row" data-flow-trace-index="${String(index)}">
      <div class="flow-trace-head">
        <span class="flow-trace-title">${directionLabel} #${index + 1}</span>
        <span class="flow-trace-badge">hops:${Math.max(0, trace.nodes.length - 1)}</span>
        ${crossCrateBadge}
      </div>
      <div class="flow-trace-steps">${steps}</div>
    </div>
  `;
}

function renderFlowTimeline(view) {
  const timeline = el("flow-timeline");
  if (!timeline) return;
  if (!view) {
    setFlowTimelineMessage("Run flow simulation to view interleaving timelines.");
    clearFlowPlaybackRowHighlight();
    return;
  }

  const traces = [...view.downstream, ...view.upstream];
  if (traces.length === 0) {
    timeline.innerHTML = `
      <div class="flow-timeline-muted">
        No traces matched current filters. Try disabling \`Only outbound\` / \`Only cross-crate\`.
      </div>
    `;
    clearFlowPlaybackRowHighlight();
    return;
  }

  const rows = traces.slice(0, 24).map((trace, index) => flowTraceToHtml(trace, index)).join("\n");
  timeline.innerHTML = rows;
  clearFlowPlaybackRowHighlight();
}

function bindFlowTimelineInteractions() {
  const timeline = el("flow-timeline");
  if (!timeline || timeline.dataset.bound === "1") return;
  timeline.dataset.bound = "1";
  timeline.addEventListener("click", (event) => {
    const target = event.target instanceof Element
      ? event.target.closest("[data-flow-node-id]")
      : null;
    if (!target) return;
    const nodeId = String(target.getAttribute("data-flow-node-id") || "").trim();
    if (!nodeId || !state.cy) return;

    const node = state.cy.$id(nodeId);
    if (!node || node.empty()) return;
    state.selectedNodeId = nodeId;
    node.select();
    setDetails(node);
    syncBlastControls();
    writeUrlViewState();
    const focusElements = node.closedNeighborhood().union(node);
    const padding = currentFitPaddingProfile();
    state.cy.animate({
      fit: { eles: focusElements, padding: padding.selectedNeighborhood },
      duration: 240,
      easing: "ease-out-cubic",
    });
    scheduleViewportCulling(state.cy);

    const traceIndex = Number(target.getAttribute("data-flow-trace-index"));
    const stepIndex = Number(target.getAttribute("data-flow-step-index"));
    if (Number.isFinite(traceIndex) && state.flowLastView) {
      startFlowPlayback(state.flowLastView, String(state.flowSourceId || ""), {
        traceIndex,
        stepIndex: Number.isFinite(stepIndex) ? stepIndex : 0,
        singleTrace: true,
      });
    }
  });
}

function flowBlastItemHtml(node) {
  const label = String(node?.label || node?.id || "unknown");
  const kind = String(node?.kind || "unknown");
  return `<div class="flow-blast-item" data-kind="${escapeHtml(kind)}" title="${escapeHtml(kind)}">${escapeHtml(label)}</div>`;
}

function renderFlowBlastRadius(view, result) {
  const upstreamEl = el("flow-blast-upstream");
  const centerEl = el("flow-blast-center");
  const downstreamEl = el("flow-blast-downstream");
  if (!upstreamEl || !centerEl || !downstreamEl) return;

  if (!view || !result) {
    upstreamEl.innerHTML = '<div class="flow-blast-muted">Run simulation to load upstream nodes.</div>';
    centerEl.innerHTML = '<div class="flow-blast-muted">No source selected.</div>';
    downstreamEl.innerHTML = '<div class="flow-blast-muted">Run simulation to load downstream sinks.</div>';
    return;
  }

  const source = result.source || {};
  centerEl.innerHTML = flowBlastItemHtml({
    id: source.id,
    label: source.label || source.id,
    kind: source.kind || "endpoint",
  });

  const upstreamMap = new Map();
  (view.upstream || []).forEach((trace) => {
    (trace.nodes || []).forEach((node, index) => {
      if (index === (trace.nodes || []).length - 1) return;
      const key = String(node?.id || "");
      if (!key || key === String(source.id || "")) return;
      if (!upstreamMap.has(key)) {
        upstreamMap.set(key, node);
      }
    });
  });

  const downstreamMap = new Map();
  (view.downstream || []).forEach((trace) => {
    const nodes = trace.nodes || [];
    if (nodes.length === 0) return;
    const sink = nodes[nodes.length - 1];
    const key = String(sink?.id || "");
    if (!key || key === String(source.id || "")) return;
    if (!downstreamMap.has(key)) {
      downstreamMap.set(key, sink);
    }
  });

  const upstreamItems = [...upstreamMap.values()].slice(0, 24);
  const downstreamItems = [...downstreamMap.values()].slice(0, 24);

  upstreamEl.innerHTML = upstreamItems.length > 0
    ? upstreamItems.map((node) => flowBlastItemHtml(node)).join("")
    : '<div class="flow-blast-muted">No upstream nodes in current view.</div>';
  downstreamEl.innerHTML = downstreamItems.length > 0
    ? downstreamItems.map((node) => flowBlastItemHtml(node)).join("")
    : '<div class="flow-blast-muted">No downstream sinks in current view.</div>';
}

function downloadTextFile(filename, content, mime = "text/plain;charset=utf-8") {
  const blob = new Blob([content], { type: mime });
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  link.download = filename;
  document.body.appendChild(link);
  link.click();
  link.remove();
  window.setTimeout(() => URL.revokeObjectURL(url), 1200);
}

function buildFlowMarkdownExport() {
  const result = state.flowLastResult;
  const view = state.flowLastView;
  if (!result || !view) {
    return "# Flow Simulation Export\n\nNo flow simulation result available.\n";
  }
  const source = result.source || {};
  const lines = [
    "# Flow Simulation Export",
    "",
    `- Source: \`${String(source.label || source.id || "")}\``,
    `- Source ID: \`${String(source.id || "")}\``,
    `- Max hops: ${String(result.max_hops || state.flowMaxHops || 6)}`,
    `- Downstream traces shown: ${String((view.downstream || []).length)}`,
    `- Upstream traces shown: ${String((view.upstream || []).length)}`,
    "",
    "## Findings",
  ];

  const findings = Array.isArray(result.findings) ? result.findings : [];
  if (findings.length === 0) {
    lines.push("- None");
  } else {
    findings.slice(0, 16).forEach((finding) => {
      lines.push(
        `- [${String(finding.severity || "info")}] ${String(finding.message || "").trim()}`
      );
    });
  }

  const writeTraces = (title, traces) => {
    lines.push("", `## ${title}`);
    if (!Array.isArray(traces) || traces.length === 0) {
      lines.push("- None");
      return;
    }
    traces.slice(0, 20).forEach((trace, index) => {
      const path = (trace.nodes || [])
        .map((node) => String(node?.label || node?.id || "unknown"))
        .join(" -> ");
      lines.push(`${index + 1}. ${path}`);
    });
  };

  writeTraces("Downstream Traces", view.downstream || []);
  writeTraces("Upstream Traces", view.upstream || []);

  if (state.flowLastDelta) {
    const delta = state.flowLastDelta.delta || {};
    const downstream = delta.downstream || {};
    const upstream = delta.upstream || {};
    lines.push(
      "",
      "## Delta Summary",
      `- Downstream: +${String(downstream.added_count || 0)} / -${String(downstream.removed_count || 0)}`,
      `- Upstream: +${String(upstream.added_count || 0)} / -${String(upstream.removed_count || 0)}`
    );
  }
  return `${lines.join("\n")}\n`;
}

function exportFlowJson() {
  if (!state.flowLastResult) {
    setFlowStatus("Run flow simulation before exporting.", "warn");
    return;
  }
  const payload = {
    generated_at: new Date().toISOString(),
    source_id: state.flowSourceId,
    filters: {
      max_hops: clampFlowHops(state.flowMaxHops),
      only_outbound: state.flowOnlyOutbound,
      only_cross_crate: state.flowOnlyCrossCrate,
      include_containers: state.flowIncludeContainers,
    },
    result: state.flowLastResult,
    view: state.flowLastView,
    delta: state.flowLastDelta,
  };
  downloadTextFile(
    "flow-simulation-export.json",
    `${JSON.stringify(payload, null, 2)}\n`,
    "application/json;charset=utf-8"
  );
  setFlowStatus("Flow JSON export downloaded.", "ok");
}

function exportFlowMarkdown() {
  downloadTextFile("flow-simulation-export.md", buildFlowMarkdownExport(), "text/markdown;charset=utf-8");
  setFlowStatus("Flow Markdown export downloaded.", "ok");
}

function fallbackFlowSourcesFromGraph() {
  if (!state.graph || !Array.isArray(state.graph.nodes)) {
    return [];
  }
  return state.graph.nodes
    .filter((node) => node.kind === "endpoint")
    .map((node) => ({
      id: String(node.id),
      label: String(node.label || node.id),
      method: String(node.method || ""),
      route: String(node.route || ""),
      handler: String(node.handler || ""),
      path: String(node.path || ""),
      line: Number(node.line || 0),
      has_mock_payload: false,
    }))
    .sort((a, b) => {
      const routeCmp = a.route.localeCompare(b.route);
      if (routeCmp !== 0) return routeCmp;
      return a.method.localeCompare(b.method);
    });
}

function syncFlowControls() {
  const sourceSelect = el("flow-source-select");
  const refreshButton = el("flow-refresh-sources");
  const mockButton = el("flow-load-mock");
  const runButton = el("flow-run");
  const runDeltaButton = el("flow-run-delta");
  const exportJsonButton = el("flow-export-json");
  const exportMdButton = el("flow-export-md");
  const deltaContext = el("flow-delta-context");
  const hopSelect = el("flow-max-hops");
  const outboundToggle = el("flow-only-outbound");
  const crossCrateToggle = el("flow-only-cross-crate");
  const includeContainersToggle = el("flow-include-containers");
  const output = el("flow-output");
  if (!sourceSelect) return;

  const previousValue = state.flowSourceId || sourceSelect.value || "";
  sourceSelect.innerHTML = "";

  if (state.flowSources.length === 0) {
    const option = document.createElement("option");
    option.value = "";
    option.textContent = state.flowSourcesLoading ? "Loading sources..." : "No endpoint sources";
    sourceSelect.appendChild(option);
    sourceSelect.value = "";
  } else {
    state.flowSources.forEach((source) => {
      const option = document.createElement("option");
      option.value = source.id;
      const routePart = source.route ? `${source.route}` : source.id;
      const methodPart = source.method ? `${source.method} ` : "";
      option.textContent = `${methodPart}${routePart}`;
      sourceSelect.appendChild(option);
    });
    const hasPrevious = state.flowSources.some((source) => source.id === previousValue);
    const selectedValue = hasPrevious ? previousValue : state.flowSources[0].id;
    sourceSelect.value = selectedValue;
    state.flowSourceId = selectedValue;
  }

  const hasSource = state.flowSourceId.length > 0;
  if (refreshButton) {
    refreshButton.disabled = state.flowRunning;
  }
  if (mockButton) {
    mockButton.disabled = !hasSource || state.flowRunning || state.flowSourcesLoading;
  }
  if (runButton) {
    runButton.disabled = !hasSource || state.flowRunning || state.flowSourcesLoading;
    runButton.textContent = state.flowRunning ? "Running..." : "Run Flow";
  }
  if (runDeltaButton) {
    runDeltaButton.disabled = !hasSource || state.flowRunning || state.flowDeltaRunning || state.flowSourcesLoading;
    runDeltaButton.textContent = state.flowDeltaRunning ? "Diffing..." : "Run Delta vs Base";
  }
  if (exportJsonButton) {
    exportJsonButton.disabled = !state.flowLastResult || state.flowRunning || state.flowDeltaRunning;
  }
  if (exportMdButton) {
    exportMdButton.disabled = !state.flowLastResult || state.flowRunning || state.flowDeltaRunning;
  }
  if (deltaContext) {
    const baseRef = String(state.impactBaseRef || "").trim() || "n/a";
    const mode = normalizeImpactCompareMode(state.impactCompareMode);
    deltaContext.textContent = `Delta uses Impact Base=${baseRef}, Compare=${mode}.`;
  }
  if (hopSelect) {
    hopSelect.disabled = state.flowRunning || state.flowDeltaRunning;
    hopSelect.value = String(clampFlowHops(state.flowMaxHops));
  }
  if (outboundToggle) {
    outboundToggle.checked = state.flowOnlyOutbound;
    outboundToggle.disabled = state.flowRunning || state.flowDeltaRunning;
  }
  if (crossCrateToggle) {
    crossCrateToggle.checked = state.flowOnlyCrossCrate;
    crossCrateToggle.disabled = state.flowRunning || state.flowDeltaRunning;
  }
  if (includeContainersToggle) {
    includeContainersToggle.checked = state.flowIncludeContainers;
    includeContainersToggle.disabled = state.flowRunning || state.flowDeltaRunning;
  }
  if (output && state.flowLastResult === null && !output.textContent) {
    output.textContent = "Run flow simulation to inspect traces and risks.";
  }
  syncGuideControls();
}

function setFlowSource(sourceId, options = {}) {
  const nextId = String(sourceId || "").trim();
  const previousId = state.flowSourceId;
  const hadFlowFocus = state.flowGraphFocusActive;
  const shouldRerenderDefaultAfterClear = () =>
    hadFlowFocus &&
    !state.impactActive &&
    !state.blastLensActive &&
    state.searchQuery.trim().length === 0;
  if (!nextId) {
    state.flowSourceId = "";
    state.guideSelectedSourceId = "";
    state.flowLastResult = null;
    state.flowLastView = null;
    state.flowLastDelta = null;
    state.guideSelectedTraceKey = "";
    state.guideLastModel = null;
    clearFlowGraphFocus();
    clearFlowHighlights();
    const output = el("flow-output");
    if (output) {
      output.textContent = "Run flow simulation to inspect traces and risks.";
    }
    const deltaOutput = el("flow-delta-output");
    if (deltaOutput) {
      deltaOutput.textContent = "Run delta compare to view added/removed traces and risk deltas.";
    }
    setFlowTimelineMessage("Run flow simulation to view interleaving timelines.");
    renderFlowBlastRadius(null, null);
    refreshGuideFromFlowState({ resetSelection: true });
    if (shouldRerenderDefaultAfterClear()) {
      renderGraph("progressive", {
        preserveViewport: false,
        focusSearch: false,
      });
    }
    syncFlowControls();
    writeUrlViewState();
    return;
  }

  if (options.ensureOption === true && !state.flowSources.some((source) => source.id === nextId)) {
    const node = state.graphIndex?.nodeById.get(nextId);
    if (node && node.kind === "endpoint") {
      state.flowSources = [
        ...state.flowSources,
        {
          id: nextId,
          label: String(node.label || node.id),
          method: String(node.method || ""),
          route: String(node.route || ""),
          handler: String(node.handler || ""),
          path: String(node.path || ""),
          line: Number(node.line || 0),
          has_mock_payload: false,
        },
      ];
      state.flowSources.sort((a, b) => {
        const routeCmp = String(a.route || "").localeCompare(String(b.route || ""));
        if (routeCmp !== 0) return routeCmp;
        return String(a.method || "").localeCompare(String(b.method || ""));
      });
    }
  }
  state.flowSourceId = nextId;
  state.guideSelectedSourceId = nextId;
  if (previousId && previousId !== nextId) {
    state.flowLastResult = null;
    state.flowLastView = null;
    state.flowLastDelta = null;
    state.guideSelectedTraceKey = "";
    state.guideLastModel = null;
    clearFlowGraphFocus();
    clearFlowHighlights();
    const output = el("flow-output");
    if (output) {
      output.textContent = "Run flow simulation to inspect traces and risks.";
    }
    const deltaOutput = el("flow-delta-output");
    if (deltaOutput) {
      deltaOutput.textContent = "Run delta compare to view added/removed traces and risk deltas.";
    }
    setFlowTimelineMessage("Run flow simulation to view interleaving timelines.");
    renderFlowBlastRadius(null, null);
    refreshGuideFromFlowState({ resetSelection: true });
    if (shouldRerenderDefaultAfterClear()) {
      renderGraph("progressive", {
        preserveViewport: false,
        focusSearch: false,
      });
    }
  }
  syncFlowControls();
  writeUrlViewState();
  if (options.setPayload === true) {
    void loadFlowMockPayload();
  }
}

function clearFlowHighlights(cy = state.cy) {
  if (cy) {
    cy.nodes().removeClass("flow-trace-source flow-trace-node flow-trace-sink");
    cy.edges().removeClass("flow-trace-edge");
    clearFlowPlaybackClasses(cy);
  }
  clearMermaidFlowPlaybackClasses();
  clearFlowPlaybackRowHighlight();
}

function applyFlowHighlights(view, sourceId = "") {
  const cy = state.cy;
  if (!cy) return;
  clearFlowHighlights(cy);
  if (!view) return;

  const traceList = [
    ...(Array.isArray(view.downstream) ? view.downstream : []),
    ...(Array.isArray(view.upstream) ? view.upstream : []),
  ];
  if (traceList.length === 0) return;

  const sourceNodeId = String(sourceId || state.flowSourceId || "");
  if (sourceNodeId) {
    const sourceNode = cy.$id(sourceNodeId);
    if (sourceNode && !sourceNode.empty()) {
      sourceNode.addClass("flow-trace-source");
    }
  }

  const sinkIds = new Set();
  traceList.slice(0, 16).forEach((trace) => {
    const nodeIds = Array.isArray(trace?.node_ids) ? trace.node_ids : [];
    if (nodeIds.length === 0) return;
    nodeIds.forEach((nodeId) => {
      const node = cy.$id(String(nodeId));
      if (node && !node.empty()) {
        node.addClass("flow-trace-node");
      }
    });
    const lastNodeId = String(nodeIds[nodeIds.length - 1] || "");
    if (lastNodeId) {
      sinkIds.add(lastNodeId);
    }
    for (let index = 0; index < nodeIds.length - 1; index += 1) {
      const from = String(nodeIds[index] || "");
      const to = String(nodeIds[index + 1] || "");
      if (!from || !to) continue;
      let edges = cy.edges().filter((edge) => edge.source().id() === from && edge.target().id() === to);
      if (edges.length === 0) {
        edges = cy.edges().filter((edge) => edge.source().id() === to && edge.target().id() === from);
      }
      if (edges.length > 0) {
        edges.addClass("flow-trace-edge");
      }
    }
  });

  sinkIds.forEach((sinkId) => {
    const sinkNode = cy.$id(sinkId);
    if (sinkNode && !sinkNode.empty()) {
      sinkNode.addClass("flow-trace-sink");
    }
  });
}

function applyFlowResultView(result) {
  const view = buildFlowResultView(result);
  state.flowLastView = view;
  renderFlowTimeline(view);
  renderFlowBlastRadius(view, result);
  refreshGuideFromFlowState();
  const sourceNodeId = String(result?.source?.id || state.flowSourceId || "");
  const focusResult = applyFlowGraphFocus(view, sourceNodeId);
  if (!focusResult.rerendered) {
    applyFlowHighlights(view, sourceNodeId);
    if (!focusResult.blockedByMode) {
      startFlowPlayback(view, sourceNodeId);
    }
    if (state.cy && !focusResult.blockedByMode) {
      const traceNodes = state.cy.nodes().filter((node) => state.flowTraceNodeIds.has(node.id()));
      if (traceNodes.length > 0) {
        const padding = currentFitPaddingProfile();
        state.cy.fit(traceNodes, padding.focused);
        scheduleViewportCulling(state.cy, { force: true });
      }
    }
  }
  return view;
}

function refreshFlowViewFromCurrentFilters() {
  if (!state.flowLastResult) {
    clearFlowGraphFocus();
    clearFlowHighlights();
    setFlowTimelineMessage("Run flow simulation to view interleaving timelines.");
    renderFlowBlastRadius(null, null);
    refreshGuideFromFlowState();
    writeUrlViewState();
    return;
  }
  applyFlowResultView(state.flowLastResult);
  writeUrlViewState();
}

async function loadFlowSources(options = {}) {
  state.flowSourcesLoading = true;
  syncFlowControls();
  const preserveSelection = options.preserveSelection !== false;
  const previousSourceId = preserveSelection ? state.flowSourceId : "";

  try {
    const response = await fetch("/api/flow/sources", {
      cache: "no-store",
    });
    if (!response.ok) {
      throw new Error(`Flow sources unavailable (HTTP ${response.status})`);
    }
    const payload = await response.json().catch(() => ({}));
    const sources = Array.isArray(payload.sources) ? payload.sources : [];
    state.flowSources = sources.map((source) => ({
      id: String(source.id || ""),
      label: String(source.label || source.id || ""),
      method: String(source.method || ""),
      route: String(source.route || ""),
      handler: String(source.handler || ""),
      path: String(source.path || ""),
      line: Number(source.line || 0),
      has_mock_payload: source.has_mock_payload === true,
    })).filter((source) => source.id.length > 0);
    if (state.flowSources.length === 0) {
      state.flowSources = fallbackFlowSourcesFromGraph();
    }
    if (previousSourceId && state.flowSources.some((source) => source.id === previousSourceId)) {
      state.flowSourceId = previousSourceId;
    } else if (!state.flowSourceId && state.flowSources.length > 0) {
      state.flowSourceId = state.flowSources[0].id;
    } else if (state.flowSourceId && !state.flowSources.some((source) => source.id === state.flowSourceId)) {
      state.flowSourceId = state.flowSources.length > 0 ? state.flowSources[0].id : "";
    }
    state.guideSelectedSourceId = state.flowSourceId;
    setFlowStatus(`Loaded ${state.flowSources.length} flow source endpoints.`, "ok");
  } catch (_err) {
    state.flowSources = fallbackFlowSourcesFromGraph();
    if (!state.flowSourceId && state.flowSources.length > 0) {
      state.flowSourceId = state.flowSources[0].id;
    }
    state.guideSelectedSourceId = state.flowSourceId;
    setFlowStatus(
      state.flowSources.length > 0
        ? "Flow API unavailable; using endpoint sources from graph artifact."
        : "Flow sources unavailable. Run `make graph-index` and refresh.",
      state.flowSources.length > 0 ? "warn" : "error"
    );
  } finally {
    state.flowSourcesLoading = false;
    syncFlowControls();
    writeUrlViewState();
  }
}

async function loadFlowMockPayload() {
  const sourceId = String(state.flowSourceId || "").trim();
  const payloadInput = el("flow-payload-input");
  if (!sourceId || !payloadInput) {
    setFlowStatus("Select a flow source first.", "warn");
    return false;
  }

  try {
    const response = await fetch(`/api/flow/mock?source_id=${encodeURIComponent(sourceId)}`, {
      cache: "no-store",
    });
    if (!response.ok) {
      throw new Error(`Mock payload unavailable (HTTP ${response.status})`);
    }
    const payload = await response.json().catch(() => ({}));
    const mockPayload = payload?.payload && typeof payload.payload === "object"
      ? payload.payload
      : { json: {}, query: {}, path: {} };
    payloadInput.value = JSON.stringify(mockPayload, null, 2);
    setFlowStatus("Mock payload loaded from payload profile.", "ok");
    return true;
  } catch (_err) {
    payloadInput.value = JSON.stringify({ json: {}, query: {}, path: {} }, null, 2);
    setFlowStatus("Mock payload unavailable; loaded empty payload template.", "warn");
    return false;
  }
}

async function runFlowSimulation() {
  const sourceId = String(state.flowSourceId || "").trim();
  const payloadInput = el("flow-payload-input");
  const output = el("flow-output");
  const deltaOutput = el("flow-delta-output");
  if (!sourceId || !payloadInput || !output) {
    setFlowStatus("Select a flow source first.", "warn");
    return false;
  }

  let payloadObject = {};
  const rawPayload = payloadInput.value.trim();
  if (rawPayload.length > 0) {
    try {
      payloadObject = JSON.parse(rawPayload);
    } catch (_err) {
      setFlowStatus("Payload must be valid JSON.", "error");
      return false;
    }
  }

  state.flowRunning = true;
  state.flowLastDelta = null;
  stopFlowPlayback();
  if (deltaOutput) {
    deltaOutput.textContent = "Run delta compare to view added/removed traces and risk deltas.";
  }
  syncFlowControls();
  try {
    const response = await fetch("/api/flow/simulate", {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
      },
      body: JSON.stringify({
        source_id: sourceId,
        payload: payloadObject,
        max_hops: clampFlowHops(state.flowMaxHops),
        max_traces: 24,
      }),
    });
    const payload = await response.json().catch(() => ({}));
    if (!response.ok) {
      const message = typeof payload.message === "string"
        ? payload.message
        : `Flow simulation failed (HTTP ${response.status})`;
      throw new Error(message);
    }

    const result = payload?.result || payload;
    state.flowLastResult = result;
    output.textContent = JSON.stringify(result, null, 2);
    const view = applyFlowResultView(result);
    setGuideArtifactsDirty(false);
    writeUrlViewState();

    const findings = Array.isArray(result.findings) ? result.findings : [];
    const highCount = findings.filter((finding) => finding?.severity === "high").length;
    const mediumCount = findings.filter((finding) => finding?.severity === "medium").length;
    const sinkCount = Number(result.reachable_sink_count || 0);
    const upstreamCount = Number(
      result.upstream_trace_count ||
      (Array.isArray(result.upstream_traces) ? result.upstream_traces.length : 0)
    );
    const filteredDownstreamCount = Array.isArray(view?.downstream) ? view.downstream.length : 0;
    const filteredUpstreamCount = Array.isArray(view?.upstream) ? view.upstream.length : 0;
    if (highCount > 0) {
      setFlowStatus(
        `Flow simulation complete: ${sinkCount} downstream sinks (${filteredDownstreamCount} shown), ${upstreamCount} upstream traces (${filteredUpstreamCount} shown), ${highCount} high-risk findings.`,
        "warn"
      );
    } else if (mediumCount > 0) {
      setFlowStatus(
        `Flow simulation complete: ${sinkCount} downstream sinks (${filteredDownstreamCount} shown), ${upstreamCount} upstream traces (${filteredUpstreamCount} shown), ${mediumCount} medium-risk findings.`,
        "warn"
      );
    } else {
      setFlowStatus(
        `Flow simulation complete: ${sinkCount} downstream sinks (${filteredDownstreamCount} shown), ${upstreamCount} upstream traces (${filteredUpstreamCount} shown), no high-risk findings.`,
        "ok"
      );
    }
    return true;
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    setFlowStatus(`Flow simulation failed: ${message}`, "error");
    return false;
  } finally {
    state.flowRunning = false;
    syncFlowControls();
  }
}

async function runFlowDelta() {
  const sourceId = String(state.flowSourceId || "").trim();
  const payloadInput = el("flow-payload-input");
  const output = el("flow-output");
  const deltaOutput = el("flow-delta-output");
  if (!sourceId || !payloadInput || !output || !deltaOutput) {
    setFlowStatus("Select a flow source first.", "warn");
    return false;
  }

  const baseRef = String(state.impactBaseRef || "").trim();
  if (!baseRef) {
    setFlowStatus("Set Impact Base first, then run delta.", "warn");
    return false;
  }

  const compareMode = normalizeImpactCompareMode(state.impactCompareMode);
  let payloadObject = {};
  const rawPayload = payloadInput.value.trim();
  if (rawPayload.length > 0) {
    try {
      payloadObject = JSON.parse(rawPayload);
    } catch (_err) {
      setFlowStatus("Payload must be valid JSON.", "error");
      return false;
    }
  }

  state.flowDeltaRunning = true;
  stopFlowPlayback();
  syncFlowControls();
  try {
    const response = await fetch("/api/flow/delta", {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
      },
      body: JSON.stringify({
        source_id: sourceId,
        payload: payloadObject,
        max_hops: clampFlowHops(state.flowMaxHops),
        max_traces: 24,
        base_ref: baseRef,
        compare_mode: compareMode,
      }),
    });
    const payload = await response.json().catch(() => ({}));
    if (!response.ok) {
      const message = typeof payload.message === "string"
        ? payload.message
        : `Flow delta failed (HTTP ${response.status})`;
      throw new Error(message);
    }

    const deltaPayload = payload || {};
    const currentResult = deltaPayload.current_result || null;
    state.flowLastDelta = deltaPayload;

    if (currentResult && typeof currentResult === "object") {
      state.flowLastResult = currentResult;
      output.textContent = JSON.stringify(currentResult, null, 2);
      applyFlowResultView(currentResult);
      setGuideArtifactsDirty(false);
    }

    deltaOutput.textContent = JSON.stringify(
      {
        base_ref: deltaPayload.base_ref || baseRef,
        compare_mode: deltaPayload.compare_mode || compareMode,
        current_commit: deltaPayload.current_commit || "",
        base_commit: deltaPayload.base_commit || "",
        delta: deltaPayload.delta || {},
      },
      null,
      2
    );

    const downstream = deltaPayload?.delta?.downstream || {};
    const upstream = deltaPayload?.delta?.upstream || {};
    const findingsDelta = deltaPayload?.delta?.findings?.delta || {};
    const highDelta = Number(findingsDelta.high || 0);
    const mediumDelta = Number(findingsDelta.medium || 0);
    const downstreamAdded = Number(downstream.added_count || 0);
    const downstreamRemoved = Number(downstream.removed_count || 0);
    const upstreamAdded = Number(upstream.added_count || 0);
    const upstreamRemoved = Number(upstream.removed_count || 0);

    const riskTone = highDelta > 0 || mediumDelta > 0 ? "warn" : "ok";
    setFlowStatus(
      `Flow delta complete: downstream +${downstreamAdded}/-${downstreamRemoved}, upstream +${upstreamAdded}/-${upstreamRemoved}, findings delta high=${highDelta}, medium=${mediumDelta}.`,
      riskTone
    );
    writeUrlViewState();
    return true;
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    setFlowStatus(`Flow delta failed: ${message}`, "error");
    return false;
  } finally {
    state.flowDeltaRunning = false;
    syncFlowControls();
  }
}

function inferMonacoLanguage(filePath) {
  const normalized = normalizeRepoRelativePath(filePath);
  const suffix = normalized.split(".").pop()?.toLowerCase() || "";
  switch (suffix) {
    case "rs":
      return "rust";
    case "ts":
      return "typescript";
    case "tsx":
      return "typescript";
    case "js":
      return "javascript";
    case "jsx":
      return "javascript";
    case "json":
      return "json";
    case "py":
      return "python";
    case "md":
      return "markdown";
    case "toml":
      return "toml";
    case "yaml":
    case "yml":
      return "yaml";
    case "html":
      return "html";
    case "css":
      return "css";
    default:
      return "plaintext";
  }
}

async function ensureMonacoEditor() {
  if (state.editorMonacoReady && state.editorMonacoInstance && state.editorMonacoModel) {
    return true;
  }
  const container = el("editor-container");
  if (!container) return false;

  if (typeof monaco === "undefined" || !monaco?.editor) {
    if (typeof require !== "function") {
      setEditorStatus("Monaco loader unavailable in this browser.", "warn");
      return false;
    }
    // Try cdnjs first, then jsdelivr — robustness against CDN outages
    // / network filters / corporate proxies. Each attempt logs to the
    // console so users can see WHICH endpoint failed.
    const cdnAttempts = [
      "https://cdnjs.cloudflare.com/ajax/libs/monaco-editor/0.52.2/min/vs",
      "https://cdn.jsdelivr.net/npm/monaco-editor@0.52.2/min/vs",
    ];
    let lastErr = null;
    for (const vsBase of cdnAttempts) {
      try {
        setEditorStatus(`Loading Monaco from ${new URL(vsBase).host}…`, "info");
        await new Promise((resolve, reject) => {
          require.config({ paths: { vs: vsBase } });
          require(["vs/editor/editor.main"], resolve, reject);
        });
        lastErr = null;
        break;
      } catch (err) {
        console.error("Monaco load failed from", vsBase, err);
        lastErr = err;
      }
    }
    if (lastErr) {
      const message = lastErr instanceof Error ? lastErr.message : String(lastErr);
      setEditorStatus(
        `Monaco failed to load (${message}). Check console + network access to cdnjs/jsdelivr.`,
        "error",
      );
      return false;
    }
  }

  if (typeof monaco === "undefined" || !monaco?.editor) {
    setEditorStatus("Monaco editor unavailable after loader initialization.", "error");
    return false;
  }

  container.innerHTML = "";
  state.editorMonacoModel = monaco.editor.createModel("", "plaintext");
  state.editorMonacoInstance = monaco.editor.create(container, {
    model: state.editorMonacoModel,
    language: "plaintext",
    minimap: { enabled: false },
    automaticLayout: true,
    theme: state.theme === "dark" ? "vs-dark" : "vs",
    smoothScrolling: true,
    scrollBeyondLastLine: false,
    wordWrap: "off",
    fontSize: 13,
  });
  state.editorMonacoModel.onDidChangeContent(() => {
    if (state.editorSuppressModelChange) return;
    state.editorDirty = true;
    state.editorExternalChangeDetected = false;
    scheduleEditorImpactComputation();
    syncEditorControls();
  });
  state.editorMonacoReady = true;
  syncEditorControls();
  return true;
}

function ensureEditorPolling() {
  if (state.editorPollHandle !== null) return;
  state.editorPollHandle = window.setInterval(() => {
    void pollEditorFileStat();
  }, EDITOR_POLL_INTERVAL_MS);
}

async function loadEditorFile(path, options = {}) {
  const requestedPath = normalizeRepoRelativePath(path);
  if (!requestedPath) {
    setEditorStatus("Enter a repository-relative file path to load.", "warn");
    return false;
  }
  const ready = await ensureMonacoEditor();
  if (!ready || !state.editorMonacoModel) {
    return false;
  }

  state.editorLoading = true;
  syncEditorControls();
  const url = `/api/file/read?path=${encodeURIComponent(requestedPath)}`;
  try {
    let response;
    try {
      response = await fetch(url, { cache: "no-store" });
    } catch (netErr) {
      // Network-level failure (server unreachable, CORS, browser
      // blocked). Surface a clear hint — the bare "Failed to fetch"
      // string isn't actionable for users.
      console.error("Editor file fetch failed at", url, netErr);
      throw new Error(
        `Network request to ${url} failed (${netErr && netErr.message ? netErr.message : netErr}). ` +
        "Is the dev server still running at this origin?"
      );
    }
    const payload = await response.json().catch(() => ({}));
    if (!response.ok) {
      const message = typeof payload.message === "string"
        ? payload.message
        : `File read failed (HTTP ${response.status})`;
      throw new Error(message);
    }

    const content = typeof payload.content === "string" ? payload.content : "";
    const loadedPath = normalizeRepoRelativePath(payload.path || requestedPath);
    const mtimeMs = Number(payload.mtime_ms);
    state.editorPath = loadedPath;
    state.editorMtimeMs = Number.isFinite(mtimeMs) ? Math.round(mtimeMs) : Date.now();
    state.editorBaseContent = content;
    state.editorDirty = false;
    state.editorExternalChangeDetected = false;

    const language = inferMonacoLanguage(loadedPath);
    monaco.editor.setModelLanguage(state.editorMonacoModel, language);
    state.editorSuppressModelChange = true;
    state.editorMonacoModel.setValue(content);
    state.editorSuppressModelChange = false;
    state.editorMonacoInstance.setPosition({ lineNumber: 1, column: 1 });
    state.editorMonacoInstance.revealLineInCenter(1);
    scheduleEditorImpactComputation();
    ensureEditorPolling();
    if (options.quiet !== true) {
      setEditorStatus(`Loaded ${loadedPath}`, "ok");
    }
    return true;
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    setEditorStatus(`Load failed: ${message}`, "error");
    return false;
  } finally {
    state.editorLoading = false;
    syncEditorControls();
  }
}

async function saveEditorFile() {
  const currentPath = normalizeRepoRelativePath(state.editorPath);
  if (!currentPath || !state.editorMonacoModel) {
    setEditorStatus("No editor file loaded.", "warn");
    return false;
  }

  state.editorSaving = true;
  syncEditorControls();
  try {
    const content = state.editorMonacoModel.getValue();
    const response = await fetch("/api/file/write", {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
      },
      body: JSON.stringify({
        path: currentPath,
        content,
      }),
    });
    const payload = await response.json().catch(() => ({}));
    if (!response.ok) {
      const message = typeof payload.message === "string"
        ? payload.message
        : `File save failed (HTTP ${response.status})`;
      throw new Error(message);
    }
    const nextPath = normalizeRepoRelativePath(payload.path || currentPath);
    const mtimeMs = Number(payload.mtime_ms);
    state.editorPath = nextPath;
    state.editorMtimeMs = Number.isFinite(mtimeMs) ? Math.round(mtimeMs) : Date.now();
    state.editorBaseContent = content;
    state.editorDirty = false;
    state.editorExternalChangeDetected = false;
    setEditorStatus(`Saved ${nextPath}`, "ok");
    scheduleEditorImpactComputation();
    return true;
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    setEditorStatus(`Save failed: ${message}`, "error");
    return false;
  } finally {
    state.editorSaving = false;
    syncEditorControls();
  }
}

async function pollEditorFileStat() {
  if (!state.editorPath || state.editorLoading || state.editorSaving) return;
  const autoRefreshEnabled = state.editorAutoRefresh;
  try {
    const response = await fetch(`/api/file/stat?path=${encodeURIComponent(state.editorPath)}`, {
      cache: "no-store",
    });
    if (!response.ok) return;
    const payload = await response.json().catch(() => ({}));
    const incomingMtime = Number(payload.mtime_ms);
    if (!Number.isFinite(incomingMtime)) return;
    if (incomingMtime <= state.editorMtimeMs) return;

    if (autoRefreshEnabled && !state.editorDirty) {
      await loadEditorFile(state.editorPath, { quiet: true });
      setEditorStatus(`File updated on disk. Reloaded ${state.editorPath}.`, "ok");
      return;
    }

    state.editorExternalChangeDetected = true;
    setEditorStatus(
      `File changed on disk (${state.editorPath}). Click Refresh to reload.`,
      "warn"
    );
    syncEditorControls();
  } catch (_err) {
    // ignore transient polling failures
  }
}

function extractIdentifierSet(text) {
  const source = String(text || "");
  const regex = /[A-Za-z_][A-Za-z0-9_]*/g;
  const result = new Set();
  let match = null;
  let guard = 0;
  while ((match = regex.exec(source)) !== null) {
    guard += 1;
    if (guard > 9000) break;
    const token = normalize(match[0]);
    if (token.length < 3 || token.length > 80) continue;
    result.add(token);
    if (result.size >= 2600) break;
  }
  return result;
}

function diffIdentifierSets(previousContent, nextContent, maxTokens = EDITOR_IMPACT_TOKEN_MAX) {
  const prev = extractIdentifierSet(previousContent);
  const next = extractIdentifierSet(nextContent);
  const changed = [];
  next.forEach((token) => {
    if (!prev.has(token)) {
      changed.push(token);
    }
  });
  prev.forEach((token) => {
    if (!next.has(token)) {
      changed.push(token);
    }
  });
  changed.sort();
  return changed.slice(0, Math.max(1, maxTokens));
}

function computeLiveImpactForEditorContent() {
  const index = state.graphIndex;
  if (!index || !state.editorMonacoModel) {
    clearLiveImpactState();
    return;
  }

  const path = normalizeRepoRelativePath(state.editorPath);
  const content = state.editorMonacoModel.getValue();
  const fileNodeId = lookupFileNodeIdForPath(path);
  const changedIdentifiers = diffIdentifierSets(state.editorBaseContent, content);
  const seedIds = new Set();
  if (fileNodeId) {
    seedIds.add(fileNodeId);
  }

  const symbolCandidates = new Set();
  for (const token of changedIdentifiers) {
    const ids = index.nodeIdsByNormalizedLabel.get(token) || [];
    for (const nodeId of ids) {
      const node = index.nodeById.get(nodeId);
      if (!node || !SYMBOL_KINDS.has(node.kind)) continue;
      const nodePath = normalizeRepoRelativePath(node.path);
      if (path && nodePath && nodePath !== path) continue;
      symbolCandidates.add(nodeId);
      if (symbolCandidates.size >= 36) break;
    }
    if (symbolCandidates.size >= 36) break;
  }

  symbolCandidates.forEach((id) => seedIds.add(id));
  if (seedIds.size === 0 && state.selectedNodeId) {
    seedIds.add(state.selectedNodeId);
  }

  if (seedIds.size === 0) {
    clearLiveImpactState();
    return;
  }

  const upstream = collectDirectedReachable(seedIds, "incoming", LIVE_IMPACT_HOP_COUNT, LIVE_IMPACT_NODE_CAP);
  const downstream = collectDirectedReachable(seedIds, "outgoing", LIVE_IMPACT_HOP_COUNT, LIVE_IMPACT_NODE_CAP);
  state.liveImpactSeedNodeIds = new Set(seedIds);
  state.liveImpactUpstreamIds = upstream.ids;
  state.liveImpactDownstreamIds = downstream.ids;
  state.liveImpactChangedSymbolCount = symbolCandidates.size;
  const summary = el("editor-impact-summary");
  if (summary) {
    summary.innerHTML =
      `<strong>Live structural impact:</strong> ` +
      `seeds ${seedIds.size} (${symbolCandidates.size} changed symbols) ` +
      `| upstream ${upstream.ids.size} | downstream ${downstream.ids.size}`;
  }
  if (state.cy) {
    applyLiveImpactClasses(state.cy);
  }
}

function scheduleEditorImpactComputation() {
  if (state.editorChangeDebounceHandle !== null) {
    window.clearTimeout(state.editorChangeDebounceHandle);
    state.editorChangeDebounceHandle = null;
  }
  state.editorChangeDebounceHandle = window.setTimeout(() => {
    state.editorChangeDebounceHandle = null;
    computeLiveImpactForEditorContent();
  }, EDITOR_CHANGE_DEBOUNCE_MS);
}

function suggestEditorPathFromNode(nodeData) {
  if (!nodeData) return;
  const nextPath = normalizeRepoRelativePath(nodeData.path);
  if (!nextPath) return;

  const input = el("editor-file-path");
  if (input && input.value.trim().length === 0) {
    input.value = nextPath;
  }
  if (nodeData.kind !== "file") return;
  if (state.editorDirty || state.editorLoading || state.editorSaving) return;
  if (normalizeRepoRelativePath(state.editorPath) === nextPath) return;
  void loadEditorFile(nextPath, { quiet: true });
}

function defaultMermaidPlaceholder() {
  return `<div class="search-insights-muted">Mermaid preview appears for active search/impact/flow results.</div>`;
}

function setInsightsTitle(mode = "search") {
  const title = el("search-insights-title");
  if (!title) return;
  if (mode === "flow") {
    title.textContent = "Flow Insights";
    return;
  }
  if (mode === "impact") {
    title.textContent = "Impact Insights";
    return;
  }
  if (mode === "focus") {
    title.textContent = "Focus Insights";
    return;
  }
  title.textContent = "Search Insights";
}

function clampMermaidZoom(value) {
  const parsed = Number(value);
  if (!Number.isFinite(parsed)) return 1;
  return Math.max(MERMAID_ZOOM_MIN, Math.min(MERMAID_ZOOM_MAX, parsed));
}

function syncMermaidZoomLabel() {
  const text = `${Math.round(state.mermaidZoom * 100)}%`;
  const splitLabel = el("mermaid-zoom-label");
  const modalLabel = el("mermaid-modal-zoom-label");
  if (splitLabel) splitLabel.textContent = text;
  if (modalLabel) modalLabel.textContent = text;
}

function syncMermaidSourceModeControl() {
  const select = el("mermaid-source-mode");
  if (!select) return;
  const mode = normalizeMermaidSourceMode(state.mermaidSourceMode);
  state.mermaidSourceMode = mode;
  select.value = mode;
}

function resolveGraphFocusElements(cy, options = {}) {
  if (!cy) {
    return {
      focusedNodes: null,
      focusedEdges: null,
    };
  }

  const ensureBaseClasses = options.ensureBaseClasses === true;
  // Tracked ids resolve the focused set directly — selector scans over
  // the whole graph cost hundreds of ms per call, and this runs on the
  // focus-flash interval.
  let focusedNodes;
  let focusedEdges;
  if (state.mermaidFocusedElementIds) {
    const nodes = [];
    const edges = [];
    for (const id of state.mermaidFocusedElementIds) {
      const el = cy.getElementById(id);
      if (el.empty()) continue;
      if (el.isNode()) nodes.push(el);
      else edges.push(el);
    }
    focusedNodes = nodes.length ? cy.collection(nodes) : cy.collection();
    focusedEdges = edges.length ? cy.collection(edges) : cy.collection();
  } else {
    focusedNodes = cy.nodes(".mermaid-focused-node");
    focusedEdges = cy.edges(".mermaid-focused-edge");
  }

  if (focusedEdges.length === 0 && state.mermaidFocusedEdge?.from && state.mermaidFocusedEdge?.to) {
    const { from, to } = state.mermaidFocusedEdge;
    const fromNode = cy.getElementById(from);
    const toNode = cy.getElementById(to);
    let edgeMatches =
      fromNode.nonempty() && toNode.nonempty() ? fromNode.edgesTo(toNode) : cy.collection();
    if (edgeMatches.length === 0 && fromNode.nonempty() && toNode.nonempty()) {
      edgeMatches = toNode.edgesTo(fromNode);
    }
    if (edgeMatches.length > 0) {
      if (ensureBaseClasses) {
        edgeMatches.addClass("mermaid-focused-edge");
      }
      focusedEdges = edgeMatches;
      const endpointNodes = edgeMatches.connectedNodes();
      if (ensureBaseClasses) {
        endpointNodes.addClass("mermaid-focused-node");
      }
      focusedNodes = focusedNodes.union(endpointNodes);
    }
  }

  if (focusedNodes.length === 0 && state.mermaidFocusedNodeId) {
    const focusedNode = cy.$id(state.mermaidFocusedNodeId);
    if (focusedNode && !focusedNode.empty()) {
      if (ensureBaseClasses) {
        focusedNode.addClass("mermaid-focused-node");
      }
      focusedNodes = focusedNodes.union(focusedNode);
    }
  }

  return {
    focusedNodes,
    focusedEdges,
  };
}

function clearGraphFocusFlash(cy = state.cy) {
  if (state.graphFocusFlashIntervalHandle !== null) {
    window.clearInterval(state.graphFocusFlashIntervalHandle);
    state.graphFocusFlashIntervalHandle = null;
  }
  if (state.graphFocusFlashTimeoutHandle !== null) {
    window.clearTimeout(state.graphFocusFlashTimeoutHandle);
    state.graphFocusFlashTimeoutHandle = null;
  }
  if (!cy) return;
  if (state.mermaidFocusedElementIds) {
    const nodes = [];
    const edges = [];
    for (const id of state.mermaidFocusedElementIds) {
      const el = cy.getElementById(id);
      if (el.empty()) continue;
      if (el.isNode()) nodes.push(el);
      else edges.push(el);
    }
    if (nodes.length) {
      cy.collection(nodes).removeClass("graph-focus-flash-node graph-focus-flash-node-peak");
    }
    if (edges.length) {
      cy.collection(edges).removeClass("graph-focus-flash-edge graph-focus-flash-edge-peak");
    }
    return;
  }
  cy.nodes().removeClass("graph-focus-flash-node graph-focus-flash-node-peak");
  cy.edges().removeClass("graph-focus-flash-edge graph-focus-flash-edge-peak");
}

function setGraphFocusFlashPhase(cy, peakPhase) {
  if (!cy) return;
  const { focusedNodes, focusedEdges } = resolveGraphFocusElements(cy, {
    ensureBaseClasses: true,
  });
  if (!focusedNodes || !focusedEdges) return;
  focusedNodes.addClass("graph-focus-flash-node");
  focusedEdges.addClass("graph-focus-flash-edge");
  if (peakPhase) {
    focusedNodes.addClass("graph-focus-flash-node-peak");
    focusedEdges.addClass("graph-focus-flash-edge-peak");
  } else {
    focusedNodes.removeClass("graph-focus-flash-node-peak");
    focusedEdges.removeClass("graph-focus-flash-edge-peak");
  }
}

function flashGraphMermaidFocus(cy = state.cy) {
  if (!cy) return;
  clearGraphFocusFlash(cy);
  const { focusedNodes, focusedEdges } = resolveGraphFocusElements(cy, {
    ensureBaseClasses: true,
  });
  if (!focusedNodes || !focusedEdges) return;
  if (focusedNodes.length === 0 && focusedEdges.length === 0) return;
  let peakPhase = true;
  setGraphFocusFlashPhase(cy, peakPhase);
  state.graphFocusFlashIntervalHandle = window.setInterval(() => {
    if (state.cy !== cy) {
      clearGraphFocusFlash(cy);
      return;
    }
    peakPhase = !peakPhase;
    setGraphFocusFlashPhase(cy, peakPhase);
  }, FOCUS_FLASH_PULSE_MS);
  state.graphFocusFlashTimeoutHandle = window.setTimeout(() => {
    if (state.cy !== cy) return;
    clearGraphFocusFlash(cy);
  }, FOCUS_FLASH_DURATION_MS);
}

function clearGraphMermaidFocus(cy = state.cy) {
  clearGraphFocusFlash(cy);
  if (!cy) return;
  // Track the elements we focused so clearing is O(focused) instead of a
  // full-collection class sweep (O(graph) per click at 100k+ elements).
  if (state.mermaidFocusedElementIds) {
    const nodes = [];
    const edges = [];
    for (const id of state.mermaidFocusedElementIds) {
      const el = cy.getElementById(id);
      if (el.empty()) continue;
      if (el.isNode()) nodes.push(el);
      else edges.push(el);
    }
    if (nodes.length) cy.collection(nodes).removeClass("mermaid-focused-node");
    if (edges.length) cy.collection(edges).removeClass("mermaid-focused-edge");
    state.mermaidFocusedElementIds = null;
    return;
  }
  cy.nodes().removeClass("mermaid-focused-node");
  cy.edges().removeClass("mermaid-focused-edge");
}

function applyGraphMermaidFocus(options = {}) {
  const cy = state.cy;
  if (!cy) return false;

  const zoomGraph = options.zoomGraph === true;
  const selectNode = options.selectNode === true;
  clearGraphMermaidFocus(cy);
  const trackFocused = (collection) => {
    const ids = [];
    collection.forEach((el) => ids.push(el.id()));
    state.mermaidFocusedElementIds = ids;
  };

  let focusElements = cy.collection();
  if (state.mermaidFocusedEdge?.from && state.mermaidFocusedEdge?.to) {
    const { from, to } = state.mermaidFocusedEdge;
    // Degree-proportional lookup: filtering ALL edges through accessor
    // calls cost seconds on the fully-loaded graph.
    const fromNode = cy.getElementById(from);
    const toNode = cy.getElementById(to);
    let focusedEdges = fromNode.nonempty() && toNode.nonempty()
      ? fromNode.edgesTo(toNode)
      : cy.collection();
    if (focusedEdges.length === 0 && fromNode.nonempty() && toNode.nonempty()) {
      focusedEdges = toNode.edgesTo(fromNode);
    }
    if (focusedEdges.length > 0) {
      focusedEdges.addClass("mermaid-focused-edge");
      const endpointNodes = focusedEdges.connectedNodes();
      endpointNodes.addClass("mermaid-focused-node");
      focusElements = endpointNodes.union(focusedEdges);
      trackFocused(focusElements);
    }
  }

  if (focusElements.length === 0 && state.mermaidFocusedNodeId) {
    const focusedNode = cy.$id(state.mermaidFocusedNodeId);
    if (focusedNode && !focusedNode.empty()) {
      focusedNode.addClass("mermaid-focused-node");
      // Incident edges only (degree-proportional). closedNeighborhood()
      // on a container node built collections over its entire subtree —
      // the click hang on the fully-loaded graph.
      const incidentEdges = focusedNode.connectedEdges();
      focusElements = focusedNode.union(incidentEdges);
      trackFocused(focusElements);
      if (selectNode) {
        state.selectedNodeId = focusedNode.id();
        focusedNode.select();
        setDetails(focusedNode);
        // Surface the details pane so the click has a visible effect
        // (was previously silent unless the user had already opened
        // the panel and switched to the details tab).
        setDetailsPanelTab("details", { ensureOpen: true });
        writeUrlViewState();
      }
    } else if (selectNode) {
      // Mermaid click landed on a node not present in the current cy
      // view (subgraph / progressive filtering). Show details directly
      // from the raw graph index and surface the details pane so the
      // click does something visible instead of silently no-op'ing.
      if (setDetailsFromGraphNodeId(state.mermaidFocusedNodeId)) {
        state.selectedNodeId = state.mermaidFocusedNodeId;
        setDetailsPanelTab("details", { ensureOpen: true });
        writeUrlViewState();
      }
    }
  }

  if (zoomGraph && focusElements.length > 0) {
    const currentZoom = cy.zoom();
    const targetZoom = Math.max(
      GRAPH_MERMAID_FOCUS_MIN_ZOOM,
      Math.min(GRAPH_MERMAID_FOCUS_MAX_ZOOM, currentZoom)
    );
    cy.animate({
      center: {
        eles: focusElements,
      },
      zoom: targetZoom,
      duration: 280,
      easing: "ease-out-cubic",
    });
  }

  return focusElements.length > 0;
}

function extractMermaidAliasToken(value) {
  const raw = String(value || "").trim();
  if (!raw) return null;
  const directMatch = raw.match(/\bn\d+\b/i);
  if (directMatch) return directMatch[0].toLowerCase();
  const segmentedMatch = raw.match(/(?:^|[-_])(n\d+)(?:[-_]|$)/i);
  if (segmentedMatch) return segmentedMatch[1].toLowerCase();
  return null;
}

function extractMermaidNodeAlias(nodeGroup) {
  if (!nodeGroup) return null;
  const datasetAlias = extractMermaidAliasToken(nodeGroup.dataset.mermaidNodeAlias || "");
  if (datasetAlias) return datasetAlias;

  const dataIdAlias = extractMermaidAliasToken(nodeGroup.getAttribute("data-id") || "");
  if (dataIdAlias) return dataIdAlias;

  const idAlias = extractMermaidAliasToken(nodeGroup.id || "");
  if (idAlias) return idAlias;

  for (const className of Array.from(nodeGroup.classList || [])) {
    const classAlias = extractMermaidAliasToken(className);
    if (classAlias) return classAlias;
  }
  return null;
}

function extractMermaidEdgeAliases(edgeGroup) {
  if (!edgeGroup) return null;
  const classes = Array.from(edgeGroup.classList || []);
  const sourceClass = classes.find((name) => name.startsWith("LS-"));
  const targetClass = classes.find((name) => name.startsWith("LE-"));
  if (!sourceClass || !targetClass) return null;
  const fromAlias = extractMermaidAliasToken(sourceClass.slice(3));
  const toAlias = extractMermaidAliasToken(targetClass.slice(3));
  if (!fromAlias || !toAlias) return null;
  return { fromAlias, toAlias };
}

function decorateMermaidContainer(container) {
  if (!container) return;
  const svg = container.querySelector("svg");
  if (!svg) return;

  svg.querySelectorAll(".mermaid-click-target").forEach((element) => {
    element.classList.remove("mermaid-click-target", "mermaid-click-target-node", "mermaid-click-target-edge");
    delete element.dataset.mermaidNodeAlias;
    delete element.dataset.mermaidGraphNodeId;
    delete element.dataset.mermaidEdgeFrom;
    delete element.dataset.mermaidEdgeTo;
  });

  svg.querySelectorAll("g.node").forEach((nodeGroup) => {
    const alias = extractMermaidNodeAlias(nodeGroup);
    if (!alias) return;
    const graphNodeId = state.mermaidGraphNodeIdByAlias.get(alias);
    if (!graphNodeId) return;
    nodeGroup.dataset.mermaidNodeAlias = alias;
    nodeGroup.dataset.mermaidGraphNodeId = graphNodeId;
    nodeGroup.classList.add("mermaid-click-target", "mermaid-click-target-node");
  });

  svg.querySelectorAll("g.edgePath, g.edgeLabel").forEach((edgeGroup) => {
    const aliases = extractMermaidEdgeAliases(edgeGroup);
    if (!aliases) return;
    const fromNodeId = state.mermaidGraphNodeIdByAlias.get(aliases.fromAlias);
    const toNodeId = state.mermaidGraphNodeIdByAlias.get(aliases.toAlias);
    if (!fromNodeId || !toNodeId) return;
    edgeGroup.dataset.mermaidEdgeFrom = fromNodeId;
    edgeGroup.dataset.mermaidEdgeTo = toNodeId;
    edgeGroup.classList.add("mermaid-click-target", "mermaid-click-target-edge");
  });
}

function applyMermaidFocusInContainer(container) {
  if (!container) return;
  const svg = container.querySelector("svg");
  if (!svg) return;

  svg.querySelectorAll(".mermaid-focused-node").forEach((element) => {
    element.classList.remove("mermaid-focused-node");
  });
  svg.querySelectorAll(".mermaid-focused-edge").forEach((element) => {
    element.classList.remove("mermaid-focused-edge");
  });
  svg.querySelectorAll(".mermaid-focus-flash-node").forEach((element) => {
    element.classList.remove("mermaid-focus-flash-node");
  });
  svg.querySelectorAll(".mermaid-focus-flash-edge").forEach((element) => {
    element.classList.remove("mermaid-focus-flash-edge");
  });

  if (state.mermaidFocusedEdge?.from && state.mermaidFocusedEdge?.to) {
    const fromId = state.mermaidFocusedEdge.from;
    const toId = state.mermaidFocusedEdge.to;
    svg.querySelectorAll("g.edgePath[data-mermaid-edge-from], g.edgeLabel[data-mermaid-edge-from]").forEach((edgeGroup) => {
      if (
        edgeGroup.dataset.mermaidEdgeFrom === fromId &&
        edgeGroup.dataset.mermaidEdgeTo === toId
      ) {
        edgeGroup.classList.add("mermaid-focused-edge");
      }
    });
    svg.querySelectorAll("g.node[data-mermaid-graph-node-id]").forEach((nodeGroup) => {
      const graphNodeId = nodeGroup.dataset.mermaidGraphNodeId;
      if (graphNodeId === fromId || graphNodeId === toId) {
        nodeGroup.classList.add("mermaid-focused-node");
      }
    });
    return;
  }

  if (!state.mermaidFocusedNodeId) return;
  svg.querySelectorAll("g.node[data-mermaid-graph-node-id]").forEach((nodeGroup) => {
    if (nodeGroup.dataset.mermaidGraphNodeId === state.mermaidFocusedNodeId) {
      nodeGroup.classList.add("mermaid-focused-node");
    }
  });
}

function clearMermaidFlowTraceClasses() {
  for (const containerId of MERMAID_RENDER_CONTAINER_IDS) {
    const container = el(containerId);
    if (!container) continue;
    const svg = container.querySelector("svg");
    if (!svg) continue;
    svg.querySelectorAll(".mermaid-flow-trace-source").forEach((element) => {
      element.classList.remove("mermaid-flow-trace-source");
    });
    svg.querySelectorAll(".mermaid-flow-trace-node").forEach((element) => {
      element.classList.remove("mermaid-flow-trace-node");
    });
    svg.querySelectorAll(".mermaid-flow-trace-sink").forEach((element) => {
      element.classList.remove("mermaid-flow-trace-sink");
    });
    svg.querySelectorAll(".mermaid-flow-trace-edge").forEach((element) => {
      element.classList.remove("mermaid-flow-trace-edge");
    });
  }
}

function applyMermaidFlowTraceClasses() {
  clearMermaidFlowTraceClasses();
  if (state.activeInsightsMode !== "flow") return;
  const view = state.flowLastView;
  if (!view) return;

  const sourceId = String(state.flowLastResult?.source?.id || state.flowSourceId || "").trim();
  const traceNodeIds = new Set();
  const sinkNodeIds = new Set();
  const traceEdges = new Set();

  const traces = [...(view.downstream || []), ...(view.upstream || [])];
  traces.forEach((trace) => {
    const nodeIds = Array.isArray(trace?.node_ids) ? trace.node_ids : [];
    nodeIds.forEach((nodeId) => {
      const id = String(nodeId || "").trim();
      if (id.length > 0) {
        traceNodeIds.add(id);
      }
    });
    const lastNodeId = String(nodeIds[nodeIds.length - 1] || "").trim();
    if (lastNodeId.length > 0) {
      sinkNodeIds.add(lastNodeId);
    }
    for (let index = 0; index < nodeIds.length - 1; index += 1) {
      const from = String(nodeIds[index] || "").trim();
      const to = String(nodeIds[index + 1] || "").trim();
      if (!from || !to) continue;
      traceEdges.add(`${from}>>>${to}`);
    }
  });

  for (const containerId of MERMAID_RENDER_CONTAINER_IDS) {
    const container = el(containerId);
    if (!container) continue;
    const svg = container.querySelector("svg");
    if (!svg) continue;

    svg.querySelectorAll("g.node[data-mermaid-graph-node-id]").forEach((nodeGroup) => {
      const nodeId = String(nodeGroup.dataset.mermaidGraphNodeId || "").trim();
      if (!nodeId) return;
      if (traceNodeIds.has(nodeId)) {
        nodeGroup.classList.add("mermaid-flow-trace-node");
      }
      if (sinkNodeIds.has(nodeId)) {
        nodeGroup.classList.add("mermaid-flow-trace-sink");
      }
      if (sourceId.length > 0 && nodeId === sourceId) {
        nodeGroup.classList.add("mermaid-flow-trace-source");
      }
    });

    svg.querySelectorAll("g.edgePath[data-mermaid-edge-from], g.edgeLabel[data-mermaid-edge-from]").forEach((edgeGroup) => {
      const from = String(edgeGroup.dataset.mermaidEdgeFrom || "").trim();
      const to = String(edgeGroup.dataset.mermaidEdgeTo || "").trim();
      if (!from || !to) return;
      if (traceEdges.has(`${from}>>>${to}`) || traceEdges.has(`${to}>>>${from}`)) {
        edgeGroup.classList.add("mermaid-flow-trace-edge");
      }
    });
  }
}

function clearMermaidFlowPlaybackClasses() {
  for (const containerId of MERMAID_RENDER_CONTAINER_IDS) {
    const container = el(containerId);
    if (!container) continue;
    const svg = container.querySelector("svg");
    if (!svg) continue;
    svg.querySelectorAll(".mermaid-flow-play-node").forEach((element) => {
      element.classList.remove("mermaid-flow-play-node", "mermaid-flow-play-node-peak");
    });
    svg.querySelectorAll(".mermaid-flow-play-edge").forEach((element) => {
      element.classList.remove("mermaid-flow-play-edge", "mermaid-flow-play-edge-peak");
    });
  }
}

function applyMermaidFlowPlaybackStep(step, options = {}) {
  if (!step) return false;
  clearMermaidFlowPlaybackClasses();

  const peak = options.peak === true;
  const autoCenter = options.autoCenter !== false;
  let foundAny = false;

  for (const containerId of MERMAID_RENDER_CONTAINER_IDS) {
    const container = el(containerId);
    if (!container) continue;
    const svg = container.querySelector("svg");
    if (!svg) continue;

    let focusElement = null;
    svg.querySelectorAll("g.node[data-mermaid-graph-node-id]").forEach((nodeGroup) => {
      if (nodeGroup.dataset.mermaidGraphNodeId === step.nodeId) {
        nodeGroup.classList.add("mermaid-flow-play-node");
        if (peak) {
          nodeGroup.classList.add("mermaid-flow-play-node-peak");
        }
        focusElement = nodeGroup;
        foundAny = true;
      }
    });

    const edgeMatches = [];
    if (step.fromId && step.toId) {
      svg.querySelectorAll("g.edgePath[data-mermaid-edge-from], g.edgeLabel[data-mermaid-edge-from]").forEach((edgeGroup) => {
        const from = edgeGroup.dataset.mermaidEdgeFrom;
        const to = edgeGroup.dataset.mermaidEdgeTo;
        if (
          (from === step.fromId && to === step.toId) ||
          (from === step.toId && to === step.fromId)
        ) {
          edgeGroup.classList.add("mermaid-flow-play-edge");
          if (peak) {
            edgeGroup.classList.add("mermaid-flow-play-edge-peak");
          }
          edgeMatches.push(edgeGroup);
          foundAny = true;
        }
      });
    }

    if (!focusElement && edgeMatches.length > 0) {
      focusElement = edgeMatches[0];
    }
    if (autoCenter && focusElement && MERMAID_INTERACTIVE_CONTAINER_IDS.includes(containerId)) {
      centerFocusedMermaidElement(container, focusElement, {
        forceCenter: false,
      });
    }
  }

  return foundAny;
}

function refreshMermaidInteractionState() {
  for (const containerId of MERMAID_RENDER_CONTAINER_IDS) {
    const container = el(containerId);
    if (!container) continue;
    decorateMermaidContainer(container);
    applyMermaidFocusInContainer(container);
  }
  applyMermaidFlowTraceClasses();
  if (state.flowPlaybackSession?.currentStep) {
    applyMermaidFlowPlaybackStep(state.flowPlaybackSession.currentStep, {
      peak: false,
      autoCenter: false,
    });
  }
}

function clearMermaidFocusFlashClasses() {
  for (const containerId of MERMAID_RENDER_CONTAINER_IDS) {
    const container = el(containerId);
    if (!container) continue;
    const svg = container.querySelector("svg");
    if (!svg) continue;
    svg.querySelectorAll(".mermaid-focus-flash-node").forEach((element) => {
      element.classList.remove("mermaid-focus-flash-node");
    });
    svg.querySelectorAll(".mermaid-focus-flash-edge").forEach((element) => {
      element.classList.remove("mermaid-focus-flash-edge");
    });
    svg.querySelectorAll(".mermaid-focus-ring").forEach((ring) => {
      ring.remove();
    });
  }
}

function clearMermaidFocusFlash() {
  if (state.mermaidFocusFlashTimeoutHandle !== null) {
    window.clearTimeout(state.mermaidFocusFlashTimeoutHandle);
    state.mermaidFocusFlashTimeoutHandle = null;
  }
  clearMermaidFocusFlashClasses();
}

function computeMermaidNodeBounds(nodeGroup) {
  if (!nodeGroup) return null;
  const shapeSelectors = [
    ":scope > path",
    ":scope > rect",
    ":scope > polygon",
    ":scope > ellipse",
    ":scope > circle",
  ];
  const shapeNodes = nodeGroup.querySelectorAll(shapeSelectors.join(", "));
  let minX = Number.POSITIVE_INFINITY;
  let minY = Number.POSITIVE_INFINITY;
  let maxX = Number.NEGATIVE_INFINITY;
  let maxY = Number.NEGATIVE_INFINITY;
  let found = false;

  for (const shapeNode of shapeNodes) {
    if (shapeNode.classList.contains("mermaid-focus-ring")) continue;
    let box;
    try {
      box = shapeNode.getBBox();
    } catch (_err) {
      continue;
    }
    if (!box || box.width <= 0 || box.height <= 0) continue;
    found = true;
    minX = Math.min(minX, box.x);
    minY = Math.min(minY, box.y);
    maxX = Math.max(maxX, box.x + box.width);
    maxY = Math.max(maxY, box.y + box.height);
  }

  if (!found) {
    try {
      const box = nodeGroup.getBBox();
      if (box && box.width > 0 && box.height > 0) {
        return {
          x: box.x,
          y: box.y,
          width: box.width,
          height: box.height,
        };
      }
    } catch (_err) {
      return null;
    }
    return null;
  }

  return {
    x: minX,
    y: minY,
    width: maxX - minX,
    height: maxY - minY,
  };
}

function ensureMermaidFocusRing(nodeGroup) {
  if (!nodeGroup) return;
  const bounds = computeMermaidNodeBounds(nodeGroup);
  if (!bounds) return;

  let ring = nodeGroup.querySelector(":scope > circle.mermaid-focus-ring");
  if (!ring) {
    ring = document.createElementNS(SVG_NS, "circle");
    ring.classList.add("mermaid-focus-ring");
    ring.setAttribute("fill", "none");
    ring.setAttribute("pointer-events", "none");
    nodeGroup.insertBefore(ring, nodeGroup.firstChild);
  }

  const cx = bounds.x + bounds.width / 2;
  const cy = bounds.y + bounds.height / 2;
  const radius = Math.max(bounds.width, bounds.height) * 0.5 + MERMAID_FOCUS_RING_EXTRA_RADIUS;
  ring.setAttribute("cx", cx.toFixed(2));
  ring.setAttribute("cy", cy.toFixed(2));
  ring.setAttribute("r", radius.toFixed(2));
}

function applyMermaidFocusFlashClasses() {
  let foundAny = false;
  for (const containerId of MERMAID_RENDER_CONTAINER_IDS) {
    const container = el(containerId);
    if (!container) continue;
    const svg = container.querySelector("svg");
    if (!svg) continue;

    const focusedNodes = svg.querySelectorAll("g.node.mermaid-focused-node");
    const focusedEdges = svg.querySelectorAll(
      "g.edgePath.mermaid-focused-edge, g.edgeLabel.mermaid-focused-edge"
    );
    if (focusedNodes.length === 0 && focusedEdges.length === 0) continue;
    foundAny = true;

    focusedNodes.forEach((nodeGroup) => {
      nodeGroup.classList.add("mermaid-focus-flash-node");
      ensureMermaidFocusRing(nodeGroup);
    });
    focusedEdges.forEach((edgeGroup) => {
      edgeGroup.classList.add("mermaid-focus-flash-edge");
    });
  }
  return foundAny;
}

function flashMermaidFocus() {
  clearMermaidFocusFlash();
  const foundAny = applyMermaidFocusFlashClasses();

  if (!foundAny) return;
  state.mermaidFocusFlashTimeoutHandle = window.setTimeout(() => {
    clearMermaidFocusFlash();
  }, FOCUS_FLASH_DURATION_MS);
}

function queueMermaidFocusSync(options = {}) {
  const requestedMinToken = Number(options.minRenderToken);
  const minRenderToken = Number.isFinite(requestedMinToken)
    ? Math.max(0, Math.floor(requestedMinToken))
    : 0;
  state.pendingMermaidFocusSync = {
    forceCenter: options.forceCenter === true,
    ensureZoom: options.ensureZoom === true,
    flash: options.flash !== false,
    minRenderToken,
  };
}

function applyPendingMermaidFocusSync(renderToken = state.mermaidRenderToken) {
  const pending = state.pendingMermaidFocusSync;
  if (!pending) return;
  if (
    Number.isFinite(pending.minRenderToken) &&
    renderToken < pending.minRenderToken
  ) {
    return;
  }
  state.pendingMermaidFocusSync = null;

  const hasFocusTarget = Boolean(state.mermaidFocusedNodeId) || Boolean(state.mermaidFocusedEdge);
  if (!hasFocusTarget) return;

  const found = scrollMermaidFocusIntoView({
    forceCenter: pending.forceCenter,
    ensureZoom: pending.ensureZoom,
  });
  if (pending.flash) {
    flashMermaidFocus();
  }
  if (!found && state.latestMermaidSource.trim().length > 0) {
    setExpansionStatusText(
      "Focused graph item is not currently present in Mermaid preview (search subset/truncation).",
      "muted"
    );
  }
}

function centerFocusedMermaidElement(container, element, options = {}) {
  if (!container || !element) return;
  if (container.clientWidth <= 1 || container.clientHeight <= 1) return;

  const forceCenter = options.forceCenter === true;
  const containerRect = container.getBoundingClientRect();
  const elementRect = element.getBoundingClientRect();
  if (elementRect.width <= 0 || elementRect.height <= 0) return;

  const margin = 26;
  const inView =
    elementRect.left >= containerRect.left + margin &&
    elementRect.right <= containerRect.right - margin &&
    elementRect.top >= containerRect.top + margin &&
    elementRect.bottom <= containerRect.bottom - margin;
  if (inView && !forceCenter) return;

  const elementCenterX = elementRect.left + elementRect.width / 2;
  const elementCenterY = elementRect.top + elementRect.height / 2;
  const containerCenterX = containerRect.left + container.clientWidth / 2;
  const containerCenterY = containerRect.top + container.clientHeight / 2;
  const deltaX = elementCenterX - containerCenterX;
  const deltaY = elementCenterY - containerCenterY;
  const blend = forceCenter ? MERMAID_FOCUS_CENTER_BLEND : 1;
  const targetScrollLeft = container.scrollLeft + deltaX * blend;
  const targetScrollTop = container.scrollTop + deltaY * blend;
  const maxScrollLeft = Math.max(0, container.scrollWidth - container.clientWidth);
  const maxScrollTop = Math.max(0, container.scrollHeight - container.clientHeight);

  container.scrollLeft = Math.min(maxScrollLeft, Math.max(0, targetScrollLeft));
  container.scrollTop = Math.min(maxScrollTop, Math.max(0, targetScrollTop));
}

function scrollMermaidFocusIntoView(options = {}) {
  const forceCenter = options.forceCenter === true;
  const ensureZoom = options.ensureZoom === true;
  if (ensureZoom && state.mermaidZoom < MERMAID_FOCUS_MIN_ZOOM) {
    state.mermaidZoom = MERMAID_FOCUS_MIN_ZOOM;
    applyMermaidZoom();
  }

  let foundAny = false;
  for (const containerId of MERMAID_RENDER_CONTAINER_IDS) {
    const container = el(containerId);
    if (!container) continue;

    let focusedElement = null;
    if (state.mermaidFocusedEdge?.from && state.mermaidFocusedEdge?.to) {
      focusedElement = container.querySelector(
        "g.edgePath.mermaid-focused-edge, g.edgeLabel.mermaid-focused-edge"
      );
    }
    if (!focusedElement) {
      focusedElement = container.querySelector("g.node.mermaid-focused-node");
    }
    if (!focusedElement) continue;
    foundAny = true;
    centerFocusedMermaidElement(container, focusedElement, { forceCenter });
  }
  return foundAny;
}

function setMermaidFocusNode(nodeId, options = {}) {
  const nextNodeId = String(nodeId || "").trim();
  if (!nextNodeId) return;
  state.mermaidFocusedNodeId = nextNodeId;
  state.mermaidFocusedEdge = null;
  applyGraphMermaidFocus({
    zoomGraph: options.zoomGraph === true,
    selectNode: options.selectNode === true,
  });
  flashGraphMermaidFocus();
  refreshMermaidInteractionState();
  let foundInMermaid = true;
  if (options.scrollMermaid !== false) {
    foundInMermaid = scrollMermaidFocusIntoView({
      forceCenter: options.forceCenterMermaid === true,
      ensureZoom: options.ensureMermaidZoom === true,
    });
  }
  flashMermaidFocus();
  if (!foundInMermaid && state.latestMermaidSource.trim().length > 0) {
    setExpansionStatusText(
      "Selected node is not currently present in Mermaid preview (search subset/truncation).",
      "muted"
    );
  }
}

function setMermaidFocusEdge(fromNodeId, toNodeId, options = {}) {
  const from = String(fromNodeId || "").trim();
  const to = String(toNodeId || "").trim();
  if (!from || !to) return;
  state.mermaidFocusedNodeId = null;
  state.mermaidFocusedEdge = { from, to };
  applyGraphMermaidFocus({
    zoomGraph: options.zoomGraph === true,
  });
  flashGraphMermaidFocus();
  refreshMermaidInteractionState();
  let foundInMermaid = true;
  if (options.scrollMermaid !== false) {
    foundInMermaid = scrollMermaidFocusIntoView({
      forceCenter: options.forceCenterMermaid === true,
      ensureZoom: options.ensureMermaidZoom === true,
    });
  }
  flashMermaidFocus();
  if (!foundInMermaid && state.latestMermaidSource.trim().length > 0) {
    setExpansionStatusText(
      "Selected edge is not currently present in Mermaid preview (search subset/truncation).",
      "muted"
    );
  }
}

function clearMermaidFocus(options = {}) {
  const applyGraph = options.applyGraph !== false;
  const applyMermaid = options.applyMermaid !== false;
  state.mermaidFocusedNodeId = null;
  state.mermaidFocusedEdge = null;
  if (applyGraph) {
    clearGraphMermaidFocus();
  }
  clearMermaidFocusFlash();
  if (applyMermaid) {
    refreshMermaidInteractionState();
  }
}

function bindMermaidContainerClickInteractions(container) {
  if (!container || container.dataset.mermaidClickBound === "1") return;
  container.dataset.mermaidClickBound = "1";

  container.addEventListener("click", (event) => {
    const svg = container.querySelector("svg");
    if (!svg) return;

    const nodeGroup = event.target.closest("g.node[data-mermaid-graph-node-id]");
    if (nodeGroup && svg.contains(nodeGroup)) {
      const graphNodeId = String(nodeGroup.dataset.mermaidGraphNodeId || "").trim();
      if (!graphNodeId) return;
      event.preventDefault();
      setMermaidFocusNode(graphNodeId, {
        zoomGraph: true,
        selectNode: true,
        scrollMermaid: false,
      });
      return;
    }

    const edgeGroup = event.target.closest("g.edgePath[data-mermaid-edge-from], g.edgeLabel[data-mermaid-edge-from]");
    if (edgeGroup && svg.contains(edgeGroup)) {
      const fromNodeId = String(edgeGroup.dataset.mermaidEdgeFrom || "").trim();
      const toNodeId = String(edgeGroup.dataset.mermaidEdgeTo || "").trim();
      if (!fromNodeId || !toNodeId) return;
      event.preventDefault();
      setMermaidFocusEdge(fromNodeId, toNodeId, {
        zoomGraph: true,
        scrollMermaid: false,
      });
    }
  });
}

function setMermaidContainerInteractiveState(container) {
  if (!container) return;
  const hasSvg = Boolean(container.querySelector("svg"));
  container.classList.toggle("mermaid-interactive", hasSvg);
  if (!hasSvg) {
    container.classList.remove("mermaid-dragging");
  }
}

function bindMermaidContainerInteractions(container) {
  if (!container || container.dataset.mermaidPanZoomBound === "1") return;
  container.dataset.mermaidPanZoomBound = "1";

  let dragState = null;
  const stopDrag = () => {
    if (!dragState) return;
    dragState = null;
    container.classList.remove("mermaid-dragging");
  };

  container.addEventListener("mousedown", (event) => {
    if (event.button !== 0) return;
    if (!container.classList.contains("mermaid-interactive")) return;
    if (!container.querySelector("svg")) return;
    dragState = {
      startX: event.clientX,
      startY: event.clientY,
      startScrollLeft: container.scrollLeft,
      startScrollTop: container.scrollTop,
    };
    container.classList.add("mermaid-dragging");
    event.preventDefault();
  });

  window.addEventListener("mousemove", (event) => {
    if (!dragState) return;
    const deltaX = event.clientX - dragState.startX;
    const deltaY = event.clientY - dragState.startY;
    container.scrollLeft = dragState.startScrollLeft - deltaX;
    container.scrollTop = dragState.startScrollTop - deltaY;
  });
  window.addEventListener("mouseup", stopDrag);
  window.addEventListener("blur", stopDrag);

  container.addEventListener("mouseleave", (event) => {
    if ((event.buttons & 1) === 0) {
      stopDrag();
    }
  });

  container.addEventListener(
    "wheel",
    (event) => {
      if (!container.classList.contains("mermaid-interactive")) return;
      const svg = container.querySelector("svg");
      if (!svg) return;
      if (Math.abs(event.deltaY) < 0.0001) return;
      event.preventDefault();

      const zoomFactor =
        event.deltaY < 0 ? MERMAID_WHEEL_ZOOM_IN_FACTOR : MERMAID_WHEEL_ZOOM_OUT_FACTOR;
      const nextZoom = clampMermaidZoom(state.mermaidZoom * zoomFactor);
      if (Math.abs(nextZoom - state.mermaidZoom) < 0.0001) {
        return;
      }

      const rect = container.getBoundingClientRect();
      const pointerX = event.clientX - rect.left;
      const pointerY = event.clientY - rect.top;
      const beforeRect = svg.getBoundingClientRect();
      const beforeWidth = beforeRect.width > 0 ? beforeRect.width : Math.max(1, svg.scrollWidth);
      const beforeHeight = beforeRect.height > 0 ? beforeRect.height : Math.max(1, svg.scrollHeight);
      const anchorX = container.scrollLeft + pointerX;
      const anchorY = container.scrollTop + pointerY;
      const ratioX = anchorX / beforeWidth;
      const ratioY = anchorY / beforeHeight;

      state.mermaidZoom = nextZoom;
      applyMermaidZoom();

      const zoomedSvg = container.querySelector("svg");
      if (!zoomedSvg) return;
      const afterRect = zoomedSvg.getBoundingClientRect();
      const afterWidth =
        afterRect.width > 0 ? afterRect.width : Math.max(1, zoomedSvg.scrollWidth);
      const afterHeight =
        afterRect.height > 0 ? afterRect.height : Math.max(1, zoomedSvg.scrollHeight);
      const nextScrollLeft = ratioX * afterWidth - pointerX;
      const nextScrollTop = ratioY * afterHeight - pointerY;
      const maxScrollLeft = Math.max(0, container.scrollWidth - container.clientWidth);
      const maxScrollTop = Math.max(0, container.scrollHeight - container.clientHeight);
      container.scrollLeft = Math.min(maxScrollLeft, Math.max(0, nextScrollLeft));
      container.scrollTop = Math.min(maxScrollTop, Math.max(0, nextScrollTop));
    },
    { passive: false }
  );
}

function bindMermaidInteractions() {
  for (const containerId of MERMAID_INTERACTIVE_CONTAINER_IDS) {
    const container = el(containerId);
    if (!container) continue;
    bindMermaidContainerInteractions(container);
    setMermaidContainerInteractiveState(container);
  }
  for (const containerId of MERMAID_RENDER_CONTAINER_IDS) {
    const container = el(containerId);
    if (!container) continue;
    bindMermaidContainerClickInteractions(container);
  }
  refreshMermaidInteractionState();
}

function applyMermaidZoom() {
  state.mermaidZoom = clampMermaidZoom(state.mermaidZoom);
  syncMermaidZoomLabel();

  for (const containerId of MERMAID_INTERACTIVE_CONTAINER_IDS) {
    const container = el(containerId);
    if (!container) continue;
    const svg = container.querySelector("svg");
    if (svg) {
      svg.style.width = `${state.mermaidZoom * 100}%`;
      svg.style.height = "auto";
      svg.style.maxWidth = "none";
    }
    setMermaidContainerInteractiveState(container);
  }
  refreshMermaidInteractionState();
}

function setMermaidSplitOpen(nextOpen, options = {}) {
  const syncUrl = options.syncUrl !== false;
  const persistSearchPreference = options.persistSearchPreference === true;
  const stage = el("graph-stage");
  const panel = el("mermaid-split-panel");
  const toggle = el("toggle-mermaid-split");
  const smallContainer = el("search-mermaid-diagram");
  const largeContainer = el("search-mermaid-diagram-large");
  const searchActive = state.searchQuery.trim().length > 0;
  const insightsPanel = el("search-insights");
  const hasMermaid = state.latestMermaidSource.trim().length > 0;
  const canOpen = searchActive || hasMermaid || (insightsPanel && insightsPanel.hidden === false);
  if (!stage || !panel || !toggle) return;

  const previousOpen = state.mermaidSplitOpen;
  state.mermaidSplitOpen = canOpen && nextOpen === true;
  if (state.mermaidSplitOpen) {
    panel.hidden = false;
    panel.removeAttribute("hidden");
  } else {
    panel.hidden = true;
    panel.setAttribute("hidden", "");
  }
  stage.classList.toggle("split-open", state.mermaidSplitOpen);
  toggle.disabled = !canOpen;
  toggle.setAttribute("aria-pressed", state.mermaidSplitOpen ? "true" : "false");
  toggle.textContent = state.mermaidSplitOpen ? "Hide Mermaid Split" : "Mermaid Split";

  if (persistSearchPreference && searchActive) {
    state.searchSplitAutoOpen = state.mermaidSplitOpen;
    writeLocalStorage(
      SEARCH_SPLIT_AUTO_OPEN_STORAGE_KEY,
      state.searchSplitAutoOpen ? "1" : "0"
    );
  }

  if (
    state.mermaidSplitOpen &&
    largeContainer &&
    smallContainer &&
    !largeContainer.querySelector("svg") &&
    smallContainer.querySelector("svg")
  ) {
    largeContainer.innerHTML = smallContainer.innerHTML;
    applyMermaidZoom();
  }

  if (state.cy && previousOpen !== state.mermaidSplitOpen) {
    const resizeCy = () => {
      if (!state.cy) return;
      state.cy.resize();
    };
    window.requestAnimationFrame(() => {
      resizeCy();
      window.requestAnimationFrame(resizeCy);
    });
    window.setTimeout(resizeCy, 120);
    window.setTimeout(() => {
      resizeCy();
      fitGraphToCurrentContext();
    }, 280);
  }
  if (syncUrl) {
    writeUrlViewState();
  }
}

function setMermaidModalOpen(nextOpen, options = {}) {
  const syncUrl = options.syncUrl !== false;
  const overlay = el("mermaid-modal-overlay");
  const splitButton = el("open-mermaid-modal");
  const inlineButton = el("open-mermaid-modal-inline");
  const searchActive = state.searchQuery.trim().length > 0;
  const insightsPanel = el("search-insights");
  const hasMermaid = state.latestMermaidSource.trim().length > 0;
  const canOpen = searchActive || hasMermaid || (insightsPanel && insightsPanel.hidden === false);
  if (!overlay) return;

  state.mermaidModalOpen = canOpen && nextOpen === true;
  overlay.hidden = !state.mermaidModalOpen;

  if (splitButton) {
    splitButton.disabled = !canOpen;
    splitButton.textContent = state.mermaidModalOpen ? "Popout Open" : "Popout";
  }
  if (inlineButton) {
    inlineButton.disabled = !canOpen;
    inlineButton.textContent = state.mermaidModalOpen ? "Close Popout" : "Mermaid Popout";
    inlineButton.setAttribute("aria-pressed", state.mermaidModalOpen ? "true" : "false");
  }

  if (!state.mermaidModalOpen) {
    if (syncUrl) {
      writeUrlViewState();
    }
    return;
  }

  const modalContainer = el("search-mermaid-diagram-modal");
  const largeContainer = el("search-mermaid-diagram-large");
  if (modalContainer) {
    if (largeContainer && largeContainer.innerHTML.trim().length > 0) {
      modalContainer.innerHTML = largeContainer.innerHTML;
    } else {
      modalContainer.innerHTML = defaultMermaidPlaceholder();
    }
  }
  applyMermaidZoom();
  if (syncUrl) {
    writeUrlViewState();
  }
}

function hideSearchInsights(options = {}) {
  const syncUrl = options.syncUrl !== false;
  const panel = el("search-insights");
  const source = el("search-mermaid-source");
  const diagram = el("search-mermaid-diagram");
  const largeDiagram = el("search-mermaid-diagram-large");
  const modalDiagram = el("search-mermaid-diagram-modal");
  const summary = el("search-insights-summary");
  const copyButton = el("copy-search-mermaid");
  if (panel) {
    panel.hidden = true;
  }
  if (summary) {
    summary.innerHTML = "";
  }
  if (source) {
    source.textContent = "Search to generate Mermaid.";
  }
  if (diagram) {
    diagram.innerHTML = defaultMermaidPlaceholder();
  }
  if (largeDiagram) {
    largeDiagram.innerHTML = defaultMermaidPlaceholder();
  }
  if (modalDiagram) {
    modalDiagram.innerHTML = defaultMermaidPlaceholder();
  }
  if (copyButton) {
    copyButton.disabled = true;
    copyButton.textContent = "Copy Mermaid";
  }
  state.mermaidAliasByGraphNodeId = new Map();
  state.mermaidGraphNodeIdByAlias = new Map();
  state.pendingMermaidFocusSync = null;
  state.activeInsightsMode = "none";
  clearMermaidFocus({ applyGraph: true, applyMermaid: false });
  clearMermaidFlowPlaybackClasses();
  clearMermaidFlowTraceClasses();
  state.latestMermaidSource = "";
  setInsightsTitle("search");
  state.mermaidRenderToken += 1;
  applyMermaidZoom();
  setMermaidModalOpen(false, { syncUrl });
  setMermaidSplitOpen(false, { syncUrl });
}

function revealSearchInsights() {
  const panel = el("search-insights");
  const toggle = el("toggle-mermaid-split");
  const splitButton = el("open-mermaid-modal");
  const inlineButton = el("open-mermaid-modal-inline");
  if (!panel) return;
  panel.hidden = false;
  if (toggle) {
    toggle.disabled = false;
    toggle.setAttribute("aria-pressed", state.mermaidSplitOpen ? "true" : "false");
    toggle.textContent = state.mermaidSplitOpen ? "Hide Mermaid Split" : "Mermaid Split";
  }
  if (splitButton) {
    splitButton.disabled = false;
  }
  if (inlineButton) {
    inlineButton.disabled = false;
    inlineButton.textContent = state.mermaidModalOpen ? "Close Popout" : "Mermaid Popout";
  }
}

function truncateNodeLabel(label, maxLength = 58) {
  const text = String(label || "");
  if (text.length <= maxLength) return text;
  return `${text.slice(0, maxLength - 1)}…`;
}

function escapeMermaidLabel(value) {
  return String(value ?? "")
    .replaceAll("\\", "\\\\")
    .replaceAll('"', '\\"')
    .replaceAll("\n", " ");
}

function searchInsightNodeRank(node) {
  const kindPriority = {
    crate: 0,
    module: 1,
    file: 2,
    function: 3,
    struct: 4,
    enum: 5,
    trait: 6,
    const: 7,
    static: 8,
    type_alias: 9,
  };
  return kindPriority[node.kind] ?? 99;
}

function buildSearchInsightsModel(slice, matchedIds) {
  const index = state.graphIndex;
  if (!index || !slice || !matchedIds) return null;
  // Always discover neighbours from the *raw* graph, not the cy slice.
  // The slice only contains nodes the cytoscape view is rendering
  // (the matched node + its ancestor crate/module/file), so walking
  // `slice.edges` for neighbour discovery silently drops all the call
  // targets of a function match — yielding a mermaid with only the
  // match plus its enclosing crate/module instead of the full local
  // subgraph the details pane already shows edge counts for.
  const rawEdges = (state.graph && Array.isArray(state.graph.edges)) ? state.graph.edges : [];

  const matchedNodes = Array.from(matchedIds)
    .map((id) => index.nodeById.get(id))
    .filter((node) => !!node)
    .sort((a, b) => {
      const rankDelta = searchInsightNodeRank(a) - searchInsightNodeRank(b);
      if (rankDelta !== 0) return rankDelta;
      const degreeDelta = (index.degreeById.get(b.id) || 0) - (index.degreeById.get(a.id) || 0);
      if (degreeDelta !== 0) return degreeDelta;
      return compareNodeLabels(index.nodeById, a.id, b.id);
    });

  const primaryMatchedLimit = Math.max(10, Math.floor(SEARCH_INSIGHTS_NODE_LIMIT * 0.65));
  const primaryMatched = matchedNodes.slice(0, primaryMatchedLimit);
  const selectedIds = new Set(primaryMatched.map((node) => node.id));

  const neighborSet = new Set();
  for (const edge of rawEdges) {
    if (selectedIds.has(edge.from) && !selectedIds.has(edge.to)) {
      neighborSet.add(edge.to);
    }
    if (selectedIds.has(edge.to) && !selectedIds.has(edge.from)) {
      neighborSet.add(edge.from);
    }
  }

  const neighborIds = Array.from(neighborSet)
    .filter((nodeId) => index.nodeById.has(nodeId))
    .sort((aId, bId) => {
      const aMatched = matchedIds.has(aId) ? 1 : 0;
      const bMatched = matchedIds.has(bId) ? 1 : 0;
      if (aMatched !== bMatched) return bMatched - aMatched;
      const aNode = index.nodeById.get(aId);
      const bNode = index.nodeById.get(bId);
      const rankDelta = searchInsightNodeRank(aNode) - searchInsightNodeRank(bNode);
      if (rankDelta !== 0) return rankDelta;
      const degreeDelta = (index.degreeById.get(bId) || 0) - (index.degreeById.get(aId) || 0);
      if (degreeDelta !== 0) return degreeDelta;
      return compareNodeLabels(index.nodeById, aId, bId);
    });

  for (const nodeId of neighborIds) {
    if (selectedIds.size >= SEARCH_INSIGHTS_NODE_LIMIT) break;
    selectedIds.add(nodeId);
  }

  const selectedNodes = Array.from(selectedIds)
    .map((nodeId) => index.nodeById.get(nodeId))
    .filter((node) => !!node)
    .sort((a, b) => {
      const aMatched = matchedIds.has(a.id) ? 1 : 0;
      const bMatched = matchedIds.has(b.id) ? 1 : 0;
      if (aMatched !== bMatched) return bMatched - aMatched;
      const rankDelta = searchInsightNodeRank(a) - searchInsightNodeRank(b);
      if (rankDelta !== 0) return rankDelta;
      return compareNodeLabels(index.nodeById, a.id, b.id);
    });

  const internalEdges = rawEdges.filter(
    (edge) => selectedIds.has(edge.from) && selectedIds.has(edge.to)
  );
  const selectedEdges = internalEdges.slice(0, SEARCH_INSIGHTS_EDGE_LIMIT);

  return {
    mode: "search",
    query: state.searchQuery.trim(),
    totalMatches: state.searchTotalMatches,
    shownMatches: matchedNodes.length,
    selectedNodes,
    selectedEdges,
    selectedIds,
    supportNodeCount: selectedNodes.filter((node) => !matchedIds.has(node.id)).length,
    nodeTruncated: matchedNodes.length > primaryMatched.length || neighborIds.length > Math.max(0, SEARCH_INSIGHTS_NODE_LIMIT - primaryMatched.length),
    internalEdgeCount: internalEdges.length,
    edgeTruncated: internalEdges.length > selectedEdges.length,
  };
}

function buildFocusInsightsModel(slice, focusNodeId) {
  const index = state.graphIndex;
  if (!index || !focusNodeId) return null;
  // Look the focus node up in the raw graph index, not the cy slice.
  // For a search hit on `list_approvals` the slice only contains the
  // function plus its ancestor crate/module/file — none of the 30
  // outgoing call neighbours are there. Pulling neighbours from
  // `state.graph.edges` (raw) lets the focus mermaid render the
  // full local subgraph even when those neighbours haven't been
  // expanded into the cytoscape view.
  const focusNode = index.nodeById.get(focusNodeId);
  if (!focusNode) return null;
  const rawEdges = (state.graph && Array.isArray(state.graph.edges)) ? state.graph.edges : [];

  const neighborSet = new Set();
  for (const edge of rawEdges) {
    if (edge.from === focusNodeId && edge.to !== focusNodeId) {
      neighborSet.add(edge.to);
    } else if (edge.to === focusNodeId && edge.from !== focusNodeId) {
      neighborSet.add(edge.from);
    }
  }

  const neighborIds = Array.from(neighborSet)
    .filter((nodeId) => index.nodeById.has(nodeId))
    .sort((aId, bId) => {
      const aNode = index.nodeById.get(aId);
      const bNode = index.nodeById.get(bId);
      const rankDelta = searchInsightNodeRank(aNode) - searchInsightNodeRank(bNode);
      if (rankDelta !== 0) return rankDelta;
      const degreeDelta = (index.degreeById.get(bId) || 0) - (index.degreeById.get(aId) || 0);
      if (degreeDelta !== 0) return degreeDelta;
      return compareNodeLabels(index.nodeById, aId, bId);
    });

  const selectedIds = new Set([focusNodeId]);
  for (const neighborId of neighborIds) {
    if (selectedIds.size >= SEARCH_INSIGHTS_NODE_LIMIT) break;
    selectedIds.add(neighborId);
  }

  const selectedNodes = [focusNode];
  selectedNodes.push(
    ...Array.from(selectedIds)
      .filter((nodeId) => nodeId !== focusNodeId)
      .map((nodeId) => index.nodeById.get(nodeId))
      .filter((node) => !!node)
  );

  const internalEdges = rawEdges.filter(
    (edge) => selectedIds.has(edge.from) && selectedIds.has(edge.to)
  );
  const selectedEdges = internalEdges.slice(0, SEARCH_INSIGHTS_EDGE_LIMIT);

  return {
    mode: "focus",
    focusNodeId,
    focusLabel: focusNode.label,
    selectedNodes,
    selectedEdges,
    selectedIds,
    supportNodeCount: Math.max(0, selectedNodes.length - 1),
    nodeTruncated: neighborIds.length > Math.max(0, SEARCH_INSIGHTS_NODE_LIMIT - 1),
    internalEdgeCount: internalEdges.length,
    edgeTruncated: internalEdges.length > selectedEdges.length,
  };
}

function buildImpactInsightsModel(slice, matchedIds) {
  const index = state.graphIndex;
  if (!index || !slice || !matchedIds) return null;
  const nodeById = new Map(slice.nodes.map((node) => [node.id, node]));

  const matchedFiles = Array.from(matchedIds)
    .map((nodeId) => nodeById.get(nodeId))
    .filter((node) => !!node)
    .sort((a, b) => {
      const rankDelta = searchInsightNodeRank(a) - searchInsightNodeRank(b);
      if (rankDelta !== 0) return rankDelta;
      return compareNodeLabels(index.nodeById, a.id, b.id);
    });

  const selectedIds = new Set();
  const primaryLimit = Math.max(8, Math.floor(SEARCH_INSIGHTS_NODE_LIMIT * 0.6));
  for (const node of matchedFiles.slice(0, primaryLimit)) {
    selectedIds.add(node.id);
  }

  if (selectedIds.size === 0) {
    const fallbackNodes = slice.nodes
      .slice()
      .sort((a, b) => {
        const degreeDelta = (index.degreeById.get(b.id) || 0) - (index.degreeById.get(a.id) || 0);
        if (degreeDelta !== 0) return degreeDelta;
        return compareNodeLabels(index.nodeById, a.id, b.id);
      })
      .slice(0, Math.min(SEARCH_INSIGHTS_NODE_LIMIT, 12));
    for (const node of fallbackNodes) {
      selectedIds.add(node.id);
    }
  }

  const neighborSet = new Set();
  for (const edge of slice.edges) {
    if (selectedIds.has(edge.from) && !selectedIds.has(edge.to)) {
      neighborSet.add(edge.to);
    }
    if (selectedIds.has(edge.to) && !selectedIds.has(edge.from)) {
      neighborSet.add(edge.from);
    }
  }

  const neighborIds = Array.from(neighborSet)
    .filter((nodeId) => nodeById.has(nodeId))
    .sort((aId, bId) => {
      const aMatched = matchedIds.has(aId) ? 1 : 0;
      const bMatched = matchedIds.has(bId) ? 1 : 0;
      if (aMatched !== bMatched) return bMatched - aMatched;
      const degreeDelta = (index.degreeById.get(bId) || 0) - (index.degreeById.get(aId) || 0);
      if (degreeDelta !== 0) return degreeDelta;
      return compareNodeLabels(index.nodeById, aId, bId);
    });
  for (const nodeId of neighborIds) {
    if (selectedIds.size >= SEARCH_INSIGHTS_NODE_LIMIT) break;
    selectedIds.add(nodeId);
  }

  const selectedNodes = Array.from(selectedIds)
    .map((nodeId) => nodeById.get(nodeId))
    .filter((node) => !!node)
    .sort((a, b) => {
      const aMatched = matchedIds.has(a.id) ? 1 : 0;
      const bMatched = matchedIds.has(b.id) ? 1 : 0;
      if (aMatched !== bMatched) return bMatched - aMatched;
      const rankDelta = searchInsightNodeRank(a) - searchInsightNodeRank(b);
      if (rankDelta !== 0) return rankDelta;
      return compareNodeLabels(index.nodeById, a.id, b.id);
    });

  const internalEdges = slice.edges.filter(
    (edge) => selectedIds.has(edge.from) && selectedIds.has(edge.to)
  );
  const selectedEdges = internalEdges.slice(0, SEARCH_INSIGHTS_EDGE_LIMIT);

  return {
    mode: "impact",
    baseRef: state.impactBaseRef,
    compareMode: normalizeImpactCompareMode(state.impactCompareMode),
    hopCount: clampImpactHops(state.impactHopCount),
    changedFileCount: state.impactChangedFiles.length,
    mappedFileCount: matchedFiles.length,
    selectedNodes,
    selectedEdges,
    selectedIds,
    supportNodeCount: selectedNodes.filter((node) => !matchedIds.has(node.id)).length,
    nodeTruncated:
      matchedFiles.length > primaryLimit ||
      neighborIds.length > Math.max(0, SEARCH_INSIGHTS_NODE_LIMIT - primaryLimit),
    internalEdgeCount: internalEdges.length,
    edgeTruncated: internalEdges.length > selectedEdges.length,
  };
}

function insightsNodeDisplayLabel(node, mode = "search") {
  if (!node || typeof node !== "object") return "unknown";
  const kind = String(node.kind || "");
  const baseLabel = String(node.label || node.id || "unknown").trim() || "unknown";

  if (kind === "endpoint") {
    const method = String(node.method || "").trim();
    const route = String(node.route || "").trim();
    const handler = String(node.handler || "").trim();
    const head = `${method} ${route}`.trim() || baseLabel;
    return handler.length > 0 ? `EP ${head} | ${handler}` : `EP ${head}`;
  }
  if (kind === "api_call") {
    const method = String(node.method || "").trim();
    const target = String(node.target || "").trim();
    const caller = String(node.caller || "").trim();
    const callHead = target.length > 0 ? `${method} ${target}`.trim() : baseLabel;
    return caller.length > 0 ? `API ${callHead} | ${caller}` : `API ${callHead}`;
  }
  if (mode === "flow" && kind === "function") {
    const module = String(node.module || "").trim();
    const moduleLeaf = module.includes("::") ? module.split("::").slice(-1)[0] : module;
    if (moduleLeaf.length > 0) {
      return `${baseLabel} | ${moduleLeaf}`;
    }
  }
  return baseLabel;
}

function buildFlowInsightsModel(slice) {
  const index = state.graphIndex;
  const flowView = state.flowLastView;
  const flowResult = state.flowLastResult;
  if (!index || !slice || !flowView || !flowResult) return null;

  const traces = [...(flowView.downstream || []), ...(flowView.upstream || [])];
  const limitedTraces = traces.slice(0, FLOW_INSIGHTS_TRACE_LIMIT);
  const nodeById = new Map(slice.nodes.map((node) => [node.id, node]));
  const traceFallbackById = new Map();
  limitedTraces.forEach((trace) => {
    const traceNodes = Array.isArray(trace?.nodes) ? trace.nodes : [];
    traceNodes.forEach((node) => {
      const nodeId = String(node?.id || "").trim();
      if (!nodeId || traceFallbackById.has(nodeId)) return;
      traceFallbackById.set(nodeId, {
        id: nodeId,
        kind: String(node?.kind || "unknown"),
        label: String(node?.label || nodeId),
        method: String(node?.method || ""),
        route: String(node?.route || ""),
        target: String(node?.target || ""),
        path: String(node?.path || ""),
      });
    });
  });

  const orderedNodeIds = [];
  const selectedIds = new Set();
  const selectedEdges = [];
  const selectedEdgeKeys = new Set();
  const traceNodeIds = new Set();

  const pushNode = (nodeId) => {
    const id = String(nodeId || "").trim();
    if (!id || selectedIds.has(id)) return;
    if (selectedIds.size >= FLOW_INSIGHTS_NODE_LIMIT) return;
    selectedIds.add(id);
    orderedNodeIds.push(id);
  };
  const pushEdge = (edge) => {
    if (!edge) return;
    const from = String(edge.from || "").trim();
    const to = String(edge.to || "").trim();
    if (!from || !to) return;
    if (!selectedIds.has(from) || !selectedIds.has(to)) return;
    if (selectedEdges.length >= FLOW_INSIGHTS_EDGE_LIMIT) return;
    const key = `${from}>>>${to}>>>${String(edge.kind || "")}`;
    if (selectedEdgeKeys.has(key)) return;
    selectedEdgeKeys.add(key);
    selectedEdges.push({
      from,
      to,
      kind: String(edge.kind || "flow"),
    });
  };

  const sourceId = String(flowResult?.source?.id || state.flowSourceId || "").trim();
  if (sourceId) {
    pushNode(sourceId);
    traceNodeIds.add(sourceId);
  }

  const sliceEdgeByPair = new Map();
  slice.edges.forEach((edge) => {
    const from = String(edge.from || "").trim();
    const to = String(edge.to || "").trim();
    if (!from || !to) return;
    const key = `${from}>>>${to}`;
    if (!sliceEdgeByPair.has(key)) {
      sliceEdgeByPair.set(key, edge);
    }
  });

  let traceNodeTotal = 0;
  let traceEdgeTotal = 0;
  limitedTraces.forEach((trace) => {
    const nodeIds = Array.isArray(trace?.node_ids) ? trace.node_ids : [];
    const edgeKinds = Array.isArray(trace?.edge_kinds) ? trace.edge_kinds : [];
    nodeIds.forEach((nodeId) => {
      const id = String(nodeId || "").trim();
      if (!id) return;
      traceNodeTotal += 1;
      traceNodeIds.add(id);
      pushNode(id);
    });
    for (let idx = 0; idx < nodeIds.length - 1; idx += 1) {
      const from = String(nodeIds[idx] || "").trim();
      const to = String(nodeIds[idx + 1] || "").trim();
      if (!from || !to) continue;
      traceEdgeTotal += 1;
      const edgeFromSlice =
        sliceEdgeByPair.get(`${from}>>>${to}`) ||
        sliceEdgeByPair.get(`${to}>>>${from}`);
      pushEdge({
        from,
        to,
        kind: edgeFromSlice?.kind || String(edgeKinds[idx] || "flow"),
      });
    }
  });

  if (selectedIds.size < FLOW_INSIGHTS_NODE_LIMIT) {
    for (const edge of slice.edges) {
      if (selectedIds.size >= FLOW_INSIGHTS_NODE_LIMIT) break;
      const from = String(edge.from || "").trim();
      const to = String(edge.to || "").trim();
      const fromSelected = selectedIds.has(from);
      const toSelected = selectedIds.has(to);
      if (fromSelected === toSelected) continue;
      if (!fromSelected) {
        pushNode(from);
      } else if (!toSelected) {
        pushNode(to);
      }
    }
  }

  if (selectedEdges.length < FLOW_INSIGHTS_EDGE_LIMIT) {
    for (const edge of slice.edges) {
      if (selectedEdges.length >= FLOW_INSIGHTS_EDGE_LIMIT) break;
      pushEdge(edge);
    }
  }

  const selectedNodes = orderedNodeIds
    .map((nodeId) => nodeById.get(nodeId) || traceFallbackById.get(nodeId) || index.nodeById.get(nodeId))
    .filter((node) => !!node)
    .map((node) => ({
      ...node,
      id: String(node.id),
      label: String(node.label || node.id || "unknown"),
      kind: String(node.kind || "unknown"),
    }));

  if (selectedNodes.length === 0) {
    const fallbackSourceId = String(flowResult?.source?.id || state.flowSourceId || "").trim();
    const fallbackSourceNode = fallbackSourceId
      ? (nodeById.get(fallbackSourceId) || index.nodeById.get(fallbackSourceId))
      : null;
    if (!fallbackSourceNode) return null;
    selectedNodes.push({
      ...fallbackSourceNode,
      id: String(fallbackSourceNode.id),
      label: String(fallbackSourceNode.label || fallbackSourceNode.id || "unknown"),
      kind: String(fallbackSourceNode.kind || "unknown"),
    });
  }

  const sourceLabel = String(
    flowResult?.source?.label ||
    flowResult?.source?.id ||
    state.flowSourceId ||
    ""
  );
  return {
    mode: "flow",
    sourceId,
    sourceLabel,
    selectedNodes,
    selectedEdges,
    selectedIds: new Set(selectedNodes.map((node) => node.id)),
    supportNodeCount: selectedNodes.filter((node) => !traceNodeIds.has(node.id)).length,
    nodeTruncated: traceNodeTotal > selectedNodes.length,
    internalEdgeCount: traceEdgeTotal,
    edgeTruncated: traceEdgeTotal > selectedEdges.length,
    downstreamShown: (flowView.downstream || []).length,
    upstreamShown: (flowView.upstream || []).length,
    downstreamTotal: Number(flowView.totalDownstream || (flowResult?.traces || []).length || 0),
    upstreamTotal: Number(flowView.totalUpstream || (flowResult?.upstream_traces || []).length || 0),
    traceCount: limitedTraces.length,
    traceTotal: traces.length,
  };
}

function buildSearchInsightsSummary(model) {
  if (!model) return `<div class="search-insights-muted">No insights available.</div>`;
  if (model.selectedNodes.length === 0 && model.mode !== "impact") {
    return (
      `<div class="kv"><strong>Query</strong>: ${escapeHtml(model.query)}</div>` +
      `<div class="search-insights-muted">No matches found in the current search scope.</div>`
    );
  }

  const kindCounts = countKindsForIds(model.selectedNodes.map((node) => node.id));
  const kindSummary = summarizeKindCounts(kindCounts, 6) || "none";
  const hotspots = model.selectedNodes
    .slice(0, 6)
    .map((node) => truncateNodeLabel(node.label, 36))
    .join(", ");
  const truncationLine = model.nodeTruncated || model.edgeTruncated
    ? `<div class="search-insights-muted">Diagram truncated to ${SEARCH_INSIGHTS_NODE_LIMIT} nodes / ${SEARCH_INSIGHTS_EDGE_LIMIT} edges for readability.</div>`
    : "";

  if (model.mode === "focus") {
    const focusRelated = model.selectedNodes
      .slice(1, 7)
      .map((node) => truncateNodeLabel(node.label, 36))
      .join(", ");
    return (
      `<div class="kv"><strong>Focus</strong>: ${escapeHtml(truncateNodeLabel(model.focusLabel || model.focusNodeId, 80))}</div>` +
      `<div class="kv"><strong>Diagram</strong>: ${model.selectedNodes.length} nodes, ${model.selectedEdges.length}${model.edgeTruncated ? "+" : ""} edges</div>` +
      `<div class="kv"><strong>Connected Nodes</strong>: ${model.supportNodeCount}</div>` +
      `<div class="kv"><strong>Kinds</strong>: ${escapeHtml(kindSummary)}</div>` +
      `<div class="kv"><strong>Top Connections</strong>: ${escapeHtml(focusRelated || "n/a")}</div>` +
      truncationLine
    );
  }

  if (model.mode === "impact") {
    const hotspots = model.selectedNodes
      .slice(0, 6)
      .map((node) => truncateNodeLabel(node.label, 36))
      .join(", ");
    return (
      `<div class="kv"><strong>Base</strong>: ${escapeHtml(model.baseRef || "n/a")}</div>` +
      `<div class="kv"><strong>Mode</strong>: ${escapeHtml(model.compareMode)}</div>` +
      `<div class="kv"><strong>Hops</strong>: +${model.hopCount}</div>` +
      `<div class="kv"><strong>Changed Files</strong>: ${model.mappedFileCount}/${model.changedFileCount}</div>` +
      `<div class="kv"><strong>Diagram</strong>: ${model.selectedNodes.length} nodes, ${model.selectedEdges.length}${model.edgeTruncated ? "+" : ""} edges</div>` +
      `<div class="kv"><strong>Context Nodes</strong>: ${model.supportNodeCount}</div>` +
      `<div class="kv"><strong>Kinds</strong>: ${escapeHtml(kindSummary)}</div>` +
      `<div class="kv"><strong>Top Impacted</strong>: ${escapeHtml(hotspots || "n/a")}</div>` +
      truncationLine
    );
  }

  if (model.mode === "flow") {
    const sourceLabel = truncateNodeLabel(model.sourceLabel || model.sourceId || "unknown", 90);
    return (
      `<div class="kv"><strong>Source</strong>: ${escapeHtml(sourceLabel)}</div>` +
      `<div class="kv"><strong>Traces</strong>: ${model.traceCount}/${model.traceTotal} shown</div>` +
      `<div class="kv"><strong>Downstream</strong>: ${model.downstreamShown}/${model.downstreamTotal}</div>` +
      `<div class="kv"><strong>Upstream</strong>: ${model.upstreamShown}/${model.upstreamTotal}</div>` +
      `<div class="kv"><strong>Diagram</strong>: ${model.selectedNodes.length} nodes, ${model.selectedEdges.length}${model.edgeTruncated ? "+" : ""} edges</div>` +
      `<div class="kv"><strong>Context Nodes</strong>: ${model.supportNodeCount}</div>` +
      `<div class="kv"><strong>Kinds</strong>: ${escapeHtml(kindSummary)}</div>` +
      truncationLine
    );
  }

  return (
    `<div class="kv"><strong>Query</strong>: ${escapeHtml(model.query)}</div>` +
    `<div class="kv"><strong>Matches</strong>: ${model.shownMatches}/${model.totalMatches}${state.searchTruncated ? "+" : ""}</div>` +
    `<div class="kv"><strong>Diagram</strong>: ${model.selectedNodes.length} nodes, ${model.selectedEdges.length}${model.edgeTruncated ? "+" : ""} edges</div>` +
    `<div class="kv"><strong>Context Nodes</strong>: ${model.supportNodeCount}</div>` +
    `<div class="kv"><strong>Kinds</strong>: ${escapeHtml(kindSummary)}</div>` +
    `<div class="kv"><strong>Top Nodes</strong>: ${escapeHtml(hotspots || "n/a")}</div>` +
    truncationLine
  );
}

function buildSearchMermaidSource(model) {
  if (!model || model.selectedNodes.length === 0) {
    return {
      source: "flowchart LR\n  no_result[\"No search matches\"]",
      aliasByGraphNodeId: new Map(),
      graphNodeIdByAlias: new Map(),
    };
  }

  const nodeIdMap = new Map();
  const aliasByGraphNodeId = new Map();
  const graphNodeIdByAlias = new Map();
  const lines = ["flowchart LR"];
  model.selectedNodes.forEach((node, index) => {
    const mermaidId = `n${index + 1}`;
    nodeIdMap.set(node.id, mermaidId);
    aliasByGraphNodeId.set(node.id, mermaidId);
    graphNodeIdByAlias.set(mermaidId, node.id);
    const label = escapeMermaidLabel(
      truncateNodeLabel(insightsNodeDisplayLabel(node, model.mode || "search"), 92)
    );
    lines.push(`  ${mermaidId}["${label}"]`);
  });

  if (model.selectedEdges.length === 0) {
    lines.push("  %% No connected edges in selected search context");
  } else {
    for (const edge of model.selectedEdges) {
      const fromId = nodeIdMap.get(edge.from);
      const toId = nodeIdMap.get(edge.to);
      if (!fromId || !toId) continue;
      const edgeLabel = escapeMermaidLabel(edge.kind);
      lines.push(`  ${fromId} -->|${edgeLabel}| ${toId}`);
    }
  }

  if (model.nodeTruncated || model.edgeTruncated) {
    lines.push("  %% truncated for readability");
  }
  return {
    source: lines.join("\n"),
    aliasByGraphNodeId,
    graphNodeIdByAlias,
  };
}

async function renderSearchMermaidDiagram(source) {
  const smallContainer = el("search-mermaid-diagram");
  const largeContainer = el("search-mermaid-diagram-large");
  const modalContainer = el("search-mermaid-diagram-modal");
  if (!smallContainer && !largeContainer && !modalContainer) return;

  const token = state.mermaidRenderToken + 1;
  state.mermaidRenderToken = token;
  if (smallContainer) {
    smallContainer.innerHTML = `<div class="search-insights-muted">Rendering Mermaid...</div>`;
  }
  if (largeContainer) {
    largeContainer.innerHTML = `<div class="search-insights-muted">Rendering Mermaid...</div>`;
  }
  if (modalContainer && state.mermaidModalOpen) {
    modalContainer.innerHTML = `<div class="search-insights-muted">Rendering Mermaid...</div>`;
  }

  if (!window.mermaid || typeof window.mermaid.render !== "function") {
    const unavailableMsg =
      `<div class="search-insights-muted">Mermaid library unavailable. Use the Mermaid source block above.</div>`;
    if (smallContainer) smallContainer.innerHTML = unavailableMsg;
    if (largeContainer) largeContainer.innerHTML = unavailableMsg;
    if (modalContainer) modalContainer.innerHTML = unavailableMsg;
    applyMermaidZoom();
    applyPendingMermaidFocusSync(token);
    return;
  }

  try {
    window.mermaid.initialize({
      startOnLoad: false,
      securityLevel: "loose",
      theme: state.theme === "dark" ? "dark" : "default",
      flowchart: {
        useMaxWidth: true,
        htmlLabels: false,
        curve: "basis",
      },
    });

    const renderedSmallPromise = smallContainer
      ? window.mermaid.render(`search_mermaid_small_${token}`, source)
      : Promise.resolve(null);
    const renderedLargePromise = largeContainer
      ? window.mermaid.render(`search_mermaid_large_${token}`, source)
      : Promise.resolve(null);
    const renderedModalPromise = modalContainer && state.mermaidModalOpen
      ? window.mermaid.render(`search_mermaid_modal_${token}`, source)
      : Promise.resolve(null);

    const [renderedSmall, renderedLarge, renderedModal] = await Promise.all([
      renderedSmallPromise,
      renderedLargePromise,
      renderedModalPromise,
    ]);
    if (token !== state.mermaidRenderToken) return;

    const svgSmall = renderedSmall
      ? (typeof renderedSmall === "string" ? renderedSmall : renderedSmall?.svg)
      : null;
    const svgLarge = renderedLarge
      ? (typeof renderedLarge === "string" ? renderedLarge : renderedLarge?.svg)
      : null;
    const svgModal = renderedModal
      ? (typeof renderedModal === "string" ? renderedModal : renderedModal?.svg)
      : null;

    if ((smallContainer && !svgSmall) || (largeContainer && !svgLarge)) {
      throw new Error("Mermaid render returned no SVG");
    }
    if (smallContainer) {
      smallContainer.innerHTML = svgSmall;
      const smallSvg = smallContainer.querySelector("svg");
      if (smallSvg) {
        smallSvg.style.maxWidth = "100%";
        smallSvg.style.width = "100%";
        smallSvg.style.height = "auto";
      }
    }
    if (largeContainer) {
      largeContainer.innerHTML = svgLarge;
    }
    if (modalContainer) {
      if (state.mermaidModalOpen && svgModal) {
        modalContainer.innerHTML = svgModal;
      } else if (largeContainer) {
        modalContainer.innerHTML = largeContainer.innerHTML;
      } else if (svgSmall) {
        modalContainer.innerHTML = svgSmall;
      }
    }
    // Toggle the flow-animation hook based on the active slash
    // command. When it's `/flows`, CSS keyframes (.flow-anim
    // declared in styles.css) animate every flowchart edge path's
    // `stroke-dashoffset` so the arrows visibly stream toward the
    // target. Other modes get a clean static diagram.
    const flowAnimOn = state.commandMode === "flows";
    [smallContainer, largeContainer, modalContainer].forEach((c) => {
      if (!c) return;
      c.classList.toggle("flow-anim", flowAnimOn);
    });
    applyMermaidZoom();
    applyPendingMermaidFocusSync(token);
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    const renderError =
      `<div class="search-insights-muted">Mermaid render failed: ${escapeHtml(message)}</div>`;
    if (smallContainer) smallContainer.innerHTML = renderError;
    if (largeContainer) largeContainer.innerHTML = renderError;
    if (modalContainer) modalContainer.innerHTML = renderError;
    applyMermaidZoom();
    applyPendingMermaidFocusSync(token);
  }
}

function renderSearchInsights(slice, matchedIds) {
  const query = state.searchQuery.trim();
  const summary = el("search-insights-summary");
  const source = el("search-mermaid-source");
  const copyButton = el("copy-search-mermaid");
  let model = null;
  const modePreference = normalizeMermaidSourceMode(state.mermaidSourceMode);

  const buildAutoModel = () => {
    // Prefer the focus model whenever a specific node is selected —
    // even with an active search query. The user explicitly drilled
    // into one node and almost always wants its full local subgraph
    // (30 outgoing calls, etc.), not a survey of every search match
    // with shared neighbour slots. Search model wins only when no
    // selection exists.
    if (state.selectedNodeId && !state.impactActive) {
      const focusModel = buildFocusInsightsModel(slice, state.selectedNodeId);
      if (focusModel) {
        setInsightsTitle("focus");
        return focusModel;
      }
    }
    if (query.length > 0) {
      const searchModel = buildSearchInsightsModel(slice, matchedIds);
      if (searchModel) {
        setInsightsTitle("search");
        return searchModel;
      }
    }
    if (state.impactActive) {
      if (state.selectedNodeId) {
        const focusModel = buildFocusInsightsModel(slice, state.selectedNodeId);
        if (focusModel) {
          setInsightsTitle("focus");
          return focusModel;
        }
      }
      const impactModel = buildImpactInsightsModel(slice, matchedIds);
      if (impactModel) {
        setInsightsTitle("impact");
        return impactModel;
      }
    }
    if (!state.impactActive && !state.blastLensActive && state.flowLastView && state.flowLastResult) {
      const flowModel = buildFlowInsightsModel(slice);
      if (flowModel) {
        setInsightsTitle("flow");
        return flowModel;
      }
    }
    return null;
  };

  const buildFlowPreferredModel = () => {
    if (!state.impactActive && !state.blastLensActive && state.flowLastView && state.flowLastResult) {
      const flowModel = buildFlowInsightsModel(slice);
      if (flowModel) {
        setInsightsTitle("flow");
        return flowModel;
      }
    }
    return buildAutoModel();
  };

  const buildSearchPreferredModel = () => {
    if (query.length > 0) {
      const searchModel = buildSearchInsightsModel(slice, matchedIds);
      if (searchModel) {
        setInsightsTitle("search");
        return searchModel;
      }
    }
    return buildAutoModel();
  };

  if (modePreference === "flow") {
    model = buildFlowPreferredModel();
  } else if (modePreference === "search") {
    model = buildSearchPreferredModel();
  } else {
    model = buildAutoModel();
  }

  if (!model) {
    setInsightsTitle("search");
    hideSearchInsights();
    return;
  }
  state.activeInsightsMode = String(model.mode || "search");

  revealSearchInsights();
  const mermaidBundle = buildSearchMermaidSource(model);
  const mermaidSource = mermaidBundle.source;
  state.mermaidAliasByGraphNodeId = mermaidBundle.aliasByGraphNodeId;
  state.mermaidGraphNodeIdByAlias = mermaidBundle.graphNodeIdByAlias;

  if (summary) {
    summary.innerHTML = buildSearchInsightsSummary(model);
  }
  if (source) {
    source.textContent = mermaidSource;
  }
  state.latestMermaidSource = mermaidSource;
  if (copyButton) {
    copyButton.disabled = mermaidSource.length === 0;
    copyButton.textContent = "Copy Mermaid";
  }

  if (state.mermaidSplitOpen) {
    setMermaidSplitOpen(true, { syncUrl: false });
  }
  if (state.mermaidModalOpen) {
    setMermaidModalOpen(true, { syncUrl: false });
  }

  void renderSearchMermaidDiagram(mermaidSource);
}

function clearScheduledInsightsRefresh() {
  if (state.insightsRefreshTimeoutHandle !== null) {
    window.clearTimeout(state.insightsRefreshTimeoutHandle);
    state.insightsRefreshTimeoutHandle = null;
  }
  if (state.insightsRefreshIdleHandle !== null) {
    if (typeof window.cancelIdleCallback === "function") {
      window.cancelIdleCallback(state.insightsRefreshIdleHandle);
    } else {
      window.clearTimeout(state.insightsRefreshIdleHandle);
    }
    state.insightsRefreshIdleHandle = null;
  }
}

// Lightweight interaction tracer: slow phases (>80ms) surface in a
// corner overlay so perf problems are diagnosable from a real user
// session instead of guesswork.
function perfMark(label, ms) {
  if (!Number.isFinite(ms) || ms < 80) return;
  let overlay = document.getElementById("perf-trace");
  if (!overlay) {
    overlay = document.createElement("div");
    overlay.id = "perf-trace";
    overlay.title = "Slow interaction phases — also logged to the console";
    overlay.style.cssText =
      "position:fixed;left:10px;bottom:34px;z-index:9999;font:11px ui-monospace,monospace;" +
      "color:#e8b339;background:rgba(10,14,22,.85);border:1px solid rgba(232,179,57,.4);" +
      "border-radius:6px;padding:4px 8px;max-width:70vw;white-space:pre;" +
      "user-select:text;-webkit-user-select:text;cursor:text;";
    document.body.appendChild(overlay);
  }
  const lines = (overlay.textContent ? overlay.textContent.split("\n") : []);
  const line = `${label} ${Math.round(ms)}ms`;
  lines.unshift(line);
  overlay.textContent = lines.slice(0, 4).join("\n");
  // Console mirror: copyable from devtools even when the overlay is awkward.
  console.log(`[perf] ${line}`);
}

function reportTapTrace(trace) {
  if (!trace || !trace.render) return;
  const seg = (a, b) => (trace[b] && trace[a] !== undefined ? trace[b] - trace[a] : 0);
  const total = trace.render - trace.start;
  perfMark(
    `tap total ${total} · focus ${seg("pre", "focus")} details ${seg("focus", "details")} ` +
    `url ${seg("details", "url")} beforeSet ${seg("url", "beforeSet")} ` +
    `afterSet ${seg("beforeSet", "afterSet")} ctx ${seg("afterSet", "context")} ` +
    `render ${seg("context", "render")}`,
    total
  );
}

function refreshInsightsForCurrentView(options = {}) {
  if (state.deferInsightsActive) return false;
  if (!state.graph || !state.graphIndex) return false;
  const t0 = performance.now();
  // Reuse the slice renderGraph just computed when the mode matches —
  // recomputing walks every edge (hundreds of ms on the full mount) on
  // every click.
  const slice =
    state.lastSlice && state.lastSliceMode === state.renderMode
      ? state.lastSlice
      : graphSliceForMode(state.graph, state.renderMode);
  const t1 = performance.now();
  const matchedIds = slice.matchedNodeIds || new Set();
  renderSearchInsights(slice, matchedIds);
  const t2 = performance.now();
  if (options.applyGraphFocus !== false) {
    applyGraphMermaidFocus({ zoomGraph: false });
  }
  const t3 = performance.now();
  perfMark(`insights slice ${t1 - t0} model ${t2 - t1} focus ${t3 - t2}`, t3 - t0);
  return true;
}

function runScheduledInsightsRefresh(token, options = {}) {
  if (token !== state.insightsRefreshToken) return;
  state.insightsRefreshIdleHandle = null;
  refreshInsightsForCurrentView({
    applyGraphFocus: options.applyGraphFocus !== false,
  });
}

function scheduleInsightsRefresh(options = {}) {
  if (!state.graph || !state.graphIndex) return;
  if (state.deferInsightsActive) return;
  const immediate = options.immediate === true;
  const preferIdle = options.preferIdle !== false;
  const requestedDebounce = Number(options.debounceMs);
  const debounceMs = Number.isFinite(requestedDebounce)
    ? Math.max(0, Math.round(requestedDebounce))
    : INSIGHTS_DEBOUNCE_MS;
  const token = state.insightsRefreshToken + 1;
  state.insightsRefreshToken = token;

  clearScheduledInsightsRefresh();
  if (immediate) {
    runScheduledInsightsRefresh(token, options);
    return;
  }

  state.insightsRefreshTimeoutHandle = window.setTimeout(() => {
    state.insightsRefreshTimeoutHandle = null;
    if (token !== state.insightsRefreshToken) return;

    if (preferIdle) {
      if (typeof window.requestIdleCallback === "function") {
        state.insightsRefreshIdleHandle = window.requestIdleCallback(
          () => {
            runScheduledInsightsRefresh(token, options);
          },
          { timeout: INSIGHTS_IDLE_TIMEOUT_MS }
        );
      } else {
        state.insightsRefreshIdleHandle = window.setTimeout(() => {
          runScheduledInsightsRefresh(token, options);
        }, 0);
      }
      return;
    }

    runScheduledInsightsRefresh(token, options);
  }, debounceMs);
}

function applyKindFilter(kind, options = {}) {
  const syncUrl = options.syncUrl !== false;
  const cy = state.cy;
  const select = el("kind-filter");
  let nextKind = normalizeKindFilter(kind);

  if (select) {
    const availableKinds = new Set(Array.from(select.options, (option) => option.value));
    if (!availableKinds.has(nextKind)) {
      nextKind = "all";
    }
    if (select.value !== nextKind) {
      select.value = nextKind;
    }
  }

  state.kindFilter = nextKind;
  if (!cy) {
    if (syncUrl) {
      writeUrlViewState();
    }
    return;
  }

  if (nextKind === "all") {
    cy.nodes().removeClass("kind-filtered");
    if (syncUrl) {
      writeUrlViewState();
    }
    return;
  }

  cy.nodes().forEach((n) => {
    if (n.data("kind") === nextKind) {
      n.removeClass("kind-filtered");
    } else {
      n.addClass("kind-filtered");
    }
  });

  if (syncUrl) {
    writeUrlViewState();
  }
}

// ── Slash-command handling (/how, /capabilities, /endpoints, /detail) ──
// Note: All user-controlled strings are escaped via the codebase's existing
// escapeHtml() sanitizer before DOM insertion, preventing XSS.

// Built-in commands are baked in. Extension-registered commands are
// matched by the more lenient regex (any [a-z-] word after the slash)
// and then resolved against the cached extension manifest.
const COMMAND_RE = /^\/([a-zA-Z][a-zA-Z0-9-]*)\s*(.*)/i;

// ── Extension-registered slash commands ─────────────────────────────
// Fetched lazily on first slash so the cache stays warm. Each entry:
// { cmd, arg, description, kinds, http_route, arg_param, summary }
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
      return all;
    })
    .catch(() => {
      _extSlashCache = [];
      return [];
    });
  return _extSlashLoading;
}
function lookupExtSlashCommand(cmdWithSlash) {
  if (!_extSlashCache) return null;
  return _extSlashCache.find((s) => s.cmd === cmdWithSlash) || null;
}
// Prime the cache early so the synchronous lookupExtSlashCommand
// in fetchCommandResults works after the first user keystroke.
_fetchExtSlashCommands();

async function fetchCommandResults(mode, arg) {
  // Analyzer endpoints get dedicated routes; the older command set
  // still goes through /api/query.
  let url;
  if (mode === "dead-code") {
    const p = new URLSearchParams({ limit: "200" });
    if (arg && arg.trim()) p.set("crate", arg.trim());
    url = "/api/dead-code?" + p.toString();
  } else if (mode === "coverage") {
    const p = new URLSearchParams({ limit: "200" });
    if (arg && arg.trim()) p.set("crate", arg.trim());
    url = "/api/test-coverage?" + p.toString();
  } else if (mode === "flows") {
    const target = (arg || "").trim();
    if (!target) return { error: "Pass a node name: /flows <target>" };
    // API path targets (starting with `/`) get a bidirectional walk
    // so the user sees frontend callers → endpoint → handler → its
    // calls all in one flow diagram.
    // Always trace BOTH upstream callers and downstream callees so the
    // user sees the full local subgraph regardless of whether they
    // passed an API path or a bare symbol name. The previous "in" only
    // default hid the 30 outgoing calls of a function target — same
    // function name run as /flows showed only callers while
    // /flows on the route serving it showed both.
    const direction = "both";
    // 5-hop BFS: deep enough to surface non-trivial call chains
    // (e.g. handler → service → repo → adapter → stdlib leaf) without
    // overwhelming the diagram. Server-side `find_flows.py` still
    // caps total nodes via its `limit` parameter, so this just opens
    // up the traversal depth — actual breadth is bounded.
    const p = new URLSearchParams({ target, hops: "5", direction });
    url = "/api/flows?" + p.toString();
  } else {
    // Core /how /detail /endpoints /capabilities all funnel through
    // /api/query. Anything else is treated as extension-registered
    // (looked up via the cached /api/extensions response).
    const builtin = new Set(["how", "detail", "endpoints", "capabilities"]);
    if (builtin.has(mode)) {
      const params = new URLSearchParams({ mode });
      if (arg) params.set("q", arg.trim());
      url = "/api/query?" + params.toString();
    } else {
      const extSpec = lookupExtSlashCommand("/" + mode);
      if (extSpec) {
        const p = new URLSearchParams();
        const t = (arg || "").trim();
        if (t && extSpec.arg_param) p.set(extSpec.arg_param, t);
        p.set("limit", "300");
        url = extSpec.http_route + "?" + p.toString();
      } else {
        return { error: "Unknown command: /" + mode };
      }
    }
  }
  try {
    const resp = await fetch(url);
    if (!resp.ok) return { error: "Server returned " + resp.status };
    return await resp.json();
  } catch (err) {
    return { error: String(err) };
  }
}

function buildCommandInsightsSummary(mode, data) {
  if (data.error) {
    return '<div class="search-insights-muted">' + escapeHtml(String(data.error)) + "</div>";
  }
  var html = "";
  var kv = function(label, value) {
    return '<div class="kv"><strong>' + escapeHtml(String(label)) + "</strong>: " + escapeHtml(String(value)) + "</div>";
  };
  var kvRaw = function(label, valueHtml) {
    // valueHtml must already be escaped by caller
    return '<div class="kv"><strong>' + escapeHtml(String(label)) + "</strong>: " + valueHtml + "</div>";
  };
  var indent = function(text) {
    return '<div class="kv" style="padding-left:1em">' + escapeHtml(String(text)) + "</div>";
  };

  if (mode === "how" && data.mode === "blueprint") {
    var files = (data.implementor || {}).files_to_create || [];
    var fns = (data.implementor || {}).functions_to_implement || [];
    var classes = (data.implementor || {}).classes_or_structs || [];
    var sdkIfaces = ((data.sdk_dependency || {}).all_interfaces || []).slice(0, 10);
    var params = data.parameters || [];
    html += kv("Mode", "Capability Blueprint");
    html += kv("Reference", data.reference_capability || "");
    html += kv("Type", data.implementation_type || "");
    if (params.length) html += kv("Parameters", params.join(", "));
    if (files.length) {
      html += '<div class="kv"><strong>Files to create</strong>:</div>';
      files.forEach(function(f) { html += indent(f.name + " \u2014 " + f.path); });
    }
    if (fns.length) html += kv("Functions", fns.slice(0, 12).join(", ") + (fns.length > 12 ? " +" + (fns.length - 12) + " more" : ""));
    if (classes.length) html += kv("Classes/Structs", classes.join(", "));
    if (sdkIfaces.length) html += kv("SDK Interfaces", sdkIfaces.join(", "));
    if (data.yaml_template) html += kv("YAML template", data.yaml_template.reference_file || "");
    return html;
  }

  if (mode === "how") {
    // Architecture explain mode
    var summary = data.summary || {};
    var keyTypes = data.key_types || [];
    var primaryFiles = data.primary_files || [];
    var endpoints = data.endpoints || [];
    var usedBy = data.used_by || [];
    var contracts = data.contracts || {};
    var crateDist = data.crate_distribution || {};
    html += kv("Mode", "Architecture Explain");
    html += kv("Topic", data.topic || "");
    var scopeParts = Object.entries(summary).map(function(e) { return e[1] + " " + e[0] + (e[1] !== 1 ? "s" : ""); });
    html += kv("Scope", scopeParts.join(", "));
    var crateNames = Object.keys(crateDist);
    if (crateNames.length) html += kv("Crates", crateNames.join(", "));
    if (keyTypes.length) {
      html += '<div class="kv"><strong>Key types</strong>:</div>';
      keyTypes.slice(0, 10).forEach(function(t) { html += indent(t.kind + " " + t.label); });
      if (keyTypes.length > 10) html += indent("+" + (keyTypes.length - 10) + " more");
    }
    if (primaryFiles.length) {
      html += '<div class="kv"><strong>Primary files</strong>:</div>';
      primaryFiles.slice(0, 6).forEach(function(f) { html += indent(f.path.split("/").pop() + " (" + f.function_count + " fns)"); });
    }
    if (endpoints.length) {
      html += '<div class="kv"><strong>Endpoints</strong>:</div>';
      endpoints.forEach(function(e) { html += indent(e.method + " " + e.route); });
    }
    if (usedBy.length) html += kv("Used by", usedBy.slice(0, 6).map(function(c) { return c.label; }).join(", "));
    var contractSections = Object.keys(contracts);
    if (contractSections.length) {
      html += '<div class="kv"><strong>Contracts</strong>:</div>';
      contractSections.forEach(function(sec) { html += indent(sec + ": " + contracts[sec].length + " items"); });
    }
    return html;
  }

  if (mode === "capabilities") {
    html += kv("Total", data.total + " capabilities");
    html += kv("Resolved", String(data.resolved));
    html += kv("Unresolved", String(data.unresolved));
    (data.capabilities || []).forEach(function(cap) {
      var dot = cap.resolved ? "\u2705" : "\u26aa";
      var eng = cap.engine ? (cap.engine.label || "") : "";
      var art = cap.artifact ? (cap.artifact.label || "") : (cap.artifact_kind || cap.implementation_type);
      var line = eng ? (eng + " \u2192 " + art) : art;
      html += indent(dot + " " + cap.name + " \u2192 " + line);
    });
    return html;
  }

  if (mode === "endpoints") {
    html += kv("Endpoints", String(data.endpoint_count));
    (data.endpoints || []).forEach(function(ep) {
      var handler = ep.handler ? (ep.handler.name || "?") : "?";
      var callerCount = (ep.called_by || []).length;
      var text = ep.method + " " + ep.route + " \u2192 " + handler;
      if (callerCount > 0) text += " (" + callerCount + " callers)";
      html += indent(text);
    });
    return html;
  }

  if (mode === "detail") {
    var pFiles = data.files || [];
    var pSymbols = data.symbols || {};
    html += kv("Crate", data.crate || "");
    var sParts = Object.entries(data.summary || {}).map(function(e) { return e[1] + " " + e[0]; });
    html += kv("Contents", sParts.join(", "));
    if (pFiles.length) html += kv("Files", pFiles.join(", "));
    Object.entries(pSymbols).forEach(function(entry) {
      var kind = entry[0], names = entry[1];
      if (names.length) html += kv(kind, names.slice(0, 10).join(", ") + (names.length > 10 ? " +" + (names.length - 10) : ""));
    });
    var outbound = (data.cross_crate_edges || []).filter(function(e) { return e.direction === "outbound"; });
    var inbound = (data.cross_crate_edges || []).filter(function(e) { return e.direction === "inbound"; });
    if (outbound.length) html += kv("Outbound", outbound.length + " cross-crate connections");
    if (inbound.length) html += kv("Inbound", inbound.length + " cross-crate connections");
    return html;
  }

  if (mode === "dead-code") {
    var totals = data.totals || {};
    var nc = data.near_certain || 0;
    html += kv("Tier A (private, no prod callers)", String(totals.A || 0));
    html += kv("Tier C (public, no prod callers)", String(totals.C || 0));
    html += kv("★ occ=1 (name appears only at definition)", String(nc));
    var cands = data.candidates || {};
    var renderList = function(label, list) {
      if (!list || !list.length) return;
      var rows = list.slice(0, 30).map(function(c) {
        var star = c.occurrences <= 1 ? "★ " : "  ";
        return star + c.name + "  (" + c.path + ":" + c.line + ")  occ=" + c.occurrences + " tcalls=" + c.test_callers;
      }).join("\n");
      html += kvRaw(label, '<pre class="cmd-pre">' + escapeHtml(rows) + "</pre>");
    };
    if (Array.isArray(cands)) {
      renderList("Candidates", cands);
    } else {
      renderList("Tier A", cands.A);
      renderList("Tier C", cands.C);
    }
    return html;
  }

  // Extension-registered command? Render from its declarative
  // `summary` spec — no per-mode branches needed.
  const extSpec = lookupExtSlashCommand("/" + mode);
  if (extSpec && extSpec.summary) {
    const sum = extSpec.summary;
    (sum.header_fields || []).forEach(function(f) {
      const v = data[f.from];
      if (v === undefined || v === null) return;
      if (f.format === "kv" && typeof v === "object") {
        Object.keys(v).sort().forEach(function(k) {
          html += kv("  " + k, String(v[k]));
        });
        html += kv(f.label, "—");
      } else {
        html += kv(f.label, typeof v === "object" ? JSON.stringify(v) : String(v));
      }
    });
    const list = sum.list_field ? (data[sum.list_field] || []) : [];
    if (list.length) {
      const template = sum.row_template || "{label}";
      const rows = list.slice(0, 80).map(function(item) {
        return template.replace(/\{(\w+)\}/g, function(_, key) {
          const v = item[key];
          return (v === undefined || v === null) ? "?" : String(v);
        });
      }).join("\n");
      html += kvRaw(sum.list_field, '<pre class="cmd-pre">' + escapeHtml(rows) + "</pre>");
    }
    return html;
  }

  if (mode === "flows") {
    var t = data.target || {};
    html += kv("Target", (t.label || "?") + "  ·  " + (t.kind || "?"));
    if (t.path) html += kv("Source", t.path + (t.line ? ":" + t.line : ""));
    html += kv("Direction", data.direction === "out" ? "outgoing (downstream)" : "incoming (upstream)");
    var ft = data.totals || {};
    var ftNodes = (ft.nodes_returned != null ? ft.nodes_returned : ft.nodes) || 0;
    var ftEdges = (ft.edges_returned != null ? ft.edges_returned : ft.edges) || 0;
    var ftReach = ft.nodes_reachable != null ? ft.nodes_reachable : ftNodes;
    html += kv("Reach", ftNodes + " nodes · " + ftEdges + " edges" + (ftReach > ftNodes ? "  (of " + ftReach + " reachable)" : ""));
    html += kv("Per-hop layers", (data.layer_counts || []).join(" · ") || "—");
    // Per-hop node list — sorted ascending by hop, then label.
    var groups = {};
    (data.nodes || []).forEach(function(n) {
      if (n.id === t.id) return;
      var h = n.hop || 0;
      (groups[h] = groups[h] || []).push(n);
    });
    Object.keys(groups).sort(function(a, b) { return Number(a) - Number(b); }).forEach(function(h) {
      var rows = groups[h].map(function(n) {
        return "  " + (n.label || "?") + "  (" + (n.path || "?") + (n.line ? ":" + n.line : "") + ")  [" + (n.kind || "?") + "]";
      }).join("\n");
      html += kvRaw("Hop " + h + " — " + groups[h].length, '<pre class="cmd-pre">' + escapeHtml(rows) + "</pre>");
    });
    return html;
  }

  if (mode === "coverage") {
    var ct = data.totals || {};
    html += kv("Untested (real gap)", String(ct.untested || 0));
    html += kv("Light (1 test caller)", String(ct.light || 0));
    html += kv("Moderate (2–5)", String(ct.moderate || 0));
    html += kv("Well (6+)", String(ct.well || 0));
    html += kv("Only-tested (no prod callers)", String(ct.only_tested || 0));
    var fns = data.functions || {};
    var renderBucket = function(label, list) {
      if (!list || !list.length) return;
      var rows = list.slice(0, 30).map(function(f) {
        return f.name + "  (" + f.path + ":" + f.line + ")  prod=" + f.prod_callers + " tcalls=" + f.test_callers;
      }).join("\n");
      html += kvRaw(label, '<pre class="cmd-pre">' + escapeHtml(rows) + "</pre>");
    };
    if (Array.isArray(fns)) {
      renderBucket("Functions", fns);
    } else {
      renderBucket("Untested", fns.untested);
      renderBucket("Light", fns.light);
      renderBucket("Moderate", fns.moderate);
      renderBucket("Well", fns.well);
      renderBucket("Only-tested", fns.only_tested);
    }
    return html;
  }

  return '<div class="search-insights-muted">Unknown command mode.</div>';
}

function buildCommandMermaidSource(mode, data) {
  // The state slots that consume these (state.mermaidAliasByGraphNodeId,
  // state.mermaidGraphNodeIdByAlias) are Maps and call `.get(alias)` on
  // them — see bindMermaidContainerClickInteractions. Return Maps here
  // (not plain objects) so that contract holds.
  if (data.error) return { source: "", aliasByGraphNodeId: new Map(), graphNodeIdByAlias: new Map() };
  var lines = ["flowchart LR"];
  var idx = 0;
  function nid() { return "n" + (++idx); }
  function safe(label) { return String(label || "?").replace(/"/g, "'").substring(0, 60); }

  if (mode === "how" && data.mode === "blueprint") {
    var capId = nid();
    lines.push("  " + capId + '["\u2b50 ' + safe(data.reference_capability) + '"]');
    ((data.implementor || {}).files_to_create || []).forEach(function(f) {
      var fid = nid();
      lines.push("  " + fid + '["\ud83d\udcc4 ' + safe(f.name) + '"]');
      lines.push("  " + capId + " -->|implements| " + fid);
    });
    if (data.sdk_dependency) {
      var sdkId = nid();
      lines.push("  " + sdkId + '["\ud83d\udce6 bot-sdk"]');
      lines.push("  " + capId + " -.->|depends on| " + sdkId);
    }
  } else if (mode === "how") {
    var topicId = nid();
    lines.push("  " + topicId + '(["' + safe(data.topic) + '"])');
    (data.key_types || []).slice(0, 12).forEach(function(t) {
      var tid = nid();
      lines.push("  " + tid + '["' + safe(t.kind + ": " + t.label) + '"]');
      lines.push("  " + topicId + " --> " + tid);
    });
    (data.endpoints || []).forEach(function(ep) {
      var eid = nid();
      lines.push("  " + eid + '["' + safe(ep.method + " " + ep.route) + '"]');
      lines.push("  " + topicId + " --> " + eid);
    });
  } else if (mode === "capabilities") {
    (data.capabilities || []).slice(0, 30).forEach(function(cap) {
      var cid = nid();
      lines.push("  " + cid + '["' + safe(cap.name) + '"]');
      if (cap.implementor) {
        var iid = nid();
        lines.push("  " + iid + '["' + safe(cap.implementor.label) + '"]');
        lines.push("  " + cid + " -->|handles| " + iid);
      }
    });
  } else if (mode === "endpoints") {
    (data.endpoints || []).slice(0, 30).forEach(function(ep) {
      var eid = nid();
      lines.push("  " + eid + '["' + safe(ep.method + " " + ep.route) + '"]');
      if (ep.handler) {
        var hid = nid();
        lines.push("  " + hid + '["' + safe(ep.handler.name) + '"]');
        lines.push("  " + eid + " -->|handles| " + hid);
      }
    });
  } else if (mode === "detail") {
    var crateNodeId = nid();
    lines.push("  " + crateNodeId + '(["' + safe(data.crate) + '"])');
    (data.files || []).slice(0, 15).forEach(function(f) {
      var fid = nid();
      lines.push("  " + fid + '["' + safe(f) + '"]');
      lines.push("  " + crateNodeId + " --> " + fid);
    });
  } else if (mode === "flows") {
    // Pretty flow diagram for `/flows <target>`. Each node gets a
    // shape based on its kind (endpoints → stadium, functions →
    // rounded box, structs/enums/traits → hexagon, files → folder,
    // crates/modules → trapezoid). classDef colours fade with hop
    // distance; the target itself is the focal "anchor" node.
    // Edges become animated dashes via the post-render CSS we inject
    // — Mermaid itself can't animate, but the SVG it emits is fair
    // game for stroke-dashoffset keyframes.
    lines = ["flowchart LR"];
    var t = data.target || {};
    var idMap = {};
    var alias = function(graphId) {
      if (!idMap[graphId]) idMap[graphId] = "n" + (++idx);
      return idMap[graphId];
    };
    var shapeFor = function(kind, label, classKey) {
      label = safe(label);
      if (kind === "endpoint")    return '([' + JSON.stringify(label) + '])';
      if (kind === "function")    return '(' + JSON.stringify(label) + ')';
      if (kind === "api_call")    return '>"' + label + '"]';
      if (kind === "struct" || kind === "enum" || kind === "trait")
                                  return '{{"' + label + '"}}';
      if (kind === "file")        return '[/"' + label + '"/]';
      if (kind === "crate" || kind === "module")
                                  return '[\\"' + label + '"\\]';
      return '["' + label + '"]';
    };
    var classFor = function(node) {
      if (node.id === t.id) return "flow-target";
      var hop = Math.min(3, node.hop || 1);
      return "flow-hop-" + hop;
    };
    (data.nodes || []).forEach(function(n) {
      lines.push("  " + alias(n.id) + shapeFor(n.kind, n.label || n.id));
      lines.push("  class " + alias(n.id) + " " + classFor(n));
    });
    (data.edges || []).forEach(function(e) {
      // Always drawn `from -> to`; the post-render CSS animates the
      // dashes so the eye reads "flow" toward the arrow head.
      if (!idMap[e.from] || !idMap[e.to]) return;
      lines.push("  " + idMap[e.from] + " -.->|" + safe(e.kind) + "| " + idMap[e.to]);
    });
    // classDef declarations — colours coordinated with the
    // light-theme palette. The injected CSS upgrades these strokes
    // to animated dashes (see ensureFlowEdgeAnimation in app.js).
    lines.push("  classDef flow-target fill:#fff3f3,stroke:#b71c1c,stroke-width:2.5px,color:#3a0606,font-weight:bold");
    lines.push("  classDef flow-hop-1 fill:#e3eefc,stroke:#0a6cd0,stroke-width:1.5px,color:#0a2a55");
    lines.push("  classDef flow-hop-2 fill:#eef3fa,stroke:#3a6a8a,stroke-width:1.2px,color:#1a3a55");
    lines.push("  classDef flow-hop-3 fill:#f4f6f9,stroke:#6a7a8a,stroke-width:1px,color:#2a3a4a");
    // Convert idMap (plain object: graphNodeId -> mermaidAlias) into
    // the Map shape the rest of the code expects.
    var aliasByGraphNodeId = new Map();
    var graphNodeIdByAlias = new Map();
    Object.keys(idMap).forEach(function(k) {
      aliasByGraphNodeId.set(k, idMap[k]);
      graphNodeIdByAlias.set(idMap[k], k);
    });
    return {
      source: lines.join("\n"),
      aliasByGraphNodeId: aliasByGraphNodeId,
      graphNodeIdByAlias: graphNodeIdByAlias,
    };
  }
  return { source: lines.join("\n"), aliasByGraphNodeId: new Map(), graphNodeIdByAlias: new Map() };
}

// ── /flows visualization ─────────────────────────────────────────────
// Renders the flow subgraph as a beautified Mermaid diagram on top of
// the main cytoscape canvas (via an absolute-positioned overlay div).
// The same `.flow-anim` CSS class animates the SVG edges in place —
// no per-frame JS loop required, the keyframes do all the work.

function hideFlowCanvas() {
  const overlay = el("flow-canvas");
  if (overlay) overlay.hidden = true;
}

// Wire close button + Esc key once on first script init.
(function () {
  const setup = () => {
    const closeBtn = el("flow-canvas-close");
    if (closeBtn && !closeBtn.dataset.bound) {
      closeBtn.dataset.bound = "1";
      closeBtn.addEventListener("click", hideFlowCanvas);
    }
    if (!document.body || document.body.dataset.flowEscBound) return;
    document.body.dataset.flowEscBound = "1";
    document.addEventListener("keydown", (ev) => {
      if (ev.key === "Escape") {
        const overlay = el("flow-canvas");
        if (overlay && !overlay.hidden) hideFlowCanvas();
      }
    });
  };
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", setup);
  } else {
    setup();
  }
})();

async function renderFlowsInGraph(data) {
  const overlay = el("flow-canvas");
  const body = el("flow-canvas-body");
  const title = el("flow-canvas-title");
  if (!overlay || !body) return;
  if (!data || !data.nodes || !data.nodes.length) { hideFlowCanvas(); return; }

  // Target-only result = file/module/crate (no calls/handles/implements
  // edges land directly on container kinds). Surface a hint instead of
  // a lonely parallelogram so the user knows to try a symbol inside.
  const _t0 = data.target || {};
  const containerKinds = new Set(["file", "module", "crate"]);
  if (data.nodes.length === 1 && containerKinds.has(_t0.kind)) {
    if (title) title.textContent =
      `Flow · ${_t0.label || "?"} (no direct call edges)`;
    body.innerHTML =
      `<div class="flow-canvas-empty" style="max-width:520px;line-height:1.5">` +
      `<strong>${_t0.kind || "container"}s</strong> don't have direct call edges &mdash; ` +
      `flow tracing follows <code>calls / handles / implements / calls_api / targets_endpoint</code> ` +
      `relations, which live on functions, endpoints, and traits.<br><br>` +
      `Try <code>/flows &lt;function-name&gt;</code> for a symbol inside ` +
      `<strong>${_t0.label || _t0.kind || "this container"}</strong>, or use ` +
      `<code>/detail ${_t0.label || ""}</code> to see what it defines.` +
      `</div>`;
    overlay.hidden = false;
    return;
  }

  const t = data.target || {};
  const dirText = data.direction === "out" ? "outgoing" :
                  data.direction === "both" ? "end-to-end" : "incoming";
  const truncHint = data.truncated ? " · truncated (wheel/drag to navigate)" : "";
  // find_flows returns totals.nodes_returned / edges_returned; older
  // call sites read .nodes / .edges, so accept either spelling.
  const tt = data.totals || {};
  const ttNodes = tt.nodes_returned != null ? tt.nodes_returned : (tt.nodes || 0);
  const ttEdges = tt.edges_returned != null ? tt.edges_returned : (tt.edges || 0);
  if (title) title.textContent =
    `Flow · ${dirText} · ${t.label || "?"} (${ttNodes} nodes, ${ttEdges} edges)${truncHint}`;

  const bundle = buildCommandMermaidSource("flows", data);
  if (!bundle.source || !window.mermaid) {
    body.textContent = "Mermaid library unavailable.";
    overlay.hidden = false;
    return;
  }
  body.textContent = "Rendering flow…";
  overlay.hidden = false;
  try {
    window.mermaid.initialize({
      startOnLoad: false,
      securityLevel: "loose",
      theme: state.theme === "dark" ? "dark" : "default",
      flowchart: { useMaxWidth: true, htmlLabels: false, curve: "basis" },
    });
    const rendered = await window.mermaid.render(`flow_canvas_${Date.now()}`, bundle.source);
    const svg = typeof rendered === "string" ? rendered : rendered?.svg;
    body.innerHTML = svg || "<div class=\"flow-canvas-empty\">No diagram.</div>";
    // Wire wheel-zoom + drag-pan into this overlay (idempotent — the
    // pan-zoom binder no-ops if it already ran on this container).
    // applyMermaidZoom syncs the SVG width to the current shared zoom
    // level so the flow opens at the same scale as the split panel.
    bindMermaidInteractions();
    applyMermaidZoom();
  } catch (err) {
    body.textContent = "Mermaid render failed: " + String(err);
  }
}

async function handleSlashCommand(mode, arg) {
  state.commandMode = mode;
  state.commandResult = null;
  // Open the right panel on the Mermaid tab so results are visible
  setDetailsPanelTab("mermaid", { ensureOpen: true });
  setInsightsTitle("command");
  revealSearchInsights();
  var summaryEl = el("search-insights-summary");
  if (summaryEl) summaryEl.textContent = "Loading /" + mode + " " + (arg || "") + "\u2026";

  var data = await fetchCommandResults(mode, arg);
  state.commandResult = data;

  // `/flows` paints the returned subgraph into the main graph stage
  // as an animated Mermaid diagram (overlay on top of cytoscape).
  // Other commands hide that overlay if it was up from a prior run.
  if (mode === "flows" && !data.error) {
    void renderFlowsInGraph(data);
  } else {
    hideFlowCanvas();
  }
  if (summaryEl) {
    // buildCommandInsightsSummary only interpolates through escapeHtml()
    summaryEl.innerHTML = buildCommandInsightsSummary(mode, data);
  }
  var mermaidBundle = buildCommandMermaidSource(mode, data);
  var sourceEl = el("search-mermaid-source");
  if (sourceEl) sourceEl.textContent = mermaidBundle.source;
  state.latestMermaidSource = mermaidBundle.source;
  state.mermaidAliasByGraphNodeId = mermaidBundle.aliasByGraphNodeId;
  state.mermaidGraphNodeIdByAlias = mermaidBundle.graphNodeIdByAlias;
  var copyButton = el("copy-search-mermaid");
  if (copyButton) { copyButton.disabled = mermaidBundle.source.length === 0; copyButton.textContent = "Copy Mermaid"; }
  // For `/flows` auto-open the Mermaid split panel — the diagram is
  // the whole point of the command, and the tiny right-panel preview
  // doesn't do it justice. Other commands keep the split state as-is.
  if (mode === "flows" && canAutoOpenMermaidSplit()) {
    setMermaidSplitOpen(true, { syncUrl: false });
  } else if (state.mermaidSplitOpen) {
    setMermaidSplitOpen(true, { syncUrl: false });
  }
  if (state.mermaidModalOpen) setMermaidModalOpen(true, { syncUrl: false });
  if (mermaidBundle.source) void renderSearchMermaidDiagram(mermaidBundle.source);
}

function applySearch(query, options = {}) {
  const syncUrl = options.syncUrl !== false;
  const allowAutoSplit = options.allowAutoSplit !== false;
  const previousQueryActive = state.searchQuery.trim().length > 0;
  const nextQuery = String(query || "");
  if (nextQuery.trim().length > 0 && state.blastLensActive) {
    setBlastLensActive(false, { rerender: false });
    setBlastStatus("Blast lens disabled because search is active.", "warn");
  }
  if (nextQuery.trim().length > 0 && state.impactActive) {
    clearImpactAnalysis({ syncUrl: false });
    setImpactStatus("Impact mode disabled because search is active.", "warn");
  }
  if (state.searchQuery === nextQuery) return;
  state.searchQuery = nextQuery;

  // Detect slash commands before normal search path
  var cmdMatch = nextQuery.trim().match(COMMAND_RE);
  if (cmdMatch) {
    // Clear search state so graph shows default progressive view, not stale search results
    state.searchQuery = "";
    renderGraph(state.renderMode, { preserveViewport: true });
    void handleSlashCommand(cmdMatch[1].toLowerCase(), cmdMatch[2] || "");
    return;
  }
  // Clear any previous command state
  state.commandMode = null;
  state.commandResult = null;
  // Dismiss the /flows overlay if it's still up from an earlier slash
  // command — otherwise it sits opaquely on top of the cytoscape
  // canvas and the new text-search result the user just kicked off
  // is invisible underneath.
  try { hideFlowCanvas(); } catch (_) {}

  if (nextQuery.trim().length > 0 && state.fullLoadSession) {
    stopProgressiveFullLoad("Full load stopped: search was activated.", "warn");
  }
  if (nextQuery.trim().length > 0) {
    stopFlowPlayback();
    setExpansionStatusText("Search active: container expansion is paused; click symbols to expand callers/usages.", "warn");

    if (
      !previousQueryActive &&
      allowAutoSplit &&
      state.searchSplitAutoOpen &&
      !state.mermaidSplitOpen &&
      canAutoOpenMermaidSplit()
    ) {
      setMermaidSplitOpen(true, { syncUrl, persistSearchPreference: false });
    }
  } else {
    setExpansionStatusText("Search cleared. Click crate/module/file nodes to expand.", "muted");
    if (state.mermaidModalOpen) {
      setMermaidModalOpen(false, { syncUrl });
    }
    if (state.mermaidSplitOpen) {
      setMermaidSplitOpen(false, { syncUrl, persistSearchPreference: false });
    }
  }
  if (syncUrl) {
    writeUrlViewState();
  }
  renderGraph(state.renderMode, { preserveViewport: false, focusSearch: true });
}

function stopGlobalRepulsionLoop() {
  if (state.globalRepelFrameHandle !== null) {
    window.cancelAnimationFrame(state.globalRepelFrameHandle);
    state.globalRepelFrameHandle = null;
  }
  state.globalRepelSettleFrames = 0;
}

function recommendedGlobalRepelSettleFrames(extraFrames = 0) {
  const nodeCount = state.cy ? state.cy.nodes().length : 0;
  const stepCount = Math.ceil(nodeCount / 200);
  const dynamicFrames =
    GLOBAL_REPEL_SETTLE_FRAMES_BASE + stepCount * GLOBAL_REPEL_SETTLE_FRAMES_PER_200_NODES;
  const parsedExtra = Number(extraFrames);
  const extra = Number.isFinite(parsedExtra) ? parsedExtra : 0;
  const targetFrames = dynamicFrames + extra;
  return Math.max(
    GLOBAL_REPEL_SETTLE_FRAMES_BASE,
    Math.min(GLOBAL_REPEL_SETTLE_FRAMES_MAX, Math.round(targetFrames))
  );
}

function kickGlobalRepulsion(settleFrames = null) {
  const requestedFrames =
    settleFrames === null || settleFrames === undefined
      ? recommendedGlobalRepelSettleFrames()
      : Math.max(0, Math.round(Number(settleFrames) || 0));
  state.globalRepelSettleFrames = Math.max(state.globalRepelSettleFrames, requestedFrames);
  if (state.globalRepelFrameHandle !== null) {
    return;
  }
  state.globalRepelFrameHandle = window.requestAnimationFrame(runGlobalRepulsionTick);
}

function runGlobalRepulsionTick() {
  state.globalRepelFrameHandle = null;
  const cy = state.cy;
  if (!cy) return;

  const nodes = cy.nodes().toArray();
  if (nodes.length === 0) return;

  const hasGrabbedNode = nodes.some((node) => node.grabbed());
  const shouldRun = hasGrabbedNode || state.globalRepelSettleFrames > 0;
  if (!shouldRun) {
    return;
  }

  let maxShift = 0;
  if (nodes.length <= GLOBAL_REPEL_MAX_NODES) {
    maxShift = applyGlobalCollisionRepulsion(nodes);
  }

  if (!hasGrabbedNode && state.globalRepelSettleFrames > 0) {
    if (maxShift > GLOBAL_REPEL_CONTINUE_DELTA) {
      state.globalRepelSettleFrames = Math.max(
        state.globalRepelSettleFrames,
        GLOBAL_REPEL_CONTINUE_MIN_FRAMES
      );
    }
    state.globalRepelSettleFrames -= 1;
  }

  if (hasGrabbedNode || state.globalRepelSettleFrames > 0) {
    state.globalRepelFrameHandle = window.requestAnimationFrame(runGlobalRepulsionTick);
  }
}

function applyGlobalCollisionRepulsion(nodes) {
  const cellSize = GLOBAL_REPEL_RADIUS;
  const radiusSq = GLOBAL_REPEL_RADIUS * GLOBAL_REPEL_RADIUS;
  const indexById = new Map();
  const positions = new Map();
  const grabbed = new Set();
  const grid = new Map();

  nodes.forEach((node, index) => {
    const id = node.id();
    indexById.set(id, index);
    const pos = node.position();
    positions.set(id, { x: pos.x, y: pos.y });
    if (node.grabbed()) {
      grabbed.add(id);
    }
  });

  nodes.forEach((node) => {
    const id = node.id();
    const pos = positions.get(id);
    const cx = Math.floor(pos.x / cellSize);
    const cy = Math.floor(pos.y / cellSize);
    const key = `${cx},${cy}`;
    const bucket = grid.get(key);
    if (bucket) {
      bucket.push(node);
    } else {
      grid.set(key, [node]);
    }
  });

  const neighborOffsets = [
    [-1, -1], [-1, 0], [-1, 1],
    [0, -1], [0, 0], [0, 1],
    [1, -1], [1, 0], [1, 1],
  ];

  nodes.forEach((nodeA) => {
    const idA = nodeA.id();
    const posA = positions.get(idA);
    const aGrabbed = grabbed.has(idA);
    const cellX = Math.floor(posA.x / cellSize);
    const cellY = Math.floor(posA.y / cellSize);
    const indexA = indexById.get(idA);

    for (const [ox, oy] of neighborOffsets) {
      const bucket = grid.get(`${cellX + ox},${cellY + oy}`);
      if (!bucket) continue;

      for (const nodeB of bucket) {
        const idB = nodeB.id();
        const indexB = indexById.get(idB);
        if (indexB <= indexA) continue;

        const posB = positions.get(idB);
        const bGrabbed = grabbed.has(idB);
        if (aGrabbed && bGrabbed) continue;

        let dx = posB.x - posA.x;
        let dy = posB.y - posA.y;
        let distSq = dx * dx + dy * dy;
        if (distSq >= radiusSq) continue;

        if (distSq < 1e-6) {
          dx = (Math.random() - 0.5) * 0.01;
          dy = (Math.random() - 0.5) * 0.01;
          distSq = dx * dx + dy * dy;
        }

        const dist = Math.sqrt(distSq);
        const ux = dx / dist;
        const uy = dy / dist;
        const overlap = GLOBAL_REPEL_RADIUS - dist;
        const push = overlap * GLOBAL_REPEL_STRENGTH;

        if (aGrabbed) {
          posB.x += ux * push;
          posB.y += uy * push;
        } else if (bGrabbed) {
          posA.x -= ux * push;
          posA.y -= uy * push;
        } else {
          const halfPush = push * 0.5;
          posA.x -= ux * halfPush;
          posA.y -= uy * halfPush;
          posB.x += ux * halfPush;
          posB.y += uy * halfPush;
        }
      }
    }
  });

  let maxShift = 0;
  nodes.forEach((node) => {
    if (node.grabbed()) return;
    const id = node.id();
    const next = positions.get(id);
    const current = node.position();
    const deltaX = next.x - current.x;
    const deltaY = next.y - current.y;
    const shift = Math.hypot(deltaX, deltaY);
    if (shift < 0.01) {
      return;
    }
    if (shift > maxShift) {
      maxShift = shift;
    }
    node.position(next);
  });
  return maxShift;
}

function resetView() {
  const cy = state.cy;
  if (!cy) return;
  clearNodeTooltipTimers();
  hideNodeTooltip();
  if (cy.nodes().length > 20000) {
    // Large mounts: cy.fit() iterates every element (seconds at 100k+).
    // Fit mathematically from the cull index bounds instead — O(1).
    if (state.viewportCullIndex && state.viewportCullBounds) {
      const b = state.viewportCullBounds;
      const padding = currentFitPaddingProfile();
      const pad = padding.reset;
      const w = cy.width();
      const h = cy.height();
      const zoom = Math.max(
        cy.minZoom() || 1e-4,
        Math.min((w - 2 * pad) / Math.max(1, b.maxX - b.minX), (h - 2 * pad) / Math.max(1, b.maxY - b.minY))
      );
      cy.zoom(zoom);
      cy.center({ x: (b.minX + b.maxX) / 2, y: (b.minY + b.maxY) / 2 });
    }
    return;
  }
  cy.elements().removeClass("faded");
  const padding = currentFitPaddingProfile();
  cy.fit(cy.elements(), padding.reset);
}

function resetNodesToDefault() {
  if (state.searchDebounceHandle !== null) {
    window.clearTimeout(state.searchDebounceHandle);
    state.searchDebounceHandle = null;
  }

  if (!state.graph || !state.graphIndex) {
    if (window.location.search.length > 0) {
      window.location.href = window.location.pathname;
    }
    return;
  }

  stopProgressiveFullLoad("", "warn", { rerender: false });
  stopStagedReveal();
  stopImpactReveal({ emitStatus: false });
  stopGlobalRepulsionLoop();
  clearScheduledInsightsRefresh();
  clearViewportCullingSchedulers();
  clearViewportCullingClasses(state.cy);
  setTransitionLodActive(state.cy, false);
  state.viewportCullingActive = false;
  clearNodeTooltipTimers();
  hideNodeTooltip();

  clearAllProgressiveExpansions();
  state.blastLensActive = false;
  state.blastCenterNodeId = null;
  state.blastLastSummary = null;
  clearLiveImpactState();
  setBlastStatus("Blast lens off.", "muted");
  state.fullLoadIncludeAll = false;
  state.selectedNodeId = null;
  state.searchQuery = "";
  state.deferInsightsActive = false;
  state.flowLastResult = null;
  state.flowLastView = null;
  state.flowLastDelta = null;
  clearFlowGraphFocus();
  clearFlowHighlights();
  const flowOutput = el("flow-output");
  if (flowOutput) {
    flowOutput.textContent = "Run flow simulation to inspect traces and risks.";
  }
  const flowDeltaOutput = el("flow-delta-output");
  if (flowDeltaOutput) {
    flowDeltaOutput.textContent = "Run delta compare to view added/removed traces and risk deltas.";
  }
  setFlowTimelineMessage("Run flow simulation to view interleaving timelines.");
  renderFlowBlastRadius(null, null);

  const searchInput = el("search-input");
  if (searchInput) {
    searchInput.value = "";
  }

  clearImpactState({ rerender: false, syncUrl: false });
  state.kindFilter = "all";
  const kindFilter = el("kind-filter");
  if (kindFilter) {
    kindFilter.value = "all";
  }

  hideSearchInsights({ syncUrl: false });
  clearMermaidFocus();
  setDetails(null);

  if (window.history && typeof window.history.replaceState === "function") {
    window.history.replaceState(null, "", window.location.pathname);
  }

  renderGraph(chooseInitialRenderMode(state.graph), {
    preserveViewport: false,
    focusSearch: true,
  });
  applyKindFilter("all", { syncUrl: false });
  setExpansionStatusText("Reset to default view.", "muted");
  writeUrlViewState();
}

function setSettingsPanelOpen(nextOpen) {
  const panel = el("settings-panel");
  const overlay = el("settings-overlay");
  const openButton = el("open-settings");
  if (!panel || !overlay || !openButton) return;

  state.settingsPanelOpen = nextOpen === true;
  panel.classList.toggle("open", state.settingsPanelOpen);
  panel.setAttribute("aria-hidden", state.settingsPanelOpen ? "false" : "true");
  overlay.hidden = !state.settingsPanelOpen;
  openButton.setAttribute("aria-expanded", state.settingsPanelOpen ? "true" : "false");
}

function bindSettingsPanel() {
  const panel = el("settings-panel");
  const overlay = el("settings-overlay");
  const openButton = el("open-settings");
  const closeButton = el("close-settings");
  if (!panel || !overlay || !openButton || !closeButton) return;

  openButton.addEventListener("click", () => {
    setSettingsPanelOpen(true);
  });
  closeButton.addEventListener("click", () => {
    setSettingsPanelOpen(false);
  });
  overlay.addEventListener("click", () => {
    setSettingsPanelOpen(false);
  });
  window.addEventListener("keydown", (event) => {
    if (event.key === "Escape" && state.settingsPanelOpen) {
      setSettingsPanelOpen(false);
    }
  });
}

function runLayoutAndFit(cy, layoutOptions, fitOptions = {}) {
  if (!cy) return;

  const preserveViewport = fitOptions.preserveViewport === true;
  const focusSearch = fitOptions.focusSearch !== false;
  const matchedNodes = fitOptions.matchedNodes || cy.collection();
  const focusElements = fitOptions.focusElements || cy.collection();
  const fitMatched = focusSearch && matchedNodes.length > 0 && matchedNodes.length <= 180;
  const fitFocused = !focusSearch && focusElements.length > 0;
  const padding = currentFitPaddingProfile();

  const normalizedLayout = {
    ...(layoutOptions || { name: "preset" }),
    animate: false,
    fit: false,
  };

  const applyFit = () => {
    if (preserveViewport || state.cy !== cy) return;
    cy.resize();
    if (fitFocused) {
      cy.fit(focusElements, padding.focused);
      scheduleViewportCulling(cy, { force: true });
      return;
    }
    if (fitMatched) {
      // Expand the fit scope to the matched nodes' closed neighborhood
      // so the call/contains fan-out we pulled into the slice during
      // search actually fits on screen. Previously the fit zoomed
      // tight onto the matched nodes only, pushing the 30 outgoing
      // call targets of a function search off the viewport.
      const matchedScope = matchedNodes.closedNeighborhood().union(matchedNodes);
      cy.fit(matchedScope, padding.matched);
      scheduleViewportCulling(cy, { force: true });
      return;
    }
    const nodes = cy.nodes();
    if (nodes.length > 0) {
      cy.fit(nodes, padding.all);
    }
    scheduleViewportCulling(cy, { force: true });
  };

  const layout = cy.layout(normalizedLayout);
  layout.on("layoutstop", applyFit);
  layout.run();

  // Fallback fits in case the browser reports layout completion timing late.
  if (!preserveViewport) {
    window.requestAnimationFrame(() => {
      window.requestAnimationFrame(applyFit);
    });
    window.setTimeout(applyFit, 260);
    window.setTimeout(applyFit, 900);
  }
}

function clearViewportCullingSchedulers() {
  if (state.viewportCullFrameHandle !== null) {
    window.cancelAnimationFrame(state.viewportCullFrameHandle);
    state.viewportCullFrameHandle = null;
  }
  if (state.viewportCullTimerHandle !== null) {
    window.clearTimeout(state.viewportCullTimerHandle);
    state.viewportCullTimerHandle = null;
  }
}

function clearViewportCullingClasses(cy = state.cy) {
  if (!cy) return;
  cy.nodes().removeClass("offscreen-node");
  cy.edges().removeClass("offscreen-edge");
  state.viewportVisibleNodeIds = new Set();
  state.viewportEdgeGateLowZoom = null;
  state.viewportOverviewLabels = null;
  state.viewportCullIndex = null;
  state.viewportCullDirty = true;
}

function shouldEnableViewportCulling(cy) {
  if (!cy) return false;
  const nodeCount = cy.nodes().length;
  const edgeCount = cy.edges().length;
  return nodeCount >= VIEWPORT_CULL_NODE_THRESHOLD || edgeCount >= VIEWPORT_CULL_EDGE_THRESHOLD;
}

function currentViewportWorldBounds(cy, marginPx = VIEWPORT_CULL_MARGIN_PX) {
  const pan = cy.pan();
  const zoom = cy.zoom() || 1;
  const width = cy.width();
  const height = cy.height();
  const margin = Math.max(0, Number(marginPx) || 0);
  return {
    minX: (-pan.x - margin) / zoom,
    minY: (-pan.y - margin) / zoom,
    maxX: (width - pan.x + margin) / zoom,
    maxY: (height - pan.y + margin) / zoom,
  };
}

function rebuildViewportCullIndex(cy) {
  const entries = [];
  let minY = Infinity;
  let maxY = -Infinity;
  cy.nodes().forEach((node) => {
    const pos = node.position();
    if (pos.y < minY) minY = pos.y;
    if (pos.y > maxY) maxY = pos.y;
    entries.push({ id: node.id(), x: pos.x, y: pos.y });
  });
  entries.sort((a, b) => a.x - b.x);
  state.viewportCullIndex = entries;
  state.viewportCullBounds =
    entries.length > 0
      ? { minX: entries[0].x, maxX: entries[entries.length - 1].x, minY, maxY }
      : null;
  state.viewportCullDirty = false;
  state.viewportCullUnsorted = false;
  state.cullTombstones = new Set();
}

// Edge length index: world-space length per rendered edge, sorted so
// zoom changes flip only the band of edges crossing the readability
// window (min/max screen span), not every edge on the canvas.
function edgeWorldLength(fromId, toId) {
  const a = state.hierPositions ? state.hierPositions.get(fromId) : null;
  const b = state.hierPositions ? state.hierPositions.get(toId) : null;
  if (!a || !b) return null;
  const dx = a.x - b.x;
  const dy = a.y - b.y;
  return Math.sqrt(dx * dx + dy * dy);
}

function rebuildEdgeLengthIndex() {
  const entries = [];
  const lengths = new Map();
  const info = state.renderedEdgeInfo;
  if (info) {
    for (const [key, meta] of info) {
      const len = edgeWorldLength(meta.from, meta.to);
      if (len === null) continue;
      entries.push({ id: meta.id, len });
      lengths.set(meta.id, len);
    }
  }
  entries.sort((a, b) => a.len - b.len);
  state.edgeLengthSorted = entries;
  state.edgeLengths = lengths;
  state.edgeLengthBandMin = null;
  state.edgeLengthBandMax = null;
}

function appendEdgesToLengthIndex(edgeElements) {
  if (!state.edgeLengthSorted) {
    rebuildEdgeLengthIndex();
    return;
  }
  for (const element of edgeElements) {
    const len = edgeWorldLength(element.data.source, element.data.target);
    if (len === null) continue;
    state.edgeLengthSorted.push({ id: element.data.id, len });
    if (state.edgeLengths) state.edgeLengths.set(element.data.id, len);
    state.edgeLengthUnsorted = true;
  }
}

function applyEdgeSpanGate(cy) {
  const sorted = state.edgeLengthSorted;
  if (!sorted || sorted.length === 0) return;
  if (state.edgeLengthUnsorted) {
    sorted.sort((a, b) => a.len - b.len);
    state.edgeLengthUnsorted = false;
  }
  const zoom = cy.zoom() || 1;
  const viewportPx = Math.max(cy.width() || 1, cy.height() || 1);
  const minLen = EDGE_SPAN_MIN_PX / zoom;
  const maxLen = Math.max(minLen + 1, (viewportPx * EDGE_SPAN_VIEWPORT_FRACTION) / zoom);
  if (minLen === state.edgeLengthBandMin && maxLen === state.edgeLengthBandMax) return;

  // First classification after a mount: assign every edge its class
  // (cheap when the wholesale low-zoom gate already hides everything).
  if (state.edgeLengthBandMin === null) {
    state.edgeLengthBandMin = minLen;
    state.edgeLengthBandMax = maxLen;
    if (cy.edges().length > 20000 && zoom < EDGE_CULL_MIN_ZOOM) {
      return; // wholesale gate owns visibility here
    }
    cy.batch(() => {
      for (const entry of sorted) {
        const el = cy.getElementById(entry.id);
        if (el.empty()) continue;
        el.toggleClass("edge-span-culled", entry.len < minLen || entry.len > maxLen);
      }
    });
    return;
  }

  // Binary-search the in-band range for old and new windows; only edges
  // between them flip class.
  const lowerBound = (value) => {
    let lo = 0, hi = sorted.length;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      if (sorted[mid].len < value) lo = mid + 1;
      else hi = mid;
    }
    return lo;
  };
  const upperBound = (value) => {
    let lo = 0, hi = sorted.length;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      if (sorted[mid].len <= value) lo = mid + 1;
      else hi = mid;
    }
    return lo;
  };
  const oldMin = state.edgeLengthBandMin === null ? minLen : state.edgeLengthBandMin;
  const oldMax = state.edgeLengthBandMax === null ? maxLen : state.edgeLengthBandMax;

  const newLo = lowerBound(minLen);
  const newHi = upperBound(maxLen);
  const oldLo = lowerBound(Math.min(oldMin, minLen));
  const oldHi = upperBound(Math.max(oldMax, maxLen));

  const flips = [];
  for (let i = oldLo; i < oldHi; i++) {
    const entry = sorted[i];
    const inBand = entry.len >= minLen && entry.len <= maxLen && i >= newLo && i < newHi;
    // Compare band membership by value to stay correct at boundaries.
    const wasInBand = entry.len >= oldMin && entry.len <= oldMax;
    const isInBand = entry.len >= minLen && entry.len <= maxLen;
    if (wasInBand !== isInBand) flips.push([entry.id, isInBand]);
  }
  state.edgeLengthBandMin = minLen;
  state.edgeLengthBandMax = maxLen;
  if (flips.length === 0) return;
  cy.batch(() => {
    for (const [id, isInBand] of flips) {
      const el = cy.getElementById(id);
      if (el.empty()) continue;
      el.toggleClass("edge-span-culled", !isInBand);
    }
  });
}

// Incremental index maintenance for small deltas: appended nodes carry
// their preset positions (no Cytoscape accessor reads), removed nodes
// are tombstoned. A full rebuild (the expensive path) runs only when
// the index is dirty or tombstones accumulate past 10%.
function appendToViewportCullIndex(cy, newNodeElements, exitedNodeIds) {
  if (!state.viewportCullIndex || state.viewportCullDirty) {
    state.viewportCullDirty = true;
    scheduleViewportCulling(cy, { force: true });
    return;
  }
  for (const element of newNodeElements) {
    if (!element.position) continue;
    state.viewportCullIndex.push({
      id: element.data.id,
      x: element.position.x,
      y: element.position.y,
    });
    state.viewportCullUnsorted = true;
  }
  if (exitedNodeIds && exitedNodeIds.length > 0) {
    if (!state.cullTombstones) state.cullTombstones = new Set();
    for (const id of exitedNodeIds) state.cullTombstones.add(id);
  }
  scheduleViewportCulling(cy, { force: true });
}

function applyViewportCulling(cy = state.cy) {
  if (!cy || state.cy !== cy) return;
  if (!shouldEnableViewportCulling(cy)) {
    if (state.viewportCullingActive) {
      clearViewportCullingClasses(cy);
      state.viewportCullingActive = false;
    }
    return;
  }

  // The cull pass used to walk every element per viewport change with
  // Cytoscape accessors (hundreds of ms at 100k+ elements, defeating its
  // purpose). Positions are stable under preset layouts, so keep an
  // x-sorted index and binary-search the viewport window; only elements
  // entering/leaving the viewport get class toggles.
  const tombstones = state.cullTombstones || new Set();
  if (
    !state.viewportCullIndex ||
    state.viewportCullDirty ||
    tombstones.size > Math.max(1000, state.viewportCullIndex.length * 0.1)
  ) {
    rebuildViewportCullIndex(cy);
  } else if (state.viewportCullUnsorted) {
    state.viewportCullIndex.sort((a, b) => a.x - b.x);
    state.viewportCullUnsorted = false;
    let minY = Infinity;
    let maxY = -Infinity;
    for (const entry of state.viewportCullIndex) {
      if (entry.y < minY) minY = entry.y;
      if (entry.y > maxY) maxY = entry.y;
    }
    if (state.viewportCullIndex.length > 0) {
      state.viewportCullBounds = {
        minX: state.viewportCullIndex[0].x,
        maxX: state.viewportCullIndex[state.viewportCullIndex.length - 1].x,
        minY,
        maxY,
      };
    }
  }
  const index = state.viewportCullIndex;

  const bounds = currentViewportWorldBounds(cy);
  let lo = 0;
  let hi = index.length;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (index[mid].x < bounds.minX) lo = mid + 1;
    else hi = mid;
  }
  const start = lo;
  lo = start;
  hi = index.length;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (index[mid].x <= bounds.maxX) lo = mid + 1;
    else hi = mid;
  }
  const end = lo;

  const visible = new Set();
  for (let i = start; i < end; i++) {
    const entry = index[i];
    if (tombstones.has(entry.id)) continue;
    if (entry.y >= bounds.minY && entry.y <= bounds.maxY) {
      visible.add(entry.id);
    }
  }

  const previous = state.viewportVisibleNodeIds || new Set();
  const entered = [];
  const exited = [];
  for (const id of visible) {
    if (!previous.has(id)) entered.push(id);
  }
  for (const id of previous) {
    if (!visible.has(id)) exited.push(id);
  }
  state.viewportVisibleNodeIds = visible;

  // Edge LOD: below this zoom edges are subpixel noise — hide all of
  // them rather than paying per-edge math. Only flipped on crossing.
  const hideAllEdges =
    cy.edges().length > 20000 && cy.zoom() < EDGE_CULL_MIN_ZOOM;

  const nodeVisible = (id) => visible.has(id);

  // Huge transitions (zoom-out to overview flips ~100k cull classes at
  // once) are applied through a frame-chunked queue so the page never
  // blocks on one batch; small deltas apply immediately.
  const toggleJobs = [];
  for (const id of exited) toggleJobs.push([id, "add"]);
  for (const id of entered) toggleJobs.push([id, "remove"]);

  const applyNodeToggles = (jobs) => {
    if (!jobs.length) return;
    cy.batch(() => {
      for (const [id, action] of jobs) {
        const el = cy.getElementById(id);
        if (action === "add") el.addClass("offscreen-node");
        else el.removeClass("offscreen-node");
      }
    });
  };

  const finishCullPass = () => {
    const touchedEdges = new Set();
    for (const id of entered) {
      cy.getElementById(id).connectedEdges().forEach((edge) => {
        touchedEdges.add(edge.id());
      });
    }
    for (const id of exited) {
      cy.getElementById(id).connectedEdges().forEach((edge) => {
        touchedEdges.add(edge.id());
      });
    }
    if (touchedEdges.size > 0) {
      cy.collection(Array.from(touchedEdges).map((edgeId) => cy.getElementById(edgeId))).forEach((edge) => {
        const edgeOn =
          !hideAllEdges &&
          (nodeVisible(edge.data("source")) || nodeVisible(edge.data("target")));
        edge.toggleClass("offscreen-edge", !edgeOn);
      });
    }

    if (state.viewportEdgeGateLowZoom !== hideAllEdges) {
      state.viewportEdgeGateLowZoom = hideAllEdges;
      if (hideAllEdges) {
        cy.edges().addClass("offscreen-edge");
      } else {
        cy.edges().forEach((edge) => {
          const edgeOn =
            nodeVisible(edge.data("source")) || nodeVisible(edge.data("target"));
          edge.toggleClass("offscreen-edge", !edgeOn);
        });
      }
    }

    // Overview label gate: strip/restore node labels on zoom crossing.
    const overviewLabels =
      !(cy.zoom() >= LABEL_MIN_ZOOM) && cy.nodes().length > 20000;
    if (state.viewportOverviewLabels !== overviewLabels) {
      state.viewportOverviewLabels = overviewLabels;
      if (overviewLabels) {
        cy.nodes().addClass("overview-nolabel");
      } else {
        cy.nodes().removeClass("overview-nolabel");
      }
    }
    state.viewportCullingActive = true;
  };

  applyEdgeSpanGate(cy);

  if (toggleJobs.length > 8000) {
    state.cullToggleQueue = toggleJobs;
    state.cullToggleOnDone = finishCullPass;
    pumpCullToggleQueue();
  } else {
    applyNodeToggles(toggleJobs);
    finishCullPass();
  }
}

function pumpCullToggleQueue() {
  if (!state.cullToggleQueue || state.cullToggleQueue.length === 0) {
    const done = state.cullToggleOnDone;
    state.cullToggleQueue = null;
    state.cullToggleOnDone = null;
    if (done) done();
    return;
  }
  if (!state.cy || !state.cullToggleQueue) return;
  const chunk = state.cullToggleQueue.splice(0, 5000);
  state.cy.batch(() => {
    for (const [id, action] of chunk) {
      const el = state.cy.getElementById(id);
      if (action === "add") el.addClass("offscreen-node");
      else el.removeClass("offscreen-node");
    }
  });
  window.requestAnimationFrame(pumpCullToggleQueue);
}

function scheduleViewportCulling(cy = state.cy, options = {}) {
  if (!cy || state.cy !== cy) return;
  const force = options.force === true;
  if (force) {
    clearViewportCullingSchedulers();
    state.viewportCullLastAt = performance.now();
    applyViewportCulling(cy);
    return;
  }
  if (state.viewportCullFrameHandle !== null || state.viewportCullTimerHandle !== null) {
    return;
  }

  const now = performance.now();
  const elapsed = now - state.viewportCullLastAt;
  const delay = Math.max(0, VIEWPORT_CULL_THROTTLE_MS - elapsed);
  const enqueue = () => {
    state.viewportCullTimerHandle = null;
    state.viewportCullFrameHandle = window.requestAnimationFrame(() => {
      state.viewportCullFrameHandle = null;
      state.viewportCullLastAt = performance.now();
      applyViewportCulling(cy);
    });
  };
  if (delay <= 0) {
    enqueue();
  } else {
    state.viewportCullTimerHandle = window.setTimeout(enqueue, delay);
  }
}

function setTransitionLodActive(cy, enabled) {
  if (!cy) return;
  const active = enabled === true;
  if (state.transitionLodActive === active) {
    return;
  }
  state.transitionLodActive = active;
  cy.batch(() => {
    if (active) {
      cy.nodes().addClass("transition-lod-node");
      cy.edges().addClass("transition-lod-edge");
    } else {
      cy.nodes().removeClass("transition-lod-node");
      cy.edges().removeClass("transition-lod-edge");
    }
  });
}

function shouldUseTransitionLod(cy = state.cy) {
  if (!cy) return false;
  const nodeCount = cy.nodes().length;
  if (nodeCount < TRANSITION_LOD_NODE_THRESHOLD) {
    return false;
  }
  return Boolean(
    state.fullLoadSession ||
    state.stagedRevealSession ||
    state.impactRevealSession ||
    state.deferInsightsActive
  );
}

function syncTransitionLod(cy = state.cy) {
  if (!cy || state.cy !== cy) return;
  setTransitionLodActive(cy, shouldUseTransitionLod(cy));
}

function buildExpansionFocusElements(cy, context) {
  if (!cy || !context) {
    return cy ? cy.collection() : null;
  }

  const sourceNode = cy.$id(context.sourceNodeId);
  if (!sourceNode || sourceNode.empty()) {
    return cy.collection();
  }

  const neighborSet = new Set(context.addedNodeIds || []);
  const neighborElements = [];
  for (const id of neighborSet) {
    const node = cy.getElementById(id);
    if (!node.empty()) neighborElements.push(node);
  }

  if (neighborElements.length > 0) {
    const nexusEdges = [];
    sourceNode.connectedEdges().forEach((edge) => {
      const sourceId = edge.data("source");
      const targetId = edge.data("target");
      if (
        (sourceId === context.sourceNodeId && neighborSet.has(targetId)) ||
        (targetId === context.sourceNodeId && neighborSet.has(sourceId))
      ) {
        nexusEdges.push(edge);
      }
    });
    return sourceNode.union(cy.collection(neighborElements)).union(
      nexusEdges.length ? cy.collection(nexusEdges) : cy.collection()
    );
  }

  // Fallback on collapse/no additions: keep the clicked node and its
  // immediate visible neighborhood centered rather than fitting the
  // whole graph. Incident edges only — closedNeighborhood() on a
  // container builds its entire subtree collection.
  return sourceNode.union(sourceNode.connectedEdges());
}

function clearExpansionFocus(cy) {
  if (!cy) return;
  cy.nodes().removeClass(
    "expansion-origin expansion-neighbor staged-pending"
  );
  cy.edges().removeClass(
    "expansion-edge-outgoing expansion-edge-incoming staged-edge-pending"
  );
}

function applyExpansionFocus(cy, context) {
  if (!cy || !context) return;
  clearExpansionFocus(cy);

  const sourceNode = cy.$id(context.sourceNodeId);
  if (!sourceNode || sourceNode.empty()) return;
  sourceNode.addClass("expansion-origin");

  const neighborSet = new Set(context.addedNodeIds.slice(0, EXPANSION_HIGHLIGHT_NODE_LIMIT));
  const neighborNodes = [];
  for (const id of neighborSet) {
    const node = cy.getElementById(id);
    if (!node.empty()) neighborNodes.push(node);
  }
  if (neighborNodes.length > 0) {
    cy.collection(neighborNodes).addClass("expansion-neighbor");

    // Nexus edges hang off the source node — resolve them from its
    // connected edges (degree-proportional) instead of filtering every
    // edge on the canvas.
    const outgoingEdges = [];
    const incomingEdges = [];
    sourceNode.connectedEdges().forEach((edge) => {
      const sourceId = edge.data("source");
      const targetId = edge.data("target");
      if (sourceId === context.sourceNodeId && neighborSet.has(targetId)) {
        outgoingEdges.push(edge);
      } else if (targetId === context.sourceNodeId && neighborSet.has(sourceId)) {
        incomingEdges.push(edge);
      }
    });
    if (outgoingEdges.length) cy.collection(outgoingEdges).addClass("expansion-edge-outgoing");
    if (incomingEdges.length) cy.collection(incomingEdges).addClass("expansion-edge-incoming");
  }
}

function stopStagedReveal() {
  if (state.stagedRevealTimeoutHandle !== null) {
    window.clearTimeout(state.stagedRevealTimeoutHandle);
    state.stagedRevealTimeoutHandle = null;
  }
  state.stagedRevealSession = null;
  syncTransitionLod();
  updateLoadingControls();
}

function revealNodeAndIncidentEdges(cy, nodeId) {
  const node = cy.$id(nodeId);
  if (!node || node.empty()) return;
  node.removeClass("staged-pending");
  node.connectedEdges().forEach((edge) => {
    const source = edge.source();
    const target = edge.target();
    if (!source.hasClass("staged-pending") && !target.hasClass("staged-pending")) {
      edge.removeClass("staged-edge-pending");
    }
  });
}

function stagedRevealDelayMs() {
  const ms = clampRevealSpeedMs(state.revealIntervalMs);
  return ms <= 0 ? 0 : Math.max(REVEAL_SPEED_STEP_MS, ms);
}

function stagedRevealBatchSizeForDelay(delayMs) {
  if (delayMs >= 260) return 1;
  if (delayMs >= 140) return 2;
  if (delayMs >= 80) return 3;
  return 5;
}

function revealFrameBudgetMs(delayMs, nodeCount = 0) {
  const normalizedNodeCount = Math.max(0, Number(nodeCount) || 0);
  if (delayMs >= 260) return REVEAL_FRAME_BUDGET_MS + 3;
  if (normalizedNodeCount >= 1600) return Math.max(4, REVEAL_FRAME_BUDGET_MS - 3);
  if (normalizedNodeCount >= 1100) return Math.max(5, REVEAL_FRAME_BUDGET_MS - 2);
  return REVEAL_FRAME_BUDGET_MS;
}

function scheduleStagedRevealTick(delayMs) {
  if (state.stagedRevealTimeoutHandle !== null) {
    window.clearTimeout(state.stagedRevealTimeoutHandle);
    state.stagedRevealTimeoutHandle = null;
  }
  if (!state.stagedRevealSession) return;

  const delay = Math.max(0, Number(delayMs) || 0);
  state.stagedRevealTimeoutHandle = window.setTimeout(() => {
    state.stagedRevealTimeoutHandle = null;
    runStagedRevealTick();
  }, delay);
}

function finishStagedReveal(session) {
  if (!session) return;
  const completionCallback = typeof session.onComplete === "function" ? session.onComplete : null;
  const completionCy = session.cy;
  if (state.stagedRevealTimeoutHandle !== null) {
    window.clearTimeout(state.stagedRevealTimeoutHandle);
    state.stagedRevealTimeoutHandle = null;
  }
  state.stagedRevealSession = null;
  updateLoadingControls();
  if (session.context) {
    const tone = session.context.removedCount > 0 ? "shrink" : "grow";
    setExpansionStatus(session.context, tone);
  } else if (state.impactActive) {
    setExpansionStatusText(`Impact reveal complete: ${session.total}/${session.total}.`, "grow");
  }
  if (session.deferInsights === true) {
    state.deferInsightsActive = false;
    scheduleInsightsRefresh({
      immediate: true,
      preferIdle: false,
      applyGraphFocus: true,
    });
  }
  syncTransitionLod();
  scheduleViewportCulling(state.cy, { force: true });
  // Ensure post-reveal spacing settles without requiring user interaction.
  kickGlobalRepulsion(recommendedGlobalRepelSettleFrames(28));
  window.setTimeout(() => {
    if (!state.cy) return;
    kickGlobalRepulsion(recommendedGlobalRepelSettleFrames(22));
  }, 220);
  if (completionCallback) {
    window.setTimeout(() => {
      if (!completionCy || state.cy !== completionCy) return;
      completionCallback(completionCy);
    }, 0);
  }
}

function revealRemainingNow(session) {
  if (!session) return;
  const { cy, queue, context } = session;
  if (state.cy !== cy) {
    stopStagedReveal();
    return;
  }

  while (queue.length > 0) {
    const nodeId = queue.shift();
    if (!nodeId) continue;
    revealNodeAndIncidentEdges(cy, nodeId);
    session.revealed += 1;
  }
  kickGlobalRepulsion(recommendedGlobalRepelSettleFrames(24));
  scheduleViewportCulling(cy);

  if (context) {
    setExpansionStatusText(
      `${describeExpansionAction(context.action, context.sourceKind)} "${state.graphIndex?.nodeById.get(context.sourceNodeId)?.label || context.sourceNodeId}" • revealing ${session.revealed}/${session.total}`,
      "grow"
    );
  } else if (state.impactActive) {
    setExpansionStatusText(`Impact reveal: ${session.revealed}/${session.total}`, "grow");
  }
  finishStagedReveal(session);
}

function runStagedRevealTick() {
  const session = state.stagedRevealSession;
  if (!session) return;

  const { cy, queue, context } = session;
  if (state.cy !== cy) {
    stopStagedReveal();
    return;
  }

  const delayMs = stagedRevealDelayMs();
  if (delayMs === 0) {
    revealRemainingNow(session);
    return;
  }

  const batchSize = stagedRevealBatchSizeForDelay(delayMs);
  const frameBudgetMs = revealFrameBudgetMs(delayMs, cy.nodes().length);
  const frameStart = performance.now();
  let revealedThisTick = 0;
  while (revealedThisTick < batchSize && queue.length > 0) {
    if (revealedThisTick > 0 && performance.now() - frameStart >= frameBudgetMs) {
      break;
    }
    const nextNodeId = queue.shift();
    if (!nextNodeId) continue;
    revealNodeAndIncidentEdges(cy, nextNodeId);
    session.revealed += 1;
    revealedThisTick += 1;
  }

  if (revealedThisTick === 0) {
    finishStagedReveal(session);
    return;
  }

  kickGlobalRepulsion(recommendedGlobalRepelSettleFrames(10 + revealedThisTick * 2));
  scheduleViewportCulling(cy);

  if (context) {
    setExpansionStatusText(
      `${describeExpansionAction(context.action, context.sourceKind)} "${state.graphIndex?.nodeById.get(context.sourceNodeId)?.label || context.sourceNodeId}" • revealing ${session.revealed}/${session.total}`,
      "grow"
    );
  } else if (state.impactActive) {
    setExpansionStatusText(`Impact reveal: ${session.revealed}/${session.total}`, "grow");
  }

  if (queue.length === 0) {
    finishStagedReveal(session);
    return;
  }

  scheduleStagedRevealTick(delayMs);
}

function startStagedReveal(cy, nodeIds, context = null, options = {}) {
  stopStagedReveal();
  if (!cy || !Array.isArray(nodeIds) || nodeIds.length === 0) return;

  const queue = nodeIds.filter((nodeId) => {
    const node = cy.$id(nodeId);
    return !!node && !node.empty();
  });
  if (queue.length === 0) return;

  const pendingSet = new Set(queue);
  cy.nodes().forEach((node) => {
    if (pendingSet.has(node.id())) {
      node.addClass("staged-pending");
    }
  });
  cy.edges().forEach((edge) => {
    if (pendingSet.has(edge.source().id()) || pendingSet.has(edge.target().id())) {
      edge.addClass("staged-edge-pending");
    }
  });

  const session = {
    cy,
    queue,
    context,
    total: queue.length,
    revealed: 0,
    deferInsights: options.deferInsights === true,
    onComplete: typeof options.onComplete === "function" ? options.onComplete : null,
  };
  state.stagedRevealSession = session;
  syncTransitionLod(cy);
  updateLoadingControls();

  if (context) {
    setExpansionStatusText(
      `${describeExpansionAction(context.action, context.sourceKind)} "${state.graphIndex?.nodeById.get(context.sourceNodeId)?.label || context.sourceNodeId}" • revealing 0/${session.total}`,
      "grow"
    );
  } else if (state.impactActive) {
    setExpansionStatusText(`Impact reveal: 0/${session.total}`, "grow");
  }

  if (stagedRevealDelayMs() === 0) {
    revealRemainingNow(session);
    return;
  }

  scheduleStagedRevealTick(0);
}

function clearAllProgressiveExpansions() {
  state.expandedModuleNodes.clear();
  state.expandedFileNodes.clear();
  state.expandedSymbolNodes.clear();
  state.expandedRelationNodes.clear();
  state.expansionLevelByNode.clear();
  state.fullLoadIncludeAll = false;
}

function collectNodeIdsByKind(kind) {
  const index = state.graphIndex;
  if (!index) return [];
  return Array.from(index.nodeById.values())
    .filter((node) => node.kind === kind)
    .map((node) => node.id)
    .sort((aId, bId) => compareNodeLabels(index.nodeById, aId, bId));
}

function fullLoadStepDelayMs() {
  const ms = clampRevealSpeedMs(state.revealIntervalMs);
  return ms <= 0 ? 0 : Math.max(FULL_LOAD_MIN_SPEED_MS, ms);
}

// Batch sizes are node-expansions per tick. Since full-load renders
// became incremental appends (no instance rebuild), the per-node cost
// is a set-add plus an amortized cy.add — batches sized for the old
// quadratic rebuilds (5 nodes per 300ms ≈ 2 hours for 115k nodes) are
// obsolete. These complete the full graph in ~2-40s by reveal speed.
function fullLoadBatchSizeForDelay(delayMs) {
  if (delayMs <= 40) return 5000;
  if (delayMs <= 80) return 4000;
  if (delayMs <= 160) return 3500;
  if (delayMs <= 300) return 3000;
  return 2500;
}

function buildFullLoadPhases() {
  const index = state.graphIndex;
  if (!index) return [];

  const crateIds = index.crateNodeIds.slice();
  const moduleIds = collectNodeIdsByKind("module");
  const fileIds = collectNodeIdsByKind("file");
  const moduleSourceIds = Array.from(new Set([...crateIds, ...moduleIds]));
  const fileSourceIds = Array.from(new Set([...crateIds, ...moduleIds]));
  const symbolSourceIds = Array.from(new Set([...crateIds, ...moduleIds, ...fileIds]));

  return [
    {
      key: "modules",
      label: "modules",
      ids: moduleSourceIds,
      apply: (nodeId) => {
        state.expandedModuleNodes.add(nodeId);
        state.expansionLevelByNode.set(nodeId, 1);
      },
    },
    {
      key: "files",
      label: "files",
      ids: fileSourceIds,
      apply: (nodeId) => {
        state.expandedFileNodes.add(nodeId);
        if ((state.expansionLevelByNode.get(nodeId) || 0) < 1) {
          state.expansionLevelByNode.set(nodeId, 1);
        }
      },
    },
    {
      key: "symbols",
      label: "symbols",
      ids: symbolSourceIds,
      apply: (nodeId) => {
        state.expandedSymbolNodes.add(nodeId);
      },
    },
  ];
}

function clearFullLoadTimer() {
  if (state.fullLoadTimeoutHandle !== null) {
    window.clearTimeout(state.fullLoadTimeoutHandle);
    state.fullLoadTimeoutHandle = null;
  }
}

function stopProgressiveFullLoad(message = "", tone = "warn", options = {}) {
  const rerender = options.rerender === true;
  clearFullLoadTimer();
  if (!state.fullLoadSession) {
    syncTransitionLod();
    updateLoadingControls();
    return;
  }
  state.fullLoadSession = null;
  state.fullLoadAppendQueue = [];
  syncTransitionLod();
  if (rerender && state.graph && state.graphIndex) {
    renderGraph("progressive", { preserveViewport: true, focusSearch: false });
  }
  if (message) {
    setExpansionStatusText(message, tone);
  }
  updateLoadingControls();
  if (state.graph) {
    updateRenderModeToggle(state.graph);
  }
}

function scheduleFullLoadTick(delayMs) {
  clearFullLoadTimer();
  if (!state.fullLoadSession) return;
  const delay = Math.max(0, Number(delayMs) || 0);
  state.fullLoadTimeoutHandle = window.setTimeout(() => {
    state.fullLoadTimeoutHandle = null;
    runFullLoadTick();
  }, delay);
}

function runFullLoadTick() {
  try {
    runFullLoadTickImpl();
  } catch (err) {
    stopProgressiveFullLoad(`Full load stopped: ${err && err.message ? err.message : err}`, "warn");
    console.error("[explorer] full load tick failed", err);
  }
}

function runFullLoadTickImpl() {
  const session = state.fullLoadSession;
  if (!session || !state.graph || !state.graphIndex) return;

  const delayMs = fullLoadStepDelayMs();
  if (delayMs < FULL_LOAD_MIN_SPEED_MS) {
    stopProgressiveFullLoad(
      `Full load stopped: set Reveal to at least ${FULL_LOAD_MIN_SPEED_MS}ms.`,
      "warn"
    );
    return;
  }

  let processedThisTick = 0;
  let phaseChanged = false;
  const batchSize = fullLoadBatchSizeForDelay(delayMs);
  const frameBudgetMs = revealFrameBudgetMs(delayMs, state.renderedNodeCount);
  const frameStart = performance.now();

  while (processedThisTick < batchSize && session.phaseIndex < session.phases.length) {
    if (processedThisTick > 0 && performance.now() - frameStart >= frameBudgetMs) {
      break;
    }
    const phase = session.phases[session.phaseIndex];
    if (session.cursor >= phase.ids.length) {
      session.phaseIndex += 1;
      session.cursor = 0;
      phaseChanged = true;
      continue;
    }
    const nodeId = phase.ids[session.cursor];
    session.cursor += 1;
    phase.apply(nodeId);
    session.processed += 1;
    processedThisTick += 1;
  }

  const done = session.phaseIndex >= session.phases.length;
  const currentPhase = done
    ? session.phases[session.phases.length - 1] || null
    : session.phases[session.phaseIndex] || null;
  const phaseDoneCount = done ? (currentPhase?.ids.length || 0) : session.cursor;
  const phaseTotalCount = currentPhase?.ids.length || 0;
  const phaseLabel = done ? "complete" : currentPhase?.label || "loading";

  setExpansionStatusText(
    `Full load ${phaseLabel}: ${phaseDoneCount}/${phaseTotalCount} • total ${session.processed}/${session.total}`,
    done ? "grow" : "muted"
  );

  const now = Date.now();
  // Render only at phase boundaries (plus the initial + final renders):
  // time-based cadence rebuilt per-render bookkeeping over the whole
  // mounted slice, saturating the main thread as it grew. Appends at
  // boundaries carry just the delta since the previous render.
  const shouldRender = done || phaseChanged || !session.hasRendered;
  if (shouldRender) {
    renderGraph("progressive", {
      preserveViewport: true,
      focusSearch: false,
    });
    scheduleViewportCulling(state.cy);
    session.hasRendered = true;
    session.lastRenderAt = now;
  }

  if (done) {
    stopProgressiveFullLoad("Full graph progressive load complete.", "grow");
    return;
  }

  scheduleFullLoadTick(delayMs);
}

// Errors in the full-load chain used to die silently and just freeze the
// loader; surface them in the expansion status instead.
function startProgressiveFullLoad() {
  // Full-graph browsing lives on the plane view now (GPU batched, no
  // mount cost). Loading everything through this Cytoscape page was the
  // historical multi-second-freeze path; keep this page for analysis on
  // bounded slices.
  const params = new URLSearchParams(window.location.search);
  params.set("all", "1");
  window.location.href = "plane.html?" + params.toString();
}

function startProgressiveFullLoadLegacy() {
  try {
    startProgressiveFullLoadImpl();
  } catch (err) {
    state.fullLoadSession = null;
    setExpansionStatusText(`Full load failed: ${err && err.message ? err.message : err}`, "warn");
    console.error("[explorer] full load failed", err);
    updateLoadingControls();
  }
}

function startProgressiveFullLoadImpl() {
  if (!state.graph || !state.graphIndex) return;
  const delayMs = fullLoadStepDelayMs();
  if (delayMs < FULL_LOAD_MIN_SPEED_MS) {
    setExpansionStatusText(
      `Set Reveal to at least ${FULL_LOAD_MIN_SPEED_MS}ms to run full progressive load.`,
      "warn"
    );
    return;
  }

  if (state.searchQuery.trim().length > 0) {
    applySearch("");
    const input = el("search-input");
    if (input) {
      input.value = "";
    }
  }

  stopProgressiveFullLoad();
  stopStagedReveal();
  clearAllProgressiveExpansions();
  writeUrlViewState();
  state.fullLoadIncludeAll = true;

  const phases = buildFullLoadPhases();
  const total = phases.reduce((sum, phase) => sum + phase.ids.length, 0);
  if (total === 0) {
    setExpansionStatusText("No nodes available for progressive full load.", "warn");
    return;
  }

  state.fullLoadSession = {
    phases,
    phaseIndex: 0,
    cursor: 0,
    total,
    processed: 0,
    hasRendered: false,
    lastRenderAt: 0,
  };

  syncTransitionLod();
  updateLoadingControls();
  renderGraph("progressive", { preserveViewport: false, focusSearch: false });
  // No repulsion kick: full-load nodes carry preset hierarchical
  // positions, and a repulsion pass over the whole mounted set each
  // tick was sized for the old 5-nodes-per-tick cadence — at scale it
  // froze the page.
  setExpansionStatusText(`Full load started: 0/${total}`, "grow");
  scheduleFullLoadTick(0);
}

function cancelActiveStagedExpansion() {
  const session = state.stagedRevealSession;
  if (!session) return false;

  const context = session.context || null;
  stopStagedReveal();
  if (!context || context.action.startsWith("collapse")) {
    setExpansionStatusText("Expansion loading stopped.", "warn");
    return true;
  }

  const beforeVisible = visibleNodeSetForCurrentView();
  const undoAction = toggleProgressiveExpansion(context.sourceNodeId, context.sourceKind);
  const afterVisible = visibleNodeSetForCurrentView();
  const collapseContext = buildExpansionContext(
    beforeVisible,
    afterVisible,
    context.sourceNodeId,
    undoAction
  );
  writeUrlViewState();
  setExpansionStatus(collapseContext, "shrink");
  renderGraph("progressive", {
    preserveViewport: true,
    focusSearch: false,
    expansionContext: collapseContext,
    stagedRevealNodeIds: [],
  });
  scheduleTransientNodeTooltip(context.sourceNodeId, 120, 1000);
  return true;
}

function stopActiveLoading() {
  if (state.fullLoadSession) {
    stopProgressiveFullLoad("Full graph progressive load stopped.", "warn", { rerender: true });
    return;
  }
  if (state.stagedRevealSession) {
    cancelActiveStagedExpansion();
    return;
  }
  if (state.impactRevealSession) {
    stopImpactReveal({ emitStatus: true, message: "Impact reveal stopped.", tone: "warn" });
    return;
  }
  if (state.flowPlaybackSession) {
    stopFlowPlayback();
    setFlowStatus("Flow playback stopped.", "muted");
    return;
  }
  setExpansionStatusText("No active loading to stop.", "muted");
}

function updateLoadingControls() {
  const stopButton = el("stop-loading");
  if (!stopButton) {
    if (state.graph) {
      updateRenderModeToggle(state.graph);
    }
    return;
  }
  const hasFullLoad = !!state.fullLoadSession;
  const hasStagedReveal = !!state.stagedRevealSession;
  const hasImpactReveal = !!state.impactRevealSession;
  const hasFlowPlayback = !!state.flowPlaybackSession;
  const active = hasFullLoad || hasStagedReveal || hasImpactReveal || hasFlowPlayback;
  stopButton.disabled = !active;
  if (hasFullLoad) {
    stopButton.textContent = "Stop Full Load";
  } else if (hasStagedReveal) {
    stopButton.textContent = "Stop Expand";
  } else if (hasImpactReveal) {
    stopButton.textContent = "Stop Impact";
  } else if (hasFlowPlayback) {
    stopButton.textContent = "Stop Flow";
  } else {
    stopButton.textContent = "Stop";
  }
  if (state.graph) {
    updateRenderModeToggle(state.graph);
  }
}

function buildKindFilter(nodes) {
  const kinds = Array.from(new Set(nodes.map((n) => n.kind))).sort();
  const select = el("kind-filter");
  if (!select) return;

  select.innerHTML = "";
  const allOption = document.createElement("option");
  allOption.value = "all";
  allOption.textContent = "All Kinds";
  select.appendChild(allOption);

  for (const kind of kinds) {
    const opt = document.createElement("option");
    opt.value = kind;
    opt.textContent = kind;
    select.appendChild(opt);
  }

  const desiredKind = kinds.includes(state.kindFilter) ? state.kindFilter : "all";
  state.kindFilter = desiredKind;
  select.value = desiredKind;
}

function chooseInitialRenderMode(graph) {
  return "progressive";
}

function getModulesForNode(nodeId) {
  const index = state.graphIndex;
  return index?.moduleChildrenByParent.get(nodeId) || [];
}

function getFilesForNode(nodeId) {
  const index = state.graphIndex;
  return index?.fileChildrenByParent.get(nodeId) || [];
}

function getSymbolsForNode(nodeId) {
  const index = state.graphIndex;
  if (!index) return [];

  const node = index.nodeById.get(nodeId);
  if (!node) return [];

  const dedupe = new Set();
  const pushSymbols = (ids) => {
    for (const id of ids || []) {
      if (dedupe.has(id)) continue;
      const symbolNode = index.nodeById.get(id);
      if (!symbolNode || !SYMBOL_KINDS.has(symbolNode.kind)) continue;
      dedupe.add(id);
    }
  };

  pushSymbols(index.symbolChildrenByParent.get(nodeId));
  for (const fileId of index.fileChildrenByParent.get(nodeId) || []) {
    pushSymbols(index.symbolChildrenByParent.get(fileId));
  }
  if (node.kind === "crate") {
    for (const moduleId of index.moduleChildrenByParent.get(nodeId) || []) {
      pushSymbols(index.symbolChildrenByParent.get(moduleId));
      for (const fileId of index.fileChildrenByParent.get(moduleId) || []) {
        pushSymbols(index.symbolChildrenByParent.get(fileId));
      }
    }
  }

  return Array.from(dedupe).sort((aId, bId) => {
    const degreeDelta = (index.degreeById.get(bId) || 0) - (index.degreeById.get(aId) || 0);
    if (degreeDelta !== 0) return degreeDelta;
    return compareNodeLabels(index.nodeById, aId, bId);
  });
}

function getRelationNeighborsForNode(nodeId) {
  const index = state.graphIndex;
  if (!index) return [];
  return index.relationNeighborsByNode.get(nodeId) || [];
}

function addAncestorParents(visibleNodeIds, maxNodes = Number.POSITIVE_INFINITY) {
  const index = state.graphIndex;
  if (!index) return visibleNodeIds;

  const queue = Array.from(visibleNodeIds);
  while (queue.length > 0 && visibleNodeIds.size < maxNodes) {
    const current = queue.pop();
    for (const parentId of index.parentsByChild.get(current) || []) {
      const parentKind = index.nodeById.get(parentId)?.kind;
      if (!parentKind || (!ROOT_PROGRESSIVE_KINDS.has(parentKind) && parentKind !== "file")) {
        continue;
      }
      if (!visibleNodeIds.has(parentId)) {
        visibleNodeIds.add(parentId);
        queue.push(parentId);
      }
      if (visibleNodeIds.size >= maxNodes) break;
    }
  }
  return visibleNodeIds;
}

function applyProgressiveNodeCap(visibleNodeIds) {
  if (visibleNodeIds.size <= SEARCH_VISIBLE_LIMIT) {
    return visibleNodeIds;
  }

  const index = state.graphIndex;
  const rootAndContainers = [];
  const symbols = [];
  for (const id of visibleNodeIds) {
    const kind = index.nodeById.get(id)?.kind;
    if (!kind) continue;
    if (SYMBOL_KINDS.has(kind)) {
      symbols.push(id);
    } else {
      rootAndContainers.push(id);
    }
  }

  symbols.sort((aId, bId) => {
    const degreeDelta = (index.degreeById.get(bId) || 0) - (index.degreeById.get(aId) || 0);
    if (degreeDelta !== 0) return degreeDelta;
    return compareNodeLabels(index.nodeById, aId, bId);
  });

  const kept = new Set(rootAndContainers);
  const remaining = Math.max(0, SEARCH_VISIBLE_LIMIT - kept.size);
  for (const symbolId of symbols.slice(0, remaining)) {
    kept.add(symbolId);
  }
  return kept;
}

function buildProgressiveVisibleSet() {
  const index = state.graphIndex;
  const visible = new Set(index.rootNodeIds);
  const uncapped = state.fullLoadIncludeAll || !!state.fullLoadSession;

  for (const nodeId of state.expandedModuleNodes) {
    const modules = uncapped
      ? getModulesForNode(nodeId)
      : getModulesForNode(nodeId).slice(0, MODULE_EXPAND_LIMIT);
    for (const moduleId of modules) {
      visible.add(moduleId);
    }
  }

  for (const nodeId of state.expandedFileNodes) {
    const files = uncapped
      ? getFilesForNode(nodeId)
      : getFilesForNode(nodeId).slice(0, FILE_EXPAND_LIMIT);
    for (const fileId of files) {
      visible.add(fileId);
    }
  }

  for (const nodeId of state.expandedSymbolNodes) {
    const symbols = uncapped
      ? getSymbolsForNode(nodeId)
      : getSymbolsForNode(nodeId).slice(0, SYMBOL_EXPAND_LIMIT);
    for (const symbolId of symbols) {
      visible.add(symbolId);
    }
  }

  for (const nodeId of state.expandedRelationNodes) {
    visible.add(nodeId);
    const neighbors = uncapped
      ? getRelationNeighborsForNode(nodeId)
      : getRelationNeighborsForNode(nodeId).slice(0, RELATION_EXPAND_LIMIT);
    for (const neighborId of neighbors) {
      visible.add(neighborId);
    }
  }

  addAncestorParents(visible);
  if (uncapped) {
    return visible;
  }
  return applyProgressiveNodeCap(visible);
}

function buildSearchVisibleSet(query) {
  const index = state.graphIndex;
  const queryNormalized = normalize(query.trim());
  if (!queryNormalized) {
    return {
      visibleNodeIds: buildProgressiveVisibleSet(),
      matchedNodeIds: new Set(),
      totalMatches: 0,
      truncated: false,
    };
  }

  const allMatches = [];
  for (const node of state.graph.nodes) {
    if (nodeMatchesQuery(node, queryNormalized)) {
      allMatches.push(node.id);
    }
  }

  const kindPriority = {
    crate: 0,
    module: 1,
    file: 2,
    function: 3,
    struct: 4,
    enum: 5,
    trait: 6,
    const: 7,
    static: 8,
    type_alias: 9,
  };
  allMatches.sort((aId, bId) => {
    const aKind = index.nodeById.get(aId)?.kind || "";
    const bKind = index.nodeById.get(bId)?.kind || "";
    const kindDelta = (kindPriority[aKind] ?? 99) - (kindPriority[bKind] ?? 99);
    if (kindDelta !== 0) return kindDelta;
    const degreeDelta = (index.degreeById.get(bId) || 0) - (index.degreeById.get(aId) || 0);
    if (degreeDelta !== 0) return degreeDelta;
    return compareNodeLabels(index.nodeById, aId, bId);
  });

  const limitedMatches = allMatches.slice(0, SEARCH_MATCH_LIMIT);
  const matchedNodeIds = new Set(limitedMatches);
  const visible = new Set(limitedMatches);
  addAncestorParents(visible, SEARCH_VISIBLE_LIMIT);

  // Pull in direct neighbours of each matched node (incoming +
  // outgoing edges) so the cytoscape canvas actually shows the
  // matched function's call fan-out — previously the slice was
  // matched + ancestors only, so a function with 30 outgoing edges
  // rendered as a lone node. Cap to keep the canvas readable.
  const NEIGHBOR_PER_MATCH_CAP = 60;
  if (visible.size < SEARCH_VISIBLE_LIMIT && state.graph && Array.isArray(state.graph.edges)) {
    for (const matchId of matchedNodeIds) {
      if (visible.size >= SEARCH_VISIBLE_LIMIT) break;
      let added = 0;
      for (const edge of state.graph.edges) {
        if (added >= NEIGHBOR_PER_MATCH_CAP || visible.size >= SEARCH_VISIBLE_LIMIT) break;
        let neighbor = null;
        if (edge.from === matchId && edge.to !== matchId) neighbor = edge.to;
        else if (edge.to === matchId && edge.from !== matchId) neighbor = edge.from;
        if (!neighbor || visible.has(neighbor)) continue;
        if (!index.nodeById.has(neighbor)) continue;
        visible.add(neighbor);
        added++;
      }
    }
    // Promote any newly-added neighbour's ancestor crate/module/file
    // into the slice so the cytoscape layout has containers to group
    // them under.
    addAncestorParents(visible, SEARCH_VISIBLE_LIMIT);
  }

  if (visible.size < SEARCH_VISIBLE_LIMIT) {
    for (const nodeId of state.expandedRelationNodes) {
      if (!visible.has(nodeId) && !matchedNodeIds.has(nodeId)) {
        continue;
      }
      const neighbors = getRelationNeighborsForNode(nodeId).slice(0, RELATION_EXPAND_LIMIT);
      for (const neighborId of neighbors) {
        if (visible.has(neighborId)) continue;
        visible.add(neighborId);
        if (visible.size >= SEARCH_VISIBLE_LIMIT) break;
      }
      if (visible.size >= SEARCH_VISIBLE_LIMIT) break;
    }
  }
  addAncestorParents(visible, SEARCH_VISIBLE_LIMIT);

  return {
    visibleNodeIds: applyProgressiveNodeCap(visible),
    matchedNodeIds,
    totalMatches: allMatches.length,
    truncated: allMatches.length > SEARCH_MATCH_LIMIT,
  };
}

function buildImpactVisibleSet() {
  const impactNodeIds =
    state.impactVisibleNodeIds instanceof Set && state.impactVisibleNodeIds.size > 0
      ? state.impactVisibleNodeIds
      : state.impactNodeIds;
  const changedFileNodeIds = state.impactChangedFileNodeIds;
  if (!(impactNodeIds instanceof Set) || impactNodeIds.size === 0) {
    return {
      visibleNodeIds: buildProgressiveVisibleSet(),
      matchedNodeIds: new Set(),
      totalMatches: 0,
      truncated: false,
    };
  }

  const visibleNodeIds = new Set(impactNodeIds);
  addAncestorParents(visibleNodeIds, IMPACT_NODE_CAP);
  return {
    visibleNodeIds,
    matchedNodeIds: changedFileNodeIds instanceof Set ? new Set(changedFileNodeIds) : new Set(),
    totalMatches: changedFileNodeIds instanceof Set ? changedFileNodeIds.size : 0,
    truncated: impactNodeIds.size >= IMPACT_NODE_CAP,
  };
}

function collectDirectedReachable(seedIds, direction, maxHops, maxNodes, options = {}) {
  const index = state.graphIndex;
  const hops = Math.max(0, Math.round(Number(maxHops) || 0));
  const cap = Math.max(1, Math.round(Number(maxNodes) || 1));
  const structureZeroCost = options.structureZeroCost === true;
  const frontier = [];
  const visited = new Set();
  const depthById = new Map();
  const bestDepthById = new Map();
  const seeds = Array.isArray(seedIds) ? seedIds : Array.from(seedIds || []);
  const seedSet = new Set();

  seeds.forEach((seedId) => {
    const id = String(seedId || "").trim();
    if (!id) return;
    frontier.push({ id, depth: 0 });
    bestDepthById.set(id, 0);
    seedSet.add(id);
  });

  let cursor = 0;
  while (frontier.length > 0 && visited.size < cap) {
    const current = frontier[cursor];
    frontier[cursor] = null;
    cursor += 1;
    if (!current) break;
    if (current.depth > hops) continue;
    const edgeEntries = direction === "incoming"
      ? index?.incomingVisibleEdgesByNode.get(current.id) || []
      : index?.outgoingVisibleEdgesByNode.get(current.id) || [];
    for (const entry of edgeEntries) {
      const neighborId = entry?.neighborId;
      const edgeKind = String(entry?.kind || "");
      if (!neighborId || seedSet.has(neighborId)) continue;
      const cost = structureZeroCost && (edgeKind === "contains" || edgeKind === "defines") ? 0 : 1;
      const nextDepth = current.depth + cost;
      if (nextDepth > hops) continue;
      const previousDepth = bestDepthById.get(neighborId);
      if (previousDepth !== undefined && previousDepth <= nextDepth) continue;
      bestDepthById.set(neighborId, nextDepth);
      visited.add(neighborId);
      depthById.set(neighborId, nextDepth);
      if (cost === 0 && cursor > 0) {
        cursor -= 1;
        frontier[cursor] = { id: neighborId, depth: nextDepth };
      } else {
        frontier.push({ id: neighborId, depth: nextDepth });
      }
      if (visited.size >= cap) break;
    }
  }

  return { ids: visited, depthById };
}

function buildBlastLayoutPositions(nodes) {
  const positions = new Map();
  const grouped = {
    upstream: new Map(),
    downstream: new Map(),
    center: new Map([[0, []]]),
  };

  nodes.forEach((node) => {
    const side = node.lens_side || "center";
    const depth = Number(node.lens_depth) || 0;
    if (!grouped[side]) return;
    if (!grouped[side].has(depth)) {
      grouped[side].set(depth, []);
    }
    grouped[side].get(depth).push(node);
  });

  grouped.center.get(0).forEach((node) => {
    positions.set(node.id, { x: 0, y: 0 });
  });

  const placeSide = (side, sign) => {
    const depthEntries = Array.from(grouped[side].entries())
      .filter(([depth]) => depth > 0)
      .sort((a, b) => a[0] - b[0]);
    for (const [depth, bucket] of depthEntries) {
      bucket.sort((a, b) => compareNodeLabels(state.graphIndex.nodeById, a.id, b.id));
      const x = sign * depth * BLAST_LAYOUT_X_STEP;
      const total = bucket.length;
      bucket.forEach((node, idx) => {
        const y = (idx - (total - 1) / 2) * BLAST_LAYOUT_Y_STEP;
        positions.set(node.id, { x, y });
      });
    }
  };

  placeSide("upstream", -1);
  placeSide("downstream", 1);
  return positions;
}

function buildBlastLensSlice(graph) {
  const index = state.graphIndex;
  const centerNodeId = String(state.blastCenterNodeId || "").trim();
  const centerNode = index?.nodeById.get(centerNodeId);
  if (!index || !centerNode) {
    state.blastLastSummary = null;
    state.blastLensActive = false;
    setBlastStatus("Blast lens reset: center node is no longer available.", "warn");
    const visibleNodeIds = buildProgressiveVisibleSet();
    return {
      ...buildSliceFromVisibleIds(graph, visibleNodeIds),
      matchedNodeIds: new Set(),
      totalMatches: 0,
      truncated: false,
      blastCenterNodeId: null,
      blastSummary: null,
    };
  }

  const hopCount = clampBlastHops(state.blastHopCount);
  const nodeCap = clampBlastNodeCap(state.blastNodeCap);
  const sideCap = Math.max(20, Math.floor((nodeCap - 1) / 2));
  const structureZeroCost = state.blastIgnoreStructureHops === true;

  const upstream = collectDirectedReachable([centerNodeId], "incoming", hopCount, sideCap, {
    structureZeroCost,
  });
  const downstream = collectDirectedReachable([centerNodeId], "outgoing", hopCount, sideCap, {
    structureZeroCost,
  });
  const visibleNodeIds = new Set([centerNodeId]);
  upstream.ids.forEach((id) => visibleNodeIds.add(id));
  downstream.ids.forEach((id) => visibleNodeIds.add(id));

  const kindVisible = (kind, nodeId) => {
    if (nodeId === centerNodeId) return true;
    if (kind === "crate") return state.blastShowCrate;
    if (kind === "module") return state.blastShowModule;
    if (kind === "file") return state.blastShowFile;
    return true;
  };

  const prefilteredNodes = graph.nodes
    .filter((node) => visibleNodeIds.has(node.id))
    .map((node) => ({
      ...node,
      lens_side: node.id === centerNodeId
        ? "center"
        : upstream.ids.has(node.id)
          ? "upstream"
          : "downstream",
      lens_depth: node.id === centerNodeId
        ? 0
        : upstream.depthById.get(node.id) || downstream.depthById.get(node.id) || 1,
    }));
  const nodes = prefilteredNodes.filter((node) => kindVisible(node.kind, node.id));
  const filteredVisibleNodeIds = new Set(nodes.map((node) => node.id));
  const edges = graph.edges.filter(
    (edge) =>
      VISIBLE_EDGE_KINDS.has(edge.kind) &&
      filteredVisibleNodeIds.has(edge.from) &&
      filteredVisibleNodeIds.has(edge.to)
  );
  const positions = buildBlastLayoutPositions(nodes);
  nodes.forEach((node) => {
    node.lens_position = positions.get(node.id) || { x: 0, y: 0 };
  });

  const summary = {
    centerNodeId,
    centerLabel: centerNode.label || centerNodeId,
    upstreamCount: upstream.ids.size,
    downstreamCount: downstream.ids.size,
    totalNodes: nodes.length,
    totalEdges: edges.length,
    hiddenContainerCount: prefilteredNodes.length - nodes.length,
    truncated: prefilteredNodes.length >= nodeCap,
  };
  state.blastLastSummary = summary;
  return {
    nodes,
    edges,
    matchedNodeIds: new Set([centerNodeId]),
    totalMatches: nodes.length,
    truncated: summary.truncated,
    blastCenterNodeId: centerNodeId,
    blastSummary: summary,
  };
}

function graphSliceForMode(graph, mode) {
  if (state.blastLensActive) {
    return buildBlastLensSlice(graph);
  }

  if (state.impactActive) {
    const impactView = buildImpactVisibleSet();
    return {
      ...buildSliceFromVisibleIds(graph, impactView.visibleNodeIds),
      matchedNodeIds: impactView.matchedNodeIds,
      totalMatches: impactView.totalMatches,
      truncated: impactView.truncated,
    };
  }

  if (
    mode === "progressive" &&
    state.flowGraphFocusActive &&
    state.searchQuery.trim().length === 0 &&
    state.flowVisibleNodeIds instanceof Set &&
    state.flowVisibleNodeIds.size > 0
  ) {
    return {
      ...buildSliceFromVisibleIds(graph, state.flowVisibleNodeIds),
      matchedNodeIds: state.flowTraceNodeIds instanceof Set ? new Set(state.flowTraceNodeIds) : new Set(),
      totalMatches: state.flowTraceNodeIds instanceof Set ? state.flowTraceNodeIds.size : 0,
      truncated: state.flowVisibleTruncated === true,
    };
  }

  const query = state.searchQuery.trim();
  if (query.length > 0) {
    const searchView = buildSearchVisibleSet(query);
    return {
      ...buildSliceFromVisibleIds(graph, searchView.visibleNodeIds),
      matchedNodeIds: searchView.matchedNodeIds,
      totalMatches: searchView.totalMatches,
      truncated: searchView.truncated,
    };
  }

  if (mode === "full") {
    return {
      nodes: graph.nodes,
      edges: graph.edges.filter((edge) => VISIBLE_EDGE_KINDS.has(edge.kind)),
      matchedNodeIds: new Set(),
      totalMatches: 0,
      truncated: false,
    };
  }

  const visibleNodeIds = buildProgressiveVisibleSet();
  return {
    ...buildSliceFromVisibleIds(graph, visibleNodeIds),
    matchedNodeIds: new Set(),
    totalMatches: 0,
    truncated: false,
  };
}

function buildSliceFromVisibleIds(graph, visibleNodeIds) {
  const structuralOnly = state.fullLoadIncludeAll === true;
  const edgeKinds = structuralOnly ? FULL_LOAD_EDGE_KINDS : VISIBLE_EDGE_KINDS;
  const nodes = graph.nodes.filter((node) => visibleNodeIds.has(node.id));
  const edges = graph.edges.filter(
    (edge) =>
      edgeKinds.has(edge.kind) &&
      visibleNodeIds.has(edge.from) &&
      visibleNodeIds.has(edge.to)
  );
  return { nodes, edges };
}

function deriveNodePosition(nodeId, previousPositions) {
  if (!previousPositions || previousPositions.size === 0) {
    return state.hierPositions?.get(nodeId) || null;
  }
  const exact = previousPositions.get(nodeId);
  if (exact) {
    return { x: exact.x, y: exact.y };
  }

  const index = state.graphIndex;
  if (!index) {
    return state.hierPositions?.get(nodeId) || null;
  }

  for (const parentId of index.parentsByChild.get(nodeId) || []) {
    const parentPosition = previousPositions.get(parentId);
    if (!parentPosition) continue;
    const offset = deterministicOffset(nodeId);
    return {
      x: parentPosition.x + offset.x,
      y: parentPosition.y + offset.y,
    };
  }

  const hier = state.hierPositions?.get(nodeId);
  if (hier) {
    return { x: hier.x, y: hier.y };
  }

  const rootId = index.crateNodeIds[0] || index.rootNodeIds[0];
  if (!rootId) {
    return null;
  }
  const rootPosition = previousPositions.get(rootId);
  if (!rootPosition) {
    return null;
  }
  const offset = deterministicOffset(nodeId);
  return {
    x: rootPosition.x + offset.x,
    y: rootPosition.y + offset.y,
  };
}

// Deterministic hierarchical 2-D layout for every node (crates →
// modules → files → symbols), computed once after the graph loads.
// Large renders use it as preset positions instead of running the grid
// layout — the full-graph view keeps the workspace's real structure
// and positions instantly instead of forming a square grid.
// Stable identity for a graph edge — (from, to, kind) is unique in
// graph.json (aggregated with weight), so it works as an append-dedup key.
function graphEdgeKey(edge) {
  return `${edge.from}→${edge.to}::${edge.kind}`;
}

// Frame-chunked element appender for the full-load path. One giant
// cy.add() blocks the main thread for seconds; ~4k elements per frame
// keeps the status line, the Stop button, and the viewport responsive.
function scheduleFullLoadAppendPump() {
  if (state.fullLoadAppendPumpScheduled) return;
  state.fullLoadAppendPumpScheduled = true;
  window.requestAnimationFrame(() => {
    state.fullLoadAppendPumpScheduled = false;
    pumpFullLoadAppendQueue();
  });
}

function pumpFullLoadAppendQueue() {
  if (!state.cy || !state.fullLoadAppendQueue || state.fullLoadAppendQueue.length === 0) {
    return;
  }
  const chunk = state.fullLoadAppendQueue.splice(0, 1500);
  try {
    state.cy.batch(() => state.cy.add(chunk));
  } catch (err) {
    state.fullLoadAppendQueue = [];
    setExpansionStatusText(`Append failed: ${err && err.message ? err.message : err}`, "warn");
    return;
  }
  const remaining = state.fullLoadAppendQueue.length;
  if (remaining > 0) {
    setExpansionStatusText(`Mounting elements… ${remaining.toLocaleString()} queued`, "muted");
    // Skip culling mid-drain: a forced pass rebuilds the index over the
    // whole (growing) element set — quadratic. New nodes render visible
    // until the drain completes, then one cull hides the offscreen ones.
    state.viewportCullDirty = true;
  } else {
    scheduleViewportCulling(state.cy, { force: true });
  }
  renderMeta(state.graph);
  if (remaining > 0) {
    scheduleFullLoadAppendPump();
  }
}

function computeHierarchicalLayout() {
  const graph = state.graph;
  const index = state.graphIndex;
  if (!graph || !index) return;

  const byCrate = new Map();
  for (const node of graph.nodes) {
    const crate = node.crate || "__root__";
    mapListPush(byCrate, crate, node);
  }

  // Golden-angle spiral placement (Vogel's method) — deterministic,
  // evenly spread, no clustering passes needed.
  const vogel = (i, spacing) => {
    const r = spacing * Math.sqrt(i + 0.5);
    const theta = i * 2.39996323;
    return { x: r * Math.cos(theta), y: r * Math.sin(theta) };
  };

  const positions = new Map();
  const crateNames = [...byCrate.keys()];
  const crateSpacing = 620;
  crateNames.forEach((crateName, ci) => {
    const cc = vogel(ci, crateSpacing);
    const nodes = byCrate.get(crateName);

    const byModule = new Map();
    for (const node of nodes) {
      const moduleKey = node.module || node.id;
      mapListPush(byModule, moduleKey, node);
    }
    const moduleKeys = [...byModule.keys()];
    const moduleSpacing = Math.max(150, 26 * Math.sqrt(moduleKeys.length));
    moduleKeys.forEach((moduleKey, mi) => {
      const mc = { x: cc.x + vogel(mi, moduleSpacing).x, y: cc.y + vogel(mi, moduleSpacing).y };
      const moduleNodes = byModule.get(moduleKey);

      const byFile = new Map();
      for (const node of moduleNodes) {
        const fileKey = node.path || node.id;
        mapListPush(byFile, fileKey, node);
      }
      const fileKeys = [...byFile.keys()];
      const fileSpacing = Math.max(60, 12 * Math.sqrt(fileKeys.length));
      fileKeys.forEach((fileKey, fi) => {
        const fp = vogel(fi, fileSpacing);
        const fileNodes = byFile.get(fileKey);
        for (const node of fileNodes) {
          if (node.kind === "crate") {
            positions.set(node.id, { x: cc.x, y: cc.y });
          } else if (node.kind === "module") {
            positions.set(node.id, { x: mc.x, y: mc.y });
          } else if (node.kind === "file") {
            positions.set(node.id, { x: mc.x + fp.x, y: mc.y + fp.y });
          } else {
            const offset = deterministicOffset(node.id);
            positions.set(node.id, {
              x: mc.x + fp.x + offset.x,
              y: mc.y + fp.y + offset.y,
            });
          }
        }
      });
    });
  });

  state.hierPositions = positions;
}

function layoutForMode(mode, nodeCount, fitGraph = true, options = {}) {
  const searchActive = options.searchActive === true;
  const impactActive = options.impactActive === true;
  const overview = options.overview === true;

  if (mode === "full") {
    return {
      name: "grid",
      animate: false,
      fit: fitGraph,
      avoidOverlap: true,
      condense: false,
      avoidOverlapPadding: 20,
      spacingFactor: 2.1,
      rows: Math.ceil(Math.sqrt(nodeCount)),
    };
  }

  if (overview) {
    // Overview = the landing view (progressive mode, no expansions /
    // search / impact / blast active). The built-in `cose` looks like
    // a uniform grid when edges are sparse (the ~30 crate nodes only
    // share ~8 `depends_on` edges, so there's no force to break the
    // initial spacing). Prefer `cose-bilkent` when its plugin is on
    // the page — it does proper clustering for sparse compound graphs.
    const hasBilkent =
      typeof cytoscape !== "undefined" &&
      typeof cytoscape.use === "function" &&
      typeof window !== "undefined" &&
      window.cytoscapeCoseBilkent;
    if (hasBilkent) {
      return {
        name: "cose-bilkent",
        animate: false,
        fit: fitGraph,
        padding: 80,
        randomize: true,           // ignore grid-like start positions
        nodeRepulsion: 4500,
        idealEdgeLength: 110,
        edgeElasticity: 0.45,      // bilkent uses 0..1 not 0..100
        gravity: 0.45,             // stronger pull toward centre
        gravityRange: 3.8,
        numIter: 2500,
        tile: true,                // place disconnected components without overlap
        tilingPaddingVertical: 24,
        tilingPaddingHorizontal: 24,
        nodeDimensionsIncludeLabels: true,
      };
    }
    // Fallback when the CDN plugin failed to load — tuned built-in
    // `cose` with weaker repulsion / stronger gravity so disconnected
    // nodes still cluster instead of spreading evenly.
    return {
      name: "cose",
      animate: false,
      fit: fitGraph,
      padding: 80,
      randomize: true,
      nodeRepulsion: 3500,
      idealEdgeLength: 90,
      edgeElasticity: 100,
      gravity: 0.55,
      numIter: 800,
      componentSpacing: 60,
    };
  }

  const allowCose = impactActive
    ? nodeCount <= 1800
    : (!searchActive && nodeCount <= 900) || (searchActive && nodeCount <= 420);
  if (allowCose) {
    const iter = impactActive ? (nodeCount > 1200 ? 240 : 300) : (searchActive ? 320 : 360);
    const repulsion = impactActive ? 7600 : (searchActive ? 8200 : 7000);
    const idealLength = impactActive ? 128 : (searchActive ? 135 : 120);
    return {
      name: "cose",
      animate: false,
      fit: fitGraph,
      padding: 80,
      nodeRepulsion: repulsion,
      idealEdgeLength: idealLength,
      edgeElasticity: 70,
      gravity: 0.26,
      numIter: iter,
      randomize: false,
      componentSpacing: 95,
    };
  }

  return {
    name: "grid",
    animate: false,
    fit: fitGraph,
    avoidOverlap: true,
    condense: false,
    avoidOverlapPadding: 22,
    spacingFactor: 2.3,
    rows: Math.ceil(Math.sqrt(nodeCount)),
  };
}

function updateRenderModeToggle(graph) {
  const toggleButton = el("toggle-full-graph");
  if (!toggleButton || !graph) return;

  if (state.impactRevealSession) {
    toggleButton.disabled = true;
    toggleButton.textContent = "Impact Reveal Running...";
    return;
  }

  if (state.impactActive) {
    toggleButton.disabled = true;
    toggleButton.textContent = "Impact Mode Active";
    return;
  }

  if (state.blastLensActive) {
    toggleButton.disabled = true;
    toggleButton.textContent = "Blast Lens Active";
    return;
  }

  if (state.fullLoadSession) {
    toggleButton.disabled = true;
    toggleButton.textContent = "Loading Full Graph...";
    return;
  }

  if (state.stagedRevealSession) {
    toggleButton.disabled = true;
    toggleButton.textContent = "Expansion in progress...";
    return;
  }

  const fullModeAllowed = graph.nodes.length <= FULL_GRAPH_ENABLE_NODE_LIMIT;
  const searchActive = state.searchQuery.trim().length > 0;
  const progressiveLoadAllowed = fullLoadStepDelayMs() >= FULL_LOAD_MIN_SPEED_MS;
  if (state.renderMode === "progressive") {
    if (fullModeAllowed) {
      toggleButton.disabled = false;
      toggleButton.textContent = "Load Full Graph";
    } else {
      if (searchActive) {
        toggleButton.disabled = true;
        toggleButton.textContent = "Load Full (clear search)";
      } else if (!progressiveLoadAllowed) {
        toggleButton.disabled = true;
        toggleButton.textContent = `Load Full (Reveal >= ${FULL_LOAD_MIN_SPEED_MS}ms)`;
      } else {
        toggleButton.disabled = false;
        toggleButton.textContent = "Load Full Progressive";
      }
    }
    return;
  }

  toggleButton.disabled = false;
  toggleButton.textContent = "Use Progressive View";
}

function renderKindLegend(nodes) {
  const legend = el("kind-legend");
  if (!legend) return;
  const kindStyleMap = currentKindStyleMap();
  const kinds = Array.from(new Set(nodes.map((node) => node.kind))).sort();
  if (kinds.length === 0) {
    legend.textContent = "No kinds available.";
    return;
  }

  const parts = kinds.map((kind) => {
    const style = kindStyleMap[kind] || {
      fill: "#6aa58d",
      border: "#3d6f5d",
      text: "#102219",
    };
    return (
      `<div class="kind-legend-item">` +
      `<span class="kind-legend-swatch" style="background:${style.fill};border-color:${style.border};"></span>` +
      `<span>${escapeHtml(kind)}</span>` +
      `</div>`
    );
  });
  legend.innerHTML = parts.join("");
}

function setBlastStatus(message, tone = "muted") {
  const status = el("blast-status");
  if (!status) return;
  status.textContent = message;
  status.dataset.tone = tone;
}

function clearLiveImpactState(options = {}) {
  state.liveImpactSeedNodeIds = new Set();
  state.liveImpactUpstreamIds = new Set();
  state.liveImpactDownstreamIds = new Set();
  state.liveImpactChangedSymbolCount = 0;
  if (options.rerender === true && state.graph) {
    renderGraph(state.renderMode, { preserveViewport: true, focusSearch: false });
  }
  const summary = el("editor-impact-summary");
  if (summary) {
    summary.textContent = "No live structural impact yet.";
  }
}

function applyLiveImpactClasses(cy = state.cy) {
  if (!cy) return;
  cy.nodes().removeClass("live-impact-seed live-impact-upstream live-impact-downstream");
  if (
    state.liveImpactSeedNodeIds.size === 0 &&
    state.liveImpactUpstreamIds.size === 0 &&
    state.liveImpactDownstreamIds.size === 0
  ) {
    return;
  }
  cy.batch(() => {
    cy.nodes().forEach((node) => {
      const nodeId = node.id();
      if (state.liveImpactSeedNodeIds.has(nodeId)) {
        node.addClass("live-impact-seed");
      } else if (state.liveImpactUpstreamIds.has(nodeId)) {
        node.addClass("live-impact-upstream");
      } else if (state.liveImpactDownstreamIds.has(nodeId)) {
        node.addClass("live-impact-downstream");
      }
    });
  });
}

function syncBlastControls() {
  const toggleButton = el("toggle-blast-lens");
  const clearButton = el("clear-blast-lens");
  const focusButton = el("focus-blast-selection");
  const hopSelect = el("blast-hop-count");
  const capSelect = el("blast-node-cap");
  const ignoreStructureToggle = el("blast-ignore-structure-hops");
  const showCrateToggle = el("blast-show-crate");
  const showModuleToggle = el("blast-show-module");
  const showFileToggle = el("blast-show-file");

  state.blastHopCount = clampBlastHops(state.blastHopCount);
  state.blastNodeCap = clampBlastNodeCap(state.blastNodeCap);
  const blastLocked = state.impactLoading || state.impactActive;

  if (hopSelect) {
    hopSelect.value = String(state.blastHopCount);
    hopSelect.disabled = blastLocked;
  }
  if (capSelect) {
    capSelect.value = String(state.blastNodeCap);
    capSelect.disabled = blastLocked;
  }
  if (toggleButton) {
    toggleButton.textContent = state.blastLensActive ? "Blast Lens: On" : "Blast Lens: Off";
    toggleButton.disabled = blastLocked;
  }
  if (clearButton) {
    clearButton.disabled = blastLocked || !state.blastLensActive;
  }
  if (focusButton) {
    focusButton.disabled = blastLocked || !state.selectedNodeId;
  }
  if (ignoreStructureToggle) {
    ignoreStructureToggle.checked = state.blastIgnoreStructureHops;
    ignoreStructureToggle.disabled = blastLocked;
  }
  if (showCrateToggle) {
    showCrateToggle.checked = state.blastShowCrate;
    showCrateToggle.disabled = blastLocked;
  }
  if (showModuleToggle) {
    showModuleToggle.checked = state.blastShowModule;
    showModuleToggle.disabled = blastLocked;
  }
  if (showFileToggle) {
    showFileToggle.checked = state.blastShowFile;
    showFileToggle.disabled = blastLocked;
  }
}

function setBlastLensActive(active, options = {}) {
  const nextActive = active === true;
  const requestedCenterId = String(options.centerNodeId || "").trim();
  const centerNodeId = requestedCenterId || state.selectedNodeId || state.blastCenterNodeId;
  if (nextActive && !centerNodeId) {
    setBlastStatus("Select a node first to enable blast lens.", "warn");
    syncBlastControls();
    return false;
  }

  const rerender = options.rerender !== false;
  state.blastLensActive = nextActive;
  if (nextActive) {
    state.blastCenterNodeId = centerNodeId;
    if (state.searchQuery.trim().length > 0) {
      state.searchQuery = "";
      const searchInput = el("search-input");
      if (searchInput) {
        searchInput.value = "";
      }
    }
    if (state.impactActive) {
      clearImpactAnalysis({ syncUrl: false });
    }
    setBlastStatus(`Blast lens active around "${centerNodeId}".`, "ok");
  } else {
    state.blastCenterNodeId = null;
    state.blastLastSummary = null;
    setBlastStatus("Blast lens off.", "muted");
  }
  syncBlastControls();
  if (rerender && state.graph) {
    renderGraph(state.renderMode, {
      preserveViewport: false,
      focusSearch: false,
    });
  }
  return true;
}

function setImpactStatus(message, tone = "muted") {
  const status = el("impact-status");
  if (!status) return;
  status.textContent = message;
  status.dataset.tone = tone;
}

function populateImpactBaseRefs(refs, selectedRef = "") {
  const select = el("impact-base-ref");
  if (!select) return;

  const normalizedRefs = Array.isArray(refs)
    ? refs
        .map((ref) => String(ref || "").trim())
        .filter((ref, index, arr) => ref.length > 0 && arr.indexOf(ref) === index)
        .sort((a, b) => a.localeCompare(b))
    : [];

  select.innerHTML = "";
  if (normalizedRefs.length === 0) {
    const option = document.createElement("option");
    option.value = "";
    option.textContent = "No refs available";
    select.appendChild(option);
    select.disabled = true;
    return;
  }

  normalizedRefs.forEach((ref) => {
    const option = document.createElement("option");
    option.value = ref;
    option.textContent = ref;
    select.appendChild(option);
  });

  const desiredRef = normalizedRefs.includes(selectedRef)
    ? selectedRef
    : normalizedRefs[0];
  select.value = desiredRef;
  select.disabled = false;
  state.impactBaseRef = desiredRef;
}

function syncImpactControls() {
  const runButton = el("run-impact");
  const clearButton = el("clear-impact");
  const baseSelect = el("impact-base-ref");
  const compareSelect = el("impact-compare-mode");
  const hopsSelect = el("impact-hop-count");

  const hasBase = String(state.impactBaseRef || "").trim().length > 0;
  const hasRefs = Array.isArray(state.gitRefs) && state.gitRefs.length > 0;
  const disabled = state.impactLoading || !state.impactApiAvailable || !hasRefs;

  if (baseSelect) {
    baseSelect.disabled = disabled || !hasRefs;
    if (!baseSelect.disabled && hasBase && baseSelect.value !== state.impactBaseRef) {
      baseSelect.value = state.impactBaseRef;
    }
  }
  if (compareSelect) {
    compareSelect.disabled = disabled;
    const mode = normalizeImpactCompareMode(state.impactCompareMode);
    if (compareSelect.value !== mode) {
      compareSelect.value = mode;
    }
  }
  if (hopsSelect) {
    hopsSelect.disabled = disabled;
    const hops = String(clampImpactHops(state.impactHopCount));
    if (hopsSelect.value !== hops) {
      hopsSelect.value = hops;
    }
  }
  if (runButton) {
    runButton.disabled = disabled || !hasBase;
    runButton.textContent = state.impactLoading ? "Running..." : "Run Impact";
  }
  if (clearButton) {
    clearButton.disabled = state.impactLoading || !state.impactActive;
  }
  syncBlastControls();
}

function computeImpactNodeSets(changedFiles, hopCount) {
  const index = state.graphIndex;
  if (!index) {
    return {
      changedFileNodeIds: new Set(),
      impactedNodeIds: new Set(),
      unmatchedFiles: [],
    };
  }

  const changedFileNodeIds = new Set();
  const unmatchedFiles = [];
  const seenChanged = new Set();
  for (const rawPath of changedFiles || []) {
    const path = String(rawPath || "").trim();
    if (!path || seenChanged.has(path)) continue;
    seenChanged.add(path);
    const fileNodeId = index.fileNodeIdByPath.get(path);
    if (fileNodeId) {
      changedFileNodeIds.add(fileNodeId);
    } else {
      unmatchedFiles.push(path);
    }
  }

  const impactedNodeIds = new Set(changedFileNodeIds);
  if (changedFileNodeIds.size === 0) {
    return {
      changedFileNodeIds,
      impactedNodeIds,
      unmatchedFiles,
    };
  }

  let frontier = new Set(changedFileNodeIds);
  const hops = clampImpactHops(hopCount);
  for (let hopIndex = 0; hopIndex < hops; hopIndex += 1) {
    const nextFrontier = new Set();
    for (const nodeId of frontier) {
      const neighbors = index.relationNeighborsByNode.get(nodeId) || [];
      for (const neighborId of neighbors) {
        if (impactedNodeIds.has(neighborId)) continue;
        impactedNodeIds.add(neighborId);
        nextFrontier.add(neighborId);
        if (impactedNodeIds.size >= IMPACT_NODE_CAP) {
          break;
        }
      }
      if (impactedNodeIds.size >= IMPACT_NODE_CAP) {
        break;
      }
    }
    if (nextFrontier.size === 0 || impactedNodeIds.size >= IMPACT_NODE_CAP) {
      break;
    }
    frontier = nextFrontier;
  }

  addAncestorParents(impactedNodeIds, IMPACT_NODE_CAP);
  return {
    changedFileNodeIds,
    impactedNodeIds,
    unmatchedFiles,
  };
}

function buildImpactRevealPlan(impactedNodeIds, changedFileNodeIds) {
  const index = state.graphIndex;
  if (!index) {
    return {
      initialVisibleNodeIds: new Set(),
      revealNodeIds: [],
    };
  }
  if (!(impactedNodeIds instanceof Set) || impactedNodeIds.size === 0) {
    return {
      initialVisibleNodeIds: new Set(),
      revealNodeIds: [],
    };
  }

  const keepVisible = new Set();
  const queue = [];
  if (changedFileNodeIds instanceof Set) {
    changedFileNodeIds.forEach((nodeId) => {
      keepVisible.add(nodeId);
      queue.push(nodeId);
    });
  }

  while (queue.length > 0 && keepVisible.size < IMPACT_NODE_CAP) {
    const current = queue.pop();
    for (const parentId of index.parentsByChild.get(current) || []) {
      if (keepVisible.has(parentId)) continue;
      keepVisible.add(parentId);
      queue.push(parentId);
      if (keepVisible.size >= IMPACT_NODE_CAP) {
        break;
      }
    }
  }

  if (keepVisible.size === 0) {
    const fallback = sortNodeIdsForReveal(Array.from(impactedNodeIds)).slice(0, 12);
    fallback.forEach((nodeId) => {
      keepVisible.add(nodeId);
    });
  }

  const revealNodeIds = [];
  impactedNodeIds.forEach((nodeId) => {
    if (!keepVisible.has(nodeId)) {
      revealNodeIds.push(nodeId);
    }
  });
  return {
    initialVisibleNodeIds: keepVisible,
    revealNodeIds: sortNodeIdsForReveal(revealNodeIds),
  };
}

function stopImpactReveal(options = {}) {
  const emitStatus = options.emitStatus === true;
  const tone = options.tone || "warn";
  const message = options.message || "Impact reveal stopped.";
  if (state.impactRevealTimeoutHandle !== null) {
    window.clearTimeout(state.impactRevealTimeoutHandle);
    state.impactRevealTimeoutHandle = null;
  }
  state.impactRevealSession = null;
  state.deferInsightsActive = false;
  syncTransitionLod();
  updateLoadingControls();
  if (emitStatus) {
    setExpansionStatusText(message, tone);
  }
}

function finishImpactReveal(session) {
  if (!session || state.impactRevealSession !== session) return;
  if (state.impactRevealTimeoutHandle !== null) {
    window.clearTimeout(state.impactRevealTimeoutHandle);
    state.impactRevealTimeoutHandle = null;
  }
  state.impactRevealSession = null;
  updateLoadingControls();
  setExpansionStatusText(`Impact reveal complete: ${session.total}/${session.total}.`, "grow");
  state.deferInsightsActive = false;
  scheduleInsightsRefresh({
    immediate: true,
    preferIdle: false,
    applyGraphFocus: true,
  });
  syncTransitionLod();
  scheduleViewportCulling(state.cy, { force: true });
  kickGlobalRepulsion(recommendedGlobalRepelSettleFrames(30));
  window.setTimeout(() => {
    if (!state.cy) return;
    kickGlobalRepulsion(recommendedGlobalRepelSettleFrames(24));
  }, 220);
}

function scheduleImpactRevealTick(delayMs) {
  if (state.impactRevealTimeoutHandle !== null) {
    window.clearTimeout(state.impactRevealTimeoutHandle);
    state.impactRevealTimeoutHandle = null;
  }
  if (!state.impactRevealSession) return;
  const delay = Math.max(0, Number(delayMs) || 0);
  state.impactRevealTimeoutHandle = window.setTimeout(() => {
    state.impactRevealTimeoutHandle = null;
    runImpactRevealTick();
  }, delay);
}

function runImpactRevealTick() {
  const session = state.impactRevealSession;
  if (!session || !state.impactActive || !state.graph) {
    stopImpactReveal({ emitStatus: false });
    return;
  }

  const delayMs = stagedRevealDelayMs();
  if (delayMs === 0) {
    session.queue.forEach((nodeId) => {
      state.impactVisibleNodeIds.add(nodeId);
    });
    session.revealed = session.total;
    renderGraph("progressive", {
      preserveViewport: false,
      focusSearch: false,
      deferInsights: false,
    });
    finishImpactReveal(session);
    return;
  }

  const batchSize = delayMs >= 260 ? 1 : delayMs >= 140 ? 2 : delayMs >= 80 ? 3 : 5;
  const frameBudgetMs = revealFrameBudgetMs(delayMs, state.renderedNodeCount);
  const frameStart = performance.now();
  let applied = 0;
  while (applied < batchSize && session.queue.length > 0) {
    if (applied > 0 && performance.now() - frameStart >= frameBudgetMs) {
      break;
    }
    const nextNodeId = session.queue.shift();
    if (!nextNodeId) continue;
    state.impactVisibleNodeIds.add(nextNodeId);
    session.revealed += 1;
    applied += 1;
  }

  if (applied === 0) {
    finishImpactReveal(session);
    return;
  }

  const done = session.queue.length === 0;
  setExpansionStatusText(`Impact reveal: ${session.revealed}/${session.total}`, "grow");
  renderGraph("progressive", {
    preserveViewport: !done,
    focusSearch: false,
    deferInsights: !done,
  });
  scheduleViewportCulling(state.cy);

  if (done) {
    finishImpactReveal(session);
    return;
  }
  scheduleImpactRevealTick(delayMs);
}

function startImpactReveal(nodeIds) {
  stopImpactReveal({ emitStatus: false });
  if (!state.impactActive || !Array.isArray(nodeIds) || nodeIds.length === 0) {
    return;
  }
  const queue = nodeIds.slice();
  const session = {
    queue,
    total: queue.length,
    revealed: 0,
  };
  state.impactRevealSession = session;
  syncTransitionLod();
  updateLoadingControls();
  setExpansionStatusText(`Impact reveal: 0/${session.total}`, "grow");
  scheduleImpactRevealTick(0);
}

function clearImpactState(options = {}) {
  const rerender = options.rerender === true;
  const syncUrl = options.syncUrl !== false;
  stopImpactReveal({ emitStatus: false });
  state.impactActive = false;
  state.impactChangedFiles = [];
  state.impactChangedFileNodeIds = new Set();
  state.impactNodeIds = new Set();
  state.impactVisibleNodeIds = new Set();
  if (rerender && state.graph) {
    renderGraph(state.renderMode, { preserveViewport: false, focusSearch: false });
  } else if (state.graph) {
    updateRenderModeToggle(state.graph);
  }
  if (syncUrl) {
    writeUrlViewState();
  }
  syncImpactControls();
}

function setMakeStatus(message, tone = "info") {
  const status = el("make-status");
  if (!status) return;
  status.textContent = message;
  status.dataset.tone = tone;
}

function setMakeOutput(text) {
  const output = el("make-output");
  if (!output) return;
  output.textContent = text;
}

function populateMakeTargets(targets) {
  const select = el("make-target");
  if (!select) return;

  select.innerHTML = "";
  for (const target of targets) {
    const option = document.createElement("option");
    option.value = target;
    option.textContent = target;
    select.appendChild(option);
  }
}

function formatDurationMs(durationMs) {
  if (typeof durationMs !== "number" || Number.isNaN(durationMs)) {
    return "unknown";
  }
  if (durationMs < 1000) return `${durationMs}ms`;
  return `${(durationMs / 1000).toFixed(2)}s`;
}

async function loadMakeTargets() {
  const runButton = el("run-make");
  const select = el("make-target");
  if (!runButton || !select) return;

  try {
    const response = await fetch("/api/make/targets", { cache: "no-store" });
    if (!response.ok) {
      throw new Error(`HTTP ${response.status}`);
    }
    const payload = await response.json();
    const targets = Array.isArray(payload.targets)
      ? payload.targets
          .map((entry) => (entry && typeof entry.name === "string" ? entry.name.trim() : ""))
          .filter((name) => name.length > 0)
      : [];

    if (targets.length === 0) {
      throw new Error("No make targets returned by server");
    }

    state.makeTargets = targets;
    state.makeRunnerAvailable = true;
    populateMakeTargets(targets);
    runButton.disabled = false;
    select.disabled = false;
    setMakeStatus("Make runner ready.", "ok");
  } catch (_err) {
    // Keep a visible fallback list to explain what can be run when the
    // graph page is served without the dev server.
    state.makeTargets = FALLBACK_MAKE_TARGETS.slice();
    state.makeRunnerAvailable = false;
    populateMakeTargets(state.makeTargets);
    runButton.disabled = true;
    select.disabled = true;
    setMakeStatus("Make API unavailable. Start with `make graph-serve`.", "warn");
  }
}

async function runSelectedMakeTarget() {
  if (state.isRunningMake || !state.makeRunnerAvailable) return;
  const runButton = el("run-make");
  const select = el("make-target");
  if (!runButton || !select) return;

  const target = String(select.value || "").trim();
  if (!target) {
    setMakeStatus("Choose a make target first.", "warn");
    return;
  }

  state.isRunningMake = true;
  runButton.disabled = true;
  select.disabled = true;
  setMakeStatus(`Running \`make ${target}\`...`, "running");
  setMakeOutput(`$ make ${target}\n\nRunning...`);

  try {
    const response = await fetch("/api/make/run", {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
      },
      body: JSON.stringify({ target }),
    });

    const payload = await response.json().catch(() => ({}));
    if (!response.ok) {
      const message =
        typeof payload.message === "string"
          ? payload.message
          : `Request failed with HTTP ${response.status}`;
      throw new Error(message);
    }

    const parts = [`$ make ${payload.target || target}`];
    parts.push(
      `exit_code=${String(payload.exit_code)} duration=${formatDurationMs(payload.duration_ms)} timed_out=${String(payload.timed_out)}`
    );
    if (typeof payload.stdout === "string" && payload.stdout.length > 0) {
      parts.push("STDOUT:");
      parts.push(payload.stdout);
    }
    if (typeof payload.stderr === "string" && payload.stderr.length > 0) {
      parts.push("STDERR:");
      parts.push(payload.stderr);
    }
    if (parts.length <= 2) {
      parts.push("(no output)");
    }

    setMakeOutput(parts.join("\n\n"));
    if (payload.exit_code === 0 && payload.timed_out === false) {
      setMakeStatus(`make ${target} succeeded in ${formatDurationMs(payload.duration_ms)}.`, "ok");
    } else {
      setMakeStatus(
        `make ${target} finished with exit code ${String(payload.exit_code)} in ${formatDurationMs(payload.duration_ms)}.`,
        "error"
      );
    }
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    setMakeStatus(`Run failed: ${message}`, "error");
    setMakeOutput(`$ make ${target}\n\nError: ${message}`);
  } finally {
    state.isRunningMake = false;
    runButton.disabled = !state.makeRunnerAvailable;
    select.disabled = !state.makeRunnerAvailable;
  }
}

async function initMakeRunner() {
  const runButton = el("run-make");
  const clearButton = el("clear-make-output");
  if (!runButton || !clearButton) return;

  runButton.disabled = true;
  runButton.addEventListener("click", () => {
    void runSelectedMakeTarget();
  });
  clearButton.addEventListener("click", () => {
    setMakeOutput("Run a make target to view output.");
    setMakeStatus(
      state.makeRunnerAvailable ? "Make runner ready." : "Make API unavailable. Start with `make graph-serve`.",
      state.makeRunnerAvailable ? "ok" : "warn"
    );
  });

  await loadMakeTargets();
}

async function loadImpactRefs(preferredBaseRef = "") {
  const fallbackSet = new Set(["origin/main", "origin/master", "main", "master"]);
  try {
    const response = await fetch("/api/git/refs", { cache: "no-store" });
    if (!response.ok) {
      throw new Error(`HTTP ${response.status}`);
    }
    const payload = await response.json();
    const refs = Array.isArray(payload.refs)
      ? payload.refs
          .map((ref) => String(ref || "").trim())
          .filter((ref, index, arr) => ref.length > 0 && arr.indexOf(ref) === index)
      : [];
    state.gitRefs = refs;
    state.gitCurrentBranch = String(payload.current_branch || "").trim();
    state.gitDefaultBaseRef = String(payload.default_base_ref || "").trim();
    state.impactApiAvailable = refs.length > 0;

    const desiredCandidates = [
      preferredBaseRef,
      state.impactBaseRef,
      state.gitDefaultBaseRef,
      "origin/main",
      "main",
      refs[0] || "",
    ]
      .map((value) => String(value || "").trim())
      .filter((value, index, arr) => value.length > 0 && arr.indexOf(value) === index);
    const selectedRef = desiredCandidates.find((ref) => refs.includes(ref)) || "";
    state.impactBaseRef = selectedRef;
    populateImpactBaseRefs(refs, selectedRef);
    if (selectedRef.length > 0) {
      setImpactStatus(`Base ref ready: ${selectedRef}`, "muted");
    } else {
      setImpactStatus("No base refs available from git.", "warn");
    }
  } catch (_err) {
    state.gitRefs = [];
    state.gitCurrentBranch = "";
    state.gitDefaultBaseRef = "";
    state.impactApiAvailable = false;

    const fallbackRefs = Array.from(fallbackSet);
    const fallbackBase = String(preferredBaseRef || state.impactBaseRef || "origin/main").trim();
    const selectedRef = fallbackRefs.includes(fallbackBase) ? fallbackBase : "origin/main";
    state.impactBaseRef = selectedRef;
    populateImpactBaseRefs(fallbackRefs, selectedRef);
    setImpactStatus("Git impact API unavailable. Start with `make graph-serve`.", "warn");
  } finally {
    syncImpactControls();
    syncFlowControls();
  }
}

async function runImpactAnalysis(options = {}) {
  const syncUrl = options.syncUrl !== false;
  if (!state.graph || !state.graphIndex) return;
  if (state.impactLoading) return;
  if (state.blastLensActive) {
    setBlastLensActive(false, { rerender: false });
    setBlastStatus("Blast lens disabled because impact mode is active.", "warn");
  }

  const baseRef = String(state.impactBaseRef || "").trim();
  if (!baseRef) {
    setImpactStatus("Choose a base ref first.", "warn");
    return;
  }

  const compareMode = normalizeImpactCompareMode(state.impactCompareMode);
  const hopCount = clampImpactHops(state.impactHopCount);

  state.impactLoading = true;
  stopImpactReveal({ emitStatus: false });
  syncImpactControls();
  setImpactStatus(`Computing impact vs ${baseRef} (${compareMode})...`, "muted");

  try {
    const response = await fetch("/api/git/impact", {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
      },
      body: JSON.stringify({
        base_ref: baseRef,
        compare_mode: compareMode,
      }),
    });
    const payload = await response.json().catch(() => ({}));
    if (!response.ok) {
      const message =
        typeof payload.message === "string"
          ? payload.message
          : `Impact request failed (HTTP ${response.status})`;
      throw new Error(message);
    }

    const changedFiles = Array.isArray(payload.changed_files)
      ? payload.changed_files
          .map((value) => String(value || "").trim())
          .filter((value, index, arr) => value.length > 0 && arr.indexOf(value) === index)
      : [];
    state.impactChangedFiles = changedFiles;

    const { changedFileNodeIds, impactedNodeIds, unmatchedFiles } = computeImpactNodeSets(
      changedFiles,
      hopCount
    );
    state.impactChangedFileNodeIds = changedFileNodeIds;
    state.impactNodeIds = impactedNodeIds;

    const changedFileCount = changedFiles.length;
    const matchedFileCount = changedFileNodeIds.size;
    const impactedNodeCount = impactedNodeIds.size;
    const unmatchedCount = unmatchedFiles.length;
    state.impactActive = impactedNodeCount > 0;

    if (state.impactActive && state.searchQuery.trim().length > 0) {
      applySearch("", { syncUrl: false });
      const searchInput = el("search-input");
      if (searchInput) {
        searchInput.value = "";
      }
    }

    if (state.impactActive) {
      stopActiveLoading();
      const revealPlan = buildImpactRevealPlan(
        impactedNodeIds,
        changedFileNodeIds
      );
      state.impactVisibleNodeIds = revealPlan.initialVisibleNodeIds;
      setExpansionStatusText(
        `Impact mode active: ${impactedNodeCount} nodes from ${matchedFileCount}/${changedFileCount} changed files.`,
        "grow"
      );
      setImpactStatus(
        `Impact active: ${impactedNodeCount} nodes • ${matchedFileCount}/${changedFileCount} files mapped${unmatchedCount > 0 ? ` • ${unmatchedCount} unmatched` : ""}`,
        unmatchedCount > 0 ? "warn" : "ok"
      );
      renderGraph("progressive", {
        preserveViewport: false,
        focusSearch: false,
        deferInsights: revealPlan.revealNodeIds.length > 0,
      });
      if (revealPlan.revealNodeIds.length > 0) {
        startImpactReveal(revealPlan.revealNodeIds);
      }
    } else if (changedFileCount === 0) {
      clearImpactState({ rerender: false, syncUrl: false });
      setImpactStatus(`No changed files vs ${baseRef} (${compareMode}).`, "ok");
    } else {
      clearImpactState({ rerender: false, syncUrl: false });
      setImpactStatus(
        `Changed files found (${changedFileCount}), but none mapped to graph file nodes.`,
        "warn"
      );
    }
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    clearImpactState({ rerender: false, syncUrl: false });
    setImpactStatus(`Impact run failed: ${message}`, "error");
  } finally {
    state.impactLoading = false;
    if (syncUrl) {
      writeUrlViewState();
    }
    syncImpactControls();
  }
}

function clearImpactAnalysis(options = {}) {
  const syncUrl = options.syncUrl !== false;
  const wasActive = state.impactActive;
  clearImpactState({ rerender: false, syncUrl: false });
  if (wasActive && state.graph) {
    renderGraph(state.renderMode, { preserveViewport: false, focusSearch: false });
  }
  setImpactStatus("Impact mode off.", "muted");
  if (syncUrl) {
    writeUrlViewState();
  }
}

function collapseExpansionBranch(nodeId) {
  const index = state.graphIndex;
  if (!index) return;

  const queue = [nodeId];
  const seen = new Set();
  while (queue.length > 0) {
    const current = queue.pop();
    if (seen.has(current)) continue;
    seen.add(current);

    state.expandedModuleNodes.delete(current);
    state.expandedFileNodes.delete(current);
    state.expandedSymbolNodes.delete(current);
    state.expandedRelationNodes.delete(current);
    state.expansionLevelByNode.delete(current);

    for (const moduleId of index.moduleChildrenByParent.get(current) || []) {
      queue.push(moduleId);
    }
    for (const fileId of index.fileChildrenByParent.get(current) || []) {
      queue.push(fileId);
    }
    for (const symbolId of index.symbolChildrenByParent.get(current) || []) {
      queue.push(symbolId);
    }
  }
}

function toggleProgressiveExpansion(nodeId, kind) {
  // Expansion state changed: the cached slice no longer reflects the
  // graph (visibleNodeSetForCurrentView must observe the mutation).
  state.lastSlice = null;
  if (kind === "crate") {
    if (state.expandedModuleNodes.has(nodeId)) {
      collapseExpansionBranch(nodeId);
      return { action: "collapse", kind };
    } else {
      state.expandedModuleNodes.add(nodeId);
      state.expansionLevelByNode.set(nodeId, 1);
      return { action: "expand_modules", kind };
    }
  }

  if (kind === "module") {
    const currentLevel = state.expansionLevelByNode.get(nodeId) || 0;
    if (currentLevel === 0) {
      state.expandedFileNodes.add(nodeId);
      state.expansionLevelByNode.set(nodeId, 1);
      return { action: "expand_files", kind };
    } else if (currentLevel === 1) {
      state.expandedSymbolNodes.add(nodeId);
      state.expansionLevelByNode.set(nodeId, 2);
      return { action: "expand_symbols", kind };
    } else {
      collapseExpansionBranch(nodeId);
      return { action: "collapse", kind };
    }
  }

  if (kind === "file") {
    if (state.expandedSymbolNodes.has(nodeId)) {
      state.expandedSymbolNodes.delete(nodeId);
      state.expansionLevelByNode.delete(nodeId);
      return { action: "collapse", kind };
    } else {
      state.expandedSymbolNodes.add(nodeId);
      state.expansionLevelByNode.set(nodeId, 1);
      return { action: "expand_symbols", kind };
    }
  }

  if (state.expandedRelationNodes.has(nodeId)) {
    state.expandedRelationNodes.delete(nodeId);
    return { action: "collapse_relations", kind };
  }

  state.expandedRelationNodes.add(nodeId);
  return { action: "expand_relations", kind };
}

function visibleNodeSetForCurrentView() {
  if (!state.graph) return new Set();
  const slice =
    state.lastSlice && state.lastSliceMode === state.renderMode
      ? state.lastSlice
      : graphSliceForMode(state.graph, state.renderMode);
  return new Set(slice.nodes.map((node) => node.id));
}

function renderGraph(mode, options = {}) {
  const graph = state.graph;
  const index = state.graphIndex;
  if (!graph || !index) return;

  const preserveViewport = options.preserveViewport === true;
  const focusSearch = options.focusSearch !== false;
  const deferInsights = options.deferInsights === true;
  const onStagedRevealComplete = typeof options.onStagedRevealComplete === "function"
    ? options.onStagedRevealComplete
    : null;
  state.deferInsightsActive = deferInsights;
  const expansionContext = options.expansionContext || null;
  const stagedRevealNodeIds = Array.isArray(options.stagedRevealNodeIds) ? options.stagedRevealNodeIds : [];
  const queryActive = state.searchQuery.trim().length > 0;
  const impactActive = state.impactActive === true;
  const hadPreviousCy = !!state.cy;
  const previousViewport =
    preserveViewport && state.cy
      ? { zoom: state.cy.zoom(), pan: state.cy.pan() }
      : null;

  const renderGraphT0 = performance.now();
  const slice = graphSliceForMode(graph, mode);
  const sliceMs = performance.now() - renderGraphT0;
  const blastActive = state.blastLensActive === true && String(slice.blastCenterNodeId || "").length > 0;
  const degreeById = index.degreeById;
  const degreeScoreById = new Map();
  let maxDegreeScore = 1;
  for (const node of slice.nodes) {
    const degree = Math.max(0, degreeById.get(node.id) || 0);
    const degreeScore = degree > 0 ? Math.pow(degree, NODE_SIZE_DEGREE_EXPONENT) : 0;
    degreeScoreById.set(node.id, degreeScore);
    if (degreeScore > maxDegreeScore) {
      maxDegreeScore = degreeScore;
    }
  }
  const degreeMs = performance.now() - renderGraphT0 - sliceMs;
  const compactView = mode === "progressive";
  const minNodeSize = compactView ? 10 : 8;
  const maxNodeSize = compactView ? 58 : 36;
  const minFontSize = compactView ? 7 : 6;
  const maxFontSize = compactView ? 13 : 10;

  state.renderMode = mode;
  state.lastSlice = slice;
  state.lastSliceMode = mode;
  state.renderedNodeCount = slice.nodes.length;
  state.renderedEdgeCount = slice.edges.length;
  state.renderedFunctionCount = slice.nodes.filter((node) => node.kind === "function").length;
  state.searchTotalMatches = slice.totalMatches || 0;
  state.searchTruncated = slice.truncated === true;

  // ── Fast delta path (before ANY O(mounted) Cytoscape work) ──
  // Delta detection uses a plain Set of mounted node ids. Reading
  // positions/ids through Cytoscape accessors for the whole mount (the
  // old previousPositions Map) cost seconds at 100k elements — that Map
  // is what made every expansion click hang the fully-loaded graph.
  const blastActiveEarly = state.blastLensActive === true;
  if (
    state.cy &&
    state.renderedNodeIds &&
    state.renderedNodeIds.size > 0 &&
    !blastActiveEarly &&
    !impactActive &&
    mode === "progressive"
  ) {
    const mountedIds = state.renderedNodeIds;
    const visibleIdSet = new Set(slice.nodes.map((node) => node.id));
    const enteredNodeIds = [];
    for (const node of slice.nodes) {
      if (!mountedIds.has(node.id)) enteredNodeIds.push(node.id);
    }
    const exitedNodeIds = [];
    for (const id of mountedIds) {
      if (!visibleIdSet.has(id)) exitedNodeIds.push(id);
    }

    const renderedEdgeInfo = state.renderedEdgeInfo || new Map();
    const enteredEdges = [];
    for (const edge of slice.edges) {
      if (!renderedEdgeInfo.has(graphEdgeKey(edge))) enteredEdges.push(edge);
    }
    const exitedEdgeIds = [];
    for (const [key, info] of renderedEdgeInfo) {
      if (visibleIdSet.has(info.from) && visibleIdSet.has(info.to)) continue;
      exitedEdgeIds.push(info.id);
      renderedEdgeInfo.delete(key);
    }

    // Edge removals ride along with node removals in Cytoscape — they
    // are bookkeeping, not work, so only nodes and entered edges count
    // toward the delta budget.
    const deltaSize = enteredNodeIds.length + exitedNodeIds.length + enteredEdges.length;
    // Staged-reveal animation is skipped on this path: applying the
    // delta directly is the difference between a fast click and a
    // multi-second full rebuild of a 100k-element mount.
    const smallDelta =
      deltaSize > 0 &&
      deltaSize <= 12000 &&
      deltaSize <= Math.max(200, mountedIds.size * 0.5);

    if (smallDelta) {
      const enteredIdSet = new Set(enteredNodeIds);
      const nextLegend = () => {
        state.legendCounter = (state.legendCounter || 0) + 1;
        return String(state.legendCounter);
      };
      const enrichNode = (node) => ({
        ...node,
        full_label: node.label,
        label: nextLegend(),
        legend: node.label,
        degree: degreeById.get(node.id) || 0,
        degree_score: degreeScoreById.get(node.id) || 0,
        lens_side: "",
        lens_depth: 0,
        lens_position: null,
      });
      const newNodeElements = [];
      for (const node of slice.nodes) {
        if (!enteredIdSet.has(node.id)) continue;
        const hier = state.hierPositions ? state.hierPositions.get(node.id) : null;
        const position = hier || { x: 0, y: 0 };
        newNodeElements.push({
          data: enrichNode(node),
          position: { x: position.x, y: position.y },
        });
      }
      const newEdgeElements = enteredEdges.map((edge) => {
        const key = graphEdgeKey(edge);
        const elementId = `e-inc-${(state.incrementalEdgeIdCounter = (state.incrementalEdgeIdCounter || 0) + 1)}`;
        renderedEdgeInfo.set(key, { id: elementId, from: edge.from, to: edge.to });
        return { data: { id: elementId, source: edge.from, target: edge.to, ...edge } };
      });
      state.renderedEdgeInfo = renderedEdgeInfo;
      state.renderedEdgeKeys = new Set(renderedEdgeInfo.keys());
      appendEdgesToLengthIndex(newEdgeElements);

      state.cy.batch(() => {
        if (exitedEdgeIds.length || exitedNodeIds.length) {
          const removeList = [];
          for (const edgeId of exitedEdgeIds) {
            const el = state.cy.getElementById(edgeId);
            if (el.nonempty()) removeList.push(el);
          }
          for (const nodeId of exitedNodeIds) {
            const el = state.cy.getElementById(nodeId);
            if (el.nonempty()) removeList.push(el);
          }
          if (removeList.length) state.cy.remove(state.cy.collection(removeList));
        }
        if (newNodeElements.length || newEdgeElements.length) {
          state.cy.add([...newNodeElements, ...newEdgeElements]);
        }
      });
      for (const id of enteredNodeIds) mountedIds.add(id);
      for (const id of exitedNodeIds) mountedIds.delete(id);
      appendToViewportCullIndex(state.cy, newNodeElements, exitedNodeIds);
      perfMark(
        `render-delta slice ${sliceMs} degree ${degreeMs} delta ${performance.now() - renderGraphT0 - sliceMs - degreeMs}`,
        performance.now() - renderGraphT0
      );
      renderMeta(graph);
      return;
    }

    // Append-only deltas beyond the sync budget (full-load phases,
    // expansions with many edges) go through the chunked pump rather
    // than a full rebuild.
    if (
      exitedNodeIds.length === 0 &&
      slice.nodes.length >= mountedIds.size
    ) {
      const nextLegend2 = () => {
        state.legendCounter = (state.legendCounter || 0) + 1;
        return String(state.legendCounter);
      };
      const enrichNode = (node) => ({
        ...node,
        full_label: node.label,
        label: nextLegend2(),
        legend: node.label,
        degree: degreeById.get(node.id) || 0,
        degree_score: degreeScoreById.get(node.id) || 0,
        lens_side: "",
        lens_depth: 0,
        lens_position: null,
      });
      const newNodeElements = [];
      for (const node of slice.nodes) {
        if (mountedIds.has(node.id)) continue;
        const hier = state.hierPositions ? state.hierPositions.get(node.id) : null;
        const position = hier || { x: 0, y: 0 };
        newNodeElements.push({
          data: enrichNode(node),
          position: { x: position.x, y: position.y },
        });
      }
      const newEdgeElements = enteredEdges.map((edge) => {
        const key = graphEdgeKey(edge);
        const elementId = `e-inc-${(state.incrementalEdgeIdCounter = (state.incrementalEdgeIdCounter || 0) + 1)}`;
        renderedEdgeInfo.set(key, { id: elementId, from: edge.from, to: edge.to });
        return { data: { id: elementId, source: edge.from, target: edge.to, ...edge } };
      });
      state.renderedEdgeInfo = renderedEdgeInfo;
      state.renderedEdgeKeys = new Set(renderedEdgeInfo.keys());
      for (const node of newNodeElements) mountedIds.add(node.data.id);
      appendEdgesToLengthIndex(newEdgeElements);
      if (newNodeElements.length || newEdgeElements.length) {
        state.fullLoadAppendQueue = (state.fullLoadAppendQueue || []).concat(
          newNodeElements,
          newEdgeElements
        );
        scheduleFullLoadAppendPump();
      }
      renderMeta(graph);
      return;
    }
  }

  // Full rebuild path: positions for layout continuity are only cheap
  // enough to harvest on small mounts — at scale the preset layouts
  // don't need them, and building the Map was the click hang.
  const previousPositions =
    state.cy && state.cy.nodes().length <= 25000
      ? new Map(state.cy.nodes().map((node) => [node.id(), node.position()]))
      : null;

  const nodesWithLegend = slice.nodes.map((node, indexInSlice) => ({
    ...node,
    full_label: node.label,
    label: String(indexInSlice + 1),
    legend: String(indexInSlice + 1),
    degree: degreeById.get(node.id) || 0,
    degree_score: degreeScoreById.get(node.id) || 0,
    lens_side: node.lens_side || "",
    lens_depth: Number(node.lens_depth) || 0,
    lens_position: node.lens_position || null,
  }));

  const previousNodeCount = previousPositions ? previousPositions.size : 0;
  const addedNodeCount = Math.max(0, nodesWithLegend.length - previousNodeCount);
  const fullLoadFastPath =
    !!state.fullLoadSession &&
    preserveViewport &&
    !!previousPositions &&
    previousPositions.size > 0;
  // Large renders (search/expansion results past the force-layout caps,
  // or full mode) use the precomputed hierarchical layout as preset
  // positions instead of falling back to a meaningless square grid.
  const structurePresetActive =
    !blastActive &&
    !impactActive &&
    (mode === "full" ||
      slice.nodes.length > (queryActive ? 420 : 900)) &&
      !!(state.hierPositions && state.hierPositions.size > 0);
  const expansionInteractionFastPath =
    !!expansionContext &&
    hadPreviousCy &&
    !!previousPositions &&
    previousPositions.size > 0;
  const stagedExpansionFastPath =
    stagedRevealNodeIds.length > 0 &&
    !!expansionContext &&
    !expansionContext.action.startsWith("collapse") &&
    hadPreviousCy &&
    !!previousPositions &&
    previousPositions.size > 0;
  const usePresetLayout =
    blastActive ||
    fullLoadFastPath ||
    expansionInteractionFastPath ||
    stagedExpansionFastPath ||
    structurePresetActive ||
    preserveViewport &&
    !!previousPositions &&
    previousPositions.size > 0 &&
    addedNodeCount <= 90;
  const useIncrementalGridLayout = preserveViewport && !usePresetLayout && !impactActive && !blastActive;
  // Structure renders normalize hierarchical positions into a compact
  // box: the workspace layout spans ±15k units, so fitting a scattered
  // search slice at world scale zooms out until nodes are subpixel.
  // Normalization keeps the relative cluster structure at readable size.
  let structurePositionById = null;
  if (structurePresetActive) {
    let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
    const rawPositions = new Map();
    for (const node of slice.nodes) {
      const p = state.hierPositions.get(node.id) || { x: 0, y: 0 };
      rawPositions.set(node.id, p);
      if (p.x < minX) minX = p.x;
      if (p.y < minY) minY = p.y;
      if (p.x > maxX) maxX = p.x;
      if (p.y > maxY) maxY = p.y;
    }
    if (rawPositions.size > 0 && minX !== Infinity) {
      const span = Math.max(maxX - minX, maxY - minY, 1);
      const scale = 2400 / span;
      const cx = (minX + maxX) / 2;
      const cy = (minY + maxY) / 2;
      structurePositionById = new Map();
      for (const [id, p] of rawPositions) {
        structurePositionById.set(id, { x: (p.x - cx) * scale, y: (p.y - cy) * scale });
      }
    }
  }

  const elements = [
    ...nodesWithLegend.map((node) => {
      const element = { data: node };
      if (usePresetLayout) {
        let position = null;
        if (blastActive) {
          position = node.lens_position || { x: 0, y: 0 };
        } else if (structurePositionById) {
          position =
            structurePositionById.get(node.id) ||
            deriveNodePosition(node.id, previousPositions);
        } else if (fullLoadFastPath) {
          position =
            previousPositions?.get(node.id) ||
            state.hierPositions?.get(node.id) ||
            deriveNodePosition(node.id, previousPositions);
        } else {
          position = deriveNodePosition(node.id, previousPositions);
        }
        if (position) {
          element.position = position;
        }
      }
      return element;
    }),
    ...slice.edges.map((edge, idx) => ({
      data: { id: `e-${idx}`, source: edge.from, target: edge.to, ...edge },
    })),
  ];

  if (state.cy) {
    clearNodeTooltipTimers();
    hideNodeTooltip();
    stopGlobalRepulsionLoop();
    stopFlowPlayback();
    stopStagedReveal();
    clearViewportCullingSchedulers();
    clearScheduledInsightsRefresh();
    state.cy.destroy();
    state.cy = null;
    state.viewportCullingActive = false;
    state.transitionLodActive = false;
    state.fullLoadAppendQueue = [];
  }

  const kindStyleMap = currentKindStyleMap();
  const graphTheme = currentGraphTheme();
  const kindStyles = Object.entries(kindStyleMap).map(([kind, style]) => ({
    selector: `node[kind = '${kind}']`,
    style: {
      "background-color": style.fill,
      "border-color": style.border,
      color: style.text,
    },
  }));

  const isOverview =
    mode === "progressive" &&
    !impactActive &&
    !blastActive &&
    !queryActive &&
    state.expandedModuleNodes.size === 0 &&
    state.expandedFileNodes.size === 0 &&
    state.expandedSymbolNodes.size === 0 &&
    state.expandedRelationNodes.size === 0;

  const targetLayout = blastActive
    ? {
        name: "preset",
        fit: false,
        padding: 0,
        animate: false,
      }
    : usePresetLayout
    ? {
        name: "preset",
        fit: false,
        padding: 0,
        animate: false,
      }
    : useIncrementalGridLayout
      ? {
          name: "grid",
          animate: false,
          fit: false,
          avoidOverlap: true,
          condense: false,
          avoidOverlapPadding: 18,
          spacingFactor: 1.8,
          rows: Math.ceil(Math.sqrt(slice.nodes.length)),
        }
      : layoutForMode(mode, slice.nodes.length, false, {
          searchActive: queryActive,
          impactActive,
          overview: isOverview,
        });

  const renderedEdgeInfo = new Map();
  slice.edges.forEach((edge, idx) => {
    renderedEdgeInfo.set(graphEdgeKey(edge), { id: `e-${idx}`, from: edge.from, to: edge.to });
  });
  state.renderedEdgeInfo = renderedEdgeInfo;
  state.renderedEdgeKeys = new Set(renderedEdgeInfo.keys());
  state.renderedNodeIds = new Set(slice.nodes.map((node) => node.id));
  rebuildEdgeLengthIndex();
  perfMark(
    `render-rebuild slice ${sliceMs} degree ${degreeMs} enrich+elements ${performance.now() - renderGraphT0 - sliceMs - degreeMs}`,
    performance.now() - renderGraphT0
  );
  const cy = cytoscape({
    container: el("cy"),
    elements,
    style: [
      {
        selector: "node",
        style: {
          label: "data(label)",
          width: `mapData(degree_score, 0, ${maxDegreeScore}, ${minNodeSize}, ${maxNodeSize})`,
          height: `mapData(degree_score, 0, ${maxDegreeScore}, ${minNodeSize}, ${maxNodeSize})`,
          "font-size": `mapData(degree_score, 0, ${maxDegreeScore}, ${minFontSize}, ${maxFontSize})`,
          color: graphTheme.defaultNodeText,
          "font-weight": 700,
          "text-valign": "center",
          "text-halign": "center",
          "border-width": 1,
        },
      },
      ...kindStyles,
      {
        selector: "edge",
        style: {
          width: 0.9,
          "line-color": graphTheme.edgeLine,
          "target-arrow-color": graphTheme.edgeArrow,
          "target-arrow-shape": "triangle",
          "curve-style": "bezier",
          opacity: 0.8,
        },
      },
      {
        selector: ".matched",
        style: {
          "border-width": 3,
          "border-color": graphTheme.matchedBorder,
        },
      },
      {
        selector: ".mermaid-focused-node",
        style: {
          "border-width": 2.8,
          "border-color": graphTheme.mermaidFocusNodeBorder,
          "shadow-color": graphTheme.mermaidFocusNodeShadow,
          "shadow-opacity": 0.48,
          "shadow-blur": 18,
          "shadow-offset-x": 0,
          "shadow-offset-y": 0,
        },
      },
      {
        selector: ".mermaid-focused-edge",
        style: {
          width: 2,
          opacity: 1,
          "line-color": graphTheme.mermaidFocusEdge,
          "target-arrow-color": graphTheme.mermaidFocusEdge,
        },
      },
      {
        selector: ".graph-focus-flash-node",
        style: {
          shape: "ellipse",
          "border-width": 4.2,
          "border-color": graphTheme.mermaidFocusNodeBorder,
          "shadow-color": graphTheme.mermaidFocusNodeShadow,
          "shadow-opacity": 0.72,
          "shadow-blur": 20,
          "z-index": 1200,
        },
      },
      {
        selector: ".graph-focus-flash-node-peak",
        style: {
          "border-width": 9.4,
          "shadow-opacity": 0.98,
          "shadow-blur": 72,
        },
      },
      {
        selector: ".graph-focus-flash-edge",
        style: {
          width: 2.9,
          opacity: 1,
          "line-color": graphTheme.mermaidFocusEdge,
          "target-arrow-color": graphTheme.mermaidFocusEdge,
        },
      },
      {
        selector: ".graph-focus-flash-edge-peak",
        style: {
          width: 6.2,
          "line-color": graphTheme.mermaidFocusNodeShadow,
          "target-arrow-color": graphTheme.mermaidFocusNodeShadow,
        },
      },
      {
        selector: ".expansion-origin",
        style: {
          "border-width": 2.2,
          "border-color": graphTheme.expansionOriginBorder,
          "background-color": graphTheme.expansionOriginFill,
          color: graphTheme.expansionOriginText,
          "shadow-color": graphTheme.expansionOriginShadow,
          "shadow-opacity": 0.45,
          "shadow-blur": 16,
          "shadow-offset-x": 0,
          "shadow-offset-y": 0,
          "z-index": 999,
        },
      },
      {
        selector: ".expansion-neighbor",
        style: {
          "background-color": graphTheme.expansionNeighborFill,
          color: graphTheme.expansionNeighborText,
          "font-weight": 700,
        },
      },
      {
        selector: ".expansion-edge-outgoing",
        style: {
          width: 0.9,
          opacity: 0.96,
          "line-color": graphTheme.expansionEdgeOutgoing,
          "target-arrow-color": graphTheme.expansionEdgeOutgoing,
        },
      },
      {
        selector: ".expansion-edge-incoming",
        style: {
          width: 0.9,
          opacity: 0.96,
          "line-color": graphTheme.expansionEdgeIncoming,
          "target-arrow-color": graphTheme.expansionEdgeIncoming,
        },
      },
      {
        selector: "node[lens_side = 'center']",
        style: {
          "border-width": 3.2,
          "border-color": "#f4b400",
          "shadow-color": "#f4b400",
          "shadow-opacity": 0.38,
          "shadow-blur": 16,
          "z-index": 1150,
        },
      },
      {
        selector: "node[lens_side = 'upstream']",
        style: {
          "border-width": 2.1,
          "border-color": "#f08a24",
        },
      },
      {
        selector: "node[lens_side = 'downstream']",
        style: {
          "border-width": 2.1,
          "border-color": "#2e9bff",
        },
      },
      {
        selector: ".graph-focus-flash-node",
        style: {
          shape: "ellipse",
          "border-width": 4.2,
          "border-color": graphTheme.mermaidFocusNodeBorder,
          "shadow-color": graphTheme.mermaidFocusNodeShadow,
          "shadow-opacity": 0.72,
          "shadow-blur": 20,
          "z-index": 1200,
        },
      },
      {
        selector: ".graph-focus-flash-node-peak",
        style: {
          "border-width": 9.4,
          "shadow-opacity": 0.98,
          "shadow-blur": 72,
        },
      },
      {
        selector: ".graph-focus-flash-edge",
        style: {
          width: 2.9,
          opacity: 1,
          "line-color": graphTheme.mermaidFocusEdge,
          "target-arrow-color": graphTheme.mermaidFocusEdge,
        },
      },
      {
        selector: ".graph-focus-flash-edge-peak",
        style: {
          width: 6.2,
          "line-color": graphTheme.mermaidFocusNodeShadow,
          "target-arrow-color": graphTheme.mermaidFocusNodeShadow,
        },
      },
      {
        selector: ".staged-pending",
        style: {
          opacity: 0,
          "text-opacity": 0,
          "background-opacity": 0,
          "border-opacity": 0,
        },
      },
      {
        selector: ".staged-edge-pending",
        style: {
          opacity: 0,
        },
      },
      {
        selector: ".transition-lod-node",
        style: {
          "text-opacity": 0,
          "border-width": 0.7,
        },
      },
      {
        selector: ".transition-lod-edge",
        style: {
          width: 0.45,
          opacity: 0.16,
          "target-arrow-shape": "none",
        },
      },
      {
        selector: ".offscreen-node",
        style: {
          // display:none (not just dimming): offscreen elements must be
          // skipped by the renderer entirely — at 100k+ elements the
          // raster cost of "almost invisible" elements still dominates
          // every pan/zoom frame.
          display: "none",
          events: "no",
        },
      },
      {
        selector: ".overview-nolabel",
        style: {
          label: "",
        },
      },
      {
        selector: ".offscreen-edge",
        style: {
          display: "none",
          "target-arrow-shape": "none",
          events: "no",
        },
      },
      {
        selector: ".edge-span-culled",
        style: {
          display: "none",
          "target-arrow-shape": "none",
          events: "no",
        },
      },
      {
        selector: ".live-impact-seed",
        style: {
          "border-width": 3.3,
          "border-color": "#f4b400",
          "shadow-color": "#f4b400",
          "shadow-opacity": 0.34,
          "shadow-blur": 14,
        },
      },
      {
        selector: ".live-impact-upstream",
        style: {
          "border-width": 2.4,
          "border-color": "#f08a24",
        },
      },
      {
        selector: ".live-impact-downstream",
        style: {
          "border-width": 2.4,
          "border-color": "#2e9bff",
        },
      },
      {
        selector: ".flow-trace-source",
        style: {
          "border-width": 3.8,
          "border-color": graphTheme.flowTraceSourceBorder,
          "shadow-color": graphTheme.flowTraceSourceGlow,
          "shadow-opacity": 0.5,
          "shadow-blur": 20,
          "shadow-offset-x": 0,
          "shadow-offset-y": 0,
          "z-index": 1160,
        },
      },
      {
        selector: ".flow-trace-node",
        style: {
          "border-width": 2.3,
          "border-color": graphTheme.flowTraceNodeBorder,
        },
      },
      {
        selector: ".flow-trace-sink",
        style: {
          "border-width": 3.1,
          "border-color": graphTheme.flowTraceSinkBorder,
          "shadow-color": graphTheme.flowTraceSinkBorder,
          "shadow-opacity": 0.35,
          "shadow-blur": 14,
          "shadow-offset-x": 0,
          "shadow-offset-y": 0,
        },
      },
      {
        selector: ".flow-trace-edge",
        style: {
          width: 2.2,
          opacity: 0.96,
          "line-color": graphTheme.flowTraceEdge,
          "target-arrow-color": graphTheme.flowTraceEdge,
        },
      },
      {
        selector: ".flow-play-node",
        style: {
          "border-width": 3.6,
          "border-color": graphTheme.flowPlaybackNodeBorder,
          "shadow-color": graphTheme.flowPlaybackNodeGlow,
          "shadow-opacity": 0.56,
          "shadow-blur": 18,
          "shadow-offset-x": 0,
          "shadow-offset-y": 0,
          "z-index": 1170,
        },
      },
      {
        selector: ".flow-play-node-peak",
        style: {
          "border-width": 6.2,
          "shadow-opacity": 0.92,
          "shadow-blur": 34,
        },
      },
      {
        selector: ".flow-play-edge",
        style: {
          width: 3,
          opacity: 1,
          "line-color": graphTheme.flowPlaybackEdge,
          "target-arrow-color": graphTheme.flowPlaybackEdge,
        },
      },
      {
        selector: ".flow-play-edge-peak",
        style: {
          width: 5.2,
        },
      },
      {
        selector: ".faded",
        style: {
          opacity: 0.08,
        },
      },
      {
        selector: ".kind-filtered",
        style: {
          opacity: 0.08,
        },
      },
    ],
    layout: {
      name: "preset",
      fit: false,
      animate: false,
    },
  });

  state.cy = cy;
  if (previousViewport) {
    cy.zoom(previousViewport.zoom);
    cy.pan(previousViewport.pan);
  }

  cy.on("select", "node", (evt) => {
    const node = evt.target;
    state.selectedNodeId = node.id();
    setDetails(node);
    syncBlastControls();
    writeUrlViewState();
    // Always refresh insights — the focus model now wins over the
    // search model when a node is selected, so the mermaid needs to
    // re-render even with a search query still in the box.
    scheduleInsightsRefresh({ debounceMs: 80 });
  });

  cy.on("unselect", "node", () => {
    if (cy.$("node:selected").length === 0) {
      state.selectedNodeId = null;
      setDetails(null);
      syncBlastControls();
      writeUrlViewState();
      scheduleInsightsRefresh({ debounceMs: 80 });
    }
  });

  cy.on("tap", "node", (evt) => {
    const tappedNode = evt.target;
    const nodeId = tappedNode.id();
    const tapTrace = { start: performance.now() };
    let mark = (label) => {
      tapTrace[label] = performance.now();
    };
    mark("pre");
    setMermaidFocusNode(nodeId, {
      zoomGraph: false,
      selectNode: false,
      forceCenterMermaid: true,
      ensureMermaidZoom: true,
    });
    mark("focus");
    state.selectedNodeId = nodeId;
    setDetails(tappedNode);
    mark("details");
    syncBlastControls();
    writeUrlViewState();
    mark("url");
    scheduleInsightsRefresh({ debounceMs: 70 });
    showTransientNodeTooltip(tappedNode, evt.originalEvent, 1200);
    mark("tooltip");
    state.tapTrace = tapTrace;

    if (state.blastLensActive) {
      state.blastCenterNodeId = nodeId;
      setBlastStatus(`Blast lens recentered on "${nodeId}".`, "ok");
      renderGraph("progressive", {
        preserveViewport: false,
        focusSearch: false,
      });
      return;
    }

    if (state.renderMode !== "progressive") return;

    const kind = index.nodeById.get(nodeId)?.kind;
    if (!kind) {
      setExpansionStatusText("This node type is not expandable.", "muted");
      return;
    }
    if (state.impactActive) {
      setExpansionStatusText("Impact mode active: clear impact to use progressive expansion.", "warn");
      return;
    }
    const searchActive = state.searchQuery.trim().length > 0;
    if (searchActive && CONTAINER_KINDS.has(kind)) {
      setExpansionStatusText("Search active: container expansion is paused; click symbols to expand callers/usages.", "warn");
      return;
    }

    const beforeVisible = visibleNodeSetForCurrentView();
    mark("beforeSet");
    const actionInfo = toggleProgressiveExpansion(nodeId, kind);
    const afterVisible = visibleNodeSetForCurrentView();
    mark("afterSet");
    const expansionContext = buildExpansionContext(beforeVisible, afterVisible, nodeId, actionInfo);
    writeUrlViewState();
    mark("context");
    const tone =
      expansionContext.addedCount > 0
        ? "grow"
        : expansionContext.removedCount > 0
          ? "shrink"
          : "muted";
    setExpansionStatus(expansionContext, tone);
    // Staged reveal animates 1-2 nodes per tick — on a large mount that
    // is minutes of saturated frames during which pan/zoom feel dead.
    // Reveal instantly instead when the graph is big.
    const stagingViable = !state.cy || state.cy.nodes().length <= 20000;
    const stagedRevealNodeIds =
      stagingViable &&
      !expansionContext.action.startsWith("collapse") &&
      expansionContext.addedCount > 1
        ? expansionContext.addedNodeIds
        : [];

    // Expansion rerender recreates Mermaid SVG; queue a post-render refocus so
    // pan/zoom/highlight still lands on the clicked node in the refreshed diagram.
    queueMermaidFocusSync({
      forceCenter: true,
      ensureZoom: true,
      flash: true,
      minRenderToken: state.mermaidRenderToken + 1,
    });

    renderGraph("progressive", {
      preserveViewport: false,
      focusSearch: false,
      expansionContext,
      stagedRevealNodeIds,
      deferInsights: stagedRevealNodeIds.length > 0,
    });
    mark("render");
    reportTapTrace(tapTrace);
    scheduleTransientNodeTooltip(nodeId, 180, 1300);
  });

  cy.on("tap", "edge", (evt) => {
    const edge = evt.target;
    const fromId = edge.source().id();
    const toId = edge.target().id();
    setMermaidFocusEdge(fromId, toId, {
      zoomGraph: false,
      forceCenterMermaid: true,
      ensureMermaidZoom: true,
    });
  });

  cy.on("tap", (evt) => {
    if (evt.target !== cy) return;
    clearMermaidFocus();
  });

  cy.on("mouseover", "node", (evt) => {
    showNodeTooltip(evt.target, evt.originalEvent);
  });
  cy.on("mousemove", "node", (evt) => {
    placeNodeTooltip(evt.originalEvent);
  });
  cy.on("mouseout", "node", () => {
    hideNodeTooltip();
  });
  cy.on("grab", "node", () => {
    kickGlobalRepulsion();
  });
  cy.on("drag", "node", (evt) => {
    kickGlobalRepulsion();
    placeNodeTooltip(evt.originalEvent);
  });
  cy.on("free", "node", () => {
    kickGlobalRepulsion();
  });
  cy.on("pan zoom resize", () => {
    scheduleViewportCulling(cy);
  });

  buildKindFilter(nodesWithLegend);
  applyKindFilter(state.kindFilter, { syncUrl: false });
  renderKindLegend(nodesWithLegend);
  updateRenderModeToggle(graph);

  const matchedIds = slice.matchedNodeIds || new Set();
  let matchedNodes = cy.collection();
  if (matchedIds.size > 0) {
    matchedNodes = cy.nodes().filter((node) => matchedIds.has(node.id()));
    matchedNodes.addClass("matched");
    state.searchShownMatches = matchedNodes.length;
  } else {
    state.searchShownMatches = 0;
  }
  applyLiveImpactClasses(cy);
  if (state.flowLastView) {
    applyFlowHighlights(state.flowLastView, String(state.flowSourceId || ""));
  }
  if (!deferInsights) {
    const shouldImmediateInsights = slice.nodes.length <= INSIGHTS_IMMEDIATE_NODE_LIMIT;
    scheduleInsightsRefresh({
      immediate: shouldImmediateInsights,
      debounceMs: shouldImmediateInsights ? 0 : INSIGHTS_DEBOUNCE_MS,
      preferIdle: !shouldImmediateInsights,
      applyGraphFocus: true,
    });
  }

  if (state.selectedNodeId) {
    const selectedNode = cy.$id(state.selectedNodeId);
    if (selectedNode && !selectedNode.empty()) {
      selectedNode.select();
      setDetails(selectedNode);
    } else {
      state.selectedNodeId = null;
      setDetails(null);
      syncBlastControls();
      writeUrlViewState();
    }
  } else {
    setDetails(null);
    syncBlastControls();
  }

  if (expansionContext) {
    applyExpansionFocus(cy, expansionContext);
  }
  if (stagedRevealNodeIds.length > 0) {
    startStagedReveal(cy, stagedRevealNodeIds, expansionContext, {
      deferInsights,
      onComplete: onStagedRevealComplete,
    });
  } else if (onStagedRevealComplete) {
    window.setTimeout(() => {
      if (state.cy !== cy) return;
      onStagedRevealComplete(cy);
    }, 0);
  }
  if (blastActive && slice.blastSummary) {
    const summary = slice.blastSummary;
    const hiddenPart = summary.hiddenContainerCount > 0
      ? ` • hidden ${summary.hiddenContainerCount}`
      : "";
    const hopModePart = state.blastIgnoreStructureHops ? " • smart-hops" : "";
    setBlastStatus(
      `Center "${summary.centerLabel}" • upstream ${summary.upstreamCount} • downstream ${summary.downstreamCount} • nodes ${summary.totalNodes}${summary.truncated ? "+" : ""}${hiddenPart}${hopModePart}`,
      summary.truncated ? "warn" : "ok"
    );
  } else if (!state.blastLensActive) {
    setBlastStatus("Blast lens off.", "muted");
  }
  syncTransitionLod(cy);
  const expansionFocusElements = expansionContext
    ? buildExpansionFocusElements(cy, expansionContext)
    : cy.collection();

  runLayoutAndFit(cy, targetLayout, {
    preserveViewport,
    focusSearch,
    matchedNodes,
    focusElements: expansionFocusElements,
  });

  const shouldAutoRepel =
    mode === "progressive" &&
    !preserveViewport &&
    slice.nodes.length > 1 &&
    slice.nodes.length <= GLOBAL_REPEL_MAX_NODES;
  if (shouldAutoRepel) {
    const settleFrames = recommendedGlobalRepelSettleFrames(expansionContext ? 28 : 18);
    kickGlobalRepulsion(settleFrames);
    window.setTimeout(() => {
      if (state.cy !== cy) return;
      kickGlobalRepulsion(settleFrames);
    }, 220);
  }
  scheduleViewportCulling(cy, { force: true });
  window.setTimeout(() => {
    if (state.cy !== cy) return;
    scheduleViewportCulling(cy, { force: true });
  }, 260);

  renderMeta(graph);
}

function bindGraphControls() {
  if (state.graphControlsBound) return;
  state.graphControlsBound = true;

  const searchInput = el("search-input");
  const kindFilter = el("kind-filter");
  const resetButton = el("reset-view");
  const resetNodesButton = el("reset-nodes");
  const toggleButton = el("toggle-full-graph");
  const themeToggle = el("theme-toggle");
  const toggleOptionsRowsButton = el("toggle-options-rows");
  const toggleInfoRowsButton = el("toggle-info-rows");
  const stopLoadingButton = el("stop-loading");
  const toggleMermaidSplitButton = el("toggle-mermaid-split");
  const toggleRightPanelButton = el("toggle-right-panel");
  const toggleRightPanelInlineButton = el("toggle-right-panel-inline");
  const detailsResizeHandle = el("details-resize-handle");
  const detailsTabButtons = Array.from(document.querySelectorAll("[data-details-tab]"));
  const closeMermaidSplitButton = el("close-mermaid-split");
  const openMermaidModalButton = el("open-mermaid-modal");
  const openMermaidModalInlineButton = el("open-mermaid-modal-inline");
  const closeMermaidModalButton = el("close-mermaid-modal");
  const mermaidModalOverlay = el("mermaid-modal-overlay");
  const mermaidZoomInButton = el("mermaid-zoom-in");
  const mermaidZoomOutButton = el("mermaid-zoom-out");
  const mermaidZoomResetButton = el("mermaid-zoom-reset");
  const mermaidModalZoomInButton = el("mermaid-modal-zoom-in");
  const mermaidModalZoomOutButton = el("mermaid-modal-zoom-out");
  const mermaidModalZoomResetButton = el("mermaid-modal-zoom-reset");
  const copyMermaidButton = el("copy-search-mermaid");
  const mermaidSourceModeSelect = el("mermaid-source-mode");
  const revealSpeed = el("reveal-speed");
  const fitTightness = el("fit-tightness");
  const impactBaseRefSelect = el("impact-base-ref");
  const impactCompareModeSelect = el("impact-compare-mode");
  const impactHopCountSelect = el("impact-hop-count");
  const runImpactButton = el("run-impact");
  const clearImpactButton = el("clear-impact");
  const blastHopCountSelect = el("blast-hop-count");
  const blastNodeCapSelect = el("blast-node-cap");
  const toggleBlastLensButton = el("toggle-blast-lens");
  const focusBlastSelectionButton = el("focus-blast-selection");
  const clearBlastLensButton = el("clear-blast-lens");
  const blastIgnoreStructureHopsToggle = el("blast-ignore-structure-hops");
  const blastShowCrateToggle = el("blast-show-crate");
  const blastShowModuleToggle = el("blast-show-module");
  const blastShowFileToggle = el("blast-show-file");
  const editorPathInput = el("editor-file-path");
  const editorLoadButton = el("editor-load-file");
  const editorSaveButton = el("editor-save-file");
  const editorRefreshButton = el("editor-refresh-file");
  const editorAutoRefreshToggle = el("editor-auto-refresh");
  const flowSourceSelect = el("flow-source-select");
  const flowRefreshSourcesButton = el("flow-refresh-sources");
  const flowLoadMockButton = el("flow-load-mock");
  const flowRunButton = el("flow-run");
  const flowRunDeltaButton = el("flow-run-delta");
  const flowExportJsonButton = el("flow-export-json");
  const flowExportMdButton = el("flow-export-md");
  const flowMaxHopsSelect = el("flow-max-hops");
  const flowOnlyOutboundToggle = el("flow-only-outbound");
  const flowOnlyCrossCrateToggle = el("flow-only-cross-crate");
  const flowIncludeContainersToggle = el("flow-include-containers");
  const guideSourceSelect = el("guide-source-select");
  const guideRunSourceButton = el("guide-run-source");
  const guideTraceDirectionSelect = el("guide-trace-direction");
  const guideTraceSelect = el("guide-trace-select");
  const guideBuildButton = el("guide-build");
  const guidePlayTraceButton = el("guide-play-trace");
  const guideOpenFlowButton = el("guide-open-flow");
  const guideExportMdButton = el("guide-export-md");
  const guideCopyMdButton = el("guide-copy-md");
  const guideSaveMdButton = el("guide-save-md");
  const guideAutoRebuildToggle = el("guide-auto-rebuild");
  const guideSteps = el("guide-steps");

  syncRevealSpeedControl();
  syncFitTightnessControl();
  syncThemeControl();
  bindMermaidInteractions();
  updateLoadingControls();
  syncImpactControls();
  syncBlastControls();
  syncEditorControls();
  syncFlowControls();
  syncGuideControls();
  refreshGuideFromFlowState({ resetSelection: true });
  ensureGuideArtifactWatcher();
  setFlowTimelineMessage("Run flow simulation to view interleaving timelines.");
  renderFlowBlastRadius(null, null);
  bindFlowTimelineInteractions();
  syncMermaidZoomLabel();
  syncMermaidSourceModeControl();
  hideSearchInsights({ syncUrl: false });
  applyControlsRowCollapseState();
  syncControlsCollapseToggles();
  applyDetailsPanelState({ persist: false, resizeGraph: false });

  if (searchInput) {
    searchInput.addEventListener("input", (e) => {
      const nextQuery = e.target.value;
      // Any input starting with / is a slash command — suppress normal search entirely
      if (nextQuery.trim().startsWith("/")) {
        if (state.searchDebounceHandle !== null) {
          window.clearTimeout(state.searchDebounceHandle);
          state.searchDebounceHandle = null;
        }
        return;
      }
      if (state.searchDebounceHandle !== null) {
        window.clearTimeout(state.searchDebounceHandle);
        state.searchDebounceHandle = null;
      }
      state.searchDebounceHandle = window.setTimeout(() => {
        state.searchDebounceHandle = null;
        applySearch(nextQuery);
      }, SEARCH_DEBOUNCE_MS);
    });
    searchInput.addEventListener("keydown", (e) => {
      if (e.key === "Enter") {
        var val = searchInput.value.trim();
        if (COMMAND_RE.test(val)) {
          e.preventDefault();
          applySearch(val);
        }
      }
    });
  }
  if (impactBaseRefSelect) {
    impactBaseRefSelect.addEventListener("change", (e) => {
      state.impactBaseRef = String(e.target.value || "").trim();
      writeUrlViewState();
      syncImpactControls();
      syncFlowControls();
    });
  }
  if (impactCompareModeSelect) {
    impactCompareModeSelect.addEventListener("change", (e) => {
      state.impactCompareMode = normalizeImpactCompareMode(e.target.value);
      writeUrlViewState();
      syncImpactControls();
      syncFlowControls();
    });
  }
  if (impactHopCountSelect) {
    impactHopCountSelect.addEventListener("change", (e) => {
      state.impactHopCount = clampImpactHops(e.target.value);
      writeUrlViewState();
      syncImpactControls();
    });
  }
  if (runImpactButton) {
    runImpactButton.addEventListener("click", () => {
      void runImpactAnalysis();
    });
  }
  if (clearImpactButton) {
    clearImpactButton.addEventListener("click", () => {
      clearImpactAnalysis();
    });
  }
  if (blastHopCountSelect) {
    blastHopCountSelect.addEventListener("change", (e) => {
      state.blastHopCount = clampBlastHops(e.target.value);
      syncBlastControls();
      if (state.blastLensActive && state.graph) {
        renderGraph(state.renderMode, { preserveViewport: false, focusSearch: false });
      }
    });
  }
  if (blastNodeCapSelect) {
    blastNodeCapSelect.addEventListener("change", (e) => {
      state.blastNodeCap = clampBlastNodeCap(e.target.value);
      syncBlastControls();
      if (state.blastLensActive && state.graph) {
        renderGraph(state.renderMode, { preserveViewport: false, focusSearch: false });
      }
    });
  }
  if (toggleBlastLensButton) {
    toggleBlastLensButton.addEventListener("click", () => {
      const enable = !state.blastLensActive;
      setBlastLensActive(enable, { centerNodeId: state.selectedNodeId, rerender: true });
    });
  }
  if (focusBlastSelectionButton) {
    focusBlastSelectionButton.addEventListener("click", () => {
      if (!state.selectedNodeId) {
        setBlastStatus("Select a node first.", "warn");
        return;
      }
      setBlastLensActive(true, { centerNodeId: state.selectedNodeId, rerender: true });
    });
  }
  if (clearBlastLensButton) {
    clearBlastLensButton.addEventListener("click", () => {
      setBlastLensActive(false, { rerender: true });
    });
  }
  if (blastIgnoreStructureHopsToggle) {
    blastIgnoreStructureHopsToggle.addEventListener("change", () => {
      state.blastIgnoreStructureHops = blastIgnoreStructureHopsToggle.checked;
      writeLocalStorage(
        BLAST_IGNORE_STRUCTURE_HOPS_STORAGE_KEY,
        state.blastIgnoreStructureHops ? "1" : "0"
      );
      syncBlastControls();
      if (state.blastLensActive && state.graph) {
        renderGraph(state.renderMode, { preserveViewport: false, focusSearch: false });
      }
    });
  }
  if (blastShowCrateToggle) {
    blastShowCrateToggle.addEventListener("change", () => {
      state.blastShowCrate = blastShowCrateToggle.checked;
      writeLocalStorage(BLAST_SHOW_CRATE_STORAGE_KEY, state.blastShowCrate ? "1" : "0");
      syncBlastControls();
      if (state.blastLensActive && state.graph) {
        renderGraph(state.renderMode, { preserveViewport: false, focusSearch: false });
      }
    });
  }
  if (blastShowModuleToggle) {
    blastShowModuleToggle.addEventListener("change", () => {
      state.blastShowModule = blastShowModuleToggle.checked;
      writeLocalStorage(BLAST_SHOW_MODULE_STORAGE_KEY, state.blastShowModule ? "1" : "0");
      syncBlastControls();
      if (state.blastLensActive && state.graph) {
        renderGraph(state.renderMode, { preserveViewport: false, focusSearch: false });
      }
    });
  }
  if (blastShowFileToggle) {
    blastShowFileToggle.addEventListener("change", () => {
      state.blastShowFile = blastShowFileToggle.checked;
      writeLocalStorage(BLAST_SHOW_FILE_STORAGE_KEY, state.blastShowFile ? "1" : "0");
      syncBlastControls();
      if (state.blastLensActive && state.graph) {
        renderGraph(state.renderMode, { preserveViewport: false, focusSearch: false });
      }
    });
  }
  if (kindFilter) {
    kindFilter.addEventListener("change", (e) => applyKindFilter(e.target.value));
  }
  if (resetButton) {
    resetButton.addEventListener("click", resetView);
  }
  if (resetNodesButton) {
    resetNodesButton.addEventListener("click", resetNodesToDefault);
  }
  if (toggleButton) {
    toggleButton.addEventListener("click", () => {
      if (!state.graph) return;
      if (state.impactActive) {
        setExpansionStatusText("Disable impact mode to use full-graph loading.", "warn");
        return;
      }
      if (state.blastLensActive) {
        setExpansionStatusText("Disable blast lens to use full-graph loading.", "warn");
        return;
      }
      if (state.fullLoadSession || state.stagedRevealSession) {
        return;
      }

      if (state.renderMode === "full") {
        clearNodeTooltipTimers();
        hideNodeTooltip();
        renderGraph("progressive", { preserveViewport: false, focusSearch: true });
        return;
      }

      if (state.graph.nodes.length <= FULL_GRAPH_ENABLE_NODE_LIMIT) {
        clearNodeTooltipTimers();
        hideNodeTooltip();
        renderGraph("full", { preserveViewport: false, focusSearch: true });
        return;
      }

      startProgressiveFullLoad();
    });
  }
  if (themeToggle) {
    themeToggle.addEventListener("click", () => {
      const nextTheme = state.theme === "dark" ? "light" : "dark";
      applyTheme(nextTheme, { persist: true, rerender: true, preserveViewport: true });
    });
  }
  if (toggleOptionsRowsButton) {
    toggleOptionsRowsButton.addEventListener("click", () => {
      setOptionsRowsCollapsed(!state.optionsRowsCollapsed);
    });
  }
  if (toggleInfoRowsButton) {
    toggleInfoRowsButton.addEventListener("click", () => {
      setInfoRowsCollapsed(!state.infoRowsCollapsed);
    });
  }
  if (stopLoadingButton) {
    stopLoadingButton.addEventListener("click", () => {
      stopActiveLoading();
    });
  }
  if (toggleMermaidSplitButton) {
    toggleMermaidSplitButton.addEventListener("click", () => {
      setMermaidSplitOpen(!state.mermaidSplitOpen, { persistSearchPreference: true });
    });
  }
  if (toggleRightPanelButton) {
    toggleRightPanelButton.addEventListener("click", () => {
      setDetailsPanelOpen(!state.detailsPanelOpen, { persist: true, resizeGraph: true });
    });
  }
  if (toggleRightPanelInlineButton) {
    toggleRightPanelInlineButton.addEventListener("click", () => {
      setDetailsPanelOpen(!state.detailsPanelOpen, { persist: true, resizeGraph: true });
    });
  }
  if (detailsResizeHandle) {
    detailsResizeHandle.addEventListener("mousedown", startDetailsPanelResize);
  }
  if (detailsTabButtons.length > 0) {
    detailsTabButtons.forEach((button) => {
      button.addEventListener("click", () => {
        setDetailsPanelTab(button.dataset.detailsTab, {
          ensureOpen: true,
          persist: true,
          resizeGraph: false,
        });
      });
    });
  }
  if (closeMermaidSplitButton) {
    closeMermaidSplitButton.addEventListener("click", () => {
      setMermaidSplitOpen(false, { persistSearchPreference: true });
    });
  }
  if (openMermaidModalButton) {
    openMermaidModalButton.addEventListener("click", () => {
      setMermaidModalOpen(!state.mermaidModalOpen);
    });
  }
  if (openMermaidModalInlineButton) {
    openMermaidModalInlineButton.addEventListener("click", () => {
      setMermaidModalOpen(!state.mermaidModalOpen);
    });
  }
  if (closeMermaidModalButton) {
    closeMermaidModalButton.addEventListener("click", () => {
      setMermaidModalOpen(false);
    });
  }
  if (mermaidModalOverlay) {
    mermaidModalOverlay.addEventListener("click", (event) => {
      if (event.target === mermaidModalOverlay) {
        setMermaidModalOpen(false);
      }
    });
  }
  if (mermaidZoomInButton) {
    mermaidZoomInButton.addEventListener("click", () => {
      state.mermaidZoom = clampMermaidZoom(state.mermaidZoom + MERMAID_ZOOM_STEP);
      applyMermaidZoom();
    });
  }
  if (mermaidZoomOutButton) {
    mermaidZoomOutButton.addEventListener("click", () => {
      state.mermaidZoom = clampMermaidZoom(state.mermaidZoom - MERMAID_ZOOM_STEP);
      applyMermaidZoom();
    });
  }
  if (mermaidZoomResetButton) {
    mermaidZoomResetButton.addEventListener("click", () => {
      state.mermaidZoom = 1;
      applyMermaidZoom();
    });
  }
  if (mermaidModalZoomInButton) {
    mermaidModalZoomInButton.addEventListener("click", () => {
      state.mermaidZoom = clampMermaidZoom(state.mermaidZoom + MERMAID_ZOOM_STEP);
      applyMermaidZoom();
    });
  }
  if (mermaidModalZoomOutButton) {
    mermaidModalZoomOutButton.addEventListener("click", () => {
      state.mermaidZoom = clampMermaidZoom(state.mermaidZoom - MERMAID_ZOOM_STEP);
      applyMermaidZoom();
    });
  }
  if (mermaidModalZoomResetButton) {
    mermaidModalZoomResetButton.addEventListener("click", () => {
      state.mermaidZoom = 1;
      applyMermaidZoom();
    });
  }
  if (copyMermaidButton) {
    copyMermaidButton.addEventListener("click", async () => {
      if (!state.latestMermaidSource) return;
      try {
        await navigator.clipboard.writeText(state.latestMermaidSource);
        copyMermaidButton.textContent = "Copied";
        window.setTimeout(() => {
          if (copyMermaidButton) {
            copyMermaidButton.textContent = "Copy Mermaid";
          }
        }, 900);
      } catch (_err) {
        copyMermaidButton.textContent = "Copy failed";
        window.setTimeout(() => {
          if (copyMermaidButton) {
            copyMermaidButton.textContent = "Copy Mermaid";
          }
        }, 1200);
      }
    });
  }
  if (mermaidSourceModeSelect) {
    mermaidSourceModeSelect.addEventListener("change", (event) => {
      const nextMode = normalizeMermaidSourceMode(event.target.value);
      state.mermaidSourceMode = nextMode;
      writeLocalStorage(MERMAID_SOURCE_MODE_STORAGE_KEY, nextMode);
      syncMermaidSourceModeControl();
      scheduleInsightsRefresh({
        immediate: true,
        preferIdle: false,
        applyGraphFocus: true,
      });
    });
  }
  if (revealSpeed) {
    const updateRevealSpeed = (rawValue) => {
      const next = clampRevealSpeedMs(rawValue);
      state.revealIntervalMs = next;
      writeLocalStorage(REVEAL_SPEED_STORAGE_KEY, String(next));
      syncRevealSpeedControl();

      if (state.fullLoadSession && next < FULL_LOAD_MIN_SPEED_MS) {
        stopProgressiveFullLoad(
          `Full load stopped: set Reveal to at least ${FULL_LOAD_MIN_SPEED_MS}ms.`,
          "warn"
        );
      }
      if (state.stagedRevealSession) {
        if (next === 0) {
          revealRemainingNow(state.stagedRevealSession);
        } else {
          scheduleStagedRevealTick(stagedRevealDelayMs());
        }
      }
      if (state.impactRevealSession) {
        if (next === 0) {
          runImpactRevealTick();
        } else {
          scheduleImpactRevealTick(stagedRevealDelayMs());
        }
      }
      if (state.flowPlaybackSession) {
        state.flowPlaybackSession.delayMs = flowPlaybackDelayMs();
        scheduleFlowPlaybackTick(0);
      }
      setExpansionStatusText(`Reveal speed set to ${formatRevealSpeed(next)} per node.`, "muted");
      if (state.graph) {
        updateRenderModeToggle(state.graph);
      }
    };

    revealSpeed.addEventListener("input", (e) => {
      updateRevealSpeed(e.target.value);
    });
  }
  if (fitTightness) {
    const updateFitTightness = (rawValue) => {
      const next = clampFitTightness(rawValue);
      state.fitTightness = next;
      writeLocalStorage(FIT_TIGHTNESS_STORAGE_KEY, String(next));
      syncFitTightnessControl();
      if (state.cy) {
        fitGraphToCurrentContext();
      }
    };

    fitTightness.addEventListener("input", (e) => {
      updateFitTightness(e.target.value);
    });
  }
  if (editorPathInput) {
    editorPathInput.addEventListener("keydown", (event) => {
      if (event.key !== "Enter") return;
      event.preventDefault();
      void loadEditorFile(editorPathInput.value);
    });
  }
  if (editorLoadButton) {
    editorLoadButton.addEventListener("click", () => {
      const pathValue = editorPathInput ? editorPathInput.value : state.editorPath;
      void loadEditorFile(pathValue);
    });
  }
  if (editorSaveButton) {
    editorSaveButton.addEventListener("click", () => {
      void saveEditorFile();
    });
  }
  if (editorRefreshButton) {
    editorRefreshButton.addEventListener("click", () => {
      void loadEditorFile(state.editorPath);
    });
  }
  if (editorAutoRefreshToggle) {
    editorAutoRefreshToggle.checked = state.editorAutoRefresh;
    editorAutoRefreshToggle.addEventListener("change", () => {
      state.editorAutoRefresh = editorAutoRefreshToggle.checked;
      if (state.editorAutoRefresh && !state.editorDirty && state.editorExternalChangeDetected && state.editorPath) {
        void loadEditorFile(state.editorPath, { quiet: true });
      }
      syncEditorControls();
    });
  }
  if (flowSourceSelect) {
    flowSourceSelect.addEventListener("change", (event) => {
      setFlowSource(event.target.value, { setPayload: false });
      setFlowStatus("Flow source updated.", "muted");
    });
  }
  if (flowRefreshSourcesButton) {
    flowRefreshSourcesButton.addEventListener("click", () => {
      setFlowStatus("Refreshing flow sources...", "muted");
      void loadFlowSources({ preserveSelection: true });
    });
  }
  if (flowLoadMockButton) {
    flowLoadMockButton.addEventListener("click", () => {
      void loadFlowMockPayload();
    });
  }
  if (flowRunButton) {
    flowRunButton.addEventListener("click", () => {
      void runFlowSimulation();
    });
  }
  if (flowRunDeltaButton) {
    flowRunDeltaButton.addEventListener("click", () => {
      void runFlowDelta();
    });
  }
  if (flowExportJsonButton) {
    flowExportJsonButton.addEventListener("click", () => {
      exportFlowJson();
    });
  }
  if (flowExportMdButton) {
    flowExportMdButton.addEventListener("click", () => {
      exportFlowMarkdown();
    });
  }
  if (flowMaxHopsSelect) {
    flowMaxHopsSelect.addEventListener("change", (event) => {
      state.flowMaxHops = clampFlowHops(event.target.value);
      syncFlowControls();
      writeUrlViewState();
    });
  }
  if (flowOnlyOutboundToggle) {
    flowOnlyOutboundToggle.addEventListener("change", () => {
      state.flowOnlyOutbound = flowOnlyOutboundToggle.checked;
      syncFlowControls();
      refreshFlowViewFromCurrentFilters();
    });
  }
  if (flowOnlyCrossCrateToggle) {
    flowOnlyCrossCrateToggle.addEventListener("change", () => {
      state.flowOnlyCrossCrate = flowOnlyCrossCrateToggle.checked;
      syncFlowControls();
      refreshFlowViewFromCurrentFilters();
    });
  }
  if (flowIncludeContainersToggle) {
    flowIncludeContainersToggle.addEventListener("change", () => {
      state.flowIncludeContainers = flowIncludeContainersToggle.checked;
      syncFlowControls();
      refreshFlowViewFromCurrentFilters();
    });
  }
  if (guideSourceSelect) {
    guideSourceSelect.addEventListener("change", (event) => {
      const nextSourceId = String(event.target.value || "").trim();
      if (!nextSourceId) return;
      state.guideSelectedSourceId = nextSourceId;
      setFlowSource(nextSourceId, { setPayload: false, ensureOption: true });
      setGuideStatus("Guide source updated.", "muted");
      syncGuideControls();
    });
  }
  if (guideRunSourceButton) {
    guideRunSourceButton.addEventListener("click", () => {
      void runGuideForSelectedSource({ openGuideTab: true });
    });
  }
  if (guideTraceDirectionSelect) {
    guideTraceDirectionSelect.addEventListener("change", (event) => {
      state.guideTraceDirection = normalizeGuideTraceDirection(event.target.value);
      state.guideSelectedTraceKey = "";
      writeLocalStorage(GUIDE_TRACE_DIRECTION_STORAGE_KEY, state.guideTraceDirection);
      refreshGuideFromFlowState({ resetSelection: true });
    });
  }
  if (guideTraceSelect) {
    guideTraceSelect.addEventListener("change", (event) => {
      state.guideSelectedTraceKey = String(event.target.value || "").trim();
      refreshGuideFromFlowState();
    });
  }
  if (guideBuildButton) {
    guideBuildButton.addEventListener("click", () => {
      void runGuideBuild({ openGuideTab: true });
    });
  }
  if (guidePlayTraceButton) {
    guidePlayTraceButton.addEventListener("click", () => {
      playGuideTrace();
    });
  }
  if (guideOpenFlowButton) {
    guideOpenFlowButton.addEventListener("click", () => {
      setDetailsPanelTab("flow", {
        ensureOpen: true,
        persist: true,
        resizeGraph: false,
      });
    });
  }
  if (guideExportMdButton) {
    guideExportMdButton.addEventListener("click", () => {
      exportGuideRunbookMarkdown();
    });
  }
  if (guideCopyMdButton) {
    guideCopyMdButton.addEventListener("click", () => {
      void copyGuideRunbookMarkdown();
    });
  }
  if (guideSaveMdButton) {
    guideSaveMdButton.addEventListener("click", () => {
      void saveGuideRunbookInRepo();
    });
  }
  if (guideAutoRebuildToggle) {
    guideAutoRebuildToggle.checked = shouldGuideAutoRebuild();
    guideAutoRebuildToggle.addEventListener("change", () => {
      state.guideAutoRebuild = guideAutoRebuildToggle.checked;
      writeLocalStorage(
        GUIDE_AUTO_REBUILD_STORAGE_KEY,
        state.guideAutoRebuild ? "1" : "0"
      );
      if (state.guideAutoRebuild && state.guideArtifactsDirty) {
        void pollGuideArtifacts();
      }
      syncGuideControls();
    });
  }
  if (guideSteps) {
    guideSteps.addEventListener("click", (event) => {
      const target = event.target instanceof Element
        ? event.target.closest("[data-guide-node-id]")
        : null;
      if (!target) return;
      const nodeId = String(target.getAttribute("data-guide-node-id") || "").trim();
      const traceIndex = Number(target.getAttribute("data-guide-trace-index"));
      const stepIndex = Number(target.getAttribute("data-guide-step-index"));
      if (!nodeId) return;
      const focused = focusGuideNodeInGraph(
        nodeId,
        Number.isFinite(traceIndex) ? traceIndex : null,
        Number.isFinite(stepIndex) ? stepIndex : 0
      );
      if (focused) {
        setGuideStatus(`Focused ${nodeId} from guide trace.`, "ok");
      }
    });
  }

  window.addEventListener("keydown", (event) => {
    if (event.key === "Escape" && state.mermaidModalOpen) {
      setMermaidModalOpen(false);
    }
  });
  window.addEventListener("resize", () => {
    if (state.detailsResizeSession) return;
    const clampedWidth = clampDetailsPanelWidth(state.detailsPanelWidth);
    if (clampedWidth !== state.detailsPanelWidth) {
      state.detailsPanelWidth = clampedWidth;
    }
    applyDetailsPanelState({ persist: true, resizeGraph: true });
  });
}

async function bootstrap() {
  bindSettingsPanel();
  setSettingsPanelOpen(false);
  initializeThemeSetting();
  initializeFitTightnessSetting();
  initializeSearchSplitAutoOpenSetting();
  initializeMermaidSourceModeSetting();
  initializeControlsCollapseSettings();
  initializeDetailsPanelSettings();
  initializeBlastSettings();
  initializeGuideSettings();
  bindGraphControls();
  const urlViewState = readUrlViewState();
  state.impactBaseRef = String(urlViewState.impactBaseRef || "").trim();
  state.impactCompareMode = normalizeImpactCompareMode(urlViewState.impactCompareMode);
  state.impactHopCount = clampImpactHops(urlViewState.impactHopCount);
  state.flowSourceId = String(urlViewState.flowSourceId || "").trim();
  state.flowMaxHops = clampFlowHops(urlViewState.flowMaxHops);
  state.flowOnlyOutbound = urlViewState.flowOnlyOutbound === true;
  state.flowOnlyCrossCrate = urlViewState.flowOnlyCrossCrate === true;
  state.flowIncludeContainers = urlViewState.flowIncludeContainers !== false;
  syncImpactControls();
  syncFlowControls();

  try {
    // Prefer the UI variant (no test_calls edges — never read by this
    // page, ~half the bytes); fall back to the full graph when only
    // that artifact exists.
    let res = await fetch("graph_ui.json", { cache: "no-store" });
    if (!res.ok) {
      res = await fetch("graph.json", { cache: "no-store" });
    }
    if (!res.ok) {
      throw new Error(`Failed to load graph data: ${res.status}`);
    }
    const graph = await res.json();
    state.graph = graph;
    state.graphIndex = buildGraphIndex(graph);
    computeHierarchicalLayout();
    state.kindFilter = urlViewState.kindFilter;
    state.mermaidSplitOpen = urlViewState.mermaidSplitOpen === true;
    state.mermaidModalOpen = urlViewState.mermaidModalOpen === true;
    restoreClickExpansionStateFromUrl(urlViewState);
    const searchInput = el("search-input");
    const initialQuery = String(urlViewState.searchQuery || "");
    const hasExplicitSplitPreference = urlViewState.mermaidSplitOpen !== null;
    if (searchInput) {
      searchInput.value = initialQuery;
    }
    await loadImpactRefs(state.impactBaseRef);
    const hasRequestedImpactView = urlViewState.impactActive === true;
    initializeRevealSpeedSetting();
    syncRevealSpeedControl();
    if (hasRequestedImpactView) {
      state.searchQuery = "";
      if (searchInput) {
        searchInput.value = "";
      }
      renderGraph(chooseInitialRenderMode(graph), { preserveViewport: false, focusSearch: true });
      await runImpactAnalysis({ syncUrl: false });
    } else if (initialQuery.trim().length > 0) {
      applySearch(initialQuery, {
        syncUrl: false,
        allowAutoSplit: !hasExplicitSplitPreference,
      });
    } else {
      state.searchQuery = "";
      renderGraph(chooseInitialRenderMode(graph), { preserveViewport: false, focusSearch: true });
    }
    setFlowStatus("Loading flow sources...", "muted");
    void loadFlowSources({ preserveSelection: true });
    writeUrlViewState();
    syncBlastControls();
    syncEditorControls();
    void ensureMonacoEditor();
    await initMakeRunner();
  } catch (err) {
    el("graph-meta").textContent = `Error: ${err.message}`;
    el("details-body").textContent =
      "Could not load graph.json. Run `make graph-index`, then serve this folder over HTTP (e.g. `make graph-serve`).";
    await loadImpactRefs(state.impactBaseRef);
    syncBlastControls();
    syncEditorControls();
    void ensureMonacoEditor();
    await initMakeRunner();
  }
}

bootstrap();
