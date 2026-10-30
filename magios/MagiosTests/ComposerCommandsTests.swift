import XCTest
@testable import Magician

/// The `/` command picker's pure logic (Shared/ComposerCommands.swift), against
/// docs/archive/plans/2026-10-03-slash-commands-and-run-budgets.md. The Android port's
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
                       ["/budget", "/budget default", "/engine", "/run-engine", "/voice-mode"])
        XCTAssertEqual(ComposerCommands.matches("bud").map(\.label), ["/budget", "/budget default"])
        XCTAssertEqual(ComposerCommands.matches("engine").map(\.label), ["/engine", "/run-engine"])
        XCTAssertEqual(ComposerCommands.matches("run").map(\.label), ["/run-engine"])
        XCTAssertEqual(ComposerCommands.matches("voice").map(\.label), ["/voice-mode"])
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
        XCTAssertEqual(ComposerCommands.exactCommand("/voice-mode"), .voiceMode)
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

    // MARK: /engine, /run-engine, /voice-mode (docs/plans/2026-10-30-slash-engine-model-voice-design.md)

    private let sol = ComposerModelOption(id: "sol", label: "GPT-6.1 Sol", detail: "OpenAI",
                                          thinkingLevels: ["auto", "high", "low"], defaultThinking: "auto",
                                          controlLevels: ["low", "high"])
    private let sonnet = ComposerModelOption(id: "sonnet", label: "Claude Sonnet", detail: "Anthropic",
                                             thinkingLevels: ["auto"])
    private let retired = ComposerModelOption(id: "old", label: "Old model", detail: "OpenAI",
                                              thinkingLevels: ["auto"], available: false)

    /// Chat on Magician · Sol · low here, Magician everywhere; runs pinned to the coding agent · Sonnet.
    private var modelContext: ComposerCommandContext {
        var ctx = ComposerCommandContext(
            engines: [(name: "magician", models: ["default"]), (name: "pi", models: ["default"]),
                      (name: "codex", models: ["default"]), (name: "claude_code", models: ["default", "opus"])],
            engineCredentials: ["codex": "login"],
            deviceEngine: "magician", deviceModel: "default",
            runEngine: "pi", runModel: "default")
        ctx.catalogModels = ["magician": [sol, sonnet, retired], "pi": [sonnet, sol]]
        ctx.deviceModelSelection = ComposerModelSelection(modelId: "sol", thinking: "low")
        ctx.serverChatEngine = "magician"
        ctx.runModelSelection = ComposerModelSelection(modelId: "sonnet", thinking: "auto")
        ctx.loopModels = [sol, sonnet]
        ctx.loopSelection = ComposerModelSelection(modelId: "sol", thinking: "high")
        ctx.voiceMode = .dictation
        ctx.voiceModesAvailable = [.dictation, .handsFree]
        return ctx
    }

    func testEngineLevelOneShowsTheChoiceAndOneRowPerEngine() {
        var state = ComposerCommandState()
        _ = ComposerCommands.open(.engine, state: &state)
        let rows = ComposerCommands.rows(text: "/engine ", state: state, context: modelContext)
        XCTAssertEqual(rows.map(\.label), [
            "Back to commands",
            "Now: Magician · GPT-6.1 Sol · Low (this device) · everywhere: Magician",
            "This device", "Magician", "Coding agent", "Codex · your login", "Claude Code",
        ])
        XCTAssertEqual(rows[1].action, ComposerCommandAction.none)
        XCTAssertEqual(rows.filter(\.current).map(\.label), ["Magician"])
        XCTAssertEqual(rows[3].detail, "GPT-6.1 Sol · Low")
        XCTAssertEqual(rows.dropFirst(3).map(\.action), [
            .openModels(engine: "magician"),
            .openModels(engine: "pi"),
            // An engine whose only model is `default` is set right here.
            .setChatEngine(engine: "codex", model: "default", selection: nil, everywhere: false),
            .openModels(engine: "claude_code"),
        ])
    }

    func testEngineEverywhereListsEnginesOnly() {
        var state = ComposerCommandState()
        _ = ComposerCommands.open(.engine, state: &state)
        state.engineEverywhere = true
        let rows = ComposerCommands.rows(text: "/engine ", state: state, context: modelContext)
        XCTAssertEqual(rows[1].label, "Now: Magician · GPT-6.1 Sol · Low (this device) · everywhere: Magician")
        XCTAssertEqual(rows[1].detail, "The model stays a choice of this device")
        XCTAssertEqual(rows[2].label, "Default everywhere")
        XCTAssertEqual(rows.dropFirst(3).map(\.action), [
            .setChatEngine(engine: "magician", model: "default", selection: nil, everywhere: true),
            .setChatEngine(engine: "pi", model: "default", selection: nil, everywhere: true),
            .setChatEngine(engine: "codex", model: "default", selection: nil, everywhere: true),
            .setChatEngine(engine: "claude_code", model: "default", selection: nil, everywhere: true),
        ])
    }

    func testEngineModelsListTheCatalogAndKeepTheThinkingLevel() {
        var state = ComposerCommandState()
        _ = ComposerCommands.open(.engine, state: &state)
        XCTAssertEqual(ComposerCommands.openModels(engine: "magician", state: &state), "/engine magician ")
        XCTAssertEqual(state.stack, [.command(.engine), .engineModels("magician")])
        let rows = ComposerCommands.rows(text: "/engine magician ", state: state, context: modelContext)
        XCTAssertEqual(rows.map(\.label), ["Back to engines", "GPT-6.1 Sol", "Claude Sonnet", "Old model"])
        XCTAssertEqual(rows.filter(\.current).map(\.label), ["GPT-6.1 Sol"])
        XCTAssertEqual(rows.map(\.action).dropFirst(), [
            // Sol has a thinking choice: it opens its levels.
            .openThinking(engine: "magician", modelId: "sol"),
            // Sonnet has none: picked at once.
            .setChatEngine(engine: "magician", model: "default",
                           selection: ComposerModelSelection(modelId: "sonnet", thinking: "auto"), everywhere: false),
            ComposerCommandAction.none,
        ])
        XCTAssertEqual(rows[1].detail, "OpenAI · Low")
        XCTAssertEqual(rows.last?.detail, "Not available here")
        // Typing filters the models; Back stays.
        XCTAssertEqual(ComposerCommands.rows(text: "/engine magician son", state: state, context: modelContext).map(\.label),
                       ["Back to engines", "Claude Sonnet"])
        XCTAssertEqual(ComposerCommands.back(state: &state), "/engine ")
        XCTAssertEqual(state.stack, [.command(.engine)])
    }

    func testEngineThinkingLevelsPickTheLevel() {
        var state = ComposerCommandState()
        _ = ComposerCommands.open(.engine, state: &state)
        _ = ComposerCommands.openModels(engine: "magician", state: &state)
        XCTAssertEqual(ComposerCommands.openThinking(engine: "magician", modelId: "sol", state: &state),
                       "/engine magician sol ")
        XCTAssertEqual(state.stack, [.command(.engine), .engineModels("magician"),
                                     .thinking(engine: "magician", modelId: "sol", run: false)])
        let rows = ComposerCommands.rows(text: "/engine magician sol ", state: state, context: modelContext)
        XCTAssertEqual(rows.map(\.label), ["Back to models", "Auto", "Low", "High"])
        // Sol is the model in use: its level is marked.
        XCTAssertEqual(rows.filter(\.current).map(\.label), ["Low"])
        XCTAssertEqual(rows.last?.action, .setChatEngine(engine: "magician", model: "default",
                                                         selection: ComposerModelSelection(modelId: "sol", thinking: "high"),
                                                         everywhere: false))
        XCTAssertTrue(ComposerCommands.holdsSend("/engine magician sol hi", state: state))
        XCTAssertEqual(ComposerCommands.back(state: &state), "/engine magician ")
        XCTAssertEqual(state.stack, [.command(.engine), .engineModels("magician")])
        // Another engine's Sol is not in use: its default (Auto) is marked.
        _ = ComposerCommands.back(state: &state)
        _ = ComposerCommands.openModels(engine: "pi", state: &state)
        _ = ComposerCommands.openThinking(engine: "pi", modelId: "sol", state: &state)
        XCTAssertEqual(ComposerCommands.rows(text: "/engine pi sol ", state: state, context: modelContext)
            .filter(\.current).map(\.label), ["Auto"])
    }

    func testEngineModelsOfAnotherEngineMarkNothing() {
        var state = ComposerCommandState()
        _ = ComposerCommands.open(.engine, state: &state)
        _ = ComposerCommands.openModels(engine: "pi", state: &state)
        let rows = ComposerCommands.rows(text: "/engine pi ", state: state, context: modelContext)
        XCTAssertEqual(rows.dropFirst().map(\.label), ["Claude Sonnet", "GPT-6.1 Sol"])
        XCTAssertTrue(rows.filter(\.current).isEmpty)
        XCTAssertEqual(rows.last?.action, .openThinking(engine: "pi", modelId: "sol"))
        XCTAssertEqual(rows[1].action, .setChatEngine(engine: "pi", model: "default",
                                                      selection: ComposerModelSelection(modelId: "sonnet", thinking: "auto"),
                                                      everywhere: false))
    }

    func testEngineModelsLoadingAndErrorRows() {
        var state = ComposerCommandState()
        _ = ComposerCommands.open(.engine, state: &state)
        _ = ComposerCommands.openModels(engine: "pi", state: &state)
        var ctx = modelContext
        ctx.catalogModels["pi"] = nil
        var rows = ComposerCommands.rows(text: "/engine pi ", state: state, context: ctx)
        XCTAssertEqual(rows.map(\.label), ["Back to engines", "Loading models…"])
        XCTAssertEqual(rows.last?.action, ComposerCommandAction.none)
        ctx.catalogErrors["pi"] = "Request failed (HTTP 500)"
        rows = ComposerCommands.rows(text: "/engine pi ", state: state, context: ctx)
        XCTAssertEqual(rows.map(\.label), ["Back to engines", "Models unavailable: Request failed (HTTP 500)"])
        XCTAssertEqual(rows.last?.action, ComposerCommandAction.none)
    }

    func testRosterModelsAtLevelTwoSetTheHarnessModel() {
        var state = ComposerCommandState()
        _ = ComposerCommands.open(.engine, state: &state)
        _ = ComposerCommands.openModels(engine: "claude_code", state: &state)
        let rows = ComposerCommands.rows(text: "/engine claude_code ", state: state, context: modelContext)
        XCTAssertEqual(rows.map(\.label), ["Back to engines", "CLI default", "opus"])
        XCTAssertEqual(rows.last?.action, .setChatEngine(engine: "claude_code", model: "opus", selection: nil, everywhere: false))
    }

    func testRunEngineLevelOneShowsTheChoiceAndSameAsChat() {
        var state = ComposerCommandState()
        _ = ComposerCommands.open(.runEngine, state: &state)
        var rows = ComposerCommands.rows(text: "/run-engine ", state: state, context: modelContext)
        XCTAssertEqual(rows.map(\.label), [
            "Back to commands", "Now: Coding agent · Claude Sonnet",
            "Same as Chat", "Magician", "Coding agent", "Codex · your login", "Claude Code",
        ])
        XCTAssertEqual(rows.filter(\.current).map(\.label), ["Coding agent"])
        XCTAssertEqual(rows.dropFirst(2).map(\.action), [
            .setRunEngine(engine: "chat", model: "default", selection: nil),
            .openModels(engine: "magician"),
            .openModels(engine: "pi"),
            .setRunEngine(engine: "codex", model: "default", selection: nil),
            .openModels(engine: "claude_code"),
        ])

        var following = modelContext
        following.runFollowsChat = true
        following.runEngine = "magician"
        rows = ComposerCommands.rows(text: "/run-engine ", state: state, context: following)
        XCTAssertEqual(rows[1].label, "Now: Same as Chat (Magician)")
        XCTAssertEqual(rows.filter(\.current).map(\.label), ["Same as Chat"])
    }

    func testRunEngineMagicianPicksTheLoopModel() {
        var state = ComposerCommandState()
        _ = ComposerCommands.open(.runEngine, state: &state)
        XCTAssertEqual(ComposerCommands.openModels(engine: "magician", state: &state), "/run-engine magician ")
        XCTAssertEqual(state.stack, [.command(.runEngine), .runModels("magician")])
        let rows = ComposerCommands.rows(text: "/run-engine magician ", state: state, context: modelContext)
        XCTAssertEqual(rows.map(\.label), ["Back to engines", "GPT-6.1 Sol", "Claude Sonnet"])
        // The run engine is Pi, so no loop model is the current run choice.
        XCTAssertTrue(rows.filter(\.current).isEmpty)
        XCTAssertEqual(rows.dropFirst().map(\.action), [
            .openThinking(engine: "magician", modelId: "sol"),
            .setLoopModel(selection: ComposerModelSelection(modelId: "sonnet", thinking: "auto")),
        ])
        var onMagician = modelContext
        onMagician.runEngine = "magician"
        XCTAssertEqual(ComposerCommands.rows(text: "/run-engine magician ", state: state, context: onMagician)
            .filter(\.current).map(\.label), ["GPT-6.1 Sol"])
        XCTAssertTrue(ComposerCommands.runResolvesToMagician(onMagician))
        XCTAssertFalse(ComposerCommands.runResolvesToMagician(modelContext))
        // "Same as Chat" resolves to Magician while the chat default is Magician.
        var following = modelContext
        following.runFollowsChat = true
        XCTAssertTrue(ComposerCommands.runResolvesToMagician(following))
        following.serverChatEngine = "codex"
        XCTAssertFalse(ComposerCommands.runResolvesToMagician(following))
        // The loop model's thinking level.
        XCTAssertEqual(ComposerCommands.openThinking(engine: "magician", modelId: "sol", state: &state),
                       "/run-engine magician sol ")
        let levels = ComposerCommands.rows(text: "/run-engine magician sol ", state: state, context: onMagician)
        XCTAssertEqual(levels.filter(\.current).map(\.label), ["High"])
        XCTAssertEqual(levels[2].action, .setLoopModel(selection: ComposerModelSelection(modelId: "sol", thinking: "low")))
        let body = ComposerCommands.loopModelBody(ComposerModelSelection(modelId: "sol", thinking: "high"))
        XCTAssertEqual(body["selection"] as? [String: String], ["model_id": "sol", "thinking": "high"])
    }

    func testRunEnginePiCarriesTheModelSelection() {
        var state = ComposerCommandState()
        _ = ComposerCommands.open(.runEngine, state: &state)
        _ = ComposerCommands.openModels(engine: "pi", state: &state)
        let rows = ComposerCommands.rows(text: "/run-engine pi ", state: state, context: modelContext)
        XCTAssertEqual(rows.map(\.label), ["Back to engines", "Default", "Claude Sonnet", "GPT-6.1 Sol"])
        XCTAssertEqual(rows.filter(\.current).map(\.label), ["Claude Sonnet"])
        XCTAssertEqual(rows[1].detail, "The coding agent's own default model")
        XCTAssertEqual(rows[1].action, .setRunEngine(engine: "pi", model: "default", selection: nil))
        XCTAssertEqual(rows[2].action, .setRunEngine(engine: "pi", model: "default",
                                                     selection: ComposerModelSelection(modelId: "sonnet", thinking: "auto")))
        XCTAssertEqual(rows.last?.action, .openThinking(engine: "pi", modelId: "sol"))
        // "Default" is current while no run model is set, and clears it with an explicit null.
        var piDefault = modelContext
        piDefault.runModelSelection = nil
        XCTAssertEqual(ComposerCommands.rows(text: "/run-engine pi ", state: state, context: piDefault)
            .filter(\.current).map(\.label), ["Default"])
        let cleared = ComposerCommands.runEngineBody(engine: "pi", model: "default", selection: nil)
        XCTAssertTrue(cleared["model_selection"] is NSNull)
        XCTAssertNil(ComposerCommands.runEngineBody(engine: "codex", model: "default", selection: nil)["model_selection"])
        let selection = ComposerModelSelection(modelId: "sol", thinking: "auto")
        let body = ComposerCommands.runEngineBody(engine: "pi", model: "default", selection: selection)
        XCTAssertEqual(body["harness_engine"] as? String, "pi")
        XCTAssertEqual(body["harness_model"] as? String, "default")
        XCTAssertEqual(body["model_selection"] as? [String: String], ["model_id": "sol", "thinking": "auto"])
    }

    func testRunEngineRosterModelsSetTheHarnessModel() {
        var state = ComposerCommandState()
        _ = ComposerCommands.open(.runEngine, state: &state)
        _ = ComposerCommands.openModels(engine: "claude_code", state: &state)
        let rows = ComposerCommands.rows(text: "/run-engine claude_code ", state: state, context: modelContext)
        XCTAssertEqual(rows.last?.action, .setRunEngine(engine: "claude_code", model: "opus", selection: nil))
        let body = ComposerCommands.engineBody(engine: "claude_code", model: "opus")
        XCTAssertEqual(body["harness_engine"] as? String, "claude_code")
        XCTAssertEqual(body["harness_model"] as? String, "opus")
        XCTAssertNil(body["model_selection"])
    }

    func testVoiceModeMarksTheCurrentModeAndDisablesUnavailableOnes() {
        var state = ComposerCommandState()
        _ = ComposerCommands.open(.voiceMode, state: &state)
        let rows = ComposerCommands.rows(text: "/voice-mode ", state: state, context: modelContext)
        XCTAssertEqual(rows.map(\.label), ["Back to commands", "Live", "Dictate", "Hands-free"])
        XCTAssertEqual(rows.filter(\.current).map(\.label), ["Dictate"])
        XCTAssertEqual(rows[1].detail, "Not available here")
        XCTAssertEqual(rows.dropFirst().map(\.action),
                       [ComposerCommandAction.none, .setVoiceMode(.dictation), .setVoiceMode(.handsFree)])
        XCTAssertEqual(ComposerVoiceMode.allCases.map(\.rawValue), ["realtime", "dictation", "hands_free"])
    }

    func testKeepThinkingPrefersTheCurrentLevel() {
        let medium = ComposerModelOption(id: "m", label: "M", thinkingLevels: ["high", "medium"], defaultThinking: "medium")
        XCTAssertEqual(ComposerCommands.keepThinking(sol, current: ComposerModelSelection(modelId: "x", thinking: "high")), "high")
        XCTAssertEqual(ComposerCommands.keepThinking(sonnet, current: ComposerModelSelection(modelId: "x", thinking: "high")), "auto")
        XCTAssertEqual(ComposerCommands.keepThinking(medium, current: nil), "medium")
    }

    func testOpenLevelHoldsTheSend() {
        var state = ComposerCommandState()
        XCTAssertTrue(ComposerCommands.holdsSend("/engine", state: state))
        XCTAssertFalse(ComposerCommands.holdsSend("hello", state: state))
        _ = ComposerCommands.open(.engine, state: &state)
        _ = ComposerCommands.openModels(engine: "pi", state: &state)
        XCTAssertTrue(ComposerCommands.holdsSend("/engine pi ", state: state))
        XCTAssertTrue(ComposerCommands.holdsSend("/engine pi son", state: state))
    }

    func testConfirmationsNameTheEngineAndModel() {
        let ctx = modelContext
        XCTAssertEqual(ComposerCommands.chatEngineConfirmation(
            engine: "pi", model: "default", selection: ComposerModelSelection(modelId: "sonnet", thinking: "auto"),
            everywhere: false, context: ctx), "Chat engine: Coding agent · Claude Sonnet")
        XCTAssertEqual(ComposerCommands.chatEngineConfirmation(
            engine: "pi", model: "default", selection: ComposerModelSelection(modelId: "sol", thinking: "low"),
            everywhere: false, context: ctx), "Chat engine: Coding agent · GPT-6.1 Sol · Low")
        XCTAssertEqual(ComposerCommands.chatEngineConfirmation(
            engine: "codex", model: "default", selection: nil, everywhere: true, context: ctx),
                       "Chat engine everywhere: Codex")
        XCTAssertEqual(ComposerCommands.loopModelConfirmation(
            selection: ComposerModelSelection(modelId: "sol", thinking: "high"), context: ctx),
                       "Agentic flows: Magician · GPT-6.1 Sol · High")
        XCTAssertEqual(ComposerCommands.runEngineConfirmation(engine: "pi", model: "default", selection: nil, context: ctx),
                       "Agentic flows: Coding agent · Default")
        XCTAssertEqual(ComposerCommands.runEngineConfirmation(engine: "chat", model: "default", selection: nil, context: ctx),
                       "Agentic flows: Same as Chat")
        XCTAssertEqual(ComposerCommands.runEngineConfirmation(engine: "claude_code", model: "opus", selection: nil, context: ctx),
                       "Agentic flows: Claude Code · opus")
        XCTAssertEqual(ComposerCommands.voiceModeConfirmation(.handsFree), "Voice mode: Hands-free")
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
