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
    /// `external_action` / `mcp_oauth` — the page the reader has to open
    /// (an OAuth consent screen for `mcp_oauth`). `authorize_url` is the
    /// payload's older spelling of the same link.
    var url: String? = nil
    var authorizeUrl: String? = nil
    /// The producer's free-form context; only its review link
    /// (`review_href` / `review_label`) is read, leniently.
    var context: HitlReviewContextData? = nil
    /// What raised a confirmation when it is not an ordinary approval.
    var escalationTrigger: String? = nil
    /// Public alignment projection; its reason is read privately.
    var alignment: AlignmentProjection? = nil

    enum CodingKeys: String, CodingKey {
        case type, options, placeholder, suggestions, destructive, instructions, multiple, filter
        case command, violation, questions, sensitive, url, context, alignment
        case escalationTrigger = "escalation_trigger"
        case requestType = "request_type"
        case authorizeUrl = "authorize_url"
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

extension AttentionFormQuestion {
    /// Answered by picking options (`choice` / `multi_choice` with options on
    /// the wire) — posted as `selected_ids`. Without options it is a text field.
    var answersByOptions: Bool {
        ["choice", "multi_choice"].contains(inputType ?? "") && !(options ?? []).isEmpty
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
///
/// A delegated child's ask (C2) and an `mcp_oauth` ask (C4) carry their
/// whole contract here — `correlation_id`, `execution_id` (the CHILD's run),
/// `prompt`, `input_type`, `input_schema` — flat, not under `identifiers`.
/// Decoded field by field so one odd field never drops the row.
struct HitlRequestPayload: Codable, Equatable {
    var identifiers: HitlIdentifiers? = nil
    var source: String? = nil
    var inputType: String? = nil
    var correlationId: String? = nil
    var executionId: String? = nil
    /// The backend's canonical placement: `scope.execution_id` is the run the
    /// answer resumes (a delegated child's own run).
    var scope: [String: String?]? = nil
    var scopeExecutionId: String? { scope?["execution_id"] ?? nil }
    var prompt: String? = nil
    var inputSchema: AttentionInputSchema? = nil
    enum CodingKeys: String, CodingKey {
        case identifiers, source, prompt, scope
        case inputType = "input_type"
        case correlationId = "correlation_id"
        case executionId = "execution_id"
        case inputSchema = "input_schema"
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        identifiers = try? c.decodeIfPresent(HitlIdentifiers.self, forKey: .identifiers)
        source = try? c.decodeIfPresent(String.self, forKey: .source)
        inputType = try? c.decodeIfPresent(String.self, forKey: .inputType)
        correlationId = try? c.decodeIfPresent(String.self, forKey: .correlationId)
        executionId = try? c.decodeIfPresent(String.self, forKey: .executionId)
        scope = try? c.decodeIfPresent([String: String?].self, forKey: .scope)
        prompt = try? c.decodeIfPresent(String.self, forKey: .prompt)
        inputSchema = try? c.decodeIfPresent(AttentionInputSchema.self, forKey: .inputSchema)
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
    /// The run the ask belongs to — for a delegated child's ask (C2), the
    /// child's execution — and the owning (parent) task.
    var executionId: String? = nil
    var taskId: String? = nil
    /// C2: the ask came from an active delegated child of this task.
    var delegationChild: Bool? = nil
    var delegatedAgentId: String? = nil
    /// C3: `waiting_without_question` — the run waits on the user but no ask
    /// reached any surface. `actions` names what the card offers.
    var attentionReason: String? = nil
    var actions: [String]? = nil

    enum CodingKeys: String, CodingKey {
        case source, question, hint, options, files, actions
        case attentionKind = "attention_kind"
        case inputType = "input_type"
        case inputSchema = "input_schema"
        case pauseStateId = "pause_state_id"
        case hitlRequest = "hitl_request"
        case reviewHref = "review_href"
        case reviewLabel = "review_label"
        case executionId = "execution_id"
        case taskId = "task_id"
        case delegationChild = "delegation_child"
        case delegatedAgentId = "delegated_agent_id"
        case attentionReason = "attention_reason"
    }
}

extension AttentionMetadata {
    /// Field-by-field decode: `metadata` is free-form, and one field of an
    /// unexpected shape used to drop the WHOLE metadata object — the row then
    /// rendered as a bare title with nowhere to post its answer.
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        source = try? c.decodeIfPresent(String.self, forKey: .source)
        attentionKind = try? c.decodeIfPresent(String.self, forKey: .attentionKind)
        inputType = try? c.decodeIfPresent(String.self, forKey: .inputType)
        inputSchema = try? c.decodeIfPresent(AttentionInputSchema.self, forKey: .inputSchema)
        question = try? c.decodeIfPresent(String.self, forKey: .question)
        hint = try? c.decodeIfPresent(String.self, forKey: .hint)
        options = try? c.decodeIfPresent([EscalationOptionData].self, forKey: .options)
        pauseStateId = try? c.decodeIfPresent(String.self, forKey: .pauseStateId)
        hitlRequest = try? c.decodeIfPresent(HitlRequestPayload.self, forKey: .hitlRequest)
        reviewHref = try? c.decodeIfPresent(String.self, forKey: .reviewHref)
        reviewLabel = try? c.decodeIfPresent(String.self, forKey: .reviewLabel)
        files = try? c.decodeIfPresent([AttentionDiffFile].self, forKey: .files)
        executionId = try? c.decodeIfPresent(String.self, forKey: .executionId)
        taskId = try? c.decodeIfPresent(String.self, forKey: .taskId)
        delegationChild = try? c.decodeIfPresent(Bool.self, forKey: .delegationChild)
        delegatedAgentId = try? c.decodeIfPresent(String.self, forKey: .delegatedAgentId)
        attentionReason = try? c.decodeIfPresent(String.self, forKey: .attentionReason)
        actions = try? c.decodeIfPresent([String].self, forKey: .actions)
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
    /// The owning task (for a delegated child's ask, the PARENT task) and the
    /// run, when the feed stamps them on the row itself.
    var taskId: String? = nil
    var executionId: String? = nil
    /// The conversation thread the row belongs to (the feed stamps it on the
    /// row itself): the composer ask dock matches it against its own thread.
    var uiThreadId: String? = nil

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
        taskId = try? c.decodeIfPresent(String.self, forKey: .taskId)
        executionId = try? c.decodeIfPresent(String.self, forKey: .executionId)
        uiThreadId = try? c.decodeIfPresent(String.self, forKey: .uiThreadId)
    }

    enum CodingKeys: String, CodingKey {
        case id, title, summary, status, metadata
        case itemType = "item_type"
        case updatedAt = "updated_at"
        case taskId = "task_id"
        case executionId = "execution_id"
        case uiThreadId = "ui_thread_id"
    }

    /// The ask's schema: the row's own, else the one inside `hitl_request`
    /// (a delegated child's ask and an `mcp_oauth` ask may carry only that).
    var schema: AttentionInputSchema? { metadata?.inputSchema ?? metadata?.hitlRequest?.inputSchema }

    // MARK: routing (C2 delegated child asks)

    /// The run the answer resumes. For a delegated child's ask this is the
    /// CHILD's execution (`hitl_request.execution_id`), never the parent's.
    var responseExecutionId: String? {
        [metadata?.hitlRequest?.scopeExecutionId, metadata?.hitlRequest?.executionId,
         metadata?.executionId, executionId]
            .compactMap { $0 }.first { !$0.isEmpty }
    }
    /// The owning task — the parent task for a delegated child's ask.
    var owningTaskId: String? {
        [taskId, metadata?.taskId].compactMap { $0 }.first { !$0.isEmpty }
    }
    var isDelegatedChildAsk: Bool { metadata?.delegationChild == true }
    /// "from <agent>" for a delegated child's ask, nil otherwise.
    var delegatedFromLabel: String? {
        guard isDelegatedChildAsk else { return nil }
        let agent = (metadata?.delegatedAgentId ?? "").trimmingCharacters(in: .whitespaces)
        return agent.isEmpty ? "from a delegated agent" : "from \(agent)"
    }

    // MARK: stalled wait (C3)

    /// The run waits on the user but no question was delivered. Not an ask:
    /// there is nothing to answer, only a run to cancel or open.
    var isStalledWait: Bool { metadata?.attentionReason == "waiting_without_question" }
    /// The actions the backend offers on a stalled wait; both by default.
    var stalledWaitActions: [String] {
        let actions = metadata?.actions ?? []
        return actions.isEmpty ? ["cancel", "open_task"] : actions
    }

    // MARK: external action / mcp_oauth (C4)

    /// The page an external action (an `mcp_oauth` consent screen) opens.
    var externalActionURL: URL? {
        [metadata?.inputSchema?.url, metadata?.hitlRequest?.inputSchema?.url,
         metadata?.inputSchema?.authorizeUrl, metadata?.hitlRequest?.inputSchema?.authorizeUrl]
            .compactMap { $0 }
            .first { !$0.isEmpty }
            .flatMap { URL(string: $0) }
            .flatMap { ["http", "https"].contains($0.scheme?.lowercased() ?? "") ? $0 : nil }
    }
    var isMcpOAuth: Bool { source == "mcp_oauth" }

    // MARK: preference alignment hold (P6.2)

    /// The hold's public projection, when this confirmation is a preference
    /// alignment hold. The answer goes as one of its typed choices.
    var alignment: AlignmentProjection? {
        guard let projection = schema?.alignment, projection.isAlignment else { return nil }
        return projection
    }

    /// The short kind shown on the list row.
    var rowKindLabel: String {
        if isStalledWait { return "stalled" }
        if isMcpOAuth { return "sign-in" }
        return inputType
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
            ?? metadata?.hitlRequest?.correlationId
            ?? metadata?.pauseStateId
            ?? ids?.pauseStateId
            ?? ids?.requestId
            ?? ids?.approvalId
            ?? "\(id)-hitl"
    }

    /// text / password / choice / multi_choice / confirmation / guidance / diff_approval / …
    var inputType: String {
        if itemType == "approval" { return "confirmation" }
        // C4: an OAuth sign-in is answered by going to the page and saying so.
        if source == "mcp_oauth" { return "external_action" }
        return metadata?.inputSchema?.type ?? metadata?.inputType
            ?? metadata?.hitlRequest?.inputSchema?.type ?? metadata?.hitlRequest?.inputType ?? "text"
    }

    var options: [EscalationOptionData] {
        schema?.options ?? metadata?.options ?? []
    }
    var prompt: String {
        let asked = metadata?.question ?? metadata?.hitlRequest?.prompt ?? ""
        return asked.isEmpty ? title : asked
    }
    var hint: String? { metadata?.hint ?? summary }
    var placeholder: String { schema?.placeholder ?? "Type your response…" }
    var confirmLabel: String { schema?.confirmLabel ?? "Approve" }
    var denyLabel: String { schema?.denyLabel ?? "Reject" }
    var isActionable: Bool { !["failed", "running"].contains(itemType) }

    /// Batched-clarification position, e.g. "Step 2 of 3" — nil when not chained.
    var chainLabel: String? {
        guard let pos = schema?.chainPosition,
              let total = schema?.chainTotal, total > 1 else { return nil }
        return "Step \(pos) of \(total)"
    }

    /// A deep link to the item's source (execution / plan) if the backend stamped one.
    var reviewURL: URL? { (metadata?.reviewHref).flatMap { URL(string: $0) } }
    var reviewLabel: String { metadata?.reviewLabel ?? "View source" }

    /// What the ask asks the reader to review before answering — the
    /// producer's `input_schema.context.review_href` first (the Relentless
    /// accepted-draft review: "Review draft" → Work Quality), else the row's
    /// own `review_href`. Opened in-app when it names a Work destination.
    var reviewLink: HitlReviewLink? {
        if let context = schema?.context,
           let link = HitlReviewLink(href: context.reviewHref, label: context.reviewLabel) {
            return link
        }
        return HitlReviewLink(href: metadata?.reviewHref, label: metadata?.reviewLabel ?? "View source")
    }

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
        return (grantKind == "sandbox" ? schema?.command : schema?.toolName) ?? ""
    }

    /// Why permission is needed, verbatim.
    var grantDetail: String? {
        let value = (grantKind == "sandbox" ? schema?.violation : schema?.paramsSummary) ?? ""
        return value.isEmpty ? nil : value
    }

    /// File roots the grant would cover. Empty for shell overrides.
    var grantRoots: [String] { schema?.allowedRoots ?? [] }

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
        let value = schema?.instructions ?? ""
        return value.isEmpty ? nil : value
    }
    var externalDoneLabel: String { schema?.doneLabel ?? (isMcpOAuth ? "I've finished" : "I've completed this") }

    /// `file_path` — whether several are wanted, and the filter the ask named.
    var wantsMultiplePaths: Bool { schema?.multiple == true }
    var pathFilter: String? {
        let value = schema?.filter ?? ""
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
    var isDestructive: Bool { schema?.destructive == true }
    var formQuestions: [AttentionFormQuestion] { schema?.questions ?? [] }

    /// The `answers` of a `form` response, one entry per question:
    /// `{id, skipped: true}`, `{id, selected_ids}` for an option question (in
    /// the options' own order), or `{id, value}`.
    func formAnswers(values: [String: String], selections: [String: Set<String>],
                     skipped: Set<String>) -> [[String: Any]] {
        formQuestions.map { question in
            if skipped.contains(question.id) { return ["id": question.id, "skipped": true] }
            if question.answersByOptions {
                let picked = selections[question.id] ?? []
                let ids = (question.options ?? []).map(\.id).filter { picked.contains($0) }
                return ["id": question.id, "selected_ids": ids]
            }
            return ["id": question.id, "value": values[question.id] ?? ""]
        }
    }

    // MARK: Secrets (P3)

    /// The backend's value-free spec, when this ask collects a secret.
    var sensitiveSpec: ChatSensitiveSpecData? { schema?.sensitive }
    /// Whether the ask collects a secret: the spec, or the two built-in secure
    /// browser request types that predate it.
    var isSensitive: Bool {
        if sensitiveSpec != nil { return true }
        let requestType = schema?.requestType ?? ""
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
    /// Correlation ids a realtime resolution event named while this client had
    /// not answered them — the ask was settled on another surface or device.
    /// An open answer sheet for one of these closes, and a submit for one is
    /// refused rather than posted (see `isAnsweredElsewhere`).
    @Published private(set) var remotelyResolved: Set<String> = []
    /// Item ids and correlation ids this view model removed itself (an answer,
    /// a cancel, a dismiss). Its own optimistic removal makes the row leave the
    /// lanes too, and that must not read as "answered elsewhere". Cleared when
    /// the mutation fails and the card is restored — the ask is pending again.
    private var locallySettled: Set<String> = []
    /// Correlation ids another authoritative read (the task's execution panel)
    /// reported pending while the feed's loaded pages did not carry them.
    /// Their absence from the lanes is not an answer given elsewhere.
    private var vouchedPending: Set<String> = []
    /// The copy of the brief notice an open sheet leaves behind when it closes
    /// because the ask was settled elsewhere.
    static let answeredElsewhereNotice = "Answered elsewhere"

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
                // A stalled wait (C3) has no ask and so no correlation id to track.
                .filter { !$0.isStalledWait }
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

    // MARK: - Answered elsewhere (an open sheet whose ask settled on another surface)

    /// Whether `item` — typically the one an answer sheet is showing — has been
    /// settled somewhere other than this view model: a realtime resolution
    /// named it, or a completed refetch no longer carries it in any lane.
    func isAnsweredElsewhere(_ item: AttentionItem) -> Bool {
        isAnsweredElsewhere(item, lanes: lanes, remotelyResolved: remotelyResolved)
    }

    /// The same check against lanes / resolutions a `@Published` publisher has
    /// just emitted — it fires in `willSet`, before the property holds them.
    func isAnsweredElsewhere(_ item: AttentionItem,
                             lanes: [String: [AttentionItem]]?,
                             remotelyResolved: Set<String>?) -> Bool {
        Self.answeredElsewhere(item, lanes: lanes ?? self.lanes, hasLoaded: pages != nil,
                               remotelyResolved: remotelyResolved ?? self.remotelyResolved,
                               locallySettled: locallySettled, vouchedPending: vouchedPending)
    }

    /// Record that `item` is pending per another authoritative read (the task
    /// panel's `needs_attention`), so answering it from there is not refused
    /// as stale just because the feed's loaded pages do not carry it. A
    /// realtime resolution still wins.
    func vouchPending(_ item: AttentionItem) {
        let cid = item.correlationId
        guard !cid.isEmpty else { return }
        vouchedPending.insert(cid)
    }

    /// Pure core of `isAnsweredElsewhere`.
    ///
    /// The raw lanes are read, not `items(_:)`: the suppression overlay hides
    /// rows a local mutation is settling, and that is not someone else's
    /// answer. Every lane is searched, so a row that moved lanes is still
    /// pending. Before the first successful fetch nothing can be concluded from
    /// absence; a failed fetch leaves the lanes untouched, so it cannot either.
    static func answeredElsewhere(_ item: AttentionItem,
                                  lanes: [String: [AttentionItem]],
                                  hasLoaded: Bool,
                                  remotelyResolved: Set<String>,
                                  locallySettled: Set<String>,
                                  vouchedPending: Set<String> = []) -> Bool {
        let cid = item.correlationId
        if locallySettled.contains(item.id) || (!cid.isEmpty && locallySettled.contains(cid)) {
            return false
        }
        if !cid.isEmpty && remotelyResolved.contains(cid) { return true }
        guard hasLoaded, cid.isEmpty || !vouchedPending.contains(cid) else { return false }
        let present = lanes.values.contains { rows in
            rows.contains { $0.id == item.id || (!cid.isEmpty && $0.correlationId == cid) }
        }
        return !present
    }

    /// The correlation id a realtime frame resolves, or `nil` when the frame is
    /// not a resolution. Shares `PendingHitlTracker`'s predicate and layered
    /// key extraction (the web `pendingHitlStore` port), and also accepts the
    /// PascalCase `Hitl*` / `Attention*` / `UserRequest*` resolution spellings.
    static func resolvedCorrelationId(fromEventText text: String) -> String? {
        guard let data = text.data(using: .utf8),
              let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let type = obj["event_type"] as? String, !type.isEmpty else { return nil }
        let pascalResolution = ["Hitl", "Attention", "UserRequest"].contains { type.hasPrefix($0) }
            && ["Resolved", "Expired", "Cancelled", "Dismissed", "Answered"].contains { type.hasSuffix($0) }
        guard PendingHitlTracker.isResolutionEvent(type) || pascalResolution else { return nil }
        return PendingHitlTracker.correlationKey(obj)
    }

    /// The correlation id a `HitlRequested` frame raises, or `nil`.
    static func requestedCorrelationId(fromEventText text: String) -> String? {
        guard let data = text.data(using: .utf8),
              let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              obj["event_type"] as? String == "HitlRequested" else { return nil }
        return PendingHitlTracker.correlationKey(obj)
    }

    /// A fresh ask under a correlation id this client saw resolved is pending
    /// again — forget the resolution so it can be answered.
    func noteRequested(correlationId: String) {
        remotelyResolved.remove(correlationId)
    }

    /// Record a resolution from the realtime stream. Ignored for an ask this
    /// view model is answering itself — that is its own answer arriving back.
    func noteRemoteResolution(correlationId: String) {
        guard !correlationId.isEmpty, !locallySettled.contains(correlationId) else { return }
        remotelyResolved.insert(correlationId)
    }

    /// Whether no surface should offer this ask again: this view model answered
    /// it (or is answering it), or a realtime resolution settled it. Unlike
    /// `isAnsweredElsewhere` this never reads absence from the lanes, so it is
    /// safe for an ask the feed's loaded pages do not carry (the task panel's
    /// own `needs_attention` rows).
    func isSettled(_ item: AttentionItem) -> Bool {
        let cid = item.correlationId
        if locallySettled.contains(item.id) || (!cid.isEmpty && locallySettled.contains(cid)) { return true }
        if !cid.isEmpty && remotelyResolved.contains(cid) { return true }
        return isAttentionSuppressed(item)
    }

    /// The same, by correlation id alone (a chat escalation card).
    func isSettled(correlationId: String) -> Bool {
        !correlationId.isEmpty
            && (locallySettled.contains(correlationId) || remotelyResolved.contains(correlationId)
                || mutationCoordinator.isSuppressed(.hitl(correlationId)))
    }

    /// Leave the brief "Answered elsewhere" notice, cleared after a few seconds
    /// unless something else has replaced it.
    func showAnsweredElsewhereNotice() {
        bulkNotice = Self.answeredElsewhereNotice
        DispatchQueue.main.asyncAfter(deadline: .now() + 4) { [weak self] in
            guard let self, self.bulkNotice == Self.answeredElsewhereNotice else { return }
            self.bulkNotice = nil
        }
    }

    // MARK: - Realtime (web parity: live inbox on HITL events)

    private var realtimeSubscription: RealtimeHubSubscription?
    private var reloadWork: DispatchWorkItem?

    /// Subscribe to the app's realtime hub and refresh (debounced) when a HITL
    /// request is raised or resolved, so the inbox stays live instead of
    /// pull-to-refresh only. The hub owns the one socket, its reconnects and
    /// resume; when events may have been missed it asks for a resync, which
    /// refetches the inbox.
    func connectRealtime() {
        guard !isRunningUnderTests else { return }   // no realtime in unit tests
        guard realtimeSubscription == nil else { return }
        realtimeSubscription = RealtimeHub.shared.subscribe(
            onEvent: { [weak self] event in self?.handleRealtimeText(event.text) },
            onResync: { [weak self] in self?.fetch() }
        )
    }

    func disconnectRealtime() {
        realtimeSubscription?.cancel()
        realtimeSubscription = nil
    }

    /// Route one realtime frame (delivered on the main thread by the hub).
    func handleRealtimeText(_ text: String) {
        if let resolved = Self.resolvedCorrelationId(fromEventText: text) {
            noteRemoteResolution(correlationId: resolved)
            scheduleReload()
        } else if let reissued = Self.requestedCorrelationId(fromEventText: text) {
            noteRequested(correlationId: reissued)
            scheduleReload()
        } else if isHitlEvent(text) {
            scheduleReload()
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

    // MARK: - Stalled wait (C3): cancel the run that waits on no question

    /// `POST /executions/{id}/cancel` for a `waiting_without_question` card.
    /// There is no ask to answer, so this never touches `/hitl/…/respond`. The
    /// card leaves optimistically and is restored if the cancel fails.
    func cancelStalledExecution(_ item: AttentionItem) {
        let pathSegment = CharacterSet.alphanumerics.union(CharacterSet(charactersIn: "-._~"))
        guard let executionId = item.responseExecutionId,
              let encoded = executionId.addingPercentEncoding(withAllowedCharacters: pathSegment),
              let url = URL(string: "\(base)/executions/\(encoded)/cancel") else {
            mutationError = "This run has no execution id to cancel. Open the task instead."
            return
        }
        let mutationKey = "attention:\(item.id)"
        guard let ticket = mutationCoordinator.begin(.attention(item.id)) else { return }
        guard pendingCardMutationKeys.insert(mutationKey).inserted else {
            mutationCoordinator.fail(ticket)
            return
        }
        let originalAnchors = removeAttentionItemLocally(item)
        optimisticAttentionLanes[.attention(item.id)] = Set(originalAnchors.keys)
        mutationError = nil
        var req = URLRequest(url: url)
        req.httpMethod = "POST"
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        MagicianAccess.authorize(&req)
        networkSession.dataTask(with: req) { [weak self, mutationCoordinator] _, response, error in
            let status = (response as? HTTPURLResponse)?.statusCode ?? 0
            let succeeded = error == nil && (200..<300).contains(status)
            DispatchQueue.main.async {
                guard let self else {
                    if succeeded { mutationCoordinator.succeed(ticket) } else { mutationCoordinator.fail(ticket) }
                    return
                }
                self.pendingCardMutationKeys.remove(mutationKey)
                if succeeded {
                    self.removeAttentionItemRowsOnly(item)
                    self.mutationCoordinator.succeed(ticket)
                    self.fetch()
                } else {
                    self.mutationCoordinator.fail(ticket)
                    self.optimisticAttentionLanes[.attention(item.id)] = nil
                    self.restoreAttentionItemLocally(item, originalAnchors: originalAnchors)
                    self.mutationError = "Could not cancel this run (HTTP \(status)). The card has been restored."
                }
                self.finishAttentionRollback(originalAnchors)
            }
        }.resume()
    }

    private func removeAttentionItemLocally(_ item: AttentionItem) -> [String: OptimisticCardListAnchor] {
        // Before the rows leave: an open sheet watching `lanes` must see this
        // removal as this client's own, never as "answered elsewhere".
        locallySettled.insert(item.id)
        if !item.correlationId.isEmpty { locallySettled.insert(item.correlationId) }
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
        locallySettled.remove(item.id)
        if !item.correlationId.isEmpty { locallySettled.remove(item.correlationId) }
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
        // Web `resolveHitlCall` parity. A delegated child's ask (C2) resumes the
        // CHILD's run, so its execution id rides the answer; planning sources
        // also name the task that owns the question.
        if let executionId = item.responseExecutionId { payload["execution_id"] = executionId }
        if let taskId = item.owningTaskId, ["clarification", "plan_approval"].contains(item.source) {
            payload["task_id"] = taskId
        }
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
        // Never post a stale answer: an ask settled on another surface has
        // nothing left to answer, and the reader is told so instead.
        if !item.correlationId.isEmpty && isAnsweredElsewhere(item) {
            showAnsweredElsewhereNotice()
            completion?(false)
            return false
        }
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
    /// A preference alignment hold's answer: a typed choice (with a new
    /// wording or a readback token riding in `other_value`), or the owner's
    /// own words for `respond`.
    func submitAlignment(_ item: AttentionItem, choiceId: String, input: String? = nil) {
        guard let value = AlignmentAsk.answerValue(choiceId: choiceId, input: input) else { return }
        submit(item, value: value)
    }

    enum AlignmentDetailOutcome: Equatable {
        case ready(AlignmentDetail)
        /// Answered or replaced elsewhere.
        case gone
        case unavailable
    }

    /// The owner-private read of why a hold was raised (iOS is a private
    /// surface in the v1 canary).
    func fetchAlignmentDetail(_ item: AttentionItem,
                              completion: @escaping (AlignmentDetailOutcome) -> Void) {
        guard let url = AlignmentAsk.detailURL(base: base, correlationId: item.correlationId) else {
            DispatchQueue.main.async { completion(.unavailable) }; return
        }
        var req = URLRequest(url: url)
        req.cachePolicy = .reloadIgnoringLocalCacheData
        req.setValue("application/json", forHTTPHeaderField: "Accept")
        MagicianAccess.authorize(&req)
        networkSession.dataTask(with: req) { data, response, _ in
            let status = (response as? HTTPURLResponse)?.statusCode ?? 0
            let outcome: AlignmentDetailOutcome
            if status == 404 || status == 409 {
                outcome = .gone
            } else if (200..<300).contains(status),
                      let data,
                      let detail = try? JSONDecoder().decode(AlignmentDetail.self, from: data) {
                outcome = .ready(detail)
            } else {
                outcome = .unavailable
            }
            DispatchQueue.main.async { completion(outcome) }
        }.resume()
    }
    /// A plan approval's answer (design §2 "Plan approval"). Reject and Approve
    /// post the confirmation like any other; Approve & Run then starts the
    /// task's run through `PlanApprovalActions`. The approval stands when the
    /// start fails, and the notice says so. Review posts nothing (the view
    /// opens the plan).
    func submitPlanApproval(_ item: AttentionItem, choice: PlanApprovalChoice,
                            client: PlanApprovalClient = LivePlanApprovalClient()) {
        guard let confirmed = choice.confirmed else { return }
        let value: [String: Any] = ["type": "confirmation", "confirmed": confirmed]
        guard choice == .approveAndRun else {
            _ = performOptimisticResponse(item, value: value) { [weak self] ok in
                guard ok, let self else { return }
                self.bulkNotice = (confirmed ? PlanApprovalOutcome.approved : .rejected).message
            }
            return
        }
        let taskId = item.owningTaskId
        Task { @MainActor [weak self] in
            let outcome = await PlanApprovalActions.approveAndRun(
                taskId: taskId,
                approve: {
                    await withCheckedContinuation { continuation in
                        guard let self else { continuation.resume(returning: .failed("")); return }
                        // The completion runs exactly once on every path,
                        // including the refused-duplicate guard.
                        _ = self.performOptimisticResponse(item, value: value) { ok in
                            continuation.resume(returning: ok ? .ok : .failed(""))
                        }
                    }
                },
                startRun: { try await client.startRun(taskId: $0) }
            )
            guard let self else { return }
            switch outcome {
            case .approvedAndStarted: self.bulkNotice = outcome.message
            case .approvedRunNotStarted: self.mutationError = outcome.message
            // A failed approval already restored the card and set its own error.
            default: break
            }
        }
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
    /// `submitDiffApproval` that waits for the server: nil once the answer is
    /// accepted, else why it was not (VibeDev's Approve shows it).
    func answerDiffApproval(_ item: AttentionItem, apply: Bool) async -> String? {
        let accepted = await withCheckedContinuation { continuation in
            performOptimisticResponse(item,
                                      value: ["type": "choice", "selected_id": apply ? "apply" : "reject"],
                                      completion: { continuation.resume(returning: $0) })
        }
        return accepted ? nil : (mutationError ?? "Could not send this answer. Open Attention to check it.")
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
