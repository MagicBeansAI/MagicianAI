//  MonitorForm.swift
//  Recurring Monitors (Phase 5, iOS) — the create/edit composer's pure form
//  logic. Twin of `ui/unified-ui/src/lib/monitors/specForm.ts`.
//
//  `validateAndNormalize` mirrors the backend admission gate
//  (`monitor_spec.rs::validate_and_normalize`) field-for-field and
//  reason-for-reason so the sheet rejects locally with the SAME stable
//  snake_case reasons the server would 400 with. The server stays
//  authoritative — anything the mirror wrongly admits still fails on POST
//  with the same reason string.

import Foundation

/// One line-list text field per spec list field, plus the cadence choice.
/// Pure and Equatable — the SwiftUI sheet binds to it; tests drive it directly.
struct MonitorForm: Equatable {

    // Bounds mirrored from monitor_spec.rs (and the web mirror).
    static let maxObjectiveChars = 2000
    static let maxListEntryChars = 500
    static let maxListEntries = 50
    static let maxSourceURLs = 100

    var title = ""
    var objective = ""
    var urlsText = ""
    var domainsText = ""
    var authenticatedText = ""
    var querySeedsText = ""
    var includeRulesText = ""
    var excludeRulesText = ""
    var matchMode: Monitors.MatchMode = .balanced
    var notificationPolicy: Monitors.NotificationPolicy = .materialChanges
    var notifyInitialBaseline = false
    /// A `cadencePresets` id, `custom`, or `none` (run on demand only).
    var cadence = "daily-9"
    var cronExpression = ""
    var timezone = ""

    // MARK: - Cadence presets (web parity)

    struct CadencePreset: Identifiable, Equatable {
        let id: String
        let label: String
        let cron: String
    }

    static let cadencePresets: [CadencePreset] = [
        CadencePreset(id: "hourly", label: "Every hour", cron: "0 * * * *"),
        CadencePreset(id: "daily-9", label: "Every day at 9:00 AM", cron: "0 9 * * *"),
        CadencePreset(id: "daily-18", label: "Every day at 6:00 PM", cron: "0 18 * * *"),
        CadencePreset(id: "weekdays-9", label: "Every weekday at 9:00 AM", cron: "0 9 * * 1-5"),
        CadencePreset(id: "weekly-mon-9", label: "Every Monday at 9:00 AM", cron: "0 9 * * 1"),
        CadencePreset(id: "monthly-1-9", label: "First of the month at 9:00 AM", cron: "0 9 1 * *"),
    ]

    // MARK: - Construction

    /// Pre-fill the edit form from a monitor detail (title + spec + schedule).
    init() {}

    init(detail: Monitors.DetailV1) {
        title = detail.title
        objective = detail.spec.objective
        urlsText = detail.spec.sources.urls.joined(separator: "\n")
        domainsText = detail.spec.sources.domains.joined(separator: "\n")
        authenticatedText = detail.spec.sources.authenticatedSources.joined(separator: "\n")
        querySeedsText = detail.spec.querySeeds.joined(separator: "\n")
        includeRulesText = detail.spec.includeRules.joined(separator: "\n")
        excludeRulesText = detail.spec.excludeRules.joined(separator: "\n")
        matchMode = detail.spec.matchMode
        notificationPolicy = detail.spec.notificationPolicy
        notifyInitialBaseline = detail.spec.notifyInitialBaseline
        if case .cron(let expression, let kindTimezone)? = detail.schedule?.kind {
            let preset = Self.cadencePresets.first { $0.cron == expression }
            cadence = preset?.id ?? "custom"
            cronExpression = expression
            timezone = kindTimezone ?? detail.schedule?.timezone ?? ""
        } else {
            // Interval/Once/OnEvent (and schedule-less) monitors present as
            // cadence "none" in the editor (web parity — the composer only
            // AUTHORS Cron). Saving does NOT clear them: `buildSchedule()`
            // yields nil for "none" and the PATCH then omits the `schedule`
            // key entirely, so the existing schedule stays untouched.
            cadence = "none"
        }
    }

    /// Phase 7 convert prefill: objective seeded from the task DESCRIPTION
    /// (falling back to the title), title kept, cadence pinned to `none`
    /// because conversion never touches the task's schedule (the POST body
    /// is `{spec, title?}` only). Twin of the web `convertFormFromTask`.
    static func convertPrefill(taskTitle: String, taskDescription: String) -> MonitorForm {
        var form = MonitorForm()
        let title = taskTitle.trimmingCharacters(in: .whitespacesAndNewlines)
        let description = taskDescription.trimmingCharacters(in: .whitespacesAndNewlines)
        form.title = title
        form.objective = description.isEmpty ? title : description
        form.cadence = "none"
        return form
    }

    // MARK: - Spec building + admission mirror

    static func splitLines(_ text: String) -> [String] {
        text.split(whereSeparator: \.isNewline)
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
    }

    /// Build the (unvalidated) spec this form describes.
    func buildSpec() -> Monitors.SpecV1 {
        Monitors.SpecV1(
            objective: objective,
            querySeeds: Self.splitLines(querySeedsText),
            sources: Monitors.Sources(
                urls: Self.splitLines(urlsText),
                domains: Self.splitLines(domainsText),
                authenticatedSources: Self.splitLines(authenticatedText)),
            includeRules: Self.splitLines(includeRulesText),
            excludeRules: Self.splitLines(excludeRulesText),
            matchMode: matchMode,
            notificationPolicy: notificationPolicy,
            notifyInitialBaseline: notifyInitialBaseline)
    }

    enum SpecValidation: Equatable {
        case ok(Monitors.SpecV1)
        case rejected(reason: String)
    }

    /// The client mirror of `monitor_spec.rs::validate_and_normalize`.
    static func validateAndNormalize(_ spec: Monitors.SpecV1) -> SpecValidation {
        guard spec.schemaVersion == 1 else {
            return .rejected(reason: "monitor_schema_version_unsupported")
        }
        let objective = spec.objective.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !objective.isEmpty else {
            return .rejected(reason: "monitor_objective_required")
        }
        guard objective.count <= maxObjectiveChars else {
            return .rejected(reason: "monitor_objective_too_long")
        }

        enum Normalized {
            case ok([String])
            case rejected(String)
        }

        func normalizeList(_ entries: [String], field: String) -> Normalized {
            var normalized: [String] = []
            for raw in entries {
                let entry = raw.trimmingCharacters(in: .whitespacesAndNewlines)
                if entry.isEmpty { continue }
                if entry.count > maxListEntryChars {
                    return .rejected("monitor_\(field)_entry_too_long")
                }
                if !normalized.contains(entry) { normalized.append(entry) }
            }
            if normalized.count > maxListEntries {
                return .rejected("monitor_\(field)_too_many_entries")
            }
            return .ok(normalized)
        }

        func normalizeSourceURLs(_ urls: [String]) -> Normalized {
            var normalized: [String] = []
            for raw in urls {
                let url = raw.trimmingCharacters(in: .whitespacesAndNewlines)
                if url.isEmpty { continue }
                guard let parsed = URL(string: url), let scheme = parsed.scheme,
                      parsed.host?.isEmpty == false else {
                    return .rejected("monitor_source_url_invalid")
                }
                guard ["http", "https"].contains(scheme.lowercased()) else {
                    return .rejected("monitor_source_url_scheme_unsupported")
                }
                // Dedup compares the RAW string (parity with the web
                // mirror): trailing-slash or case variants
                // ("https://a.example/p" vs "https://a.example/p/") are NOT
                // collapsed client-side — canonical URL dedup happens
                // server-side. Pinned by `MonitorFormTests`.
                if !normalized.contains(url) { normalized.append(url) }
            }
            if normalized.count > maxSourceURLs {
                return .rejected("monitor_source_urls_too_many")
            }
            return .ok(normalized)
        }

        let querySeeds: [String], includeRules: [String], excludeRules: [String]
        let domains: [String], authenticated: [String], urls: [String]
        switch normalizeList(spec.querySeeds, field: "query_seeds") {
        case .ok(let list): querySeeds = list
        case .rejected(let reason): return .rejected(reason: reason)
        }
        switch normalizeList(spec.includeRules, field: "include_rules") {
        case .ok(let list): includeRules = list
        case .rejected(let reason): return .rejected(reason: reason)
        }
        switch normalizeList(spec.excludeRules, field: "exclude_rules") {
        case .ok(let list): excludeRules = list
        case .rejected(let reason): return .rejected(reason: reason)
        }
        switch normalizeList(spec.sources.domains, field: "domains") {
        case .ok(let list): domains = list
        case .rejected(let reason): return .rejected(reason: reason)
        }
        switch normalizeList(spec.sources.authenticatedSources, field: "authenticated_sources") {
        case .ok(let list): authenticated = list
        case .rejected(let reason): return .rejected(reason: reason)
        }
        switch normalizeSourceURLs(spec.sources.urls) {
        case .ok(let list): urls = list
        case .rejected(let reason): return .rejected(reason: reason)
        }

        if urls.isEmpty && domains.isEmpty && querySeeds.isEmpty {
            return .rejected(reason: "monitor_sources_required")
        }

        return .ok(Monitors.SpecV1(
            objective: objective,
            querySeeds: querySeeds,
            sources: Monitors.Sources(urls: urls, domains: domains,
                                      authenticatedSources: authenticated),
            includeRules: includeRules,
            excludeRules: excludeRules,
            matchMode: spec.matchMode,
            notificationPolicy: spec.notificationPolicy,
            notifyInitialBaseline: spec.notifyInitialBaseline))
    }

    enum ScheduleValidation: Equatable {
        /// `nil` schedule = the explicit "run on demand only" choice.
        case ok(Monitors.ScheduleWire?)
        case rejected(reason: String)
    }

    /// Build the `Task.schedule` wire object from the cadence choice.
    func buildSchedule() -> ScheduleValidation {
        if cadence == "none" { return .ok(nil) }
        let preset = Self.cadencePresets.first { $0.id == cadence }
        let expression = preset?.cron
            ?? cronExpression.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !expression.isEmpty else {
            return .rejected(reason: "monitor_schedule_required")
        }
        guard expression.split(whereSeparator: { $0.isWhitespace }).count == 5 else {
            return .rejected(reason: "monitor_schedule_invalid")
        }
        let zone = timezone.trimmingCharacters(in: .whitespacesAndNewlines)
        return .ok(Monitors.ScheduleWire(
            kind: .cron(expression: expression, timezone: zone.isEmpty ? nil : zone)))
    }

    /// The trimmed title, or nil so the server derives it from the objective.
    var normalizedTitle: String? {
        let trimmed = title.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? nil : trimmed
    }

    // MARK: - Labels

    /// Human message for the stable reasons (fallback: the raw reason).
    static func reasonLabel(_ reason: String) -> String {
        switch reason {
        case "monitor_objective_required":
            return "Describe what to monitor."
        case "monitor_objective_too_long":
            return "Keep the objective under \(maxObjectiveChars) characters."
        case "monitor_sources_required":
            return "Add at least one URL, domain, or search phrase."
        case "monitor_source_url_invalid":
            return "One of the URLs is not a valid URL."
        case "monitor_source_url_scheme_unsupported":
            return "Only http(s) URLs can be monitored."
        case "monitor_source_urls_too_many":
            return "Keep it to \(maxSourceURLs) URLs or fewer."
        case "monitor_schedule_required":
            return "Enter a cron expression or choose a preset."
        case "monitor_schedule_invalid":
            return "The schedule must be a five-field cron expression, for example 0 9 * * *."
        case "monitor_feedback_verdict_invalid":
            return "That feedback verdict isn't supported."
        default:
            if reason.hasSuffix("_entry_too_long") {
                return "One entry is longer than \(maxListEntryChars) characters."
            }
            if reason.hasSuffix("_too_many_entries") {
                return "Keep each list to \(maxListEntries) entries or fewer."
            }
            return reason
        }
    }
}

// MARK: - Shared presentation labels (list / detail / composer)

extension Monitors {

    static func notificationPolicyLabel(_ policy: NotificationPolicy) -> String {
        switch policy {
        case .materialChanges: return "Material changes only"
        case .everyRun: return "Every run"
        case .never: return "Never"
        }
    }

    static func matchModeLabel(_ mode: MatchMode) -> String {
        switch mode {
        case .strict: return "Strict"
        case .balanced: return "Balanced"
        case .broad: return "Broad"
        }
    }

    static func healthLabel(_ health: String) -> String {
        switch health {
        case "ok": return "Healthy"
        case "needs_attention": return "Needs attention"
        case "failing": return "Failing"
        default: return health
        }
    }

    static func runStatusLabel(_ status: String) -> String {
        switch status {
        case "changed": return "Changed"
        case "baseline": return "Baseline"
        case "unchanged": return "Unchanged"
        case "degraded": return "Degraded"
        case "failed": return "Failed"
        case "never_ran": return "Never ran"
        default: return status
        }
    }
}
