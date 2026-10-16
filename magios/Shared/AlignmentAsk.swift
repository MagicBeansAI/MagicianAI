import Foundation

/// A preference alignment hold (P6.2) — the iOS port of the web's
/// `ui/unified-ui/src/lib/hitl/alignmentAsk.ts`.
///
/// A hold is an ordinary `confirmation` ask whose `input_schema` carries
/// `escalation_trigger: "preference_alignment"` and an `alignment` block: the
/// public projection (choice ids, non-revealing labels, deadline, stage). The
/// shared event never says *why*. iOS is one of the owner-private surfaces in
/// the v1 canary, so it reads the reason, the cited preferences and any
/// wording readback from `GET /hitl/{id}/alignment?surface=ios`.
///
/// Every answer is a typed choice or the owner's own text — never a label:
/// - a choice posts `{type: choice, selected_id}`;
/// - "change my preference" carries the new wording in `other_value`;
/// - "save this wording" carries the readback's token in `other_value`, which
///   only the private read shows;
/// - a typed reply posts `{type: text, value}` — an instruction, not consent.
struct AlignmentChoice: Codable, Equatable, Identifiable {
    let id: String
    let label: String
}

/// `input_schema.alignment` — safe on any surface.
struct AlignmentProjection: Codable, Equatable {
    let kind: String
    let askId: String
    let purpose: String?
    let choices: [AlignmentChoice]
    let freeText: Bool?
    let deadlineAtMs: Int64?
    let stage: String?

    enum CodingKeys: String, CodingKey {
        case kind, purpose, choices, stage
        case askId = "ask_id"
        case freeText = "free_text"
        case deadlineAtMs = "deadline_at_ms"
    }

    init(kind: String, askId: String, purpose: String?, choices: [AlignmentChoice],
         freeText: Bool?, deadlineAtMs: Int64?, stage: String?) {
        self.kind = kind
        self.askId = askId
        self.purpose = purpose
        self.choices = choices
        self.freeText = freeText
        self.deadlineAtMs = deadlineAtMs
        self.stage = stage
    }

    /// Never throws: a malformed block must not fail the whole attention
    /// row's decode. It reads as "not an alignment hold" instead.
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        kind = (try? c.decodeIfPresent(String.self, forKey: .kind)) ?? ""
        askId = (try? c.decodeIfPresent(String.self, forKey: .askId)) ?? ""
        purpose = try? c.decodeIfPresent(String.self, forKey: .purpose)
        choices = (try? c.decodeIfPresent([AlignmentChoice].self, forKey: .choices)) ?? []
        freeText = try? c.decodeIfPresent(Bool.self, forKey: .freeText)
        deadlineAtMs = try? c.decodeIfPresent(Int64.self, forKey: .deadlineAtMs)
        stage = try? c.decodeIfPresent(String.self, forKey: .stage)
    }

    var isAlignment: Bool { kind == "preference_alignment" && !askId.isEmpty && !choices.isEmpty }
    var isWordingStage: Bool { stage == "confirm_wording" }
    /// The act's choices: everything that is not a lasting change, which
    /// needs the private readback.
    var actChoices: [AlignmentChoice] {
        choices.filter { !AlignmentAsk.lastingChoiceIds.contains($0.id) }
    }
}

struct AlignmentDetailChoice: Decodable, Equatable, Identifiable {
    let id: String
    let label: String
    let lasting: Bool
    let consent: Bool
    let needsWording: Bool

    enum CodingKeys: String, CodingKey {
        case id, label, lasting, consent
        case needsWording = "needs_wording"
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        label = try c.decode(String.self, forKey: .label)
        lasting = (try? c.decodeIfPresent(Bool.self, forKey: .lasting)) ?? false
        consent = (try? c.decodeIfPresent(Bool.self, forKey: .consent)) ?? false
        needsWording = (try? c.decodeIfPresent(Bool.self, forKey: .needsWording)) ?? false
    }
}

struct AlignmentIntention: Decodable, Equatable, Identifiable {
    let id: String
    let statement: String
    let assertionStatus: String?

    enum CodingKeys: String, CodingKey {
        case id, statement
        case assertionStatus = "assertion_status"
    }

    /// Who the preference came from, in the owner's words.
    var sourceLabel: String {
        switch assertionStatus {
        case "direct_owner_statement", "confirmed": return "You said"
        case "inferred": return "Magician noticed"
        default: return "Noted"
        }
    }
}

struct AlignmentReadback: Decodable, Equatable {
    let kind: String
    let wording: String
    let superseded: String?
    let token: String
}

/// Another gate a combined ask also settles (plan §9.4), with that gate's
/// own reason. Allowing the act approves it for this gate too.
struct AlignmentCoveredRequirement: Decodable, Equatable, Identifiable {
    let gate: String
    let reason: String

    var id: String { gate }

    enum CodingKeys: String, CodingKey { case gate, reason }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        gate = try c.decode(String.self, forKey: .gate)
        reason = (try? c.decodeIfPresent(String.self, forKey: .reason)) ?? ""
    }

    /// The gate's reason, or plain words for it when the server sent none.
    var line: String {
        if !reason.isEmpty { return reason }
        switch gate {
        case "approval_rule", "plane_approval": return "One of your approval rules matched it."
        case "criticality": return "It is a high-impact action."
        default: return "It was marked as needing confirmation."
        }
    }
}

struct AlignmentRemember: Decodable, Equatable {
    let id: String
    let statement: String
}

/// The owner-private read's `alignment` block.
struct AlignmentPrivateProjection: Decodable, Equatable {
    let stage: String?
    let explanation: String?
    let actSummary: String?
    let intentions: [AlignmentIntention]
    let remember: AlignmentRemember?
    let choices: [AlignmentDetailChoice]
    let readback: AlignmentReadback?
    let deadlineAtMs: Int64?
    let alsoCovers: [AlignmentCoveredRequirement]

    enum CodingKeys: String, CodingKey {
        case stage, explanation, intentions, remember, choices, readback
        case actSummary = "act_summary"
        case deadlineAtMs = "deadline_at_ms"
        case alsoCovers = "also_covers"
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        stage = try? c.decodeIfPresent(String.self, forKey: .stage)
        explanation = try? c.decodeIfPresent(String.self, forKey: .explanation)
        actSummary = try? c.decodeIfPresent(String.self, forKey: .actSummary)
        intentions = (try? c.decodeIfPresent([AlignmentIntention].self, forKey: .intentions)) ?? []
        remember = try? c.decodeIfPresent(AlignmentRemember.self, forKey: .remember)
        choices = (try? c.decodeIfPresent([AlignmentDetailChoice].self, forKey: .choices)) ?? []
        readback = try? c.decodeIfPresent(AlignmentReadback.self, forKey: .readback)
        deadlineAtMs = try? c.decodeIfPresent(Int64.self, forKey: .deadlineAtMs)
        alsoCovers = (try? c.decodeIfPresent([AlignmentCoveredRequirement].self, forKey: .alsoCovers)) ?? []
    }

    var isWordingStage: Bool { stage == "confirm_wording" }
    var actChoices: [AlignmentDetailChoice] {
        choices.filter { !$0.lasting && $0.id != "keep_as_is" }
    }
    var lastingChoices: [AlignmentDetailChoice] { choices.filter { $0.lasting } }
}

/// `GET /hitl/{id}/alignment` body.
struct AlignmentDetail: Decodable, Equatable {
    let isPrivate: Bool
    let retryReason: String?
    let alignment: AlignmentPrivateProjection

    enum CodingKeys: String, CodingKey {
        case alignment
        case isPrivate = "private"
        case retryReason = "retry_reason"
    }
}

enum AlignmentAsk {
    /// Choices that change what Magician remembers rather than decide the act.
    static let lastingChoiceIds: Set<String> = [
        "change_preference", "remember_preference", "confirm_wording",
    ]
    /// The choice a typed reply rides on in the chat card.
    static let replyChoiceId = "respond"

    /// The owner-private read for one hold, as this surface.
    static func detailURL(base: String, correlationId: String) -> URL? {
        var allowed = CharacterSet.urlPathAllowed
        allowed.remove(charactersIn: "/?#")
        guard let id = correlationId.addingPercentEncoding(withAllowedCharacters: allowed) else {
            return nil
        }
        return URL(string: "\(base)/hitl/\(id)/alignment?surface=ios")
    }

    /// An answer as the respond route's value; `nil` for an empty reply.
    static func answerValue(choiceId: String, input: String? = nil) -> [String: Any]? {
        let words = (input ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        if choiceId == replyChoiceId {
            return words.isEmpty ? nil : ["type": "text", "value": words]
        }
        var value: [String: Any] = ["type": "choice", "selected_id": choiceId]
        if !words.isEmpty { value["other_value"] = words }
        return value
    }

    /// How long the owner has to answer. `nil` when unknown.
    static func deadlineLine(deadlineAtMs: Int64?, now: Date = Date()) -> String? {
        guard let ms = deadlineAtMs else { return nil }
        let left = Double(ms) / 1000 - now.timeIntervalSince1970
        if left <= 0 { return "The answer window has closed; a late answer runs nothing." }
        let minutes = Int((left / 60).rounded())
        if minutes < 60 { return "Answer within \(max(minutes, 1)) min; after that it won't run." }
        return "Answer within \(Int((Double(minutes) / 60).rounded())) h; after that it won't run."
    }
}

/// A nudge that let an act through, with any offer to remember an inferred
/// preference — the iOS port of the web's `PreferenceNotice`
/// (`ui/unified-ui/src/lib/plane/preferenceChecks.ts`). Read with
/// `surface=ios`, an owner-private surface; a shared one gets only the count.
struct PreferenceNotice: Decodable, Equatable, Identifiable {
    struct Intention: Decodable, Equatable, Identifiable {
        let id: String
        let statement: String
    }

    struct Remember: Decodable, Equatable {
        let statement: String
        let wording: String
        /// Binds a "remember" answer to exactly this wording.
        let token: String
    }

    let id: String
    let createdAtMs: Int64
    let actSummary: String
    let explanation: String
    let intentions: [Intention]
    let remember: Remember?
    let state: String

    enum CodingKeys: String, CodingKey {
        case id, explanation, intentions, remember, state
        case createdAtMs = "created_at_ms"
        case actSummary = "act_summary"
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        createdAtMs = (try? c.decodeIfPresent(Int64.self, forKey: .createdAtMs)) ?? 0
        actSummary = (try? c.decodeIfPresent(String.self, forKey: .actSummary)) ?? ""
        explanation = (try? c.decodeIfPresent(String.self, forKey: .explanation)) ?? ""
        intentions = (try? c.decodeIfPresent([Intention].self, forKey: .intentions)) ?? []
        let remember = try? c.decodeIfPresent(Remember.self, forKey: .remember)
        self.remember = remember.flatMap { $0.token.isEmpty || $0.wording.isEmpty ? nil : $0 }
        state = (try? c.decodeIfPresent(String.self, forKey: .state)) ?? "open"
    }

    var isOpen: Bool { state == "open" }
}

enum PreferenceNotices {
    /// The list, as this surface.
    static func listURL(base: String) -> URL? {
        URL(string: "\(base)/preference-alignment/notices?surface=ios")
    }

    static func answerURL(base: String, id: String) -> URL? {
        var allowed = CharacterSet.urlPathAllowed
        allowed.remove(charactersIn: "/?#")
        guard let encoded = id.addingPercentEncoding(withAllowedCharacters: allowed) else {
            return nil
        }
        return URL(string: "\(base)/preference-alignment/notices/\(encoded)/answer")
    }

    /// The open notices in a list body; malformed rows are skipped.
    static func parse(_ data: Data) -> [PreferenceNotice] {
        struct Body: Decodable {
            let notices: [Lenient]
        }
        struct Lenient: Decodable {
            let notice: PreferenceNotice?
            init(from decoder: Decoder) throws { notice = try? PreferenceNotice(from: decoder) }
        }
        guard let body = try? JSONDecoder().decode(Body.self, from: data) else { return [] }
        return body.notices.compactMap(\.notice).filter(\.isOpen)
    }

    /// The answer body. Remembering carries the token the private read
    /// showed; without one it is not sent.
    static func answerBody(remember: Bool, token: String?) -> [String: Any]? {
        guard remember else { return ["answer": "dismiss"] }
        guard let token, !token.isEmpty else { return nil }
        return ["answer": "remember", "token": token]
    }
}

/// One of the owner's preferences exactly as preference checks see it
/// (`GET /preference-alignment/intentions?surface=ios`).
struct OwnerPreference: Decodable, Identifiable, Equatable {
    let id: String
    let revision: String
    /// `standing_instruction`, `goal` or `taste`.
    let kind: String
    /// How strong the evidence is: `direct_owner_statement`, `confirmed`,
    /// `inferred`, `inferred_untrusted` or `unknown_provenance`.
    let status: String
    let statement: String
    let conditions: String?
    let lifecycle: String
    let editable: Bool
    let lastCheckedAtMs: Double?

    private enum CodingKeys: String, CodingKey {
        case id, revision, kind, statement, conditions, lifecycle, editable
        case status = "assertion_status"
        case lastCheckedAtMs = "last_checked_at_ms"
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        revision = try c.decode(String.self, forKey: .revision)
        kind = (try? c.decode(String.self, forKey: .kind)) ?? "taste"
        status = (try? c.decode(String.self, forKey: .status)) ?? "inferred"
        statement = (try? c.decode(String.self, forKey: .statement)) ?? ""
        conditions = try? c.decodeIfPresent(String.self, forKey: .conditions)
        lifecycle = (try? c.decode(String.self, forKey: .lifecycle)) ?? "active"
        editable = (try? c.decode(Bool.self, forKey: .editable)) ?? false
        lastCheckedAtMs = try? c.decodeIfPresent(Double.self, forKey: .lastCheckedAtMs)
    }

    /// What the owner reads about where the preference's force comes from.
    var statusWords: String {
        switch status {
        case "direct_owner_statement": return "You said it"
        case "confirmed": return "You confirmed it"
        case "inferred": return "Inferred from your activity"
        case "inferred_untrusted": return "From a page or tool — only ever mentioned"
        default: return "Source unknown — only ever mentioned"
        }
    }

    /// The wording without its memory key (`key: wording`), for editing.
    var wording: String {
        guard let colon = statement.firstIndex(of: ":"),
              statement.distance(from: statement.startIndex, to: colon) <= 60 else { return statement }
        return statement[statement.index(after: colon)...].trimmingCharacters(in: .whitespaces)
    }
}

/// A stored statement shaped for reading. Memory sometimes stores a
/// preference as `key: {json}`; shown raw it is a blob. Display only:
/// editing, checks and history keep the stored wording. The same rule as the
/// web's `presentStatement` and Android's `presentStatement` (fields sorted
/// by key on every client).
struct PresentedStatement: Equatable {
    struct Field: Equatable {
        let label: String
        let value: String
    }

    let title: String
    let fields: [Field]

    private static let acronyms: Set<String> = ["ui", "api", "url", "llm", "id", "pdf", "sms", "ai"]
    private static let pattern = try? NSRegularExpression(
        pattern: #"^\s*([A-Za-z][\w.]*)\s*:\s*(\{.*\})\s*$"#,
        options: [.dotMatchesLineSeparators]
    )

    static func humanize(_ key: String) -> String {
        let words = key
            .replacingOccurrences(of: #"[._]+"#, with: " ", options: .regularExpression)
            .split(separator: " ")
            .map { acronyms.contains($0.lowercased()) ? $0.uppercased() : String($0) }
            .joined(separator: " ")
        return words.prefix(1).uppercased() + words.dropFirst()
    }

    /// `nil` when the statement is ordinary prose.
    static func parse(_ statement: String) -> PresentedStatement? {
        let range = NSRange(statement.startIndex..., in: statement)
        guard let match = pattern?.firstMatch(in: statement, range: range),
              let keyRange = Range(match.range(at: 1), in: statement),
              let jsonRange = Range(match.range(at: 2), in: statement),
              let data = String(statement[jsonRange]).data(using: .utf8),
              let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return nil }
        let fields = object.keys.sorted().map { key -> Field in
            let value = object[key]
            let text: String
            if let string = value as? String {
                text = string
            } else if let value, JSONSerialization.isValidJSONObject([value]),
                      let encoded = try? JSONSerialization.data(withJSONObject: [value], options: [.withoutEscapingSlashes]),
                      let array = String(data: encoded, encoding: .utf8) {
                text = String(array.dropFirst().dropLast())
            } else {
                text = "\(value ?? "")"
            }
            return Field(label: humanize(key), value: text)
        }
        return PresentedStatement(title: humanize(String(statement[keyRange])), fields: fields)
    }
}

extension OwnerPreference {
    var presented: PresentedStatement? { PresentedStatement.parse(statement) }
}

/// One change in a preference's history. A forgotten preference keeps its
/// dated entries with the wording erased.
struct PreferenceHistoryEntry: Decodable, Identifiable, Equatable {
    let atMs: Double
    let op: String
    let via: String
    let surface: String?
    let wordingBefore: String?
    let wordingAfter: String?
    let wordingErased: Bool
    var id: String { "\(atMs)-\(op)" }

    private enum CodingKeys: String, CodingKey {
        case op, actor
        case atMs = "at_ms"
        case wordingBefore = "wording_before"
        case wordingAfter = "wording_after"
        case wordingErased = "wording_erased"
    }
    private struct Actor: Decodable {
        let via: String?
        let surface: String?
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        atMs = (try? c.decode(Double.self, forKey: .atMs)) ?? 0
        op = (try? c.decode(String.self, forKey: .op)) ?? "changed"
        let actor = try? c.decodeIfPresent(Actor.self, forKey: .actor)
        via = actor?.via ?? "unknown"
        surface = actor?.surface
        wordingBefore = try? c.decodeIfPresent(String.self, forKey: .wordingBefore)
        wordingAfter = try? c.decodeIfPresent(String.self, forKey: .wordingAfter)
        wordingErased = (try? c.decodeIfPresent(Bool.self, forKey: .wordingErased)) ?? false
    }
}

/// Requests for the owner's preferences: the view, edits that name the
/// revision on screen, and history. Only the owner's web and iOS apps may
/// call them; the server refuses any other surface.
enum OwnerPreferences {
    enum Outcome: Equatable {
        case applied
        /// The preference changed since it was shown.
        case stale
        case failed
    }

    private static func encoded(_ id: String) -> String? {
        var allowed = CharacterSet.urlPathAllowed
        allowed.remove(charactersIn: "/?#")
        return id.addingPercentEncoding(withAllowedCharacters: allowed)
    }

    static func listURL(base: String) -> URL? {
        URL(string: "\(base)/preference-alignment/intentions?surface=ios")
    }

    /// One preference; `expectedRevision` is added for a forget.
    static func itemURL(base: String, id: String, expectedRevision: String? = nil) -> URL? {
        guard let id = encoded(id) else { return nil }
        var url = "\(base)/preference-alignment/intentions/\(id)?surface=ios"
        if let expectedRevision {
            guard let revision = expectedRevision.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed) else {
                return nil
            }
            url += "&expected_revision=\(revision)"
        }
        return URL(string: url)
    }

    static func historyURL(base: String, id: String) -> URL? {
        guard let id = encoded(id) else { return nil }
        return URL(string: "\(base)/preference-alignment/intentions/\(id)/history?surface=ios")
    }

    /// The preferences in a view body; malformed rows are skipped.
    static func parse(_ data: Data) -> [OwnerPreference] {
        struct Body: Decodable { let intentions: [Lenient] }
        struct Lenient: Decodable {
            let preference: OwnerPreference?
            init(from decoder: Decoder) throws { preference = try? OwnerPreference(from: decoder) }
        }
        guard let body = try? JSONDecoder().decode(Body.self, from: data) else { return [] }
        return body.intentions.compactMap(\.preference)
    }

    static func parseHistory(_ data: Data) -> [PreferenceHistoryEntry] {
        struct Body: Decodable { let entries: [PreferenceHistoryEntry] }
        return (try? JSONDecoder().decode(Body.self, from: data))?.entries ?? []
    }

    /// An edit, confirm or retire body against the revision on screen. An
    /// edit without wording is not sent.
    static func patchBody(action: String, expectedRevision: String, wording: String? = nil) -> [String: Any]? {
        var body: [String: Any] = ["action": action, "expected_revision": expectedRevision]
        if action == "edit" {
            guard let wording = wording?.trimmingCharacters(in: .whitespacesAndNewlines), !wording.isEmpty else {
                return nil
            }
            body["wording"] = wording
        }
        return body
    }

    static func addBody(kind: String, wording: String, label: String?, conditions: String?) -> [String: Any]? {
        let wording = wording.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !wording.isEmpty else { return nil }
        var body: [String: Any] = ["kind": kind, "wording": wording]
        if let label = label?.trimmingCharacters(in: .whitespaces), !label.isEmpty { body["label"] = label }
        if let conditions = conditions?.trimmingCharacters(in: .whitespaces), !conditions.isEmpty {
            body["conditions"] = conditions
        }
        return body
    }

    static func outcome(status: Int, data: Data?) -> Outcome {
        if (200..<300).contains(status) { return .applied }
        let error = data
            .flatMap { try? JSONSerialization.jsonObject(with: $0) as? [String: Any] }?["error"] as? String
        return status == 409 && error == "stale_preference" ? .stale : .failed
    }
}
