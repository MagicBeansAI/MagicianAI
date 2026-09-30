import CoreGraphics
import Foundation

// Morning Edition — the newspaper-style Today page (web parity with
// `ui/unified-ui/src/lib/today/MorningEdition.svelte`). This file holds the
// page's value types and every PURE helper it renders through, so the
// masthead numbering, the wire's count chip and event titles, the deck's
// interleave and release thresholds, and the ledger's spend/pie maths are
// unit-testable without a view (`TodayMorningEditionTests`).

/// One line on the Realtime Wire: a feed item, an agent update, or a live
/// websocket event. Never seeded — the wire shows only real transmissions.
struct TodayWireItem: Identifiable, Equatable {
    enum Kind: String, CaseIterable {
        case event, insight, activity

        var tag: String { rawValue.uppercased() }
    }

    enum Severity: String {
        case info, warn, error, success
    }

    let id: String
    let kind: Kind
    let title: String
    let summary: String
    /// Epoch milliseconds.
    let timestamp: Int64
    let badge: String
    let severity: Severity
    let taskID: String?
    let threadID: String?
}

/// The wire drawer's filter chips.
enum TodayWireFilter: String, CaseIterable, Identifiable {
    case all, event, insight, activity

    var id: String { rawValue }

    var title: String {
        switch self {
        case .all: return "All"
        case .event: return "Events"
        case .insight: return "Insights"
        case .activity: return "Activity"
        }
    }

    func matches(_ item: TodayWireItem) -> Bool {
        switch self {
        case .all: return true
        case .event: return item.kind == .event
        case .insight: return item.kind == .insight
        case .activity: return item.kind == .activity
        }
    }
}

/// `GET /api/magician/v2/agents/updates` → `{events:[…]}`. Decoded tolerantly:
/// the envelope carries variant-specific fields, and only the ones the wire
/// renders are read.
struct TodayAgentUpdate: Decodable, Equatable {
    let id: String
    let agentID: String?
    let kind: String
    let ts: Int64?
    let threadID: String?
    let error: String?
    let focusArea: String?
    let reason: String?
    let outcome: String?

    enum CodingKeys: String, CodingKey {
        case id, kind, ts, error, reason, outcome
        case agentID = "agent_id"
        case threadID = "thread_id"
        case focusArea = "focus_area"
    }

    init(id: String, agentID: String?, kind: String, ts: Int64?, threadID: String? = nil,
         error: String? = nil, focusArea: String? = nil, reason: String? = nil, outcome: String? = nil) {
        self.id = id; self.agentID = agentID; self.kind = kind; self.ts = ts; self.threadID = threadID
        self.error = error; self.focusArea = focusArea; self.reason = reason; self.outcome = outcome
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        if let text = try? c.decode(String.self, forKey: .id) { id = text }
        else if let number = try? c.decode(Int64.self, forKey: .id) { id = String(number) }
        else { id = UUID().uuidString }
        agentID = try? c.decodeIfPresent(String.self, forKey: .agentID)
        kind = (try? c.decode(String.self, forKey: .kind)) ?? "update"
        if let number = try? c.decode(Int64.self, forKey: .ts) { ts = number }
        else if let double = try? c.decode(Double.self, forKey: .ts) { ts = Int64(double) }
        else if let text = try? c.decode(String.self, forKey: .ts) { ts = TodayMorningEdition.epochMilliseconds(fromISO: text) }
        else { ts = nil }
        threadID = try? c.decodeIfPresent(String.self, forKey: .threadID)
        error = (try? c.decodeIfPresent(String.self, forKey: .error)) ?? nil
        focusArea = (try? c.decodeIfPresent(String.self, forKey: .focusArea)) ?? nil
        reason = (try? c.decodeIfPresent(String.self, forKey: .reason)) ?? nil
        outcome = (try? c.decodeIfPresent(String.self, forKey: .outcome)) ?? nil
    }
}

struct TodayAgentUpdatesResponse: Decodable {
    let events: [TodayAgentUpdate]

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        events = (try? c.decode([TodayAgentUpdate].self, forKey: .events)) ?? []
    }

    enum CodingKeys: String, CodingKey { case events }
}

/// One row of `GET /api/magician/v2/agents` — only the fields the fleet
/// panel counts.
struct TodayAgentSummary: Decodable, Equatable {
    let status: String
    let disabled: Bool
    /// `definition.agent_id` (or a top-level `agent_id` / `id`).
    let agentID: String
    /// `definition.name`, falling back to the agent id.
    let name: String

    init(status: String, disabled: Bool = false, agentID: String = "", name: String? = nil) {
        self.status = status
        self.disabled = disabled
        self.agentID = agentID
        self.name = name ?? agentID
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let definition = try? c.nestedContainer(keyedBy: CodingKeys.self, forKey: .definition)
        status = (try? c.decode(String.self, forKey: .status)) ?? "unknown"
        disabled = ((try? c.decode(Bool.self, forKey: .disabled)) ?? false)
            || ((try? definition?.decode(Bool.self, forKey: .disabled)) ?? false)
        let id = (try? definition?.decode(String.self, forKey: .agentID))
            ?? (try? c.decode(String.self, forKey: .agentID))
            ?? (try? c.decode(String.self, forKey: .id)) ?? ""
        agentID = id
        name = TodayMorningEdition.firstNonEmpty((try? definition?.decode(String.self, forKey: .name)) ?? nil,
                                                 (try? c.decode(String.self, forKey: .name)) ?? nil) ?? id
    }

    enum CodingKeys: String, CodingKey { case status, disabled, definition, name, id; case agentID = "agent_id" }
}

/// One `llm_calls` aggregate row of the crew SQL (last 24 h, per agent).
struct TodayCrewLLMRow: Equatable {
    let agentID: String
    let calls: Int
    let costUSD: Double
    let okCalls: Int
}

/// One `/v3/tasks` row as the crew slide counts it.
struct TodayCrewTaskRow: Equatable {
    let agentID: String
    let status: String
    /// Epoch milliseconds (0 when unknown).
    let updatedAt: Int64
}

/// One agent line on the State of the Crew slide.
struct TodayCrewMember: Identifiable, Equatable {
    let id: String
    let name: String
    let active: Bool
    let disabled: Bool
    let costUSD: Double
    let calls: Int
    let okCalls: Int
    let tasksDone: Int
    let tasksFailed: Int

    /// done / (done + failed) in whole percent; nil ("—") when both are 0.
    var successPercent: Int? {
        tasksDone + tasksFailed == 0 ? nil : Int((Double(tasksDone) / Double(tasksDone + tasksFailed) * 100).rounded())
    }

    /// ok_calls / calls in whole percent; nil ("—") without calls.
    var reliabilityPercent: Int? {
        calls == 0 ? nil : Int((Double(okCalls) / Double(calls) * 100).rounded())
    }
}

/// State of the Crew: listed members plus crew totals.
struct TodayCrewSummary: Equatable {
    let members: [TodayCrewMember]
    let activeAgents: Int
    let totalAgents: Int
    let costUSD: Double
    let tasksDone: Int
    /// Σok / Σcalls in whole percent; nil without calls.
    let reliabilityPercent: Int?
}

/// `{agents:[…], system_agents:[…]}` — the fleet panel counts `agents` only.
/// A bare array is accepted too (the tasks list's agent picker tolerates it).
struct TodayAgentsResponse: Decodable {
    let agents: [TodayAgentSummary]

    init(from decoder: Decoder) throws {
        if let keyed = try? decoder.container(keyedBy: CodingKeys.self),
           let agents = try? keyed.decode([TodayAgentSummary].self, forKey: .agents) {
            self.agents = agents
        } else {
            self.agents = (try? [TodayAgentSummary](from: decoder)) ?? []
        }
    }

    enum CodingKeys: String, CodingKey { case agents }
}

/// Enabled / Active / Total, as the fleet panel prints them.
struct TodayAgentCounts: Equatable {
    var total = 0
    var enabled = 0
    var active = 0
}

/// One row of the State of Operations task list.
struct TodayRecentTask: Identifiable, Equatable {
    let id: String
    let title: String
    let status: String
    /// Epoch milliseconds (0 when unknown).
    let updatedAt: Int64
}

/// Auto-advance rule for the Operations carousel: every 8 s, wrapping; never
/// under Reduce Motion; paused for 15 s after a manual swipe / dot tap.
enum TodayCarouselAutoAdvance {
    static let interval: TimeInterval = 8
    static let manualPause: TimeInterval = 15

    static func nextIndex(after current: Int, count: Int) -> Int {
        count <= 0 ? 0 : (current + 1) % count
    }

    static func shouldAdvance(now: Date, lastAdvance: Date, pausedUntil: Date?,
                              reduceMotion: Bool, slideCount: Int) -> Bool {
        guard !reduceMotion, slideCount > 1 else { return false }
        if let pausedUntil, now < pausedUntil { return false }
        return now.timeIntervalSince(lastAdvance) >= interval
    }

    static func pauseUntil(afterInteractionAt now: Date) -> Date {
        now.addingTimeInterval(manualPause)
    }

    /// `Slide 2 of 2, State of Operations`.
    static func dotLabel(index: Int, count: Int, title: String) -> String {
        "Slide \(index + 1) of \(count), \(title)"
    }
}

/// Solid-pie slice shares, in whole percent (they always sum to 100 when any
/// task was attempted).
struct TodayTaskShares: Equatable {
    let succeeded: Int
    let failed: Int
    let inFlight: Int
}

/// Which Reading Room view is showing. Persisted per device.
enum TodayReadingMode: String, CaseIterable, Identifiable {
    case deck, broadsheet
    var id: String { rawValue }
}

/// The Broadsheet's own tabs (persisted as `todayBroadsheetTab`).
enum TodayBroadsheetTab: String, CaseIterable, Identifiable {
    case forYou = "for_you", worth

    var id: String { rawValue }

    var title: String {
        switch self {
        case .forYou: return "For You"
        case .worth: return "Worth a Look"
        }
    }
}

/// One server page of a Broadsheet column — web `ServerPager` maths.
struct TodayPageWindow: Equatable {
    let page: Int
    let pageSize: Int
    let total: Int
    /// Rows actually on this page (after optimistic removals).
    let loaded: Int

    var pageCount: Int { max(1, Int(ceil(Double(max(0, total)) / Double(max(1, pageSize))))) }
    var startItem: Int { total <= 0 || loaded <= 0 ? 0 : (page - 1) * pageSize + 1 }
    var endItem: Int { startItem == 0 ? 0 : min(total, startItem + loaded - 1) }
    var hasPrevious: Bool { page > 1 }
    var hasNext: Bool { page < pageCount }
    /// `6–10 of 23`, or `0 of 0` when empty.
    var rangeLabel: String { startItem == 0 ? "0 of \(max(0, total))" : "\(startItem)–\(endItem) of \(total)" }
    /// `2 / 5`.
    var pageLabel: String { "\(page) / \(pageCount)" }
}

/// The Morning Brief deck's filter tabs.
enum TodayDeckTab: String, CaseIterable, Identifiable {
    case all, forYou = "for_you", worth

    var id: String { rawValue }

    var title: String {
        switch self {
        case .all: return "All"
        case .forYou: return "For You"
        case .worth: return "Worth a Look"
        }
    }
}

/// How a released deck drag resolves. Only horizontal swipes triage — Seen
/// (acknowledge) is a button, because an upward swipe on a card that fills
/// most of the viewport is indistinguishable from scrolling the page.
enum TodayDeckSwipe: String, Equatable {
    case useful, dismiss
}

/// One card in the Morning Brief deck: a channel follow-up ("For You") or a
/// resurfacing card ("Worth a Look"). The raw model rides along so a tap can
/// open the existing rich detail sheet and actions map onto the existing
/// optimistic view-model calls.
struct TodayDeckCard: Identifiable, Equatable {
    enum Source: Equatable {
        case followUp(ChannelFollowUp)
        case worth(ResurfacingCard)
    }

    let source: Source

    var id: String {
        switch source {
        case .followUp(let item): return "followup:\(item.id)"
        case .worth(let card): return "worth:\(card.id)"
        }
    }

    var isForYou: Bool {
        if case .followUp = source { return true }
        return false
    }

    var category: String {
        switch source {
        case .followUp(let item):
            let provider = item.provider.trimmingCharacters(in: .whitespacesAndNewlines)
            return "DISPATCH · \(provider.isEmpty || provider == "unknown" ? "CORRESPONDENCE" : provider.uppercased())"
        case .worth(let card):
            let kind = card.sourceKind.trimmingCharacters(in: .whitespacesAndNewlines)
            let label = kind.isEmpty || kind == "unknown" ? "NOTE" : kind.replacingOccurrences(of: "_", with: " ").uppercased()
            return "READING ROOM · \(label)"
        }
    }

    var title: String {
        switch source {
        case .followUp(let item):
            return TodayMorningEdition.firstNonEmpty(item.subject) ?? "Untitled Message"
        case .worth(let card):
            return TodayMorningEdition.firstNonEmpty(card.sourceTitle, card.line) ?? "Resurfaced Note"
        }
    }

    var summary: String {
        switch source {
        case .followUp(let item):
            return TodayMorningEdition.firstNonEmpty(item.summary, item.reason) ?? "No preview available"
        case .worth(let card):
            return TodayMorningEdition.firstNonEmpty(card.summary, card.whyNow) ?? "Resurfaced for your attention"
        }
    }

    var sender: String? {
        if case .followUp(let item) = source { return TodayMorningEdition.firstNonEmpty(item.sender) }
        return nil
    }

    /// ⚡ posts `approve` for a follow-up (start a follow-up task) and `open`
    /// feedback + source navigation for a worth card.
    var primaryLabel: String { isForYou ? "Do it" : "Open" }

    var deliveryBinding: AttentionDeliveryBinding? {
        switch source {
        case .followUp(let item): return item.deliveryBinding
        case .worth(let card): return card.deliveryBinding
        }
    }
}

enum TodayMorningEdition {
    /// Vol. counts publication years from 2023 (Vol. I).
    static let inceptionYear = 2023

    // MARK: Masthead

    /// Standard subtractive Roman numerals; anything below 1 renders as I.
    static func romanNumeral(_ value: Int) -> String {
        let lookup: [(String, Int)] = [
            ("M", 1000), ("CM", 900), ("D", 500), ("CD", 400),
            ("C", 100), ("XC", 90), ("L", 50), ("XL", 40),
            ("X", 10), ("IX", 9), ("V", 5), ("IV", 4), ("I", 1)
        ]
        var remaining = max(1, value)
        var result = ""
        for (numeral, amount) in lookup {
            while remaining >= amount {
                result += numeral
                remaining -= amount
            }
        }
        return result
    }

    /// 1-based day of the year in the reader's calendar.
    static func dayOfYear(_ date: Date, calendar: Calendar = .current) -> Int {
        calendar.ordinality(of: .day, in: .year, for: date) ?? 1
    }

    /// `VOL. IV · NO. 271`.
    static func volumeLine(for date: Date, calendar: Calendar = .current) -> String {
        let year = calendar.component(.year, from: date)
        return "VOL. \(romanNumeral(year - inceptionYear + 1)) · NO. \(dayOfYear(date, calendar: calendar))"
    }

    /// `Monday, September 28, 2026` in the reader's zone (English, like web).
    static func mastheadDate(_ date: Date, calendar: Calendar = .current) -> String {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.calendar = calendar
        formatter.timeZone = calendar.timeZone
        formatter.dateFormat = "EEEE, MMMM d, yyyy"
        return formatter.string(from: date)
    }

    static func weatherLine(urgent: Int) -> String {
        urgent <= 0 ? "WEATHER: ALL QUIET" : "WEATHER: \(urgent) URGENT"
    }

    /// Web `greeting.ts`: 05–11 morning, 12–17 afternoon, otherwise evening.
    static func greeting(for date: Date, calendar: Calendar = .current) -> String {
        switch calendar.component(.hour, from: date) {
        case 5..<12: return "Good morning"
        case 12..<18: return "Good afternoon"
        default: return "Good evening"
        }
    }

    // MARK: Realtime Wire

    /// `35086 → 35K/24H`, `1500 → 1.5K/24H`, `0 → 0/24H`.
    static func compact24hCount(_ value: Int) -> String {
        guard value > 0 else { return "0/24H" }
        func scaled(_ amount: Double) -> String {
            if amount >= 10 { return String(Int(amount.rounded())) }
            let text = String(format: "%.1f", amount)
            return text.hasSuffix(".0") ? String(text.dropLast(2)) : text
        }
        if value >= 1_000_000 { return "\(scaled(Double(value) / 1_000_000))M/24H" }
        if value >= 1_000 { return "\(scaled(Double(value) / 1_000))K/24H" }
        return "\(value)/24H"
    }

    private static let acronyms: Set<String> = ["llm", "hitl", "ui", "api", "id", "cli", "sse", "db"]

    static func titleCaseWord(_ word: String) -> String {
        guard !word.isEmpty else { return "" }
        let lower = word.lowercased()
        if acronyms.contains(lower) { return lower.uppercased() }
        return lower.prefix(1).uppercased() + lower.dropFirst()
    }

    /// Inserts word breaks into CamelCase (`MessageProcessingStarted`,
    /// `HITLRequest`) so wire-serialized enum variant names read as words.
    static func splittingCamelCase(_ value: String) -> String {
        let characters = Array(value)
        var result = ""
        for (index, character) in characters.enumerated() {
            if index > 0, character.isUppercase {
                let previous = characters[index - 1]
                let next = index + 1 < characters.count ? characters[index + 1] : nil
                if previous.isLowercase || previous.isNumber
                    || (previous.isUppercase && next?.isLowercase == true) {
                    result.append(" ")
                }
            }
            result.append(character)
        }
        return result
    }

    private static func humanWords(_ value: String) -> String {
        splittingCamelCase(value.replacingOccurrences(of: "_", with: " "))
            .split(whereSeparator: \.isWhitespace)
            .map { titleCaseWord(String($0)) }
            .joined(separator: " ")
    }

    /// `event.agent.cycle_completed → Agent: Cycle Completed`,
    /// `llm.call.failed → LLM: Call Failed`.
    static func humanizeEventType(_ raw: String) -> String {
        var cleaned = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        if cleaned.lowercased().hasPrefix("event.") { cleaned.removeFirst(6) }
        guard !cleaned.isEmpty else { return "System event" }
        let parts = cleaned.split(separator: ".", omittingEmptySubsequences: true).map(String.init)
        guard parts.count > 1 else { return humanWords(cleaned) }
        return "\(humanWords(parts[0])): \(humanWords(parts.dropFirst().joined(separator: " ")))"
    }

    private static let insightItemTypes: Set<String> = ["learning_insight", "learning_candidate", "agent_learning"]

    /// A `/api/magician/v2/feed` row as a wire line.
    static func wireItem(fromFeed item: TodayActivityItem, now: Date = Date()) -> TodayWireItem {
        let isInsight = insightItemTypes.contains(item.itemType)
        let summary = firstNonEmpty(item.summary)
            ?? item.taskID.flatMap { firstNonEmpty($0) }.map { "Task \($0)" }
            ?? "Feed insight recorded"
        let timestamp = item.updatedAt > 0 ? item.updatedAt
            : item.createdAt > 0 ? item.createdAt : Int64(now.timeIntervalSince1970 * 1_000)
        return TodayWireItem(
            id: "feed-\(item.id)",
            kind: isInsight ? .insight : .activity,
            title: firstNonEmpty(item.title) ?? (isInsight ? "Distilled Memory" : "Fleet Activity"),
            summary: summary,
            timestamp: timestamp,
            badge: item.itemType.replacingOccurrences(of: "_", with: " "),
            severity: item.status == "failed" ? .error : item.status == "done" ? .success : .info,
            taskID: firstNonEmpty(item.taskID),
            threadID: firstNonEmpty(item.threadID)
        )
    }

    /// An agent update envelope as a wire line.
    static func wireItem(fromAgentUpdate update: TodayAgentUpdate, now: Date = Date()) -> TodayWireItem {
        let agent = firstNonEmpty(update.agentID).map(titleCaseWord) ?? "Fleet Agent"
        let kind = update.kind.replacingOccurrences(of: "_", with: " ")
        return TodayWireItem(
            id: "agent-update-\(update.id)",
            kind: .activity,
            title: "\(agent): \(kind)",
            summary: firstNonEmpty(update.error, update.focusArea, update.reason) ?? "Autonomous agent cycle logged",
            timestamp: update.ts.flatMap { $0 > 0 ? $0 : nil } ?? Int64(now.timeIntervalSince1970 * 1_000),
            badge: kind,
            severity: update.outcome == "failed" ? .error : .info,
            taskID: nil,
            threadID: firstNonEmpty(update.threadID)
        )
    }

    private static let ignoredRealtimeTypes: Set<String> = [
        "ping", "pong", "heartbeat", "keepalive", "connected", "welcome", "error"
    ]

    /// A live `/api/magician/v2/realtime/ws` frame as an `EVENT` wire line, or
    /// nil for control frames / unparseable text. `sequence` keeps two events
    /// of the same type in the same millisecond distinct.
    static func wireItem(fromRealtimeText text: String, sequence: Int, now: Date = Date()) -> TodayWireItem? {
        guard let data = text.data(using: .utf8),
              let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return nil }
        let outerType = string(root["event_type"]) ?? string(root["type"]) ?? ""
        guard !outerType.isEmpty, !outerType.hasPrefix("__"),
              !ignoredRealtimeTypes.contains(outerType.lowercased()) else { return nil }
        let body = root["data"] as? [String: Any]
        let inner = body?["event"] as? [String: Any]
        let innerType = string(inner?["event_type"])
        let effectiveType = outerType == "AgentEvent" ? (innerType ?? outerType) : outerType
        // Token/UI streaming deltas arrive many per second and carry no news.
        let lowered = effectiveType.lowercased()
        guard !["delta", "chunk", "token"].contains(where: { lowered.contains($0) }) else { return nil }
        let payload = (inner?["payload"] as? [String: Any]) ?? (body?["payload"] as? [String: Any])
            ?? (root["payload"] as? [String: Any])
        func field(_ keys: String...) -> String? {
            for record in [payload, inner, body, root] {
                for key in keys {
                    if let value = string(record?[key]) { return value }
                }
            }
            return nil
        }
        let error = field("error")
        let outcome = field("outcome")?.lowercased()
        let taskID = field("task_id")
        let agentLabel = field("agent_id").map { agent in
            agent.split(whereSeparator: { $0 == "-" || $0 == "_" }).map { titleCaseWord(String($0)) }.joined(separator: " ")
        }
        // An agent-scoped type (`agent.cycle.completed`) already names its
        // subject through the agent label: `Night Owl: Cycle Completed`.
        let typeParts = effectiveType.split(separator: ".").map(String.init)
        let typeTitle = agentLabel != nil && typeParts.count > 1 && typeParts[0].lowercased() == "agent"
            ? humanWords(typeParts.dropFirst().joined(separator: " "))
            : humanizeEventType(effectiveType)
        let baseTitle = field("title") ?? typeTitle
        let title: String
        if let agentLabel, !agentLabel.isEmpty, !baseTitle.lowercased().contains(agentLabel.lowercased()) {
            title = "\(agentLabel): \(baseTitle)"
        } else {
            title = baseTitle
        }
        let summary = field("summary", "message") ?? error ?? field("description", "reason", "focus_area")
            ?? taskID.map { "Task \($0)" } ?? "System telemetry dispatch"
        let severity: TodayWireItem.Severity
        if error != nil || outcome == "failed" || outcome == "failure" { severity = .error }
        else if ["completed", "done", "success", "succeeded"].contains(outcome ?? "") { severity = .success }
        else { severity = .info }
        let timestamp = [root["timestamp_ms"], root["timestamp"], body?["timestamp_ms"], body?["timestamp"],
                         inner?["timestamp_ms"], inner?["timestamp"], payload?["timestamp_ms"], payload?["timestamp"]]
            .lazy.compactMap(epochMilliseconds(from:)).first
            ?? Int64(now.timeIntervalSince1970 * 1_000)
        let badgeSource = effectiveType.split(separator: ".").first.map(String.init) ?? effectiveType
        let badge = humanWords(badgeSource).split(separator: " ").first.map { $0.lowercased() } ?? "event"
        return TodayWireItem(
            id: "event-\(effectiveType)-\(timestamp)-\(sequence)",
            kind: .event,
            title: title,
            summary: summary,
            timestamp: timestamp,
            badge: badge,
            severity: severity,
            taskID: taskID,
            threadID: field("thread_id", "ui_thread_id")
        )
    }

    /// Newest first, de-duplicated by id (the incoming copy wins), capped at 50.
    static func mergingWireItems(_ existing: [TodayWireItem], _ incoming: [TodayWireItem],
                                 limit: Int = 50) -> [TodayWireItem] {
        var byID: [String: TodayWireItem] = [:]
        for item in existing { byID[item.id] = item }
        for item in incoming { byID[item.id] = item }
        return Array(byID.values
            .sorted { $0.timestamp == $1.timestamp ? $0.id < $1.id : $0.timestamp > $1.timestamp }
            .prefix(limit))
    }

    /// Web wire `formatTimeAgo`: under 45s reads "just now".
    static func wireTimeAgo(_ epochMilliseconds: Int64, now: Date = Date()) -> String {
        let seconds = max(0, Int(now.timeIntervalSince1970) - Int(epochMilliseconds / 1_000))
        if seconds < 45 { return "just now" }
        let minutes = seconds / 60
        if minutes < 60 { return "\(minutes)m ago" }
        let hours = minutes / 60
        if hours < 24 { return "\(hours)h ago" }
        return "\(hours / 24)d ago"
    }

    // MARK: Reading Room

    /// Repeat {2 follow-ups, then 1 worth card} until both run out.
    static func interleaveDeck(followUps: [ChannelFollowUp], worth: [ResurfacingCard]) -> [TodayDeckCard] {
        var result: [TodayDeckCard] = []
        var followIndex = 0
        var worthIndex = 0
        while followIndex < followUps.count || worthIndex < worth.count {
            for _ in 0..<2 where followIndex < followUps.count {
                result.append(TodayDeckCard(source: .followUp(followUps[followIndex])))
                followIndex += 1
            }
            if worthIndex < worth.count {
                result.append(TodayDeckCard(source: .worth(worth[worthIndex])))
                worthIndex += 1
            }
        }
        return result
    }

    static func deckCards(_ cards: [TodayDeckCard], in tab: TodayDeckTab) -> [TodayDeckCard] {
        switch tab {
        case .all: return cards
        case .forYou: return cards.filter(\.isForYou)
        case .worth: return cards.filter { !$0.isForYou }
        }
    }

    /// `🃏 Morning Brief (n)` — capped as `50+`.
    static func deckCountLabel(_ total: Int) -> String {
        total > 50 ? "50+" : "\(max(0, total))"
    }

    static let deckHorizontalThreshold: Double = 95

    /// True when a drag belongs to the card (horizontal) rather than to the
    /// page scroll (vertical). Decided once, on the drag's first movement.
    static func isHorizontalDeckDrag(_ translation: CGSize) -> Bool {
        abs(translation.width) > abs(translation.height)
    }

    /// Resolve a released drag: right past 95pt = Useful, left = Dismiss; a
    /// flick whose PREDICTED end crosses the threshold also commits. A
    /// predominantly vertical drag never triages — it is a page scroll.
    static func deckRelease(translation: CGSize, predicted: CGSize) -> TodayDeckSwipe? {
        guard isHorizontalDeckDrag(translation) else { return nil }
        let dx = Double(translation.width)
        if dx > deckHorizontalThreshold { return .useful }
        if dx < -deckHorizontalThreshold { return .dismiss }
        let px = Double(predicted.width), py = Double(predicted.height)
        guard abs(px) >= abs(py) else { return nil }
        if px > deckHorizontalThreshold { return .useful }
        if px < -deckHorizontalThreshold { return .dismiss }
        return nil
    }

    private static func clamp01(_ value: Double) -> Double { min(1, max(0, value)) }

    static func usefulStampOpacity(dx: Double) -> Double { clamp01((dx - 25) / 75) }
    static func dismissStampOpacity(dx: Double) -> Double { clamp01((-dx - 25) / 75) }

    /// Follow-ups page by opaque keyset cursor, so page N is reachable only
    /// by walking `next_cursor` forward from the nearest known page (web
    /// `ensureFollowUpCursor`). Returns the page actually reached — the last
    /// reachable one when the cursors run out — its cursor, and the updated
    /// page→cursor map (page 1's cursor is nil).
    static func walkFollowUpCursor(
        to target: Int,
        known: [Int: String?],
        fetch: (String?) async throws -> (nextCursor: String?, hasMore: Bool)
    ) async -> (page: Int, cursor: String?, known: [Int: String?]) {
        var map = known
        map[1] = .some(nil)
        guard target > 1 else { return (1, nil, map) }
        if let cursor = map[target] { return (target, cursor, map) }
        var page = map.keys.filter { $0 < target }.max() ?? 1
        var cursor: String? = map[page] ?? nil
        while page < target {
            guard let next = try? await fetch(cursor), next.hasMore, let nextCursor = next.nextCursor else { break }
            page += 1
            cursor = nextCursor
            map[page] = .some(nextCursor)
        }
        return (page, cursor, map)
    }

    // MARK: Economics of Operations

    static let currencyDeltaNoiseUSD = 0.005

    /// `$X.XX`; from $100 up the cents are noise → `$123`.
    static func formatSpend(_ value: Double) -> String {
        value >= 100 ? String(format: "$%.0f", value) : String(format: "$%.2f", value)
    }

    /// The big ledger figure split for newspaper setting: `$` + dollars + `.cents`.
    static func spendFigureParts(_ value: Double) -> (dollars: String, cents: String) {
        let text = String(formatSpend(value).dropFirst())
        let pieces = text.split(separator: ".", maxSplits: 1).map(String.init)
        return (pieces.first ?? "0", pieces.count > 1 ? ".\(pieces[1])" : "")
    }

    /// Web `formatDelta(…, 'currency')` — U+2212 minus.
    static func formatSpendDelta(today: Double, yesterday: Double) -> String {
        if today == 0 && yesterday == 0 { return "" }
        if yesterday == 0 && today > 0 { return "new today" }
        let diff = today - yesterday
        if abs(diff) < currencyDeltaNoiseUSD { return "" }
        return "\(diff > 0 ? "+" : "\u{2212}")$\(String(format: "%.2f", abs(diff)))"
    }

    enum Tone: Equatable { case good, bad, neutral }

    /// INVERTED polarity: spending more than yesterday is the watch-out side.
    static func spendTone(today: Double, yesterday: Double) -> Tone {
        if today == 0 && yesterday == 0 { return .neutral }
        if yesterday == 0 && today > 0 { return .bad }
        let diff = today - yesterday
        if abs(diff) < currencyDeltaNoiseUSD { return .neutral }
        return diff > 0 ? .bad : .good
    }

    /// `+$0.51 vs $0.91 yday` or `steady vs $0.91 yday`.
    static func spendDeltaLine(today: Double, yesterday: Double) -> String {
        let delta = formatSpendDelta(today: today, yesterday: yesterday)
        return "\(delta.isEmpty ? "steady" : delta) vs \(formatSpend(yesterday)) yday"
    }

    /// `72` for a whole share, else two decimals (`33.33`).
    static func providerSharePercent(_ share: Double) -> String {
        let percent = share * 100
        let rounded = (percent * 100).rounded() / 100
        return rounded.truncatingRemainder(dividingBy: 1) == 0 ? String(Int(rounded)) : String(format: "%.2f", rounded)
    }

    /// `0 → 12a`, `12 → 12p`, `15 → 3p`.
    static func hourLabel(_ hour: Int) -> String {
        if hour == 0 { return "12a" }
        if hour == 12 { return "12p" }
        return hour > 12 ? "\(hour - 12)p" : "\(hour)a"
    }

    /// `3p: $0.42 (12 calls)`.
    static func hourDetail(hour: Int, spend: Double, calls: Int) -> String {
        "\(hourLabel(hour)): $\(String(format: "%.2f", spend)) (\(calls) \(calls == 1 ? "call" : "calls"))"
    }

    /// Round succeeded and failed; in-flight takes the remainder (never < 0).
    static func pieShares(succeeded: Int, failed: Int, inFlight: Int) -> TodayTaskShares {
        let total = succeeded + failed + inFlight
        guard total > 0 else { return TodayTaskShares(succeeded: 0, failed: 0, inFlight: 0) }
        let succeededPct = Int((Double(succeeded) / Double(total) * 100).rounded())
        let failedPct = Int((Double(failed) / Double(total) * 100).rounded())
        return TodayTaskShares(succeeded: succeededPct, failed: failedPct,
                               inFlight: max(0, 100 - succeededPct - failedPct))
    }

    /// Newest first, at most `limit` rows; unknown times sort last.
    static func recentTasks(_ tasks: [TodayRecentTask], limit: Int = 20) -> [TodayRecentTask] {
        Array(tasks.sorted { $0.updatedAt == $1.updatedAt ? $0.id < $1.id : $0.updatedAt > $1.updatedAt }.prefix(limit))
    }

    /// Short relative time for one-line rows: `now`, `4m`, `2h`, `1d`.
    static func shortRelativeTime(_ epochMilliseconds: Int64, now: Date = Date()) -> String {
        guard epochMilliseconds > 0 else { return "" }
        let seconds = max(0, Int(now.timeIntervalSince1970) - Int(epochMilliseconds / 1_000))
        if seconds < 60 { return "now" }
        if seconds < 3_600 { return "\(seconds / 60)m" }
        if seconds < 86_400 { return "\(seconds / 3_600)h" }
        return "\(seconds / 86_400)d"
    }

    // MARK: State of the Crew

    static let crewWindowMS: Int64 = 86_400_000
    static let crewTaskPageLimit = 100
    static let crewTaskMaxPages = 5

    /// Per-agent model use over the last 24 h (not the calendar day).
    static func crewSQL(since: Int64) -> String {
        "SELECT agent_id, COUNT(*) AS calls, COALESCE(SUM(cost_usd), 0) AS cost_usd, "
            + "SUM(CASE WHEN success THEN 1 ELSE 0 END) AS ok_calls, AVG(latency_ms) AS avg_latency_ms "
            + "FROM llm_calls WHERE timestamp_ms >= \(since) AND NULLIF(TRIM(agent_id), '') IS NOT NULL "
            + "AND (COALESCE(provider_attempt_count, 1) <> 0 OR response_kind = 'harness_aggregate') "
            + "GROUP BY agent_id ORDER BY cost_usd DESC"
    }

    /// Keep paging `/v3/tasks` (newest first) only while the page's last row
    /// is still inside the window, a cursor exists, and under the page cap.
    static func shouldFetchNextCrewTaskPage(lastRowUpdatedAt: Int64?, since: Int64,
                                            nextCursor: String?, pagesFetched: Int) -> Bool {
        guard let lastRowUpdatedAt, lastRowUpdatedAt >= since,
              let nextCursor, !nextCursor.isEmpty else { return false }
        return pagesFetched < crewTaskMaxPages
    }

    private static let activeTaskStatuses: Set<String> = ["running", "paused", "planning"]

    /// Join agents, per-agent llm aggregates and task rows (window-filtered)
    /// into the crew list: agents with any calls or tasks, plus any active
    /// agent; active first, then cost, then tasks.
    static func crewSummary(agents: [TodayAgentSummary], llm: [TodayCrewLLMRow],
                            tasks: [TodayCrewTaskRow], since: Int64) -> TodayCrewSummary {
        let windowTasks = tasks.filter { $0.updatedAt >= since && !$0.agentID.isEmpty }
        let tasksByAgent = Dictionary(grouping: windowTasks, by: \.agentID)
        let llmByAgent = Dictionary(llm.map { ($0.agentID, $0) }, uniquingKeysWith: { first, _ in first })
        let knownIDs = Set(agents.map(\.agentID))
        var ids = agents.map(\.agentID).filter { !$0.isEmpty }
        for id in llmByAgent.keys.sorted() + tasksByAgent.keys.sorted() where !knownIDs.contains(id) && !ids.contains(id) {
            ids.append(id)
        }
        let byID = Dictionary(agents.map { ($0.agentID, $0) }, uniquingKeysWith: { first, _ in first })
        let members: [TodayCrewMember] = ids.compactMap { id in
            let agent = byID[id]
            let rows = tasksByAgent[id] ?? []
            let usage = llmByAgent[id]
            let hasActiveTask = rows.contains { activeTaskStatuses.contains($0.status) }
            let statusActive = agent.map { $0.status == "running" || $0.status == "triggered" } ?? false
            let member = TodayCrewMember(
                id: id,
                name: agent?.name ?? id,
                active: statusActive || hasActiveTask,
                disabled: agent.map { $0.disabled || $0.status == "disabled" } ?? false,
                costUSD: usage?.costUSD ?? 0,
                calls: usage?.calls ?? 0,
                okCalls: usage?.okCalls ?? 0,
                tasksDone: rows.filter { $0.status == "completed" || $0.status == "done" }.count,
                tasksFailed: rows.filter { $0.status == "failed" }.count
            )
            return member.active || member.calls > 0 || !rows.isEmpty ? member : nil
        }
        .sorted { left, right in
            if left.active != right.active { return left.active }
            if left.costUSD != right.costUSD { return left.costUSD > right.costUSD }
            let leftTasks = left.tasksDone + left.tasksFailed, rightTasks = right.tasksDone + right.tasksFailed
            if leftTasks != rightTasks { return leftTasks > rightTasks }
            return left.name.localizedCaseInsensitiveCompare(right.name) == .orderedAscending
        }
        let calls = llm.reduce(0) { $0 + $1.calls }
        let ok = llm.reduce(0) { $0 + $1.okCalls }
        return TodayCrewSummary(
            members: members,
            activeAgents: agents.filter { $0.status == "running" || $0.status == "triggered" }.count,
            totalAgents: agents.count,
            costUSD: llm.reduce(0) { $0 + $1.costUSD },
            tasksDone: windowTasks.filter { $0.status == "completed" || $0.status == "done" }.count,
            reliabilityPercent: calls == 0 ? nil : Int((Double(ok) / Double(calls) * 100).rounded())
        )
    }

    /// `$0.0016` under a cent, `$0.06` otherwise.
    static func formatPerCall(_ value: Double) -> String {
        value > 0 && value < 0.01 ? String(format: "$%.4f", value) : String(format: "$%.2f", value)
    }

    /// Spend per call, or "—" without calls.
    static func averagePerCall(spend: Double, calls: Int) -> String {
        calls > 0 ? formatPerCall(spend / Double(calls)) : "\u{2014}"
    }

    /// The hour with the most spend (`3p`), or "—" when nothing was spent.
    static func peakHour(_ hourlySpend: [Double]) -> String {
        guard let peak = hourlySpend.enumerated().max(by: { $0.element < $1.element }), peak.element > 0 else {
            return "\u{2014}"
        }
        return hourLabel(peak.offset)
    }

    /// Percent with its tone: ≥ 95 good, 80–95 warning (neutral), < 80 bad.
    static func percentTone(_ percent: Int) -> Tone {
        percent >= 95 ? .good : percent >= 80 ? .neutral : .bad
    }

    static func agentCounts(_ agents: [TodayAgentSummary]) -> TodayAgentCounts {
        TodayAgentCounts(
            total: agents.count,
            enabled: agents.filter { !$0.disabled && $0.status != "disabled" }.count,
            active: agents.filter { $0.status == "running" || $0.status == "triggered" }.count
        )
    }

    // MARK: Shared

    static func firstNonEmpty(_ values: String?...) -> String? {
        for value in values {
            if let trimmed = value?.trimmingCharacters(in: .whitespacesAndNewlines), !trimmed.isEmpty { return trimmed }
        }
        return nil
    }

    static func epochMilliseconds(fromISO value: String) -> Int64? {
        let fractional = ISO8601DateFormatter()
        fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        guard let date = fractional.date(from: value) ?? ISO8601DateFormatter().date(from: value) else { return nil }
        return Int64(date.timeIntervalSince1970 * 1_000)
    }

    private static func string(_ value: Any?) -> String? {
        guard let text = value as? String else { return nil }
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? nil : trimmed
    }

    private static func epochMilliseconds(from value: Any?) -> Int64? {
        if let number = value as? NSNumber, !(value is Bool) {
            let raw = number.doubleValue
            guard raw.isFinite, raw > 0 else { return nil }
            // Second-resolution timestamps are promoted to milliseconds.
            return raw < 100_000_000_000 ? Int64(raw * 1_000) : Int64(raw)
        }
        if let text = value as? String { return epochMilliseconds(fromISO: text) }
        return nil
    }
}
