import Foundation
import Combine
import WidgetKit

enum JSONValue: Codable, Equatable {
    case string(String), number(Double), bool(Bool), object([String: JSONValue]), array([JSONValue]), null

    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if container.decodeNil() { self = .null }
        else if let value = try? container.decode(Bool.self) { self = .bool(value) }
        else if let value = try? container.decode(Double.self) { self = .number(value) }
        else if let value = try? container.decode(String.self) { self = .string(value) }
        else if let value = try? container.decode([String: JSONValue].self) { self = .object(value) }
        else if let value = try? container.decode([JSONValue].self) { self = .array(value) }
        else { self = .null }
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case .string(let value): try container.encode(value)
        case .number(let value): try container.encode(value)
        case .bool(let value): try container.encode(value)
        case .object(let value): try container.encode(value)
        case .array(let value): try container.encode(value)
        case .null: try container.encodeNil()
        }
    }

    var stringValue: String? {
        switch self {
        case .string(let value): return value
        case .number(let value): return String(value)
        case .bool(let value): return String(value)
        default: return nil
        }
    }
    var objectValue: [String: JSONValue]? { if case .object(let value) = self { return value }; return nil }
    var arrayValue: [JSONValue]? { if case .array(let value) = self { return value }; return nil }
}

struct TodayAction: Codable, Identifiable, Equatable {
    let id: String
    let label: String
    let actionType: String?
    let payload: JSONValue
    enum CodingKeys: String, CodingKey { case id, label, payload; case actionType = "action_type" }

    var executionEndpoint: String? {
        guard let record = payload.objectValue,
              (record["method"]?.stringValue ?? "POST").uppercased() == "POST",
              let endpoint = record["endpoint"]?.stringValue?.trimmingCharacters(in: .whitespacesAndNewlines),
              endpoint.hasPrefix("/api/magician/v2/today/items/") else { return nil }
        return endpoint
    }

    var systemImage: String {
        if payload.objectValue?["icon"]?.stringValue == "checklist" || id == "create_task" { return "checklist" }
        return "bolt.fill"
    }
}

private struct TodayActionTaskManifest: Decodable { let taskID: String?; enum CodingKeys: String, CodingKey { case taskID = "task_id" } }
private struct TodayActionTaskRecord: Decodable { let manifest: TodayActionTaskManifest? }
private struct TodayActionNavigation: Decodable { let taskID: String?; enum CodingKeys: String, CodingKey { case taskID = "task_id" } }
private struct TodayActionExecutionResult: Decodable {
    let taskID: String?
    let task: TodayActionTaskRecord?
    let navigateTo: TodayActionNavigation?
    enum CodingKeys: String, CodingKey { case task, taskID = "task_id", navigateTo = "navigate_to" }
    var resolvedTaskID: String? { navigateTo?.taskID ?? taskID ?? task?.manifest?.taskID }
}

struct TodayLearnedItem: Identifiable, Equatable {
    let id: String
    let title: String
    let summary: String?
    let updatedAt: Int64?
}

enum TodaySection: String, CaseIterable, Codable, Identifiable {
    case needsYou = "needs_you"
    case followups
    case worthALook = "worth_a_look"
    case activeWork = "active_work"
    case delivered
    case changed

    var id: String { rawValue }

    var title: String {
        switch self {
        case .needsYou: return "Needs You"
        case .followups: return "Follow-ups"
        case .worthALook: return "Worth a look"
        case .activeWork: return "Active Work"
        case .delivered: return "Delivered"
        case .changed: return "Changed"
        }
    }

    var summary: String {
        switch self {
        case .needsYou: return "Decisions, approvals, failures, and blocked work."
        case .followups: return "Due, blocked, or stale work that needs a next action."
        case .worthALook: return "Memory, tasks, and messages that may be useful again."
        case .activeWork: return "Current running work, shown as status instead of logs."
        case .delivered: return "Finished work with something useful to open."
        case .changed: return "Durable memory and state changes worth knowing."
        }
    }

    var systemImage: String {
        switch self {
        case .needsYou: return "exclamationmark.triangle.fill"
        case .followups: return "arrow.turn.down.right"
        case .worthALook: return "sparkles"
        case .activeWork: return "bolt.fill"
        case .delivered: return "checkmark.circle.fill"
        case .changed: return "info.circle.fill"
        }
    }
}

struct TodayItem: Codable, Identifiable, Equatable {
    let id: String
    let section: String
    let priority: Int
    let title: String
    let summary: String?
    let reason: String
    let sourceKind: String
    let sourceID: String
    let sourceURL: String?
    let spaceIDs: [String]
    let threadID: String?
    let taskID: String?
    let agentID: String?
    let status: String
    let actions: [TodayAction]
    let evidenceRefs: [JSONValue]
    let metadata: JSONValue
    let createdAt: Int64
    let updatedAt: Int64
    let expiresAt: Int64?
    let seenAt: Int64?
    let dismissedAt: Int64?
    let snoozedUntil: Int64?

    enum CodingKeys: String, CodingKey {
        case id, section, priority, title, summary, reason, status, actions, metadata
        case evidenceRefs = "evidence_refs"
        case sourceKind = "source_kind"
        case sourceID = "source_id"
        case sourceURL = "source_url"
        case spaceIDs = "space_ids"
        case threadID = "thread_id"
        case taskID = "task_id"
        case agentID = "agent_id"
        case createdAt = "created_at"
        case updatedAt = "updated_at"
        case expiresAt = "expires_at"
        case seenAt = "seen_at"
        case dismissedAt = "dismissed_at"
        case snoozedUntil = "snoozed_until"
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = (try? c.decode(String.self, forKey: .id)) ?? UUID().uuidString
        section = (try? c.decode(String.self, forKey: .section)) ?? "changed"
        priority = (try? c.decode(Int.self, forKey: .priority)) ?? 0
        title = (try? c.decode(String.self, forKey: .title)) ?? "Today item"
        summary = try? c.decodeIfPresent(String.self, forKey: .summary)
        reason = (try? c.decode(String.self, forKey: .reason)) ?? ""
        sourceKind = (try? c.decode(String.self, forKey: .sourceKind)) ?? "unknown"
        sourceID = (try? c.decode(String.self, forKey: .sourceID)) ?? ""
        sourceURL = try? c.decodeIfPresent(String.self, forKey: .sourceURL)
        spaceIDs = (try? c.decode([String].self, forKey: .spaceIDs)) ?? []
        threadID = try? c.decodeIfPresent(String.self, forKey: .threadID)
        taskID = try? c.decodeIfPresent(String.self, forKey: .taskID)
        agentID = try? c.decodeIfPresent(String.self, forKey: .agentID)
        status = (try? c.decode(String.self, forKey: .status)) ?? "info"
        actions = (try? c.decode([TodayAction].self, forKey: .actions)) ?? []
        evidenceRefs = (try? c.decode([JSONValue].self, forKey: .evidenceRefs)) ?? []
        metadata = (try? c.decode(JSONValue.self, forKey: .metadata)) ?? .null
        createdAt = (try? c.decode(Int64.self, forKey: .createdAt)) ?? 0
        updatedAt = (try? c.decode(Int64.self, forKey: .updatedAt)) ?? 0
        expiresAt = try? c.decodeIfPresent(Int64.self, forKey: .expiresAt)
        seenAt = try? c.decodeIfPresent(Int64.self, forKey: .seenAt)
        dismissedAt = try? c.decodeIfPresent(Int64.self, forKey: .dismissedAt)
        snoozedUntil = try? c.decodeIfPresent(Int64.self, forKey: .snoozedUntil)
    }

    init(id: String, section: String, priority: Int, title: String, summary: String?, reason: String,
         sourceKind: String, sourceID: String, sourceURL: String?, spaceIDs: [String], threadID: String?,
         taskID: String?, agentID: String?, status: String, createdAt: Int64, updatedAt: Int64) {
        self.id = id; self.section = section; self.priority = priority; self.title = title
        self.summary = summary; self.reason = reason; self.sourceKind = sourceKind; self.sourceID = sourceID
        self.sourceURL = sourceURL; self.spaceIDs = spaceIDs; self.threadID = threadID; self.taskID = taskID
        self.agentID = agentID; self.status = status; self.createdAt = createdAt; self.updatedAt = updatedAt
        self.actions = []; self.evidenceRefs = []; self.metadata = .null
        self.expiresAt = nil; self.seenAt = nil; self.dismissedAt = nil; self.snoozedUntil = nil
    }


    var learnedItems: [TodayLearnedItem] {
        guard let values = metadata.objectValue?["learned_items"]?.arrayValue else { return [] }
        return values.enumerated().compactMap { index, value in
            guard let record = value.objectValue else { return nil }
            let title = record["title"]?.stringValue?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
            let summaryValue = record["summary"]?.stringValue?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
            guard !title.isEmpty || !summaryValue.isEmpty else { return nil }
            let rawUpdated: Int64?
            if case .number(let value) = record["updated_at"] { rawUpdated = Int64(value) } else { rawUpdated = nil }
            return TodayLearnedItem(id: record["id"]?.stringValue ?? "\(id):\(index)",
                                    title: title.isEmpty ? summaryValue : title,
                                    summary: summaryValue.isEmpty || summaryValue == title ? nil : summaryValue,
                                    updatedAt: rawUpdated)
        }.prefix(4).map { $0 }
    }

    var attentionItemID: String? {
        if let sourceURL, let components = URLComponents(string: sourceURL),
           let value = components.queryItems?.first(where: { ["selected", "selected_item", "item_id"].contains($0.name) })?.value,
           !value.isEmpty { return value }
        let keys = ["pause_state_id", "approval_id", "correlation_id", "request_id", "feed_item_id", "dedupe_key", "dedupe_id", "dedupe"]
        if let record = metadata.objectValue {
            for key in keys where !(record[key]?.stringValue ?? "").isEmpty { return record[key]?.stringValue }
            if let identifiers = record["hitl_request"]?.objectValue?["identifiers"]?.objectValue {
                for key in keys where !(identifiers[key]?.stringValue ?? "").isEmpty { return identifiers[key]?.stringValue }
            }
        }
        let prefix = "today:\(section):"
        if id.hasPrefix(prefix) { return String(id.dropFirst(prefix.count)) }
        if ["v3:attention:", "skill_evolution_approval:", "skill_evolution_rollback:", "skill_evolution_post_promotion:"].contains(where: sourceID.hasPrefix) { return sourceID }
        return nil
    }
}

struct TodayCounts: Codable, Equatable {
    var needsYou = 0
    var delivered = 0
    var changed = 0
    var activeWork = 0
    var followups = 0
    var total = 0

    enum CodingKeys: String, CodingKey {
        case needsYou = "needs_you", delivered, changed, followups, total
        case activeWork = "active_work"
    }

    func count(for section: TodaySection, resurfacing: Int = 0) -> Int {
        switch section {
        case .needsYou: return needsYou
        case .followups: return followups
        case .worthALook: return resurfacing
        case .activeWork: return activeWork
        case .delivered: return delivered
        case .changed: return changed
        }
    }
}

struct TodaySectionsPayload: Codable, Equatable {
    var needsYou: [TodayItem] = []
    var delivered: [TodayItem] = []
    var changed: [TodayItem] = []
    var activeWork: [TodayItem] = []
    var followups: [TodayItem] = []

    enum CodingKeys: String, CodingKey {
        case needsYou = "needs_you", delivered, changed, followups
        case activeWork = "active_work"
    }

    func items(for section: TodaySection) -> [TodayItem] {
        switch section {
        case .needsYou: return needsYou
        case .followups: return followups
        case .activeWork: return activeWork
        case .delivered: return delivered
        case .changed: return changed
        case .worthALook: return []
        }
    }
}

struct TodayDigestBullet: Codable, Identifiable, Equatable {
    let id: String
    let text: String
    let sourceKind: String
    let sourceID: String
    let sourceURL: String?
    let spaceIDs: [String]
    let updatedAt: Int64

    enum CodingKeys: String, CodingKey {
        case id, text
        case sourceKind = "source_kind"
        case sourceID = "source_id"
        case sourceURL = "source_url"
        case spaceIDs = "space_ids"
        case updatedAt = "updated_at"
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = (try? c.decode(String.self, forKey: .id)) ?? UUID().uuidString
        text = (try? c.decode(String.self, forKey: .text)) ?? ""
        sourceKind = (try? c.decode(String.self, forKey: .sourceKind)) ?? "unknown"
        sourceID = (try? c.decode(String.self, forKey: .sourceID)) ?? ""
        sourceURL = try? c.decodeIfPresent(String.self, forKey: .sourceURL)
        spaceIDs = (try? c.decode([String].self, forKey: .spaceIDs)) ?? []
        updatedAt = (try? c.decode(Int64.self, forKey: .updatedAt)) ?? 0
    }
}

struct TodayDigest: Codable, Equatable {
    var generatedAt: Int64 = 0
    var since: Int64?
    var total: Int = 0
    var limit: Int = 0
    var offset: Int = 0
    var bullets: [TodayDigestBullet] = []
    enum CodingKeys: String, CodingKey {
        case total, since, limit, offset, bullets
        case generatedAt = "generated_at"
    }

    init(generatedAt: Int64 = 0, since: Int64? = nil, total: Int = 0,
         limit: Int = 0, offset: Int = 0, bullets: [TodayDigestBullet] = []) {
        self.generatedAt = generatedAt; self.since = since; self.total = total
        self.limit = limit; self.offset = offset; self.bullets = bullets
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        generatedAt = (try? c.decode(Int64.self, forKey: .generatedAt)) ?? 0
        since = try? c.decodeIfPresent(Int64.self, forKey: .since)
        total = (try? c.decode(Int.self, forKey: .total)) ?? 0
        limit = (try? c.decode(Int.self, forKey: .limit)) ?? 0
        offset = (try? c.decode(Int.self, forKey: .offset)) ?? 0
        bullets = (try? c.decode([TodayDigestBullet].self, forKey: .bullets)) ?? []
    }
}

struct TodaySectionPage: Codable, Equatable {
    let section: String
    let total: Int
    let limit: Int
    let cursor: String?
    let nextCursor: String?
    let hasMore: Bool
    enum CodingKeys: String, CodingKey {
        case section, total, limit, cursor
        case nextCursor = "next_cursor"
        case hasMore = "has_more"
    }
}

struct TodayResponse: Codable, Equatable {
    let generatedAt: Int64
    let headline: String
    let digest: TodayDigest
    let sections: TodaySectionsPayload
    let counts: TodayCounts
    let sectionPage: TodaySectionPage?

    enum CodingKeys: String, CodingKey {
        case headline, digest, sections, counts
        case sectionPage = "section_page"
        case generatedAt = "generated_at"
    }
}

struct TodayVisibilitySnapshot: Codable, Equatable {
    let title: String
    let summary: String?
    let reason: String
    let section: String
    let sourceKind: String
    let sourceID: String
    let sourceURL: String?
    let spaceIDs: [String]
    let itemUpdatedAt: Int64

    enum CodingKeys: String, CodingKey {
        case title, summary, reason, section
        case sourceKind = "source_kind"
        case sourceID = "source_id"
        case sourceURL = "source_url"
        case spaceIDs = "space_ids"
        case itemUpdatedAt = "item_updated_at"
    }
}

struct HiddenTodayRecord: Codable, Equatable {
    let seenAt: Int64?
    let dismissedAt: Int64?
    let snoozedUntil: Int64?
    let snapshot: TodayVisibilitySnapshot?
    enum CodingKeys: String, CodingKey {
        case snapshot
        case seenAt = "seen_at"
        case dismissedAt = "dismissed_at"
        case snoozedUntil = "snoozed_until"
    }
}

struct HiddenTodayItem: Codable, Identifiable, Equatable {
    let itemID: String
    let hiddenKind: String
    let record: HiddenTodayRecord
    var id: String { itemID }
    enum CodingKeys: String, CodingKey {
        case itemID = "item_id"
        case hiddenKind = "hidden_kind"
        case record
    }
}

private struct HiddenTodayResponse: Codable { let items: [HiddenTodayItem] }

/// Frozen server-issued binding for an attention card. Feedback may update the
/// contextual posterior only when the card was selected and actually served in
/// the lane where the owner acted on it. Keep this deliberately smaller than
/// the full routing diagnostic: iOS needs the causal identity, not model internals.
struct AttentionDecisionBinding: Codable, Equatable {
    let decisionID: String
    let candidateID: String
    let sourceRevision: String?
    let servedRoute: String
    let selected: Bool

    enum CodingKeys: String, CodingKey {
        case selected
        case decisionID = "decision_id"
        case candidateID = "candidate_id"
        case sourceRevision = "source_revision"
        case servedRoute = "served_route"
    }
}

/// Causal attribution accepted by the shared attention-learning outcome APIs.
/// Impression and canonical-delivery identities are optional by contract; the
/// legacy Today lanes have an exact decision binding but no delivery page.
struct AttentionFeedbackAttribution: Codable, Equatable {
    let decisionID: String
    let candidateID: String
    let sourceRevision: String?
    let impressionID: String?
    let deliveryID: String?

    enum CodingKeys: String, CodingKey {
        case decisionID = "decision_id"
        case candidateID = "candidate_id"
        case sourceRevision = "source_revision"
        case impressionID = "impression_id"
        case deliveryID = "delivery_id"
    }

    var jsonObject: [String: Any] {
        var value: [String: Any] = [
            "decision_id": decisionID,
            "candidate_id": candidateID
        ]
        if let sourceRevision {
            value["source_revision"] = sourceRevision
        } else {
            value["source_revision"] = NSNull()
        }
        if let impressionID { value["impression_id"] = impressionID }
        if let deliveryID { value["delivery_id"] = deliveryID }
        return value
    }
}

/// Fail-soft learning diagnostics returned by every feedback-bearing channel
/// action. The UI intentionally stays quiet on success, but retaining this
/// receipt makes disabled/degraded posterior updates inspectable instead of
/// discarding the only end-to-end acknowledgement from the learner.
struct AttentionPosteriorUpdateReceipt: Codable, Equatable {
    let status: String
    let attributionQuality: String?
    let degradationReason: String?
    let posteriorVersionBefore: UInt64?
    let posteriorVersionAfter: UInt64?
    let rescoreScheduled: Bool?

    enum CodingKeys: String, CodingKey {
        case status
        case attributionQuality = "attribution_quality"
        case degradationReason = "degradation_reason"
        case posteriorVersionBefore = "posterior_version_before"
        case posteriorVersionAfter = "posterior_version_after"
        case rescoreScheduled = "rescore_scheduled"
    }
}

struct AttentionFeedbackReceipt: Codable, Equatable, Identifiable {
    let outcomeID: String
    let outcome: String
    let surface: String
    let feedbackRecorded: Bool
    let affectedCandidates: Int
    let rescoreStatus: String
    let diagnosticHref: String?
    let posteriorUpdate: AttentionPosteriorUpdateReceipt?

    var id: String { outcomeID }

    enum CodingKeys: String, CodingKey {
        case outcome, surface
        case outcomeID = "outcome_id"
        case feedbackRecorded = "feedback_recorded"
        case affectedCandidates = "affected_candidates"
        case rescoreStatus = "rescore_status"
        case diagnosticHref = "diagnostic_href"
        case posteriorUpdate = "posterior_update"
    }
}

private struct AttentionFeedbackEnvelope: Codable {
    let feedbackReceipt: AttentionFeedbackReceipt?
    enum CodingKeys: String, CodingKey { case feedbackReceipt = "feedback_receipt" }
}

struct CanonicalAttentionProjectionReference: Codable, Equatable {
    let projectionID: String
    let universeDigest: String
    let status: String

    enum CodingKeys: String, CodingKey {
        case status
        case projectionID = "projection_id"
        case universeDigest = "universe_digest"
    }
}

private struct AttentionDeliveryRootDecision: Codable {
    let decisionID: String
    let lane: String
    let projectionID: String
    let universeDigest: String
    let expiresAt: Int64

    enum CodingKeys: String, CodingKey {
        case lane
        case decisionID = "decision_id"
        case projectionID = "projection_id"
        case universeDigest = "universe_digest"
        case expiresAt = "expires_at"
    }
}

private struct AttentionDeliveryPageIdentity: Codable {
    let deliveryID: String
    let pageIndex: Int
    let pageStart: Int
    let pageSize: Int
    let nextCursor: String?
    let expiresAt: Int64

    enum CodingKeys: String, CodingKey {
        case deliveryID = "delivery_id"
        case pageIndex = "page_index"
        case pageStart = "page_start"
        case pageSize = "page_size"
        case nextCursor = "next_cursor"
        case expiresAt = "expires_at"
    }
}

private struct AttentionImpressionPolicy: Codable {
    let minVisibleMS: Int
    let visibilityRuleVersion: String

    enum CodingKeys: String, CodingKey {
        case minVisibleMS = "min_visible_ms"
        case visibilityRuleVersion = "visibility_rule_version"
    }
}

private struct AttentionCanonicalOrigin: Codable {
    let kind: String
    let annotationID: String?
    let candidateID: String?

    enum CodingKeys: String, CodingKey {
        case kind
        case annotationID = "annotation_id"
        case candidateID = "candidate_id"
    }
}

private struct AttentionDeliveredCanonicalItem: Codable {
    let canonicalID: String
    let sourceRevision: String?
    let servedLane: String
    let origin: AttentionCanonicalOrigin

    enum CodingKeys: String, CodingKey {
        case origin
        case canonicalID = "canonical_id"
        case sourceRevision = "source_revision"
        case servedLane = "served_lane"
    }
}

private struct AttentionDeliveredItem: Codable {
    let position: Int
    let candidateID: String
    let sourceRevision: String?
    let rootPolicyPropensity: Double
    let conditionalDeliveryPropensity: Double
    let exposureToken: String
    let item: AttentionDeliveredCanonicalItem

    enum CodingKeys: String, CodingKey {
        case position, item
        case candidateID = "candidate_id"
        case sourceRevision = "source_revision"
        case rootPolicyPropensity = "root_policy_propensity"
        case conditionalDeliveryPropensity = "conditional_delivery_propensity"
        case exposureToken = "exposure_token"
    }
}

private struct AttentionDeliveryPageResponse: Codable {
    let schemaVersion: Int
    let rootDecision: AttentionDeliveryRootDecision
    let page: AttentionDeliveryPageIdentity
    let items: [AttentionDeliveredItem]
    let impressionPolicy: AttentionImpressionPolicy

    enum CodingKeys: String, CodingKey {
        case items, page
        case schemaVersion = "schema_version"
        case rootDecision = "root_decision"
        case impressionPolicy = "impression_policy"
    }

    func validatedBindings(
        expected reference: CanonicalAttentionProjectionReference,
        surface: String,
        principal: String,
        workspace: String,
        nowMS: Int64 = Int64(Date().timeIntervalSince1970 * 1_000)
    ) -> [AttentionDeliveryBinding]? {
        guard schemaVersion == 1,
              rootDecision.lane == surface,
              rootDecision.projectionID == reference.projectionID,
              rootDecision.universeDigest == reference.universeDigest,
              rootDecision.expiresAt == page.expiresAt,
              rootDecision.expiresAt > nowMS,
              !rootDecision.decisionID.isEmpty,
              !page.deliveryID.isEmpty,
              page.pageIndex >= 0,
              page.pageStart >= 0,
              page.pageSize >= 1,
              items.count <= page.pageSize,
              (1...60_000).contains(impressionPolicy.minVisibleMS),
              !impressionPolicy.visibilityRuleVersion.isEmpty else { return nil }

        var rawIDs = Set<String>()
        var candidateIDs = Set<String>()
        var exposureTokens = Set<String>()
        var bindings: [AttentionDeliveryBinding] = []
        bindings.reserveCapacity(items.count)
        for (offset, delivered) in items.enumerated() {
            let expectedPosition = page.pageStart + offset + 1
            guard delivered.position == expectedPosition,
                  delivered.candidateID == delivered.item.canonicalID,
                  delivered.sourceRevision == delivered.item.sourceRevision,
                  delivered.item.servedLane == surface,
                  (0...1).contains(delivered.rootPolicyPropensity),
                  delivered.conditionalDeliveryPropensity == 1,
                  !delivered.exposureToken.isEmpty else { return nil }
            let rawID: String?
            switch delivered.item.origin.kind {
            case "follow_up": rawID = delivered.item.origin.annotationID
            case "worth_a_look": rawID = delivered.item.origin.candidateID
            default: rawID = nil
            }
            guard let rawID, !rawID.isEmpty,
                  rawIDs.insert(rawID).inserted,
                  candidateIDs.insert(delivered.candidateID).inserted,
                  exposureTokens.insert(delivered.exposureToken).inserted else { return nil }
            bindings.append(AttentionDeliveryBinding(
                principal: principal,
                workspace: workspace,
                rawItemID: rawID,
                originKind: delivered.item.origin.kind,
                decisionID: rootDecision.decisionID,
                deliveryID: page.deliveryID,
                pageIndex: page.pageIndex,
                position: delivered.position,
                exposureToken: delivered.exposureToken,
                candidateID: delivered.candidateID,
                sourceRevision: delivered.sourceRevision,
                surface: surface,
                minVisibleMS: impressionPolicy.minVisibleMS,
                visibilityRuleVersion: impressionPolicy.visibilityRuleVersion,
                rootPolicyPropensity: delivered.rootPolicyPropensity,
                expiresAt: rootDecision.expiresAt
            ))
        }
        return bindings
    }
}

struct AttentionDeliveryBinding: Equatable {
    let principal: String
    let workspace: String
    let rawItemID: String
    let originKind: String
    let decisionID: String
    let deliveryID: String
    let pageIndex: Int
    let position: Int
    let exposureToken: String
    let candidateID: String
    let sourceRevision: String?
    let surface: String
    let minVisibleMS: Int
    let visibilityRuleVersion: String
    let rootPolicyPropensity: Double
    let expiresAt: Int64

    var identity: String {
        [principal, workspace, decisionID, deliveryID, String(pageIndex), String(position),
         candidateID, sourceRevision ?? "", exposureToken, visibilityRuleVersion]
            .joined(separator: "\u{0}")
    }

    var feedbackAttribution: AttentionFeedbackAttribution {
        AttentionFeedbackAttribution(
            decisionID: decisionID,
            candidateID: candidateID,
            sourceRevision: sourceRevision,
            impressionID: AttentionImpressionLedger.shared.impressionID(for: identity),
            deliveryID: deliveryID
        )
    }
}

struct AttentionImpressionReceipt: Codable, Equatable {
    let impressionID: String
    let eventID: String
    let decisionID: String
    let deliveryID: String
    let pageIndex: Int
    let position: Int
    let exposureToken: String
    let candidateID: String
    let sourceRevision: String?
    let surface: String
    let accumulatedVisibleMS: Int
    let minVisibleMS: Int
    let visibilityRuleVersion: String
    let rootPolicyPropensity: Double
    let conditionalDeliveryPropensity: Double
    let verified: Bool
    let deduplicated: Bool

    enum CodingKeys: String, CodingKey {
        case surface, verified, deduplicated, position
        case impressionID = "impression_id"
        case eventID = "event_id"
        case decisionID = "decision_id"
        case deliveryID = "delivery_id"
        case pageIndex = "page_index"
        case exposureToken = "exposure_token"
        case candidateID = "candidate_id"
        case sourceRevision = "source_revision"
        case accumulatedVisibleMS = "accumulated_visible_ms"
        case minVisibleMS = "min_visible_ms"
        case visibilityRuleVersion = "visibility_rule_version"
        case rootPolicyPropensity = "root_policy_propensity"
        case conditionalDeliveryPropensity = "conditional_delivery_propensity"
    }
}

final class AttentionImpressionLedger: @unchecked Sendable {
    static let shared = AttentionImpressionLedger()
    private let lock = NSLock()
    private var receipts: [String: AttentionImpressionReceipt] = [:]

    private init() {}

    func impressionID(for identity: String) -> String? {
        lock.lock(); defer { lock.unlock() }
        return receipts[identity]?.impressionID
    }

    func receipt(for identity: String) -> AttentionImpressionReceipt? {
        lock.lock(); defer { lock.unlock() }
        return receipts[identity]
    }

    func store(_ receipt: AttentionImpressionReceipt, for identity: String) {
        lock.lock(); defer { lock.unlock() }
        receipts[identity] = receipt
        if receipts.count > 2_000 {
            receipts.removeValue(forKey: receipts.keys.first!)
        }
    }

    #if DEBUG
    func resetForTests() {
        lock.lock(); defer { lock.unlock() }
        receipts.removeAll()
    }
    #endif
}

actor AttentionImpressionRecorder {
    static let shared = AttentionImpressionRecorder()
    private var eventIDs: [String: String] = [:]
    private var inFlight: Set<String> = []
    private var terminal: Set<String> = []

    func record(_ binding: AttentionDeliveryBinding,
                visibleMS: Int,
                networkSession: URLSession = .shared,
                baseURL: URL = URL(string: "\(MagicianAccess.baseURL.absoluteString)")!) async {
        let identity = binding.identity
        guard AttentionImpressionLedger.shared.receipt(for: identity) == nil,
              !inFlight.contains(identity), !terminal.contains(identity),
              binding.expiresAt > Int64(Date().timeIntervalSince1970 * 1_000) else { return }
        inFlight.insert(identity)
        defer { inFlight.remove(identity) }
        let eventID = eventIDs[identity] ?? UUID().uuidString
        eventIDs[identity] = eventID
        let boundedVisibleMS = min(86_400_000, max(binding.minVisibleMS, visibleMS))

        for attempt in 0..<4 {
            guard !Task.isCancelled else { return }
            do {
                let url = baseURL.appendingPathComponent(
                    "/api/magician/v2/channel-assist/attention-learning/impressions"
                )
                var request = URLRequest(url: url)
                request.httpMethod = "POST"
                request.setValue("application/json", forHTTPHeaderField: "Content-Type")
                let clientVersion: String =
                    (Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString")
                        as? String) ?? "development"
                let sourceRevision: Any
                if let value = binding.sourceRevision {
                    sourceRevision = value
                } else {
                    sourceRevision = NSNull()
                }
                request.httpBody = try JSONSerialization.data(withJSONObject: [
                    "event_id": eventID,
                    "decision_id": binding.decisionID,
                    "delivery_id": binding.deliveryID,
                    "page_index": binding.pageIndex,
                    "position": binding.position,
                    "exposure_token": binding.exposureToken,
                    "candidate_id": binding.candidateID,
                    "source_revision": sourceRevision,
                    "surface": binding.surface,
                    "visible_ms": boundedVisibleMS,
                    "visibility_rule_version": binding.visibilityRuleVersion,
                    "client_type": "ios",
                    "client_version": clientVersion,
                    "viewport_class": "compact"
                ])
                MagicianAccess.authorize(&request)
                let (data, response) = try await networkSession.data(for: request)
                let status = (response as? HTTPURLResponse)?.statusCode ?? 0
                guard (200..<300).contains(status) else {
                    if status >= 500, attempt < 3 {
                        try await Task.sleep(for: .milliseconds(500 * (1 << attempt)))
                        continue
                    }
                    terminal.insert(identity)
                    return
                }
                let receipt = try JSONDecoder().decode(AttentionImpressionReceipt.self, from: data)
                guard receipt.verified,
                      receipt.eventID == eventID,
                      receipt.decisionID == binding.decisionID,
                      receipt.deliveryID == binding.deliveryID,
                      receipt.pageIndex == binding.pageIndex,
                      receipt.position == binding.position,
                      receipt.exposureToken == binding.exposureToken,
                      receipt.candidateID == binding.candidateID,
                      receipt.sourceRevision == binding.sourceRevision,
                      receipt.surface == binding.surface,
                      receipt.visibilityRuleVersion == binding.visibilityRuleVersion,
                      receipt.rootPolicyPropensity == binding.rootPolicyPropensity,
                      receipt.conditionalDeliveryPropensity == 1,
                      receipt.accumulatedVisibleMS >= boundedVisibleMS else {
                    terminal.insert(identity)
                    return
                }
                AttentionImpressionLedger.shared.store(receipt, for: identity)
                terminal.insert(identity)
                return
            } catch {
                if attempt < 3 {
                    try? await Task.sleep(for: .milliseconds(500 * (1 << attempt)))
                } else {
                    terminal.insert(identity)
                }
            }
        }
    }
}

enum AttentionDeliveryLoader {
    static func load(
        surface: String,
        reference: CanonicalAttentionProjectionReference,
        pageSize: Int,
        principal: String,
        workspace: String,
        networkSession: URLSession,
        baseURL: URL
    ) async throws -> [AttentionDeliveryBinding] {
        guard surface == "follow_up" || surface == "worth_a_look" else {
            throw URLError(.badURL)
        }
        var components = URLComponents(
            url: baseURL.appendingPathComponent(
                "/api/magician/v2/channel-assist/attention-learning/canonical-deliveries/\(surface)"
            ),
            resolvingAgainstBaseURL: false
        )!
        components.queryItems = [
            URLQueryItem(name: "page_size", value: String(max(1, min(100, pageSize))))
        ]
        var request = URLRequest(url: components.url!)
        MagicianAccess.authorize(&request)
        let (data, response) = try await networkSession.data(for: request)
        guard let http = response as? HTTPURLResponse, (200..<300).contains(http.statusCode) else {
            throw URLError(.badServerResponse)
        }
        let page = try JSONDecoder().decode(AttentionDeliveryPageResponse.self, from: data)
        guard let bindings = page.validatedBindings(
            expected: reference,
            surface: surface,
            principal: principal,
            workspace: workspace
        ) else { throw URLError(.cannotParseResponse) }
        return bindings
    }
}

private func validatedAttentionFeedbackAttribution(
    decision: AttentionDecisionBinding?,
    candidateID: String,
    sourceRevision: String?,
    servedRoute: String
) -> AttentionFeedbackAttribution? {
    guard let decision,
          decision.selected,
          decision.servedRoute == servedRoute,
          decision.candidateID == candidateID,
          decision.sourceRevision == sourceRevision else { return nil }
    return AttentionFeedbackAttribution(
        decisionID: decision.decisionID,
        candidateID: decision.candidateID,
        sourceRevision: decision.sourceRevision,
        impressionID: nil,
        deliveryID: nil
    )
}

private func addingAttentionFeedbackMetadata(
    to body: [String: Any],
    action: String,
    attribution: AttentionFeedbackAttribution?
) -> [String: Any] {
    // Snooze is lifecycle-only. These are the channel endpoints that record a
    // durable attention outcome and therefore accept causal attribution.
    guard ["approve", "dismiss", "acknowledge", "useful"].contains(action) else { return body }
    var value = body
    value["event_id"] = UUID().uuidString
    if let attribution { value["attribution"] = attribution.jsonObject }
    return value
}

struct ResurfacingCard: Codable, Identifiable, Equatable {
    let candidateID: String
    let sourceRevision: String?
    let line: String
    let whyNow: String
    let sourceTitle: String
    let summary: String
    let sourceKind: String
    let sourceRef: String
    let detailLabel: String
    let temporalAnchorAt: Int64?
    let brief: ResurfacingBrief?
    let briefStatus: String
    let contentRevision: String?
    let sourceUpdated: Bool
    let recommendedAction: ResurfacingRecommendation?
    let actions: [ResurfacingActionCapability]
    let decisionItem: AttentionDecisionBinding?
    /// Where the Morning Brief deck's ⚡ Open lands: an external link, or an
    /// in-app route (`/tasks…`, `/t/…`). Both optional on the wire.
    let openURL: String?
    let sourceRoute: String?
    var deliveryBinding: AttentionDeliveryBinding?

    var id: String { candidateID }
    enum CodingKeys: String, CodingKey {
        case line, summary, brief, actions
        case openURL = "open_url"
        case sourceRoute = "source_route"
        case candidateID = "candidate_id"
        case sourceRevision = "source_revision"
        case whyNow = "why_now"
        case sourceTitle = "source_title"
        case sourceKind = "source_kind"
        case sourceRef = "source_ref"
        case detailLabel = "detail_label"
        case temporalAnchorAt = "temporal_anchor_at"
        case briefStatus = "brief_status"
        case contentRevision = "content_revision"
        case sourceUpdated = "source_updated"
        case recommendedAction = "recommended_action"
        case decisionItem = "decision_item"
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        candidateID = (try? c.decode(String.self, forKey: .candidateID)) ?? UUID().uuidString
        sourceRevision = try? c.decodeIfPresent(String.self, forKey: .sourceRevision)
        line = (try? c.decode(String.self, forKey: .line)) ?? ""
        whyNow = (try? c.decode(String.self, forKey: .whyNow)) ?? ""
        sourceTitle = (try? c.decode(String.self, forKey: .sourceTitle)) ?? ""
        summary = (try? c.decode(String.self, forKey: .summary)) ?? ""
        sourceKind = (try? c.decode(String.self, forKey: .sourceKind)) ?? "unknown"
        sourceRef = (try? c.decode(String.self, forKey: .sourceRef)) ?? ""
        detailLabel = (try? c.decode(String.self, forKey: .detailLabel)) ?? "Details"
        temporalAnchorAt = try? c.decodeIfPresent(Int64.self, forKey: .temporalAnchorAt)
        brief = try? c.decodeIfPresent(ResurfacingBrief.self, forKey: .brief)
        briefStatus = (try? c.decode(String.self, forKey: .briefStatus)) ?? "legacy"
        contentRevision = try? c.decodeIfPresent(String.self, forKey: .contentRevision)
        sourceUpdated = (try? c.decode(Bool.self, forKey: .sourceUpdated)) ?? false
        recommendedAction = try? c.decodeIfPresent(ResurfacingRecommendation.self, forKey: .recommendedAction)
        actions = (try? c.decode([ResurfacingActionCapability].self, forKey: .actions)) ?? []
        decisionItem = try? c.decodeIfPresent(AttentionDecisionBinding.self, forKey: .decisionItem)
        openURL = (try? c.decodeIfPresent(String.self, forKey: .openURL)) ?? nil
        sourceRoute = (try? c.decodeIfPresent(String.self, forKey: .sourceRoute)) ?? nil
        deliveryBinding = nil
    }

    var feedbackAttribution: AttentionFeedbackAttribution? {
        if let deliveryBinding { return deliveryBinding.feedbackAttribution }
        return validatedAttentionFeedbackAttribution(
            decision: decisionItem,
            candidateID: candidateID,
            sourceRevision: sourceRevision,
            servedRoute: "worth_a_look"
        )
    }
}

struct ResurfacingChangeFact: Codable, Equatable {
    let aspect: String
    let before: String?
    let after: String?
    let effectiveText: String?
    enum CodingKeys: String, CodingKey { case aspect, before, after; case effectiveText = "effective_text" }
}

struct ResurfacingTemporalFact: Codable, Equatable, Identifiable {
    let kind: String
    let text: String
    let atMS: Int64?
    let timezone: String?
    var id: String { "\(kind):\(text):\(atMS ?? 0)" }
    enum CodingKeys: String, CodingKey { case kind, text, timezone; case atMS = "at_ms" }
}

struct ResurfacingBrief: Codable, Equatable {
    let schemaVersion: Int
    let keyFacts: [String]
    let changes: [ResurfacingChangeFact]
    let temporalFacts: [ResurfacingTemporalFact]
    let detailStatus: String
    let missingDetails: [String]
    enum CodingKeys: String, CodingKey {
        case changes
        case schemaVersion = "schema_version"
        case keyFacts = "key_facts"
        case temporalFacts = "temporal_facts"
        case detailStatus = "detail_status"
        case missingDetails = "missing_details"
    }
}

enum ResurfacingActionKind: String, Codable, CaseIterable {
    case viewDetails = "view_details", openSource = "open_source", showOriginal = "show_original"
    case askPresto = "ask_presto", createTask = "create_task", createReminder = "create_reminder"
    case share, saveToMemory = "save_to_memory", summarizeDeeper = "summarize_deeper"

    var systemImage: String {
        switch self {
        case .viewDetails: return "doc.text.magnifyingglass"
        case .openSource: return "arrow.up.right.square"
        case .showOriginal: return "text.quote"
        case .askPresto: return "bubble.left.and.bubble.right"
        case .createTask: return "checklist"
        case .createReminder: return "clock.badge"
        case .share: return "square.and.arrow.up"
        case .saveToMemory: return "archivebox"
        case .summarizeDeeper: return "sparkles"
        }
    }
}

enum ResurfacingFeedbackAction: String, CaseIterable, Identifiable {
    case open
    case acknowledge
    case dismiss

    var id: String { rawValue }
    var label: String {
        switch self {
        case .open: return "Mark useful"
        case .acknowledge: return "Acknowledge"
        case .dismiss: return "Dismiss"
        }
    }
    var systemImage: String {
        switch self {
        case .open: return "hand.thumbsup.fill"
        case .acknowledge: return "checkmark.circle.fill"
        case .dismiss: return "xmark"
        }
    }
}

struct ResurfacingActionCapability: Codable, Equatable, Identifiable {
    let kind: ResurfacingActionKind
    let label: String
    let requiresInput: Bool
    let sideEffect: String
    var id: String { kind.rawValue }
    enum CodingKeys: String, CodingKey {
        case kind, label
        case requiresInput = "requires_input"
        case sideEffect = "side_effect"
    }
}

struct ResurfacingRecommendation: Codable, Equatable {
    let kind: ResurfacingActionKind
    let label: String
    let rationale: String
    let confidence: Double
    let contentRevision: String?
    let source: String
    enum CodingKeys: String, CodingKey { case kind, label, rationale, confidence, source; case contentRevision = "content_revision" }
}

struct ResurfacingCursor: Codable, Equatable {
    let surfacedAt: Int64
    let score: Double
    let candidateID: String
    enum CodingKeys: String, CodingKey { case score; case surfacedAt = "surfaced_at"; case candidateID = "candidate_id" }
}

struct ResurfacingPage: Codable {
    let cards: [ResurfacingCard]
    let total: Int
    let limit: Int?
    let offset: Int?
    let hasMore: Bool?
    let nextCursor: ResurfacingCursor?
    let canonicalProjectionReference: CanonicalAttentionProjectionReference?
    enum CodingKeys: String, CodingKey {
        case cards, total, limit, offset
        case hasMore = "has_more"
        case nextCursor = "next_cursor"
        case canonicalProjectionReference = "canonical_attention_projection_ref"
    }
}

struct ResurfacingDetail: Codable, Equatable, Identifiable {
    let candidateID: String
    let sourceKind: String
    let status: String
    let title: String?
    let summary: String?
    let brief: ResurfacingBrief?
    let contentRevision: String?
    let sourceRevision: String?
    let sourceUpdated: Bool
    let hasNewer: Bool
    let sourceRoute: String?
    let openURL: String?
    let source: JSONValue?
    let recommendedAction: ResurfacingRecommendation?
    let actions: [ResurfacingActionCapability]
    let original: JSONValue?
    let temporalAnchorAt: Int64?
    var id: String { candidateID }
    enum CodingKeys: String, CodingKey {
        case status, title, summary, brief, source, actions, original
        case candidateID = "candidate_id"; case sourceKind = "source_kind"
        case contentRevision = "content_revision"; case sourceRevision = "source_revision"
        case sourceUpdated = "source_updated"; case hasNewer = "has_newer"
        case sourceRoute = "source_route"; case openURL = "open_url"
        case recommendedAction = "recommended_action"; case temporalAnchorAt = "temporal_anchor_at"
    }
}

struct ResurfacingActionResult: Codable, Equatable {
    let candidateID: String?
    let action: String?
    let resultRef: String?
    let replayed: Bool?
    let result: JSONValue?
    enum CodingKeys: String, CodingKey { case action, replayed, result; case candidateID = "candidate_id"; case resultRef = "result_ref" }
}

struct ChannelActionDescriptor: Codable, Identifiable, Equatable {
    let id: String
    let label: String
    let needsCompose: Bool
    let confirm: Bool
    let icon: String?

    enum CodingKeys: String, CodingKey {
        case id, label, confirm, icon
        case needsCompose = "needs_compose"
    }

    func composeEndpoint(annotationID: String) -> String {
        "/api/magician/v2/channel-assist/annotations/\(Self.pathComponent(annotationID))/action/\(Self.pathComponent(id))/compose"
    }

    func commitEndpoint(annotationID: String) -> String {
        "/api/magician/v2/channel-assist/annotations/\(Self.pathComponent(annotationID))/action/\(Self.pathComponent(id))/commit"
    }

    var systemImage: String {
        switch icon?.lowercased() {
        case "reply": return "arrowshape.turn.up.left.fill"
        case "forward": return "arrowshape.turn.up.right.fill"
        case "heart", "like": return "heart.fill"
        default: return "bolt.fill"
        }
    }

    private static func pathComponent(_ value: String) -> String {
        var allowed = CharacterSet.urlPathAllowed
        allowed.remove(charactersIn: "/?#")
        return value.addingPercentEncoding(withAllowedCharacters: allowed) ?? value
    }
}

struct ChannelActionDraft: Codable, Equatable {
    let composeID: String
    let text: String
    enum CodingKeys: String, CodingKey { case text; case composeID = "compose_id" }
}

struct ChannelFollowUp: Codable, Identifiable, Equatable {
    let candidateID: String
    let sourceRevision: String?
    let annotationID: String
    let provider: String
    let accountAlias: String
    let accountEmail: String?
    let threadID: String
    let lane: String
    let label: String?
    let reason: String?
    let confidence: Double?
    let proposedAction: JSONValue
    let subject: String?
    let sender: String?
    let summary: String?
    let receivedAt: Int64?
    let createdAt: Int64
    let evidenceMessageID: String?
    let evidenceMessageAt: Int64?
    let openURL: String?
    let state: String?
    let reviewRequired: Bool
    let sourceFamily: String?
    let availableActions: [ChannelActionDescriptor]
    let decisionItem: AttentionDecisionBinding?
    var deliveryBinding: AttentionDeliveryBinding?

    var id: String { annotationID }
    var canAcknowledge: Bool { !reviewRequired }
    enum CodingKeys: String, CodingKey {
        case provider, lane, label, reason, subject, sender, summary, confidence
        case candidateID = "candidate_id"
        case sourceRevision = "source_revision"
        case annotationID = "annotation_id"
        case accountAlias = "account_alias"
        case accountEmail = "account_email"
        case threadID = "thread_id"
        case proposedAction = "proposed_action"
        case createdAt = "created_at"
        case evidenceMessageID = "evidence_message_id"
        case evidenceMessageAt = "evidence_message_at"
        case receivedAt = "received_at"
        case openURL = "open_url"
        case state
        case reviewRequired = "review_required"
        case sourceFamily = "source_family"
        case availableActions = "available_actions"
        case decisionItem = "decision_item"
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        annotationID = (try? c.decode(String.self, forKey: .annotationID)) ?? UUID().uuidString
        candidateID = (try? c.decode(String.self, forKey: .candidateID)) ?? annotationID
        sourceRevision = try? c.decodeIfPresent(String.self, forKey: .sourceRevision)
        provider = (try? c.decode(String.self, forKey: .provider)) ?? "unknown"
        accountAlias = (try? c.decode(String.self, forKey: .accountAlias)) ?? ""
        accountEmail = try? c.decodeIfPresent(String.self, forKey: .accountEmail)
        threadID = (try? c.decode(String.self, forKey: .threadID)) ?? ""
        lane = (try? c.decode(String.self, forKey: .lane)) ?? "user_assist"
        label = try? c.decodeIfPresent(String.self, forKey: .label)
        reason = try? c.decodeIfPresent(String.self, forKey: .reason)
        confidence = try? c.decodeIfPresent(Double.self, forKey: .confidence)
        proposedAction = (try? c.decode(JSONValue.self, forKey: .proposedAction)) ?? .null
        subject = try? c.decodeIfPresent(String.self, forKey: .subject)
        sender = try? c.decodeIfPresent(String.self, forKey: .sender)
        summary = try? c.decodeIfPresent(String.self, forKey: .summary)
        receivedAt = try? c.decodeIfPresent(Int64.self, forKey: .receivedAt)
        createdAt = (try? c.decode(Int64.self, forKey: .createdAt)) ?? 0
        evidenceMessageID = try? c.decodeIfPresent(String.self, forKey: .evidenceMessageID)
        evidenceMessageAt = try? c.decodeIfPresent(Int64.self, forKey: .evidenceMessageAt)
        openURL = try? c.decodeIfPresent(String.self, forKey: .openURL)
        state = try? c.decodeIfPresent(String.self, forKey: .state)
        reviewRequired = (try? c.decode(Bool.self, forKey: .reviewRequired)) ?? false
        sourceFamily = try? c.decodeIfPresent(String.self, forKey: .sourceFamily)
        availableActions = (try? c.decode([ChannelActionDescriptor].self, forKey: .availableActions)) ?? []
        decisionItem = try? c.decodeIfPresent(AttentionDecisionBinding.self, forKey: .decisionItem)
        deliveryBinding = nil
    }

    var feedbackAttribution: AttentionFeedbackAttribution? {
        if let deliveryBinding { return deliveryBinding.feedbackAttribution }
        return validatedAttentionFeedbackAttribution(
            decision: decisionItem,
            candidateID: candidateID,
            sourceRevision: sourceRevision,
            servedRoute: "follow_up"
        )
    }

    var actionSummary: String? {
        guard let value = proposedAction.objectValue else { return nil }
        var parts: [String] = []
        if let kind = value["follow_up_kind"]?.stringValue, !kind.isEmpty { parts.append(kind.replacingOccurrences(of: "_", with: " ").capitalized) }
        if let owner = value["action_owner"]?.stringValue, !owner.isEmpty, owner != "unknown" { parts.append("Owner: \(owner.capitalized)") }
        if let due = value["due_text"]?.stringValue, !due.isEmpty { parts.append("Due: \(due)") }
        if let urgency = value["urgency"]?.stringValue, !urgency.isEmpty, urgency != "normal" { parts.append(urgency.capitalized) }
        if let details = value["key_details"]?.arrayValue?.compactMap(\.stringValue).prefix(3), !details.isEmpty {
            parts.append("Details: \(details.joined(separator: " · "))")
        }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }
}

struct ChannelFollowUpPage: Codable {
    let items: [ChannelFollowUp]
    let total: Int
    let limit: Int?
    let cursor: String?
    let nextCursor: String?
    let hasMore: Bool?
    let canonicalProjectionReference: CanonicalAttentionProjectionReference?
    enum CodingKeys: String, CodingKey {
        case items, total, limit, cursor
        case nextCursor = "next_cursor"
        case hasMore = "has_more"
        case canonicalProjectionReference = "canonical_attention_projection_ref"
    }
}

func applyingAttentionDelivery(
    _ bindings: [AttentionDeliveryBinding],
    to items: [ChannelFollowUp]
) -> [ChannelFollowUp]? {
    guard bindings.allSatisfy({ $0.surface == "follow_up" && $0.originKind == "follow_up" }) else {
        return nil
    }
    var source: [String: ChannelFollowUp] = [:]
    for item in items {
        if source.updateValue(item, forKey: item.id) != nil { return nil }
    }
    guard bindings.allSatisfy({ binding in
              source[binding.rawItemID]?.sourceRevision == binding.sourceRevision
          }) else { return nil }
    let boundIDs = Set(bindings.map(\.rawItemID))
    var ordered: [ChannelFollowUp] = []
    ordered.reserveCapacity(items.count)
    for binding in bindings.sorted(by: { $0.position < $1.position }) {
        guard var item = source[binding.rawItemID] else { return nil }
        item.deliveryBinding = binding
        ordered.append(item)
    }
    ordered.append(contentsOf: items.filter { !boundIDs.contains($0.id) })
    return ordered
}

private func applyingAttentionDelivery(
    _ bindings: [AttentionDeliveryBinding],
    to cards: [ResurfacingCard]
) -> [ResurfacingCard]? {
    guard bindings.allSatisfy({ $0.surface == "worth_a_look" && $0.originKind == "worth_a_look" }) else {
        return nil
    }
    var source: [String: ResurfacingCard] = [:]
    for card in cards {
        if source.updateValue(card, forKey: card.id) != nil { return nil }
    }
    guard bindings.allSatisfy({ binding in
              source[binding.rawItemID]?.sourceRevision == binding.sourceRevision
          }) else { return nil }
    let boundIDs = Set(bindings.map(\.rawItemID))
    var ordered: [ResurfacingCard] = []
    ordered.reserveCapacity(cards.count)
    for binding in bindings.sorted(by: { $0.position < $1.position }) {
        guard var card = source[binding.rawItemID] else { return nil }
        card.deliveryBinding = binding
        ordered.append(card)
    }
    ordered.append(contentsOf: cards.filter { !boundIDs.contains($0.id) })
    return ordered
}

struct ChannelEvidenceMessage: Codable, Equatable, Identifiable {
    let messageID: String?
    let body: String?
    let summary: String?
    let subject: String?
    let receivedAt: Int64?
    var id: String { messageID ?? "\(subject ?? "message"):\(receivedAt ?? 0)" }
    enum CodingKeys: String, CodingKey { case body, summary, subject; case messageID = "message_id"; case receivedAt = "received_at" }
}

struct ChannelMessageView: Codable, Equatable {
    let body: String?
    let summary: String?
    let subject: String?
    let hasNewer: Bool
    let evidenceMessages: [ChannelEvidenceMessage]
    enum CodingKeys: String, CodingKey { case body, summary, subject; case hasNewer = "has_newer"; case evidenceMessages = "evidence_messages" }
}

/// A learned writing preference for a sender/domain — web parity with the
/// ChannelFollowUpActions "Writing style" modal (`ChannelWritingPreference`).
struct ChannelWritingPreference: Codable, Equatable, Identifiable {
    let id: String
    let scopeKind: String
    let scopeValue: String
    let statement: String
    let status: String
    let evidenceCount: Int
    enum CodingKeys: String, CodingKey {
        case id, statement, status
        case scopeKind = "scope_kind"
        case scopeValue = "scope_value"
        case evidenceCount = "evidence_count"
    }
}

private struct ChannelWritingPreferencesResponse: Codable { let items: [ChannelWritingPreference] }

/// The canonical channel-follow-up dismissal choices shared by Today and
/// Attention. Keep these codes aligned with the web ChannelFollowUpActions
/// contract because the backend records them as recommendation feedback.
struct ChannelFollowUpDismissOption: Identifiable, Equatable {
    let code: String?
    let label: String

    var id: String { code ?? "no_reason" }

    static let all: [ChannelFollowUpDismissOption] = [
        .init(code: nil, label: "No reason"),
        .init(code: "spam", label: "Spam / junk"),
        .init(code: "already_handled", label: "Already taken care of"),
        .init(code: "duplicate", label: "Duplicate request"),
        .init(code: "delegated", label: "Someone else handles this"),
        .init(code: "not_relevant", label: "Not relevant to me"),
        .init(code: "wrong_classification", label: "Shouldn't have been flagged"),
    ]
}

/// Resurfacing ("Worth a look") dismissal reasons. Resurfacing records a
/// narrower vocabulary than channel follow-ups — no `wrong_classification` —
/// and the web client drops anything else before posting, so iOS offers only
/// these (plus the one-click no-reason dismiss).
struct ResurfacingDismissOption: Identifiable, Equatable {
    let code: String?
    let label: String

    var id: String { code ?? "no_reason" }

    static let all: [ResurfacingDismissOption] = [
        .init(code: nil, label: "No reason"),
        .init(code: "spam", label: "Spam / junk"),
        .init(code: "already_handled", label: "Already taken care of"),
        .init(code: "duplicate", label: "Duplicate"),
        .init(code: "delegated", label: "Someone else handles this"),
        .init(code: "not_relevant", label: "Not relevant to me"),
    ]

    static let allowedCodes: Set<String> = Set(all.compactMap(\.code))
}

/// App-scoped optimistic suppression shared by Today and Attention. A committed
/// tombstone keeps racing refreshes from resurrecting a just-resolved card, then
/// expires so a legitimately reissued card with the same identity can return.
enum OptimisticCardKey: Hashable, Sendable {
    case today(String)
    case resurfacing(String)
    case channelFollowUp(String)
    case attention(String)
    case hitl(String)
}

struct OptimisticCardMutationTicket: Equatable, Sendable {
    let key: OptimisticCardKey
    fileprivate let generation: UUID
}

/// Stable pre-mutation order for compensating rollback. Absolute indices and
/// immediate neighbors can both disappear when several cards leave together;
/// the full order finds the nearest surviving successor or predecessor.
struct OptimisticCardListAnchor: Equatable, Sendable {
    let fallbackIndex: Int
    private let orderedIDs: [String]

    init(ids: [String], index: Int) {
        orderedIDs = ids
        fallbackIndex = index
    }

    func insertionIndex(in ids: [String]) -> Int {
        let successorStart = min(fallbackIndex + 1, orderedIDs.count)
        for successor in orderedIDs.dropFirst(successorStart) {
            if let index = ids.firstIndex(of: successor) { return index }
        }
        for predecessor in orderedIDs.prefix(min(fallbackIndex, orderedIDs.count)).reversed() {
            if let index = ids.firstIndex(of: predecessor) { return index + 1 }
        }
        return min(fallbackIndex, ids.count)
    }
}

/// Preserves the pre-mutation order for a list until every concurrent
/// optimistic mutation in that list settles. Each rollback anchor therefore
/// sees rows removed by earlier transactions, including when the whole visible
/// list was removed before failures arrive.
struct OptimisticCardOrderLedger {
    private var orders: [String: [String]] = [:]
    private var pendingCounts: [String: Int] = [:]

    mutating func begin(listKey: String,
                        itemID: String,
                        currentIDs: [String]) -> OptimisticCardListAnchor {
        var stableIDs = orders[listKey] ?? currentIDs
        for id in currentIDs where !stableIDs.contains(id) { stableIDs.append(id) }
        if !stableIDs.contains(itemID) { stableIDs.append(itemID) }
        orders[listKey] = stableIDs
        pendingCounts[listKey, default: 0] += 1
        return OptimisticCardListAnchor(
            ids: stableIDs,
            index: stableIDs.firstIndex(of: itemID) ?? stableIDs.count - 1
        )
    }

    mutating func finish(listKey: String) {
        let remaining = max(0, (pendingCounts[listKey] ?? 0) - 1)
        if remaining == 0 {
            pendingCounts[listKey] = nil
            orders[listKey] = nil
        } else {
            pendingCounts[listKey] = remaining
        }
    }
}

final class CardMutationCoordinator: ObservableObject {
    static let shared = CardMutationCoordinator()
    let expired = PassthroughSubject<OptimisticCardKey, Never>()

    private enum State: Equatable {
        case pending(UUID)
        case committed(until: Date)
    }

    @Published private var states: [OptimisticCardKey: State] = [:]
    private var expiryWork: [OptimisticCardKey: DispatchWorkItem] = [:]

    func isSuppressed(_ key: OptimisticCardKey) -> Bool {
        guard let state = states[key] else { return false }
        if case .committed(let until) = state { return until > Date() }
        return true
    }

    func isPending(_ key: OptimisticCardKey) -> Bool {
        guard let state = states[key], case .pending = state else { return false }
        return true
    }

    func begin(_ key: OptimisticCardKey) -> OptimisticCardMutationTicket? {
        pruneExpiredCommitted()
        guard states[key] == nil else { return nil }
        let generation = UUID()
        states[key] = .pending(generation)
        return OptimisticCardMutationTicket(key: key, generation: generation)
    }

    func succeed(_ ticket: OptimisticCardMutationTicket,
                 suppressFor gracePeriod: TimeInterval = 120) {
        guard states[ticket.key] == .pending(ticket.generation) else { return }
        let until = Date().addingTimeInterval(gracePeriod)
        states[ticket.key] = .committed(until: until)
        scheduleExpiry(of: ticket.key, at: until)
    }

    func fail(_ ticket: OptimisticCardMutationTicket) {
        guard states[ticket.key] == .pending(ticket.generation) else { return }
        expiryWork[ticket.key]?.cancel()
        expiryWork[ticket.key] = nil
        states[ticket.key] = nil
    }

    /// Use when the network side effect already succeeded (for example a sent
    /// channel action) and every surface should suppress its stale local copy.
    func commit(_ key: OptimisticCardKey, suppressFor gracePeriod: TimeInterval = 120) {
        pruneExpiredCommitted()
        let until = Date().addingTimeInterval(gracePeriod)
        states[key] = .committed(until: until)
        scheduleExpiry(of: key, at: until)
    }

    func clear(_ key: OptimisticCardKey) {
        expiryWork[key]?.cancel()
        expiryWork[key] = nil
        states[key] = nil
    }

    private func pruneExpiredCommitted(now: Date = Date()) {
        let expiredKeys = states.compactMap { key, state -> OptimisticCardKey? in
            guard case .committed(let until) = state, until <= now else { return nil }
            return key
        }
        guard !expiredKeys.isEmpty else { return }
        let retained = states.filter { key, _ in
            !expiredKeys.contains(key)
        }
        states = retained
        for key in expiredKeys {
            expiryWork[key]?.cancel()
            expiryWork[key] = nil
            expired.send(key)
        }
    }

    private func scheduleExpiry(of key: OptimisticCardKey, at until: Date) {
        expiryWork[key]?.cancel()
        let work = DispatchWorkItem { [weak self] in
            guard let self,
                  case .committed(let currentUntil) = self.states[key],
                  currentUntil == until,
                  currentUntil <= Date() else { return }
            self.states[key] = nil
            self.expiryWork[key] = nil
            self.expired.send(key)
        }
        expiryWork[key] = work
        DispatchQueue.main.asyncAfter(
            deadline: .now() + max(0, until.timeIntervalSinceNow),
            execute: work
        )
    }
}

/// Shared, source-agnostic executor for channel follow-up actions. Today and
/// Attention render the same adapter descriptors through this client so a new
/// channel action does not require provider-specific iOS code.
@MainActor
final class ChannelFollowUpActionClient: ObservableObject {
    @Published private(set) var busyKey: String?
    @Published private(set) var resolvingIDs: Set<String> = []
    @Published private(set) var error: String?
    @Published private(set) var messages: [String: ChannelMessageView] = [:]
    @Published private(set) var feedbackReceipts: [String: AttentionFeedbackReceipt] = [:]

    private let networkSession: URLSession
    private let baseURL: URL
    private let principal: String
    private let workspace: String

    init(networkSession: URLSession = .shared,
         baseURL: URL = URL(string: "\(MagicianAccess.baseURL.absoluteString)")!,
         principal: String = MagicianAccess.principal,
         workspace: String = MagicianAccess.workspace) {
        self.networkSession = networkSession
        self.baseURL = baseURL
        self.principal = principal
        self.workspace = workspace
    }

    func clearError() { error = nil }

    func compose(_ descriptor: ChannelActionDescriptor,
                 for item: ChannelFollowUp,
                 hint: String? = nil) async -> ChannelActionDraft? {
        let key = "compose:\(item.id):\(descriptor.id)"
        guard busyKey == nil else { return nil }
        busyKey = key
        error = nil
        defer { busyKey = nil }
        do {
            var body: [String: Any] = [:]
            if let hint = hint?.trimmingCharacters(in: .whitespacesAndNewlines), !hint.isEmpty {
                body["hint"] = hint
            }
            return try await postDecoding(descriptor.composeEndpoint(annotationID: item.id), body: body)
        } catch {
            self.error = Self.errorMessage(error)
            return nil
        }
    }

    func commit(_ descriptor: ChannelActionDescriptor,
                for item: ChannelFollowUp,
                body: String? = nil,
                composeID: String? = nil) async -> Bool {
        let key = "commit:\(item.id):\(descriptor.id)"
        guard busyKey == nil else { return false }
        busyKey = key
        error = nil
        defer { busyKey = nil }
        do {
            var payload: [String: Any] = [:]
            if let body = body?.trimmingCharacters(in: .whitespacesAndNewlines), !body.isEmpty {
                payload["body"] = body
            }
            if let composeID, !composeID.isEmpty { payload["compose_id"] = composeID }
            payload["event_id"] = UUID().uuidString
            if let attribution = item.feedbackAttribution {
                payload["attribution"] = attribution.jsonObject
            }
            let receipt = try await postFeedback(
                descriptor.commitEndpoint(annotationID: item.id),
                body: payload
            )
            if let receipt { feedbackReceipts[item.id] = receipt }
            return true
        } catch {
            self.error = Self.errorMessage(error)
            return false
        }
    }

    func resolve(_ item: ChannelFollowUp,
                 action: String,
                 hint: String? = nil,
                 reason: String? = nil) async -> Bool {
        // Resolution is a background mutation after the owning list has already
        // hidden the card. Serialize only the same card; unrelated cards must be
        // free to resolve concurrently instead of sharing the compose/commit lock.
        guard !resolvingIDs.contains(item.id),
              busyKey?.contains(":\(item.id)") != true else { return false }
        resolvingIDs.insert(item.id)
        error = nil
        defer { resolvingIDs.remove(item.id) }
        do {
            var body: [String: Any] = [:]
            if let hint = hint?.trimmingCharacters(in: .whitespacesAndNewlines), !hint.isEmpty {
                body["hint"] = hint
            }
            if let reason, !reason.isEmpty { body["reason"] = reason }
            body = addingAttentionFeedbackMetadata(
                to: body,
                action: action,
                attribution: item.feedbackAttribution
            )
            let endpoint = "/api/magician/v2/channel-assist/annotations/\(Self.pathComponent(item.id))/\(Self.pathComponent(action))"
            let receipt = try await postFeedback(endpoint, body: body)
            if let receipt { feedbackReceipts[item.id] = receipt }
            return true
        } catch {
            self.error = Self.errorMessage(error)
            return false
        }
    }

    /// Create an Apple Reminder from a message follow-up.
    ///
    /// Two-phase, matching the Worth-a-look flow: EventKit creates the reminder
    /// locally first, a pending receipt is persisted so a crash or relaunch
    /// before the server call cannot create a second one, and only then is the
    /// action recorded server-side with the EventKit identifier. The server
    /// shares one contextual-action path across both lanes, so this is the same
    /// contract the Worth-a-look surface already uses.
    @discardableResult
    func createReminder(for item: ChannelFollowUp,
                        title: String,
                        notes: String,
                        dueAt: Date,
                        timeZone: TimeZone) async -> Bool {
        guard busyKey == nil else { return false }
        busyKey = "reminder:\(item.id)"
        error = nil
        defer { busyKey = nil }

        let pendingKey = AppleReminderPendingReceiptStore.operationKey(candidateID: item.id)
        let pending = AppleReminderPendingReceiptStore.shared.receipt(for: pendingKey)
        let idempotencyKey = pending?.idempotencyKey ?? UUID().uuidString
        do {
            let externalID: String
            if let pending {
                // A reminder already exists from an interrupted attempt; commit
                // that one rather than creating a duplicate.
                externalID = pending.identifier
            } else {
                let created = try await AppleReminderService.shared.create(
                    title: title,
                    notes: notes,
                    dueAt: dueAt,
                    timeZone: timeZone
                )
                externalID = created.identifier
                AppleReminderPendingReceiptStore.shared.save(
                    PendingAppleReminderReceipt(
                        idempotencyKey: idempotencyKey,
                        identifier: created.identifier,
                        title: title,
                        notes: notes,
                        dueAt: dueAt,
                        timeZoneIdentifier: timeZone.identifier,
                        createdAt: Date()
                    ),
                    for: pendingKey
                )
            }
            let body: [String: Any] = [
                "kind": "create_reminder",
                "idempotency_key": idempotencyKey,
                "content_revision": item.sourceRevision ?? NSNull(),
                "input": [
                    "title": title,
                    "instruction": notes,
                    "at": ISO8601DateFormatter().string(from: dueAt),
                    "timezone": timeZone.identifier,
                    "delivery": "client_apple_eventkit",
                    "external_id": externalID
                ]
            ]
            let endpoint = "/api/magician/v2/channel-assist/follow-ups/\(Self.pathComponent(item.id))/actions"
            try await post(endpoint, body: body)
            AppleReminderPendingReceiptStore.shared.remove(for: pendingKey)
            return true
        } catch {
            // The pending receipt is deliberately retained: the reminder may
            // already exist locally, so a retry must reuse it.
            self.error = Self.errorMessage(error)
            return false
        }
    }

    func fetchMessage(for item: ChannelFollowUp) async {
        guard busyKey == nil else { return }
        busyKey = "message:\(item.id)"
        error = nil
        defer { busyKey = nil }
        do {
            let endpoint = "/api/magician/v2/channel-assist/annotations/\(Self.pathComponent(item.id))/message"
            let message: ChannelMessageView = try await get(endpoint)
            messages[item.id] = message
        } catch {
            self.error = Self.errorMessage(error)
        }
    }

    /// Fetch the learned writing preferences for the follow-up's sender/domain —
    /// web parity with the "Writing style" modal. Returns [] on error.
    func fetchWritingPreferences(for item: ChannelFollowUp) async -> [ChannelWritingPreference] {
        do {
            let endpoint = "/api/magician/v2/channel-assist/annotations/\(Self.pathComponent(item.id))/writing-preferences"
            let response: ChannelWritingPreferencesResponse = try await get(endpoint)
            return response.items
        } catch {
            self.error = Self.errorMessage(error)
            return []
        }
    }

    /// Learn an exact writing-style statement for the sender or domain. Web parity:
    /// POST annotations/{id}/writing-preferences { scope, statement, promote }.
    func learnWritingPreference(for item: ChannelFollowUp,
                                scope: String,
                                statement: String,
                                promote: Bool) async -> Bool {
        let trimmed = statement.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, busyKey == nil else { return false }
        busyKey = "writing:\(item.id)"
        error = nil
        defer { busyKey = nil }
        do {
            let endpoint = "/api/magician/v2/channel-assist/annotations/\(Self.pathComponent(item.id))/writing-preferences"
            try await post(endpoint, body: ["scope": scope, "statement": trimmed, "promote": promote])
            return true
        } catch {
            self.error = Self.errorMessage(error)
            return false
        }
    }

    /// Promote or dismiss a candidate writing preference. Web parity:
    /// POST writing-preferences/{id}/{action}.
    func updateWritingPreference(id: String, action: String) async -> Bool {
        error = nil
        do {
            let endpoint = "/api/magician/v2/channel-assist/writing-preferences/\(Self.pathComponent(id))/\(Self.pathComponent(action))"
            try await post(endpoint, body: [:])
            return true
        } catch {
            self.error = Self.errorMessage(error)
            return false
        }
    }

    private func get<T: Decodable>(_ path: String) async throws -> T {
        let request = try request(path: path, method: "GET", body: nil)
        let (data, response) = try await networkSession.data(for: request)
        try Self.validate(response, data: data)
        return try JSONDecoder().decode(T.self, from: data)
    }

    private func post(_ path: String, body: [String: Any]) async throws {
        let request = try request(path: path, method: "POST", body: body)
        let (data, response) = try await networkSession.data(for: request)
        try Self.validate(response, data: data)
    }

    private func postFeedback(_ path: String, body: [String: Any]) async throws -> AttentionFeedbackReceipt? {
        let request = try request(path: path, method: "POST", body: body)
        let (data, response) = try await networkSession.data(for: request)
        try Self.validate(response, data: data)
        return (try? JSONDecoder().decode(AttentionFeedbackEnvelope.self, from: data))?.feedbackReceipt
    }

    private func postDecoding<T: Decodable>(_ path: String, body: [String: Any]) async throws -> T {
        let request = try request(path: path, method: "POST", body: body)
        let (data, response) = try await networkSession.data(for: request)
        try Self.validate(response, data: data)
        return try JSONDecoder().decode(T.self, from: data)
    }

    private func request(path: String, method: String, body: [String: Any]?) throws -> URLRequest {
        var request = URLRequest(url: baseURL.appendingPathComponent(path))
        request.httpMethod = method
        if let body {
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            request.httpBody = try JSONSerialization.data(withJSONObject: body)
        }
        MagicianAccess.authorize(&request)
        return request
    }

    private static func validate(_ response: URLResponse, data: Data) throws {
        guard let http = response as? HTTPURLResponse else { throw URLError(.badServerResponse) }
        guard (200..<300).contains(http.statusCode) else {
            let message = (try? JSONSerialization.jsonObject(with: data) as? [String: Any])?["error"] as? String
            throw NSError(domain: "ChannelFollowUpAction", code: http.statusCode,
                          userInfo: [NSLocalizedDescriptionKey: message ?? "Request failed (HTTP \(http.statusCode))."])
        }
    }

    private static func errorMessage(_ error: Error) -> String {
        (error as NSError).localizedDescription
    }

    private static func pathComponent(_ value: String) -> String {
        var allowed = CharacterSet.urlPathAllowed
        allowed.remove(charactersIn: "/?#")
        return value.addingPercentEncoding(withAllowedCharacters: allowed) ?? value
    }
}

struct TodayActivityItem: Codable, Identifiable, Equatable {
    let id: String
    let itemType: String
    let taskID: String?
    let threadID: String?
    let agentID: String?
    let title: String
    let summary: String?
    let status: String
    let updatedAt: Int64
    let createdAt: Int64
    let actions: [TodayAction]
    let metadata: JSONValue

    enum CodingKeys: String, CodingKey {
        case id, title, summary, status, actions, metadata
        case itemType = "item_type"
        case taskID = "task_id"
        case threadID = "ui_thread_id"
        case agentID = "agent_id"
        case updatedAt = "updated_at"
        case createdAt = "created_at"
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = (try? c.decode(String.self, forKey: .id)) ?? UUID().uuidString
        itemType = (try? c.decode(String.self, forKey: .itemType)) ?? "unknown"
        taskID = try? c.decodeIfPresent(String.self, forKey: .taskID)
        threadID = try? c.decodeIfPresent(String.self, forKey: .threadID)
        agentID = try? c.decodeIfPresent(String.self, forKey: .agentID)
        // Empty when the row has no title, so the Realtime Wire can apply its
        // own "Distilled Memory" / "Fleet Activity" fallback; the Activity
        // sheet renders `displayTitle`.
        title = (try? c.decode(String.self, forKey: .title)) ?? ""
        summary = try? c.decodeIfPresent(String.self, forKey: .summary)
        status = (try? c.decode(String.self, forKey: .status)) ?? "info"
        updatedAt = (try? c.decode(Int64.self, forKey: .updatedAt)) ?? 0
        createdAt = (try? c.decode(Int64.self, forKey: .createdAt)) ?? updatedAt
        actions = (try? c.decode([TodayAction].self, forKey: .actions)) ?? []
        metadata = (try? c.decode(JSONValue.self, forKey: .metadata)) ?? .null
    }

    func metadataString(_ key: String) -> String? {
        let value = metadata.objectValue?[key]?.stringValue?.trimmingCharacters(in: .whitespacesAndNewlines)
        return value?.isEmpty == false ? value : nil
    }

    var briefingSurfaceID: String? { metadataString("surface_id") }
    var route: String? { metadataString("route") }
    var displayTitle: String {
        let trimmed = title.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? "Activity" : trimmed
    }
}

struct TodayActivityResponse: Codable { let items: [TodayActivityItem] }

struct TodayBriefing: Codable, Identifiable, Equatable {
    let surface: Surface
    let taskTitle: String?
    let taskStatus: String?
    let sourceAgentID: String?
    let sourceOutputSummary: String?
    let renderKind: String?
    let presentationState: String?
    var id: String { surface.surfaceID }

    struct Surface: Codable, Equatable {
        let surfaceID: String
        let route: String
        let title: String
        let summary: String?
        let taskID: String?
        let uiThreadID: String?
        let documentKey: String?
        let status: String?
        let publishedAt: String
        let updatedAt: String?
        enum CodingKeys: String, CodingKey {
            case route, title, summary, status
            case taskID = "task_id"
            case uiThreadID = "ui_thread_id"
            case documentKey = "document_key"
            case surfaceID = "surface_id"
            case publishedAt = "published_at"
            case updatedAt = "updated_at"
        }
    }

    enum CodingKeys: String, CodingKey {
        case surface
        case taskTitle = "task_title"
        case taskStatus = "task_status"
        case sourceAgentID = "source_agent_id"
        case sourceOutputSummary = "source_output_summary"
        case renderKind = "render_kind"
        case presentationState = "presentation_state"
    }
}

struct TodayBriefingsResponse: Codable { let surfaces: [TodayBriefing] }

struct TodayBriefingRenderEnvelope: Codable { let render: TodayBriefingRender }
struct TodayBriefingRender: Codable, Equatable {
    let surface: JSONValue?
    let sourceAgentID: String?
    let sourceOutputSummary: String?
    let textContent: String?
    let jsonContent: JSONValue?
    let muijDocument: JSONValue?
    let unavailableReason: String?
    enum CodingKeys: String, CodingKey {
        case surface
        case sourceAgentID = "source_agent_id"
        case sourceOutputSummary = "source_output_summary"
        case textContent = "text_content"
        case jsonContent = "json_content"
        case muijDocument = "muij_document"
        case unavailableReason = "unavailable_reason"
    }
}

struct TodayPulse: Equatable {
    struct TopModel: Equatable { let provider: String; let model: String; let share: Double }
    var spendToday = 0.0
    var spendYesterday = 0.0
    var callsToday = 0
    var callsYesterday = 0
    var hourlySpend = Array(repeating: 0.0, count: 24)
    var hourlyCalls = Array(repeating: 0, count: 24)
    var topModel: TopModel?
    var tasksCompletedToday = 0
    var tasksCompletedYesterday = 0
    var codingRunsToday = 0
    var memoriesToday = 0
    var evalCasesToday = 0
    var evalPassesToday = 0
    /// Fleet panel task buckets from `/api/magician/v3/tasks`: succeeded is
    /// `max(#completed, tasksCompletedToday)`, in flight is running/paused/planning.
    var tasksSucceeded = 0
    var tasksFailed = 0
    var tasksInFlight = 0
    /// From `/api/magician/v2/agents`; nil until that fetch succeeds once.
    var agents: TodayAgentCounts?
    /// Most recently updated tasks (max 20) for the State of Operations slide.
    var recentTasks: [TodayRecentTask] = []
    /// State of the Crew (last 24 h); nil until it loads once.
    var crew: TodayCrewSummary?
    /// Why the crew slide could not refresh (shown with Retry; the last good
    /// crew, if any, stays — never invented numbers).
    var crewError: String?

    var taskShares: TodayTaskShares {
        TodayMorningEdition.pieShares(succeeded: tasksSucceeded, failed: tasksFailed, inFlight: tasksInFlight)
    }

    var tasksAttempted: Int { tasksSucceeded + tasksFailed + tasksInFlight }

    var isEmpty: Bool {
        spendToday == 0 && callsToday == 0 && tasksCompletedToday == 0 && codingRunsToday == 0
            && memoriesToday == 0 && evalCasesToday == 0
    }
}

private enum PulseCell: Codable {
    case number(Double), string(String), bool(Bool), null

    init(from decoder: Decoder) throws {
        let c = try decoder.singleValueContainer()
        if c.decodeNil() { self = .null }
        else if let value = try? c.decode(Double.self) { self = .number(value) }
        else if let value = try? c.decode(String.self) { self = .string(value) }
        else if let value = try? c.decode(Bool.self) { self = .bool(value) }
        else { self = .null }
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.singleValueContainer()
        switch self { case .number(let v): try c.encode(v); case .string(let v): try c.encode(v); case .bool(let v): try c.encode(v); case .null: try c.encodeNil() }
    }

    var number: Double {
        switch self { case .number(let v): return v; case .string(let v): return Double(v) ?? 0; case .bool(let v): return v ? 1 : 0; case .null: return 0 }
    }
    var text: String {
        switch self { case .string(let v): return v; case .number(let v): return String(v); case .bool(let v): return String(v); case .null: return "" }
    }
}

private struct PulseQueryResponse: Codable {
    let columns: [String]
    let rows: [[PulseCell]]
}

private struct PulseTaskPage: Codable { let tasks: [PulseTask] }

/// `/v3/tasks?sort=updated_at&order=desc` rows as the crew slide reads them.
private struct CrewTaskPage: Decodable {
    struct Row: Decodable {
        let agentID: String
        let status: String
        let updatedAt: Int64
        enum CodingKeys: String, CodingKey { case status; case agentID = "agent_id"; case updatedAt = "updated_at" }
        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            agentID = (try? c.decode(String.self, forKey: .agentID)) ?? ""
            status = (try? c.decode(String.self, forKey: .status)) ?? "unknown"
            if let text = try? c.decode(String.self, forKey: .updatedAt) {
                updatedAt = TodayMorningEdition.epochMilliseconds(fromISO: text) ?? 0
            } else {
                updatedAt = (try? c.decode(Int64.self, forKey: .updatedAt)) ?? 0
            }
        }
    }
    struct Pagination: Decodable {
        let nextCursor: String?
        enum CodingKeys: String, CodingKey { case nextCursor = "next_cursor" }
    }
    let tasks: [Row]
    let pagination: Pagination?
    enum CodingKeys: String, CodingKey { case tasks, pagination }
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        tasks = (try? c.decode([Row].self, forKey: .tasks)) ?? []
        pagination = try? c.decodeIfPresent(Pagination.self, forKey: .pagination)
    }
}
private struct PulseTask: Codable {
    let status: String
    let updatedAt: String?
    let taskID: String?
    let title: String?
    enum CodingKeys: String, CodingKey { case status, title; case updatedAt = "updated_at"; case taskID = "task_id"; case id }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        status = try c.decode(String.self, forKey: .status)
        updatedAt = try? c.decodeIfPresent(String.self, forKey: .updatedAt)
        taskID = (try? c.decodeIfPresent(String.self, forKey: .taskID)) ?? (try? c.decodeIfPresent(String.self, forKey: .id)) ?? nil
        title = try? c.decodeIfPresent(String.self, forKey: .title)
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(status, forKey: .status)
        try c.encodeIfPresent(updatedAt, forKey: .updatedAt)
        try c.encodeIfPresent(taskID, forKey: .taskID)
        try c.encodeIfPresent(title, forKey: .title)
    }
}

@MainActor
final class TodayViewModel: ObservableObject {
    @Published private(set) var payload: TodayResponse?
    @Published private(set) var hiddenItems: [HiddenTodayItem] = []
    @Published private(set) var resurfacingCards: [ResurfacingCard] = []
    @Published private(set) var resurfacingTotal = 0
    @Published private(set) var messageFollowUps: [ChannelFollowUp] = []
    @Published private(set) var messageFollowUpTotal = 0
    @Published private(set) var activityItems: [TodayActivityItem] = []
    @Published private(set) var briefings: [TodayBriefing] = []
    @Published private(set) var briefingRenders: [String: TodayBriefingRender] = [:]
    @Published private(set) var pulse: TodayPulse?
    @Published private(set) var isLoading = false
    @Published private(set) var error: String?
    @Published private(set) var sectionErrors: [String: String] = [:]
    @Published private(set) var actionItemID: String?
    @Published private(set) var lastHiddenItem: HiddenTodayItem?
    @Published private(set) var resurfacingDetails: [String: ResurfacingDetail] = [:]
    @Published private(set) var resurfacingActionResults: [String: ResurfacingActionResult] = [:]
    @Published private(set) var channelMessages: [String: ChannelMessageView] = [:]
    @Published private(set) var attentionFeedbackReceipts: [String: AttentionFeedbackReceipt] = [:]
    @Published private(set) var digestOffset = 0
    @Published private(set) var isDigestLoading = false
    /// In-flight optimistic card mutations. Keys are namespaced by surface so
    /// unrelated cards can queue independently without a global UI lock.
    @Published private(set) var pendingCardMutationKeys: Set<String> = []
    /// Realtime Wire lines (feed, agent updates, live websocket events):
    /// newest first, de-duplicated, at most 50. Never seeded with placeholders.
    @Published private(set) var wireItems: [TodayWireItem] = []
    /// Events in the last 24h — the analytics count, bumped per live event.
    /// Stays at its last known value (initially 0) when the query fails.
    @Published private(set) var wireEventCount24h = 0
    /// True while the Morning Brief deck is pulling the next follow-up /
    /// worth page because its visible stack ran low.
    @Published private(set) var isDeckLoadingMore = false
    /// Deck vs Broadsheet, remembered per device.
    @Published var readingRoomMode: TodayReadingMode {
        didSet { defaults.set(readingRoomMode.rawValue, forKey: Self.readingRoomModeKey) }
    }

    /// Broadsheet server pages — separate from the deck's load-more lists.
    /// A nil total means that tab has not loaded a page yet.
    @Published private(set) var broadsheetFollowUps: [ChannelFollowUp] = []
    @Published private(set) var broadsheetFollowUpPage = 1
    @Published private(set) var broadsheetFollowUpTotal: Int?
    @Published private(set) var broadsheetWorth: [ResurfacingCard] = []
    @Published private(set) var broadsheetWorthPage = 1
    @Published private(set) var broadsheetWorthTotal: Int?
    @Published private(set) var broadsheetLoading: Set<TodayBroadsheetTab> = []

    static let readingRoomModeKey = "todayReadingRoomMode"
    static let broadsheetPageSize = 5
    /// Digest rows per page in § 5 The Chronicle & Digest.
    static let digestPageSize = 6

    /// How the reader's own calendar date is read, as `yyyy-MM-dd`.
    ///
    /// It delegates to the tasks list's derivation instead of owning a second
    /// one: `TasksViewModel.localDateISO(_:in:)` already spells the zone out,
    /// and a private copy here would be free to drift from it — which is how
    /// the two clients came to disagree about "today" in the first place.
    ///
    /// A closure evaluated per request, not a value cached at init, for two
    /// reasons. Today polls for hours, so a session that crosses local midnight
    /// has to start asking about the new day. And a test can pin a reader east
    /// of UTC at an hour where their date and the UTC date are different days,
    /// which is the only way to tell a local date from a UTC-rendered one.
    var readerLocalDate: () -> String = { TasksViewModel.localDateISO(Date(), in: .current) }

    private let networkSession: URLSession
    private let baseURL: URL
    private let principal: String
    private let workspace: String
    private let usesUITestFixture: Bool
    private let refreshesAfterActions: Bool
    private let mutationCoordinator: CardMutationCoordinator
    private let defaults: UserDefaults
    private var wireSequence = 0
    private var followUpPageCursors: [Int: String?] = [1: nil]
    private var broadsheetGeneration: [TodayBroadsheetTab: Int] = [:]
    private var mutationCoordinatorCancellable: AnyCancellable?
    private var mutationExpiryCancellable: AnyCancellable?
    private var sectionCursors: [TodaySection: String] = [:]
    private var resurfacingCursor: ResurfacingCursor?
    /// Where the next page of follow-ups begins — a KEYSET position
    /// (`created_at`, `annotation_id`) carried whole in the cursor, not an
    /// offset and not a row the server has to look up again.
    ///
    /// So resolving a follow-up does not move it. Rewinding it to `nil` used
    /// to be this list's undoing: the next Load more re-asked for page one,
    /// `merging` deduped every row away, and the button did nothing.
    private var messageFollowUpCursor: String?
    private var refreshTimer: Timer?
    private var followUpTimer: Timer?
    private var pulseTimer: Timer?
    private var webSocket: URLSessionWebSocketTask?
    private var realtimeRefresh: DispatchWorkItem?
    private var hideTasks: [String: Task<Void, Never>] = [:]
    private var rollbackOrderLedger = OptimisticCardOrderLedger()
    private var pendingTodayItems: [String: TodayItem] = [:]
    private var optimisticTodaySections: [String: TodaySection] = [:]
    private var pendingResurfacingIDs: Set<String> = []
    private var pendingFollowUpIDs: Set<String> = []
    private var optimisticResurfacingIDs: Set<String> = []
    private var optimisticFollowUpIDs: Set<String> = []
    private var started = false
    private var fetchGeneration = 0
    private var followUpProjectionReference: CanonicalAttentionProjectionReference?
    private var worthProjectionReference: CanonicalAttentionProjectionReference?

    init(networkSession: URLSession = .shared, baseURL: URL = URL(string: "\(MagicianAccess.baseURL.absoluteString)")!,
         principal: String = MagicianAccess.principal, workspace: String = MagicianAccess.workspace,
         refreshesAfterActions: Bool = true,
         mutationCoordinator: CardMutationCoordinator? = nil,
         defaults: UserDefaults = .standard) {
        self.networkSession = networkSession
        self.defaults = defaults
        self.readingRoomMode = defaults.string(forKey: Self.readingRoomModeKey)
            .flatMap(TodayReadingMode.init(rawValue:)) ?? .deck
        self.baseURL = baseURL
        self.principal = principal
        self.workspace = workspace
        self.refreshesAfterActions = refreshesAfterActions
        self.mutationCoordinator = mutationCoordinator
            ?? (isRunningUnderTests ? CardMutationCoordinator() : .shared)
#if DEBUG
        self.usesUITestFixture = ProcessInfo.processInfo.arguments.contains("--today-ui-test-fixture")
#else
        self.usesUITestFixture = false
#endif
        mutationCoordinatorCancellable = self.mutationCoordinator.objectWillChange
            .sink { [weak self] _ in self?.objectWillChange.send() }
        mutationExpiryCancellable = self.mutationCoordinator.expired
            .sink { [weak self] key in self?.restoreExpiredProjection(for: key) }
        if usesUITestFixture {
            // Deterministic Reading Room for UI tests: never inherit a mode a
            // previous run persisted.
            readingRoomMode = ProcessInfo.processInfo.arguments.contains("--today-ui-test-broadsheet") ? .broadsheet : .deck
            seedUITestFixture()
        }
    }

    private func seedUITestFixture() {
        payload = try? JSONDecoder().decode(TodayResponse.self, from: Self.uiTestTodayData)
        hiddenItems = (try? JSONDecoder().decode(HiddenTodayResponse.self, from: Self.uiTestHiddenData).items) ?? []
        resurfacingCards = (try? JSONDecoder().decode(ResurfacingPage.self, from: Self.uiTestResurfacingData).cards) ?? []
        resurfacingTotal = resurfacingCards.count
        messageFollowUps = (try? JSONDecoder().decode(ChannelFollowUpPage.self, from: Self.uiTestFollowUpsData).items) ?? []
        messageFollowUpTotal = messageFollowUps.count
        let rawActivity = (try? JSONDecoder().decode(TodayActivityResponse.self, from: Self.uiTestActivityData).items) ?? []
        activityItems = rawActivity.filter(Self.isDurableActivity)
        let updates = (try? JSONDecoder().decode(TodayAgentUpdatesResponse.self, from: Self.uiTestAgentUpdatesData).events) ?? []
        wireItems = TodayMorningEdition.mergingWireItems([], rawActivity.map { TodayMorningEdition.wireItem(fromFeed: $0) }
            + updates.map { TodayMorningEdition.wireItem(fromAgentUpdate: $0) })
        wireEventCount24h = 1_500
        briefings = (try? JSONDecoder().decode(TodayBriefingsResponse.self, from: Self.uiTestBriefingsData).surfaces) ?? []
        pulse = TodayPulse(spendToday: 1.42, spendYesterday: 0.91, callsToday: 18, callsYesterday: 12,
                           hourlySpend: [0, 0, 0, 0, 0, 0, 0.05, 0.2, 0.4, 0.77] + Array(repeating: 0, count: 14),
                           hourlyCalls: [0, 0, 0, 0, 0, 0, 1, 3, 5, 9] + Array(repeating: 0, count: 14),
                           topModel: .init(provider: "openai", model: "gpt-test", share: 0.72),
                           tasksCompletedToday: 3, tasksCompletedYesterday: 2, codingRunsToday: 4,
                           memoriesToday: 2, evalCasesToday: 5, evalPassesToday: 4,
                           tasksSucceeded: 6, tasksFailed: 1, tasksInFlight: 2,
                           agents: TodayAgentCounts(total: 4, enabled: 3, active: 1),
                           recentTasks: [
                               TodayRecentTask(id: "task-active", title: "Preparing release notes", status: "running", updatedAt: 1_783_900_800_000),
                               TodayRecentTask(id: "task-complete", title: "Release checklist completed", status: "completed", updatedAt: 1_783_900_700_000),
                               TodayRecentTask(id: "task-failed", title: "Sync vendor invoices", status: "failed", updatedAt: 1_783_900_600_000)
                           ],
                           crew: TodayCrewSummary(
                               members: [
                                   TodayCrewMember(id: "presto", name: "Presto", active: true, disabled: false, costUSD: 0.063,
                                                   calls: 22, okCalls: 22, tasksDone: 3, tasksFailed: 0),
                                   TodayCrewMember(id: "scout", name: "Scout", active: false, disabled: false, costUSD: 0.0042,
                                                   calls: 5, okCalls: 4, tasksDone: 1, tasksFailed: 1)
                               ],
                               activeAgents: 1, totalAgents: 4, costUSD: 0.0672, tasksDone: 4, reliabilityPercent: 96))
    }

    var counts: TodayCounts { payload?.counts ?? TodayCounts() }
    var headline: String { payload?.headline ?? "" }
    var generatedAt: Int64? { payload?.generatedAt }
    var digest: TodayDigest { payload?.digest ?? TodayDigest() }

    var availableSections: [TodaySection] {
        var sections: [TodaySection] = []
        if coreCount(for: .needsYou) > 0 { sections.append(.needsYou) }
        sections.append(contentsOf: [.followups, .worthALook, .activeWork, .delivered, .changed])
        return sections
    }

    func count(for section: TodaySection) -> Int {
        if section == .followups { return coreCount(for: section) + visibleMessageFollowUpTotal }
        if section == .worthALook { return visibleResurfacingTotal }
        return coreCount(for: section)
    }

    func coreCount(for section: TodaySection) -> Int {
        if section == .worthALook { return visibleResurfacingTotal }
        let rawItems = payload?.sections.items(for: section) ?? []
        let suppressedLoaded = rawItems.count - items(for: section).count
        let rawTotal = counts.count(for: section, resurfacing: visibleResurfacingTotal)
        return max(items(for: section).count, rawTotal - suppressedLoaded)
    }

    func items(for section: TodaySection) -> [TodayItem] {
        (payload?.sections.items(for: section) ?? []).filter {
            !mutationCoordinator.isSuppressed(.today($0.id))
                && !($0.attentionItemID.map { mutationCoordinator.isSuppressed(.hitl($0)) } ?? false)
        }
    }

    var visibleResurfacingCards: [ResurfacingCard] {
        resurfacingCards.filter { !mutationCoordinator.isSuppressed(.resurfacing($0.id)) }
    }

    var visibleMessageFollowUps: [ChannelFollowUp] {
        messageFollowUps.filter { !mutationCoordinator.isSuppressed(.channelFollowUp($0.id)) }
    }

    var visibleMessageFollowUpTotal: Int {
        let externallySuppressed = messageFollowUps.lazy.filter { [self] followUp in
            self.mutationCoordinator.isSuppressed(.channelFollowUp(followUp.id))
                && !self.optimisticFollowUpIDs.contains(followUp.id)
        }.count
        return max(visibleMessageFollowUps.count, messageFollowUpTotal - externallySuppressed)
    }

    var visibleResurfacingTotal: Int {
        let externallySuppressed = resurfacingCards.lazy.filter { [self] card in
            self.mutationCoordinator.isSuppressed(.resurfacing(card.id))
                && !self.optimisticResurfacingIDs.contains(card.id)
        }.count
        return max(visibleResurfacingCards.count, resurfacingTotal - externallySuppressed)
    }

    func start() {
        guard !started else { return }
        started = true
        guard !usesUITestFixture else { return }
        fetch()
        refreshTimer = Timer.scheduledTimer(withTimeInterval: 30, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.fetch() }
        }
        followUpTimer = Timer.scheduledTimer(withTimeInterval: 20, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.refreshMessageFollowUps() }
        }
        pulseTimer = Timer.scheduledTimer(withTimeInterval: 60, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.refreshPulse() }
        }
        connectRealtime()
    }

    func stop() {
        started = false
        refreshTimer?.invalidate(); refreshTimer = nil
        followUpTimer?.invalidate(); followUpTimer = nil
        pulseTimer?.invalidate(); pulseTimer = nil
        realtimeRefresh?.cancel(); realtimeRefresh = nil
        webSocket?.cancel(with: .goingAway, reason: nil); webSocket = nil
    }

    func fetch() {
        guard !isLoading else { return }
        fetchGeneration += 1
        let generation = fetchGeneration
        isLoading = true
        error = nil
        Task {
            do {
                async let today = captured {
                    try await self.get("/api/magician/v2/today", query: [
                        "per_section": "8", "digest_limit": String(Self.digestPageSize)
                    ]) as TodayResponse
                }
                async let hidden = captured { try await self.get("/api/magician/v2/today/visibility") as HiddenTodayResponse }
                async let resurfacing = captured { try await self.get("/api/magician/v2/channel-assist/resurfacing/today", query: ["limit": "8"]) as ResurfacingPage }
                async let followups = captured { try await self.get("/api/magician/v2/channel-assist/follow-ups", query: ["limit": "5"]) as ChannelFollowUpPage }
                async let activity = captured { try await self.get("/api/magician/v2/feed", query: [
                    "limit": "80"
                ]) as TodayActivityResponse }
                async let briefingPage = captured { try await self.get("/api/magician/v3/published-surfaces/projections", query: [
                    "route": "/briefing", "limit": "8"
                ]) as TodayBriefingsResponse }
                async let pulseValue = captured { try await self.fetchPulse() }
                async let agentUpdates = captured { try await self.get("/api/magician/v2/agents/updates") as TodayAgentUpdatesResponse }
                async let eventCount = captured { try await self.fetchWireEventCount() }

                let todayResult = await today
                guard generation == fetchGeneration else { return }
                switch todayResult {
                case .success(let todayValue):
                    payload = todayValue
                    if let data = try? JSONEncoder().encode(todayValue),
                       let glance = try? MagicanGlanceSnapshot.reducingToday(data) {
                        if MagicanGlanceCache.save(glance) {
                            WidgetCenter.shared.reloadTimelines(ofKind: "MagiosWidget")
                        }
                    }
                    // A refresh may race an optimistic hide while the backend is
                    // still applying it. Reapply the local projection so the card
                    // cannot flash back into the lane during that window.
                    for item in pendingTodayItems.values { removeItemLocally(item) }
                    digestOffset = 0
                case .failure(let failure):
                    error = Self.errorMessage(failure)
                }
                let hiddenResult = await hidden
                let resurfacingResult = await resurfacing
                let followUpsResult = await followups
                let activityResult = await activity
                let briefingResult = await briefingPage
                let pulseResult = await pulseValue
                let agentUpdatesResult = await agentUpdates
                let eventCountResult = await eventCount
                guard generation == fetchGeneration else { return }
                applyHidden(hiddenResult)
                applyResurfacing(resurfacingResult)
                applyFollowUps(followUpsResult)
                applyActivity(activityResult)
                applyBriefings(briefingResult)
                applyPulse(pulseResult)
                applyWire(feed: activityResult, updates: agentUpdatesResult, count: eventCountResult)
                reloadLoadedBroadsheetPages()
            }
            if generation == fetchGeneration { isLoading = false }
        }
    }

    private func captured<T>(_ operation: () async throws -> T) async -> Result<T, Error> {
        do { return .success(try await operation()) } catch { return .failure(error) }
    }

    private func applyHidden(_ result: Result<HiddenTodayResponse, Error>) {
        switch result { case .success(let value): hiddenItems = value.items; sectionErrors["hidden"] = nil
        case .failure(let value): sectionErrors["hidden"] = Self.errorMessage(value) }
    }
    private func applyResurfacing(_ result: Result<ResurfacingPage, Error>) {
        switch result {
        case .success(let value):
            resurfacingCards = value.cards
            worthProjectionReference = value.canonicalProjectionReference
            optimisticResurfacingIDs = Set(optimisticResurfacingIDs.filter {
                mutationCoordinator.isSuppressed(.resurfacing($0))
            })
            let rawIDs = Set(value.cards.map(\.id))
            let projectedDecrements = optimisticResurfacingIDs.lazy.filter { [self] id in
                let key = OptimisticCardKey.resurfacing(id)
                return self.mutationCoordinator.isPending(key)
                    || (self.mutationCoordinator.isSuppressed(key) && rawIDs.contains(id))
            }.count
            resurfacingTotal = max(visibleResurfacingCards.count, value.total - projectedDecrements)
            resurfacingCursor = value.nextCursor; sectionErrors[TodaySection.worthALook.rawValue] = nil
            refreshAttentionDelivery(
                surface: "worth_a_look",
                reference: value.canonicalProjectionReference,
                pageSize: max(1, value.cards.count)
            )
        case .failure(let value): sectionErrors[TodaySection.worthALook.rawValue] = Self.errorMessage(value)
        }
    }
    private func applyFollowUps(_ result: Result<ChannelFollowUpPage, Error>) {
        switch result {
        case .success(let value):
            messageFollowUps = value.items
            followUpProjectionReference = value.canonicalProjectionReference
            optimisticFollowUpIDs = Set(optimisticFollowUpIDs.filter {
                mutationCoordinator.isSuppressed(.channelFollowUp($0))
            })
            let rawIDs = Set(value.items.map(\.id))
            let projectedDecrements = optimisticFollowUpIDs.lazy.filter { [self] id in
                let key = OptimisticCardKey.channelFollowUp(id)
                return self.mutationCoordinator.isPending(key)
                    || (self.mutationCoordinator.isSuppressed(key) && rawIDs.contains(id))
            }.count
            messageFollowUpTotal = max(visibleMessageFollowUps.count, value.total - projectedDecrements)
            messageFollowUpCursor = value.nextCursor; sectionErrors["message_followups"] = nil
            refreshAttentionDelivery(
                surface: "follow_up",
                reference: value.canonicalProjectionReference,
                pageSize: max(1, value.items.count)
            )
        case .failure(let value): sectionErrors["message_followups"] = Self.errorMessage(value)
        }
    }

    private func refreshAttentionDelivery(
        surface: String,
        reference: CanonicalAttentionProjectionReference?,
        pageSize: Int
    ) {
        guard let reference else { return }
        Task { @MainActor in
            guard let bindings = try? await AttentionDeliveryLoader.load(
                surface: surface,
                reference: reference,
                pageSize: pageSize,
                principal: principal,
                workspace: workspace,
                networkSession: networkSession,
                baseURL: baseURL
            ) else { return }
            if surface == "follow_up" {
                guard followUpProjectionReference == reference,
                      let delivered = applyingAttentionDelivery(bindings, to: messageFollowUps) else { return }
                messageFollowUps = delivered
            } else {
                guard worthProjectionReference == reference,
                      let delivered = applyingAttentionDelivery(bindings, to: resurfacingCards) else { return }
                resurfacingCards = delivered
            }
        }
    }
    /// Wire lines from the SAME `/feed` response the Activity sheet uses
    /// (its newest 15 rows, like web's `feed?limit=15`) plus agent updates.
    /// Live websocket events already on the wire are kept; each source
    /// fails soft on its own.
    private func applyWire(feed: Result<TodayActivityResponse, Error>,
                           updates: Result<TodayAgentUpdatesResponse, Error>,
                           count: Result<Int, Error>) {
        var incoming: [TodayWireItem] = []
        if case .success(let value) = feed {
            incoming += value.items.prefix(Self.wireFeedLimit).map { TodayMorningEdition.wireItem(fromFeed: $0) }
        }
        if case .success(let value) = updates {
            incoming += value.events.map { TodayMorningEdition.wireItem(fromAgentUpdate: $0) }
        }
        if !incoming.isEmpty { wireItems = TodayMorningEdition.mergingWireItems(wireItems, incoming) }
        if case .success(let value) = count { wireEventCount24h = value }
    }

    static let wireFeedLimit = 15

    private func fetchWireEventCount(now: Date = Date()) async throws -> Int {
        let floor = Int64(now.timeIntervalSince1970 * 1_000) - 86_400_000
        let response = try await postQuery(
            "/api/magician/v2/analytics/query",
            sql: "SELECT COUNT(*) AS total_24h FROM events WHERE epoch_ms(timestamp) >= \(floor)"
        )
        guard let first = response.rows.first?.first, case .number(let value) = first else {
            throw URLError(.cannotParseResponse)
        }
        return Int(value)
    }

    private func applyActivity(_ result: Result<TodayActivityResponse, Error>) {
        switch result { case .success(let value): activityItems = value.items.filter(Self.isDurableActivity); sectionErrors["activity"] = nil
        case .failure(let value): sectionErrors["activity"] = Self.errorMessage(value) }
    }
    private func applyBriefings(_ result: Result<TodayBriefingsResponse, Error>) {
        switch result { case .success(let value): briefings = value.surfaces; sectionErrors["briefings"] = nil
        case .failure(let value): sectionErrors["briefings"] = Self.errorMessage(value) }
    }
    private func applyPulse(_ result: Result<TodayPulse, Error>) {
        switch result { case .success(let value): pulse = value; sectionErrors["pulse"] = nil
        case .failure(let value): sectionErrors["pulse"] = Self.errorMessage(value) }
    }

    func loadRemaining(_ section: TodaySection) {
        guard section != .worthALook, actionItemID == nil else { return }
        actionItemID = "lane:\(section.rawValue)"
        Task {
            do {
                let previousIDs = Set(items(for: section).map(\.id))
                let startingCursor = sectionCursors[section]
                var query = [
                    "section": section.rawValue,
                    "per_section": "8", "limit": "8", "digest_limit": String(Self.digestPageSize)
                ]
                if let cursor = sectionCursors[section] { query["cursor"] = cursor }
                let page: TodayResponse = try await get("/api/magician/v2/today", query: query)
                mergeSection(section, with: page.sections.items(for: section), counts: page.counts)
                sectionCursors[section] = page.sectionPage?.nextCursor
                let added = page.sections.items(for: section).contains { !previousIDs.contains($0.id) }
                if startingCursor == nil, !added, let cursor = page.sectionPage?.nextCursor {
                    query["cursor"] = cursor
                    let next: TodayResponse = try await get("/api/magician/v2/today", query: query)
                    mergeSection(section, with: next.sections.items(for: section), counts: next.counts)
                    sectionCursors[section] = next.sectionPage?.nextCursor
                }
                sectionErrors[section.rawValue] = nil
            } catch { sectionErrors[section.rawValue] = Self.errorMessage(error) }
            actionItemID = nil
        }
    }

    func loadRemainingResurfacing() {
        guard actionItemID == nil else { return }
        actionItemID = "lane:worth_a_look"
        Task {
            await fetchNextResurfacingPage()
            actionItemID = nil
        }
    }

    private func fetchNextResurfacingPage() async {
        do {
            var query = ["limit": "8"]
            if let cursor = resurfacingCursor {
                query["cursor_surfaced_at"] = String(cursor.surfacedAt)
                query["cursor_score"] = String(cursor.score)
                query["cursor_candidate_id"] = cursor.candidateID
            }
            let page: ResurfacingPage = try await get("/api/magician/v2/channel-assist/resurfacing/today", query: query)
            resurfacingCards = Self.merging(resurfacingCards, page.cards)
            let rawIDs = Set(resurfacingCards.map(\.id))
            let projectedDecrements = optimisticResurfacingIDs.lazy.filter { [self] id in
                let key = OptimisticCardKey.resurfacing(id)
                return self.mutationCoordinator.isPending(key)
                    || (self.mutationCoordinator.isSuppressed(key) && rawIDs.contains(id))
            }.count
            resurfacingTotal = max(visibleResurfacingCards.count, page.total - projectedDecrements)
            resurfacingCursor = page.nextCursor
            sectionErrors[TodaySection.worthALook.rawValue] = nil
        } catch { sectionErrors[TodaySection.worthALook.rawValue] = Self.errorMessage(error) }
    }

    func loadRemainingMessageFollowUps() {
        guard actionItemID == nil else { return }
        actionItemID = "lane:message_followups"
        Task {
            await fetchNextMessageFollowUpPage()
            actionItemID = nil
        }
    }

    private func fetchNextMessageFollowUpPage() async {
        do {
            var query = ["limit": "5"]
            if let cursor = messageFollowUpCursor { query["cursor"] = cursor }
            let page: ChannelFollowUpPage = try await get("/api/magician/v2/channel-assist/follow-ups", query: query)
            messageFollowUps = Self.merging(messageFollowUps, page.items)
            let rawIDs = Set(messageFollowUps.map(\.id))
            let projectedDecrements = optimisticFollowUpIDs.lazy.filter { [self] id in
                let key = OptimisticCardKey.channelFollowUp(id)
                return self.mutationCoordinator.isPending(key)
                    || (self.mutationCoordinator.isSuppressed(key) && rawIDs.contains(id))
            }.count
            messageFollowUpTotal = max(visibleMessageFollowUps.count, page.total - projectedDecrements)
            messageFollowUpCursor = page.nextCursor
            sectionErrors["message_followups"] = nil
        } catch { sectionErrors["message_followups"] = Self.errorMessage(error) }
    }

    /// Another follow-up page exists: the server said so AND handed back a
    /// cursor (without one a "next page" would re-read page one).
    var hasMoreMessageFollowUps: Bool {
        messageFollowUpCursor != nil && visibleMessageFollowUpTotal > visibleMessageFollowUps.count
    }

    var hasMoreResurfacing: Bool {
        resurfacingCursor != nil && visibleResurfacingTotal > visibleResurfacingCards.count
    }

    // MARK: Broadsheet server paging

    var visibleBroadsheetFollowUps: [ChannelFollowUp] {
        broadsheetFollowUps.filter { !mutationCoordinator.isSuppressed(.channelFollowUp($0.id)) }
    }

    var visibleBroadsheetWorth: [ResurfacingCard] {
        broadsheetWorth.filter { !mutationCoordinator.isSuppressed(.resurfacing($0.id)) }
    }

    /// The pager window for a tab; before its first page loads the totals
    /// already read for the deck stand in.
    func broadsheetWindow(_ tab: TodayBroadsheetTab) -> TodayPageWindow {
        switch tab {
        case .forYou:
            return TodayPageWindow(page: broadsheetFollowUpPage, pageSize: Self.broadsheetPageSize,
                                   total: broadsheetFollowUpTotal ?? visibleMessageFollowUpTotal,
                                   loaded: visibleBroadsheetFollowUps.count)
        case .worth:
            return TodayPageWindow(page: broadsheetWorthPage, pageSize: Self.broadsheetPageSize,
                                   total: broadsheetWorthTotal ?? visibleResurfacingTotal,
                                   loaded: visibleBroadsheetWorth.count)
        }
    }

    func isBroadsheetLoaded(_ tab: TodayBroadsheetTab) -> Bool {
        tab == .forYou ? broadsheetFollowUpTotal != nil : broadsheetWorthTotal != nil
    }

    /// Load page 1 the first time a tab is shown.
    func ensureBroadsheetLoaded(_ tab: TodayBroadsheetTab) {
        guard !isBroadsheetLoaded(tab), !broadsheetLoading.contains(tab) else { return }
        loadBroadsheetPage(tab, page: 1)
    }

    /// Fetch one Broadsheet page. Worth pages by offset; follow-ups by keyset
    /// cursor, walking forward from the nearest known page. A per-tab
    /// generation drops a late response for a page the reader already left.
    func loadBroadsheetPage(_ tab: TodayBroadsheetTab, page requested: Int) {
        let target = max(1, requested)
        let generation = (broadsheetGeneration[tab] ?? 0) + 1
        broadsheetGeneration[tab] = generation
        broadsheetLoading.insert(tab)
        if usesUITestFixture {
            applyFixtureBroadsheetPage(tab, page: target)
            broadsheetLoading.remove(tab)
            return
        }
        let size = Self.broadsheetPageSize
        Task { @MainActor in
            defer { if broadsheetGeneration[tab] == generation { broadsheetLoading.remove(tab) } }
            do {
                switch tab {
                case .worth:
                    let pageValue: ResurfacingPage = try await get(
                        "/api/magician/v2/channel-assist/resurfacing/today",
                        query: ["limit": String(size), "offset": String((target - 1) * size)]
                    )
                    guard broadsheetGeneration[tab] == generation else { return }
                    // Past the end (the column shrank): land on the last page.
                    if pageValue.cards.isEmpty, target > 1 {
                        let last = TodayPageWindow(page: 1, pageSize: size, total: pageValue.total, loaded: 0).pageCount
                        if last < target {
                            DispatchQueue.main.async { self.loadBroadsheetPage(tab, page: last) }
                            return
                        }
                    }
                    broadsheetWorth = pageValue.cards
                    broadsheetWorthTotal = pageValue.total
                    broadsheetWorthPage = target
                case .forYou:
                    let walk = await TodayMorningEdition.walkFollowUpCursor(to: target, known: followUpPageCursors) { cursor in
                        let page = try await self.fetchFollowUpPage(cursor: cursor, limit: size)
                        return (page.nextCursor, page.hasMore ?? (page.nextCursor != nil))
                    }
                    guard broadsheetGeneration[tab] == generation else { return }
                    followUpPageCursors = walk.known
                    let pageValue = try await fetchFollowUpPage(cursor: walk.cursor, limit: size)
                    guard broadsheetGeneration[tab] == generation else { return }
                    if let next = pageValue.nextCursor, pageValue.hasMore ?? true {
                        followUpPageCursors[walk.page + 1] = .some(next)
                    }
                    if pageValue.items.isEmpty, walk.page > 1 {
                        DispatchQueue.main.async { self.loadBroadsheetPage(tab, page: walk.page - 1) }
                        return
                    }
                    broadsheetFollowUps = pageValue.items
                    broadsheetFollowUpTotal = pageValue.total
                    broadsheetFollowUpPage = walk.page
                }
                sectionErrors["broadsheet:\(tab.rawValue)"] = nil
            } catch {
                guard broadsheetGeneration[tab] == generation else { return }
                sectionErrors["broadsheet:\(tab.rawValue)"] = Self.errorMessage(error)
            }
        }
    }

    private func fetchFollowUpPage(cursor: String?, limit: Int) async throws -> ChannelFollowUpPage {
        var query = ["limit": String(limit)]
        if let cursor { query["cursor"] = cursor }
        return try await get("/api/magician/v2/channel-assist/follow-ups", query: query)
    }

    /// Re-read a tab's current page (backfill after an action / on the poll).
    private func reloadBroadsheetPage(_ tab: TodayBroadsheetTab) {
        guard isBroadsheetLoaded(tab) else { return }
        loadBroadsheetPage(tab, page: tab == .forYou ? broadsheetFollowUpPage : broadsheetWorthPage)
    }

    private func reloadLoadedBroadsheetPages() {
        reloadBroadsheetPage(.forYou)
        reloadBroadsheetPage(.worth)
    }

    private func applyFixtureBroadsheetPage(_ tab: TodayBroadsheetTab, page: Int) {
        let size = Self.broadsheetPageSize
        switch tab {
        case .forYou:
            broadsheetFollowUps = Array(messageFollowUps.dropFirst((page - 1) * size).prefix(size))
            broadsheetFollowUpTotal = messageFollowUpTotal
            broadsheetFollowUpPage = page
        case .worth:
            broadsheetWorth = Array(resurfacingCards.dropFirst((page - 1) * size).prefix(size))
            broadsheetWorthTotal = resurfacingTotal
            broadsheetWorthPage = page
        }
    }

    /// Keep the Morning Brief deck flowing: when its visible stack runs low,
    /// pull the next page of whichever source(s) still have one, through the
    /// same cursor paging the broadsheet's "Load more" uses.
    func loadMoreDeckCards(followUps: Bool = true, worth: Bool = true) {
        let wantsFollowUps = followUps && hasMoreMessageFollowUps
        let wantsWorth = worth && hasMoreResurfacing
        guard !isDeckLoadingMore, actionItemID == nil, wantsFollowUps || wantsWorth else { return }
        isDeckLoadingMore = true
        Task {
            if wantsFollowUps { await fetchNextMessageFollowUpPage() }
            if wantsWorth { await fetchNextResurfacingPage() }
            isDeckLoadingMore = false
        }
    }

    func hide(_ item: TodayItem, action: String, snoozeMinutes: Int? = nil) {
        let mutationKey = "today:\(item.id)"
        let section = TodaySection(rawValue: item.section) ?? .changed
        optimisticTodaySections = optimisticTodaySections.filter { itemID, _ in
            mutationCoordinator.isSuppressed(.today(itemID))
        }
        guard let ticket = mutationCoordinator.begin(.today(item.id)) else { return }
        guard pendingCardMutationKeys.insert(mutationKey).inserted else {
            mutationCoordinator.fail(ticket)
            return
        }
        pendingTodayItems[item.id] = item
        optimisticTodaySections[item.id] = section
        let originalAnchor = anchorForTodayItem(item)
        let snapshot = TodayVisibilitySnapshot(title: item.title, summary: item.summary, reason: item.reason,
                                               section: item.section, sourceKind: item.sourceKind,
                                               sourceID: item.sourceID, sourceURL: item.sourceURL,
                                               spaceIDs: item.spaceIDs, itemUpdatedAt: item.updatedAt)
        let now = Int64(Date().timeIntervalSince1970 * 1_000)
        let optimistic = HiddenTodayItem(itemID: item.id, hiddenKind: action == "snooze" ? "snoozed" : "dismissed",
                                         record: HiddenTodayRecord(seenAt: item.seenAt,
                                                                   dismissedAt: action == "dismiss" ? now : nil,
                                                                   snoozedUntil: snoozeMinutes.map { now + Int64($0 * 60_000) },
                                                                   snapshot: snapshot))
        removeItemLocally(item)
        hiddenItems.removeAll { $0.id == item.id }
        hiddenItems.insert(optimistic, at: 0)
        lastHiddenItem = optimistic
        sectionCursors[section] = nil
        let operation = Task { @MainActor in
            defer {
                pendingCardMutationKeys.remove(mutationKey)
                pendingTodayItems[item.id] = nil
                hideTasks[item.id] = nil
                rollbackOrderLedger.finish(listKey: "today:\(section.rawValue)")
            }
            do {
                var body: [String: Any] = ["action": action, "snapshot": snapshotDictionary(item)]
                if let snoozeMinutes { body["snooze_minutes"] = snoozeMinutes }
                try await post("/api/magician/v2/today/items/\(item.id)/visibility", body: body)
                mutationCoordinator.succeed(ticket)
                fetchAfterAction()
            } catch {
                mutationCoordinator.fail(ticket)
                optimisticTodaySections[item.id] = nil
                hiddenItems.removeAll { $0.id == item.id }
                if lastHiddenItem?.id == item.id { lastHiddenItem = nil }
                insertItemLocally(item, at: originalAnchor)
                self.error = Self.errorMessage(error)
            }
        }
        hideTasks[item.id] = operation
    }

    func markSeen(_ item: TodayItem) {
        Task { try? await post("/api/magician/v2/today/items/\(item.id)/visibility", body: ["action": "mark_seen"]) }
    }

    func performTodayAction(_ action: TodayAction, for item: TodayItem, onTask: @escaping (String) -> Void) {
        guard actionItemID == nil else { return }
        guard let endpoint = action.executionEndpoint else {
            error = "This Today action is no longer available."
            return
        }
        actionItemID = item.id
        Task {
            do {
                let result: TodayActionExecutionResult = try await postDecoding(endpoint, body: [:])
                guard let taskID = result.resolvedTaskID, !taskID.isEmpty else {
                    throw URLError(.cannotParseResponse)
                }
                removeItemLocally(item)
                fetchAfterAction()
                onTask(taskID)
            } catch {
                self.error = Self.errorMessage(error)
            }
            actionItemID = nil
        }
    }

    func undoLastHidden() {
        guard let item = lastHiddenItem else { return }
        lastHiddenItem = nil
        restore(item)
    }

    func restore(_ item: HiddenTodayItem) {
        actionItemID = item.id
        let pendingHide = hideTasks[item.id]
        Task { @MainActor in
            await pendingHide?.value
            do {
                try await post("/api/magician/v2/today/items/\(item.id)/visibility", body: ["action": "restore"])
                mutationCoordinator.clear(.today(item.id))
                optimisticTodaySections[item.id] = nil
                hiddenItems.removeAll { $0.id == item.id }
                if lastHiddenItem?.id == item.id { lastHiddenItem = nil }
                if let raw = item.record.snapshot?.section, let section = TodaySection(rawValue: raw) { sectionCursors[section] = nil }
                fetchAfterAction()
            } catch { self.error = Self.errorMessage(error) }
            actionItemID = nil
        }
    }

    /// `completion` reports whether the server accepted the resolution (false
    /// after a rollback, or when the card was not resolvable) — the Morning
    /// Brief deck uses it to put a failed card back on the stack.
    func resolveFollowUp(_ item: ChannelFollowUp, action: String, hint: String? = nil, reason: String? = nil,
                         completion: ((Bool) -> Void)? = nil) {
        let mutationKey = "followup:\(item.id)"
        guard let ticket = mutationCoordinator.begin(.channelFollowUp(item.id)) else { completion?(false); return }
        let inDeck = messageFollowUps.contains(where: { $0.id == item.id })
        let broadsheetIndex = broadsheetFollowUps.firstIndex(where: { $0.id == item.id })
        guard pendingCardMutationKeys.insert(mutationKey).inserted, inDeck || broadsheetIndex != nil else {
            pendingCardMutationKeys.remove(mutationKey)
            mutationCoordinator.fail(ticket)
            completion?(false)
            return
        }
        if let broadsheetIndex {
            broadsheetFollowUps.remove(at: broadsheetIndex)
            broadsheetFollowUpTotal = broadsheetFollowUpTotal.map { max(0, $0 - 1) }
        }
        let originalAnchor = rollbackOrderLedger.begin(
            listKey: "message_followups",
            itemID: item.id,
            currentIDs: messageFollowUps.map(\.id)
        )
        pendingFollowUpIDs.insert(item.id)
        optimisticFollowUpIDs.insert(item.id)
        messageFollowUps.removeAll { $0.id == item.id }
        messageFollowUpTotal = max(0, messageFollowUpTotal - 1)
        error = nil
        Task { @MainActor in
            defer {
                pendingFollowUpIDs.remove(item.id)
                pendingCardMutationKeys.remove(mutationKey)
                rollbackOrderLedger.finish(listKey: "message_followups")
            }
            do {
                var body: [String: Any] = [:]
                if let hint = hint?.trimmingCharacters(in: .whitespacesAndNewlines), !hint.isEmpty { body["hint"] = hint }
                if let reason, !reason.isEmpty { body["reason"] = reason }
                body = addingAttentionFeedbackMetadata(
                    to: body,
                    action: action,
                    attribution: item.feedbackAttribution
                )
                let envelope: AttentionFeedbackEnvelope = try await postDecoding(
                    "/api/magician/v2/channel-assist/annotations/\(item.id)/\(action)",
                    body: body.isEmpty ? nil : body
                )
                if let receipt = envelope.feedbackReceipt {
                    attentionFeedbackReceipts[item.id] = receipt
                }
                mutationCoordinator.succeed(ticket)
                messageFollowUps.removeAll { $0.id == item.id }
                reloadBroadsheetPage(.forYou)
                completion?(true)
            } catch {
                mutationCoordinator.fail(ticket)
                optimisticFollowUpIDs.remove(item.id)
                if let broadsheetIndex, !broadsheetFollowUps.contains(where: { $0.id == item.id }) {
                    broadsheetFollowUps.insert(item, at: min(broadsheetIndex, broadsheetFollowUps.count))
                    broadsheetFollowUpTotal = broadsheetFollowUpTotal.map { $0 + 1 }
                }
                if inDeck, !messageFollowUps.contains(where: { $0.id == item.id }) {
                    let index = originalAnchor.insertionIndex(in: messageFollowUps.map(\.id))
                    messageFollowUps.insert(item, at: index)
                }
                // This transaction decremented the projected total exactly once.
                // A racing reset may already have restored the raw row while
                // deliberately retaining that decrement, so compensate the
                // total regardless of whether insertion was still necessary.
                messageFollowUpTotal += 1
                self.error = Self.errorMessage(error)
                reloadBroadsheetPage(.forYou)
                completion?(false)
            }
        }
    }

    func removeMessageFollowUp(id: String) {
        mutationCoordinator.commit(.channelFollowUp(id))
        optimisticFollowUpIDs.insert(id)
        if broadsheetFollowUps.contains(where: { $0.id == id }) {
            broadsheetFollowUps.removeAll { $0.id == id }
            broadsheetFollowUpTotal = broadsheetFollowUpTotal.map { max(0, $0 - 1) }
            reloadBroadsheetPage(.forYou)
        }
        guard messageFollowUps.contains(where: { $0.id == id }) else { return }
        messageFollowUps.removeAll { $0.id == id }
        messageFollowUpTotal = max(0, messageFollowUpTotal - 1)
    }

    func fetchChannelMessage(_ item: ChannelFollowUp) {
        guard !usesUITestFixture else { return }
        guard actionItemID == nil else { return }
        actionItemID = "message:\(item.id)"
        Task {
            do {
                let value: ChannelMessageView = try await get("/api/magician/v2/channel-assist/annotations/\(item.id)/message")
                channelMessages[item.id] = value
                sectionErrors["message:\(item.id)"] = nil
            } catch { sectionErrors["message:\(item.id)"] = Self.errorMessage(error) }
            actionItemID = nil
        }
    }

    /// Resurfacing has no snooze. A dismiss may carry one of
    /// `ResurfacingDismissOption.allowedCodes`; any other reason is dropped.
    func resolveResurfacing(_ card: ResurfacingCard, action: ResurfacingFeedbackAction, reason: String? = nil,
                            completion: ((Bool) -> Void)? = nil) {
        let mutationKey = "resurfacing:\(card.id)"
        guard let ticket = mutationCoordinator.begin(.resurfacing(card.id)) else { completion?(false); return }
        let inDeck = resurfacingCards.contains(where: { $0.id == card.id })
        let broadsheetIndex = broadsheetWorth.firstIndex(where: { $0.id == card.id })
        guard pendingCardMutationKeys.insert(mutationKey).inserted, inDeck || broadsheetIndex != nil else {
            pendingCardMutationKeys.remove(mutationKey)
            mutationCoordinator.fail(ticket)
            completion?(false)
            return
        }
        if let broadsheetIndex {
            broadsheetWorth.remove(at: broadsheetIndex)
            broadsheetWorthTotal = broadsheetWorthTotal.map { max(0, $0 - 1) }
        }
        let originalAnchor = rollbackOrderLedger.begin(
            listKey: "resurfacing",
            itemID: card.id,
            currentIDs: resurfacingCards.map(\.id)
        )
        pendingResurfacingIDs.insert(card.id)
        optimisticResurfacingIDs.insert(card.id)
        resurfacingCards.removeAll { $0.id == card.id }
        resurfacingTotal = max(0, resurfacingTotal - 1)
        error = nil
        Task { @MainActor in
            defer {
                pendingResurfacingIDs.remove(card.id)
                pendingCardMutationKeys.remove(mutationKey)
                rollbackOrderLedger.finish(listKey: "resurfacing")
            }
            do {
                var body: [String: Any] = [
                    "action": action.rawValue,
                    "event_id": UUID().uuidString
                ]
                if action == .dismiss, let reason, ResurfacingDismissOption.allowedCodes.contains(reason) {
                    body["reason"] = reason
                }
                if let attribution = card.feedbackAttribution {
                    body["attribution"] = attribution.jsonObject
                }
                let envelope: AttentionFeedbackEnvelope = try await postDecoding(
                    "/api/magician/v2/channel-assist/resurfacing/\(card.id)/action",
                    body: body
                )
                if let receipt = envelope.feedbackReceipt {
                    attentionFeedbackReceipts[card.id] = receipt
                }
                mutationCoordinator.succeed(ticket)
                resurfacingCards.removeAll { $0.id == card.id }
                reloadBroadsheetPage(.worth)
                completion?(true)
            } catch {
                mutationCoordinator.fail(ticket)
                optimisticResurfacingIDs.remove(card.id)
                if let broadsheetIndex, !broadsheetWorth.contains(where: { $0.id == card.id }) {
                    broadsheetWorth.insert(card, at: min(broadsheetIndex, broadsheetWorth.count))
                    broadsheetWorthTotal = broadsheetWorthTotal.map { $0 + 1 }
                }
                if inDeck, !resurfacingCards.contains(where: { $0.id == card.id }) {
                    let index = originalAnchor.insertionIndex(in: resurfacingCards.map(\.id))
                    resurfacingCards.insert(card, at: index)
                }
                resurfacingTotal += 1
                self.error = Self.errorMessage(error)
                reloadBroadsheetPage(.worth)
                completion?(false)
            }
        }
    }

    func dismissResurfacing(_ card: ResurfacingCard) {
        resolveResurfacing(card, action: .dismiss)
    }

    func fetchResurfacingDetail(_ card: ResurfacingCard, original: Bool = false) {
        guard !usesUITestFixture else { return }
        actionItemID = "resurfacing:\(card.id)"
        Task {
            do {
                let suffix = original ? "original" : "detail"
                let detail: ResurfacingDetail = try await get("/api/magician/v2/channel-assist/resurfacing/\(card.id)/\(suffix)")
                resurfacingDetails[card.id] = detail
                sectionErrors["resurfacing:\(card.id)"] = nil
                if !original, let recommendation = detail.recommendedAction ?? card.recommendedAction {
                    try? await post("/api/magician/v2/channel-assist/resurfacing/\(card.id)/recommendation-event",
                                    body: ["kind": recommendation.kind.rawValue,
                                           "content_revision": recommendation.contentRevision ?? NSNull(),
                                           "event": "presented"])
                }
            } catch { sectionErrors["resurfacing:\(card.id)"] = Self.errorMessage(error) }
            actionItemID = nil
        }
    }

    func performResurfacingAction(_ card: ResurfacingCard, kind: ResurfacingActionKind,
                                  input: [String: Any] = [:],
                                  idempotencyKey: String = UUID().uuidString,
                                  completion: ((Result<ResurfacingActionResult, Error>) -> Void)? = nil) {
        let key = "resurfacing-action:\(card.id):\(kind.rawValue)"
        actionItemID = key
        Task {
            do {
                let revision = resurfacingDetails[card.id]?.contentRevision ?? card.contentRevision
                let recommended = resurfacingDetails[card.id]?.recommendedAction ?? card.recommendedAction
                if recommended?.kind == kind {
                    try? await post("/api/magician/v2/channel-assist/resurfacing/\(card.id)/recommendation-event",
                                    body: ["kind": kind.rawValue, "content_revision": revision ?? NSNull(), "event": "selected"])
                }
                let body: [String: Any] = [
                    "kind": kind.rawValue,
                    "idempotency_key": idempotencyKey,
                    "content_revision": revision ?? NSNull(),
                    "input": input
                ]
                let result: ResurfacingActionResult = try await postDecoding(
                    "/api/magician/v2/channel-assist/resurfacing/\(card.id)/actions", body: body)
                resurfacingActionResults[card.id] = result
                if let recommendation = recommended,
                   recommendation.kind == kind {
                    try? await post("/api/magician/v2/channel-assist/resurfacing/\(card.id)/recommendation-event",
                                    body: ["kind": kind.rawValue, "content_revision": revision ?? NSNull(), "event": "completed"])
                }
                sectionErrors[key] = nil
                fetchAfterAction()
                completion?(.success(result))
            } catch {
                sectionErrors[key] = Self.errorMessage(error)
                completion?(.failure(error))
            }
            actionItemID = nil
        }
    }

    func loadDigest(offset: Int) {
        guard !isDigestLoading else { return }
        isDigestLoading = true
        Task {
            do {
                let next: TodayResponse = try await get("/api/magician/v2/today", query: [
                    "per_section": "8",
                    "digest_limit": String(Self.digestPageSize), "digest_offset": String(max(0, offset))
                ])
                if let current = payload {
                    payload = TodayResponse(generatedAt: next.generatedAt, headline: current.headline,
                                            digest: next.digest, sections: current.sections,
                                            counts: current.counts, sectionPage: current.sectionPage)
                }
                digestOffset = max(0, offset)
                sectionErrors["digest"] = nil
            } catch { sectionErrors["digest"] = Self.errorMessage(error) }
            isDigestLoading = false
        }
    }

    func fetchBriefingRender(_ briefing: TodayBriefing) {
        guard !usesUITestFixture else { return }
        actionItemID = "briefing:\(briefing.id)"
        Task {
            do {
                let envelope: TodayBriefingRenderEnvelope = try await get("/api/magician/v3/published-surfaces/\(briefing.id)/render")
                briefingRenders[briefing.id] = envelope.render
                sectionErrors["briefing:\(briefing.id)"] = nil
            } catch { sectionErrors["briefing:\(briefing.id)"] = Self.errorMessage(error) }
            actionItemID = nil
        }
    }

    func loadAllBriefings() {
        guard actionItemID == nil else { return }
        actionItemID = "briefings"
        Task {
            do {
                let page: TodayBriefingsResponse = try await get("/api/magician/v3/published-surfaces/projections", query: [
                    "route": "/briefing", "limit": "50"
                ])
                briefings = page.surfaces
                sectionErrors["briefings"] = nil
            } catch { sectionErrors["briefings"] = Self.errorMessage(error) }
            actionItemID = nil
        }
    }

    private func fetchAfterAction() {
        guard refreshesAfterActions else { return }
        isLoading = false
        fetch()
    }

    private func restoreExpiredProjection(for key: OptimisticCardKey) {
        // A same-key reissue may already own the projection when an old expiry
        // is observed; never compensate across transaction generations.
        guard !mutationCoordinator.isSuppressed(key) else { return }
        switch key {
        case .channelFollowUp(let id):
            guard optimisticFollowUpIDs.remove(id) != nil,
                  messageFollowUps.contains(where: { $0.id == id }) else { return }
            messageFollowUpTotal += 1
        case .resurfacing(let id):
            guard optimisticResurfacingIDs.remove(id) != nil,
                  resurfacingCards.contains(where: { $0.id == id }) else { return }
            resurfacingTotal += 1
        case .today(let id):
            optimisticTodaySections[id] = nil
        default:
            break
        }
    }

    private func removeItemLocally(_ item: TodayItem) {
        guard var current = payload else { return }
        var sections = current.sections
        let existed: Bool
        switch item.section {
        case TodaySection.needsYou.rawValue:
            existed = sections.needsYou.contains { $0.id == item.id }; sections.needsYou.removeAll { $0.id == item.id }
        case TodaySection.followups.rawValue:
            existed = sections.followups.contains { $0.id == item.id }; sections.followups.removeAll { $0.id == item.id }
        case TodaySection.activeWork.rawValue:
            existed = sections.activeWork.contains { $0.id == item.id }; sections.activeWork.removeAll { $0.id == item.id }
        case TodaySection.delivered.rawValue:
            existed = sections.delivered.contains { $0.id == item.id }; sections.delivered.removeAll { $0.id == item.id }
        default:
            existed = sections.changed.contains { $0.id == item.id }; sections.changed.removeAll { $0.id == item.id }
        }
        guard existed else { return }
        var nextCounts = current.counts
        switch item.section {
        case TodaySection.needsYou.rawValue: nextCounts.needsYou = max(0, nextCounts.needsYou - 1)
        case TodaySection.followups.rawValue: nextCounts.followups = max(0, nextCounts.followups - 1)
        case TodaySection.activeWork.rawValue: nextCounts.activeWork = max(0, nextCounts.activeWork - 1)
        case TodaySection.delivered.rawValue: nextCounts.delivered = max(0, nextCounts.delivered - 1)
        default: nextCounts.changed = max(0, nextCounts.changed - 1)
        }
        nextCounts.total = max(0, nextCounts.total - 1)
        current = TodayResponse(generatedAt: current.generatedAt, headline: current.headline,
                                digest: current.digest, sections: sections, counts: nextCounts,
                                sectionPage: current.sectionPage)
        payload = current
    }

    private func anchorForTodayItem(_ item: TodayItem) -> OptimisticCardListAnchor {
        guard let sections = payload?.sections else {
            return OptimisticCardListAnchor(ids: [], index: 0)
        }
        let values: [TodayItem]
        switch item.section {
        case TodaySection.needsYou.rawValue: values = sections.needsYou
        case TodaySection.followups.rawValue: values = sections.followups
        case TodaySection.activeWork.rawValue: values = sections.activeWork
        case TodaySection.delivered.rawValue: values = sections.delivered
        default: values = sections.changed
        }
        return rollbackOrderLedger.begin(
            listKey: "today:\(item.section)",
            itemID: item.id,
            currentIDs: values.map(\.id)
        )
    }

    private func insertItemLocally(_ item: TodayItem, at anchor: OptimisticCardListAnchor) {
        guard var current = payload else { return }
        var sections = current.sections
        var nextCounts = current.counts
        func restored(_ values: [TodayItem]) -> [TodayItem] {
            guard !values.contains(where: { $0.id == item.id }) else { return values }
            var next = values
            let index = anchor.insertionIndex(in: values.map(\.id))
            next.insert(item, at: index)
            return next
        }
        let existed: Bool
        switch item.section {
        case TodaySection.needsYou.rawValue:
            existed = sections.needsYou.contains { $0.id == item.id }
            sections.needsYou = restored(sections.needsYou); if !existed { nextCounts.needsYou += 1 }
        case TodaySection.followups.rawValue:
            existed = sections.followups.contains { $0.id == item.id }
            sections.followups = restored(sections.followups); if !existed { nextCounts.followups += 1 }
        case TodaySection.activeWork.rawValue:
            existed = sections.activeWork.contains { $0.id == item.id }
            sections.activeWork = restored(sections.activeWork); if !existed { nextCounts.activeWork += 1 }
        case TodaySection.delivered.rawValue:
            existed = sections.delivered.contains { $0.id == item.id }
            sections.delivered = restored(sections.delivered); if !existed { nextCounts.delivered += 1 }
        default:
            existed = sections.changed.contains { $0.id == item.id }
            sections.changed = restored(sections.changed); if !existed { nextCounts.changed += 1 }
        }
        if !existed { nextCounts.total += 1 }
        current = TodayResponse(generatedAt: current.generatedAt, headline: current.headline,
                                digest: current.digest, sections: sections, counts: nextCounts,
                                sectionPage: current.sectionPage)
        payload = current
    }

    private func mergeSection(_ section: TodaySection, with items: [TodayItem], counts: TodayCounts) {
        guard let current = payload else { return }
        var sections = current.sections
        let visibleIncoming = items.filter {
            !mutationCoordinator.isSuppressed(.today($0.id))
        }
        switch section {
        case .needsYou: sections.needsYou = Self.merging(sections.needsYou, visibleIncoming)
        case .followups: sections.followups = Self.merging(sections.followups, visibleIncoming)
        case .activeWork: sections.activeWork = Self.merging(sections.activeWork, visibleIncoming)
        case .delivered: sections.delivered = Self.merging(sections.delivered, visibleIncoming)
        case .changed: sections.changed = Self.merging(sections.changed, visibleIncoming)
        case .worthALook: return
        }
        let projectedCounts = preservingOptimisticTodayCounts(
            incoming: counts,
            current: current.counts
        )
        payload = TodayResponse(generatedAt: current.generatedAt, headline: current.headline,
                                digest: current.digest, sections: sections, counts: projectedCounts,
                                sectionPage: current.sectionPage)
    }

    private func preservingOptimisticTodayCounts(incoming: TodayCounts,
                                                  current: TodayCounts) -> TodayCounts {
        let activeSections = Set(optimisticTodaySections.compactMap { itemID, section in
            mutationCoordinator.isSuppressed(.today(itemID)) ? section : nil
        })
        optimisticTodaySections = optimisticTodaySections.filter { itemID, _ in
            mutationCoordinator.isSuppressed(.today(itemID))
        }
        guard !activeSections.isEmpty else { return incoming }

        var projected = incoming
        if activeSections.contains(.needsYou) { projected.needsYou = current.needsYou }
        if activeSections.contains(.followups) { projected.followups = current.followups }
        if activeSections.contains(.activeWork) { projected.activeWork = current.activeWork }
        if activeSections.contains(.delivered) { projected.delivered = current.delivered }
        if activeSections.contains(.changed) { projected.changed = current.changed }
        projected.total = current.total
        return projected
    }

    private static func merging<T: Identifiable>(_ existing: [T], _ incoming: [T]) -> [T] where T.ID: Hashable {
        var seen = Set<T.ID>()
        return (existing + incoming).filter { seen.insert($0.id).inserted }
    }

    private func snapshotDictionary(_ item: TodayItem) -> [String: Any] {
        var value: [String: Any] = [
            "title": item.title, "reason": item.reason, "section": item.section,
            "source_kind": item.sourceKind, "source_id": item.sourceID,
            "space_ids": item.spaceIDs, "item_updated_at": item.updatedAt
        ]
        if let summary = item.summary { value["summary"] = summary }
        if let sourceURL = item.sourceURL { value["source_url"] = sourceURL }
        return value
    }

    private func get<T: Decodable>(_ path: String, query: [String: String] = [:]) async throws -> T {
        var components = URLComponents(url: baseURL.appendingPathComponent(path), resolvingAgainstBaseURL: false)!
        var allQuery = query
        // Every `/today` predicate — due today, changed since, overdue — is a
        // date comparison, and the server cannot know where the reader is: left
        // to its own clock it answers from the UTC date, which names the wrong
        // day for part of every day anywhere east of Greenwich. Attached in the
        // one place all three `/today` reads pass through, so
        // the preview fetch, section paging and digest paging cannot end up
        // disagreeing about which day they asked about.
        if path == "/api/magician/v2/today" && allQuery["today"] == nil {
            allQuery["today"] = readerLocalDate()
        }
        components.queryItems = allQuery.sorted { $0.key < $1.key }.map(URLQueryItem.init)
        var request = URLRequest(url: components.url!)
        MagicianAccess.authorize(&request)
        let (data, response) = try await networkSession.data(for: request)
        try Self.validate(response: response, data: data)
        return try JSONDecoder().decode(T.self, from: data)
    }

    private func post(_ path: String, body: [String: Any]?) async throws {
        let _: EmptyResponse = try await postDecoding(path, body: body)
    }

    private struct EmptyResponse: Decodable {}

    private func postDecoding<T: Decodable>(_ path: String, body: [String: Any]?) async throws -> T {
        var request = URLRequest(url: baseURL.appendingPathComponent(path))
        request.httpMethod = "POST"
        if let body {
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            request.httpBody = try JSONSerialization.data(withJSONObject: body)
        }
        MagicianAccess.authorize(&request)
        let (data, response) = try await networkSession.data(for: request)
        try Self.validate(response: response, data: data)
        if T.self == EmptyResponse.self { return EmptyResponse() as! T }
        return try JSONDecoder().decode(T.self, from: data)
    }

    func removeActivity(_ item: TodayActivityItem) {
        activityItems.removeAll { $0.id == item.id }
        Task {
            do { try await delete("/api/magician/v2/feed/items/\(item.id)") }
            catch { self.error = Self.errorMessage(error); refreshActivity() }
        }
    }

    func clearActivity() {
        let previous = activityItems
        activityItems = []
        Task {
            var failed: [TodayActivityItem] = []
            for item in previous {
                do { try await delete("/api/magician/v2/feed/items/\(item.id)") }
                catch { failed.append(item); self.error = Self.errorMessage(error) }
            }
            if !failed.isEmpty { activityItems = failed }
        }
    }

    private func delete(_ path: String, query: [String: String] = [:]) async throws {
        var components = URLComponents(url: baseURL.appendingPathComponent(path), resolvingAgainstBaseURL: false)!
        components.queryItems = query.sorted { $0.key < $1.key }.map(URLQueryItem.init)
        var request = URLRequest(url: components.url!); request.httpMethod = "DELETE"
        MagicianAccess.authorize(&request)
        let (data, response) = try await networkSession.data(for: request)
        try Self.validate(response: response, data: data)
    }

    private func postQuery(_ path: String, sql: String) async throws -> PulseQueryResponse {
        let url = baseURL.appendingPathComponent(path)
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try JSONSerialization.data(withJSONObject: ["sql": sql])
        MagicianAccess.authorize(&request)
        let (data, response) = try await networkSession.data(for: request)
        try Self.validate(response: response, data: data)
        return try JSONDecoder().decode(PulseQueryResponse.self, from: data)
    }

    private func fetchPulse(now: Date = Date(), calendar: Calendar = .current) async throws -> TodayPulse {
        let boundaries = Self.localDayBoundaries(now: now, calendar: calendar)
        async let llm = postQuery("/api/magician/v2/analytics/llm_calls/query", sql: Self.llmPulseSQL(boundaries))
        async let coding = postQuery("/api/magician/v2/analytics/query", sql: Self.codingPulseSQL(boundaries))
        async let memory = postQuery("/api/magician/v2/analytics/memory_events/query", sql: Self.memoryPulseSQL(boundaries))
        async let tasks: PulseTaskPage = get("/api/magician/v3/tasks")
        async let agents: TodayAgentsResponse = get("/api/magician/v2/agents")
        let crewSince = Int64(now.timeIntervalSince1970 * 1_000) - TodayMorningEdition.crewWindowMS
        async let crewLLM = captured {
            try await self.postQuery("/api/magician/v2/analytics/llm_calls/query",
                                     sql: TodayMorningEdition.crewSQL(since: crewSince))
        }
        async let crewTasks = captured { try await self.fetchCrewTasks(since: crewSince) }

        var result = pulse ?? TodayPulse()
        var successfulSlices = 0
        if let response = try? await llm {
            result.spendToday = 0; result.spendYesterday = 0; result.callsToday = 0; result.callsYesterday = 0
            result.hourlySpend = Array(repeating: 0, count: 24); result.hourlyCalls = Array(repeating: 0, count: 24)
            result.topModel = nil
            Self.applyLLMPulse(response, to: &result); successfulSlices += 1
        }
        if let response = try? await coding { result.codingRunsToday = Int(Self.cell(response, row: 0, column: "n").number); successfulSlices += 1 }
        if let response = try? await memory {
            result.memoriesToday = 0; result.evalCasesToday = 0; result.evalPassesToday = 0
            Self.applyMemoryPulse(response, to: &result); successfulSlices += 1
        }
        if let taskPage = try? await tasks {
            result.tasksCompletedToday = 0; result.tasksCompletedYesterday = 0
            for task in taskPage.tasks where task.status == "completed" {
                guard let raw = task.updatedAt, let date = ISO8601DateFormatter().date(from: raw) else { continue }
                let milliseconds = Int64(date.timeIntervalSince1970 * 1_000)
                if milliseconds >= boundaries.today && milliseconds < boundaries.tomorrow { result.tasksCompletedToday += 1 }
                else if milliseconds >= boundaries.yesterday && milliseconds < boundaries.today { result.tasksCompletedYesterday += 1 }
            }
            // Fleet panel buckets (web TodayNewspaperLedger): succeeded never
            // reads below the completed-today pulse count.
            let statuses = taskPage.tasks.map(\.status)
            result.tasksSucceeded = max(statuses.filter { $0 == "completed" }.count, result.tasksCompletedToday)
            result.tasksFailed = statuses.filter { $0 == "failed" }.count
            result.tasksInFlight = statuses.filter { ["running", "paused", "planning"].contains($0) }.count
            // State of Operations task list: the same read, newest first.
            result.recentTasks = TodayMorningEdition.recentTasks(taskPage.tasks.compactMap { task in
                guard let id = task.taskID, !id.isEmpty else { return nil }
                return TodayRecentTask(
                    id: id,
                    title: TodayMorningEdition.firstNonEmpty(task.title) ?? "Untitled task",
                    status: task.status,
                    updatedAt: task.updatedAt.flatMap(TodayMorningEdition.epochMilliseconds(fromISO:)) ?? 0
                )
            })
        }
        // Fail-soft: a missing agents list keeps the last known counts.
        let agentPage = try? await agents
        if let agentPage {
            result.agents = TodayMorningEdition.agentCounts(agentPage.agents)
        }
        // State of the Crew needs all three reads; otherwise it reports an
        // error and keeps the last good crew.
        switch (agentPage, await crewLLM, await crewTasks) {
        case (let agentPage?, .success(let llm), .success(let taskRows)):
            let rows = llm.rows.indices.compactMap { row -> TodayCrewLLMRow? in
                let id = Self.cell(llm, row: row, column: "agent_id").text.trimmingCharacters(in: .whitespaces)
                guard !id.isEmpty else { return nil }
                return TodayCrewLLMRow(agentID: id,
                                       calls: Int(Self.cell(llm, row: row, column: "calls").number),
                                       costUSD: Self.cell(llm, row: row, column: "cost_usd").number,
                                       okCalls: Int(Self.cell(llm, row: row, column: "ok_calls").number))
            }
            result.crew = TodayMorningEdition.crewSummary(agents: agentPage.agents, llm: rows,
                                                          tasks: taskRows, since: crewSince)
            result.crewError = nil
        case (_, .failure(let failure), _), (_, _, .failure(let failure)):
            result.crewError = Self.errorMessage(failure)
        default:
            result.crewError = "The crew roster is temporarily unavailable."
        }
        if successfulSlices == 0 { throw NSError(domain: "TodayPulse", code: -1, userInfo: [NSLocalizedDescriptionKey: "Today's pulse is temporarily unavailable."]) }
        return result
    }

    /// `/v3/tasks` newest-first pages for the crew window (max 5 pages).
    private func fetchCrewTasks(since: Int64) async throws -> [TodayCrewTaskRow] {
        var rows: [TodayCrewTaskRow] = []
        var cursor: String?
        var pages = 0
        while true {
            var query = ["limit": String(TodayMorningEdition.crewTaskPageLimit), "sort": "updated_at", "order": "desc"]
            if let cursor { query["cursor"] = cursor }
            let page: CrewTaskPage = try await get("/api/magician/v3/tasks", query: query)
            pages += 1
            let mapped = page.tasks.map { TodayCrewTaskRow(agentID: $0.agentID, status: $0.status, updatedAt: $0.updatedAt) }
            rows += mapped
            guard TodayMorningEdition.shouldFetchNextCrewTaskPage(lastRowUpdatedAt: mapped.last?.updatedAt, since: since,
                                                                  nextCursor: page.pagination?.nextCursor,
                                                                  pagesFetched: pages) else { break }
            cursor = page.pagination?.nextCursor
        }
        return rows
    }

    private typealias DayBoundaries = (yesterday: Int64, today: Int64, tomorrow: Int64)
    private static func localDayBoundaries(now: Date, calendar: Calendar) -> DayBoundaries {
        let today = calendar.startOfDay(for: now)
        let yesterday = calendar.date(byAdding: .day, value: -1, to: today)!
        let tomorrow = calendar.date(byAdding: .day, value: 1, to: today)!
        return (Int64(yesterday.timeIntervalSince1970 * 1_000), Int64(today.timeIntervalSince1970 * 1_000), Int64(tomorrow.timeIntervalSince1970 * 1_000))
    }

    private static func llmPulseSQL(_ b: DayBoundaries) -> String {
        "SELECT 'today_hour' AS section, CAST(CAST(FLOOR((timestamp_ms - \(b.today)) / 3600000.0) AS INTEGER) AS VARCHAR) AS k, NULL::VARCHAR AS model, COALESCE(SUM(cost_usd), 0) AS v1, COUNT(*) AS v2 FROM llm_calls WHERE timestamp_ms >= \(b.today) AND timestamp_ms < \(b.tomorrow) GROUP BY 2 UNION ALL SELECT 'today_total', 'all', NULL::VARCHAR, COALESCE(SUM(cost_usd), 0), COUNT(*) FROM llm_calls WHERE timestamp_ms >= \(b.today) AND timestamp_ms < \(b.tomorrow) UNION ALL SELECT 'yesterday_total', 'all', NULL::VARCHAR, COALESCE(SUM(cost_usd), 0), COUNT(*) FROM llm_calls WHERE timestamp_ms >= \(b.yesterday) AND timestamp_ms < \(b.today) UNION ALL SELECT 'today_provider', COALESCE(NULLIF(provider, ''), 'unknown'), COALESCE(NULLIF(model, ''), 'unknown'), COALESCE(SUM(cost_usd), 0), COUNT(*) FROM llm_calls WHERE timestamp_ms >= \(b.today) AND timestamp_ms < \(b.tomorrow) GROUP BY 2, 3"
    }

    private static func codingPulseSQL(_ b: DayBoundaries) -> String {
        "SELECT 'coding_runs_today' AS section, COUNT(*) AS n FROM events WHERE event_type = 'coding.started' AND source = 'coding_engine' AND epoch_ms(timestamp) >= \(b.today) AND epoch_ms(timestamp) < \(b.tomorrow)"
    }

    private static func memoryPulseSQL(_ b: DayBoundaries) -> String {
        "SELECT 'memories_today' AS section, CAST(COUNT(*) AS DOUBLE) AS n, CAST(0 AS DOUBLE) AS passes FROM memory_events WHERE timestamp_ms >= \(b.today) AND timestamp_ms < \(b.tomorrow) AND event_kind IN ('learning_memory_candidate_promoted', 'learning_memory_candidate_review_promoted') UNION ALL SELECT 'evals_today', CAST(COUNT(*) AS DOUBLE), CAST(COALESCE(SUM(CASE WHEN eval_pass THEN 1 ELSE 0 END), 0) AS DOUBLE) FROM memory_events WHERE timestamp_ms >= \(b.today) AND timestamp_ms < \(b.tomorrow) AND event_kind = 'eval_case'"
    }

    private static func cell(_ response: PulseQueryResponse, row: Int, column: String) -> PulseCell {
        guard row < response.rows.count, let index = response.columns.firstIndex(of: column), index < response.rows[row].count else { return .null }
        return response.rows[row][index]
    }

    private static func applyLLMPulse(_ response: PulseQueryResponse, to pulse: inout TodayPulse) {
        var providers: [(String, String, Double)] = []
        for row in response.rows.indices {
            switch cell(response, row: row, column: "section").text {
            case "today_hour":
                let hour = Int(cell(response, row: row, column: "k").number)
                if resultRange24.contains(hour) {
                    pulse.hourlySpend[hour] += cell(response, row: row, column: "v1").number
                    pulse.hourlyCalls[hour] += Int(cell(response, row: row, column: "v2").number)
                }
            case "today_total":
                pulse.spendToday = cell(response, row: row, column: "v1").number
                pulse.callsToday = Int(cell(response, row: row, column: "v2").number)
            case "yesterday_total":
                pulse.spendYesterday = cell(response, row: row, column: "v1").number
                pulse.callsYesterday = Int(cell(response, row: row, column: "v2").number)
            case "today_provider":
                providers.append((cell(response, row: row, column: "k").text,
                                  cell(response, row: row, column: "model").text,
                                  cell(response, row: row, column: "v2").number))
            default: break
            }
        }
        let total = providers.reduce(0) { $0 + $1.2 }
        if let top = providers.max(by: { $0.2 < $1.2 }), total > 0 {
            pulse.topModel = TodayPulse.TopModel(provider: top.0, model: top.1, share: top.2 / total)
        }
    }

    private static let resultRange24 = 0..<24

    private static func applyMemoryPulse(_ response: PulseQueryResponse, to pulse: inout TodayPulse) {
        for row in response.rows.indices {
            switch cell(response, row: row, column: "section").text {
            case "memories_today": pulse.memoriesToday = Int(cell(response, row: row, column: "n").number)
            case "evals_today":
                pulse.evalCasesToday = Int(cell(response, row: row, column: "n").number)
                pulse.evalPassesToday = Int(cell(response, row: row, column: "passes").number)
            default: break
            }
        }
    }

    private static func validate(response: URLResponse, data: Data) throws {
        guard let http = response as? HTTPURLResponse, (200..<300).contains(http.statusCode) else {
            let message = String(data: data, encoding: .utf8) ?? "Today is unavailable."
            throw NSError(domain: "Today", code: (response as? HTTPURLResponse)?.statusCode ?? -1,
                          userInfo: [NSLocalizedDescriptionKey: message])
        }
    }

    private static func errorMessage(_ error: Error) -> String {
        let value = error.localizedDescription.trimmingCharacters(in: .whitespacesAndNewlines)
        return value.isEmpty ? "Today is unavailable." : value
    }

    private static func isDurableActivity(_ item: TodayActivityItem) -> Bool {
        if ["agent_learning", "data_delivery", "routine_result"].contains(item.itemType) { return true }
        guard item.itemType == "task" else { return false }
        guard item.status == "done" else { return item.status == "failed" }
        if !(item.summary ?? "").trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { return true }
        if item.metadataString("completion_outcome") != nil { return true }
        guard let artifacts = item.metadata.objectValue?["completion_artifact_names"] else { return false }
        if let text = artifacts.stringValue { return !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }
        return artifacts.arrayValue?.isEmpty == false
    }

    private func refreshMessageFollowUps() {
        Task {
            let result: Result<ChannelFollowUpPage, Error> = await captured {
                try await self.get("/api/magician/v2/channel-assist/follow-ups", query: ["limit": "5"])
            }
            applyFollowUps(result)
        }
    }

    private func refreshPulse() {
        Task { applyPulse(await captured { try await self.fetchPulse() }) }
    }

    private func refreshActivity() {
        Task {
            let result: Result<TodayActivityResponse, Error> = await captured {
                try await self.get("/api/magician/v2/feed", query: ["limit": "80"])
            }
            applyActivity(result)
        }
    }

    private func connectRealtime() {
        guard webSocket == nil else { return }
        var components = URLComponents(url: baseURL.appendingPathComponent("/api/magician/v2/realtime/ws"),
                                       resolvingAgainstBaseURL: false)
        components?.scheme = baseURL.scheme == "http" ? "ws" : "wss"
        guard let url = components?.url else { return }
        var request = URLRequest(url: url); MagicianAccess.authorize(&request)
        let socket = networkSession.webSocketTask(with: request)
        webSocket = socket; socket.resume(); receiveRealtime()
    }

    private func receiveRealtime() {
        webSocket?.receive { [weak self] result in
            guard let self else { return }
            switch result {
            case .success(let message):
                let text: String
                switch message { case .string(let value): text = value; case .data(let value): text = String(data: value, encoding: .utf8) ?? ""; @unknown default: text = "" }
                Task { @MainActor in
                    self.receiveWireEvent(text)
                    if Self.isRelevantRealtimeEvent(text) { self.scheduleRealtimeRefresh() }
                }
                Task { @MainActor in self.receiveRealtime() }
            case .failure:
                Task { @MainActor in
                    guard self.started else { return }
                    self.webSocket = nil
                    DispatchQueue.main.asyncAfter(deadline: .now() + 2) { [weak self] in self?.connectRealtime() }
                }
            }
        }
    }

    /// Every live event frame lands on the Realtime Wire as an `EVENT` line
    /// and bumps the 24h count. `AgentEvent` envelopes are unwrapped to their
    /// inner type (most frames are agent events, which the Today refresh
    /// filter ignores); control frames and streaming deltas are skipped.
    func receiveWireEvent(_ text: String, now: Date = Date()) {
        wireSequence += 1
        guard let item = TodayMorningEdition.wireItem(fromRealtimeText: text, sequence: wireSequence, now: now) else { return }
        wireItems = TodayMorningEdition.mergingWireItems(wireItems, [item])
        wireEventCount24h += 1
    }

    private func scheduleRealtimeRefresh() {
        realtimeRefresh?.cancel()
        let work = DispatchWorkItem { [weak self] in self?.fetch() }
        realtimeRefresh = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.75, execute: work)
    }

    nonisolated static func isRelevantRealtimeEvent(_ text: String) -> Bool {
        guard let data = text.data(using: .utf8),
              let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return false }
        let type = ((root["event_type"] as? String) ?? (root["type"] as? String) ?? "").lowercased()
        return ["today", "feed", "task", "planning", "execution", "learning", "published", "surface",
                "attention", "approval", "channel", "follow_up", "resurfacing"].contains { type.contains($0) }
    }
}

private extension TodayViewModel {
    static let uiTestTodayData = Data(#"""
    {"generated_at":1783900800000,"headline":"A few things are ready for you.",
     "digest":{"generated_at":1783900800000,"total":8,"limit":7,"offset":0,"bullets":[
       {"id":"digest-1","text":"The travel policy changed","source_kind":"memory_learning","source_id":"policy","space_ids":["operations"],"updated_at":1783900800000}]},
     "sections":{
       "needs_you":[{"id":"needs-1","section":"needs_you","priority":10,"title":"Approval needed","summary":"Review the launch plan.","reason":"The agent is waiting for your decision.","source_kind":"approval","source_id":"pause-1","space_ids":["launch"],"status":"needs_action","created_at":1783900800000,"updated_at":1783900800000,"metadata":{"pause_state_id":"pause-1"}}],
       "followups":[{"id":"follow-1","section":"followups","priority":5,"title":"Follow up with design","summary":"The handoff is due today.","reason":"Due today","source_kind":"task","source_id":"task-follow","space_ids":["launch"],"status":"info","created_at":1783900800000,"updated_at":1783900800000}],
       "active_work":[{"id":"active-1","section":"active_work","priority":4,"title":"Preparing release notes","summary":"Drafting the final release summary.","reason":"In progress","source_kind":"task","source_id":"task-active","task_id":"task-active","space_ids":["launch"],"status":"running","created_at":1783900800000,"updated_at":1783900800000}],
       "delivered":[{"id":"delivered-1","section":"delivered","priority":3,"title":"Morning briefing delivered","summary":"Your daily briefing is ready.","reason":"Published","source_kind":"published_surface","source_id":"brief-1","space_ids":[],"status":"done","created_at":1783900800000,"updated_at":1783900800000}],
       "changed":[{"id":"changed-1","section":"changed","priority":2,"title":"Preference learned","summary":"Concise status updates are preferred.","reason":"New memory","source_kind":"memory_learning","source_id":"memory-1","space_ids":[],"status":"info","created_at":1783900800000,"updated_at":1783900800000}]},
     "counts":{"needs_you":1,"followups":1,"active_work":1,"delivered":1,"changed":1,"total":5}}
    """#.utf8)

    static let uiTestHiddenData = Data(#"""
    {"items":[{"item_id":"hidden-1","hidden_kind":"dismissed","record":{"dismissed_at":1783900800000,
      "snapshot":{"title":"Deferred review","summary":"Not needed this morning.","reason":"Later","section":"changed","source_kind":"memory","source_id":"memory-hidden","space_ids":[],"item_updated_at":1783900800000}}}]}
    """#.utf8)

    static let uiTestResurfacingData = Data(#"""
    {"cards":[{"candidate_id":"resurface-1","line":"Review travel policy","why_now":"It becomes effective tomorrow","source_title":"Travel policy","summary":"The reimbursement limit increased.","source_kind":"memory","source_ref":"policy:1","brief_status":"v2","brief":{"schema_version":2,"key_facts":["Limit is now $100"],"changes":[{"aspect":"Limit","before":"$75","after":"$100"}],"temporal_facts":[],"detail_status":"complete","missing_details":[]},"actions":[{"kind":"create_task","label":"Create task","requires_input":true,"side_effect":"creates_task"}]},
      {"candidate_id":"resurface-2","line":"Vendor contract renewal","why_now":"Renewal window opens next week","source_title":"Vendor contract notes","summary":"Last year's renewal terms and the discount we negotiated.","source_kind":"project_note","source_ref":"note:2"}],"total":2}
    """#.utf8)

    static let uiTestFollowUpsData = Data(#"""
    {"items":[{"annotation_id":"message-1","provider":"gmail","account_alias":"work","account_email":"sam@example.com","thread_id":"thread-1","lane":"user_assist","label":"needs_reply","reason":"A direct question needs a response.","subject":"Client reply needed","sender":"Alex","summary":"Confirm the launch time.","received_at":1783900800000,"created_at":1783900800000,"open_url":"https://mail.google.com/mail/u/0/#inbox/thread-1","proposed_action":{"follow_up_kind":"reply","action_owner":"me","due_text":"Today","urgency":"high"}},
      {"annotation_id":"message-2","provider":"slack","account_alias":"team","thread_id":"thread-2","lane":"user_assist","label":"needs_reply","reason":"Asked for a review.","subject":"Review the pricing page copy","sender":"Priya","summary":"Priya wants a yes/no on the new headline before noon.","received_at":1783900700000,"created_at":1783900700000}],"total":2}
    """#.utf8)

    static let uiTestActivityData = Data(#"""
    {"items":[{"id":"activity-1","item_type":"agent_learning","title":"Learned writing preference","summary":"Use short paragraphs.","status":"info","updated_at":1783900800000,"created_at":1783900800000,"metadata":{"memory_id":"memory-1"}},
      {"id":"activity-2","item_type":"task","task_id":"task-complete","title":"Release checklist completed","summary":"All checks passed.","status":"done","updated_at":1783900800000,"created_at":1783900800000,"metadata":{"completion_outcome":"success"}}]}
    """#.utf8)

    static let uiTestAgentUpdatesData = Data(#"""
    {"events":[{"id":"update-1","agent_id":"presto","kind":"cycle_completed","ts":1783900700000,"reason":"Morning sweep finished"}]}
    """#.utf8)

    static let uiTestBriefingsData = Data(#"""
    {"surfaces":[{"surface":{"surface_id":"brief-1","route":"/briefing","title":"Morning briefing","summary":"Launch status and priorities.","task_id":"task-brief","published_at":"2026-07-13T00:00:00Z"},"task_title":"Daily briefing","source_agent_id":"presto","render_kind":"markdown","presentation_state":"published"}]}
    """#.utf8)
}

extension TodayViewModel {
    enum SnoozeOption: String, CaseIterable, Identifiable {
        case tonight
        case tomorrowMorning
        case nextWeek

        var id: String { rawValue }

        var title: String {
            switch self {
            case .tonight: return "Until tonight"
            case .tomorrowMorning: return "Tomorrow morning"
            case .nextWeek: return "Next week"
            }
        }

        var systemImage: String { "clock" }
    }

    nonisolated static func snoozeMinutes(for option: SnoozeOption, now: Date = Date(), calendar: Calendar = .current) -> Int {
        var target: Date
        switch option {
        case .tonight:
            target = calendar.date(bySettingHour: 18, minute: 0, second: 0, of: now) ?? now
            if target <= now { return 180 }
        case .tomorrowMorning:
            target = calendar.date(bySettingHour: 8, minute: 0, second: 0, of: now) ?? now
            if target <= now { target = calendar.date(byAdding: .day, value: 1, to: target) ?? target }
        case .nextWeek:
            let weekday = calendar.component(.weekday, from: now)
            let days = (9 - weekday) % 7
            target = calendar.date(byAdding: .day, value: days == 0 ? 7 : days, to: now) ?? now
            target = calendar.date(bySettingHour: 8, minute: 0, second: 0, of: target) ?? target
        }
        return max(1, Int((target.timeIntervalSince(now) / 60).rounded()))
    }

    /// Web `greeting.ts` dayparts — late night reads as evening.
    nonisolated static func greeting(for date: Date, calendar: Calendar = .current) -> String {
        TodayMorningEdition.greeting(for: date, calendar: calendar)
    }

    nonisolated static func relativeTime(_ epochMilliseconds: Int64, now: Date = Date()) -> String {
        guard epochMilliseconds > 0 else { return "just now" }
        let seconds = max(0, Int(now.timeIntervalSince1970) - Int(epochMilliseconds / 1000))
        if seconds < 60 { return "just now" }
        if seconds < 3_600 { return "\(seconds / 60)m ago" }
        if seconds < 172_800 { return "\(seconds / 3_600)h ago" }
        return "\(seconds / 86_400)d ago"
    }

    nonisolated static func localDateTime(_ epochMilliseconds: Int64) -> String {
        guard epochMilliseconds > 0 else { return "" }
        let formatter = DateFormatter(); formatter.dateStyle = .medium; formatter.timeStyle = .short
        return formatter.string(from: Date(timeIntervalSince1970: Double(epochMilliseconds) / 1_000))
    }

    nonisolated static func futureDistance(_ epochMilliseconds: Int64?, now: Date = Date()) -> String {
        guard let epochMilliseconds else { return "until later" }
        let minutes = Int(ceil((Double(epochMilliseconds) / 1_000 - now.timeIntervalSince1970) / 60))
        if minutes <= 0 { return "until now" }
        if minutes < 60 { return "for \(minutes)m" }
        let hours = Int(ceil(Double(minutes) / 60)); if hours < 48 { return "for \(hours)h" }
        return "for \(Int(ceil(Double(hours) / 24)))d"
    }

    nonisolated static func spaceGroups(_ items: [TodayItem]) -> [(id: String, label: String, items: [TodayItem])] {
        let grouped = Dictionary(grouping: items) { item in
            item.spaceIDs.first(where: { !$0.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }) ?? "unfiled"
        }
        return grouped.map { key, values in
            let label = key == "unfiled" ? "Other" : key.replacingOccurrences(of: "_", with: " ").replacingOccurrences(of: "-", with: " ").capitalized
            return (key, label, values)
        }.sorted { left, right in
            if left.id == "unfiled" { return false }; if right.id == "unfiled" { return true }
            return left.label.localizedCaseInsensitiveCompare(right.label) == .orderedAscending
        }
    }
}
