//  MonitorDeepLink.swift
//  Recurring Monitors (Phase 5, iOS) — the in-app deep-link resolution path.
//
//  Three inbound shapes resolve to the SAME target (§9.3.4):
//    1. Today `monitor_update` cards — metadata carries the authoritative
//       `monitor_task_id` + `update_id` (feed_api::today_item_from_monitor_update).
//    2. The canonical web route the backend emits everywhere
//       (`/tasks?type=monitors&selected=task_…&update=mu_…` — the
//       `monitor_deep_link` helper / web `monitorsTaskRoute`).
//    3. `magican://monitor/{task_id}?update=mu_…` — the URL scheme host push
//       notifications will ride when iOS push lands (no OS push exists on iOS
//       today; this resolver is the landing path, so push only needs to open
//       the URL).
//
//  `magican://task/{id}` additionally FALLS BACK to monitor mode: TasksView
//  probes `GET /monitors/{id}` and opens the monitor detail on 200, the plain
//  task detail on `monitor_not_found` (see `TasksView.revealRequestedTask`).

import Foundation

extension Monitors {

    /// The resolved destination: a monitor task, optionally pinned to an
    /// exact update record (highlighted in the detail's Updates section).
    struct DeepLinkTarget: Equatable, Identifiable {
        let taskID: String
        let updateID: String?

        var id: String { "\(taskID)#\(updateID ?? "")" }

        init(taskID: String, updateID: String? = nil) {
            self.taskID = taskID
            self.updateID = normalized(updateID)
        }
    }

    private static func normalized(_ value: String?) -> String? {
        guard let value = value?.trimmingCharacters(in: .whitespacesAndNewlines),
              !value.isEmpty else { return nil }
        return value
    }

    /// Parse the canonical monitors route
    /// (`/tasks?type=monitors[&selected=…][&update=…]`). Returns nil for any
    /// non-monitor `/tasks` route and for a monitors route without a
    /// selection (the bare list is not an item target).
    static func parseTasksRoute(_ route: String) -> DeepLinkTarget? {
        guard let components = URLComponents(string: route),
              components.path == "/tasks" || components.path.hasSuffix("/tasks"),
              let query = components.queryItems,
              query.contains(where: { $0.name == "type" && $0.value == "monitors" }),
              let selected = normalized(query.first(where: { $0.name == "selected" })?.value)
        else { return nil }
        let update = query.first(where: { $0.name == "update" })?.value
        return DeepLinkTarget(taskID: selected, updateID: update)
    }

    /// Parse `magican://monitor/{task_id}[?update=mu_…]`.
    static func parseDeepLinkURL(_ url: URL) -> DeepLinkTarget? {
        guard MagicanAppURL.isScheme(url.scheme), url.host == "monitor" else { return nil }
        guard let taskID = normalized(url.pathComponents.dropFirst().first) else { return nil }
        let update = URLComponents(url: url, resolvingAgainstBaseURL: false)?
            .queryItems?.first(where: { $0.name == "update" })?.value
        return DeepLinkTarget(taskID: taskID, updateID: update)
    }

    // MARK: - magican://task fallback-probe termination

    /// Terminal outcome of the `magican://task/{id}` fallback probe
    /// (`TasksView.revealRequestedTask`).
    enum TaskLinkOutcome: Equatable {
        /// `GET /monitors/{id}` returned 200 — open the monitor detail.
        case openMonitor
        /// The probe failed and the task list resolved the id — open the
        /// shared task detail.
        case openTask
        /// The probe failed AND the id is unresolvable right now (offline,
        /// deleted task). The pending target is STILL consumed so
        /// navigation degrades ONCE — re-tapping the link retries.
        case degraded
        /// A newer target replaced this one mid-flight: nothing is
        /// consumed; the new target's own resolution pass handles it.
        case superseded
    }

    /// Applies a terminal probe result to the pending deep-link target.
    /// EVERY terminal outcome consumes the target exactly once — INCLUDING
    /// probe-failed + unresolved (`.degraded`). Leaving the target pending
    /// on that path would re-run the monitor probe on every `$tasks`
    /// publish forever (the infinite re-probe loop the adversarial review
    /// flagged). Pinned by `MonitorDeepLinkTests`.
    static func finishTaskLink(
        actions: AppActions, target: String,
        monitorProbeSucceeded: Bool, taskResolved: Bool
    ) -> TaskLinkOutcome {
        guard actions.taskTargetID == target else { return .superseded }
        actions.consumeTaskTarget()
        if monitorProbeSucceeded { return .openMonitor }
        return taskResolved ? .openTask : .degraded
    }
}

// MARK: - Today card resolution

extension TodayItem {

    /// The monitor destination a Today card describes, or nil for
    /// non-monitor cards. Metadata is authoritative (`monitor_task_id` +
    /// `update_id`); the canonical `source_url` route is the fallback for
    /// records that predate the metadata block.
    var monitorDeepLinkTarget: Monitors.DeepLinkTarget? {
        let record = metadata.objectValue
        let isMonitorUpdate = sourceKind == "monitor_update"
            || record?["update_kind"]?.stringValue == "monitor_update"
        if isMonitorUpdate {
            if let taskID = record?["monitor_task_id"]?.stringValue,
               !taskID.isEmpty {
                return Monitors.DeepLinkTarget(
                    taskID: taskID,
                    updateID: record?["update_id"]?.stringValue)
            }
            if let taskID, !taskID.isEmpty {
                return Monitors.DeepLinkTarget(taskID: taskID, updateID: sourceID)
            }
        }
        // Any card carrying the canonical monitors route (e.g. the
        // access-problem escalation's monitor link) resolves too.
        if let route = sourceURL, let target = Monitors.parseTasksRoute(route) {
            return target
        }
        return nil
    }
}
