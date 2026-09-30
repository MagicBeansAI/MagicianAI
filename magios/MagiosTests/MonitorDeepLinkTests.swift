//  MonitorDeepLinkTests.swift
//  Recurring Monitors (Phase 5, iOS) — deep-link resolution: the canonical
//  monitors route, the `magican://monitor` URL, Today `monitor_update` card
//  metadata, and the AppActions monitor target plumbing.

import XCTest
@testable import Magician

final class MonitorDeepLinkTests: XCTestCase {

    // MARK: - Canonical route parsing

    func testParseTasksRouteWithSelectedAndUpdate() {
        let target = Monitors.parseTasksRoute(
            "/tasks?type=monitors&selected=task_monitor_fixture_001&update=mu_fixture_0001")
        XCTAssertEqual(target?.taskID, "task_monitor_fixture_001")
        XCTAssertEqual(target?.updateID, "mu_fixture_0001")
    }

    func testParseTasksRouteWithoutUpdate() {
        let target = Monitors.parseTasksRoute("/tasks?type=monitors&selected=task_a")
        XCTAssertEqual(target?.taskID, "task_a")
        XCTAssertNil(target?.updateID)
    }

    func testParseTasksRouteRejectsNonMonitorShapes() {
        XCTAssertNil(Monitors.parseTasksRoute("/tasks?selected=task_a"),
                     "a plain tasks route is not a monitor target")
        XCTAssertNil(Monitors.parseTasksRoute("/tasks?type=monitors"),
                     "the bare monitors list is not an item target")
        XCTAssertNil(Monitors.parseTasksRoute("/today?type=monitors&selected=x"))
        XCTAssertNil(Monitors.parseTasksRoute("/tasks?type=monitors&selected="))
    }

    // MARK: - magican://monitor URL parsing

    func testParseDeepLinkURL() throws {
        let url = try XCTUnwrap(URL(string: "magican://monitor/task_1?update=mu_2"))
        let target = Monitors.parseDeepLinkURL(url)
        XCTAssertEqual(target?.taskID, "task_1")
        XCTAssertEqual(target?.updateID, "mu_2")

        let bare = try XCTUnwrap(URL(string: "magican://monitor/task_1"))
        XCTAssertEqual(Monitors.parseDeepLinkURL(bare)?.taskID, "task_1")
        XCTAssertNil(Monitors.parseDeepLinkURL(bare)?.updateID)

        XCTAssertNil(Monitors.parseDeepLinkURL(
            try XCTUnwrap(URL(string: "magican://task/task_1"))),
            "magican://task stays on the task path (fallback probe handles monitors)")
        XCTAssertNil(Monitors.parseDeepLinkURL(
            try XCTUnwrap(URL(string: "https://monitor/task_1"))))
        XCTAssertNil(Monitors.parseDeepLinkURL(
            try XCTUnwrap(URL(string: "magican://monitor"))),
            "a monitor link without a task id resolves nothing")
    }

    // MARK: - Today card resolution

    private func todayItem(_ json: String) throws -> TodayItem {
        try JSONDecoder().decode(TodayItem.self, from: Data(json.utf8))
    }

    func testMonitorUpdateCardResolvesFromMetadata() throws {
        // The exact metadata feed_api::today_item_from_monitor_update emits.
        let item = try todayItem("""
        {"id":"today:changed:monitor_update:mu_fixture_0001","section":"changed",
         "priority":640,"title":"Acme Robotics pricing: Pro plan +$10/month",
         "reason":"A monitor you set up found a material change.",
         "source_kind":"monitor_update","source_id":"mu_fixture_0001",
         "source_url":"/tasks?type=monitors&selected=task_monitor_fixture_001&update=mu_fixture_0001",
         "task_id":"task_monitor_fixture_001","status":"info",
         "metadata":{"update_kind":"monitor_update",
           "monitor_task_id":"task_monitor_fixture_001",
           "update_id":"mu_fixture_0001","monitor_revision":2},
         "created_at":1,"updated_at":1}
        """)
        let target = try XCTUnwrap(item.monitorDeepLinkTarget)
        XCTAssertEqual(target.taskID, "task_monitor_fixture_001")
        XCTAssertEqual(target.updateID, "mu_fixture_0001")
    }

    func testMonitorUpdateCardFallsBackToRouteThenTaskID() throws {
        // Metadata-less record → the canonical source_url route resolves.
        let routed = try todayItem("""
        {"id":"x","section":"changed","priority":1,"title":"t","reason":"r",
         "source_kind":"monitor_update","source_id":"mu_9",
         "source_url":"/tasks?type=monitors&selected=task_9&update=mu_9",
         "status":"info","created_at":1,"updated_at":1}
        """)
        XCTAssertEqual(routed.monitorDeepLinkTarget?.taskID, "task_9")
        XCTAssertEqual(routed.monitorDeepLinkTarget?.updateID, "mu_9")

        // No metadata, no route → the card's task id + source id (update id).
        let bare = try todayItem("""
        {"id":"x","section":"changed","priority":1,"title":"t","reason":"r",
         "source_kind":"monitor_update","source_id":"mu_7",
         "task_id":"task_7","status":"info","created_at":1,"updated_at":1}
        """)
        XCTAssertEqual(bare.monitorDeepLinkTarget?.taskID, "task_7")
        XCTAssertEqual(bare.monitorDeepLinkTarget?.updateID, "mu_7")
    }

    func testOrdinaryCardsResolveNoMonitorTarget() throws {
        let task = try todayItem("""
        {"id":"x","section":"active_work","priority":1,"title":"t","reason":"r",
         "source_kind":"task","source_id":"task_1","source_url":"/tasks?task=task_1",
         "task_id":"task_1","status":"running","created_at":1,"updated_at":1}
        """)
        XCTAssertNil(task.monitorDeepLinkTarget,
                     "plain task cards keep the existing task routing")
    }

    // MARK: - AppActions plumbing

    func testRequestMonitorSetsAndConsumesTarget() {
        let actions = AppActions()
        XCTAssertNil(actions.monitorTargetTaskID)
        actions.requestMonitor(taskID: "task_1", updateID: "mu_2")
        XCTAssertEqual(actions.monitorRequestID, 1)
        XCTAssertEqual(actions.monitorTargetTaskID, "task_1")
        XCTAssertEqual(actions.monitorTargetUpdateID, "mu_2")
        actions.consumeMonitorTarget()
        XCTAssertNil(actions.monitorTargetTaskID)
        XCTAssertNil(actions.monitorTargetUpdateID)

        actions.requestMonitor(taskID: "task_2")
        XCTAssertEqual(actions.monitorRequestID, 2)
        XCTAssertNil(actions.monitorTargetUpdateID)
    }

    // MARK: - magican://task fallback-probe termination (adversarial-review C2)
    //
    // `TasksView.revealRequestedTask` itself is view code (untestable at the
    // unit level), so the extracted decision seam — `Monitors.finishTaskLink`,
    // which the view routes EVERY terminal probe outcome through — is pinned
    // here instead.

    func testFinishTaskLinkConsumesUnresolvedTargetExactlyOnce() {
        let actions = AppActions()
        actions.requestTask("task_gone")
        // Offline/unresolvable: the monitor probe failed AND the task list
        // could not resolve the id.
        let outcome = Monitors.finishTaskLink(
            actions: actions, target: "task_gone",
            monitorProbeSucceeded: false, taskResolved: false)
        XCTAssertEqual(outcome, .degraded)
        XCTAssertNil(actions.taskTargetID,
                     "the unresolved path must CONSUME the pending target — " +
                     "degrade once, not re-probe on every $tasks publish")
        // The next $tasks publish finds nothing pending: no loop.
        XCTAssertEqual(Monitors.finishTaskLink(
            actions: actions, target: "task_gone",
            monitorProbeSucceeded: false, taskResolved: false), .superseded)
        XCTAssertNil(actions.taskTargetID)
    }

    func testFinishTaskLinkOutcomeTable() {
        let actions = AppActions()
        // Monitor probe 200 → open the monitor detail, target consumed.
        actions.requestTask("t1")
        XCTAssertEqual(Monitors.finishTaskLink(
            actions: actions, target: "t1",
            monitorProbeSucceeded: true, taskResolved: false), .openMonitor)
        XCTAssertNil(actions.taskTargetID)
        // Probe failed, task resolved → open the task, target consumed.
        actions.requestTask("t2")
        XCTAssertEqual(Monitors.finishTaskLink(
            actions: actions, target: "t2",
            monitorProbeSucceeded: false, taskResolved: true), .openTask)
        XCTAssertNil(actions.taskTargetID)
        // A stale probe completing after a retarget must NOT consume the
        // newer pending target.
        actions.requestTask("t3")
        XCTAssertEqual(Monitors.finishTaskLink(
            actions: actions, target: "t2",
            monitorProbeSucceeded: false, taskResolved: false), .superseded)
        XCTAssertEqual(actions.taskTargetID, "t3")
    }

    // MARK: - Lane wiring

    func testMonitorsLaneExistsBetweenTaskLanes() {
        XCTAssertEqual(TaskLane.allCases.map(\.rawValue),
                       ["tasks", "monitors", "internalTasks"])
        XCTAssertEqual(TaskLane.monitors.title, "Monitors")
    }

    @MainActor
    func testMonitorsLaneShowsNoTaskCards() {
        let vm = TasksViewModel()
        vm.lane = .monitors
        XCTAssertTrue(vm.activeTasks.isEmpty,
                      "the monitors lane renders MonitorsLaneView rows, not TaskV3 cards")
        XCTAssertTrue(vm.visibleTasks.isEmpty)
    }

    // MARK: - DeepLinkTarget normalization

    func testDeepLinkTargetNormalizesEmptyUpdate() {
        XCTAssertNil(Monitors.DeepLinkTarget(taskID: "t", updateID: "  ").updateID)
        XCTAssertNil(Monitors.DeepLinkTarget(taskID: "t", updateID: nil).updateID)
        XCTAssertEqual(Monitors.DeepLinkTarget(taskID: "t", updateID: "mu_1").updateID, "mu_1")
    }
}
