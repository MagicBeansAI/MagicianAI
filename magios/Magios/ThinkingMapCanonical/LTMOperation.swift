//  LTMOperation.swift
//  Live Thinking Map (LTM) — the `MapOperation` analogue.
//
//  `MapOperation` is internally tagged on `"op"` (serde `#[serde(tag = "op")]`),
//  so the wire form is e.g. `{"op":"add_node","node":{...}}` — the discriminant
//  is a sibling key alongside the variant's inline fields. Swift has no built-in
//  internally-tagged enum, so this is a hand-written `Codable` covering EVERY
//  variant.
//
//  `UpdateNode.detail_markdown` is Rust `Option<Option<String>>`, modeled here
//  as `LTM.FieldEdit<String>` (absent → `.unchanged`, null → `.clear`,
//  value → `.set`). See `LTMTaggedTypes.swift`.

import Foundation

extension LTM {
    /// `MapOperation` — a single bounded change to an `LTM.Map`. Manual `Codable`
    /// keyed on the `"op"` discriminant. All 24 Rust variants are represented.
    public enum Operation: Codable, Equatable, Sendable {
        case addNode(node: Node)
        case updateNode(
            nodeId: String,
            label: String?,
            detailMarkdown: FieldEdit<String>,
            confidence: Float?
        )
        case setNodeKind(nodeId: String, kind: NodeKind)
        case setEpistemicState(nodeId: String, state: EpistemicState)
        case tombstoneNode(nodeId: String)
        case restoreNode(nodeId: String)
        case connect(edge: Edge)
        case disconnect(edgeId: String)
        case moveToParent(nodeId: String, parentId: String?)
        case moveNode(nodeId: String, position: Position)
        case setPositionLock(nodeId: String, locked: Bool)
        case createClarification(clarification: Clarification)
        case resolveClarification(
            clarificationId: String,
            state: ClarificationState,
            answer: String?
        )
        case proposeRestructure(proposal: RestructureProposal)
        case confirmRestructure(proposalId: String)
        case rejectRestructure(proposalId: String)
        case setSharedView(viewState: SharedViewState)
        case linkPromotedObject(nodeId: String, promoted: PromotedRef)
        case unlinkPromotedObject(
            nodeId: String,
            destinationKind: PromotionKind,
            objectId: String
        )
        case renameSpeaker(oldSpeakerId: String, newDisplayName: String)
        case setTitle(title: String)
        case setLifecycle(lifecycle: MapLifecycle)

        private enum CodingKeys: String, CodingKey {
            case op
            case node
            case nodeId = "node_id"
            case label
            case detailMarkdown = "detail_markdown"
            case confidence
            case kind
            case state
            case edge
            case edgeId = "edge_id"
            case parentId = "parent_id"
            case position
            case locked
            case clarification
            case clarificationId = "clarification_id"
            case answer
            case proposal
            case proposalId = "proposal_id"
            case viewState = "view_state"
            case promoted
            case destinationKind = "destination_kind"
            case objectId = "object_id"
            case oldSpeakerId = "old_speaker_id"
            case newDisplayName = "new_display_name"
            case title
            case lifecycle
        }

        // MARK: Decode

        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            let op = try c.decode(String.self, forKey: .op)
            switch op {
            case "add_node":
                self = .addNode(node: try c.decode(Node.self, forKey: .node))
            case "update_node":
                self = .updateNode(
                    nodeId: try c.decode(String.self, forKey: .nodeId),
                    label: try c.decodeIfPresent(String.self, forKey: .label),
                    detailMarkdown: try c.decodeFieldEdit(.detailMarkdown),
                    confidence: try c.decodeIfPresent(Float.self, forKey: .confidence)
                )
            case "set_node_kind":
                self = .setNodeKind(
                    nodeId: try c.decode(String.self, forKey: .nodeId),
                    kind: try c.decode(NodeKind.self, forKey: .kind)
                )
            case "set_epistemic_state":
                self = .setEpistemicState(
                    nodeId: try c.decode(String.self, forKey: .nodeId),
                    state: try c.decode(EpistemicState.self, forKey: .state)
                )
            case "tombstone_node":
                self = .tombstoneNode(nodeId: try c.decode(String.self, forKey: .nodeId))
            case "restore_node":
                self = .restoreNode(nodeId: try c.decode(String.self, forKey: .nodeId))
            case "connect":
                self = .connect(edge: try c.decode(Edge.self, forKey: .edge))
            case "disconnect":
                self = .disconnect(edgeId: try c.decode(String.self, forKey: .edgeId))
            case "move_to_parent":
                self = .moveToParent(
                    nodeId: try c.decode(String.self, forKey: .nodeId),
                    parentId: try c.decodeIfPresent(String.self, forKey: .parentId)
                )
            case "move_node":
                self = .moveNode(
                    nodeId: try c.decode(String.self, forKey: .nodeId),
                    position: try c.decode(Position.self, forKey: .position)
                )
            case "set_position_lock":
                self = .setPositionLock(
                    nodeId: try c.decode(String.self, forKey: .nodeId),
                    locked: try c.decode(Bool.self, forKey: .locked)
                )
            case "create_clarification":
                self = .createClarification(
                    clarification: try c.decode(Clarification.self, forKey: .clarification))
            case "resolve_clarification":
                self = .resolveClarification(
                    clarificationId: try c.decode(String.self, forKey: .clarificationId),
                    state: try c.decode(ClarificationState.self, forKey: .state),
                    answer: try c.decodeIfPresent(String.self, forKey: .answer)
                )
            case "propose_restructure":
                self = .proposeRestructure(
                    proposal: try c.decode(RestructureProposal.self, forKey: .proposal))
            case "confirm_restructure":
                self = .confirmRestructure(
                    proposalId: try c.decode(String.self, forKey: .proposalId))
            case "reject_restructure":
                self = .rejectRestructure(
                    proposalId: try c.decode(String.self, forKey: .proposalId))
            case "set_shared_view":
                self = .setSharedView(
                    viewState: try c.decode(SharedViewState.self, forKey: .viewState))
            case "link_promoted_object":
                self = .linkPromotedObject(
                    nodeId: try c.decode(String.self, forKey: .nodeId),
                    promoted: try c.decode(PromotedRef.self, forKey: .promoted)
                )
            case "unlink_promoted_object":
                self = .unlinkPromotedObject(
                    nodeId: try c.decode(String.self, forKey: .nodeId),
                    destinationKind: try c.decode(PromotionKind.self, forKey: .destinationKind),
                    objectId: try c.decode(String.self, forKey: .objectId)
                )
            case "rename_speaker":
                self = .renameSpeaker(
                    oldSpeakerId: try c.decode(String.self, forKey: .oldSpeakerId),
                    newDisplayName: try c.decode(String.self, forKey: .newDisplayName)
                )
            case "set_title":
                self = .setTitle(title: try c.decode(String.self, forKey: .title))
            case "set_lifecycle":
                self = .setLifecycle(
                    lifecycle: try c.decode(MapLifecycle.self, forKey: .lifecycle))
            default:
                throw DecodingError.dataCorruptedError(
                    forKey: .op, in: c,
                    debugDescription: "unknown MapOperation op: \(op)")
            }
        }

        // MARK: Encode

        public func encode(to encoder: Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            switch self {
            case let .addNode(node):
                try c.encode("add_node", forKey: .op)
                try c.encode(node, forKey: .node)
            case let .updateNode(nodeId, label, detailMarkdown, confidence):
                try c.encode("update_node", forKey: .op)
                try c.encode(nodeId, forKey: .nodeId)
                try c.encodeIfPresent(label, forKey: .label)
                try c.encodeFieldEdit(detailMarkdown, forKey: .detailMarkdown)
                try c.encodeIfPresent(confidence, forKey: .confidence)
            case let .setNodeKind(nodeId, kind):
                try c.encode("set_node_kind", forKey: .op)
                try c.encode(nodeId, forKey: .nodeId)
                try c.encode(kind, forKey: .kind)
            case let .setEpistemicState(nodeId, state):
                try c.encode("set_epistemic_state", forKey: .op)
                try c.encode(nodeId, forKey: .nodeId)
                try c.encode(state, forKey: .state)
            case let .tombstoneNode(nodeId):
                try c.encode("tombstone_node", forKey: .op)
                try c.encode(nodeId, forKey: .nodeId)
            case let .restoreNode(nodeId):
                try c.encode("restore_node", forKey: .op)
                try c.encode(nodeId, forKey: .nodeId)
            case let .connect(edge):
                try c.encode("connect", forKey: .op)
                try c.encode(edge, forKey: .edge)
            case let .disconnect(edgeId):
                try c.encode("disconnect", forKey: .op)
                try c.encode(edgeId, forKey: .edgeId)
            case let .moveToParent(nodeId, parentId):
                try c.encode("move_to_parent", forKey: .op)
                try c.encode(nodeId, forKey: .nodeId)
                try c.encodeIfPresent(parentId, forKey: .parentId)
            case let .moveNode(nodeId, position):
                try c.encode("move_node", forKey: .op)
                try c.encode(nodeId, forKey: .nodeId)
                try c.encode(position, forKey: .position)
            case let .setPositionLock(nodeId, locked):
                try c.encode("set_position_lock", forKey: .op)
                try c.encode(nodeId, forKey: .nodeId)
                try c.encode(locked, forKey: .locked)
            case let .createClarification(clarification):
                try c.encode("create_clarification", forKey: .op)
                try c.encode(clarification, forKey: .clarification)
            case let .resolveClarification(clarificationId, state, answer):
                try c.encode("resolve_clarification", forKey: .op)
                try c.encode(clarificationId, forKey: .clarificationId)
                try c.encode(state, forKey: .state)
                try c.encodeIfPresent(answer, forKey: .answer)
            case let .proposeRestructure(proposal):
                try c.encode("propose_restructure", forKey: .op)
                try c.encode(proposal, forKey: .proposal)
            case let .confirmRestructure(proposalId):
                try c.encode("confirm_restructure", forKey: .op)
                try c.encode(proposalId, forKey: .proposalId)
            case let .rejectRestructure(proposalId):
                try c.encode("reject_restructure", forKey: .op)
                try c.encode(proposalId, forKey: .proposalId)
            case let .setSharedView(viewState):
                try c.encode("set_shared_view", forKey: .op)
                try c.encode(viewState, forKey: .viewState)
            case let .linkPromotedObject(nodeId, promoted):
                try c.encode("link_promoted_object", forKey: .op)
                try c.encode(nodeId, forKey: .nodeId)
                try c.encode(promoted, forKey: .promoted)
            case let .unlinkPromotedObject(nodeId, destinationKind, objectId):
                try c.encode("unlink_promoted_object", forKey: .op)
                try c.encode(nodeId, forKey: .nodeId)
                try c.encode(destinationKind, forKey: .destinationKind)
                try c.encode(objectId, forKey: .objectId)
            case let .renameSpeaker(oldSpeakerId, newDisplayName):
                try c.encode("rename_speaker", forKey: .op)
                try c.encode(oldSpeakerId, forKey: .oldSpeakerId)
                try c.encode(newDisplayName, forKey: .newDisplayName)
            case let .setTitle(title):
                try c.encode("set_title", forKey: .op)
                try c.encode(title, forKey: .title)
            case let .setLifecycle(lifecycle):
                try c.encode("set_lifecycle", forKey: .op)
                try c.encode(lifecycle, forKey: .lifecycle)
            }
        }
    }
}
