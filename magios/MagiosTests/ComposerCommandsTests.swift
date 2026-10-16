import XCTest
@testable import Magician

/// The `/` command picker's pure logic (Shared/ComposerCommands.swift), against
/// docs/plans/2026-10-03-slash-commands-and-run-budgets.md. The Android port's
/// `ComposerCommandsTest.kt` asserts the same cases.
final class ComposerCommandsTests: XCTestCase {
    private let context = ComposerCommandContext(
        engines: [(name: "magician", models: ["default"]),
                  (name: "claude_code", models: ["default", "opus"])],
        deviceEngine: "claude_code", deviceModel: "opus",
        runEngine: "magician", runModel: "default",
        tasks: [
            ComposerCommandTask(taskId: "t-old", title: "Old research", status: "running"),
            ComposerCommandTask(taskId: "t-done", title: "Done", status: "completed"),
            ComposerCommandTask(taskId: "t-plan", title: "Planned", status: "planning"),
            ComposerCommandTask(taskId: "t-new", title: "New research", status: "paused"),
            // A later card for the same task replaces the earlier one.
            ComposerCommandTask(taskId: "t-done", title: "Done", status: "completed"),
        ])

    func testTriggerNeedsLeadingSlashOnOneLine() {
        XCTAssertEqual(ComposerCommands.trigger("/"), "")
        XCTAssertEqual(ComposerCommands.trigger("/bud"), "bud")
        XCTAssertNil(ComposerCommands.trigger(" /budget"))
        XCTAssertNil(ComposerCommands.trigger("hello /budget"))
        XCTAssertNil(ComposerCommands.trigger("/budget\nmore"))
        XCTAssertNil(ComposerCommands.trigger(""))
    }

    func testCommandListFiltersAsYouType() {
        XCTAssertEqual(ComposerCommands.matches("").map(\.label),
                       ["/budget", "/budget default", "/engine", "/run-engine"])
        XCTAssertEqual(ComposerCommands.matches("bud").map(\.label), ["/budget", "/budget default"])
        XCTAssertEqual(ComposerCommands.matches("engine").map(\.label), ["/engine", "/run-engine"])
        XCTAssertEqual(ComposerCommands.matches("run").map(\.label), ["/run-engine"])
        XCTAssertTrue(ComposerCommands.matches("zzz").isEmpty)
    }

    func testRowsWithoutLevelAreTheCommandList() {
        let state = ComposerCommandState()
        XCTAssertEqual(ComposerCommands.rows(text: "/def", state: state, context: context).map(\.action),
                       [.open(.budgetDefault)])
        XCTAssertTrue(ComposerCommands.rows(text: "plain text", state: state, context: context).isEmpty)
    }

    func testExactCommandIsInterceptedOnSend() {
        XCTAssertEqual(ComposerCommands.exactCommand("/budget"), .budget)
        XCTAssertEqual(ComposerCommands.exactCommand(" /budget  default "), .budgetDefault)
        XCTAssertEqual(ComposerCommands.exactCommand("/RUN-ENGINE"), .runEngine)
        XCTAssertNil(ComposerCommands.exactCommand("/budget please"))
        XCTAssertNil(ComposerCommands.exactCommand("budget"))
    }

    func testOpenBackAndSyncWalkTheTree() {
        var state = ComposerCommandState()
        XCTAssertEqual(ComposerCommands.open(.budget, state: &state), "/budget ")
        XCTAssertEqual(state.stack, [.command(.budget)])
        XCTAssertEqual(ComposerCommands.pickTask(taskId: "t-new", title: "New research", state: &state), "/budget ")
        XCTAssertEqual(state.stack.count, 2)
        let grantRows = ComposerCommands.rows(text: "/budget ", state: state, context: context)
        XCTAssertEqual(grantRows.first?.label, "Back to tasks")
        XCTAssertEqual(grantRows.dropFirst().map(\.label),
                       ["+$25", "+$100", "+1M tokens", "+50 iterations", "Unlimited"])
        XCTAssertEqual(ComposerCommands.back(state: &state), "/budget ")
        XCTAssertEqual(state.stack, [.command(.budget)])
        XCTAssertEqual(ComposerCommands.back(state: &state), "/")
        XCTAssertTrue(state.stack.isEmpty)

        _ = ComposerCommands.open(.engine, state: &state)
        ComposerCommands.sync(text: "/engine op", state: &state)
        XCTAssertEqual(state.stack, [.command(.engine)])
        ComposerCommands.sync(text: "/engin", state: &state)
        XCTAssertTrue(state.stack.isEmpty, "deleting into the prefix closes the level")
    }

    func testBudgetListsRunningAndPausedTasksNewestFirst() {
        var state = ComposerCommandState()
        _ = ComposerCommands.open(.budget, state: &state)
        let rows = ComposerCommands.rows(text: "/budget ", state: state, context: context)
        XCTAssertEqual(rows.first?.action, .back)
        XCTAssertEqual(rows.dropFirst().map(\.action), [
            .pickBudgetTask(taskId: "t-new", title: "New research"),
            .pickBudgetTask(taskId: "t-old", title: "Old research"),
        ])
        // Typing after the prefix filters the options; Back stays.
        let filtered = ComposerCommands.rows(text: "/budget old", state: state, context: context)
        XCTAssertEqual(filtered.map(\.id), ["back", "task:t-old"])
    }

    func testBudgetWithNoTasksSaysSo() {
        var state = ComposerCommandState()
        _ = ComposerCommands.open(.budget, state: &state)
        let rows = ComposerCommands.rows(text: "/budget ", state: state, context: ComposerCommandContext())
        XCTAssertEqual(rows.map(\.action), [.back, ComposerCommandAction.none])
    }

    func testGrantBodiesMatchTheContract() {
        XCTAssertEqual(BudgetGrant.usd25.body["add_cost_usd"] as? Int, 25)
        XCTAssertEqual(BudgetGrant.usd100.body["add_cost_usd"] as? Int, 100)
        XCTAssertEqual(BudgetGrant.tokens1M.body["add_tokens"] as? Int, 1_000_000)
        XCTAssertEqual(BudgetGrant.iterations50.body["add_iterations"] as? Int, 50)
        XCTAssertEqual(BudgetGrant.unlimited.body["unlimited"] as? Bool, true)
        XCTAssertEqual(ComposerCommands.grantConfirmation(.usd25, title: "Research", resumed: true),
                       "Added $25 to Research — resumed")
        XCTAssertEqual(ComposerCommands.grantConfirmation(.unlimited, title: "Research", resumed: false),
                       "Removed the budget cap on Research")
    }

    func testEngineRowsMarkTheDeviceChoiceAndCarryTheScope() {
        var state = ComposerCommandState()
        _ = ComposerCommands.open(.engine, state: &state)
        var rows = ComposerCommands.rows(text: "/engine ", state: state, context: context)
        XCTAssertEqual(rows.map(\.label), ["Back to commands", "This device", "magician", "claude_code", "claude_code · opus"])
        XCTAssertEqual(rows.filter(\.current).map(\.label), ["claude_code · opus"])
        XCTAssertEqual(rows.last?.action, .setChatEngine(engine: "claude_code", model: "opus", everywhere: false))
        state.engineEverywhere = true
        rows = ComposerCommands.rows(text: "/engine ", state: state, context: context)
        XCTAssertEqual(rows[1].label, "Default everywhere")
        XCTAssertEqual(rows.last?.action, .setChatEngine(engine: "claude_code", model: "opus", everywhere: true))
    }

    func testRunEngineRowsMarkTheRunEngine() {
        var state = ComposerCommandState()
        _ = ComposerCommands.open(.runEngine, state: &state)
        let rows = ComposerCommands.rows(text: "/run-engine ", state: state, context: context)
        XCTAssertEqual(rows.filter(\.current).map(\.label), ["magician"])
        XCTAssertEqual(rows.last?.action, .setRunEngine(engine: "claude_code", model: "opus"))
        let body = ComposerCommands.engineBody(engine: "claude_code", model: "opus")
        XCTAssertEqual(body["harness_engine"] as? String, "claude_code")
        XCTAssertEqual(body["harness_model"] as? String, "opus")
    }

    func testBudgetDefaultsDecodeMarkAndEncode() throws {
        let data = Data(#"{"native_max_cost_usd":15,"harness_max_cost_usd":"unlimited"}"#.utf8)
        let defaults = try XCTUnwrap(BudgetDefaults.decode(data))
        XCTAssertEqual(defaults, BudgetDefaults(native: .usd(15), harness: .unlimited))

        var state = ComposerCommandState()
        _ = ComposerCommands.open(.budgetDefault, state: &state)
        var ctx = context
        ctx.budgetDefaults = defaults
        let rows = ComposerCommands.rows(text: "/budget default ", state: state, context: ctx)
        XCTAssertEqual(rows.dropFirst().map(\.label),
                       ["$50", "$100", "$250", "Unlimited", "$5", "$15", "$50", "Unlimited"])
        XCTAssertEqual(rows.filter(\.current).map(\.id), ["default:harness:Unlimited", "default:native:$15"])

        let body = ComposerCommands.defaultsBody(kind: .harness, cap: .usd(250), current: defaults)
        XCTAssertEqual(body["harness_max_cost_usd"] as? Double, 250)
        XCTAssertEqual(body["native_max_cost_usd"] as? Double, 15)
        let unlimited = ComposerCommands.defaultsBody(kind: .native, cap: .unlimited, current: nil)
        XCTAssertEqual(unlimited["native_max_cost_usd"] as? String, "unlimited")
        XCTAssertNil(unlimited["harness_max_cost_usd"])
    }

    func testBudgetDefaultsErrorShowsAsANoteRow() {
        var state = ComposerCommandState()
        _ = ComposerCommands.open(.budgetDefault, state: &state)
        var ctx = context
        ctx.budgetDefaultsError = "route not found"
        let rows = ComposerCommands.rows(text: "/budget default ", state: state, context: ctx)
        XCTAssertEqual(rows[1].detail, "route not found")
        XCTAssertEqual(rows[1].action, ComposerCommandAction.none)
    }

    func testServerErrorMessagePrefersTheServersWords() {
        let json = Data(#"{"error":"harness_engine_not_installed","message":"`grok` is not installed"}"#.utf8)
        XCTAssertEqual(ComposerCommands.serverErrorMessage(status: 400, body: json), "`grok` is not installed")
        XCTAssertEqual(ComposerCommands.serverErrorMessage(status: 404, body: Data(#"{"error":"task_not_found"}"#.utf8)),
                       "task_not_found")
        XCTAssertEqual(ComposerCommands.serverErrorMessage(status: 502, body: Data("bad gateway".utf8)), "bad gateway")
        XCTAssertEqual(ComposerCommands.serverErrorMessage(status: 404, body: nil),
                       "Not available on this Magician (HTTP 404)")
    }
}
