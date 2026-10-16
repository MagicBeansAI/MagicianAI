//  MonitorModels.swift
//  Recurring Monitors (Phase 5, iOS) — the typed wire contract.
//
//  Swift mirror of the CANONICAL monitor wire shapes:
//    - fixtures:  magician/tests/fixtures/monitors/*.json (single source of
//      truth — the Rust, web, and these iOS models all decode the same files;
//      `MonitorContractFixtureTests` fails the build on incompatible drift)
//    - backend:   magician/src/magician_v2/api/monitors_api.rs (+ the
//      `artifact_v2/monitor_spec.rs` / `monitor_run.rs` / `monitor_updates.rs`
//      contracts it serves)
//    - web twin:  ui/unified-ui/src/lib/types/monitor.ts
//
//  Everything is namespaced under `Monitors` (the `LTM` idiom) and uses
//  explicit snake_case CodingKeys. Optionality follows the contract exactly:
//  models stay TOLERANT of old tasks/records that predate a field, and of
//  reserved-not-yet-emitted fields (`next_run_at`, `health: "failing"`).

import Foundation

/// Namespace for the Recurring Monitors client stack (models + API client +
/// deep-link helpers). Mirrors the `LTM` namespace idiom.
enum Monitors {}

// MARK: - Spec enums (strict — a vocabulary change is a contract change)

extension Monitors {

    enum MatchMode: String, Codable, Equatable, CaseIterable {
        case strict, balanced, broad
    }

    enum NotificationPolicy: String, Codable, Equatable, CaseIterable {
        case materialChanges = "material_changes"
        case everyRun = "every_run"
        case never
    }

    /// Run/update status vocabulary. List rows additionally use the string
    /// `"never_ran"` (Phase 1 `TaskState` projection), which is NOT a run
    /// status — `ListItemV1.lastRunStatus` therefore stays a plain String.
    enum RunStatus: String, Codable, Equatable {
        case baseline, changed, unchanged, degraded, failed
    }

    enum FindingClassification: String, Codable, Equatable {
        case new, updated, unchanged
        case possiblyRemoved = "possibly_removed"
    }

    enum SourceOutcomeStatus: String, Codable, Equatable {
        case ok
        case authFailed = "auth_failed"
        case timeout
        case rateLimited = "rate_limited"
        case error
    }
}

// MARK: - MonitorSpecV1

extension Monitors {

    struct Sources: Codable, Equatable {
        var urls: [String]
        var domains: [String]
        var authenticatedSources: [String]

        enum CodingKeys: String, CodingKey {
            case urls, domains
            case authenticatedSources = "authenticated_sources"
        }

        init(urls: [String] = [], domains: [String] = [],
             authenticatedSources: [String] = []) {
            self.urls = urls
            self.domains = domains
            self.authenticatedSources = authenticatedSources
        }
    }

    /// The typed monitor contract (`monitor_spec_v1.json`).
    struct SpecV1: Codable, Equatable {
        var schemaVersion: Int
        var objective: String
        var querySeeds: [String]
        var sources: Sources
        var includeRules: [String]
        var excludeRules: [String]
        var matchMode: MatchMode
        var notificationPolicy: NotificationPolicy
        var notifyInitialBaseline: Bool

        enum CodingKeys: String, CodingKey {
            case schemaVersion = "schema_version"
            case objective
            case querySeeds = "query_seeds"
            case sources
            case includeRules = "include_rules"
            case excludeRules = "exclude_rules"
            case matchMode = "match_mode"
            case notificationPolicy = "notification_policy"
            case notifyInitialBaseline = "notify_initial_baseline"
        }

        init(schemaVersion: Int = 1,
             objective: String,
             querySeeds: [String] = [],
             sources: Sources = Sources(),
             includeRules: [String] = [],
             excludeRules: [String] = [],
             matchMode: MatchMode = .balanced,
             notificationPolicy: NotificationPolicy = .materialChanges,
             notifyInitialBaseline: Bool = false) {
            self.schemaVersion = schemaVersion
            self.objective = objective
            self.querySeeds = querySeeds
            self.sources = sources
            self.includeRules = includeRules
            self.excludeRules = excludeRules
            self.matchMode = matchMode
            self.notificationPolicy = notificationPolicy
            self.notifyInitialBaseline = notifyInitialBaseline
        }
    }
}

// MARK: - MonitorRunResultV1 family

extension Monitors {

    struct EvidenceV1: Codable, Equatable {
        var kind: String
        var value: String
        var url: String?
    }

    struct FindingV1: Codable, Equatable {
        var stableKey: String
        var title: String
        /// Optional AND nullable on the wire (fixtures pin `null`).
        var canonicalURL: String?
        var source: String
        var observedAt: String
        var publishedAt: String?
        var summary: String
        var whyItMatters: String
        var entities: [String]
        var evidence: [EvidenceV1]
        var contentFingerprint: String
        var classification: FindingClassification

        enum CodingKeys: String, CodingKey {
            case stableKey = "stable_key"
            case title
            case canonicalURL = "canonical_url"
            case source
            case observedAt = "observed_at"
            case publishedAt = "published_at"
            case summary
            case whyItMatters = "why_it_matters"
            case entities, evidence
            case contentFingerprint = "content_fingerprint"
            case classification
        }
    }

    struct SourceOutcomeV1: Codable, Equatable {
        var source: String
        var status: SourceOutcomeStatus
        var complete: Bool
        var itemsScanned: Int
        var note: String?

        enum CodingKeys: String, CodingKey {
            case source, status, complete, note
            case itemsScanned = "items_scanned"
        }
    }

    struct CountsV1: Codable, Equatable {
        var scanned: Int
        var new: Int
        var updated: Int
        var unchanged: Int
        var possiblyRemoved: Int

        enum CodingKeys: String, CodingKey {
            case scanned, new, updated, unchanged
            case possiblyRemoved = "possibly_removed"
        }
    }

    /// Present when a source needs the user (login/permission).
    struct AccessProblemV1: Codable, Equatable {
        var source: String
        var kind: String
        var message: String
        var since: String
    }

    /// One accepted, server-finalized run
    /// (`monitor_run_result_v1_{changed,unchanged,degraded}.json`).
    struct RunResultV1: Codable, Equatable {
        var monitorTaskID: String
        var executionID: String
        var monitorRevision: Int
        var startedAt: String
        var completedAt: String
        var status: RunStatus
        var completeScan: Bool
        var sourceOutcomes: [SourceOutcomeV1]
        var counts: CountsV1
        var findings: [FindingV1]
        var runFingerprint: String
        /// Present ONLY when status is `changed` — a finalized `baseline` or
        /// `failed` run never carries one (§7 invariants).
        var changeFingerprint: String?
        var accessProblem: AccessProblemV1?

        enum CodingKeys: String, CodingKey {
            case monitorTaskID = "monitor_task_id"
            case executionID = "execution_id"
            case monitorRevision = "monitor_revision"
            case startedAt = "started_at"
            case completedAt = "completed_at"
            case status
            case completeScan = "complete_scan"
            case sourceOutcomes = "source_outcomes"
            case counts, findings
            case runFingerprint = "run_fingerprint"
            case changeFingerprint = "change_fingerprint"
            case accessProblem = "access_problem"
        }
    }
}

// MARK: - MonitorUpdateDetailV1 (Phase 3 notification ledger record)

extension Monitors {

    struct NotificationV1: Codable, Equatable {
        var policy: NotificationPolicy
        var emitted: Bool
        var channel: String
        /// §7.4: scope + monitor task id + revision + change fingerprint + channel.
        var dedupeKey: String

        enum CodingKeys: String, CodingKey {
            case policy, emitted, channel
            case dedupeKey = "dedupe_key"
        }
    }

    /// One durable update record (`monitor_update_detail_v1.json`).
    struct UpdateDetailV1: Codable, Equatable, Identifiable {
        var updateID: String
        var monitorTaskID: String
        var monitorRevision: Int
        var executionID: String
        var occurredAt: String
        var status: RunStatus
        /// Absent on updates without a change fingerprint (quiet `every_run`
        /// receipts, empty baselines).
        var changeFingerprint: String?
        var headline: String
        var summary: String
        var findings: [FindingV1]
        var notification: NotificationV1

        var id: String { updateID }

        enum CodingKeys: String, CodingKey {
            case updateID = "update_id"
            case monitorTaskID = "monitor_task_id"
            case monitorRevision = "monitor_revision"
            case executionID = "execution_id"
            case occurredAt = "occurred_at"
            case status
            case changeFingerprint = "change_fingerprint"
            case headline, summary, findings, notification
        }
    }
}

// MARK: - List row + page envelopes

extension Monitors {

    /// One row of `GET /monitors?limit=&cursor=&state=`
    /// (`monitor_list_page_v1.json`).
    struct ListItemV1: Codable, Equatable, Identifiable {
        var taskID: String
        var title: String
        var objective: String
        /// `active` | `paused`.
        var state: String
        /// Server-rendered, shown VERBATIM (e.g. `Cron 0 6 * * 1
        /// (America/Los_Angeles)`, `Every 3600s`, `unscheduled`).
        var cadenceSummary: String
        var monitorRevision: Int
        var lastRunAt: String?
        /// Full run-status vocabulary plus `never_ran`. The Phase 1 backend
        /// derives this from `TaskState` alone and only emits
        /// `never_ran` | `unchanged` | `failed`; the richer statuses arrive
        /// once list rows read the run ledger — hence a String, not RunStatus.
        var lastRunStatus: String
        /// Reserved, NOT emitted yet (next-fire time is in-memory scheduler
        /// state); the fixture models the future shape.
        var nextRunAt: String?
        /// `ok` | `needs_attention` today; `failing` reserved-not-emitted.
        var health: String

        var id: String { taskID }

        enum CodingKeys: String, CodingKey {
            case taskID = "task_id"
            case title, objective, state, health
            case cadenceSummary = "cadence_summary"
            case monitorRevision = "monitor_revision"
            case lastRunAt = "last_run_at"
            case lastRunStatus = "last_run_status"
            case nextRunAt = "next_run_at"
        }
    }

    /// Cursor page envelope of the monitors list
    /// (`{items, next_cursor, limit, total, offset}`; `next_cursor` is `null`
    /// on the last page).
    struct ListPageV1: Codable, Equatable {
        var items: [ListItemV1]
        var nextCursor: String?
        var limit: Int
        /// Monitors in the whole filtered pool the cursor is walking — the
        /// size of the corpus, never the size of the page.
        ///
        /// OPTIONAL, and `nil` is a real state: a binary from before the
        /// additive envelope sends no `total`, and absent means "no page
        /// count, cursor paging only" — never zero pages. A default of `0`
        /// would report an empty pool while its rows sit on the screen.
        var total: Int?
        /// Where this page starts in that corpus — the position the cursor
        /// RESOLVED to, not a number the client sent (`/monitors` takes no
        /// `offset` parameter). Absent for the same reason as `total`.
        var offset: Int?

        enum CodingKeys: String, CodingKey {
            case items, limit, total, offset
            case nextCursor = "next_cursor"
        }

        init(items: [ListItemV1], nextCursor: String?, limit: Int,
             total: Int? = nil, offset: Int? = nil) {
            self.items = items
            self.nextCursor = nextCursor
            self.limit = limit
            self.total = total
            self.offset = offset
        }

        /// `next_cursor` must decode `null` (last page) as well as absent, and
        /// `total`/`offset` must survive both too — see `total`.
        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            items = try c.decode([ListItemV1].self, forKey: .items)
            nextCursor = try c.decodeIfPresent(String.self, forKey: .nextCursor)
            limit = try c.decode(Int.self, forKey: .limit)
            total = try c.decodeIfPresent(Int.self, forKey: .total)
            offset = try c.decodeIfPresent(Int.self, forKey: .offset)
        }
    }

    /// Simple page envelope of the runs/updates read seams (`next_cursor` is
    /// always `null` today).
    struct ItemsPageV1<Item: Codable & Equatable>: Codable, Equatable {
        var items: [Item]
        var nextCursor: String?
        var limit: Int

        enum CodingKeys: String, CodingKey {
            case items, limit
            case nextCursor = "next_cursor"
        }

        init(items: [Item], nextCursor: String?, limit: Int) {
            self.items = items
            self.nextCursor = nextCursor
            self.limit = limit
        }

        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            items = try c.decode([Item].self, forKey: .items)
            nextCursor = try c.decodeIfPresent(String.self, forKey: .nextCursor)
            limit = try c.decode(Int.self, forKey: .limit)
        }
    }
}

// MARK: - Task schedule wire (serde EXTERNAL enum tagging)

extension Monitors {

    /// `TaskScheduleKind` on the wire: ONE variant key wrapping the variant
    /// fields — `{"Cron":{"expression":…,"timezone":…}}` /
    /// `{"Interval":{"seconds":…}}`. KNOWN variants decode STRICTLY (a
    /// malformed `Cron`/`Interval` payload throws — silently demoting it to
    /// `.other` would render "unscheduled" and mask backend regressions);
    /// only UNKNOWN variant keys (`Once`/`OnEvent`/future kinds) ride the
    /// tolerant `.other` arm untouched (monitor surfaces only AUTHOR Cron).
    enum ScheduleKindWire: Equatable {
        case cron(expression: String, timezone: String?)
        case interval(seconds: Int, jitterSeconds: Int?)
        case other(JSONValue)
    }

    /// The `Task.schedule` wire object (the existing `TaskSchedule` shape).
    /// Unknown sibling fields (retention, …) are ignored on decode; edits
    /// replace the schedule wholesale, mirroring the web composer.
    struct ScheduleWire: Codable, Equatable {
        var kind: ScheduleKindWire
        var timezone: String?
        var maxRuns: Int?
        var paused: Bool?

        enum CodingKeys: String, CodingKey {
            case kind, timezone, paused
            case maxRuns = "max_runs"
        }

        init(kind: ScheduleKindWire, timezone: String? = nil,
             maxRuns: Int? = nil, paused: Bool? = nil) {
            self.kind = kind
            self.timezone = timezone
            self.maxRuns = maxRuns
            self.paused = paused
        }
    }
}

extension Monitors.ScheduleKindWire: Codable {

    private struct VariantKey: CodingKey {
        var stringValue: String
        var intValue: Int? { nil }
        init(_ value: String) { stringValue = value }
        init?(stringValue: String) { self.stringValue = stringValue }
        init?(intValue: Int) { nil }
    }

    private struct CronPayload: Codable {
        var expression: String
        var timezone: String?
    }

    private struct IntervalPayload: Codable {
        var seconds: Int
        var jitterSeconds: Int?
        enum CodingKeys: String, CodingKey {
            case seconds
            case jitterSeconds = "jitter_seconds"
        }
    }

    init(from decoder: Decoder) throws {
        // STRICT for KNOWN variants: when the "Cron"/"Interval" key is
        // present its payload MUST decode — a malformed known variant
        // (e.g. `{"Cron":{}}` missing `expression`) THROWS instead of
        // silently demoting to `.other`, which would show "unscheduled",
        // misread the edit form, and mask a backend regression. Only
        // UNKNOWN variant keys fall through to the tolerant `.other` arm.
        let container = try decoder.container(keyedBy: VariantKey.self)
        if let key = container.allKeys.first(where: { $0.stringValue == "Cron" }) {
            let payload = try container.decode(CronPayload.self, forKey: key)
            self = .cron(expression: payload.expression, timezone: payload.timezone)
            return
        }
        if let key = container.allKeys.first(where: { $0.stringValue == "Interval" }) {
            let payload = try container.decode(IntervalPayload.self, forKey: key)
            self = .interval(seconds: payload.seconds,
                             jitterSeconds: payload.jitterSeconds)
            return
        }
        self = .other((try? JSONValue(from: decoder)) ?? .null)
    }

    func encode(to encoder: Encoder) throws {
        switch self {
        case .cron(let expression, let timezone):
            var container = encoder.container(keyedBy: VariantKey.self)
            try container.encode(CronPayload(expression: expression, timezone: timezone),
                                 forKey: VariantKey("Cron"))
        case .interval(let seconds, let jitterSeconds):
            var container = encoder.container(keyedBy: VariantKey.self)
            try container.encode(IntervalPayload(seconds: seconds, jitterSeconds: jitterSeconds),
                                 forKey: VariantKey("Interval"))
        case .other(let value):
            try value.encode(to: encoder)
        }
    }
}

// MARK: - Detail + request/response bodies

extension Monitors {

    /// `GET /monitors/{task_id}` — the monitor detail projection.
    struct DetailV1: Codable, Equatable {
        struct StateBlock: Codable, Equatable {
            var status: String
            var scheduleFireCount: Int

            enum CodingKeys: String, CodingKey {
                case status
                case scheduleFireCount = "schedule_fire_count"
            }
        }

        var taskID: String
        var title: String
        var spec: SpecV1
        var monitorRevision: Int
        var schedule: ScheduleWire?
        var state: StateBlock
        var createdAt: String
        var updatedAt: String
        /// Includes the read-time-projected reserved `system:monitor` tag.
        var tags: [String]

        enum CodingKeys: String, CodingKey {
            case taskID = "task_id"
            case title, spec, schedule, state, tags
            case monitorRevision = "monitor_revision"
            case createdAt = "created_at"
            case updatedAt = "updated_at"
        }
    }

    /// `POST /monitors` body — `{title?, spec, schedule?}`.
    struct CreateRequestV1: Encodable {
        var title: String?
        var spec: SpecV1
        var schedule: ScheduleWire?

        func encode(to encoder: Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            try c.encodeIfPresent(title, forKey: .title)
            try c.encode(spec, forKey: .spec)
            try c.encodeIfPresent(schedule, forKey: .schedule)
        }

        enum CodingKeys: String, CodingKey { case title, spec, schedule }
    }

    /// `PATCH /monitors/{task_id}` body — provided fields replace.
    struct UpdateRequestV1: Encodable {
        var title: String?
        var spec: SpecV1?
        var schedule: ScheduleWire?

        func encode(to encoder: Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            try c.encodeIfPresent(title, forKey: .title)
            try c.encodeIfPresent(spec, forKey: .spec)
            try c.encodeIfPresent(schedule, forKey: .schedule)
        }

        enum CodingKeys: String, CodingKey { case title, spec, schedule }
    }

    /// `POST /monitors` (201) / `PATCH /monitors/{task_id}` (200) response.
    struct MutationResponseV1: Codable, Equatable {
        var taskID: String
        var monitorRevision: Int

        enum CodingKeys: String, CodingKey {
            case taskID = "task_id"
            case monitorRevision = "monitor_revision"
        }
    }

    /// `POST /monitors/{task_id}/convert` body — the FIXED Phase 7
    /// conversion contract: `{spec, title?}` ONLY. The task keeps its id,
    /// schedule, history, executions, and outputs; there is deliberately
    /// no `schedule` key.
    struct ConvertRequestV1: Encodable {
        var spec: SpecV1
        var title: String?

        func encode(to encoder: Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            try c.encode(spec, forKey: .spec)
            try c.encodeIfPresent(title, forKey: .title)
        }

        enum CodingKeys: String, CodingKey { case spec, title }
    }

    /// Convert response — `200 {task_id, monitor_revision: 1, converted:
    /// true}`. Errors: 404 `task_not_found` · 409 `monitor_already_exists`
    /// (incl. archived former monitors) · 409
    /// `task_not_eligible_for_monitor` · 400 the standard `monitor_*`
    /// admission reasons.
    struct ConvertResponseV1: Codable, Equatable {
        var taskID: String
        var monitorRevision: Int
        var converted: Bool

        enum CodingKeys: String, CodingKey {
            case taskID = "task_id"
            case monitorRevision = "monitor_revision"
            case converted
        }
    }

    /// `POST /monitors/{task_id}/pause` · `/resume` response —
    /// `{task_id, state}` where state is `active` | `paused`.
    struct StateChangeResponseV1: Codable, Equatable {
        var taskID: String
        var state: String

        enum CodingKeys: String, CodingKey {
            case taskID = "task_id"
            case state
        }
    }

    /// `DELETE /monitors/{task_id}` response (the DELETE stamps `archived`;
    /// the monitor is gone from every monitor surface afterwards).
    struct DeleteResponseV1: Codable, Equatable {
        var ok: Bool
        var taskID: String
        var filesRemoved: Bool

        enum CodingKeys: String, CodingKey {
            case ok
            case taskID = "task_id"
            case filesRemoved = "files_removed"
        }
    }
}

// MARK: - Phase 6 feedback (plan §10)

extension Monitors {

    enum FeedbackVerdict: String, Codable, Equatable, CaseIterable {
        case useful
        case notRelevant = "not_relevant"
    }

    /// `POST /monitors/{task_id}/updates/{update_id}/feedback` body.
    struct FeedbackRequestV1: Encodable {
        var verdict: FeedbackVerdict
        /// Optional, ≤500 chars.
        var note: String?

        func encode(to encoder: Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            try c.encode(verdict, forKey: .verdict)
            try c.encodeIfPresent(note, forKey: .note)
        }

        enum CodingKeys: String, CodingKey { case verdict, note }
    }

    /// Feedback POST response. `recorded == false` is the idempotent replay
    /// of the SAME verdict (nothing new stored); posting the OPPOSITE verdict
    /// replaces the stored one and records `true`. Either way the response
    /// carries the authoritative verdict + `feedback_id` (`mf_…`).
    struct FeedbackResponseV1: Codable, Equatable {
        var taskID: String
        var updateID: String
        var verdict: FeedbackVerdict
        var recorded: Bool
        var feedbackID: String

        enum CodingKeys: String, CodingKey {
            case taskID = "task_id"
            case updateID = "update_id"
            case verdict, recorded
            case feedbackID = "feedback_id"
        }
    }

    /// One row of `GET /monitors/{task_id}/feedback?limit=`.
    struct FeedbackRecordV1: Codable, Equatable, Identifiable {
        var feedbackID: String
        var updateID: String
        var verdict: FeedbackVerdict
        var note: String?
        var recordedAt: String

        var id: String { feedbackID }

        enum CodingKeys: String, CodingKey {
            case feedbackID = "feedback_id"
            case updateID = "update_id"
            case verdict, note
            case recordedAt = "recorded_at"
        }
    }

    /// Client-side per-update verdict state (stored or optimistic).
    struct UpdateFeedbackState: Equatable {
        var verdict: FeedbackVerdict
        /// nil while an optimistic submit is in flight (the server owns ids).
        var feedbackID: String?
        var inFlight: Bool
    }

    /// Merge stored records into per-update state: ONE verdict per
    /// `update_id`, the newest `recorded_at` wins regardless of item order.
    /// On unparseable or tied timestamps the FIRST-SEEN record wins (the
    /// server returns newest first, so first-seen is newest under the
    /// server's own ordering).
    static func feedbackStates(
        from records: [FeedbackRecordV1]
    ) -> [String: UpdateFeedbackState] {
        var newest: [String: (record: FeedbackRecordV1, at: Date?)] = [:]
        for record in records {
            let at = TaskV3.parseISO(record.recordedAt)
            guard let existing = newest[record.updateID] else {
                newest[record.updateID] = (record, at)
                continue
            }
            let laterTimestamp: Bool
            if let at, let existingAt = existing.at {
                laterTimestamp = at > existingAt
            } else {
                laterTimestamp = false
            }
            let onlyThisParses = at != nil && existing.at == nil
            if laterTimestamp || onlyThisParses {
                newest[record.updateID] = (record, at)
            }
        }
        return newest.mapValues {
            UpdateFeedbackState(verdict: $0.record.verdict,
                                feedbackID: $0.record.feedbackID,
                                inFlight: false)
        }
    }

    /// §10: feedback rides MATERIAL updates — a `changed` ledger record.
    /// Baselines and quiet `every_run` receipts don't take verdicts.
    static func isMaterialUpdate(_ update: UpdateDetailV1) -> Bool {
        update.status == .changed
    }

    static func feedbackVerdictLabel(_ verdict: FeedbackVerdict) -> String {
        switch verdict {
        case .useful: return "Useful"
        case .notRelevant: return "Not relevant"
        }
    }
}

// MARK: - Cadence summary mirror (monitors_api::cadence_summary)

extension Monitors {

    /// Mirror of the backend's ONE human cadence string (list rows, chat
    /// preview, review card): `Cron <expr> (<tz>)` / `Every <n>s` /
    /// `Once at <ts>` / `On event <pattern>` / `unscheduled`.
    static func cadenceSummary(_ schedule: ScheduleWire?) -> String {
        guard let schedule else { return "unscheduled" }
        switch schedule.kind {
        case .cron(let expression, let kindTimezone):
            let timezone = (kindTimezone ?? schedule.timezone)?
                .trimmingCharacters(in: .whitespacesAndNewlines)
            if let timezone, !timezone.isEmpty {
                return "Cron \(expression) (\(timezone))"
            }
            return "Cron \(expression)"
        case .interval(let seconds, _):
            return "Every \(seconds)s"
        case .other(let value):
            guard let object = value.objectValue else { return "unscheduled" }
            if let at = object["Once"]?.objectValue?["at"]?.stringValue {
                return "Once at \(at)"
            }
            if let pattern = object["OnEvent"]?.objectValue?["event_pattern"]?.stringValue {
                return "On event \(pattern)"
            }
            return "unscheduled"
        }
    }
}
