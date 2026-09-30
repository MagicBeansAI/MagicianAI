import Foundation

// Pure, unit-tested pieces of the Observe "Command Deck" (the iOS port of the
// web /observe console): which view is selected, what the header status line and
// the four KPI cards say, and how the Sources / Recent / Audio payloads decode.
// No SwiftUI and no networking here — the views and view models call into these.

// MARK: - Views (panes)

/// The four deck views. The KPI cards are the only switchers; the selection is
/// remembered per device and can be set by `magican://observe?pane=…`.
enum ObservePane: String, CaseIterable, Identifiable {
    case now
    case sources
    case audio
    case notes

    var id: String { rawValue }

    /// `@AppStorage` key for the remembered selection.
    static let storageKey = "observe.selectedPane"

    /// Lenient query-value parse (`now|sources|audio|notes`, case-insensitive).
    init?(queryValue: String?) {
        guard let raw = queryValue?.trimmingCharacters(in: .whitespacesAndNewlines).lowercased(),
              let pane = ObservePane(rawValue: raw) else { return nil }
        self = pane
    }

    /// `magican://observe[?pane=…]` → `.some(pane or nil)`; any other URL → `nil`.
    /// The outer optional says "this is an Observe link"; the inner one carries
    /// the requested view (absent/unknown → keep the remembered one).
    static func deepLink(_ url: URL) -> ObservePane?? {
        guard MagicanAppURL.isScheme(url.scheme), url.host == "observe" else { return nil }
        let components = URLComponents(url: url, resolvingAgainstBaseURL: false)
        let raw = components?.queryItems?.first(where: { $0.name == "pane" })?.value
            ?? url.pathComponents.dropFirst().first
        return .some(ObservePane(queryValue: raw))
    }
}

// MARK: - Header + KPI metrics

/// Everything the header status line and the KPI grid display, derived from
/// the live inputs. Pure so the text and the counting rules are testable.
struct ObserveDeckMetrics: Equatable {
    /// Live captures: server active sessions + the in-app capture when it is not
    /// already in that list.
    let activeCaptureCount: Int
    /// Calendar events flagged `live_now` (a meeting is on, nothing is capturing).
    let liveMeetingCount: Int
    /// Enabled observation sources, or nil while nothing has loaded.
    let sourcesOn: Int?
    /// Transcription surfaces configured (meeting + listening), nil if unavailable.
    let audioSurfaces: Int?
    /// Recent captures when loaded, else the published-notes total, else nil.
    let notesCount: Int?

    init(
        activeSessionIds: [String],
        inAppSessionId: String?,
        inAppActive: Bool,
        liveMeetingCount: Int,
        sourcesOn: Int?,
        audioSurfaces: Int?,
        recentCount: Int?,
        publishedNotesTotal: Int?
    ) {
        var count = activeSessionIds.count
        if inAppActive {
            // The in-app capture counts once: either it is already in the server
            // list (same session id) or it is still starting / not yet listed.
            let listed = inAppSessionId.map { activeSessionIds.contains($0) } ?? false
            if !listed { count += 1 }
        }
        activeCaptureCount = count
        self.liveMeetingCount = liveMeetingCount
        self.sourcesOn = sourcesOn
        self.audioSurfaces = audioSurfaces
        notesCount = recentCount ?? publishedNotesTotal
    }

    var anyActive: Bool { activeCaptureCount > 0 }

    /// "N capture(s) live" | "N meeting(s) live — nothing capturing" | "Quiet — nothing capturing".
    var statusLine: String {
        if anyActive { return "\(activeCaptureCount) \(Self.plural(activeCaptureCount, "capture")) live" }
        if liveMeetingCount > 0 {
            return "\(liveMeetingCount) \(Self.plural(liveMeetingCount, "meeting")) live — nothing capturing"
        }
        return "Quiet — nothing capturing"
    }

    /// Sub text of the "Now & Live" card.
    var nowSub: String {
        if anyActive { return "\(activeCaptureCount) live \(Self.plural(activeCaptureCount, "capture"))" }
        if liveMeetingCount > 0 { return "\(liveMeetingCount) live \(Self.plural(liveMeetingCount, "meeting"))" }
        return "Live captures & meetings"
    }

    func metricText(_ pane: ObservePane) -> String {
        switch pane {
        case .now: return "\(activeCaptureCount)"
        case .sources: return sourcesOn.map(String.init) ?? "—"
        case .audio: return audioSurfaces.map(String.init) ?? "—"
        case .notes: return notesCount.map(String.init) ?? "—"
        }
    }

    func subText(_ pane: ObservePane) -> String {
        switch pane {
        case .now: return nowSub
        case .sources: return "Channels, tabs & feeds"
        case .audio: return "Meeting & listening STT"
        case .notes: return "Observations & journals"
        }
    }

    /// VoiceOver label for a KPI card, e.g. "Now and live, 1 live capture, selected".
    func accessibilityLabel(_ pane: ObservePane, selected: Bool) -> String {
        let value: String
        switch pane {
        case .now: value = nowSub
        case .sources: value = "\(metricText(.sources)) sources on"
        case .audio: value = "\(metricText(.audio)) transcription surfaces"
        case .notes: value = "\(metricText(.notes)) recent captures and notes"
        }
        return "\(pane.kpiSpokenTitle), \(value)\(selected ? ", selected" : "")"
    }

    static func plural(_ n: Int, _ noun: String) -> String { n == 1 ? noun : noun + "s" }
}

extension ObservePane {
    var kpiTitle: String {
        switch self {
        case .now: return "Now & Live"
        case .sources: return "Sources on"
        case .audio: return "Audio Profiles"
        case .notes: return "Notes & Recents"
        }
    }

    var kpiSpokenTitle: String {
        switch self {
        case .now: return "Now and live"
        case .sources: return "Sources on"
        case .audio: return "Audio profiles"
        case .notes: return "Notes and recents"
        }
    }

    var kpiFooter: String {
        switch self {
        case .now: return "Open live deck →"
        case .sources: return "View sources →"
        case .audio: return "Configure audio →"
        case .notes: return "Browse notes →"
        }
    }

    var kpiIcon: String {
        switch self {
        case .now: return "bolt.fill"
        case .sources: return "slider.horizontal.3"
        case .audio: return "mic.fill"
        case .notes: return "doc.text.fill"
        }
    }
}

// MARK: - Lenient JSON helpers

enum ObserveJSON {
    /// A timestamp that may arrive as epoch ms, epoch seconds, or RFC3339.
    static func date(_ value: Any?) -> Date? {
        switch value {
        case let n as NSNumber:
            let v = n.doubleValue
            guard v > 0 else { return nil }
            return Date(timeIntervalSince1970: v > 100_000_000_000 ? v / 1000 : v)
        case let s as String:
            if let d = MeetingsClient.parseDate(s) { return d }
            if let v = Double(s), v > 0 { return date(NSNumber(value: v)) }
            return nil
        default:
            return nil
        }
    }

    static func int(_ value: Any?) -> Int? {
        if let n = value as? NSNumber { return n.intValue }
        if let s = value as? String { return Int(s) }
        return nil
    }

    static func string(_ value: Any?) -> String? {
        guard let s = value as? String else { return nil }
        let t = s.trimmingCharacters(in: .whitespacesAndNewlines)
        return t.isEmpty ? nil : t
    }
}

// MARK: - Recent captures

/// One row of the Now view's "Recent" list: a meeting thread from
/// `GET /meetings` → `recent`, or a screen-watch session from
/// `GET /chat/sessions?ui_thread_id=screen-watch`.
struct RecentCapture: Identifiable, Equatable {
    enum Kind: String { case meeting, observation }

    let id: String
    let kind: Kind
    let title: String
    /// The thread the row opens (`magican://thread/{threadId}`).
    let threadId: String
    let updatedAt: Date?
    let mode: String?

    static let maxRows = 30

    /// `GET /meetings` body → meeting rows (lenient; rows without a thread drop).
    static func meetings(from object: [String: Any]) -> [RecentCapture] {
        let rows = object["recent"] as? [[String: Any]] ?? []
        return rows.compactMap { row in
            guard let thread = ObserveJSON.string(row["thread_id"]) else { return nil }
            return RecentCapture(
                id: "m:\(thread)",
                kind: .meeting,
                title: ObserveJSON.string(row["title"]) ?? thread,
                threadId: thread,
                updatedAt: ObserveJSON.date(row["updated_at"] ?? row["started_at"]),
                mode: ObserveJSON.string(row["mode"])
            )
        }
    }

    /// `GET /chat/sessions?ui_thread_id=screen-watch` body → observation rows.
    static func watchSessions(from object: [String: Any]) -> [RecentCapture] {
        let rows = object["sessions"] as? [[String: Any]] ?? []
        return rows.prefix(20).compactMap { row in
            guard let id = ObserveJSON.string(row["id"]) else { return nil }
            return RecentCapture(
                id: "o:\(id)",
                kind: .observation,
                title: ObserveJSON.string(row["title"]) ?? "Screen observation",
                threadId: ObserveJSON.string(row["ui_thread_id"]) ?? "screen-watch",
                updatedAt: ObserveJSON.date(row["updated_at"]),
                mode: "screen"
            )
        }
    }

    /// Newest first, capped at `maxRows`.
    static func merged(_ lists: [RecentCapture]...) -> [RecentCapture] {
        Array(
            lists.flatMap { $0 }
                .sorted { ($0.updatedAt ?? .distantPast) > ($1.updatedAt ?? .distantPast) }
                .prefix(maxRows)
        )
    }
}

// MARK: - Sources (view-only web & account blocks)

struct ObserveChannel: Identifiable, Equatable {
    let provider: String
    let providerLabel: String
    let account: String
    let enabled: Bool
    let connected: Bool
    let messageCount: Int
    let threadCount: Int
    let lane: String?
    let verificationCodes: Bool

    var id: String { "\(provider):\(account)" }

    static func list(from object: [String: Any]) -> [ObserveChannel] {
        let rows = object["channels"] as? [[String: Any]] ?? []
        return rows.compactMap { row in
            guard let provider = ObserveJSON.string(row["provider"]) else { return nil }
            let alias = ObserveJSON.string(row["account_alias"]) ?? ""
            let purposes = row["purposes"] as? [String] ?? []
            return ObserveChannel(
                provider: provider,
                providerLabel: ObserveJSON.string(row["provider_display"]) ?? Self.titleCase(provider),
                account: ObserveJSON.string(row["display"]) ?? alias,
                enabled: row["enabled"] as? Bool ?? false,
                connected: row["connected"] as? Bool ?? false,
                messageCount: ObserveJSON.int(row["message_count"]) ?? 0,
                threadCount: ObserveJSON.int(row["thread_count"]) ?? 0,
                lane: ObserveJSON.string(row["lane"]),
                verificationCodes: purposes.contains("verification_codes")
            )
        }
    }

    static func titleCase(_ raw: String) -> String {
        raw.replacingOccurrences(of: "_", with: " ")
            .split(separator: " ")
            .map { $0.prefix(1).uppercased() + $0.dropFirst() }
            .joined(separator: " ")
    }
}

struct ObserveCalendarStatus: Equatable {
    let enabled: Bool
    let accounts: [String]
    let frequency: String?
    let time: String?
    let lastSyncAt: Date?
    let totalSynced: Int

    init(json: [String: Any]) {
        enabled = json["enabled"] as? Bool ?? false
        accounts = (json["accounts"] as? [String]) ?? []
        frequency = ObserveJSON.string(json["frequency"])
        time = ObserveJSON.string(json["time"])
        lastSyncAt = ObserveJSON.date(json["last_sync_at"])
        totalSynced = ObserveJSON.int(json["total_synced"]) ?? 0
    }

    var cadenceText: String? {
        guard let frequency else { return time }
        let f = frequency.replacingOccurrences(of: "-", with: " ").replacingOccurrences(of: "_", with: " ")
        if let time { return "\(f.prefix(1).uppercased() + f.dropFirst()) at \(time)" }
        return f.prefix(1).uppercased() + f.dropFirst()
    }
}

struct ObserveSubscription: Identifiable, Equatable {
    let id: String
    let name: String
    let enabled: Bool
    let consecutiveFailures: Int
    let provider: String
    let lastSuccessAt: Date?
    let nextRunAt: Date?

    /// Web wording: enabled → Listening (or Retrying after failures), else Paused.
    var stateLabel: String {
        guard enabled else { return "Paused" }
        return consecutiveFailures > 0 ? "Retrying" : "Listening"
    }

    static func page(from object: [String: Any]) -> (items: [ObserveSubscription], total: Int) {
        let rows = object["items"] as? [[String: Any]] ?? []
        let items: [ObserveSubscription] = rows.compactMap { row in
            guard let id = ObserveJSON.string(row["subscription_id"]) else { return nil }
            let action = ObserveJSON.string(row["action_id"]) ?? ""
            let provider = action.split(separator: ".", maxSplits: 1).first.map(String.init) ?? ""
            return ObserveSubscription(
                id: id,
                name: ObserveJSON.string(row["display_name"]) ?? id,
                enabled: row["enabled"] as? Bool ?? false,
                consecutiveFailures: ObserveJSON.int(row["consecutive_failures"]) ?? 0,
                provider: provider.isEmpty ? "Configured source" : ObserveChannel.titleCase(provider),
                lastSuccessAt: ObserveJSON.date(row["last_success_at_ms"]),
                nextRunAt: ObserveJSON.date(row["next_run_at_ms"])
            )
        }
        return (items, ObserveJSON.int(object["total"]) ?? items.count)
    }
}

struct ObserveAmbientStatus: Equatable {
    let enabled: Bool
    let paired: Bool
    let totalSignals: Int
    let acceptedToday: Int?
    let lastSignalAt: Date?

    init(json: [String: Any]) {
        enabled = json["enabled"] as? Bool ?? false
        paired = json["paired"] as? Bool ?? false
        totalSignals = ObserveJSON.int(json["total_signals"]) ?? 0
        acceptedToday = ObserveJSON.int(json["accepted_today"])
        lastSignalAt = ObserveJSON.date(json["last_signal_at"])
    }
}

struct ObserveCatchUpStatus: Equatable {
    let phase: String
    let policyEnabled: Bool
    let processed: Int
    let running: Int
    let remaining: Int

    init(json: [String: Any]) {
        let status = json["status"] as? [String: Any] ?? json
        phase = ObserveJSON.string(status["phase"]) ?? "unknown"
        policyEnabled = (status["policy"] as? [String: Any])?["enabled"] as? Bool ?? false
        processed = ObserveJSON.int(status["processed_items"]) ?? 0
        running = ObserveJSON.int(status["reserved_items"]) ?? 0
        remaining = ObserveJSON.int(status["remaining_items"]) ?? 0
    }

    var phaseLabel: String {
        let p = phase.replacingOccurrences(of: "_", with: " ")
        return p.prefix(1).uppercased() + p.dropFirst()
    }

    var progressLine: String { "\(processed) processed · \(running) running · \(remaining) budget left" }
}

/// The "Sources on" KPI: channels (any enabled) + calendar + browser tabs, each
/// counting 1, plus the total of enabled continuous subscriptions. Nil only when
/// no input has loaded yet (so the card shows "—" instead of a false 0).
enum ObserveSourcesCount {
    static func enabled(
        channels: [ObserveChannel]?,
        calendar: ObserveCalendarStatus?,
        ambient: ObserveAmbientStatus?,
        enabledSubscriptions: Int?
    ) -> Int? {
        if channels == nil, calendar == nil, ambient == nil, enabledSubscriptions == nil { return nil }
        var n = 0
        if channels?.contains(where: { $0.enabled }) == true { n += 1 }
        if calendar?.enabled == true { n += 1 }
        if ambient?.enabled == true { n += 1 }
        return n + (enabledSubscriptions ?? 0)
    }
}

// MARK: - Audio profiles (meeting + listening)

enum ObserveAudioSurface: String, CaseIterable, Identifiable {
    case meeting
    case listening

    var id: String { rawValue }
    var label: String { self == .meeting ? "Meeting" : "Listening" }
    var detail: String {
        self == .meeting
            ? "Calls you join, send the agent to, or share from this phone."
            : "Room capture with \u{201C}Listen here\u{201D} on this phone."
    }
}

struct ObserveAudioProfile: Identifiable, Equatable {
    let id: String
    let surface: ObserveAudioSurface
    /// Enabled stage labels, e.g. ["Voice activity", "Transcription", "Speakers"].
    let stages: [String]

    /// Same label rule as the web control (`compat-` / `-vN` stripped, title case).
    var label: String { Self.label(for: id) }

    var summary: String { stages.isEmpty ? "No stages enabled" : stages.joined(separator: " · ") }

    static func label(for id: String) -> String {
        id.replacingOccurrences(of: "^compat-", with: "", options: .regularExpression)
            .replacingOccurrences(of: "-v[0-9]+$", with: "", options: .regularExpression)
            .replacingOccurrences(of: "-", with: " ")
            .split(separator: " ")
            .map { $0.prefix(1).uppercased() + $0.dropFirst() }
            .joined(separator: " ")
    }
}

struct ObserveAudioCatalog: Equatable {
    let profiles: [ObserveAudioSurface: [ObserveAudioProfile]]
    let defaults: [ObserveAudioSurface: String]

    static let empty = ObserveAudioCatalog(profiles: [:], defaults: [:])

    private static let stageOrder: [(key: String, label: String)] = [
        ("vad", "Voice activity"),
        ("recording_stt", "Transcription"),
        ("streaming_stt", "Live transcription"),
        ("diarization", "Speakers"),
        ("tts", "Voice"),
    ]

    /// `GET /media/providers` → the meeting/listening surface profiles.
    static func decode(providers object: [String: Any]) -> ObserveAudioCatalog {
        let raw = object["surface_profiles"] as? [String: Any] ?? [:]
        var profiles: [ObserveAudioSurface: [ObserveAudioProfile]] = [:]
        for (id, value) in raw {
            guard let dict = value as? [String: Any],
                  let surface = ObserveAudioSurface(rawValue: dict["surface"] as? String ?? "") else { continue }
            let stages = stageOrder.compactMap { stage -> String? in
                guard let s = dict[stage.key] as? [String: Any], s["enabled"] as? Bool == true else { return nil }
                return stage.label
            }
            profiles[surface, default: []].append(ObserveAudioProfile(id: id, surface: surface, stages: stages))
        }
        for key in profiles.keys {
            profiles[key]?.sort { $0.label.localizedCaseInsensitiveCompare($1.label) == .orderedAscending }
        }
        let rawDefaults = object["default_surface_profiles"] as? [String: Any] ?? [:]
        var defaults: [ObserveAudioSurface: String] = [:]
        for surface in ObserveAudioSurface.allCases {
            if let id = ObserveJSON.string(rawDefaults[surface.rawValue]) { defaults[surface] = id }
        }
        return ObserveAudioCatalog(profiles: profiles, defaults: defaults)
    }
}

/// The per-surface profile selection from `GET/PUT /media/preferences`.
enum ObserveAudioPreferences {
    /// `surface_profiles` → meeting/listening explicit selections ("" / absent = default).
    static func selections(from object: [String: Any]) -> [ObserveAudioSurface: String] {
        let raw = object["surface_profiles"] as? [String: Any] ?? [:]
        var out: [ObserveAudioSurface: String] = [:]
        for surface in ObserveAudioSurface.allCases {
            if let id = ObserveJSON.string(raw[surface.rawValue]),
               !["auto", "default"].contains(id.lowercased()) {
                out[surface] = id
            }
        }
        return out
    }

    /// PUT patch selecting `profileId` (nil = configured default) for `surface`.
    /// Matches the web control: a profile change also clears that surface's
    /// per-stage overrides so the new profile's order applies.
    static func patch(surface: ObserveAudioSurface, profileId: String?) -> [String: Any] {
        let stageClears = Dictionary(uniqueKeysWithValues: [
            "vad", "recording_stt", "streaming_stt", "diarization", "tts",
        ].map { ($0, "") })
        return [
            "surface_profiles": [surface.rawValue: profileId ?? ""],
            "surface_stage_options": [surface.rawValue: stageClears],
        ]
    }
}

// MARK: - Broadcast arm + reattach decisions

enum ObserveCaptureRules {
    /// Whether a prepared broadcast session should stop showing as armed: the
    /// server's live list (from a successful fetch) no longer includes it —
    /// the broadcast ended, was stopped, or the armed session idle-timed-out.
    static func broadcastArmExpired(armedSessionId: String?, activeSessionIds: [String]) -> Bool {
        guard let armedSessionId else { return false }
        return !activeSessionIds.contains(armedSessionId)
    }

    enum Reattach: Equatable {
        /// Nothing armed (or the controller is busy): do nothing.
        case none
        /// The armed session is gone server-side: clear the stale arm.
        case clearStale
        /// A broadcast (`mic:true`) session is live: it is the ReplayKit
        /// extension's capture, so the in-app mic must NOT start.
        case broadcastLive(sessionId: String)
        /// An in-app Listen session is live: rebuild the pump + mic engine.
        case resumeMic
    }

    static func reattach(arm: ObservationArm?, activeSessionIds: [String]) -> Reattach {
        guard let arm else { return .none }
        guard activeSessionIds.contains(arm.sessionId) else { return .clearStale }
        return arm.micEnabled ? .broadcastLive(sessionId: arm.sessionId) : .resumeMic
    }
}
