//  LTMModels.swift
//  Live Thinking Map (LTM) — canonical graph structs + store/response types.
//
//  Mirrors `magician_v2::thinking_map::{models,operations,store}`. All struct
//  field names are snake_case on the wire (as written in Rust); we use explicit
//  `CodingKeys` (NOT `.convertFromSnakeCase`) so the manual enum `Codable`s and
//  the struct `Codable`s share ONE consistent, unambiguous key strategy.
//
//  Optional-with-skip fields (`skip_serializing_if = "Option::is_none"`) →
//  Swift `Optional` encoded with `encodeIfPresent` (absent when nil).
//  `#[serde(default)]`-only collection/bool fields (source_refs, promoted_refs,
//  position_locked, tombstoned, operations, affected_node_ids, applied_envelopes)
//  are ALWAYS present on the wire, so they are non-optional and always encoded.

import Foundation

extension LTM {
    // MARK: Supporting structs

    /// `SharedViewState`. `active_node` skips-if-none; `lens` has `#[serde(default)]`
    /// (always present on the wire).
    public struct SharedViewState: Codable, Equatable, Sendable {
        public var activeNode: String?
        public var lens: ViewLens

        public init(activeNode: String? = nil, lens: ViewLens = .graph) {
            self.activeNode = activeNode
            self.lens = lens
        }

        private enum CodingKeys: String, CodingKey {
            case activeNode = "active_node"
            case lens
        }

        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            activeNode = try c.decodeIfPresent(String.self, forKey: .activeNode)
            // `#[serde(default)]` → default to `.graph` if absent.
            lens = try c.decodeIfPresent(ViewLens.self, forKey: .lens) ?? .graph
        }

        public func encode(to encoder: Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            try c.encodeIfPresent(activeNode, forKey: .activeNode)
            try c.encode(lens, forKey: .lens)
        }
    }

    /// `SpeakerRef`. `display_name` skips-if-none.
    public struct SpeakerRef: Codable, Equatable, Sendable {
        public var speakerId: String
        public var displayName: String?

        public init(speakerId: String, displayName: String? = nil) {
            self.speakerId = speakerId
            self.displayName = displayName
        }

        private enum CodingKeys: String, CodingKey {
            case speakerId = "speaker_id"
            case displayName = "display_name"
        }

        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            speakerId = try c.decode(String.self, forKey: .speakerId)
            displayName = try c.decodeIfPresent(String.self, forKey: .displayName)
        }

        public func encode(to encoder: Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            try c.encode(speakerId, forKey: .speakerId)
            try c.encodeIfPresent(displayName, forKey: .displayName)
        }
    }

    /// `SourceRef`. Every field skips-if-none.
    public struct SourceRef: Codable, Equatable, Sendable {
        public var utteranceId: String?
        public var threadId: String?
        public var quote: String?
        public var timestamp: String?

        public init(
            utteranceId: String? = nil, threadId: String? = nil,
            quote: String? = nil, timestamp: String? = nil
        ) {
            self.utteranceId = utteranceId
            self.threadId = threadId
            self.quote = quote
            self.timestamp = timestamp
        }

        private enum CodingKeys: String, CodingKey {
            case utteranceId = "utterance_id"
            case threadId = "thread_id"
            case quote
            case timestamp
        }

        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            utteranceId = try c.decodeIfPresent(String.self, forKey: .utteranceId)
            threadId = try c.decodeIfPresent(String.self, forKey: .threadId)
            quote = try c.decodeIfPresent(String.self, forKey: .quote)
            timestamp = try c.decodeIfPresent(String.self, forKey: .timestamp)
        }

        public func encode(to encoder: Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            try c.encodeIfPresent(utteranceId, forKey: .utteranceId)
            try c.encodeIfPresent(threadId, forKey: .threadId)
            try c.encodeIfPresent(quote, forKey: .quote)
            try c.encodeIfPresent(timestamp, forKey: .timestamp)
        }
    }

    /// `Position`. `x`/`y` are `f64` → `Double`.
    public struct Position: Codable, Equatable, Sendable {
        public var x: Double
        public var y: Double

        public init(x: Double, y: Double) {
            self.x = x
            self.y = y
        }
    }

    /// `PromotedRef`.
    public struct PromotedRef: Codable, Equatable, Sendable {
        public var destinationKind: PromotionKind
        public var objectId: String
        public var linkedAt: String

        public init(destinationKind: PromotionKind, objectId: String, linkedAt: String) {
            self.destinationKind = destinationKind
            self.objectId = objectId
            self.linkedAt = linkedAt
        }

        private enum CodingKeys: String, CodingKey {
            case destinationKind = "destination_kind"
            case objectId = "object_id"
            case linkedAt = "linked_at"
        }
    }

    /// `Clarification`. `state` has `#[serde(default)]` (always on wire);
    /// `answer`/`resolved_at` skip-if-none.
    public struct Clarification: Codable, Equatable, Sendable {
        public var clarificationId: String
        public var nodeId: String
        public var question: String
        public var state: ClarificationState
        public var answer: String?
        public var createdAt: String
        public var resolvedAt: String?

        public init(
            clarificationId: String, nodeId: String, question: String,
            state: ClarificationState = .open, answer: String? = nil,
            createdAt: String, resolvedAt: String? = nil
        ) {
            self.clarificationId = clarificationId
            self.nodeId = nodeId
            self.question = question
            self.state = state
            self.answer = answer
            self.createdAt = createdAt
            self.resolvedAt = resolvedAt
        }

        private enum CodingKeys: String, CodingKey {
            case clarificationId = "clarification_id"
            case nodeId = "node_id"
            case question
            case state
            case answer
            case createdAt = "created_at"
            case resolvedAt = "resolved_at"
        }

        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            clarificationId = try c.decode(String.self, forKey: .clarificationId)
            nodeId = try c.decode(String.self, forKey: .nodeId)
            question = try c.decode(String.self, forKey: .question)
            state = try c.decodeIfPresent(ClarificationState.self, forKey: .state) ?? .open
            answer = try c.decodeIfPresent(String.self, forKey: .answer)
            createdAt = try c.decode(String.self, forKey: .createdAt)
            resolvedAt = try c.decodeIfPresent(String.self, forKey: .resolvedAt)
        }

        public func encode(to encoder: Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            try c.encode(clarificationId, forKey: .clarificationId)
            try c.encode(nodeId, forKey: .nodeId)
            try c.encode(question, forKey: .question)
            try c.encode(state, forKey: .state)
            try c.encodeIfPresent(answer, forKey: .answer)
            try c.encode(createdAt, forKey: .createdAt)
            try c.encodeIfPresent(resolvedAt, forKey: .resolvedAt)
        }
    }

    /// `AppliedEnvelopeRecord` — idempotency-ledger bookkeeping record.
    public struct AppliedEnvelopeRecord: Codable, Equatable, Sendable {
        public var envelopeId: String
        public var idempotencyKey: String
        public var resultingRevision: UInt64

        public init(envelopeId: String, idempotencyKey: String, resultingRevision: UInt64) {
            self.envelopeId = envelopeId
            self.idempotencyKey = idempotencyKey
            self.resultingRevision = resultingRevision
        }

        private enum CodingKeys: String, CodingKey {
            case envelopeId = "envelope_id"
            case idempotencyKey = "idempotency_key"
            case resultingRevision = "resulting_revision"
        }
    }

    /// `RestructureProposal`. `proposed_by` has a serde default; `operations`/
    /// `state`/`affected_node_ids` have `#[serde(default)]` (always on wire);
    /// `resolved_at` skips-if-none.
    public struct RestructureProposal: Codable, Equatable, Sendable {
        public var proposalId: String
        public var proposedBy: Actor
        public var rationale: String
        public var operations: [Operation]
        public var state: ProposalState
        public var affectedNodeIds: [String]
        public var createdAt: String
        public var resolvedAt: String?

        public init(
            proposalId: String, proposedBy: Actor, rationale: String,
            operations: [Operation] = [], state: ProposalState = .proposed,
            affectedNodeIds: [String] = [], createdAt: String, resolvedAt: String? = nil
        ) {
            self.proposalId = proposalId
            self.proposedBy = proposedBy
            self.rationale = rationale
            self.operations = operations
            self.state = state
            self.affectedNodeIds = affectedNodeIds
            self.createdAt = createdAt
            self.resolvedAt = resolvedAt
        }

        private enum CodingKeys: String, CodingKey {
            case proposalId = "proposal_id"
            case proposedBy = "proposed_by"
            case rationale
            case operations
            case state
            case affectedNodeIds = "affected_node_ids"
            case createdAt = "created_at"
            case resolvedAt = "resolved_at"
        }

        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            proposalId = try c.decode(String.self, forKey: .proposalId)
            proposedBy = try c.decode(Actor.self, forKey: .proposedBy)
            rationale = try c.decode(String.self, forKey: .rationale)
            operations = try c.decodeIfPresent([Operation].self, forKey: .operations) ?? []
            state = try c.decodeIfPresent(ProposalState.self, forKey: .state) ?? .proposed
            affectedNodeIds = try c.decodeIfPresent([String].self, forKey: .affectedNodeIds) ?? []
            createdAt = try c.decode(String.self, forKey: .createdAt)
            resolvedAt = try c.decodeIfPresent(String.self, forKey: .resolvedAt)
        }

        public func encode(to encoder: Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            try c.encode(proposalId, forKey: .proposalId)
            try c.encode(proposedBy, forKey: .proposedBy)
            try c.encode(rationale, forKey: .rationale)
            try c.encode(operations, forKey: .operations)
            try c.encode(state, forKey: .state)
            try c.encode(affectedNodeIds, forKey: .affectedNodeIds)
            try c.encode(createdAt, forKey: .createdAt)
            try c.encodeIfPresent(resolvedAt, forKey: .resolvedAt)
        }
    }

    // MARK: Core graph elements

    /// `ThinkingNode`. `confidence` is `f32` → `Float`. `detail_markdown`,
    /// `speaker`, `parent_id`, `position` skip-if-none. `source_refs`,
    /// `promoted_refs`, `position_locked`, `tombstoned` are always on the wire.
    public struct Node: Codable, Equatable, Sendable {
        public var nodeId: String
        public var kind: NodeKind
        public var label: String
        public var detailMarkdown: String?
        public var epistemicState: EpistemicState
        public var assertionOrigin: AssertionOrigin
        public var confidence: Float
        public var speaker: SpeakerRef?
        public var sourceRefs: [SourceRef]
        public var parentId: String?
        public var position: Position?
        public var positionLocked: Bool
        public var promotedRefs: [PromotedRef]
        public var tombstoned: Bool
        public var createdAt: String
        public var updatedAt: String

        public init(
            nodeId: String, kind: NodeKind, label: String, detailMarkdown: String? = nil,
            epistemicState: EpistemicState, assertionOrigin: AssertionOrigin, confidence: Float,
            speaker: SpeakerRef? = nil, sourceRefs: [SourceRef] = [], parentId: String? = nil,
            position: Position? = nil, positionLocked: Bool = false,
            promotedRefs: [PromotedRef] = [], tombstoned: Bool = false,
            createdAt: String, updatedAt: String
        ) {
            self.nodeId = nodeId
            self.kind = kind
            self.label = label
            self.detailMarkdown = detailMarkdown
            self.epistemicState = epistemicState
            self.assertionOrigin = assertionOrigin
            self.confidence = confidence
            self.speaker = speaker
            self.sourceRefs = sourceRefs
            self.parentId = parentId
            self.position = position
            self.positionLocked = positionLocked
            self.promotedRefs = promotedRefs
            self.tombstoned = tombstoned
            self.createdAt = createdAt
            self.updatedAt = updatedAt
        }

        private enum CodingKeys: String, CodingKey {
            case nodeId = "node_id"
            case kind
            case label
            case detailMarkdown = "detail_markdown"
            case epistemicState = "epistemic_state"
            case assertionOrigin = "assertion_origin"
            case confidence
            case speaker
            case sourceRefs = "source_refs"
            case parentId = "parent_id"
            case position
            case positionLocked = "position_locked"
            case promotedRefs = "promoted_refs"
            case tombstoned
            case createdAt = "created_at"
            case updatedAt = "updated_at"
        }

        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            nodeId = try c.decode(String.self, forKey: .nodeId)
            kind = try c.decode(NodeKind.self, forKey: .kind)
            label = try c.decode(String.self, forKey: .label)
            detailMarkdown = try c.decodeIfPresent(String.self, forKey: .detailMarkdown)
            epistemicState = try c.decode(EpistemicState.self, forKey: .epistemicState)
            assertionOrigin = try c.decode(AssertionOrigin.self, forKey: .assertionOrigin)
            confidence = try c.decode(Float.self, forKey: .confidence)
            speaker = try c.decodeIfPresent(SpeakerRef.self, forKey: .speaker)
            sourceRefs = try c.decodeIfPresent([SourceRef].self, forKey: .sourceRefs) ?? []
            parentId = try c.decodeIfPresent(String.self, forKey: .parentId)
            position = try c.decodeIfPresent(Position.self, forKey: .position)
            positionLocked = try c.decodeIfPresent(Bool.self, forKey: .positionLocked) ?? false
            promotedRefs = try c.decodeIfPresent([PromotedRef].self, forKey: .promotedRefs) ?? []
            tombstoned = try c.decodeIfPresent(Bool.self, forKey: .tombstoned) ?? false
            createdAt = try c.decode(String.self, forKey: .createdAt)
            updatedAt = try c.decode(String.self, forKey: .updatedAt)
        }

        public func encode(to encoder: Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            try c.encode(nodeId, forKey: .nodeId)
            try c.encode(kind, forKey: .kind)
            try c.encode(label, forKey: .label)
            try c.encodeIfPresent(detailMarkdown, forKey: .detailMarkdown)
            try c.encode(epistemicState, forKey: .epistemicState)
            try c.encode(assertionOrigin, forKey: .assertionOrigin)
            try c.encode(confidence, forKey: .confidence)
            try c.encodeIfPresent(speaker, forKey: .speaker)
            try c.encode(sourceRefs, forKey: .sourceRefs)
            try c.encodeIfPresent(parentId, forKey: .parentId)
            try c.encodeIfPresent(position, forKey: .position)
            try c.encode(positionLocked, forKey: .positionLocked)
            try c.encode(promotedRefs, forKey: .promotedRefs)
            try c.encode(tombstoned, forKey: .tombstoned)
            try c.encode(createdAt, forKey: .createdAt)
            try c.encode(updatedAt, forKey: .updatedAt)
        }
    }

    /// `ThinkingEdge`. `tombstoned` always on the wire (`#[serde(default)]`).
    public struct Edge: Codable, Equatable, Sendable {
        public var edgeId: String
        public var fromNode: String
        public var toNode: String
        public var kind: EdgeKind
        public var assertionOrigin: AssertionOrigin
        public var tombstoned: Bool
        public var createdAt: String
        public var updatedAt: String

        public init(
            edgeId: String, fromNode: String, toNode: String, kind: EdgeKind,
            assertionOrigin: AssertionOrigin, tombstoned: Bool = false,
            createdAt: String, updatedAt: String
        ) {
            self.edgeId = edgeId
            self.fromNode = fromNode
            self.toNode = toNode
            self.kind = kind
            self.assertionOrigin = assertionOrigin
            self.tombstoned = tombstoned
            self.createdAt = createdAt
            self.updatedAt = updatedAt
        }

        private enum CodingKeys: String, CodingKey {
            case edgeId = "edge_id"
            case fromNode = "from_node"
            case toNode = "to_node"
            case kind
            case assertionOrigin = "assertion_origin"
            case tombstoned
            case createdAt = "created_at"
            case updatedAt = "updated_at"
        }

        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            edgeId = try c.decode(String.self, forKey: .edgeId)
            fromNode = try c.decode(String.self, forKey: .fromNode)
            toNode = try c.decode(String.self, forKey: .toNode)
            kind = try c.decode(EdgeKind.self, forKey: .kind)
            assertionOrigin = try c.decode(AssertionOrigin.self, forKey: .assertionOrigin)
            tombstoned = try c.decodeIfPresent(Bool.self, forKey: .tombstoned) ?? false
            createdAt = try c.decode(String.self, forKey: .createdAt)
            updatedAt = try c.decode(String.self, forKey: .updatedAt)
        }

        public func encode(to encoder: Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            try c.encode(edgeId, forKey: .edgeId)
            try c.encode(fromNode, forKey: .fromNode)
            try c.encode(toNode, forKey: .toNode)
            try c.encode(kind, forKey: .kind)
            try c.encode(assertionOrigin, forKey: .assertionOrigin)
            try c.encode(tombstoned, forKey: .tombstoned)
            try c.encode(createdAt, forKey: .createdAt)
            try c.encode(updatedAt, forKey: .updatedAt)
        }
    }

    // MARK: The full map document

    /// `ThinkingMap`. `nodes`/`edges`/`clarifications`/`proposals` are Rust
    /// `BTreeMap`s → JSON objects keyed by id, decoded as `[String: T]`. All the
    /// map/collection fields carry `#[serde(default)]` (always present on wire).
    /// `applied_envelopes` (a `VecDeque`) is a JSON array.
    public struct Map: Codable, Equatable, Sendable {
        public var schemaVersion: UInt32
        public var mapId: String
        public var principal: String
        public var workspace: String
        public var title: String
        public var source: Source
        public var lifecycle: MapLifecycle
        public var revision: UInt64
        public var viewState: SharedViewState
        public var nodes: [String: Node]
        public var edges: [String: Edge]
        public var clarifications: [String: Clarification]
        public var proposals: [String: RestructureProposal]
        public var appliedEnvelopes: [AppliedEnvelopeRecord]
        public var createdAt: String
        public var updatedAt: String

        public init(
            schemaVersion: UInt32, mapId: String, principal: String, workspace: String,
            title: String, source: Source, lifecycle: MapLifecycle = .active, revision: UInt64,
            viewState: SharedViewState = SharedViewState(), nodes: [String: Node] = [:],
            edges: [String: Edge] = [:], clarifications: [String: Clarification] = [:],
            proposals: [String: RestructureProposal] = [:],
            appliedEnvelopes: [AppliedEnvelopeRecord] = [], createdAt: String, updatedAt: String
        ) {
            self.schemaVersion = schemaVersion
            self.mapId = mapId
            self.principal = principal
            self.workspace = workspace
            self.title = title
            self.source = source
            self.lifecycle = lifecycle
            self.revision = revision
            self.viewState = viewState
            self.nodes = nodes
            self.edges = edges
            self.clarifications = clarifications
            self.proposals = proposals
            self.appliedEnvelopes = appliedEnvelopes
            self.createdAt = createdAt
            self.updatedAt = updatedAt
        }

        private enum CodingKeys: String, CodingKey {
            case schemaVersion = "schema_version"
            case mapId = "map_id"
            case principal
            case workspace
            case title
            case source
            case lifecycle
            case revision
            case viewState = "view_state"
            case nodes
            case edges
            case clarifications
            case proposals
            case appliedEnvelopes = "applied_envelopes"
            case createdAt = "created_at"
            case updatedAt = "updated_at"
        }

        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            schemaVersion = try c.decode(UInt32.self, forKey: .schemaVersion)
            mapId = try c.decode(String.self, forKey: .mapId)
            principal = try c.decode(String.self, forKey: .principal)
            workspace = try c.decode(String.self, forKey: .workspace)
            title = try c.decode(String.self, forKey: .title)
            source = try c.decode(Source.self, forKey: .source)
            lifecycle = try c.decodeIfPresent(MapLifecycle.self, forKey: .lifecycle) ?? .active
            revision = try c.decode(UInt64.self, forKey: .revision)
            viewState = try c.decodeIfPresent(SharedViewState.self, forKey: .viewState)
                ?? SharedViewState()
            nodes = try c.decodeIfPresent([String: Node].self, forKey: .nodes) ?? [:]
            edges = try c.decodeIfPresent([String: Edge].self, forKey: .edges) ?? [:]
            clarifications = try c.decodeIfPresent([String: Clarification].self, forKey: .clarifications) ?? [:]
            proposals = try c.decodeIfPresent([String: RestructureProposal].self, forKey: .proposals) ?? [:]
            appliedEnvelopes = try c.decodeIfPresent([AppliedEnvelopeRecord].self, forKey: .appliedEnvelopes) ?? []
            createdAt = try c.decode(String.self, forKey: .createdAt)
            updatedAt = try c.decode(String.self, forKey: .updatedAt)
        }

        public func encode(to encoder: Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            try c.encode(schemaVersion, forKey: .schemaVersion)
            try c.encode(mapId, forKey: .mapId)
            try c.encode(principal, forKey: .principal)
            try c.encode(workspace, forKey: .workspace)
            try c.encode(title, forKey: .title)
            try c.encode(source, forKey: .source)
            try c.encode(lifecycle, forKey: .lifecycle)
            try c.encode(revision, forKey: .revision)
            try c.encode(viewState, forKey: .viewState)
            try c.encode(nodes, forKey: .nodes)
            try c.encode(edges, forKey: .edges)
            try c.encode(clarifications, forKey: .clarifications)
            try c.encode(proposals, forKey: .proposals)
            try c.encode(appliedEnvelopes, forKey: .appliedEnvelopes)
            try c.encode(createdAt, forKey: .createdAt)
            try c.encode(updatedAt, forKey: .updatedAt)
        }
    }

    // MARK: Envelope + store types

    /// `ModelTraceRef`. `model_profile` skips-if-none.
    public struct ModelTraceRef: Codable, Equatable, Sendable {
        public var traceId: String
        public var modelProfile: String?

        public init(traceId: String, modelProfile: String? = nil) {
            self.traceId = traceId
            self.modelProfile = modelProfile
        }

        private enum CodingKeys: String, CodingKey {
            case traceId = "trace_id"
            case modelProfile = "model_profile"
        }

        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            traceId = try c.decode(String.self, forKey: .traceId)
            modelProfile = try c.decodeIfPresent(String.self, forKey: .modelProfile)
        }

        public func encode(to encoder: Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            try c.encode(traceId, forKey: .traceId)
            try c.encodeIfPresent(modelProfile, forKey: .modelProfile)
        }
    }

    /// `MapOperationEnvelope`. `utterance_id`/`model_trace` skip-if-none;
    /// `operations` (`#[serde(default)]`) is always on the wire.
    public struct OperationEnvelope: Codable, Equatable, Sendable {
        public var schemaVersion: UInt32
        public var envelopeId: String
        public var mapId: String
        public var baseRevision: UInt64
        public var utteranceId: String?
        public var actor: Actor
        public var idempotencyKey: String
        public var operations: [Operation]
        public var modelTrace: ModelTraceRef?
        public var createdAt: String

        public init(
            schemaVersion: UInt32, envelopeId: String, mapId: String, baseRevision: UInt64,
            utteranceId: String? = nil, actor: Actor, idempotencyKey: String,
            operations: [Operation] = [], modelTrace: ModelTraceRef? = nil, createdAt: String
        ) {
            self.schemaVersion = schemaVersion
            self.envelopeId = envelopeId
            self.mapId = mapId
            self.baseRevision = baseRevision
            self.utteranceId = utteranceId
            self.actor = actor
            self.idempotencyKey = idempotencyKey
            self.operations = operations
            self.modelTrace = modelTrace
            self.createdAt = createdAt
        }

        private enum CodingKeys: String, CodingKey {
            case schemaVersion = "schema_version"
            case envelopeId = "envelope_id"
            case mapId = "map_id"
            case baseRevision = "base_revision"
            case utteranceId = "utterance_id"
            case actor
            case idempotencyKey = "idempotency_key"
            case operations
            case modelTrace = "model_trace"
            case createdAt = "created_at"
        }

        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            schemaVersion = try c.decode(UInt32.self, forKey: .schemaVersion)
            envelopeId = try c.decode(String.self, forKey: .envelopeId)
            mapId = try c.decode(String.self, forKey: .mapId)
            baseRevision = try c.decode(UInt64.self, forKey: .baseRevision)
            utteranceId = try c.decodeIfPresent(String.self, forKey: .utteranceId)
            actor = try c.decode(Actor.self, forKey: .actor)
            idempotencyKey = try c.decode(String.self, forKey: .idempotencyKey)
            operations = try c.decodeIfPresent([Operation].self, forKey: .operations) ?? []
            modelTrace = try c.decodeIfPresent(ModelTraceRef.self, forKey: .modelTrace)
            createdAt = try c.decode(String.self, forKey: .createdAt)
        }

        public func encode(to encoder: Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            try c.encode(schemaVersion, forKey: .schemaVersion)
            try c.encode(envelopeId, forKey: .envelopeId)
            try c.encode(mapId, forKey: .mapId)
            try c.encode(baseRevision, forKey: .baseRevision)
            try c.encodeIfPresent(utteranceId, forKey: .utteranceId)
            try c.encode(actor, forKey: .actor)
            try c.encode(idempotencyKey, forKey: .idempotencyKey)
            try c.encode(operations, forKey: .operations)
            try c.encodeIfPresent(modelTrace, forKey: .modelTrace)
            try c.encode(createdAt, forKey: .createdAt)
        }
    }

    /// One node in a [`NodePreview`] — the stripped-down projection the library
    /// card mini-graph needs (identity, tree link, kind, a title snippet, and the
    /// model-suggested flag). Mirrors the Rust `NodePreviewNode` wire contract.
    public struct NodePreviewNode: Codable, Equatable, Sendable {
        public var nodeId: String
        /// Present only when the parent is ALSO in the preview set.
        public var parentId: String?
        public var kind: NodeKind
        /// True for model-inferred / provisional nodes.
        public var suggested: Bool
        public var title: String

        public init(
            nodeId: String, parentId: String?, kind: NodeKind,
            suggested: Bool, title: String
        ) {
            self.nodeId = nodeId
            self.parentId = parentId
            self.kind = kind
            self.suggested = suggested
            self.title = title
        }

        private enum CodingKeys: String, CodingKey {
            case nodeId = "node_id"
            case parentId = "parent_id"
            case kind
            case suggested
            case title
        }
    }

    /// One parent→child branch edge in a [`NodePreview`]. Mirrors the Rust
    /// `NodePreviewEdge` wire contract.
    public struct NodePreviewEdge: Codable, Equatable, Sendable {
        public var from: String
        public var to: String

        public init(from: String, to: String) {
            self.from = from
            self.to = to
        }
    }

    /// A bounded per-map node preview for the library-card mini-graph: the first
    /// few live nodes + their branch edges. NOT the full graph. Mirrors the Rust
    /// `NodePreview` wire contract.
    public struct NodePreview: Codable, Equatable, Sendable {
        public var nodes: [NodePreviewNode]
        public var edges: [NodePreviewEdge]

        public init(nodes: [NodePreviewNode], edges: [NodePreviewEdge]) {
            self.nodes = nodes
            self.edges = edges
        }
    }

    /// `MapSummary` — lightweight list item. Core metadata is always present;
    /// `nodePreview` is OPTIONAL (older servers omit it, empty maps omit it).
    public struct Summary: Codable, Equatable, Sendable {
        public var mapId: String
        public var title: String
        public var lifecycle: MapLifecycle
        public var latestRevision: UInt64
        public var updatedAt: String
        /// Bounded node preview for the library-card mini-graph. `nil` when the
        /// server omits it (older server / empty map) — decoding is
        /// back-compatible (`decodeIfPresent`).
        public var nodePreview: NodePreview?

        public init(
            mapId: String, title: String, lifecycle: MapLifecycle,
            latestRevision: UInt64, updatedAt: String,
            nodePreview: NodePreview? = nil
        ) {
            self.mapId = mapId
            self.title = title
            self.lifecycle = lifecycle
            self.latestRevision = latestRevision
            self.updatedAt = updatedAt
            self.nodePreview = nodePreview
        }

        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            mapId = try c.decode(String.self, forKey: .mapId)
            title = try c.decode(String.self, forKey: .title)
            lifecycle = try c.decode(MapLifecycle.self, forKey: .lifecycle)
            latestRevision = try c.decode(UInt64.self, forKey: .latestRevision)
            updatedAt = try c.decode(String.self, forKey: .updatedAt)
            // Optional + back-compatible: absent field ⇒ nil (old responses).
            nodePreview = try c.decodeIfPresent(NodePreview.self, forKey: .nodePreview)
        }

        public func encode(to encoder: Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            try c.encode(mapId, forKey: .mapId)
            try c.encode(title, forKey: .title)
            try c.encode(lifecycle, forKey: .lifecycle)
            try c.encode(latestRevision, forKey: .latestRevision)
            try c.encode(updatedAt, forKey: .updatedAt)
            // Omit entirely when nil so the round-trip matches the Rust
            // `skip_serializing_if = "Option::is_none"` shape.
            try c.encodeIfPresent(nodePreview, forKey: .nodePreview)
        }

        private enum CodingKeys: String, CodingKey {
            case mapId = "map_id"
            case title
            case lifecycle
            case latestRevision = "latest_revision"
            case updatedAt = "updated_at"
            case nodePreview = "node_preview"
        }
    }

    /// One page of `GET /thinking-maps?limit=&offset=` — `total` is the
    /// scope-wide count, so `offset + maps.count < total` ⇒ more pages exist.
    public struct SummaryPage: Codable, Equatable, Sendable {
        public var maps: [Summary]
        public var total: Int
        public var offset: Int
        public var limit: Int

        public init(maps: [Summary], total: Int, offset: Int, limit: Int) {
            self.maps = maps
            self.total = total
            self.offset = offset
            self.limit = limit
        }
    }

    /// `MapEvent` — one applied envelope in the event log.
    public struct Event: Codable, Equatable, Sendable {
        public var sequence: UInt64
        public var envelope: OperationEnvelope
        public var resultingRevision: UInt64
        public var semanticHash: String
        public var appliedAt: String

        public init(
            sequence: UInt64, envelope: OperationEnvelope, resultingRevision: UInt64,
            semanticHash: String, appliedAt: String
        ) {
            self.sequence = sequence
            self.envelope = envelope
            self.resultingRevision = resultingRevision
            self.semanticHash = semanticHash
            self.appliedAt = appliedAt
        }

        private enum CodingKeys: String, CodingKey {
            case sequence
            case envelope
            case resultingRevision = "resulting_revision"
            case semanticHash = "semantic_hash"
            case appliedAt = "applied_at"
        }
    }

    /// `MapManifest` — the per-map head record. `branched_from_*` skip-if-none.
    public struct Manifest: Codable, Equatable, Sendable {
        public var schemaVersion: UInt32
        public var mapId: String
        public var principal: String
        public var workspace: String
        public var title: String
        public var source: Source
        public var lifecycle: MapLifecycle
        public var latestRevision: UInt64
        public var latestSequence: UInt64
        public var latestSemanticHash: String
        public var createdAt: String
        public var updatedAt: String
        public var branchedFromMapId: String?
        public var branchedFromSequence: UInt64?

        public init(
            schemaVersion: UInt32, mapId: String, principal: String, workspace: String,
            title: String, source: Source, lifecycle: MapLifecycle, latestRevision: UInt64,
            latestSequence: UInt64, latestSemanticHash: String, createdAt: String,
            updatedAt: String, branchedFromMapId: String? = nil,
            branchedFromSequence: UInt64? = nil
        ) {
            self.schemaVersion = schemaVersion
            self.mapId = mapId
            self.principal = principal
            self.workspace = workspace
            self.title = title
            self.source = source
            self.lifecycle = lifecycle
            self.latestRevision = latestRevision
            self.latestSequence = latestSequence
            self.latestSemanticHash = latestSemanticHash
            self.createdAt = createdAt
            self.updatedAt = updatedAt
            self.branchedFromMapId = branchedFromMapId
            self.branchedFromSequence = branchedFromSequence
        }

        private enum CodingKeys: String, CodingKey {
            case schemaVersion = "schema_version"
            case mapId = "map_id"
            case principal
            case workspace
            case title
            case source
            case lifecycle
            case latestRevision = "latest_revision"
            case latestSequence = "latest_sequence"
            case latestSemanticHash = "latest_semantic_hash"
            case createdAt = "created_at"
            case updatedAt = "updated_at"
            case branchedFromMapId = "branched_from_map_id"
            case branchedFromSequence = "branched_from_sequence"
        }

        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            schemaVersion = try c.decode(UInt32.self, forKey: .schemaVersion)
            mapId = try c.decode(String.self, forKey: .mapId)
            principal = try c.decode(String.self, forKey: .principal)
            workspace = try c.decode(String.self, forKey: .workspace)
            title = try c.decode(String.self, forKey: .title)
            source = try c.decode(Source.self, forKey: .source)
            lifecycle = try c.decode(MapLifecycle.self, forKey: .lifecycle)
            latestRevision = try c.decode(UInt64.self, forKey: .latestRevision)
            latestSequence = try c.decode(UInt64.self, forKey: .latestSequence)
            latestSemanticHash = try c.decode(String.self, forKey: .latestSemanticHash)
            createdAt = try c.decode(String.self, forKey: .createdAt)
            updatedAt = try c.decode(String.self, forKey: .updatedAt)
            branchedFromMapId = try c.decodeIfPresent(String.self, forKey: .branchedFromMapId)
            branchedFromSequence = try c.decodeIfPresent(UInt64.self, forKey: .branchedFromSequence)
        }

        public func encode(to encoder: Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            try c.encode(schemaVersion, forKey: .schemaVersion)
            try c.encode(mapId, forKey: .mapId)
            try c.encode(principal, forKey: .principal)
            try c.encode(workspace, forKey: .workspace)
            try c.encode(title, forKey: .title)
            try c.encode(source, forKey: .source)
            try c.encode(lifecycle, forKey: .lifecycle)
            try c.encode(latestRevision, forKey: .latestRevision)
            try c.encode(latestSequence, forKey: .latestSequence)
            try c.encode(latestSemanticHash, forKey: .latestSemanticHash)
            try c.encode(createdAt, forKey: .createdAt)
            try c.encode(updatedAt, forKey: .updatedAt)
            try c.encodeIfPresent(branchedFromMapId, forKey: .branchedFromMapId)
            try c.encodeIfPresent(branchedFromSequence, forKey: .branchedFromSequence)
        }
    }
}
