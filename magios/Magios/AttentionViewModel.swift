import Foundation
import Combine

// MARK: - Wire models

struct AttentionInputSchema: Codable, Equatable {
    let type: String?
    let options: [EscalationOptionData]?
    let placeholder: String?
    let allowOther: Bool?
    let confirmLabel: String?
    let denyLabel: String?
    let suggestions: [String]?
    // Batched-clarification chain (web: renders a "STEP X OF N" eyebrow).
    let chainId: String?
    let chainPosition: Int?
    let chainTotal: Int?

    /// `confirmation` — the backend's own flag. It bands the decision and never
    /// changes what is posted: a confirmed destructive action and a confirmed
    /// benign one are the same response value.
    let destructive: Bool?

    /// `external_action` — what the reader has to go and do, and the label on the
    /// control that says they did. **`instructions` is the whole content of this
    /// input type and reached no surface on either client**, so the reader got a
    /// bare note field under a prompt that assumed they already knew the task.
    let instructions: String?
    let doneLabel: String?

    /// `file_path` — whether several paths are wanted, and what shape they should
    /// be. Both were on the wire and neither reached the reader, so a prompt
    /// wanting three CSVs looked exactly like one wanting a config file.
    let multiple: Bool?
    let filter: String?

    /// `tool_authorization` — what the agent wants to call, and with what.
    /// Rendered **verbatim**: the prompt is a sentence the backend composed
    /// around these, and a grant made against the sentence is a grant made
    /// against something the reader was never shown.
    let toolName: String?
    let paramsSummary: String?

    /// `sandbox_override` — the command that broke policy, the policy it broke,
    /// and the file roots the grant would cover. `allowedRoots` is empty for
    /// shell overrides, which renders no roots line rather than "no roots".
    let command: String?
    let violation: String?
    let allowedRoots: [String]?
    let questions: [AttentionFormQuestion]?
    /// The backend's classification when the ask collects a secret. Shared
    /// with the chat card (`ChatSensitiveSpecData`) so both surfaces mask by
    /// the same spec.
    let sensitive: ChatSensitiveSpecData?
    /// The service request type (`secure_browser_input` and friends predate
    /// the spec and are still secret by name).
    let requestType: String?

    enum CodingKeys: String, CodingKey {
        case type, options, placeholder, suggestions, destructive, instructions, multiple, filter
        case command, violation, questions, sensitive
        case requestType = "request_type"
        case allowOther = "allow_other"
        case confirmLabel = "confirm_label"
        case denyLabel = "deny_label"
        case chainId = "chain_id"
        case chainPosition = "chain_position"
        case chainTotal = "chain_total"
        case doneLabel = "done_label"
        case toolName = "tool_name"
        case paramsSummary = "params_summary"
        case allowedRoots = "allowed_roots"
    }
}

struct AttentionFormQuestion: Codable, Equatable, Identifiable {
    let id: String
    let prompt: String
    let inputType: String?
    let options: [EscalationOptionData]?
    enum CodingKeys: String, CodingKey {
        case id, prompt, question, options
        case inputType = "input_type"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        prompt = try container.decodeIfPresent(String.self, forKey: .prompt)
            ?? container.decodeIfPresent(String.self, forKey: .question)
            ?? ""
        inputType = try container.decodeIfPresent(String.self, forKey: .inputType)
        options = try container.decodeIfPresent([EscalationOptionData].self, forKey: .options)
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(id, forKey: .id)
        try container.encode(prompt, forKey: .prompt)
        try container.encodeIfPresent(inputType, forKey: .inputType)
        try container.encodeIfPresent(options, forKey: .options)
    }
}

/// One grant on offer in an authorization ask.
///
/// Its own two-field shape rather than `EscalationOptionData`, because the
/// fallback pair has to be *constructed* when the payload carries no options and
/// that type lives in `Shared` with no initializer this file should reach for.
/// Nothing here needs `requires_input`: a grant is a decision, not a form.
struct AttentionGrantOption: Identifiable, Equatable {
    let id: String
    let label: String
}

/// The canonical HITL identifiers — the backend nests these under
/// `metadata.hitl_request.identifiers`, NOT at the top level of metadata.
struct HitlIdentifiers: Codable, Equatable {
    let pauseStateId: String?
    let approvalId: String?
    let correlationId: String?
    let requestId: String?
    enum CodingKeys: String, CodingKey {
        case pauseStateId = "pause_state_id"
        case approvalId = "approval_id"
        case correlationId = "correlation_id"
        case requestId = "request_id"
    }
}

/// The whole HITL open-payload the backend stamps at `metadata.hitl_request`.
struct HitlRequestPayload: Codable, Equatable {
    let identifiers: HitlIdentifiers?
    let source: String?
    let inputType: String?
    enum CodingKeys: String, CodingKey {
        case identifiers, source
        case inputType = "input_type"
    }
}

/// One staged file in a diff_approval proposal (web `HitlDiffApprovalFile`).
struct AttentionDiffFile: Codable, Equatable, Identifiable {
    let path: String
    let status: String?
    let additions: Int?
    let deletions: Int?
    let unifiedDiff: String?
    var id: String { path }
    enum CodingKeys: String, CodingKey {
        case path, status, additions, deletions
        case unifiedDiff = "unified_diff"
    }
}

/// The free-form `metadata` object attached to a feed item, holding the HITL contract.
struct AttentionMetadata: Codable, Equatable {
    let source: String?
    let attentionKind: String?
    let inputType: String?
    let inputSchema: AttentionInputSchema?
    let question: String?
    let hint: String?
    let options: [EscalationOptionData]?
    let pauseStateId: String?
    let hitlRequest: HitlRequestPayload?
    let reviewHref: String?
    let reviewLabel: String?
    let files: [AttentionDiffFile]?

    enum CodingKeys: String, CodingKey {
        case source, question, hint, options, files
        case attentionKind = "attention_kind"
        case inputType = "input_type"
        case inputSchema = "input_schema"
        case pauseStateId = "pause_state_id"
        case hitlRequest = "hitl_request"
        case reviewHref = "review_href"
        case reviewLabel = "review_label"
    }
}

/// Pending chat questions can precede their feed projection. Reuse the same
/// typed input schema and answer forms once they are read from the ledger.
struct PendingUserRequestsResponse: Decodable {
    let requests: [PendingUserRequest]
}

struct PendingUserRequest: Decodable {
    let id: String
    let question: String
    let options: [EscalationOptionData]?
    let context: Context?
    let createdAt: Double?

    struct Context: Decodable {
        let inputType: String?
        let inputSchema: AttentionInputSchema?
        enum CodingKeys: String, CodingKey {
            case inputType = "input_type", inputSchema = "input_schema"
        }
    }
    enum CodingKeys: String, CodingKey {
        case id, question, options, context
        case createdAt = "created_at"
    }
}

struct AttentionItem: Codable, Identifiable, Equatable {
    let id: String
    let title: String
    let summary: String?
    let itemType: String
    let status: String
    let metadata: AttentionMetadata?
    /// Epoch milliseconds, from the feed item's `updated_at` — every card
    /// carries a date/time so a failure can be placed in the day it happened.
    var updatedAt: Double? = nil

    init(pending: PendingUserRequest) {
        id = "user-request:\(pending.id)"
        title = pending.question
        summary = nil
        itemType = "request"
        status = "needs_action"
        updatedAt = pending.createdAt
        metadata = AttentionMetadata(
            source: "user_request", attentionKind: "user_request.pending",
            inputType: pending.context?.inputType ?? ((pending.options ?? []).isEmpty ? "text" : "choice"),
            inputSchema: pending.context?.inputSchema, question: pending.question,
            hint: nil, options: pending.options, pauseStateId: pending.id,
            hitlRequest: nil, reviewHref: nil, reviewLabel: nil, files: nil
        )
    }

    var isPendingLedgerProjection: Bool {
        source == "user_request" && id == "user-request:\(correlationId)"
    }

    // Lenient decode — `metadata` is free-form, so tolerate any shape.
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = (try? c.decode(String.self, forKey: .id)) ?? UUID().uuidString
        title = (try? c.decode(String.self, forKey: .title)) ?? "Needs attention"
        summary = try? c.decodeIfPresent(String.self, forKey: .summary)
        itemType = (try? c.decode(String.self, forKey: .itemType)) ?? ""
        status = (try? c.decode(String.self, forKey: .status)) ?? ""
        metadata = try? c.decodeIfPresent(AttentionMetadata.self, forKey: .metadata)
        updatedAt = try? c.decode(Double.self, forKey: .updatedAt)
    }

    enum CodingKeys: String, CodingKey {
        case id, title, summary, status, metadata
        case itemType = "item_type"
        case updatedAt = "updated_at"
    }

    // MARK: derived HITL contract (mirrors web hitlRequestFromFeedItem)

    /// Where the response POST is routed (agentic / approval / escalation / user_request / …).
    var source: String {
        if itemType == "approval" { return "approval" }
        if let s = metadata?.source, !s.isEmpty { return s }
        if let s = metadata?.hitlRequest?.source, !s.isEmpty { return s }
        switch metadata?.attentionKind {
        case "user_request.pending": return "user_request"
        case "max_iterations_reached": return "escalation"
        case "diff_approval": return "diff_approval"
        case "clarification": return "clarification"
        case "plan_approval": return "plan_approval"
        default: return "agentic"
        }
    }

    /// The id to POST the response to. correlation_id lives under
    /// `metadata.hitl_request.identifiers`; pause_state_id is a top-level alias.
    var correlationId: String {
        let ids = metadata?.hitlRequest?.identifiers
        return ids?.correlationId
            ?? metadata?.pauseStateId
            ?? ids?.pauseStateId
            ?? ids?.requestId
            ?? ids?.approvalId
            ?? "\(id)-hitl"
    }

    /// text / password / choice / multi_choice / confirmation / guidance / diff_approval / …
    var inputType: String {
        if itemType == "approval" { return "confirmation" }
        return metadata?.inputSchema?.type ?? metadata?.inputType ?? metadata?.hitlRequest?.inputType ?? "text"
    }

    var options: [EscalationOptionData] {
        metadata?.inputSchema?.options ?? metadata?.options ?? []
    }
    var prompt: String { metadata?.question ?? title }
    var hint: String? { metadata?.hint ?? summary }
    var placeholder: String { metadata?.inputSchema?.placeholder ?? "Type your response…" }
    var confirmLabel: String { metadata?.inputSchema?.confirmLabel ?? "Approve" }
    var denyLabel: String { metadata?.inputSchema?.denyLabel ?? "Reject" }
    var isActionable: Bool { !["failed", "running"].contains(itemType) }

    /// Batched-clarification position, e.g. "Step 2 of 3" — nil when not chained.
    var chainLabel: String? {
        guard let pos = metadata?.inputSchema?.chainPosition,
              let total = metadata?.inputSchema?.chainTotal, total > 1 else { return nil }
        return "Step \(pos) of \(total)"
    }

    /// A deep link to the item's source (execution / plan) if the backend stamped one.
    var reviewURL: URL? { (metadata?.reviewHref).flatMap { URL(string: $0) } }
    var reviewLabel: String { metadata?.reviewLabel ?? "View source" }

    /// Staged files in a diff_approval proposal (empty when whole-proposal only).
    var diffFiles: [AttentionDiffFile] { metadata?.files ?? [] }

    // MARK: the five that used to render as something else

    /// Which permission a grant asks for, or `nil` when this is not one.
    ///
    /// **One accessor for two input types**, because the decision is the same and
    /// only the nouns differ: a tool the agent is not listed for, or a command
    /// that violated sandbox policy. Both used to fall into the generic `choice`
    /// list, where `Deny` rendered identically to `Allow for This Run` and a
    /// sandbox escape read exactly like "which quarter?".
    var grantKind: String? {
        switch inputType {
        case "tool_authorization": return "tool"
        case "sandbox_override": return "sandbox"
        default: return nil
        }
    }

    /// What would run, verbatim. Empty when the payload named nothing, which
    /// renders no line rather than an empty box.
    var grantSubject: String {
        let schema = metadata?.inputSchema
        return (grantKind == "sandbox" ? schema?.command : schema?.toolName) ?? ""
    }

    /// Why permission is needed, verbatim.
    var grantDetail: String? {
        let schema = metadata?.inputSchema
        let value = (grantKind == "sandbox" ? schema?.violation : schema?.paramsSummary) ?? ""
        return value.isEmpty ? nil : value
    }

    /// File roots the grant would cover. Empty for shell overrides.
    var grantRoots: [String] { metadata?.inputSchema?.allowedRoots ?? [] }

    /// The grants on offer, and the one that refuses.
    ///
    /// **Not a fixed pair.** `UserInputType::options_json` offers a tool
    /// authorization *three* ids — `allow_once`, `allow_always`, `deny` — and
    /// `allow_always` writes the tool into the session allowlist, a strictly
    /// broader grant than the one-off. Collapsing them into Allow/Deny would
    /// answer a broader question than the one asked, silently.
    ///
    /// With no options on the payload this falls back to the two ids the resume
    /// dispatcher has always accepted. That is a floor, not a guess: a broader
    /// grant is never offered on a payload that did not name it.
    var grantOptions: [AttentionGrantOption] {
        options.isEmpty
            ? [AttentionGrantOption(id: "allow_once", label: "Allow once"),
               AttentionGrantOption(id: "deny", label: "Deny")]
            : options.map { AttentionGrantOption(id: $0.id, label: $0.label) }
    }

    /// `deny` is the id the runtime's resume dispatcher matches on and the id
    /// `options_json` emits for both grants, so this is a contract rather than a
    /// guess. Matched by **id** and never by label: a backend that relabels
    /// `Deny` changes what the reader sees and nothing about which id refuses.
    var grantDenyOption: AttentionGrantOption? { grantOptions.first { $0.id == "deny" } }
    var grantAllowOptions: [AttentionGrantOption] { grantOptions.filter { $0.id != "deny" } }

    /// `external_action` — what to go and do, and the acknowledgement's label.
    var externalInstructions: String? {
        let value = metadata?.inputSchema?.instructions ?? ""
        return value.isEmpty ? nil : value
    }
    var externalDoneLabel: String { metadata?.inputSchema?.doneLabel ?? "I've completed this" }

    /// `file_path` — whether several are wanted, and the filter the ask named.
    var wantsMultiplePaths: Bool { metadata?.inputSchema?.multiple == true }
    var pathFilter: String? {
        let value = metadata?.inputSchema?.filter ?? ""
        return value.isEmpty ? nil : value
    }
    /// The field's own label, so the reader is not guessing how many paths are
    /// wanted or how to separate them — the separator is stated because the
    /// response splits on it.
    var pathFieldLabel: String {
        let head = wantsMultiplePaths ? "File paths, comma-separated" : "File path"
        guard let filter = pathFilter else { return head }
        return "\(head) · matching \(filter)"
    }

    /// `confirmation` — the backend's destructive flag, which bands the decision
    /// and changes nothing about the value posted.
    var isDestructive: Bool { metadata?.inputSchema?.destructive == true }
    var formQuestions: [AttentionFormQuestion] { metadata?.inputSchema?.questions ?? [] }

    // MARK: Secrets (P3)

    /// The backend's value-free spec, when this ask collects a secret.
    var sensitiveSpec: ChatSensitiveSpecData? { metadata?.inputSchema?.sensitive }
    /// Whether the ask collects a secret: the spec, or the two built-in secure
    /// browser request types that predate it.
    var isSensitive: Bool {
        if sensitiveSpec != nil { return true }
        let requestType = metadata?.inputSchema?.requestType ?? ""
        return requestType == "secure_browser_input" || requestType == "secure_browser_confirm"
    }
    /// How the single-value field renders: `otp`, `password`, or the widget type.
    var renderKind: String { hitlRenderKind(inputType: inputType, sensitiveKind: sensitiveSpec?.kind) }
    /// The flagged kind of one form field — the spec first, then a question
    /// typed `password`/`otp` for an entry announced before the spec existed.
    func sensitiveFieldKind(_ questionId: String) -> String? {
        if let kind = sensitiveSpec?.fieldKind(questionId) { return kind }
        let typed = formQuestions.first(where: { $0.id == questionId })?.inputType
        return typed == "password" || typed == "otp" ? typed : nil
    }
    var isOneTime: Bool { sensitiveSpec?.oneTime == true }
    var sensitiveDeadline: Date? {
        guard let ms = sensitiveSpec?.collectionDeadlineMs, ms > 0 else { return nil }
        return Date(timeIntervalSince1970: TimeInterval(ms) / 1000)
    }
}

struct LanePage: Codable, Equatable {
    let total: Int?
    let nextCursor: String?
    let hasMore: Bool?
    enum CodingKeys: String, CodingKey {
        case total
        case nextCursor = "next_cursor"
        case hasMore = "has_more"
    }
}

struct AttentionPages: Codable, Equatable {
    var requests, approvals, escalations, failed, running: LanePage?
}

/// Backend attention feed counts (web `FeedAttentionCounts`). The badge reads
/// `needs_action` + `failed` from here — the same backend numbers the web uses.
/// Lenient decode: any missing/odd field is 0.
struct FeedAttentionCounts: Codable, Equatable {
    var needsAction = 0
    var failed = 0
    var requests = 0
    var approvals = 0
    var escalations = 0

    enum CodingKeys: String, CodingKey {
        case needsAction = "needs_action"
        case failed, requests, approvals, escalations
    }

    init() {}
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        func n(_ k: CodingKeys) -> Int { (try? c.decode(Int.self, forKey: k)) ?? 0 }
        needsAction = n(.needsAction); failed = n(.failed)
        requests = n(.requests); approvals = n(.approvals); escalations = n(.escalations)
    }
}

struct FeedAttentionResponse: Codable {
    var requests, approvals, escalations, failed, running: [AttentionItem]
    var pages: AttentionPages?
    var counts: FeedAttentionCounts?

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        func lane(_ k: CodingKeys) -> [AttentionItem] { (try? c.decode([AttentionItem].self, forKey: k)) ?? [] }
        requests = lane(.requests); approvals = lane(.approvals); escalations = lane(.escalations)
        failed = lane(.failed); running = lane(.running)
        pages = try? c.decodeIfPresent(AttentionPages.self, forKey: .pages)
        counts = try? c.decodeIfPresent(FeedAttentionCounts.self, forKey: .counts)
    }
    enum CodingKeys: String, CodingKey { case requests, approvals, escalations, failed, running, pages, counts }

    func withPendingUserRequests(_ pending: [PendingUserRequest]) -> Self {
        var seen = Set((requests + approvals + escalations)
            .filter { $0.source == "user_request" }.map(\.correlationId))
        let added = pending.filter { !$0.id.isEmpty && seen.insert($0.id).inserted }
            .map(AttentionItem.init(pending:))
        guard !added.isEmpty else { return self }
        var result = self
        result.requests = (requests + added).sorted { ($0.updatedAt ?? 0) > ($1.updatedAt ?? 0) }
        var nextCounts = counts ?? FeedAttentionCounts()
        if counts == nil {
            nextCounts.requests = requests.count
            nextCounts.approvals = approvals.count
            nextCounts.escalations = escalations.count
            nextCounts.failed = failed.count
            nextCounts.needsAction = requests.count + approvals.count + escalations.count
        }
        nextCounts.requests += added.count
        nextCounts.needsAction += added.count
        result.counts = nextCounts
        if let page = pages?.requests {
            result.pages?.requests = LanePage(
                total: page.total.map { $0 + added.count },
                nextCursor: page.nextCursor, hasMore: page.hasMore
            )
        }
        return result
    }

    func page(_ lane: String) -> LanePage? {
        switch lane {
        case "requests": return pages?.requests
        case "approvals": return pages?.approvals
        case "escalations": return pages?.escalations
        case "failed": return pages?.failed
        case "running": return pages?.running
        default: return nil
        }
    }
}

/// A resolved HITL pair (HitlRequested + matching HitlResolved) for the history view.
struct ResolvedHitl: Identifiable, Equatable {
    let correlationId: String
    let prompt: String
    let outcome: String
    let decision: String?
    let resolvedAt: Double
    var id: String { correlationId }
}

// MARK: - ViewModel

final class AttentionViewModel: ObservableObject {
    /// Lane key -> items (accumulated across pages).
    @Published var lanes: [String: [AttentionItem]] = [:]
    @Published var pages: AttentionPages?
    @Published var isLoading = false
    /// True only for a refresh the USER asked for (pull-to-refresh, the nav
    /// refresh button). Background refetches (realtime events, tab switches)
    /// run silently — a busy system refetches constantly, and surfacing every
    /// one as a spinner made the whole screen look stuck "refreshing".
    @Published var isUserRefresh = false
    @Published private(set) var loadingLanes: Set<String> = []
    /// Resolved-HITL history (the web "all/history" view mode).
    @Published var resolved: [ResolvedHitl] = []
    @Published var showHistory = false
    @Published var historyLoading = false
    /// True while a bulk "Approve all" apply is in flight (web `approvingAllDiffs`).
    @Published var approvingAllDiffs = false
    /// Transient summary after a bulk apply, e.g. "Applied 3 code change sets." (web notice).
    @Published var bulkNotice: String?
    /// Backend feed counts (needs_action/failed/…) from the last fetch.
    @Published var counts: FeedAttentionCounts?
    /// Canonical "Needs you" badge count (see `resolveAttentionBadgeCount`).
    @Published var badgeCount = 0
    @Published private(set) var pendingCardMutationKeys: Set<String> = []
    @Published var mutationError: String?
    @Published private(set) var loadError: String?

    /// Canonical Attention badge count — ported VERBATIM from the web
    /// `resolveAttentionBadgeCount` (ui/unified-ui/src/lib/attention/attentionBadgeCount.ts):
    /// pending-HITL and feed needs-action overlap, so the larger projection wins;
    /// non-HITL failed rows are then added once.
    ///
    /// The realtime pending-HITL tracker can briefly lead the fetched feed. Local
    /// optimistic responses drop its correlation id synchronously and restore it
    /// if the response API fails.
    static func resolveAttentionBadgeCount(pendingHitl: Int, needsAction: Int, failed: Int) -> Int {
        max(max(0, pendingHitl), needsAction) + max(0, failed)
    }

    /// Tab order — parity with the web attention categories (ATTENTION_CATEGORY_ORDER):
    /// All, Requests, Approvals, Escalations, Failed. "all" aggregates the HITL lanes.
    /// ("running" is no longer a top tab — web has no such category.) Channel
    /// follow-ups are no longer an attention category — they live on Today.
    static let laneKeys = ["all", "requests", "approvals", "escalations", "failed"]

    private let pageLimit = 25
    /// The widest `limit` the feed read seam will honour — `/feed/attention`
    /// clamps to 200. A reset asks for the span the reader holds, capped here
    /// so what is asked for is what can be served.
    private static let maxFeedPageSize = 200
    /// Rows the reader has asked for — in the widest feed lane. Kept APART from
    /// the row counts so a card a mutation just removed does not shrink the
    /// window its own refetch asks for: the reader paged for these rows and
    /// still has them.
    ///
    /// The lanes are cursor-paginated, so an oversized ask is harmless — the
    /// span is a window size here, never a position.
    private var feedLoadedSpan = 0
    private let networkSession: URLSession
    private let mutationCoordinator: CardMutationCoordinator
    private var mutationCoordinatorCancellable: AnyCancellable?
    private var mutationExpiryCancellable: AnyCancellable?
    private var rollbackOrderLedger = OptimisticCardOrderLedger()
    private var optimisticAttentionLanes: [OptimisticCardKey: Set<String>] = [:]
    private var base: String { "\(MagicianAccess.baseURL.absoluteString)/api/magician/v2" }

    init(networkSession: URLSession = .shared,
         mutationCoordinator: CardMutationCoordinator? = nil) {
        self.networkSession = networkSession
        self.mutationCoordinator = mutationCoordinator
            ?? (isRunningUnderTests ? CardMutationCoordinator() : .shared)
        mutationCoordinatorCancellable = self.mutationCoordinator.objectWillChange
            .sink { [weak self] _ in self?.objectWillChange.send() }
        mutationExpiryCancellable = self.mutationCoordinator.expired
            .sink { [weak self] key in self?.restoreExpiredReissue(for: key) }
    }

    func items(_ lane: String) -> [AttentionItem] {
        if lane == "all" {   // web parity: the actionable HITL union
            return ((lanes["requests"] ?? []) + (lanes["approvals"] ?? [])
                 + (lanes["escalations"] ?? []) + (lanes["failed"] ?? []))
                .filter { !isAttentionSuppressed($0) }
        }
        return (lanes[lane] ?? []).filter { !isAttentionSuppressed($0) }
    }

    func total(_ lane: String) -> Int {
        switch lane {
        case "all":
            // The four HITL lanes. Keeps this equal to the badge
            // (`needs_action + failed`) and to what the `all` list renders.
            return ["requests", "approvals", "escalations", "failed"].reduce(0) {
                $0 + total($1)
            }
        case "requests": return page(lane)?.total ?? counts?.requests ?? items(lane).count
        case "approvals": return page(lane)?.total ?? counts?.approvals ?? items(lane).count
        case "escalations": return page(lane)?.total ?? counts?.escalations ?? items(lane).count
        case "failed": return page(lane)?.total ?? counts?.failed ?? items(lane).count
        default: return page(lane)?.total ?? items(lane).count
        }
    }

    func loadedCount(_ lane: String) -> Int {
        items(lane).count
    }

    func hasMore(_ lane: String) -> Bool {
        switch lane {
        case "all":
            // The four HITL lanes — `all` renders only those, so it must not
            // claim more pages on any other list's behalf.
            return ["requests", "approvals", "escalations", "failed"].contains { page($0)?.hasMore == true }
        default: return page(lane)?.hasMore == true
        }
    }

    func isLoadingPage(_ lane: String) -> Bool {
        if lane == "all" {
            // `all` aggregates the four HITL lanes, so it is loading while any of them is.
            return !loadingLanes.isDisjoint(with: ["requests", "approvals", "escalations", "failed"])
        }
        return loadingLanes.contains(lane)
    }

    private func attentionFeedURL(lane: String? = nil, cursor: String? = nil,
                                  limit: Int? = nil) -> URL? {
        guard var components = URLComponents(string: "\(base)/feed/attention") else { return nil }
        var query = [
            URLQueryItem(name: "limit", value: String(limit ?? pageLimit)),
        ]
        if let lane, let cursor {
            query.append(URLQueryItem(name: "\(lane)_cursor", value: cursor))
        }
        components.queryItems = query
        return components.url
    }

    /// The page size a reset must ask for to hand the reader back what they
    /// were holding: whatever they have loaded, never below one page, never
    /// above what the endpoint will serve.
    ///
    /// Past that ceiling the span is trimmed rather than lost — the response's
    /// own `next_cursor` and `total` still describe the rows that came back,
    /// so paging resumes exactly where the trimmed list ends.
    private func loadedSpan(_ loaded: Int, ceiling: Int) -> Int {
        min(max(pageLimit, loaded), ceiling)
    }

    func page(_ lane: String) -> LanePage? {
        switch lane {
        case "requests": return pages?.requests
        case "approvals": return pages?.approvals
        case "escalations": return pages?.escalations
        case "failed": return pages?.failed
        case "running": return pages?.running
        default: return nil
        }
    }

    func fetch() { fetch(completion: nil) }

    /// Pull-to-refresh / the nav refresh button: identical fetch, flagged so
    /// the UI can show the spinner only where a human asked for it.
    func fetchUserInitiated() { fetch(completion: nil, userInitiated: true) }

    func refresh() async {
        await withCheckedContinuation { continuation in
            fetch(completion: { continuation.resume() }, userInitiated: true)
        }
    }

    private func fetch(completion: (() -> Void)?, userInitiated: Bool = false) {
        guard !isUITestLaunch else { completion?(); return }   // UI tests: no real backend fetch (keeps launch idle)
        guard !isLoading else { completion?(); return }
        isLoading = true
        isUserRefresh = userInitiated
        let group = DispatchGroup()
        group.enter()
        // Re-read every lane over the span the reader has loaded. The lanes
        // accumulate, so restarting each at its first page would discard the
        // pages the reader paged into before acting on a card.
        let feedSpan = loadedSpan(feedLoadedSpan, ceiling: Self.maxFeedPageSize)
        request(attentionFeedURL(limit: feedSpan)?.absoluteString ?? "") { [weak self] decoded in
            defer { group.leave() }
            guard let self, let d = decoded else { return }
            let rawLanes = [
                "requests": d.requests, "approvals": d.approvals, "escalations": d.escalations,
                "failed": d.failed, "running": d.running,
            ]
            var suppressedByLane: [String: Int] = [:]
            for (lane, items) in rawLanes {
                suppressedByLane[lane] = self.attentionProjectionDecrements(
                    in: lane,
                    rawItems: items
                )
            }
            self.optimisticAttentionLanes = self.optimisticAttentionLanes.filter { key, _ in
                self.mutationCoordinator.isSuppressed(key)
            }
            // Keep the raw rows behind the shared suppression overlay. If a
            // same-ID request is legitimately reissued during the grace window,
            // expiry can reveal that exact row without waiting for another poll.
            self.lanes = rawLanes
            self.feedLoadedSpan = max(self.feedLoadedSpan,
                                      rawLanes.values.map(\.count).max() ?? 0)
            self.pages = Self.adjustedPages(d.pages, suppressedByLane: suppressedByLane)
            self.updateCountsAndBadge(d.counts)
        }
        group.notify(queue: .main) { [weak self] in
            self?.isLoading = false
            self?.isUserRefresh = false
            completion?()
        }
    }

    private func updateCountsAndBadge(_ incoming: FeedAttentionCounts?) {
        let visibleLanes = lanes.mapValues { $0.filter { !isAttentionSuppressed($0) } }
        let suppressed = Dictionary(uniqueKeysWithValues: lanes.map { lane, items in
            (lane, attentionProjectionDecrements(in: lane, rawItems: items))
        })
        var nextCounts = incoming
        if var projectedCounts = nextCounts {
            let requests = suppressed["requests"] ?? 0
            let approvals = suppressed["approvals"] ?? 0
            let escalations = suppressed["escalations"] ?? 0
            let failed = suppressed["failed"] ?? 0
            projectedCounts.requests = max(0, projectedCounts.requests - requests)
            projectedCounts.approvals = max(0, projectedCounts.approvals - approvals)
            projectedCounts.escalations = max(0, projectedCounts.escalations - escalations)
            projectedCounts.needsAction = max(
                0,
                projectedCounts.needsAction - requests - approvals - escalations
            )
            projectedCounts.failed = max(0, projectedCounts.failed - failed)
            nextCounts = projectedCounts
        }
        self.counts = nextCounts
        // Seed the pending-HITL baseline from the feed's actionable HITL rows.
        // AppTabView renders the live badge (which also reacts to WS-driven
        // tracker changes between fetches); this snapshot keeps the VM's own
        // badgeCount consistent.
        PendingHitlTracker.shared.seed(correlationIds:
            ((visibleLanes["requests"] ?? []) + (visibleLanes["approvals"] ?? [])
                + (visibleLanes["escalations"] ?? []))
                .map { $0.correlationId }.filter { !$0.isEmpty })
        self.badgeCount = Self.resolveAttentionBadgeCount(
            pendingHitl: PendingHitlTracker.shared.count,
            needsAction: nextCounts?.needsAction ?? 0,
            failed: nextCounts?.failed ?? 0
        )
    }

    func loadMore(_ lane: String) {
        if lane == "all" {
            for key in ["requests", "approvals", "escalations", "failed"] where page(key)?.hasMore == true {
                loadMoreFeedLane(key)
            }
            return
        }
        loadMoreFeedLane(lane)
    }

    private func loadMoreFeedLane(_ lane: String) {
        guard !loadingLanes.contains(lane) else { return }
        guard let cursor = page(lane)?.nextCursor, !cursor.isEmpty else { return }
        guard let url = attentionFeedURL(lane: lane, cursor: cursor) else { return }
        loadingLanes.insert(lane)
        request(url.absoluteString, includePending: false) { [weak self] decoded in
            guard let self else { return }
            defer { self.loadingLanes.remove(lane) }
            guard let d = decoded else { return }
            let more: [AttentionItem]
            switch lane {
            case "requests": more = d.requests
            case "approvals": more = d.approvals
            case "escalations": more = d.escalations
            case "failed": more = d.failed
            case "running": more = d.running
            default: more = []
            }
            let previousProjectionDecrements = self.attentionProjectionDecrements(
                in: lane,
                rawItems: self.lanes[lane] ?? []
            )
            self.lanes[lane] = Self.mergedAttentionItems(
                self.lanes[lane] ?? [],
                more
            )
            self.feedLoadedSpan = max(self.feedLoadedSpan, self.lanes[lane]?.count ?? 0)
            let projectedDecrements = self.attentionProjectionDecrements(
                in: lane,
                rawItems: self.lanes[lane] ?? []
            )
            let newlyDiscoveredDecrements = projectedDecrements - previousProjectionDecrements
            if newlyDiscoveredDecrements != 0 {
                self.adjustAttentionCountsOnly(
                    for: lane,
                    delta: -newlyDiscoveredDecrements
                )
            }
            if var counts = d.counts {
                let ledgerOnly = (self.lanes["requests"] ?? []).filter(\.isPendingLedgerProjection).count
                counts.requests += ledgerOnly
                counts.needsAction += ledgerOnly
                self.updateCountsAndBadge(counts)
            }
            if let incomingPage = d.page(lane) {
                let ledgerOnly = (self.lanes[lane] ?? []).filter(\.isPendingLedgerProjection).count
                let projectedTotal = incomingPage.total.map {
                    max(self.items(lane).count, $0 + ledgerOnly - projectedDecrements)
                } ?? self.page(lane)?.total
                self.setPage(
                    LanePage(
                        total: projectedTotal,
                        nextCursor: incomingPage.nextCursor,
                        hasMore: incomingPage.hasMore
                    ),
                    for: lane
                )
            }
        }
    }

    private func setPage(_ page: LanePage?, for lane: String) {
        var value = pages ?? AttentionPages(requests: nil, approvals: nil, escalations: nil, failed: nil, running: nil)
        switch lane {
        case "requests": value.requests = page
        case "approvals": value.approvals = page
        case "escalations": value.escalations = page
        case "failed": value.failed = page
        case "running": value.running = page
        default: return
        }
        pages = value
    }

    static func mergedAttentionItems(_ existing: [AttentionItem], _ incoming: [AttentionItem]) -> [AttentionItem] {
        var result = existing
        for item in incoming {
            if let index = result.firstIndex(where: {
                $0.id == item.id || ($0.source == "user_request" && item.source == "user_request"
                    && !item.correlationId.isEmpty && $0.correlationId == item.correlationId)
            }) {
                if !item.isPendingLedgerProjection || result[index].isPendingLedgerProjection {
                    result[index] = item
                }
            } else {
                result.append(item)
            }
        }
        return result
    }

    private static func adjustedPages(_ pages: AttentionPages?,
                                      suppressedByLane: [String: Int]) -> AttentionPages? {
        guard var pages else { return nil }
        func adjusted(_ page: LanePage?, _ lane: String) -> LanePage? {
            guard let page else { return nil }
            return LanePage(
                total: page.total.map { max(0, $0 - (suppressedByLane[lane] ?? 0)) },
                nextCursor: page.nextCursor,
                hasMore: page.hasMore
            )
        }
        pages.requests = adjusted(pages.requests, "requests")
        pages.approvals = adjusted(pages.approvals, "approvals")
        pages.escalations = adjusted(pages.escalations, "escalations")
        pages.failed = adjusted(pages.failed, "failed")
        pages.running = adjusted(pages.running, "running")
        return pages
    }

    private func isAttentionSuppressed(_ item: AttentionItem) -> Bool {
        mutationCoordinator.isSuppressed(.attention(item.id))
            || (!item.correlationId.isEmpty
                && mutationCoordinator.isSuppressed(.hitl(item.correlationId)))
    }

    private func attentionKey(_ key: OptimisticCardKey, matches item: AttentionItem) -> Bool {
        switch key {
        case .attention(let id): return item.id == id
        case .hitl(let correlationID): return item.correlationId == correlationID
        default: return false
        }
    }

    private func attentionProjectionDecrements(in lane: String,
                                               rawItems: [AttentionItem]) -> Int {
        let suppressedRaw = rawItems.lazy.filter(isAttentionSuppressed).count
        let pendingOutsidePage = optimisticAttentionLanes.lazy.filter { [self] key, originalLanes in
            originalLanes.contains(lane)
                && self.mutationCoordinator.isPending(key)
                && !rawItems.contains(where: { self.attentionKey(key, matches: $0) })
        }.count
        return suppressedRaw + pendingOutsidePage
    }

    private func restoreExpiredReissue(for key: OptimisticCardKey) {
        guard !mutationCoordinator.isSuppressed(key) else { return }
        optimisticAttentionLanes[key] = nil
        let restoredLanes = Set(lanes.compactMap { lane, items in
            items.contains { item in
                attentionKey(key, matches: item) && !isAttentionSuppressed(item)
            } ? lane : nil
        })
        guard !restoredLanes.isEmpty else { return }
        adjustAttentionProjection(for: restoredLanes, delta: 1)
    }

    // MARK: - Realtime (web parity: live inbox on HITL events)

    private var webSocket: URLSessionWebSocketTask?
    private var reloadWork: DispatchWorkItem?
    /// Whether this view still wants a live socket. A reconnect is scheduled on
    /// the main queue after a delay, so without this flag a socket torn down by
    /// `disconnectRealtime()` in the meantime would be resurrected behind the
    /// view that no longer exists.
    private var realtimeStarted = false

    /// Subscribe to the realtime stream and refresh (debounced) when a HITL request
    /// is raised or resolved, so the inbox stays live instead of pull-to-refresh only.
    func connectRealtime() {
        guard !isRunningUnderTests else { return }   // no real WebSocket in unit tests
        realtimeStarted = true
        guard webSocket == nil,
              // The paired bearer authorizes the upgrade and binds the event scope.
              let url = URL(string: "\(MagicianAccess.webSocketBaseURL.absoluteString)/api/magician/v2/realtime/ws") else { return }
        var req = URLRequest(url: url)
        MagicianAccess.authorize(&req)
        let ws = URLSession.shared.webSocketTask(with: req)
        webSocket = ws
        ws.resume()
        receiveRealtime()
    }

    func disconnectRealtime() {
        realtimeStarted = false
        webSocket?.cancel(with: .goingAway, reason: nil)
        webSocket = nil
    }

    /// Mirrors `TodayViewModel.receiveRealtime`, which is the correct shape.
    ///
    /// A `.failure` has to clear `webSocket` and schedule a reconnect. Re-arming
    /// only on `.success` ends the receive loop at the first error — a tunnel
    /// blip, a backend restart, the app being backgrounded — and because
    /// `connectRealtime()` guards on `webSocket == nil`, a stale non-nil socket
    /// then makes every later reconnect attempt a silent no-op. The inbox goes
    /// back to pull-to-refresh for the rest of the view's life while still
    /// looking connected, which is the failure this whole path exists to avoid.
    private func receiveRealtime() {
        webSocket?.receive { [weak self] result in
            guard let self = self else { return }
            switch result {
            case .success(let message):
                if self.isHitlEvent(Self.realtimeText(from: message)) { self.scheduleReload() }
                Task { @MainActor in self.receiveRealtime() }
            case .failure:
                Task { @MainActor in
                    guard self.realtimeStarted else { return }
                    self.webSocket = nil
                    DispatchQueue.main.asyncAfter(deadline: .now() + 2) { [weak self] in
                        self?.connectRealtime()
                    }
                }
            }
        }
    }

    /// Flatten a socket frame to its text payload.
    ///
    /// The same event may arrive framed as text or as binary; reading only
    /// `.string` drops the binary ones on the floor, and a dropped event looks
    /// exactly like a quiet inbox.
    static func realtimeText(from message: URLSessionWebSocketTask.Message) -> String {
        switch message {
        case .string(let value): return value
        case .data(let value): return String(data: value, encoding: .utf8) ?? ""
        @unknown default: return ""
        }
    }

    func isHitlEvent(_ text: String) -> Bool {
        guard let data = text.data(using: .utf8),
              let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let type = obj["event_type"] as? String else { return false }
        return type.contains("Hitl") || type.contains("Attention") || type.contains("UserRequest")
    }

    private func scheduleReload() {
        reloadWork?.cancel()
        let work = DispatchWorkItem { [weak self] in self?.fetch() }
        reloadWork = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.6, execute: work)
    }

    // MARK: - Dismiss / undismiss (failed items — web parity)

    func dismiss(_ item: AttentionItem) { setDismissed(item, path: "dismiss") }
    func undismiss(_ item: AttentionItem) { setDismissed(item, path: "undismiss") }

    private func setDismissed(_ item: AttentionItem, path: String) {
        guard let url = URL(string: "\(base)/feed/attention/\(path)") else { return }
        let mutationKey = "attention:\(item.id)"
        let ticket: OptimisticCardMutationTicket?
        let originalAnchors: [String: OptimisticCardListAnchor]
        if path == "dismiss" {
            guard let started = mutationCoordinator.begin(.attention(item.id)) else { return }
            ticket = started
            guard pendingCardMutationKeys.insert(mutationKey).inserted else {
                mutationCoordinator.fail(started)
                return
            }
            originalAnchors = removeAttentionItemLocally(item)
            optimisticAttentionLanes[.attention(item.id)] = Set(originalAnchors.keys)
            mutationError = nil
        } else {
            ticket = nil
            originalAnchors = [:]
        }
        var req = URLRequest(url: url)
        req.httpMethod = "POST"
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        MagicianAccess.authorize(&req)
        req.httpBody = try? JSONSerialization.data(withJSONObject: ["item_id": item.id])
        networkSession.dataTask(with: req) { [weak self, mutationCoordinator] _, response, error in
            let status = (response as? HTTPURLResponse)?.statusCode ?? 0
            let succeeded = error == nil && (200..<300).contains(status)
            DispatchQueue.main.async {
                guard let self else {
                    if let ticket {
                        if succeeded { mutationCoordinator.succeed(ticket) }
                        else { mutationCoordinator.fail(ticket) }
                    } else if succeeded {
                        mutationCoordinator.clear(.attention(item.id))
                    }
                    return
                }
                if let ticket {
                    self.pendingCardMutationKeys.remove(mutationKey)
                    if succeeded {
                        self.removeAttentionItemRowsOnly(item)
                        self.mutationCoordinator.succeed(ticket)
                        self.fetch()
                    } else {
                        self.mutationCoordinator.fail(ticket)
                        self.optimisticAttentionLanes[.attention(item.id)] = nil
                        self.restoreAttentionItemLocally(item, originalAnchors: originalAnchors)
                        self.mutationError = "Could not dismiss this card. It has been restored."
                    }
                    self.finishAttentionRollback(originalAnchors)
                } else if succeeded {
                    self.mutationCoordinator.clear(.attention(item.id))
                    self.optimisticAttentionLanes[.attention(item.id)] = nil
                    self.fetch()
                } else {
                    self.mutationError = "Could not restore this card. Please try again."
                }
            }
        }.resume()
    }

    private func removeAttentionItemLocally(_ item: AttentionItem) -> [String: OptimisticCardListAnchor] {
        var originalAnchors: [String: OptimisticCardListAnchor] = [:]
        for lane in ["requests", "approvals", "escalations", "failed", "running"] {
            guard let values = lanes[lane],
                  values.contains(where: { $0.id == item.id }) else { continue }
            originalAnchors[lane] = rollbackOrderLedger.begin(
                listKey: "attention:\(lane)",
                itemID: item.id,
                currentIDs: values.map(\.id)
            )
            lanes[lane]?.removeAll { $0.id == item.id }
        }
        adjustAttentionProjection(for: Set(originalAnchors.keys), delta: -1)
        return originalAnchors
    }

    private func removeAttentionItemRowsOnly(_ item: AttentionItem) {
        for lane in ["requests", "approvals", "escalations", "failed", "running"] {
            lanes[lane]?.removeAll { $0.id == item.id }
        }
    }

    private func restoreAttentionItemLocally(_ item: AttentionItem,
                                             originalAnchors: [String: OptimisticCardListAnchor]) {
        for (lane, anchor) in originalAnchors {
            guard lanes[lane]?.contains(where: { $0.id == item.id }) != true else { continue }
            var values = lanes[lane] ?? []
            let index = anchor.insertionIndex(in: values.map(\.id))
            values.insert(item, at: index)
            lanes[lane] = values
        }
        adjustAttentionProjection(for: Set(originalAnchors.keys), delta: 1)
    }

    private func finishAttentionRollback(_ originalAnchors: [String: OptimisticCardListAnchor]) {
        for lane in originalAnchors.keys {
            rollbackOrderLedger.finish(listKey: "attention:\(lane)")
        }
    }

    private func adjustAttentionProjection(for laneNames: Set<String>, delta: Int) {
        if var nextCounts = counts {
            if laneNames.contains("requests") { nextCounts.requests = max(0, nextCounts.requests + delta) }
            if laneNames.contains("approvals") { nextCounts.approvals = max(0, nextCounts.approvals + delta) }
            if laneNames.contains("escalations") { nextCounts.escalations = max(0, nextCounts.escalations + delta) }
            let actionableDelta = laneNames.intersection(["requests", "approvals", "escalations"]).count * delta
            nextCounts.needsAction = max(0, nextCounts.needsAction + actionableDelta)
            if laneNames.contains("failed") { nextCounts.failed = max(0, nextCounts.failed + delta) }
            counts = nextCounts
            badgeCount = Self.resolveAttentionBadgeCount(
                pendingHitl: PendingHitlTracker.shared.count,
                needsAction: nextCounts.needsAction,
                failed: nextCounts.failed
            )
        }
        for lane in laneNames {
            guard let current = page(lane) else { continue }
            setPage(
                LanePage(total: current.total.map { max(0, $0 + delta) },
                         nextCursor: current.nextCursor,
                         hasMore: current.hasMore),
                for: lane
            )
        }
    }

    private func adjustAttentionCountsOnly(for lane: String, delta: Int) {
        guard var nextCounts = counts else { return }
        switch lane {
        case "requests":
            nextCounts.requests = max(0, nextCounts.requests + delta)
            nextCounts.needsAction = max(0, nextCounts.needsAction + delta)
        case "approvals":
            nextCounts.approvals = max(0, nextCounts.approvals + delta)
            nextCounts.needsAction = max(0, nextCounts.needsAction + delta)
        case "escalations":
            nextCounts.escalations = max(0, nextCounts.escalations + delta)
            nextCounts.needsAction = max(0, nextCounts.needsAction + delta)
        case "failed":
            nextCounts.failed = max(0, nextCounts.failed + delta)
        default:
            return
        }
        counts = nextCounts
        badgeCount = Self.resolveAttentionBadgeCount(
            pendingHitl: PendingHitlTracker.shared.count,
            needsAction: nextCounts.needsAction,
            failed: nextCounts.failed
        )
    }

    // MARK: - History (resolved HITL — web "all" view mode)

    /// Pull the bounded `/v3/events` HITL backfill (NDJSON) and pair
    /// HitlRequested + HitlResolved by correlation id into resolved rows.
    func fetchHistory() {
        historyLoading = true
        guard let url = URL(string: "\(base)/events?category=hitl&backfill_only=true&limit=300") else {
            historyLoading = false; return
        }
        var req = URLRequest(url: url)
        MagicianAccess.authorize(&req)
        networkSession.dataTask(with: req) { [weak self] data, _, _ in
            let rows = data.flatMap { String(data: $0, encoding: .utf8) }.map { Self.parseResolved($0) } ?? []
            DispatchQueue.main.async { self?.historyLoading = false; self?.resolved = rows }
        }.resume()
    }

    static func parseResolved(_ ndjson: String) -> [ResolvedHitl] {
        var prompts: [String: (prompt: String, ts: Double)] = [:]
        var resolvedEvents: [(cid: String, outcome: String, decision: String?, at: Double)] = []
        for line in ndjson.split(separator: "\n") {
            let t = line.trimmingCharacters(in: .whitespaces)
            guard !t.isEmpty, let d = t.data(using: .utf8),
                  let evt = try? JSONSerialization.jsonObject(with: d) as? [String: Any],
                  let type = evt["event_type"] as? String else { continue }
            let payload = (evt["data"] as? [String: Any]) ?? evt
            let ts = (evt["timestamp_ms"] as? Double) ?? (evt["timestamp"] as? Double)
                ?? (payload["timestamp"] as? Double) ?? 0
            guard let cid = correlationId(from: payload) else { continue }
            if type == "HitlRequested" {
                let p = (payload["question"] as? String) ?? (payload["prompt"] as? String)
                    ?? (payload["title"] as? String) ?? "Request"
                prompts[cid] = (p, ts)
            } else if type == "HitlResolved" {
                let outcome = (payload["outcome"] as? String) ?? "responded"
                resolvedEvents.append((cid, outcome, payload["decision"] as? String, ts))
            }
        }
        let out = resolvedEvents.compactMap { r -> ResolvedHitl? in
            guard let req = prompts[r.cid] else { return nil }   // resolved w/o matching request in window
            return ResolvedHitl(correlationId: r.cid, prompt: req.prompt, outcome: r.outcome,
                                decision: r.decision, resolvedAt: r.at)
        }
        return out.sorted { $0.resolvedAt > $1.resolvedAt }
    }

    /// correlation id from an event payload (top-level or nested identifiers).
    static func correlationId(from data: [String: Any]) -> String? {
        if let c = data["correlation_id"] as? String { return c }
        if let c = data["pause_state_id"] as? String { return c }
        if let c = data["approval_id"] as? String { return c }
        let hitl = (data["hitl_request"] as? [String: Any])
            ?? (data["metadata"] as? [String: Any])?["hitl_request"] as? [String: Any]
        if let ids = hitl?["identifiers"] as? [String: Any], let c = ids["correlation_id"] as? String { return c }
        return nil
    }

    private func request(_ urlString: String, includePending: Bool = true,
                         _ completion: @escaping (FeedAttentionResponse?) -> Void) {
        let requestBase = base
        guard let url = URL(string: urlString),
              let pendingURL = URL(string: "\(requestBase)/user-requests") else {
            loadError = "Could not refresh Attention. Check the Magician connection."
            completion(nil)
            return
        }
        // Prepare both requests in the same enrolled scope, with bounded waits.
        func authorized(_ url: URL) -> URLRequest {
            var request = URLRequest(url: url)
            request.timeoutInterval = 15
            MagicianAccess.authorize(&request)
            return request
        }
        let feedRequest = authorized(url)
        let pendingRequest = authorized(pendingURL)
        readJSON(feedRequest, as: FeedAttentionResponse.self) { [weak self] feed in
            guard let self, self.base == requestBase else { completion(nil); return }
            guard let feed else {
                self.loadError = "Could not refresh Attention. Check the connection and retry."
                completion(nil)
                return
            }
            guard includePending else { completion(feed); return }
            self.readJSON(pendingRequest, as: PendingUserRequestsResponse.self) { [weak self] pending in
                guard let self, self.base == requestBase else { completion(nil); return }
                guard let pending else {
                    self.loadError = "Could not load pending questions. Check the connection and retry."
                    completion(nil)
                    return
                }
                self.loadError = nil
                completion(feed.withPendingUserRequests(pending.requests))
            }
        }
    }

    private func readJSON<T: Decodable>(_ request: URLRequest, as type: T.Type,
                                        completion: @escaping (T?) -> Void) {
        networkSession.dataTask(with: request) { data, response, error in
            let status = (response as? HTTPURLResponse)?.statusCode ?? 0
            let decoded = error == nil && (200..<300).contains(status)
                ? data.flatMap { try? JSONDecoder().decode(T.self, from: $0) } : nil
            DispatchQueue.main.async { completion(decoded) }
        }.resume()
    }

    // MARK: - Responding

    private enum HitlResponseOutcome {
        case resolved
        case reask(String)
        case failed(String)
    }

    /// Low-level HITL response POST. Calls back on the main queue with the
    /// protocol outcome and does not refresh the feed. A validation reask is a
    /// live card, while an already-resolved response is a soft success.
    private func postResponse(_ item: AttentionItem, value: [String: Any], selectedPaths: [String] = [],
                              completion: @escaping (HitlResponseOutcome) -> Void) {
        guard let url = URL(string: "\(base)/hitl/\(item.correlationId)/respond") else {
            DispatchQueue.main.async { completion(.failed("The response URL is invalid.")) }; return
        }
        var req = URLRequest(url: url)
        req.httpMethod = "POST"
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        MagicianAccess.authorize(&req)
        var payload: [String: Any] = [
            "source": item.source,
            "channel": "ios",
            "value": value,
            "input_type": item.inputType,
            "correlation_id": item.correlationId,
            "pause_state_id": item.correlationId
        ]
        if !selectedPaths.isEmpty { payload["selected_paths"] = selectedPaths }
        req.httpBody = try? JSONSerialization.data(withJSONObject: payload)
        networkSession.dataTask(with: req) { data, response, error in
            let status = (response as? HTTPURLResponse)?.statusCode ?? 0
            let payload = data.flatMap {
                try? JSONSerialization.jsonObject(with: $0) as? [String: Any]
            }
            let text = data.flatMap { String(data: $0, encoding: .utf8) }?
                .trimmingCharacters(in: .whitespacesAndNewlines)
            let message = (payload?["message"] as? String)
                ?? (payload?["reason"] as? String)
                ?? (text?.isEmpty == false ? text : nil)
            let outcome: HitlResponseOutcome
            if let error {
                outcome = .failed(error.localizedDescription)
            } else if (200..<300).contains(status) {
                if payload?["status"] as? String == "reask_required" {
                    outcome = .reask(message ?? "Please revise your answer.")
                } else if payload?["accepted"] as? Bool == false
                            || payload?["resumed"] as? Bool == false {
                    outcome = .failed(message ?? "The response was not accepted.")
                } else {
                    outcome = .resolved
                }
            } else if (status == 404 || status == 410)
                        && (item.source == "agentic" || item.source == "escalation") {
                outcome = .resolved
            } else if status == 409, payload?["reason"] as? String == "already_resolved" {
                outcome = .resolved
            } else {
                outcome = .failed(message ?? "The response failed (HTTP \(status)).")
            }
            DispatchQueue.main.async {
                // Idempotent with the eager drop performed by the optimistic
                // transaction; also keeps direct low-level callers correct.
                if case .resolved = outcome {
                    PendingHitlTracker.shared.drop(item.correlationId)
                }
                completion(outcome)
            }
        }.resume()
    }

    /// POST /hitl/{id}/respond with the given AgenticResumeValue. The card,
    /// counts, and badge are updated synchronously; the request then runs in the
    /// background. A failed request restores the exact local projection.
    func submit(_ item: AttentionItem, value: [String: Any], selectedPaths: [String] = []) {
        _ = performOptimisticResponse(item, value: value, selectedPaths: selectedPaths)
    }

    @discardableResult
    private func performOptimisticResponse(_ item: AttentionItem,
                                           value: [String: Any],
                                           selectedPaths: [String] = [],
                                           refreshOnSuccess: Bool = true,
                                           completion: ((Bool) -> Void)? = nil) -> Bool {
        let mutationKey = "hitl:\(item.correlationId)"
        guard !item.correlationId.isEmpty,
              let ticket = mutationCoordinator.begin(.hitl(item.correlationId)) else {
            completion?(false)
            return false
        }
        guard pendingCardMutationKeys.insert(mutationKey).inserted else {
            mutationCoordinator.fail(ticket)
            completion?(false)
            return false
        }

        let originalAnchors = removeAttentionItemLocally(item)
        let coordinatorKey = OptimisticCardKey.hitl(item.correlationId)
        optimisticAttentionLanes[coordinatorKey] = Set(originalAnchors.keys)
        let wasTracked = PendingHitlTracker.shared.contains(item.correlationId)
        if wasTracked { PendingHitlTracker.shared.drop(item.correlationId) }
        if let current = counts {
            badgeCount = Self.resolveAttentionBadgeCount(
                pendingHitl: PendingHitlTracker.shared.count,
                needsAction: current.needsAction,
                failed: current.failed
            )
        }
        mutationError = nil

        postResponse(item, value: value, selectedPaths: selectedPaths) { [weak self, mutationCoordinator] outcome in
            guard let self else {
                if case .resolved = outcome { mutationCoordinator.succeed(ticket) }
                else { mutationCoordinator.fail(ticket) }
                completion?(false)
                return
            }
            self.pendingCardMutationKeys.remove(mutationKey)
            switch outcome {
            case .resolved:
                self.removeAttentionItemRowsOnly(item)
                self.mutationCoordinator.succeed(ticket)
                if refreshOnSuccess { self.fetch() }
                completion?(true)
            case .reask(let message):
                self.mutationCoordinator.fail(ticket)
                self.optimisticAttentionLanes[coordinatorKey] = nil
                if wasTracked { PendingHitlTracker.shared.restore(item.correlationId) }
                self.restoreAttentionItemLocally(item, originalAnchors: originalAnchors)
                self.mutationError = "More information is needed: \(message)"
                if refreshOnSuccess { self.fetch() }
                completion?(false)
            case .failed(let message):
                self.mutationCoordinator.fail(ticket)
                self.optimisticAttentionLanes[coordinatorKey] = nil
                if wasTracked { PendingHitlTracker.shared.restore(item.correlationId) }
                self.restoreAttentionItemLocally(item, originalAnchors: originalAnchors)
                self.mutationError = "Could not send this response. \(message) The card has been restored."
                completion?(false)
            }
            self.finishAttentionRollback(originalAnchors)
        }
        return true
    }

    // MARK: - Bulk apply (web parity: "Approve all (N)" for diff_approval items)

    /// Non-failed diff_approval items in a lane — the pool the bulk button applies.
    func diffApprovalItems(in lane: String) -> [AttentionItem] {
        items(lane).filter { $0.isActionable && $0.inputType == "diff_approval" }
    }

    /// Apply every non-failed diff_approval item in the lane (each an "apply" choice),
    /// then refresh once. Mirrors web `approveAllDiffApprovals` incl. the applied/failed notice.
    func approveAllDiffApprovals(in lane: String) {
        guard !approvingAllDiffs else { return }
        let targets = diffApprovalItems(in: lane)
        guard !targets.isEmpty else { return }
        approvingAllDiffs = true
        bulkNotice = nil
        var applied = 0
        var failed = 0
        let group = DispatchGroup()
        for item in targets {
            group.enter()
            _ = performOptimisticResponse(
                item,
                value: ["type": "choice", "selected_id": "apply"],
                refreshOnSuccess: false
            ) { ok in
                if ok { applied += 1 } else { failed += 1 }   // completions run on main → serialized
                group.leave()
            }
        }
        group.notify(queue: .main) { [weak self] in
            guard let self = self else { return }
            self.approvingAllDiffs = false
            self.bulkNotice = failed == 0
                ? "Applied \(applied) code change set\(applied == 1 ? "" : "s")."
                : "Applied \(applied); \(failed) failed."
            self.fetch()
        }
    }

    func submitText(_ item: AttentionItem, _ text: String) {
        submit(item, value: ["type": "text", "value": text])
    }
    func submitPassword(_ item: AttentionItem, _ value: String) {
        submit(item, value: ["type": "password", "value": value])
    }
    func submitGuidance(_ item: AttentionItem, _ advice: String) {
        submit(item, value: ["type": "guidance", "advice": advice])
    }
    func submitChoice(_ item: AttentionItem, optionId: String, otherValue: String? = nil) {
        var v: [String: Any] = ["type": "choice", "selected_id": optionId]
        if let other = otherValue, !other.isEmpty { v["other_value"] = other }
        submit(item, value: v)
    }
    func submitMultiChoice(_ item: AttentionItem, ids: [String]) {
        submit(item, value: ["type": "multi_choice", "selected_ids": ids])
    }
    func submitConfirmation(_ item: AttentionItem, confirmed: Bool) {
        submit(item, value: ["type": "confirmation", "confirmed": confirmed])
    }
    func submitFilePaths(_ item: AttentionItem, paths: [String]) {
        submit(item, value: ["type": "file_path", "paths": paths])
    }
    func submitExternalDone(_ item: AttentionItem, guidance: String?) {
        var v: [String: Any] = ["type": "external_action_completed"]
        if let g = guidance, !g.isEmpty { v["guidance"] = g }
        submit(item, value: v)
    }
    /// diff_approval is a choice of "apply"/"reject"; empty paths applies the whole proposal.
    func submitDiffApproval(_ item: AttentionItem, apply: Bool, paths: [String] = []) {
        submit(item,
               value: ["type": "choice", "selected_id": apply ? "apply" : "reject"],
               selectedPaths: apply ? paths : [])
    }
    func submitForm(_ item: AttentionItem, answers: [[String: Any]]) {
        submit(item, value: ["type": "form", "answers": answers])
    }
    func cancel(_ item: AttentionItem, reason: String? = nil) {
        var value: [String: Any] = ["type": "aborted"]
        if let reason = reason, !reason.isEmpty { value["reason"] = reason }
        submit(item, value: value)
    }
}
