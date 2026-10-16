//
//  CanonicalThinkingMapBackend.swift
//  Magios
//
//  The canonical backing for `ThinkingMapModel` — the sole source of truth for
//  the thinking map. Split into:
//    1. PURE  — `CanonicalThinkingMapProjection` (canonical map → view types) +
//       `CanonicalThinkingMapOps` (a mutation → canonical `[LTM.Operation]`
//       builders). Deterministic, no IO. Unit-testable directly.
//    2. Runtime — `final class CanonicalThinkingMapBackend` that owns an
//       `LTM.SyncStore`, applies the built ops, and re-projects on every map
//       change via an injected `onMap`. Also owns the library
//       (maps/open/create/rename/archive), the AI `/interpret` bridge, and the
//       one-time importer that migrates the OLD `UserDefaults` maps.
//

import Foundation

// MARK: - 0. Clarification / restructure view-models (pure, UNGATED)

/// A projected clarification — the model asked the owner a question about a node.
/// Pure/`Codable`/`Equatable`; the (future) UI renders these + calls
/// `ThinkingMapModel.answerClarification`. `nodeID` is the SAME UUID the node
/// projection assigns (via the shared id resolution), so it lines up with a node.
struct ThinkingClarification: Identifiable, Equatable, Codable {
    let id: String
    let nodeID: UUID?
    let question: String
    let answered: Bool
    let answer: String?
}

/// A projected restructure proposal — the model staged a reorganization awaiting
/// the owner's confirm/reject. This is a SUMMARY (rationale + affected nodes +
/// op count); the full before/after preview is a later refinement.
/// `affectedNodeIDs` are the SAME UUIDs the node projection assigns.
struct ThinkingProposal: Identifiable, Equatable, Codable {
    let id: String
    let rationale: String
    let affectedNodeIDs: [UUID]
    let operationCount: Int
}

// MARK: - 1. Pure projection: canonical LTM.Map → E0 view types (UNGATED)

/// Projects a canonical `LTM.Map` down onto E0's local view types. Pure and
/// deterministic: given the same map it always yields the same nodes/edges/
/// active id, and it performs no IO. This is the READ half of the canonical
/// backing — the model publishes exactly this whenever the store reports a new
/// authoritative map.
enum CanonicalThinkingMapProjection {
    /// Project the canonical map into E0's `(nodes, edges, activeNodeID)`.
    ///
    /// - Non-tombstoned `LTM.Node`s become `ThinkingNode`s.
    /// - The tree is modeled canonically as `parent_id`, so one synthetic
    ///   `.branch` edge is emitted per node that has a resolvable parent.
    /// - Non-tombstoned `related_to` edges become `.related` `ThinkingEdge`s.
    /// - `viewState.activeNode` becomes `activeNodeID`.
    static func project(_ map: LTM.Map) -> (
        nodes: [ThinkingNode],
        edges: [ThinkingEdge],
        activeNodeID: UUID?,
        canonicalNodeIDs: [UUID: String],
        canonicalEdgeIDs: [UUID: String]
    ) {
        // Stable id resolution: a canonical node/edge id string → a UUID the E0
        // view types require. Delegates to the shared `resolveNodeUUID` so the
        // clarification/proposal projections line up on the SAME UUIDs as nodes.
        func resolve(_ id: String) -> UUID { Self.resolveNodeUUID(id) }

        let liveNodes = map.nodes.values.filter { !$0.tombstoned }

        // Build ThinkingNodes. Sort by created_at then node_id so the output
        // order is deterministic regardless of the dictionary's iteration order.
        let ordered = liveNodes.sorted {
            $0.createdAt == $1.createdAt ? $0.nodeId < $1.nodeId : $0.createdAt < $1.createdAt
        }

        var nodes: [ThinkingNode] = []
        nodes.reserveCapacity(ordered.count)
        var branchEdges: [ThinkingEdge] = []
        var canonicalNodeIDs: [UUID: String] = [:]
        canonicalNodeIDs.reserveCapacity(ordered.count)

        // The set of live node ids (as canonical strings) so we only synthesize a
        // branch edge / keep a related edge when BOTH endpoints still exist.
        let liveNodeIDs = Set(liveNodes.map(\.nodeId))

        for node in ordered {
            let nodeUUID = resolve(node.nodeId)
            // Keep the exact wire identifier. UUID text is case-insensitive as a
            // UUID but canonical map identifiers are string keys; rebuilding it
            // later with `uuidString` can change case and address another key.
            canonicalNodeIDs[nodeUUID] = node.nodeId
            // Only resolve a parent that is itself a live (non-tombstoned) node.
            let parentUUID: UUID? = node.parentId.flatMap { pid in
                liveNodeIDs.contains(pid) ? resolve(pid) : nil
            }
            let suggested = (node.assertionOrigin == .modelInferred) || (node.epistemicState == .provisional)
            nodes.append(
                ThinkingNode(
                    id: nodeUUID,
                    parentID: parentUUID,
                    kind: Self.thinkingKind(from: node.kind),
                    title: node.label,
                    detail: node.detailMarkdown ?? "",
                    source: node.assertionOrigin.rawValue,
                    suggested: suggested,
                    revision: 1,
                    createdAt: Self.parseTimestamp(node.createdAt),
                    // Destination kinds this node was already promoted to
                    // ("task"/"memory") — drives the detail sheet's promoted
                    // badges instead of re-offering the buttons.
                    promotedKinds: node.promotedRefs.map { $0.destinationKind.rawValue }
                )
            )
            // Synthetic branch edge for the parent/child link (canonical models
            // the tree as parent_id, not edges).
            if let parentUUID {
                branchEdges.append(
                    ThinkingEdge(id: Self.branchEdgeID(child: node.nodeId), from: parentUUID, to: nodeUUID, kind: .branch)
                )
            }
        }

        // Related edges: each non-tombstoned canonical `related_to` edge with
        // both endpoints still live becomes a `.related` E0 edge.
        var relatedEdges: [ThinkingEdge] = []
        var canonicalEdgeIDs: [UUID: String] = [:]
        for edge in map.edges.values.sorted(by: { $0.edgeId < $1.edgeId })
        where !edge.tombstoned
            && edge.kind == .relatedTo
            && liveNodeIDs.contains(edge.fromNode)
            && liveNodeIDs.contains(edge.toNode) {
            let edgeUUID = resolve(edge.edgeId)
            canonicalEdgeIDs[edgeUUID] = edge.edgeId
            relatedEdges.append(
                ThinkingEdge(
                    id: edgeUUID,
                    from: resolve(edge.fromNode),
                    to: resolve(edge.toNode),
                    kind: .related
                )
            )
        }

        let activeNodeID: UUID? = map.viewState.activeNode.flatMap { active in
            liveNodeIDs.contains(active) ? resolve(active) : nil
        }

        return (
            nodes,
            branchEdges + relatedEdges,
            activeNodeID,
            canonicalNodeIDs,
            canonicalEdgeIDs)
    }

    /// Canonical node/edge id string → the UUID the view types require. Real
    /// canonical ids are UUID strings; anything else (e.g. a test/seed id like
    /// "node-1") gets a DETERMINISTIC fallback so the same string always maps to
    /// the same UUID. This is the ONE resolver `project`, `projectClarifications`,
    /// and `projectProposals` all share — so a clarification/proposal's node ids
    /// line up exactly with the projected nodes.
    static func resolveNodeUUID(_ id: String) -> UUID {
        UUID(uuidString: id) ?? deterministicUUID(from: id)
    }

    /// Project the map's OPEN (unanswered) clarifications into view-models. A
    /// clarification is surfaced when its `state == .open` (a resolved/dismissed/
    /// deferred one is dropped). Ordering is deterministic: by `createdAt` then id.
    /// `nodeID` resolves through `resolveNodeUUID` so it matches a projected node.
    static func projectClarifications(_ map: LTM.Map) -> [ThinkingClarification] {
        map.clarifications.values
            .filter { $0.state == .open }
            .sorted {
                $0.createdAt == $1.createdAt
                    ? $0.clarificationId < $1.clarificationId
                    : $0.createdAt < $1.createdAt
            }
            .map { c in
                ThinkingClarification(
                    id: c.clarificationId,
                    nodeID: resolveNodeUUID(c.nodeId),
                    question: c.question,
                    answered: c.answer != nil,
                    answer: c.answer
                )
            }
    }

    /// Project the map's PENDING (`state == .proposed`) restructure proposals into
    /// summary view-models (a confirmed/rejected/deferred one is dropped).
    /// `operationCount` is the staged op count; `affectedNodeIDs` resolve through
    /// `resolveNodeUUID` so they match projected nodes. Deterministic ordering:
    /// by `createdAt` then id.
    static func projectProposals(_ map: LTM.Map) -> [ThinkingProposal] {
        map.proposals.values
            .filter { $0.state == .proposed }
            .sorted {
                $0.createdAt == $1.createdAt
                    ? $0.proposalId < $1.proposalId
                    : $0.createdAt < $1.createdAt
            }
            .map { p in
                ThinkingProposal(
                    id: p.proposalId,
                    rationale: p.rationale,
                    affectedNodeIDs: p.affectedNodeIds.map(resolveNodeUUID),
                    operationCount: p.operations.count
                )
            }
    }

    /// Canonical `LTM.NodeKind` → E0 `ThinkingNodeKind`. E0's first five kinds
    /// carry TITLE-cased raw values (`"Idea"`, `"Decision"`, …) while the rest use
    /// the snake_case canonical names, so a direct `rawValue` match misses the
    /// first five. Match case-insensitively on the canonical name (which is always
    /// lowercase) to line both halves up 1:1; unknown kinds fall back to `.idea`.
    static func thinkingKind(from kind: LTM.NodeKind) -> ThinkingNodeKind {
        let canonical = kind.rawValue // always lowercase snake_case, single words
        return ThinkingNodeKind.allCases.first { $0.rawValue.lowercased() == canonical } ?? .idea
    }

    // MARK: Deterministic helpers

    /// A stable, deterministic UUID derived from a non-UUID canonical id string.
    /// Same input → same UUID (a folded FNV-1a hash expanded to 16 bytes). This
    /// keeps the projection pure and lets synthetic branch edges reference the
    /// exact same node UUIDs.
    static func deterministicUUID(from string: String) -> UUID {
        var bytes = [UInt8](repeating: 0, count: 16)
        // Two independent FNV-1a streams (forward + reverse-salted) fill 16 bytes.
        var h1: UInt64 = 0xcbf29ce484222325
        var h2: UInt64 = 0x100000001b3
        for byte in string.utf8 {
            h1 = (h1 ^ UInt64(byte)) &* 0x100000001b3
            h2 = (h2 &+ UInt64(byte)) &* 0xcbf29ce484222325
        }
        for i in 0..<8 { bytes[i] = UInt8((h1 >> (UInt64(i) * 8)) & 0xff) }
        for i in 0..<8 { bytes[8 + i] = UInt8((h2 >> (UInt64(i) * 8)) & 0xff) }
        // Stamp RFC-4122 version 4 / variant bits so it is a well-formed UUID.
        bytes[6] = (bytes[6] & 0x0f) | 0x40
        bytes[8] = (bytes[8] & 0x3f) | 0x80
        return UUID(uuid: (
            bytes[0], bytes[1], bytes[2], bytes[3],
            bytes[4], bytes[5], bytes[6], bytes[7],
            bytes[8], bytes[9], bytes[10], bytes[11],
            bytes[12], bytes[13], bytes[14], bytes[15]
        ))
    }

    /// The synthetic id for a parent→child branch edge, derived from the child's
    /// canonical id so it is stable across projections of the same map.
    private static func branchEdgeID(child: String) -> UUID {
        deterministicUUID(from: "branch:" + child)
    }

    /// Best-effort parse of a canonical RFC3339 timestamp; falls back to a stable
    /// epoch so ordering stays deterministic when the string is unparseable.
    private static func parseTimestamp(_ string: String) -> Date {
        Self.timestampParser.date(from: string) ?? Date(timeIntervalSince1970: 0)
    }

    private static let timestampParser: ISO8601DateFormatter = {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter
    }()
}

// MARK: - 2. Pure operation builders: E0 mutation → [LTM.Operation] (UNGATED)

/// Pure builders that translate an E0 mutation into canonical `[LTM.Operation]`
/// batches, minting canonical node/edge ids and stamping owner-authored origin/
/// state + RFC3339 timestamps. These never touch the network or the store — the
/// gated backend calls them and hands the result to `LTM.SyncStore.apply`.
enum CanonicalThinkingMapOps {
    /// Add a captured thought under the active node (owner-spoken, asserted). The
    /// new node is minted here so the caller can select it optimistically; a
    /// `.moveToParent` is appended only when there is an active node.
    ///
    /// `detail` optionally overrides the node's detail markdown (default: the
    /// text itself). The Share ingestion path uses it to carry the FULL shared
    /// text prefixed with a provenance line — the origin stays `owner_spoken`
    /// because the owner `/operations` surface only accepts owner origins
    /// (`imported_source` is reserved for the backend's `Imported` actor).
    ///
    /// Returns the ops plus the new node's canonical id.
    static func addThought(
        text: String,
        kind: ThinkingNodeKind,
        activeID: String?,
        detail: String? = nil,
        now: Date = Date()
    ) -> (ops: [LTM.Operation], newNodeID: String) {
        let newID = UUID().uuidString
        let stamp = timestamp(now)
        let node = LTM.Node(
            nodeId: newID,
            kind: canonicalKind(kind),
            label: text,
            detailMarkdown: detail ?? (text.isEmpty ? nil : text),
            epistemicState: .asserted,
            assertionOrigin: .ownerSpoken,
            confidence: 1.0,
            parentId: activeID,
            createdAt: stamp,
            updatedAt: stamp
        )
        var ops: [LTM.Operation] = [.addNode(node: node)]
        if let activeID {
            ops.append(.moveToParent(nodeId: newID, parentId: activeID))
        }
        return (ops, newID)
    }

    /// Edit a node's title/detail. Editing PROMOTES a model-provisional node to an
    /// owner-asserted one, so when `wasProvisional` is true a `.setEpistemicState`
    /// is appended after the `.updateNode`.
    static func updateNode(
        id: String,
        title: String,
        detail: String,
        wasProvisional: Bool,
        now: Date = Date()
    ) -> [LTM.Operation] {
        var ops: [LTM.Operation] = [
            .updateNode(
                nodeId: id,
                label: title,
                detailMarkdown: detail.isEmpty ? .clear : .set(detail),
                confidence: nil
            )
        ]
        if wasProvisional {
            ops.append(.setEpistemicState(nodeId: id, state: .asserted))
        }
        return ops
    }

    /// Connect two nodes with a canonical `related_to` edge (owner-asserted). The
    /// edge id is minted here.
    static func connect(
        from: String,
        to: String,
        now: Date = Date()
    ) -> [LTM.Operation] {
        let stamp = timestamp(now)
        let edge = LTM.Edge(
            edgeId: UUID().uuidString,
            fromNode: from,
            toNode: to,
            kind: .relatedTo,
            assertionOrigin: .ownerSpoken,
            createdAt: stamp,
            updatedAt: stamp
        )
        return [.connect(edge: edge)]
    }

    /// Remove a related connection by its canonical edge id.
    static func disconnect(edgeID: String) -> [LTM.Operation] {
        [.disconnect(edgeId: edgeID)]
    }

    /// Remove a branch (subtree root) by tombstoning it. The reducer cascades to
    /// incident edges and descendants; the projection filters tombstoned nodes.
    static func removeBranch(id: String) -> [LTM.Operation] {
        [.tombstoneNode(nodeId: id)]
    }

    /// Rename the map.
    static func setTitle(_ title: String) -> [LTM.Operation] {
        [.setTitle(title: title)]
    }

    // MARK: Helpers

    /// E0's kind → canonical kind. E0's `ThinkingNodeKind` raw values are the
    /// title-cased UI labels for the first five and the snake_case canonical
    /// names for the rest, so map on the lowercased raw value which lines up with
    /// `LTM.NodeKind`'s snake_case cases.
    static func canonicalKind(_ kind: ThinkingNodeKind) -> LTM.NodeKind {
        LTM.NodeKind(rawValue: kind.rawValue.lowercased()) ?? .idea
    }

    /// RFC3339 / ISO-8601 UTC timestamp used for minted canonical records.
    static func timestamp(_ date: Date = Date()) -> String {
        Self.formatter.string(from: date)
    }

    private static let formatter: ISO8601DateFormatter = {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime]
        return formatter
    }()
}

// MARK: - 3. Runtime backend

/// Owns the canonical `LTM.SyncStore` for one map and drives it from the model's
/// mutations. Every mutation builds ops via `CanonicalThinkingMapOps`, applies
/// them through the store, and reports the resulting authoritative map back via
/// `onMap` so `ThinkingMapModel` re-projects + publishes. This is the sole
/// backing for the thinking map.
@MainActor
final class CanonicalThinkingMapBackend {
    /// The `SyncStore` for the CURRENTLY-open map. `var` because opening a
    /// different library map (S2d) re-points the backend at a new store built
    /// over the shared `client` + `persistence` for that map id.
    private var syncStore: LTM.SyncStore
    /// Reports the current authoritative map after any change (open/refresh/apply)
    /// so the model can re-project. Called on the main actor.
    private let onMap: (LTM.Map) -> Void

    /// The REST client + persistence retained so the library (S2d) can list/
    /// create/patch/restore maps and re-point `syncStore` on open. nil only when
    /// constructed directly from a `SyncStore` (tests) — the library methods then
    /// no-op / throw, and E0's local path is used instead.
    let client: LTM.APIClient?
    private let persistence: LTM.Persistence?

    /// Construct directly from an already-built `LTM.SyncStore` (tests / an
    /// explicitly-wired store). No `client` is retained, so the library API is
    /// unavailable on this instance.
    init(syncStore: LTM.SyncStore, onMap: @escaping (LTM.Map) -> Void) {
        self.syncStore = syncStore
        self.onMap = onMap
        self.client = nil
        self.persistence = nil
    }

    /// Convenience construction from the app's scope + base URL. When called with
    /// no explicit arguments it wires to `MagicianAccess` (the app's single scope
    /// source of truth) and a file-backed persistence in the caches directory.
    /// The `client` + `persistence` are retained so the library (S2d) can
    /// list/create/patch/restore + re-point the store on open.
    ///
    /// TODO(S4/S5): the app is responsible for choosing/creating the `mapID` and
    /// for open/refresh sequencing; this convenience path is a scaffold.
    convenience init(
        mapID: String,
        baseURL: URL = MagicianAccess.baseURL,
        scope: LTM.Scope = LTM.Scope(
            principal: MagicianAccess.principal,
            workspace: MagicianAccess.workspace
        ),
        onMap: @escaping (LTM.Map) -> Void
    ) {
        // UI-test launches are deliberately offline, but the canonical map is
        // server-backed — so the demo seed and the library would dead-end.
        // Swap in the in-process fake backend (`LTM.InMemoryTransport`) plus an
        // ephemeral persistence so no cache leaks between test cases. The
        // production path below is untouched.
        let isUITest = ProcessInfo.processInfo.arguments.contains("--ui-test")
        let transport: LTM.Transport = isUITest
            ? LTM.InMemoryTransport.shared
            : LTM.URLSessionTransport(
                session: .shared,
                extraHeaders: MagicianAccess.authorizedHeaders(for: baseURL)
            )
        let client = LTM.APIClient(baseURL: baseURL, scope: scope, transport: transport)
        let persistence: LTM.Persistence
        if isUITest {
            persistence = LTM.EphemeralPersistence()
        } else {
            let dir = FileManager.default
                .urls(for: .cachesDirectory, in: .userDomainMask)[0]
                .appendingPathComponent("ltm-syncstore", isDirectory: true)
            persistence = LTM.FilePersistence(directory: dir)
        }
        let store = LTM.SyncStore(mapID: mapID, client: client, persistence: persistence)
        self.init(client: client, persistence: persistence, syncStore: store, onMap: onMap)
    }

    /// Designated init that retains the `client` + `persistence` alongside the
    /// initial store, enabling the library API + open(mapID:) re-pointing.
    private init(
        client: LTM.APIClient,
        persistence: LTM.Persistence,
        syncStore: LTM.SyncStore,
        onMap: @escaping (LTM.Map) -> Void
    ) {
        self.client = client
        self.persistence = persistence
        self.syncStore = syncStore
        self.onMap = onMap
    }

    /// The canonical id of the CURRENTLY-open map (the store re-points on
    /// open/create). Used to filter realtime `ThinkingMapUpdated` notices.
    var currentMapID: String { syncStore.mapID }

    // MARK: Lifecycle

    /// Open the store (load cache, refresh from server if online, flush queued
    /// ops) then report the resulting map.
    func open() async {
        try? await syncStore.open()
        reportMap()
    }

    /// Refresh from the server and report the fresh authoritative map.
    func refresh() async {
        try? await syncStore.refresh()
        reportMap()
    }

    // MARK: Interpret (AI frontier — applies model operations server-side)

    /// S2d — the canonical AI "frontier" path. Sends `text` + `intent` to the
    /// server's `/interpret`, which APPLIES the model's operations (adding
    /// `model_inferred` / `provisional` nodes) and returns the updated map. On
    /// success the store adopts the authoritative map and we report it so the
    /// model re-projects — the new provisional nodes surface as E0's dashed
    /// `suggested` cards. Throws (e.g. `LTM.APIError.transport` when offline) so
    /// the caller can fall back to the local prompt palette.
    func interpret(
        text: String,
        intent: LTM.InterpretIntent,
        focusNodeID: String? = nil,
        utteranceID: String? = nil
    ) async throws {
        try await syncStore.interpret(
            text: text,
            intent: intent,
            focusNodeID: focusNodeID,
            utteranceID: utteranceID)
        reportMap()
    }

    // MARK: Clarifications + restructure (S-clar)

    /// Ask the model to STAGE a restructure proposal (online only). On success the
    /// store adopts the map now carrying a pending `proposals[...]`; we report it
    /// so the model re-projects (`pendingProposals` populates). Throws (e.g.
    /// offline) so the caller can degrade gracefully.
    func consolidate() async throws {
        try await syncStore.consolidate()
        reportMap()
    }

    /// Confirm or reject a pending restructure proposal (online only). On confirm
    /// the proposal's inner ops materialize server-side; on `.applied` the store
    /// adopts the returned map and we report it so the model re-projects.
    func decideProposal(_ proposalID: String, decision: String) async throws {
        try await syncStore.decideProposal(proposalID, decision: decision)
        reportMap()
    }

    /// Answer / resolve a clarification (owner op; works offline via the queue).
    /// Reports the resulting map so the model re-projects (the clarification drops
    /// out of `clarifications` once resolved server-side).
    func respondClarification(_ clarificationID: String, answer: String?) async {
        await syncStore.respondClarification(clarificationID, answer: answer)
        reportMap()
    }

    // MARK: Node promotion (online only)

    /// GOVERNED promotion of a node into a durable object (`target` =
    /// `"task"` | `"memory"`). Online only; rethrows so the caller can surface
    /// 409 `.confirmationRequired` as an inline confirm + retry. On success
    /// the server committed a `link_promoted_object` op, so refresh (best
    /// effort) + report so the projection surfaces the node's new
    /// `promoted_refs` (the detail sheet's promoted badge).
    func promoteNode(
        _ nodeID: String, target: String, confirm: Bool
    ) async throws -> LTM.PromoteNodeResult {
        let result = try await syncStore.promoteNode(nodeID, target: target, confirm: confirm)
        try? await syncStore.refresh()
        reportMap()
        return result
    }

    // MARK: Ambient "Listen" mode (attach/detach a live source session)

    /// Attach a live source session so its finalized user utterances auto-map
    /// onto the CURRENTLY-open map (server-side ambient coordinator). Pass the
    /// voice client's **media/voice session id** — the coordinator matches it
    /// against the `presence_session_id` the voice orchestrator stamps onto
    /// each spoken turn. Online only; rethrows so the caller can degrade.
    @discardableResult
    func attachSession(_ sourceSessionID: String) async throws -> Bool {
        try await syncStore.attachSession(sourceSessionID)
    }

    /// Detach a previously-attached source session (idempotent). Online only.
    @discardableResult
    func detachSession(_ sourceSessionID: String) async throws -> Bool {
        try await syncStore.detachSession(sourceSessionID)
    }

    // MARK: Mutations (build ops → apply → report)

    /// Add a captured thought under `activeID`. Returns the minted canonical id.
    /// `detail` optionally overrides the node's detail markdown (Share seeds).
    @discardableResult
    func addThought(
        _ text: String, kind: ThinkingNodeKind, activeID: String?, detail: String? = nil
    ) async -> String {
        let built = CanonicalThinkingMapOps.addThought(
            text: text, kind: kind, activeID: activeID, detail: detail)
        await apply(built.ops)
        return built.newNodeID
    }

    /// Edit a node's title/detail; promotes a provisional node to asserted.
    func updateNode(id: String, title: String, detail: String, wasProvisional: Bool) async {
        await apply(CanonicalThinkingMapOps.updateNode(
            id: id, title: title, detail: detail, wasProvisional: wasProvisional))
    }

    /// Connect two nodes with a `related_to` edge.
    func connect(from: String, to: String) async {
        await apply(CanonicalThinkingMapOps.connect(from: from, to: to))
    }

    /// Remove a related connection by canonical edge id.
    func disconnect(edgeID: String) async {
        await apply(CanonicalThinkingMapOps.disconnect(edgeID: edgeID))
    }

    /// Tombstone a branch (subtree root).
    func removeBranch(id: String) async {
        await apply(CanonicalThinkingMapOps.removeBranch(id: id))
    }

    /// Rename the map.
    func setTitle(_ title: String) async {
        await apply(CanonicalThinkingMapOps.setTitle(title))
    }

    /// Set the shared active node (optional — the model may keep selection local).
    func select(_ id: String) async {
        await apply([.setSharedView(viewState: LTM.SharedViewState(activeNode: id))])
    }

    // MARK: - Library (S2d — over the canonical client)

    /// One server page per request (server clamp is 200).
    private static let listPageSize = 100
    /// Runaway guard for the paging loop (100 × 100 = 10k maps — far beyond
    /// any real library; a server that keeps inflating `total` can't spin us).
    private static let listMaxPages = 100

    /// List the map summaries from the server — all of them, fetched through
    /// the paginated API in `listPageSize` chunks so one giant library never
    /// rides a single response. The library projection reconciles against the
    /// FULL list, so this loops to completion rather than stopping at page 1.
    /// Degrades gracefully: on mid-loop error it returns what it has so far
    /// (offline / disabled ⇒ empty), never throwing into the UI.
    func listMaps() async -> [LTM.Summary] {
        guard let client else { return [] }
        var all: [LTM.Summary] = []
        var seen = Set<String>()
        var offset = 0
        for _ in 0..<Self.listMaxPages {
            guard let page = try? await client.listMaps(
                limit: Self.listPageSize, offset: offset)
            else { return all }
            // Advance by the RAW page size (not deduped count) so an
            // all-duplicates page — a map bumped between requests shifts
            // offsets — still makes progress instead of refetching forever.
            offset += page.maps.count
            for summary in page.maps where seen.insert(summary.mapId).inserted {
                all.append(summary)
            }
            if page.maps.isEmpty || offset >= page.total { return all }
        }
        return all
    }

    /// Create a new solo map and re-point the backend at it, reporting its map.
    /// Returns the created map's id, or nil if no client is available / it failed.
    @discardableResult
    func createMap(title: String, mapID: String? = nil) async -> String? {
        guard let client else { return nil }
        guard let map = try? await client.createMap(title: title, source: .solo, mapID: mapID)
        else { return nil }
        await repoint(to: map.mapId)
        return map.mapId
    }

    /// Open an EXISTING map by id: re-point the store at it, open it (cache +
    /// refresh + flush), and report so the model re-projects the active graph.
    func open(mapID: String) async {
        await repoint(to: mapID)
    }

    /// Rename a map by patching its title. No store re-point (the open map's own
    /// rename still flows through `setTitle`); this is the library-level patch.
    func renameMap(id: String, title: String) async {
        guard let client else { return }
        _ = try? await client.patchMap(id, title: title, lifecycle: nil)
    }

    /// Archive / un-archive (restore) a map via a lifecycle patch.
    func setArchived(_ archived: Bool, id: String) async {
        guard let client else { return }
        _ = try? await client.patchMap(id, title: nil, lifecycle: archived ? .archived : .active)
    }

    /// Soft-delete a map: there is no hard-delete endpoint, so mark it deleted.
    /// The list filters `deleted` out entirely.
    func deleteMap(id: String) async {
        guard let client else { return }
        _ = try? await client.patchMap(id, title: nil, lifecycle: .deleted)
    }

    /// Duplicate a map = restore-as-branch: fork a NEW map from the source's
    /// latest revision, leaving the source untouched. Returns the new map id.
    @discardableResult
    func duplicateMap(sourceID: String, latestRevision: UInt64, newTitle: String) async -> String? {
        guard let client else { return nil }
        let newID = UUID().uuidString
        guard let branch = try? await client.restore(
            sourceID, atSequence: latestRevision, newMapID: newID, newTitle: newTitle)
        else { return nil }
        return branch.mapId
    }

    // MARK: - Importer (S2e — one-time UserDefaults → canonical replay)

    /// Import ONE local record into a fresh canonical map: create it (with the
    /// local UUID as the map id, so a re-run collides + is skipped), then apply
    /// the planned owner operations. Returns true on a successful create+apply.
    /// Resilient: a create/apply failure returns false and imports nothing (the
    /// caller records success only on true, keeping the ledger idempotent).
    func importRecord(_ record: ThinkingMapRecord) async -> Bool {
        guard let client else { return false }
        let plan = CanonicalMapImportPlanner.importOps(for: record)
        // Create the map at the local id. `already_exists` means a prior run
        // already created it → treat as a no-op success (idempotent).
        do {
            _ = try await client.createMap(title: plan.title, source: .solo, mapID: plan.mapID)
        } catch LTM.APIError.alreadyExists {
            return true
        } catch {
            return false
        }
        guard !plan.operations.isEmpty else { return true }
        do {
            _ = try await client.applyOperations(
                plan.mapID,
                operations: plan.operations,
                baseRevision: 0,
                idempotencyKey: UUID().uuidString,
                envelopeID: nil,
                utteranceID: nil)
            return true
        } catch {
            return false
        }
    }

    // MARK: Internals

    /// Rebuild the `SyncStore` over the shared client/persistence for `mapID`,
    /// open it, and report the resulting map. No-op if no client is retained.
    private func repoint(to mapID: String) async {
        guard let client, let persistence else { return }
        syncStore = LTM.SyncStore(mapID: mapID, client: client, persistence: persistence)
        try? await syncStore.open()
        reportMap()
    }

    private func apply(_ ops: [LTM.Operation]) async {
        await syncStore.apply(ops)
        reportMap()
    }

    private func reportMap() {
        if let map = syncStore.map { onMap(map) }
    }
}
