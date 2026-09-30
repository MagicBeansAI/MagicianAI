//
//  ThinkingMapModel.swift
//  Magios
//
//  Local-first data model for E0's Thinking Map. Extracted verbatim from
//  ThinkingMapPrototypeView.swift as a pure move (no logic changes) to shrink
//  the view file and isolate the upcoming rewire onto the canonical backend
//  (LTM.SyncStore). Owns the persistent record/snapshot/envelope types and the
//  ObservableObject model; views and the AI-bridge types remain in
//  ThinkingMapPrototypeView.swift and reference these across the module.
//

import Foundation
import SwiftUI
import Combine

enum ThinkingMapMode: String, CaseIterable, Identifiable, Codable {
    case map = "Map"
    case focus = "Focus"
    case outline = "Outline"
    var id: String { rawValue }
    var label: String { self == .map ? "Canvas" : rawValue }
    var icon: String {
        switch self {
        case .map: "point.3.connected.trianglepath.dotted"
        case .focus: "scope"
        case .outline: "list.bullet.indent"
        }
    }
}

enum ThinkingNodeKind: String, CaseIterable, Identifiable, Codable {
    case idea = "Idea"
    case question = "Question"
    case risk = "Risk"
    case decision = "Decision"
    case action = "Action"
    // Additional canonical kinds — raw values match LTM.NodeKind (snake_case,
    // single words) so this enum aligns 1:1 with the backend contract.
    case fact = "fact"
    case option = "option"
    case metric = "metric"
    case assumption = "assumption"
    case evidence = "evidence"
    case group = "group"
    var id: String { rawValue }
    var icon: String {
        switch self {
        case .idea: "lightbulb.fill"
        case .question: "questionmark.bubble.fill"
        case .risk: "exclamationmark.triangle.fill"
        case .decision: "checkmark.seal.fill"
        case .action: "arrow.up.forward.circle.fill"
        case .fact: "checkmark.seal"
        case .option: "arrow.triangle.branch"
        case .metric: "chart.bar"
        case .assumption: "questionmark.diamond"
        case .evidence: "doc.text.magnifyingglass"
        case .group: "square.stack.3d.up"
        }
    }
    var color: Color {
        let theme = ThemeManager.shared
        switch self {
        case .idea: return theme.discoveryColor
        case .question: return theme.infoColor
        case .risk: return theme.warningColor
        case .decision: return theme.successColor
        case .action: return theme.accentColor
        case .fact: return theme.successColor.opacity(0.75)
        case .option: return theme.accentHoverColor
        case .metric: return theme.infoColor.opacity(0.7)
        case .assumption: return theme.warningColor.opacity(0.7)
        case .evidence: return theme.discoveryColor.opacity(0.7)
        case .group: return theme.secondaryTextColor
        }
    }
}

enum ThinkingEdgeKind: String, Codable {
    case branch
    case related
}

struct ThinkingNode: Identifiable, Codable, Equatable {
    let id: UUID
    var parentID: UUID?
    var kind: ThinkingNodeKind
    var title: String
    var detail: String
    var source: String
    var suggested: Bool
    var revision: Int
    let createdAt: Date
    /// Destination kinds this node has been promoted to (`"task"`/`"memory"`),
    /// projected from the canonical node's `promoted_refs`. ADDITIVE with a
    /// default: old persisted E0 snapshots (which predate the field) decode to
    /// `[]` via the custom `init(from:)` below, so the read-only importer path
    /// stays back-compatible. Drives the detail sheet's promoted badges.
    var promotedKinds: [String] = []
}

extension ThinkingNode {
    // Custom decode ONLY so `promotedKinds` may be absent (old E0 data);
    // declared in an extension so the memberwise init survives. Encoding
    // stays synthesized (always writes the field).
    private enum DecodingKeys: String, CodingKey {
        case id, parentID, kind, title, detail, source, suggested, revision,
             createdAt, promotedKinds
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: DecodingKeys.self)
        id = try c.decode(UUID.self, forKey: .id)
        parentID = try c.decodeIfPresent(UUID.self, forKey: .parentID)
        kind = try c.decode(ThinkingNodeKind.self, forKey: .kind)
        title = try c.decode(String.self, forKey: .title)
        detail = try c.decode(String.self, forKey: .detail)
        source = try c.decode(String.self, forKey: .source)
        suggested = try c.decode(Bool.self, forKey: .suggested)
        revision = try c.decode(Int.self, forKey: .revision)
        createdAt = try c.decode(Date.self, forKey: .createdAt)
        promotedKinds = try c.decodeIfPresent([String].self, forKey: .promotedKinds) ?? []
    }
}

struct ThinkingEdge: Identifiable, Codable, Equatable {
    let id: UUID
    let from: UUID
    let to: UUID
    let kind: ThinkingEdgeKind
}

struct ThinkingConnectionSuggestion: Identifiable, Equatable {
    let id = UUID()
    let from: UUID
    let to: UUID
    let reason: String
}

struct ThinkingMapSnapshot: Codable, Equatable {
    var title: String
    var nodes: [ThinkingNode]
    var edges: [ThinkingEdge]
    var activeNodeID: UUID?
}

struct ThinkingMapRecord: Identifiable, Codable, Equatable {
    let id: UUID
    let createdAt: Date
    var updatedAt: Date
    var lastOpenedAt: Date
    var isPinned: Bool
    var isArchived: Bool
    var preferredMode: ThinkingMapMode
    /// Exact Loom-owned chat session for this idea. Optional keeps existing
    /// local map libraries backward-compatible; the first AI turn binds it.
    var brainstormSessionID: String?
    var snapshot: ThinkingMapSnapshot

    var title: String { snapshot.title }
    var nodeCount: Int { snapshot.nodes.count }
    var activeThought: String {
        snapshot.activeNodeID
            .flatMap { id in snapshot.nodes.first(where: { $0.id == id })?.title }
            ?? snapshot.nodes.first?.title
            ?? "Unstarted idea"
    }
    var capturedNodeCount: Int { snapshot.nodes.filter { !$0.suggested }.count }
    var questionCount: Int { snapshot.nodes.filter { $0.kind == .question }.count }
    var actionCount: Int { snapshot.nodes.filter { $0.kind == .action }.count }

    func matches(_ query: String) -> Bool {
        let needle = query.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard !needle.isEmpty else { return true }
        if title.lowercased().contains(needle) || activeThought.lowercased().contains(needle) { return true }
        return snapshot.nodes.contains {
            $0.title.lowercased().contains(needle) || $0.detail.lowercased().contains(needle)
        }
    }
}

private struct ThinkingMapLibraryEnvelope: Codable, Equatable {
    static let currentVersion = 2
    var version = currentVersion
    var maps: [ThinkingMapRecord]
    var selectedMapID: UUID?
}

@MainActor
final class ThinkingMapModel: ObservableObject {
    static let shared = ThinkingMapModel()
    nonisolated static let persistenceKey = "thinking-map.library.v2"
    nonisolated static let legacyPersistenceKey = "thinking-map.local.v1"

    @Published private(set) var title = "Idea space"
    @Published private(set) var nodes: [ThinkingNode] = []
    @Published private(set) var edges: [ThinkingEdge] = []
    @Published private(set) var activeNodeID: UUID?
    @Published private(set) var pendingConnection: ThinkingConnectionSuggestion?
    @Published private(set) var maps: [ThinkingMapRecord] = []
    @Published private(set) var openMapID: UUID?
    /// The user's most recent local selection while its canonical shared-view
    /// operation is still being acknowledged. A racing stale projection may
    /// update the graph, but must not visibly snap the active cursor to root.
    private var pendingActiveNodeID: UUID?

    /// Open (unanswered) clarifications the model asked about a node — projected
    /// from the canonical map on every `onMap`. The (future) UI renders these and
    /// calls `answerClarification`.
    @Published private(set) var clarifications: [ThinkingClarification] = []
    /// Pending restructure proposals (`state == .proposed`) staged by the model,
    /// awaiting the owner's confirm/reject — projected on every `onMap`. The
    /// (future) UI renders these and calls `decideProposal`.
    @Published private(set) var pendingProposals: [ThinkingProposal] = []

    /// A transient AI-unavailable flag set when a canonical AI action
    /// (consolidate / interpret) fails (e.g. offline). The (future) UI can surface
    /// a soft banner; cleared on the next successful projection.
    @Published private(set) var aiUnavailable = false

    /// True while the map is in ambient "Listen" mode — a live voice session is
    /// attached server-side, so every finalized spoken user turn auto-maps onto
    /// the board. The UI shows a listening indicator and a live-refresh runs
    /// while this is set. See `startListening(sessionID:)` / `stopListening()`.
    @Published private(set) var isListening = false

    /// Set when entering Listen mode failed (offline / no ambient coordinator /
    /// no live session). The UI can surface a soft notice; cleared on the next
    /// successful `startListening`.
    @Published private(set) var listeningUnavailable = false

    /// The `UserDefaults`/key that hold the OLD E0 `thinking-map.library.v2`
    /// library. Retained READ-ONLY: the importer decodes existing E0 maps from
    /// here to migrate them into the canonical backend. Nothing writes them.
    private let defaults: UserDefaults?
    private let storageKey: String?

    // MARK: Canonical backing (the only path)
    //
    // The canonical backend is the sole source of truth. Every mutation delegates
    // to it and the model's @Published nodes/edges/activeNodeID are set from
    // `CanonicalThinkingMapProjection.project(map)` whenever the backend reports a
    // fresh authoritative map. The old E0 `UserDefaults` library is READ ONLY —
    // `importLocalMapsIfNeeded` migrates it once into the canonical backend and
    // otherwise leaves it untouched as a backup.

    /// The canonical backend — the sole source of truth. Always constructed in
    /// `init`; implicitly-unwrapped only so the `onMap` closure can capture `self`
    /// after the other stored properties are set.
    private var canonicalBackend: CanonicalThinkingMapBackend!
    /// Exact canonical string keys for projected UUID view identifiers. UUIDs
    /// compare without letter case, while the canonical map store keys by the
    /// original string; round-tripping through `UUID.uuidString` can therefore
    /// target a different node. Replaced atomically with every projection.
    private var canonicalNodeIDs: [UUID: String] = [:]
    private var canonicalEdgeIDs: [UUID: String] = [:]
    /// Client-local (pin / preferredMode / lastOpened) prefs + import ledger for
    /// the canonical library. `maps` is projected from the server list merged with
    /// THIS.
    private let canonicalLocalPrefs = CanonicalMapLocalPrefs()
    /// Guards `importLocalMapsIfNeeded` so it runs at most once per launch.
    private var didImportLocalMaps = false

    /// The source session id currently attached for ambient mapping (the live
    /// voice media session id), or nil when not listening — used to detach on
    /// stop / when re-pointing to a new session.
    private var attachedListenSessionID: String?
    /// Realtime `ThinkingMapUpdated` push subscriber — the PRIMARY refresh
    /// signal while listening (a spoken turn lands server-side → notice →
    /// immediate re-fetch). Started/stopped with Listen mode.
    private let listenRealtime = ThinkingMapRealtime()
    /// The slow SAFETY poll behind the push: a dropped socket or missed notice
    /// costs at most one interval, never a stale board. Cancelled on
    /// `stopListening`.
    private var listenPollTask: Task<Void, Never>?

    init(defaults: UserDefaults? = .standard, storageKey: String? = ThinkingMapModel.persistenceKey) {
        self.defaults = defaults
        self.storageKey = storageKey
        // Stand up the canonical backend. It reports every authoritative map
        // through `applyProjectedMap`, which re-projects onto the published state.
        // TODO(S4/S5): the app owns real mapID selection + open()/refresh()
        // sequencing; a fresh solo map id is used here as a scaffold.
        canonicalBackend = CanonicalThinkingMapBackend(
            mapID: "ios-thinking-map-solo",
            onMap: { [weak self] map in
                Task { @MainActor in self?.applyProjectedMap(map) }
            }
        )
        // Kick the one-time UserDefaults→canonical import + first library
        // projection on activation. Idempotent (guarded + ledgered); the OLD E0
        // UserDefaults data is read-only here and left intact.
        Task { @MainActor [weak self] in
            await self?.importLocalMapsIfNeeded()
            await self?.refreshLibrary()
        }
    }

    /// Re-project a canonical map onto the published view state. Called from the
    /// backend's `onMap` callback on the main actor.
    private func applyProjectedMap(_ map: LTM.Map) {
        let projected = CanonicalThinkingMapProjection.project(map)
        title = map.title
        nodes = projected.nodes
        edges = projected.edges
        canonicalNodeIDs = projected.canonicalNodeIDs
        canonicalEdgeIDs = projected.canonicalEdgeIDs
        let reconciledSelection = Self.reconcileActiveNodeSelection(
            projected: projected.activeNodeID,
            pending: pendingActiveNodeID,
            liveNodeIDs: Set(projected.nodes.map(\.id))
        )
        activeNodeID = reconciledSelection.active
        pendingActiveNodeID = reconciledSelection.pending
        clarifications = CanonicalThinkingMapProjection.projectClarifications(map)
        pendingProposals = CanonicalThinkingMapProjection.projectProposals(map)
        // A fresh authoritative map means the backend is reachable again.
        aiUnavailable = false
    }

    /// Preserve an optimistic local selection across an older authoritative
    /// projection. The pending marker clears only when that projection carries
    /// the same node (acknowledged) or the node no longer exists (invalidated).
    nonisolated static func reconcileActiveNodeSelection(
        projected: UUID?,
        pending: UUID?,
        liveNodeIDs: Set<UUID>
    ) -> (active: UUID?, pending: UUID?) {
        guard let pending else { return (projected, nil) }
        guard liveNodeIDs.contains(pending) else { return (projected, nil) }
        guard projected != pending else { return (pending, nil) }
        return (pending, pending)
    }

    /// Exact canonical string id for a published UUID node. Locally minted nodes
    /// have not been projected yet, so their UUID spelling remains the fallback.
    private func canonicalID(for uuid: UUID) -> String {
        canonicalNodeIDs[uuid] ?? uuid.uuidString
    }

    /// Exact canonical id for a projected related edge. Synthetic branch edges
    /// intentionally have no canonical edge record and are never disconnectable.
    private func canonicalEdgeID(for uuid: UUID) -> String {
        canonicalEdgeIDs[uuid] ?? uuid.uuidString
    }

    /// Refresh the map from its backing store: pull the canonical map AND
    /// re-project the library from the server, running the one-time importer
    /// first so old E0 maps surface in the canonical list.
    func refresh() async {
        await importLocalMapsIfNeeded()
        await canonicalBackend.refresh()
        await refreshLibrary()
    }

    /// Re-project `maps` from the canonical server list merged with the
    /// client-local prefs. `deleted` maps are filtered out entirely; `archived`
    /// maps are KEPT (the Archive filter shows them). Search stays client-side
    /// over the projected records (`ThinkingMapRecord.matches`).
    func refreshLibrary() async {
        let summaries = await canonicalBackend.listMaps()
        maps = summaries
            .filter { $0.lifecycle != .deleted }
            .map { summary in
                CanonicalMapLibraryProjection.record(
                    from: summary,
                    pref: canonicalLocalPrefs.pref(for: summary.mapId))
            }
    }

    /// The canonical map-id string for an E0 library UUID. Real canonical ids ARE
    /// UUID strings; the projection preserves them, so the round-trip is exact.
    private func canonicalMapID(for id: UUID) -> String { id.uuidString }

    /// The server's latest revision for a projected library id, if known — used
    /// as the branch point for duplicate = restore-as-branch.
    private func latestRevision(for id: UUID) async -> UInt64 {
        let target = canonicalMapID(for: id)
        return await canonicalBackend.listMaps()
            .first { $0.mapId == target }?.latestRevision ?? 0
    }

    /// True when the canonical backend is live (has a client). The AI
    /// bridge checks this to route a frontier request through `/interpret`.
    var isCanonicalBackendActive: Bool { canonicalBackend.client != nil }

    /// The canonical map id, for subscribers that filter realtime frames by
    /// map (the intelligence controller's progress channel).
    var currentCanonicalMapID: String { canonicalBackend.currentMapID }

    /// The canonical AI "frontier" request. Forwards `text` + `intent` to
    /// the backend's `/interpret`, which APPLIES the model's operations
    /// server-side and returns the updated map; the backend's `onMap` callback
    /// then re-projects, so the new `model_inferred`/`provisional` nodes surface
    /// as dashed `suggested` cards (NOT a separate prompt list). Rethrows so the
    /// caller can fall back to the local prompt palette when `/interpret` is
    /// unavailable (e.g. offline).
    ///
    /// `utteranceID` is the caller-minted run id: the server tags each
    /// `ThinkingMapInterpretProgress` stage event with it, which is how the
    /// strip claims narration for exactly this run and no other.
    func canonicalInterpret(
        text: String,
        intent: LTM.InterpretIntent,
        focusNodeID: UUID? = nil,
        utteranceID: String? = nil
    ) async throws {
        try await canonicalBackend.interpret(
            text: text,
            intent: intent,
            focusNodeID: focusNodeID.map(canonicalID(for:)),
            utteranceID: utteranceID)
    }

    // MARK: - Clarifications + restructure (future-UI actions)
    //
    // Thin async wrappers the (future) clarification/restructure UI calls. Each is
    // a no-op when the canonical backend isn't live (no client), and each handles
    // errors gracefully (a transient `aiUnavailable` flag) rather than throwing
    // into the UI — mirroring how the AI frontier degrades.

    /// Ask the model to STAGE a restructure proposal. On success the backend's
    /// `onMap` re-projects, populating `pendingProposals`. On failure (offline /
    /// disabled) sets `aiUnavailable` and leaves the map unchanged.
    func consolidate() async {
        guard isCanonicalBackendActive else { return }
        do {
            try await canonicalBackend.consolidate()
        } catch {
            aiUnavailable = true
        }
    }

    /// Confirm (`confirm == true`) or reject a pending restructure proposal by id.
    /// On confirm the proposal's ops materialize server-side; the re-projection
    /// drops it from `pendingProposals`.
    func decideProposal(_ id: String, confirm: Bool) async {
        guard isCanonicalBackendActive else { return }
        do {
            try await canonicalBackend.decideProposal(id, decision: confirm ? "confirm" : "reject")
        } catch {
            aiUnavailable = true
        }
    }

    /// Answer / resolve a clarification by id. Owner op (works offline via the
    /// queue); the re-projection drops it from `clarifications` once resolved.
    func answerClarification(_ id: String, answer: String?) async {
        guard isCanonicalBackendActive else { return }
        await canonicalBackend.respondClarification(id, answer: answer)
    }

    // MARK: - Node promotion (detail-sheet actions)

    /// Result of a governed node promotion, surfaced to the detail-sheet UI.
    /// `promoted == false` = the node already carried a link of this kind and
    /// the EXISTING object was returned (idempotent — no duplicate).
    struct ThinkingPromotionResult: Equatable {
        let promoted: Bool
        let objectKind: String
        let objectID: String
    }

    /// Promote a node into a durable Magician object (`target` = `"task"` |
    /// `"memory"`). ONLINE only. THROWS — deliberately unlike the other AI
    /// wrappers — because the UI must distinguish
    /// `LTM.APIError.confirmationRequired` (409: the node is AI-suggested, ask
    /// the owner and retry with `confirm: true`) from a plain failure. On
    /// success the backend refreshes, so the node re-projects with the new
    /// promoted kind (the badge).
    func promoteNode(_ id: UUID, target: String, confirm: Bool = false) async throws -> ThinkingPromotionResult {
        let canonical = canonicalID(for: id)
        let result = try await canonicalBackend.promoteNode(canonical, target: target, confirm: confirm)
        return ThinkingPromotionResult(
            promoted: result.promoted,
            objectKind: result.objectKind.rawValue,
            objectID: result.objectId)
    }

    // MARK: - Ambient "Listen" mode
    //
    // Attaches a live voice session to the OPEN map so spoken user turns
    // auto-map onto the board (the server-side `ThinkingMapSessionCoordinator`),
    // and runs a light live-refresh so the new nodes surface. The voice session
    // itself is owned by the view (`RealtimeVoiceClient`); the model is handed
    // its **media session id** — the same id the coordinator matches against a
    // spoken turn's `presence_session_id`.

    /// Enter Listen mode for `sessionID` (a live voice media session id): attach
    /// it to the open map for ambient mapping and start the live-refresh loop.
    /// A no-op when already listening on the same session; re-points (detaches
    /// the old, attaches the new) when a different session id arrives. On attach
    /// failure sets `listeningUnavailable` and stays out of Listen mode.
    func startListening(sessionID: String) async {
        guard isCanonicalBackendActive else {
            listeningUnavailable = true
            return
        }
        // Already listening on this exact session ⇒ nothing to do.
        if isListening, attachedListenSessionID == sessionID { return }
        // Switching sessions ⇒ detach the previous binding first (best-effort).
        if let existing = attachedListenSessionID, existing != sessionID {
            _ = try? await canonicalBackend.detachSession(existing)
            attachedListenSessionID = nil
        }
        do {
            _ = try await canonicalBackend.attachSession(sessionID)
        } catch {
            listeningUnavailable = true
            return
        }
        attachedListenSessionID = sessionID
        listeningUnavailable = false
        isListening = true
        // Push-first refresh: a ThinkingMapUpdated notice for the open map
        // triggers an immediate authoritative re-fetch. The slow poll below is
        // only the safety net for a dropped socket / missed notice.
        listenRealtime.start(mapID: canonicalBackend.currentMapID) { [weak self] in
            Task { @MainActor in await self?.canonicalBackend.refresh() }
        }
        startListenPolling()
    }

    /// Leave Listen mode: cancel the live-refresh loop and detach the session so
    /// the server stops auto-mapping its turns. Idempotent (safe to call when not
    /// listening). Call this when the map view disappears.
    func stopListening() async {
        listenRealtime.stop()
        listenPollTask?.cancel()
        listenPollTask = nil
        isListening = false
        if let sessionID = attachedListenSessionID {
            attachedListenSessionID = nil
            _ = try? await canonicalBackend.detachSession(sessionID)
        }
    }

    /// Surface the soft "couldn't start listening" notice. Called by the view
    /// when the live voice call never went ready, so Listen mode never began —
    /// the model itself has no visibility into the call's transport.
    func markListeningUnavailable() {
        listeningUnavailable = true
    }

    /// SAFETY-poll cadence while listening (20s). The realtime
    /// `ThinkingMapUpdated` push is the primary refresh signal — this loop only
    /// bounds the staleness window when the socket is down or a notice is
    /// missed, so it can be an order of magnitude slower than the old 2s poll.
    private static let listenPollNanoseconds: UInt64 = 20_000_000_000

    /// Spawn (or replace) the safety-poll loop that refreshes the OPEN map
    /// while listening. Runs on the model's `@MainActor` context (inherited by
    /// the `Task`); `[weak self]` so it never keeps the model alive, and it
    /// no-ops once `self` is gone. Refreshes only the open map (not the library).
    private func startListenPolling() {
        listenPollTask?.cancel()
        listenPollTask = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: ThinkingMapModel.listenPollNanoseconds)
                if Task.isCancelled { return }
                await self?.canonicalBackend.refresh()
            }
        }
    }

    /// One-time migration of the OLD E0 `UserDefaults` library into the canonical
    /// backend. Runs at most once per launch (guarded by `didImportLocalMaps`)
    /// and is idempotent across launches via the local-prefs import ledger.
    ///
    /// It reads the EXISTING E0 `thinking-map.library.v2` envelope directly (NOT
    /// mutating or deleting it — that data stays as a backup), and for each
    /// not-yet-imported record replays its content as OWNER operations through the
    /// backend (`createMap` + add_node + move_to_parent + connect). One record
    /// failing logs + continues; nothing is deleted.
    func importLocalMapsIfNeeded() async {
        guard !didImportLocalMaps else { return }
        didImportLocalMaps = true
        guard canonicalBackend.client != nil else { return }

        let records = loadLocalLibraryRecords()
        for record in records {
            let localID = record.id.uuidString
            guard !canonicalLocalPrefs.isImported(localMapID: localID) else { continue }
            let ok = await canonicalBackend.importRecord(record)
            if ok {
                canonicalLocalPrefs.markImported(localMapID: localID)
            }
            // On failure: leave it unmarked (a later launch retries) and continue
            // to the next record — a single bad map never blocks the rest.
        }
    }

    /// Decode the OLD E0 `UserDefaults` library into its records, WITHOUT touching
    /// the model's own state or the stored data. Reads from the model's OWN
    /// `defaults`/`storageKey` so the import sees exactly the E0 library; returns
    /// [] when nothing is saved. This is read-only — the E0 data is left intact as
    /// a backup.
    ///
    /// Falls back to the pre-library legacy single-snapshot (`thinking-map.local.v1`)
    /// so the very oldest maps (users who never gained a v2 library) still migrate,
    /// mirroring the original `restore()` legacy path.
    private func loadLocalLibraryRecords() -> [ThinkingMapRecord] {
        guard let defaults, let storageKey else { return [] }
        let decoder = JSONDecoder()
        let candidates = [
            defaults.data(forKey: storageKey),
            defaults.data(forKey: storageKey + ".backup")
        ].compactMap { $0 }
        if let envelope = candidates.lazy
            .compactMap({ try? decoder.decode(ThinkingMapLibraryEnvelope.self, from: $0) })
            .first {
            return envelope.maps
        }

        // No v2 library — try the legacy single-snapshot store as a last resort.
        var legacyCandidates = candidates
        if storageKey == Self.persistenceKey,
           let originalStore = defaults.data(forKey: Self.legacyPersistenceKey) {
            legacyCandidates.append(originalStore)
        }
        if let legacy = legacyCandidates.lazy
            .compactMap({ try? decoder.decode(ThinkingMapSnapshot.self, from: $0) })
            .first(where: { !$0.nodes.isEmpty }) {
            let now = Date()
            return [ThinkingMapRecord(
                id: UUID(),
                createdAt: legacy.nodes.map(\.createdAt).min() ?? now,
                updatedAt: now,
                lastOpenedAt: now,
                isPinned: false,
                isArchived: false,
                preferredMode: .map,
                brainstormSessionID: nil,
                snapshot: legacy
            )]
        }
        return []
    }

    var hasMap: Bool { !nodes.isEmpty }
    var activeNode: ThinkingNode? { activeNodeID.flatMap(node) }

    /// A sensible node to focus when none is explicitly active — the root
    /// thought (no parent) if the map has one, otherwise the first node in
    /// exploration order. Nil only for an empty map. Focus/Outline modes use
    /// this so entering them on a freshly-opened map is never blank.
    var defaultFocusNode: ThinkingNode? {
        if let active = activeNode { return active }
        return nodes.first(where: { $0.parentID == nil }) ?? orderedNodes.first
    }

    /// Ensure some node is the active/focused cursor, defaulting to
    /// `defaultFocusNode` when nothing is selected yet. A no-op once an active
    /// node exists, so it never overrides the user's current selection.
    func focusDefaultIfNeeded() {
        guard activeNode == nil, let target = defaultFocusNode else { return }
        select(target.id)
    }

    var openRecord: ThinkingMapRecord? { openMapID.flatMap { id in maps.first { $0.id == id } } }
    var preferredMode: ThinkingMapMode { openRecord?.preferredMode ?? .map }

    var libraryMaps: [ThinkingMapRecord] {
        maps.sorted {
            if $0.isPinned != $1.isPinned { return $0.isPinned }
            return $0.lastOpenedAt > $1.lastOpenedAt
        }
    }

    var ancestry: [ThinkingNode] {
        guard let activeNodeID else { return [] }
        var result: [ThinkingNode] = []
        var cursor: UUID? = activeNodeID
        var visited = Set<UUID>()
        while let id = cursor, !visited.contains(id), let current = node(id) {
            visited.insert(id)
            result.append(current)
            cursor = current.parentID
        }
        return result.reversed()
    }

    var orderedNodes: [ThinkingNode] {
        nodes.sorted {
            let lhsDepth = depth(of: $0.id)
            let rhsDepth = depth(of: $1.id)
            return lhsDepth == rhsDepth ? $0.createdAt < $1.createdAt : lhsDepth < rhsDepth
        }
    }

    var branchPrompts: [ThinkingPrompt] {
        guard let active = activeNode else { return [] }
        switch active.kind {
        case .idea:
            return [
                .init(id: "idea-question", title: "Ask what matters", detail: "What must become clearer?", icon: "questionmark.bubble", kind: .question),
                .init(id: "idea-risk", title: "Stress-test it", detail: "What could make this fail?", icon: "exclamationmark.triangle", kind: .risk),
                .init(id: "idea-action", title: "Make it smaller", detail: "What is the smallest useful experiment?", icon: "flask", kind: .action)
            ]
        case .question:
            return [
                .init(id: "question-answer", title: "Offer an answer", detail: "Capture a working answer, even if uncertain.", icon: "lightbulb", kind: .idea),
                .init(id: "question-evidence", title: "Find evidence", detail: "What would change your confidence?", icon: "magnifyingglass", kind: .question),
                .init(id: "question-assumption", title: "Expose an assumption", detail: "What are you taking for granted?", icon: "eye", kind: .risk)
            ]
        case .risk:
            return [
                .init(id: "risk-reduce", title: "Reduce the risk", detail: "How could this become safer or reversible?", icon: "shield", kind: .idea),
                .init(id: "risk-signal", title: "Define a warning sign", detail: "What would tell you this is happening?", icon: "waveform.path.ecg", kind: .question),
                .init(id: "risk-response", title: "Prepare a response", detail: "What would you do next?", icon: "arrow.up.forward", kind: .action)
            ]
        case .decision:
            return [
                .init(id: "decision-reverse", title: "Set a reversal trigger", detail: "What would make you revisit this?", icon: "arrow.uturn.backward", kind: .risk),
                .init(id: "decision-action", title: "Make the first commitment", detail: "What concretely happens next?", icon: "checkmark.circle", kind: .action),
                .init(id: "decision-alt", title: "Keep an alternative", detail: "What is the strongest other route?", icon: "arrow.triangle.branch", kind: .idea)
            ]
        case .action:
            return [
                .init(id: "action-prereq", title: "Name the prerequisite", detail: "What must be true first?", icon: "list.bullet.clipboard", kind: .question),
                .init(id: "action-blocker", title: "Find the blocker", detail: "What could stop this action?", icon: "exclamationmark.octagon", kind: .risk),
                .init(id: "action-next", title: "Add the next move", detail: "What follows after this?", icon: "arrow.right", kind: .action)
            ]
        case .fact:
            return [
                .init(id: "fact-implication", title: "Draw the implication", detail: "If this is true, what follows?", icon: "lightbulb", kind: .idea),
                .init(id: "fact-source", title: "Cite the source", detail: "What backs this up?", icon: "doc.text.magnifyingglass", kind: .evidence),
                .init(id: "fact-question", title: "Probe the edges", detail: "Where might this not hold?", icon: "questionmark.bubble", kind: .question)
            ]
        case .option:
            return [
                .init(id: "option-tradeoff", title: "Weigh the trade-off", detail: "What do you give up by choosing this?", icon: "scalemass", kind: .risk),
                .init(id: "option-choose", title: "Make the call", detail: "Is this the route you commit to?", icon: "checkmark.seal", kind: .decision),
                .init(id: "option-alt", title: "Name another route", detail: "What is the strongest alternative?", icon: "arrow.triangle.branch", kind: .option)
            ]
        case .metric:
            return [
                .init(id: "metric-target", title: "Set the target", detail: "What number would count as success?", icon: "target", kind: .decision),
                .init(id: "metric-measure", title: "Define the measurement", detail: "How exactly do you capture this?", icon: "ruler", kind: .action),
                .init(id: "metric-risk", title: "Watch for gaming", detail: "How could this metric mislead?", icon: "exclamationmark.triangle", kind: .risk)
            ]
        case .assumption:
            return [
                .init(id: "assumption-test", title: "Test the assumption", detail: "What would confirm or break it?", icon: "magnifyingglass", kind: .evidence),
                .init(id: "assumption-risk", title: "If it's wrong…", detail: "What breaks if this is false?", icon: "exclamationmark.triangle", kind: .risk),
                .init(id: "assumption-question", title: "Question it directly", detail: "Why do you believe this?", icon: "questionmark.bubble", kind: .question)
            ]
        case .evidence:
            return [
                .init(id: "evidence-conclusion", title: "State what it shows", detail: "What can you now assert?", icon: "checkmark.seal", kind: .fact),
                .init(id: "evidence-counter", title: "Look for counter-evidence", detail: "What would point the other way?", icon: "magnifyingglass", kind: .evidence),
                .init(id: "evidence-question", title: "Question the strength", detail: "How reliable is this?", icon: "questionmark.bubble", kind: .question)
            ]
        case .group:
            return [
                .init(id: "group-theme", title: "Name the theme", detail: "What ties these together?", icon: "lightbulb", kind: .idea),
                .init(id: "group-gap", title: "Find the gap", detail: "What is missing from this cluster?", icon: "questionmark.bubble", kind: .question),
                .init(id: "group-next", title: "Act on the cluster", detail: "What does this group ask you to do?", icon: "arrow.right", kind: .action)
            ]
        }
    }

    var exportMarkdown: String {
        markdown(for: ThinkingMapSnapshot(title: title, nodes: nodes, edges: edges, activeNodeID: activeNodeID))
    }

    func exportMarkdown(for mapID: UUID) -> String {
        guard let record = maps.first(where: { $0.id == mapID }) else { return "" }
        return markdown(for: record.snapshot)
    }

    private func markdown(for snapshot: ThinkingMapSnapshot) -> String {
        func snapshotNode(_ id: UUID) -> ThinkingNode? { snapshot.nodes.first { $0.id == id } }
        func snapshotDepth(_ node: ThinkingNode) -> Int {
            var result = 0
            var cursor = node.parentID
            var visited = Set<UUID>()
            while let id = cursor, !visited.contains(id), let parent = snapshotNode(id) {
                visited.insert(id)
                result += 1
                cursor = parent.parentID
            }
            return result
        }
        let ordered = snapshot.nodes.sorted {
            let lhs = snapshotDepth($0)
            let rhs = snapshotDepth($1)
            return lhs == rhs ? $0.createdAt < $1.createdAt : lhs < rhs
        }
        var lines = ["# \(snapshot.title)", ""]
        for node in ordered {
            let indent = String(repeating: "  ", count: snapshotDepth(node))
            lines.append("\(indent)- **\(node.kind.rawValue):** \(node.title)\(node.suggested ? " _(suggested)_" : "")")
            if !node.detail.isEmpty, node.detail != node.title {
                lines.append("\(indent)  \(node.detail)")
            }
        }
        let crossLinks = snapshot.edges.filter { $0.kind == .related }
        if !crossLinks.isEmpty {
            lines += ["", "## Connections"]
            for edge in crossLinks {
                if let from = snapshotNode(edge.from), let to = snapshotNode(edge.to) {
                    lines.append("- \(from.title) ↔ \(to.title)")
                }
            }
        }
        return lines.joined(separator: "\n")
    }

    func node(_ id: UUID) -> ThinkingNode? { nodes.first { $0.id == id } }

    func children(of id: UUID) -> [ThinkingNode] {
        nodes.filter { $0.parentID == id }.sorted { lhs, rhs in
            if lhs.suggested != rhs.suggested { return !lhs.suggested }
            return lhs.createdAt < rhs.createdAt
        }
    }

    func depth(of id: UUID) -> Int {
        var result = 0
        var cursor = node(id)?.parentID
        var visited = Set<UUID>()
        while let parent = cursor, !visited.contains(parent), let parentNode = node(parent) {
            visited.insert(parent)
            result += 1
            cursor = parentNode.parentID
        }
        return result
    }

    func relatedNodes(to id: UUID) -> [ThinkingNode] {
        let ids = edges.compactMap { edge -> UUID? in
            guard edge.kind == .related else { return nil }
            if edge.from == id { return edge.to }
            if edge.to == id { return edge.from }
            return nil
        }
        return ids.compactMap(node)
    }

    func begin(with text: String) {
        let cleaned = normalized(text)
        guard !cleaned.isEmpty else { return }
        // Seed the map as the first owner-spoken root thought; the fresh root
        // becomes the active cursor (E0 parity — capture advances the cursor).
        Task {
            let newID = await canonicalBackend.addThought(cleaned, kind: .idea, activeID: nil)
            await canonicalBackend.select(newID)
        }
    }

    /// Create a NEW map and seed it with `text` as the root thought, in ONE
    /// sequential flow. The `@brainstorm` chat lane used to call
    /// `startNewMap()` then `begin(with:)` — two independent async Tasks — and
    /// the seed could race onto the PREVIOUS map before the backend re-pointed
    /// (E0's versions were synchronous, so the pair was safe there). Await the
    /// create + re-point BEFORE adding the seed so it can't misland.
    /// `detail` optionally overrides the seed node's detail markdown — Share
    /// ingestion passes the full shared text + a provenance line while `text`
    /// stays a bounded label.
    func beginNewMap(with text: String, detail: String? = nil) {
        let cleaned = normalized(text)
        // The seed titles the map (truncated) so the library card + search read
        // the thought, not "Idea space" — E0's records carried the seed as the
        // title, and the web `@brainstorm` lane titles the same way.
        let mapTitle = cleaned.isEmpty
            ? "Idea space"
            : (cleaned.count > 60 ? String(cleaned.prefix(57)) + "…" : cleaned)
        openMapID = nil
        title = mapTitle
        nodes = []
        edges = []
        activeNodeID = nil
        pendingActiveNodeID = nil
        pendingConnection = nil
        Task { @MainActor in
            guard let newID = await canonicalBackend.createMap(title: mapTitle) else {
                await refreshLibrary()
                return
            }
            openMapID = CanonicalMapLibraryProjection.resolveUUID(newID)
            canonicalLocalPrefs.markOpened(newID)
            if !cleaned.isEmpty {
                let nodeID = await canonicalBackend.addThought(
                    cleaned, kind: .idea, activeID: nil, detail: detail)
                await canonicalBackend.select(nodeID)
            }
            await refreshLibrary()
        }
    }

    /// The append target for a shared "Add to current Thinking Map" seed: the
    /// most-recently-opened NON-ARCHIVED map, or nil when there is none (the
    /// caller falls back to new-map seeding). Recency is `lastOpenedAt` — a
    /// pin is a library-display concern, not a recency signal — with a stable
    /// id tiebreak. Pure + nonisolated so it unit-tests directly.
    nonisolated static func mostRecentAppendTarget(in maps: [ThinkingMapRecord]) -> ThinkingMapRecord? {
        maps.filter { !$0.isArchived }
            .sorted {
                $0.lastOpenedAt == $1.lastOpenedAt
                    ? $0.id.uuidString < $1.id.uuidString
                    : $0.lastOpenedAt > $1.lastOpenedAt
            }
            .first
    }

    /// Append a shared seed to the MOST-RECENT non-archived map as an owner
    /// thought (the share sheet's "Add to current Thinking Map" choice): open
    /// that map, add the seed as a root-level owner thought (`detail` carries
    /// the share provenance), and select it. Falls back to `beginNewMap` when
    /// the library has no non-archived map. Sequenced as ONE task (refresh →
    /// open → add → select) so the seed can never land on the previously-open
    /// map — the same race `beginNewMap` exists to prevent (`openMap()` +
    /// `addThought()` would be two independent Tasks against a re-pointing
    /// backend, and `addThought` would read the STALE projected active node).
    func appendToMostRecentMap(_ text: String, detail: String? = nil) {
        let cleaned = normalized(text)
        guard !cleaned.isEmpty else { return }
        Task { @MainActor in
            // The library may not be projected yet right after launch (the
            // share drain runs on activation) — refresh before choosing.
            await refreshLibrary()
            guard let target = Self.mostRecentAppendTarget(in: maps) else {
                beginNewMap(with: cleaned, detail: detail)
                return
            }
            let mapID = canonicalMapID(for: target.id)
            openMapID = target.id
            pendingConnection = nil
            canonicalLocalPrefs.markOpened(mapID)
            await canonicalBackend.open(mapID: mapID)
            let nodeID = await canonicalBackend.addThought(
                cleaned, kind: .idea, activeID: nil, detail: detail)
            await canonicalBackend.select(nodeID)
            await refreshLibrary()
        }
    }

    func addThought(_ text: String, preferredKind: ThinkingNodeKind?) {
        let cleaned = normalized(text)
        guard !cleaned.isEmpty else { return }
        guard activeNode != nil else { begin(with: cleaned); return }
        let kind = preferredKind ?? classify(cleaned)
        let active = activeNodeID.map(canonicalID(for:))
        // E0 parity: a capture ADVANCES the active cursor onto the new thought
        // (the conversation flows down the branch you're growing). The selection
        // publishes canonically via `set_shared_view` after the add lands.
        Task {
            let newID = await canonicalBackend.addThought(cleaned, kind: kind, activeID: active)
            await canonicalBackend.select(newID)
        }
    }

    func select(_ id: UUID) {
        guard node(id) != nil else { return }
        // Optimistic local selection now; publish the shared view canonically.
        pendingActiveNodeID = id
        activeNodeID = id
        let canonical = canonicalID(for: id)
        Task { await canonicalBackend.select(canonical) }
    }

    func addSuggestedBranch(_ prompt: ThinkingPrompt, source: String = "Suggested from the active thought") {
        guard let parent = activeNode else { return }
        if let existing = children(of: parent.id).first(where: { $0.title == prompt.detail }) {
            select(existing.id)
            return
        }
        // The fallback palette's suggested branch becomes an owner-captured thought
        // under the active node on the canonical backend. (E0 kept it as a dashed
        // "suggested" node; the canonical model surfaces model-inferred nodes via
        // the projection, so an owner-chosen palette branch is a plain thought.)
        let active = canonicalID(for: parent.id)
        Task { await canonicalBackend.addThought(prompt.detail, kind: prompt.kind, activeID: active) }
    }

    func updateNode(_ id: UUID, title: String, detail: String) {
        let cleanedTitle = normalized(title)
        guard !cleanedTitle.isEmpty, let index = nodes.firstIndex(where: { $0.id == id }) else { return }
        // Editing promotes a model-suggested node to owner-asserted.
        let wasProvisional = nodes[index].suggested
        let canonical = canonicalID(for: id)
        Task {
            await canonicalBackend.updateNode(
                id: canonical, title: cleanedTitle,
                detail: normalized(detail), wasProvisional: wasProvisional)
        }
    }

    func connect(_ from: UUID, to: UUID) {
        guard from != to, node(from) != nil, node(to) != nil else { return }
        guard !edges.contains(where: { $0.kind == .related && Set([$0.from, $0.to]) == Set([from, to]) }) else { return }
        let f = canonicalID(for: from)
        let t = canonicalID(for: to)
        Task { await canonicalBackend.connect(from: f, to: t) }
    }

    func disconnect(_ first: UUID, from second: UUID) {
        guard let index = edges.firstIndex(where: {
            $0.kind == .related && Set([$0.from, $0.to]) == Set([first, second])
        }) else { return }
        // The canonical disconnect keys on the exact edge string retained by
        // the projection; recreating UUID text can change its letter case.
        let canonicalEdgeID = canonicalEdgeID(for: edges[index].id)
        Task { await canonicalBackend.disconnect(edgeID: canonicalEdgeID) }
    }

    func acceptConnectionSuggestion() {
        guard let suggestion = pendingConnection else { return }
        connect(suggestion.from, to: suggestion.to)
        pendingConnection = nil
    }

    func dismissConnectionSuggestion() { pendingConnection = nil }

    func removeBranch(_ id: UUID) {
        // Only non-root nodes are removable branches.
        guard let target = node(id), target.parentID != nil else { return }
        // Tombstone the subtree root; the reducer cascades descendants + edges.
        let canonical = canonicalID(for: id)
        Task { await canonicalBackend.removeBranch(id: canonical) }
    }

    func startNewMap() {
        // A fresh solo map on the server; the backend re-points + reports it.
        openMapID = nil
        title = "Idea space"
        nodes = []
        edges = []
        activeNodeID = nil
        pendingActiveNodeID = nil
        pendingConnection = nil
        Task { @MainActor in
            if let newID = await canonicalBackend.createMap(title: "Idea space") {
                openMapID = CanonicalMapLibraryProjection.resolveUUID(newID)
                canonicalLocalPrefs.markOpened(newID)
            }
            await refreshLibrary()
        }
    }

    func openMap(_ id: UUID) {
        let mapID = canonicalMapID(for: id)
        openMapID = id
        pendingActiveNodeID = nil
        pendingConnection = nil
        canonicalLocalPrefs.markOpened(mapID)
        Task { @MainActor in
            await canonicalBackend.open(mapID: mapID)
            await refreshLibrary()
        }
    }

    func setPreferredMode(_ mode: ThinkingMapMode) {
        // preferredMode is CLIENT-LOCAL (no server metadata).
        guard let openMapID else { return }
        let mapID = canonicalMapID(for: openMapID)
        guard canonicalLocalPrefs.preferredMode(mapID) != mode else { return }
        canonicalLocalPrefs.setPreferredMode(mode, for: mapID)
        objectWillChange.send()
        Task { @MainActor in await refreshLibrary() }
    }

    func renameMap(_ id: UUID, to newTitle: String) {
        let cleaned = normalized(newTitle)
        guard !cleaned.isEmpty else { return }
        let finalTitle = concise(cleaned, limit: 60)
        let mapID = canonicalMapID(for: id)
        if openMapID == id { title = finalTitle }
        Task { @MainActor in
            await canonicalBackend.renameMap(id: mapID, title: finalTitle)
            await refreshLibrary()
        }
    }

    func togglePinned(_ id: UUID) {
        // pin is CLIENT-LOCAL (no server metadata).
        canonicalLocalPrefs.togglePinned(canonicalMapID(for: id))
        Task { @MainActor in await refreshLibrary() }
    }

    func setArchived(_ archived: Bool, for id: UUID) {
        // archive / restore(un-archive) = a server lifecycle patch.
        let mapID = canonicalMapID(for: id)
        Task { @MainActor in
            await canonicalBackend.setArchived(archived, id: mapID)
            await refreshLibrary()
        }
    }

    /// Duplicate (= restore-as-branch off the source's latest revision). The
    /// canonical create is ASYNC, so the copy's id arrives via `completion`
    /// (main actor; nil on failure) — the old synchronous return could never
    /// carry it, which left duplicated maps unopened.
    func duplicateMap(_ id: UUID, completion: @escaping (UUID?) -> Void = { _ in }) {
        guard let original = maps.first(where: { $0.id == id }) else {
            completion(nil)
            return
        }
        let mapID = canonicalMapID(for: id)
        let newTitle = concise("\(original.title) copy", limit: 60)
        Task { @MainActor in
            let revision = await latestRevision(for: id)
            let newID = await canonicalBackend.duplicateMap(
                sourceID: mapID, latestRevision: revision, newTitle: newTitle)
            await refreshLibrary()
            completion(newID.map(CanonicalMapLibraryProjection.resolveUUID))
        }
    }

    func deleteMap(_ id: UUID) {
        // Soft-delete: lifecycle = deleted (no hard-delete endpoint). Drop
        // this map's client-local prefs too.
        let mapID = canonicalMapID(for: id)
        if openMapID == id {
            openMapID = nil
            title = "Idea space"
            nodes = []
            edges = []
            activeNodeID = nil
            pendingActiveNodeID = nil
            pendingConnection = nil
        }
        canonicalLocalPrefs.forget(mapID)
        Task { @MainActor in
            await canonicalBackend.deleteMap(id: mapID)
            await refreshLibrary()
        }
    }

    /// Seed the interactive guided example onto the canonical backend: create a
    /// fresh map, then replay the example as owner thoughts (root → children),
    /// awaiting each add so parent ids resolve. The backend's `onMap` re-projects
    /// each step onto the published state.
    ///
    /// Returns the seeding task so a caller can sequence work AFTER the example
    /// lands (e.g. the demo launch path refreshes the AI frontier once the
    /// active node exists — E0's synchronous seed made that implicit).
    @discardableResult
    func loadExample() -> Task<Void, Never> {
        openMapID = nil
        title = "Idea space"
        nodes = []
        edges = []
        activeNodeID = nil
        pendingActiveNodeID = nil
        pendingConnection = nil
        return Task { @MainActor in
            guard let newID = await canonicalBackend.createMap(title: "A voice companion for ideas") else {
                await refreshLibrary()
                return
            }
            openMapID = CanonicalMapLibraryProjection.resolveUUID(newID)
            canonicalLocalPrefs.markOpened(newID)

            let rootID = await canonicalBackend.addThought(
                "A private voice companion for ideas", kind: .idea, activeID: nil)
            let audienceID = await canonicalBackend.addThought(
                "Who needs this most?", kind: .question, activeID: rootID)
            let flowID = await canonicalBackend.addThought(
                "The selected node becomes the next conversation", kind: .idea, activeID: rootID)
            let overloadID = await canonicalBackend.addThought(
                "The graph could become visually overwhelming", kind: .risk, activeID: rootID)
            _ = await canonicalBackend.addThought(
                "Keep one active branch in the foreground", kind: .decision, activeID: overloadID)
            _ = await canonicalBackend.addThought(
                "Test a five-turn branching session on iPhone", kind: .action, activeID: flowID)
            _ = await canonicalBackend.addThought(
                "What moment currently loses good ideas?", kind: .question, activeID: audienceID)

            await canonicalBackend.select(audienceID)
            await refreshLibrary()
        }
    }

    private func classify(_ text: String) -> ThinkingNodeKind {
        let lower = text.lowercased()
        if lower.contains("?") || ["what", "why", "how", "when", "who"].contains(where: { lower.hasPrefix($0 + " ") }) { return .question }
        if ["risk", "worry", "afraid", "concern", "fail", "danger", "block"].contains(where: lower.contains) { return .risk }
        if ["assume", "assuming", "presume", "suppose", "taken for granted"].contains(where: lower.contains) { return .assumption }
        if ["evidence", "shows that", "data shows", "proof", "study", "source"].contains(where: lower.contains) { return .evidence }
        if ["metric", "measure", "kpi", "target of", "percent", "rate of"].contains(where: lower.contains) { return .metric }
        if ["option", "alternative", "either", "or we could", "another way"].contains(where: lower.contains) { return .option }
        if ["fact", "in fact", "it is true", "actually", "the truth is"].contains(where: lower.contains) { return .fact }
        if ["decide", "decision", "choose", "we will", "i will", "commit"].contains(where: lower.contains) { return .decision }
        if ["next", "need to", "should", "build", "call", "send", "test", "create"].contains(where: lower.contains) { return .action }
        return .idea
    }

    private func normalized(_ text: String) -> String {
        text.split(whereSeparator: \Character.isWhitespace).joined(separator: " ")
    }

    private func concise(_ text: String, limit: Int = 76) -> String {
        let text = normalized(text)
        guard text.count > limit else { return text }
        let prefix = text.prefix(limit - 1)
        if let boundary = prefix.lastIndex(of: " ") { return String(prefix[..<boundary]) + "…" }
        return String(prefix) + "…"
    }

}
