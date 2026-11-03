import Foundation

// MARK: - Composer slash commands
//
// `/` at the start of the composer opens a command list styled like the `@`
// picker: `/budget`, `/budget default`, `/engine`, `/run-engine`, `/voice-mode`.
// Choosing a command shows its options in the same picker; choosing an option
// runs it on the client (never as a chat message). `/engine` and `/run-engine`
// go engine → model, mirroring the composer's model dropdown and Settings →
// Agentic flows. Contract:
// docs/archive/plans/2026-10-03-slash-commands-and-run-budgets.md, extended by
// docs/plans/2026-10-30-slash-engine-model-voice-design.md. The Kotlin port is
// magdroid `chat/ComposerCommands.kt` and the web one `composerCommands.ts`;
// keep their rows and wording in step.
//
// Everything here is pure: trigger, command tree, option rows, filtering, the
// request bodies and the confirmation lines. The view model does the I/O.

public enum ComposerCommandId: String, CaseIterable, Equatable {
    case budget
    case budgetDefault = "budget default"
    case engine
    case runEngine = "run-engine"
    case voiceMode = "voice-mode"
}

public struct ComposerCommand: Identifiable, Equatable {
    public let id: ComposerCommandId
    /// Without the slash: `budget default`.
    public var name: String { id.rawValue }
    public var label: String { "/" + id.rawValue }
    public let detail: String
}

/// A run's USD cap: a number, or switched off.
public enum BudgetCap: Equatable {
    case usd(Double)
    case unlimited

    /// Wire form: a JSON number, or the string `"unlimited"`.
    public var wireValue: Any {
        switch self {
        case .usd(let value): return value
        case .unlimited: return "unlimited"
        }
    }

    public static func from(wire: Any?) -> BudgetCap? {
        if let text = wire as? String {
            if text.lowercased() == "unlimited" { return .unlimited }
            return Double(text).map { .usd($0) }
        }
        if let number = wire as? NSNumber { return .usd(number.doubleValue) }
        return nil
    }

    public var label: String {
        switch self {
        case .unlimited: return "Unlimited"
        case .usd(let value): return composerUsdLabel(value)
        }
    }
}

/// `GET /api/magician/v2/budgets/defaults`.
public struct BudgetDefaults: Equatable {
    public var native: BudgetCap?
    public var harness: BudgetCap?

    public init(native: BudgetCap? = nil, harness: BudgetCap? = nil) {
        self.native = native; self.harness = harness
    }

    public static func decode(_ data: Data) -> BudgetDefaults? {
        guard let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return nil }
        return BudgetDefaults(
            native: BudgetCap.from(wire: object["native_max_cost_usd"]),
            harness: BudgetCap.from(wire: object["harness_max_cost_usd"]))
    }
}

public enum BudgetRunKind: String, Equatable {
    case harness, native

    public var title: String { self == .harness ? "Harness runs" : "Native runs" }
    var wireKey: String { self == .harness ? "harness_max_cost_usd" : "native_max_cost_usd" }
}

/// One `POST /tasks/{id}/budget` grant.
public enum BudgetGrant: String, CaseIterable, Equatable {
    case usd25, usd100, tokens1M, iterations50, unlimited

    public var label: String {
        switch self {
        case .usd25: return "+$25"
        case .usd100: return "+$100"
        case .tokens1M: return "+1M tokens"
        case .iterations50: return "+50 iterations"
        case .unlimited: return "Unlimited"
        }
    }

    public var detail: String {
        switch self {
        case .usd25, .usd100: return "Raise the run's cost cap"
        case .tokens1M: return "Raise the run's token allowance"
        case .iterations50: return "Raise the run's iteration allowance"
        case .unlimited: return "Remove the cost cap and token meter for this run"
        }
    }

    public var body: [String: Any] {
        switch self {
        case .usd25: return ["add_cost_usd": 25]
        case .usd100: return ["add_cost_usd": 100]
        case .tokens1M: return ["add_tokens": 1_000_000]
        case .iterations50: return ["add_iterations": 50]
        case .unlimited: return ["unlimited": true]
        }
    }
}

/// A catalog model's `{model_id, thinking}` (the `model_selection` the
/// composer sends, and what `PUT /llm/routing/{operation}` takes).
public struct ComposerModelSelection: Equatable {
    public let modelId: String
    public let thinking: String
    public init(modelId: String, thinking: String) { self.modelId = modelId; self.thinking = thinking }
    public var wireValue: [String: String] { ["model_id": modelId, "thinking": thinking] }
}

/// One catalog model (`GET /chat/models?engine=`, or `GET /llm/routing`'s
/// `models`) as the picker shows it.
public struct ComposerModelOption: Equatable {
    public let id: String
    public let label: String
    /// The provider, shown under the label.
    public let detail: String?
    /// The thinking levels it can route (its `choices` keys).
    public let thinkingLevels: [String]
    public let defaultThinking: String?
    /// False when the model's source no longer offers it.
    public let available: Bool
    /// The thinking control's `mode` (`none` = no control) and its `levels`,
    /// in the order `ModelThinkingControl` shows them.
    public let thinkingMode: String
    public let controlLevels: [String]

    public init(id: String, label: String, detail: String? = nil, thinkingLevels: [String],
                defaultThinking: String? = nil, available: Bool = true,
                thinkingMode: String = "effort", controlLevels: [String]? = nil) {
        self.id = id; self.label = label; self.detail = detail; self.thinkingLevels = thinkingLevels
        self.defaultThinking = defaultThinking; self.available = available
        self.thinkingMode = thinkingMode
        self.controlLevels = controlLevels ?? thinkingLevels.filter { $0 != "auto" }
    }

    /// The levels the `/` picker offers, as `ModelThinkingControl` orders
    /// them (Auto, then the control's levels), limited to those it routes.
    /// Empty without a thinking control.
    public var thinkingChoices: [String] {
        guard thinkingMode != "none" else { return [] }
        var seen = Set<String>()
        return (["auto"] + controlLevels).filter { thinkingLevels.contains($0) && seen.insert($0).inserted }
    }
}

/// What the composer's voice button starts (`AudioSettings.composerVoiceMode`
/// wire values), in picker order.
public enum ComposerVoiceMode: String, CaseIterable, Equatable {
    case realtime
    case dictation
    case handsFree = "hands_free"

    public var label: String {
        switch self {
        case .realtime: return "Live"
        case .dictation: return "Dictate"
        case .handsFree: return "Hands-free"
        }
    }
}

/// What picking a row does.
public enum ComposerCommandAction: Equatable {
    /// Pop one level.
    case back
    /// Open a command's options.
    case open(ComposerCommandId)
    /// `/engine` or `/run-engine`: open an engine's models.
    case openModels(engine: String)
    /// … then a catalog model's thinking levels.
    case openThinking(engine: String, modelId: String)
    /// `/engine`: flip between "this device" and "default everywhere".
    case toggleEngineScope
    /// `/engine`: the per-device chat engine and its model (a catalog
    /// `selection` for magician/pi, else `model`); `everywhere` also
    /// `PUT /plane/chat-engine`.
    case setChatEngine(engine: String, model: String, selection: ComposerModelSelection?, everywhere: Bool)
    /// `/run-engine`: `PUT /plane/engine` (`selection` for Pi, where nil is its
    /// own default; engine `chat` = Same as Chat).
    case setRunEngine(engine: String, model: String, selection: ComposerModelSelection?)
    /// `/run-engine magician`: `PUT /llm/routing/agentic_decision`, pinning magician first if needed.
    case setLoopModel(selection: ComposerModelSelection)
    /// `/voice-mode`.
    case setVoiceMode(ComposerVoiceMode)
    /// `/budget`: choose the task, then the grant.
    case pickBudgetTask(taskId: String, title: String)
    case grantBudget(taskId: String, title: String, grant: BudgetGrant)
    /// `/budget default`: `PUT /budgets/defaults`.
    case setBudgetDefault(kind: BudgetRunKind, cap: BudgetCap)
    /// An informational row (nothing to pick).
    case none
}

public struct ComposerCommandRow: Identifiable, Equatable {
    public let id: String
    /// The small uppercase chip, like the `@` picker's kind chip.
    public let badge: String
    public let label: String
    public let detail: String?
    /// The current value (rendered with a checkmark).
    public let current: Bool
    public let action: ComposerCommandAction
    private let pinnedOverride: Bool?

    public init(id: String, badge: String, label: String, detail: String? = nil,
                current: Bool = false, action: ComposerCommandAction, pinned: Bool? = nil) {
        self.id = id; self.badge = badge; self.label = label; self.detail = detail
        self.current = current; self.action = action; self.pinnedOverride = pinned
    }

    /// Always shown whatever the filter says (notes, Back, the scope row).
    var pinned: Bool {
        if let pinnedOverride { return pinnedOverride }
        switch action {
        case .back, .toggleEngineScope, .none: return true
        default: return false
        }
    }
}

/// A task the chat's transcript shows (from its task cards, oldest first).
public struct ComposerCommandTask: Equatable {
    public let taskId: String
    public let title: String
    public let status: String
    public init(taskId: String, title: String, status: String) {
        self.taskId = taskId; self.title = title; self.status = status
    }
}

/// Everything the option rows read.
public struct ComposerCommandContext: Equatable {
    /// Installed engines and their models, roster order.
    public var engines: [(name: String, models: [String])]
    /// Engine name → its `credentials` (`login`, `magician_key`, `none`).
    public var engineCredentials: [String: String]
    public var deviceEngine: String
    public var deviceModel: String
    public var runEngine: String?
    public var runModel: String?
    /// This chat's tasks, transcript order (oldest first).
    public var tasks: [ComposerCommandTask]
    public var budgetDefaults: BudgetDefaults?
    /// Why the defaults could not be read, when they could not.
    public var budgetDefaultsError: String?
    /// `GET /chat/models?engine=` per catalog engine (magician, pi); nil = loading.
    public var catalogModels: [String: [ComposerModelOption]] = [:]
    public var catalogErrors: [String: String] = [:]
    /// The composer's model on a catalog engine.
    public var deviceModelSelection: ComposerModelSelection?
    /// The server's chat engine (`chat_current` / `chat_model`): clients without a pick of their own.
    public var serverChatEngine: String?
    public var serverChatModel: String?
    /// Agentic flows follow the chat engine (`follows_chat`); `runEngine` is what that resolves to.
    public var runFollowsChat = false
    /// Pi's run model (`run_model_selection`).
    public var runModelSelection: ComposerModelSelection?
    /// Magician's loop model: `GET /llm/routing`, operation `agentic_decision`; nil = loading.
    public var loopModels: [ComposerModelOption]?
    public var loopSelection: ComposerModelSelection?
    public var loopModelsError: String?
    /// `/voice-mode`: the composer's mode and the modes it can start here.
    public var voiceMode: ComposerVoiceMode?
    public var voiceModesAvailable: Set<ComposerVoiceMode> = []

    public init(engines: [(name: String, models: [String])] = [], engineCredentials: [String: String] = [:],
                deviceEngine: String = "magician",
                deviceModel: String = "default", runEngine: String? = nil, runModel: String? = nil,
                tasks: [ComposerCommandTask] = [], budgetDefaults: BudgetDefaults? = nil,
                budgetDefaultsError: String? = nil) {
        self.engines = engines; self.engineCredentials = engineCredentials
        self.deviceEngine = deviceEngine; self.deviceModel = deviceModel
        self.runEngine = runEngine; self.runModel = runModel; self.tasks = tasks
        self.budgetDefaults = budgetDefaults; self.budgetDefaultsError = budgetDefaultsError
    }

    public static func == (lhs: ComposerCommandContext, rhs: ComposerCommandContext) -> Bool {
        lhs.engines.map { "\($0.name)=\($0.models.joined(separator: ","))" }
            == rhs.engines.map { "\($0.name)=\($0.models.joined(separator: ","))" }
            && lhs.engineCredentials == rhs.engineCredentials
            && lhs.deviceEngine == rhs.deviceEngine && lhs.deviceModel == rhs.deviceModel
            && lhs.runEngine == rhs.runEngine && lhs.runModel == rhs.runModel
            && lhs.tasks == rhs.tasks && lhs.budgetDefaults == rhs.budgetDefaults
            && lhs.budgetDefaultsError == rhs.budgetDefaultsError
            && lhs.catalogModels == rhs.catalogModels && lhs.catalogErrors == rhs.catalogErrors
            && lhs.deviceModelSelection == rhs.deviceModelSelection
            && lhs.serverChatEngine == rhs.serverChatEngine && lhs.serverChatModel == rhs.serverChatModel
            && lhs.runFollowsChat == rhs.runFollowsChat && lhs.runModelSelection == rhs.runModelSelection
            && lhs.loopModels == rhs.loopModels && lhs.loopSelection == rhs.loopSelection
            && lhs.loopModelsError == rhs.loopModelsError
            && lhs.voiceMode == rhs.voiceMode && lhs.voiceModesAvailable == rhs.voiceModesAvailable
    }
}

/// Where the picker is. Empty `stack` = the command list.
public enum ComposerCommandLevel: Equatable {
    case command(ComposerCommandId)
    case budgetTask(taskId: String, title: String)
    /// `/engine <engine>`: that engine's models.
    case engineModels(String)
    /// `/run-engine <engine>`: that engine's models for agentic flows.
    case runModels(String)
    /// `… <engine> <model>`: a catalog model's thinking levels (`run` under `/run-engine`).
    case thinking(engine: String, modelId: String, run: Bool)
}

public struct ComposerCommandState: Equatable {
    public var stack: [ComposerCommandLevel] = []
    /// `/engine` applies everywhere (also `PUT /plane/chat-engine`).
    public var engineEverywhere = false
    /// The composer text when the current level opened (`/budget `); typing
    /// after it filters the options, deleting into it closes the level.
    public var prefix: String = ""
    public init() {}
}

public enum ComposerCommands {
    public static let all: [ComposerCommand] = [
        ComposerCommand(id: .budget, detail: "Give a running or paused task more budget"),
        ComposerCommand(id: .budgetDefault, detail: "Default cost caps for new runs"),
        ComposerCommand(id: .engine, detail: "Who answers chat on this device"),
        ComposerCommand(id: .runEngine, detail: "Who runs tasks"),
        ComposerCommand(id: .voiceMode, detail: "What the voice button starts"),
    ]

    /// Engines whose models come from the model catalog (a `model_selection`).
    static let catalogEngines: Set<String> = ["magician", "pi"]

    static let terminalStatuses: Set<String> = [
        "complete", "completed", "done", "failed", "error", "cancelled", "canceled",
    ]
    /// Statuses of a task with no run yet: nothing to budget.
    static let notStartedStatuses: Set<String> = [
        "created", "pending", "draft", "planning", "plan_ready", "ready", "rejected",
    ]

    /// The query after a leading `/`, or nil. Only a composer that starts with
    /// `/` (no leading space) and holds a single line opens the list.
    public static func trigger(_ text: String) -> String? {
        guard text.hasPrefix("/"), !text.contains("\n") else { return nil }
        return String(text.dropFirst())
    }

    /// Commands whose name contains the (trimmed) query, declared order.
    public static func matches(_ query: String) -> [ComposerCommand] {
        let needle = query.trimmingCharacters(in: .whitespaces).lowercased()
        if needle.isEmpty { return all }
        return all.filter { $0.name.contains(needle) }
    }

    /// A composer holding exactly a command (`/budget`): sending it opens the
    /// command's options instead of posting it to the model.
    public static func exactCommand(_ text: String) -> ComposerCommandId? {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard trimmed.hasPrefix("/") else { return nil }
        let name = String(trimmed.dropFirst())
            .split(whereSeparator: { $0 == " " }).joined(separator: " ")
        return ComposerCommandId(rawValue: name)
    }

    /// A top-level draft the picker still lists a command for (`/eng`,
    /// `/budget defa`): every word of it appears in the command's name, as the
    /// web picker matches. Send opens that command, the picker's first row.
    /// Anything else (`/budget please help`, `/usr/bin …`) is ordinary text.
    public static func partialCommand(_ text: String, state: ComposerCommandState) -> ComposerCommandId? {
        guard state.stack.isEmpty, let query = trigger(text.trimmingCharacters(in: .whitespaces)) else { return nil }
        let words = query.lowercased().split(separator: " ").map(String.init)
        return all.first { command in words.allSatisfy { command.name.contains($0) } }?.id
    }

    // MARK: Picker state

    /// Enter a command's options. Returns the text the composer should hold.
    public static func open(_ id: ComposerCommandId, state: inout ComposerCommandState) -> String {
        state.stack = [.command(id)]
        state.engineEverywhere = false
        state.prefix = "/\(id.rawValue) "
        return state.prefix
    }

    /// Choose a task under `/budget`.
    public static func pickTask(taskId: String, title: String, state: inout ComposerCommandState) -> String {
        state.stack.append(.budgetTask(taskId: taskId, title: title))
        return state.prefix
    }

    /// Choose an engine under `/engine` or `/run-engine`: its models.
    public static func openModels(engine: String, state: inout ComposerCommandState) -> String {
        let run = state.stack.last == .command(.runEngine)
        state.stack.append(run ? .runModels(engine) : .engineModels(engine))
        state.prefix = prefix(for: state.stack)
        return state.prefix
    }

    /// Choose a catalog model with a thinking choice: its levels.
    public static func openThinking(engine: String, modelId: String, state: inout ComposerCommandState) -> String {
        let run: Bool
        switch state.stack.last {
        case .runModels: run = true
        default: run = false
        }
        state.stack.append(.thinking(engine: engine, modelId: modelId, run: run))
        state.prefix = prefix(for: state.stack)
        return state.prefix
    }

    /// The composer text a stack shows: `/engine pi <model> `. A chosen task
    /// adds no token.
    static func prefix(for stack: [ComposerCommandLevel]) -> String {
        stack.reduce("/") { text, level in
            switch level {
            case .command(let id): return "/\(id.rawValue) "
            case .budgetTask: return text
            case .engineModels(let engine), .runModels(let engine): return text + "\(engine) "
            case .thinking(_, let modelId, _): return text + "\(modelId) "
            }
        }
    }

    /// Pop a level. Returns the composer's new text.
    public static func back(state: inout ComposerCommandState) -> String {
        if !state.stack.isEmpty { state.stack.removeLast() }
        if state.stack.isEmpty {
            state.prefix = ""
            state.engineEverywhere = false
            return "/"
        }
        state.prefix = prefix(for: state.stack)
        return state.prefix
    }

    /// Send must not post this text: it is a command, or an open level's
    /// prefix and filter (`/engine pi son`). Commands never reach the model.
    public static func holdsSend(_ text: String, state: ComposerCommandState) -> Bool {
        if exactCommand(text) != nil || partialCommand(text, state: state) != nil { return true }
        guard !state.stack.isEmpty else { return false }
        let prefix = state.prefix.trimmingCharacters(in: .whitespaces).lowercased()
        return !prefix.isEmpty && text.trimmingCharacters(in: .whitespacesAndNewlines).lowercased().hasPrefix(prefix)
    }

    /// Keep the level while the text still starts with its prefix; otherwise
    /// close it (the picker falls back to the command list or nothing).
    public static func sync(text: String, state: inout ComposerCommandState) {
        guard !state.stack.isEmpty else { return }
        if !text.lowercased().hasPrefix(state.prefix.lowercased()) {
            state = ComposerCommandState()
        }
    }

    /// The rows the picker shows for `text` in `state`.
    public static func rows(text: String, state: ComposerCommandState, context: ComposerCommandContext) -> [ComposerCommandRow] {
        guard let level = state.stack.last else {
            guard let query = trigger(text) else { return [] }
            return matches(query).map { command in
                ComposerCommandRow(id: "command:\(command.name)", badge: "Command",
                                   label: command.label, detail: command.detail,
                                   action: .open(command.id))
            }
        }
        let query = text.count >= state.prefix.count ? String(text.dropFirst(state.prefix.count)) : ""
        let back = ComposerCommandRow(id: "back", badge: "Back", label: backLabel(state), action: .back)
        let options = optionRows(level: level, state: state, context: context)
        return [back] + filter(options, query: query)
    }

    static func backLabel(_ state: ComposerCommandState) -> String {
        switch state.stack.last {
        case .budgetTask: return "Back to tasks"
        case .engineModels, .runModels: return "Back to engines"
        case .thinking: return "Back to models"
        default: return "Back to commands"
        }
    }

    static func filter(_ rows: [ComposerCommandRow], query: String) -> [ComposerCommandRow] {
        let needle = query.trimmingCharacters(in: .whitespaces).lowercased()
        if needle.isEmpty { return rows }
        return rows.filter { row in
            row.pinned || "\(row.label) \(row.detail ?? "")".lowercased().contains(needle)
        }
    }

    static func optionRows(level: ComposerCommandLevel, state: ComposerCommandState, context: ComposerCommandContext) -> [ComposerCommandRow] {
        switch level {
        case .command(.engine):
            let scope = ComposerCommandRow(
                id: "engine:scope", badge: "Scope",
                label: state.engineEverywhere ? "Default everywhere" : "This device",
                detail: state.engineEverywhere
                    ? "Tap to apply to this device only"
                    : "Tap to make the choice the default everywhere",
                action: .toggleEngineScope)
            let marked = state.engineEverywhere ? context.serverChatEngine : context.deviceEngine
            let rows = context.engines.map { engine -> ComposerCommandRow in
                let current = engine.name == marked
                // Everywhere sets engines only: a model stays this device's choice.
                let action: ComposerCommandAction = state.engineEverywhere || !hasModelChoice(engine)
                    ? .setChatEngine(engine: engine.name, model: "default", selection: nil,
                                     everywhere: state.engineEverywhere)
                    : .openModels(engine: engine.name)
                return ComposerCommandRow(
                    id: "engine:\(engine.name)", badge: "Engine",
                    label: engineOptionLabel(engine.name, context),
                    detail: current && !state.engineEverywhere ? deviceModelLabel(context) : nil,
                    current: current, action: action)
            }
            return [engineNote(state: state, context: context), scope] + rows
        case .engineModels(let engine):
            if let catalog = catalogLevel(engine: engine, run: false, context: context) {
                return catalogRows(catalog)
            }
            return rosterRows(engine, context: context, id: "engine:\(engine)",
                              current: context.deviceEngine == engine ? context.deviceModel : nil) { model in
                .setChatEngine(engine: engine, model: model, selection: nil, everywhere: false)
            }
        case .command(.runEngine):
            var rows = [ComposerCommandRow(id: "run:now", badge: "Note", label: runNote(context), action: .none)]
            rows.append(ComposerCommandRow(
                id: "run:chat", badge: "Run", label: "Same as Chat",
                detail: engineDisplayName(context.serverChatEngine ?? "magician"),
                current: context.runFollowsChat,
                action: .setRunEngine(engine: "chat", model: "default", selection: nil)))
            rows += context.engines.map { engine in
                ComposerCommandRow(
                    id: "run:\(engine.name)", badge: "Run",
                    label: engineOptionLabel(engine.name, context),
                    current: !context.runFollowsChat && engine.name == context.runEngine,
                    action: hasModelChoice(engine)
                        ? .openModels(engine: engine.name)
                        : .setRunEngine(engine: engine.name, model: "default", selection: nil))
            }
            return rows
        case .runModels(let engine):
            let onEngine = !context.runFollowsChat && context.runEngine == engine
            if let catalog = catalogLevel(engine: engine, run: true, context: context) {
                let rows = catalogRows(catalog)
                // Pi: Settings' "Use default" clears the run model (`model_selection: null`).
                guard engine == "pi", catalog.models != nil, catalog.error == nil else { return rows }
                return [ComposerCommandRow(
                    id: "run:pi:default", badge: "Model", label: "Default",
                    detail: "The coding agent's own default model",
                    current: onEngine && context.runModelSelection == nil,
                    action: .setRunEngine(engine: "pi", model: "default", selection: nil), pinned: false)] + rows
            }
            return rosterRows(engine, context: context, id: "run:\(engine)",
                              current: onEngine ? (context.runModel ?? "default") : nil) { model in
                .setRunEngine(engine: engine, model: model, selection: nil)
            }
        case .thinking(let engine, let modelId, let run):
            guard let catalog = catalogLevel(engine: engine, run: run, context: context) else { return [] }
            guard let models = catalog.models, catalog.error == nil else { return catalogRows(catalog) }
            guard let model = models.first(where: { $0.id == modelId }) else { return [] }
            return thinkingRows(model, level: catalog)
        case .command(.voiceMode):
            return ComposerVoiceMode.allCases.map { mode in
                let available = context.voiceModesAvailable.contains(mode)
                return ComposerCommandRow(
                    id: "voice:\(mode.rawValue)", badge: "Voice", label: mode.label,
                    detail: available ? nil : "Not available here",
                    current: context.voiceMode == mode,
                    action: available ? .setVoiceMode(mode) : .none, pinned: false)
            }
        case .command(.budget):
            let tasks = budgetTasks(context.tasks)
            if tasks.isEmpty {
                return [ComposerCommandRow(id: "budget:none", badge: "Task",
                                           label: "No running or paused tasks in this chat",
                                           action: .none)]
            }
            return tasks.map { task in
                ComposerCommandRow(id: "task:\(task.taskId)", badge: "Task", label: task.title,
                                   detail: statusWord(task.status),
                                   action: .pickTask(task))
            }
        case .budgetTask(let taskId, let title):
            return BudgetGrant.allCases.map { grant in
                ComposerCommandRow(id: "grant:\(grant.rawValue)", badge: "Budget", label: grant.label,
                                   detail: grant.detail,
                                   action: .grantBudget(taskId: taskId, title: title, grant: grant))
            }
        case .command(.budgetDefault):
            var rows: [ComposerCommandRow] = []
            if let error = context.budgetDefaultsError {
                rows.append(ComposerCommandRow(id: "defaults:error", badge: "Note",
                                               label: "Current defaults unavailable", detail: error,
                                               action: .none))
            }
            rows += defaultRows(.harness, caps: [.usd(50), .usd(100), .usd(250), .unlimited],
                                current: context.budgetDefaults?.harness)
            rows += defaultRows(.native, caps: [.usd(5), .usd(15), .usd(50), .unlimited],
                                current: context.budgetDefaults?.native)
            return rows
        }
    }

    static func defaultRows(_ kind: BudgetRunKind, caps: [BudgetCap], current: BudgetCap?) -> [ComposerCommandRow] {
        caps.map { cap in
            ComposerCommandRow(
                id: "default:\(kind.rawValue):\(cap.label)",
                badge: kind == .harness ? "Harness" : "Native",
                label: cap.label, detail: kind.title,
                current: current == cap,
                action: .setBudgetDefault(kind: kind, cap: cap))
        }
    }

    /// Whether picking the engine opens a model level: a catalog engine, or a
    /// roster engine with more than `default`.
    static func hasModelChoice(_ engine: (name: String, models: [String])) -> Bool {
        catalogEngines.contains(engine.name) || engine.models.contains { $0 != "default" }
    }

    /// A catalog model level's inputs: the list (nil = loading), the pick in
    /// force (only while that engine is the one in use), the pick whose
    /// thinking level a new model keeps, and how a pick becomes an action.
    struct CatalogLevel {
        let engine: String
        let id: String
        let models: [ComposerModelOption]?
        let error: String?
        let current: ComposerModelSelection?
        let keep: ComposerModelSelection?
        let action: (ComposerModelSelection) -> ComposerCommandAction
    }

    /// The catalog behind `/engine <engine>` or `/run-engine <engine>`; nil
    /// for a roster engine.
    static func catalogLevel(engine: String, run: Bool, context: ComposerCommandContext) -> CatalogLevel? {
        guard catalogEngines.contains(engine) else { return nil }
        if !run {
            return CatalogLevel(engine: engine, id: "engine:\(engine)", models: context.catalogModels[engine],
                                error: context.catalogErrors[engine],
                                current: context.deviceEngine == engine ? context.deviceModelSelection : nil,
                                keep: context.deviceModelSelection) { selection in
                .setChatEngine(engine: engine, model: "default", selection: selection, everywhere: false)
            }
        }
        if engine == "magician" {
            // Magician's loop has no per-run model: it decides with the model
            // mapped to `agentic_decision` (Settings → Agentic flows), which is
            // in force only while runs think with Magician.
            return CatalogLevel(engine: engine, id: "run:magician", models: context.loopModels,
                                error: context.loopModelsError,
                                current: runResolvesToMagician(context) ? context.loopSelection : nil,
                                keep: context.loopSelection) { .setLoopModel(selection: $0) }
        }
        let onEngine = !context.runFollowsChat && context.runEngine == engine
        return CatalogLevel(engine: engine, id: "run:\(engine)", models: context.catalogModels[engine],
                            error: context.catalogErrors[engine],
                            current: onEngine ? context.runModelSelection : nil,
                            keep: context.runModelSelection) { selection in
            .setRunEngine(engine: engine, model: "default", selection: selection)
        }
    }

    /// Level 2 from a model catalog: a note on failure, "Loading models…"
    /// until it arrives, else one row per model. A model with a thinking
    /// choice opens its levels; any other is picked at once, keeping the
    /// thinking level `keep` names when the model offers it.
    static func catalogRows(_ level: CatalogLevel) -> [ComposerCommandRow] {
        let id = level.id
        if let error = level.error {
            return [ComposerCommandRow(id: "\(id):error", badge: "Note", label: "Models unavailable: \(error)",
                                       action: .none)]
        }
        guard let models = level.models else {
            return [ComposerCommandRow(id: "\(id):loading", badge: "Note", label: "Loading models…", action: .none)]
        }
        return models.map { model in
            let current = level.current?.modelId == model.id
            var detail = model.detail
            if current, let thinking = level.current?.thinking, thinking != "auto", model.thinkingMode != "none" {
                detail = [detail, thinkingLabel(thinking)].compactMap { $0 }.joined(separator: " · ")
            }
            let action: ComposerCommandAction
            if !model.available {
                action = .none
            } else if model.thinkingChoices.count > 1 {
                action = .openThinking(engine: level.engine, modelId: model.id)
            } else {
                action = level.action(ComposerModelSelection(modelId: model.id, thinking: keepThinking(model, current: level.keep)))
            }
            return ComposerCommandRow(
                id: "\(id):\(model.id)", badge: "Model", label: model.label,
                detail: model.available ? detail : "Not available here",
                current: current, action: action, pinned: false)
        }
    }

    /// Level 3: a model's thinking levels, in `ModelThinkingControl`'s order
    /// and words. The level in force is marked when this is the model in use,
    /// else the model's default.
    static func thinkingRows(_ model: ComposerModelOption, level: CatalogLevel) -> [ComposerCommandRow] {
        let levels = model.thinkingChoices
        let inUse = level.current?.modelId == model.id ? level.current?.thinking : nil
        let marked: String
        if let inUse, levels.contains(inUse) {
            marked = inUse
        } else if let fallback = model.defaultThinking, levels.contains(fallback) {
            marked = fallback
        } else {
            marked = "auto"
        }
        return levels.map { thinking in
            ComposerCommandRow(
                id: "\(level.id):\(model.id):\(thinking)", badge: "Thinking", label: thinkingLabel(thinking),
                detail: thinking == "auto" ? model.defaultThinking.flatMap { fallback -> String? in
                    fallback == "auto" ? nil : "Default · \(thinkingLabel(fallback))"
                } : nil,
                current: thinking == marked,
                action: level.action(ComposerModelSelection(modelId: model.id, thinking: thinking)), pinned: false)
        }
    }

    /// Level 2 from the roster: the engine's `models[]`, `default` first.
    static func rosterRows(_ engine: String, context: ComposerCommandContext, id: String, current: String?,
                           action: (String) -> ComposerCommandAction) -> [ComposerCommandRow] {
        let models = context.engines.first { $0.name == engine }?.models ?? []
        return (["default"] + models.filter { $0 != "default" }).map { model in
            ComposerCommandRow(id: "\(id):\(model)", badge: "Model", label: rosterModelLabel(model),
                               current: model == current, action: action(model))
        }
    }

    /// A roster `models[]` entry: `default` is the CLI's own pick.
    static func rosterModelLabel(_ model: String) -> String { model == "default" ? "CLI default" : model }

    /// The thinking level a newly picked model gets: the current one when the
    /// model offers it, else the model's default, else `auto`.
    public static func keepThinking(_ model: ComposerModelOption, current: ComposerModelSelection?) -> String {
        if let level = current?.thinking, model.thinkingLevels.contains(level) { return level }
        if let level = model.defaultThinking, model.thinkingLevels.contains(level) { return level }
        return model.thinkingLevels.contains("auto") || model.thinkingLevels.isEmpty
            ? "auto" : model.thinkingLevels.sorted()[0]
    }

    // MARK: Labels

    public static func engineDisplayName(_ engine: String) -> String {
        ["magician": "Magician", "pi": "Coding agent", "claude_code": "Claude Code", "codex": "Codex",
         "codex_app_server": "Codex App Server", "grok": "Grok", "agy": "Antigravity", "chat": "Same as Chat"][engine]
            ?? engine.replacingOccurrences(of: "_", with: " ")
    }

    /// Web `thinkingLabel`: known levels get a display name.
    public static func thinkingLabel(_ level: String) -> String {
        ["auto": "Auto", "none": "Off", "on": "On", "minimal": "Minimal", "low": "Low", "medium": "Medium",
         "high": "High", "xhigh": "Extra high", "max": "Max", "ultra": "Ultra"][level] ?? level
    }

    /// "GPT-6.1 Sol · Low" (no level for `auto` or a model without a thinking
    /// control); the model id while the list is not loaded.
    static func selectionLabel(_ selection: ComposerModelSelection?, in models: [ComposerModelOption]?) -> String? {
        guard let selection else { return nil }
        guard let model = models?.first(where: { $0.id == selection.modelId }) else { return selection.modelId }
        let thinking = selection.thinking
        return thinking.isEmpty || thinking == "auto" || model.thinkingMode == "none"
            ? model.label : "\(model.label) · \(thinkingLabel(thinking))"
    }

    /// "Magician · GPT-6.1 Sol · Low", "Claude Code · opus", "Codex".
    static func choiceLabel(_ engine: String, modelLabel: String?) -> String {
        [engineDisplayName(engine), modelLabel].compactMap { $0 }.joined(separator: " · ")
    }

    /// An engine row: "Codex · your login".
    static func engineOptionLabel(_ engine: String, _ context: ComposerCommandContext) -> String {
        HarnessCredentialsLabel.engineName(engineDisplayName(engine), engine: engine,
                                           credentials: context.engineCredentials[engine])
    }

    static func deviceModelLabel(_ context: ComposerCommandContext) -> String? {
        if catalogEngines.contains(context.deviceEngine) {
            return selectionLabel(context.deviceModelSelection, in: context.catalogModels[context.deviceEngine])
        }
        return context.deviceModel == "default" ? nil : context.deviceModel
    }

    static func runModelLabel(_ context: ComposerCommandContext) -> String? {
        switch context.runEngine {
        case "magician": return selectionLabel(context.loopSelection, in: context.loopModels)
        case "pi": return selectionLabel(context.runModelSelection, in: context.catalogModels["pi"])
        default: return context.runModel.flatMap { $0 == "default" ? nil : $0 }
        }
    }

    /// "Now: Magician · GPT-6.1 Sol · Low (this device) · everywhere: Magician".
    static func engineNote(state: ComposerCommandState, context: ComposerCommandContext) -> ComposerCommandRow {
        let device = choiceLabel(context.deviceEngine, modelLabel: deviceModelLabel(context))
        let server = context.serverChatEngine.map { " · everywhere: \(engineDisplayName($0))" } ?? ""
        // The server stores no chat model selection: "everywhere" sets the engine only.
        return ComposerCommandRow(id: "engine:now", badge: "Note", label: "Now: \(device) (this device)\(server)",
                                  detail: state.engineEverywhere ? "The model stays a choice of this device" : nil,
                                  action: .none)
    }

    /// "Now: Coding agent · Claude Sonnet", or "Now: Same as Chat (Magician)".
    static func runNote(_ context: ComposerCommandContext) -> String {
        if context.runFollowsChat {
            return "Now: Same as Chat (\(engineDisplayName(context.serverChatEngine ?? "magician")))"
        }
        return "Now: \(choiceLabel(context.runEngine ?? "magician", modelLabel: runModelLabel(context)))"
    }

    /// Agentic flows already think with Magician (its engine, or "Same as
    /// Chat" while the chat default is Magician: Settings' `resolvedRunEngine`),
    /// so a loop model needs no pin.
    public static func runResolvesToMagician(_ context: ComposerCommandContext) -> Bool {
        context.runFollowsChat ? context.serverChatEngine == "magician" : context.runEngine == "magician"
    }

    /// The chat's running/paused tasks, newest first, one row per task.
    public static func budgetTasks(_ tasks: [ComposerCommandTask]) -> [ComposerCommandTask] {
        var seen = Set<String>()
        var latest: [ComposerCommandTask] = []
        for task in tasks.reversed() where !task.taskId.isEmpty && task.taskId != "unknown-task" {
            if seen.insert(task.taskId).inserted { latest.append(task) }
        }
        return latest.filter { task in
            let status = task.status.lowercased()
            return !terminalStatuses.contains(status) && !notStartedStatuses.contains(status)
        }
    }

    static func statusWord(_ status: String) -> String {
        let spaced = status.replacingOccurrences(of: "_", with: " ")
        return spaced.prefix(1).uppercased() + spaced.dropFirst()
    }

    // MARK: Requests and confirmations

    /// `PUT /budgets/defaults` body: the changed cap, plus the other one when known.
    public static func defaultsBody(kind: BudgetRunKind, cap: BudgetCap, current: BudgetDefaults?) -> [String: Any] {
        var body: [String: Any] = [kind.wireKey: cap.wireValue]
        let other: BudgetRunKind = kind == .harness ? .native : .harness
        if let known = other == .harness ? current?.harness : current?.native {
            body[other.wireKey] = known.wireValue
        }
        return body
    }

    /// `PUT /plane/engine` / `PUT /plane/chat-engine` body; Pi's catalog
    /// model rides as `model_selection`.
    public static func engineBody(engine: String, model: String, selection: ComposerModelSelection? = nil) -> [String: Any] {
        var body: [String: Any] = ["harness_engine": engine, "harness_model": model]
        if let selection { body["model_selection"] = selection.wireValue }
        return body
    }

    /// `PUT /plane/engine` body for agentic flows. Pi's run model is always
    /// explicit, as Settings sends it: the selection, or JSON null for the
    /// coding agent's own default.
    public static func runEngineBody(engine: String, model: String, selection: ComposerModelSelection?) -> [String: Any] {
        var body = engineBody(engine: engine, model: model, selection: selection)
        if engine == "pi", selection == nil { body["model_selection"] = NSNull() }
        return body
    }

    /// `PUT /llm/routing/agentic_decision` body.
    public static func loopModelBody(_ selection: ComposerModelSelection) -> [String: Any] {
        ["selection": selection.wireValue]
    }

    /// "Chat engine: Coding agent · Claude Sonnet · High"; everywhere "Chat engine everywhere: Codex".
    public static func chatEngineConfirmation(engine: String, model: String, selection: ComposerModelSelection?,
                                              everywhere: Bool, context: ComposerCommandContext) -> String {
        let label = choiceLabel(engine, modelLabel: modelName(model: model, selection: selection,
                                                              models: context.catalogModels[engine]))
        return everywhere ? "Chat engine everywhere: \(label)" : "Chat engine: \(label)"
    }

    /// "Agentic flows: Coding agent · Claude Sonnet · High", "Agentic flows:
    /// Coding agent · Default", "Agentic flows: Same as Chat".
    public static func runEngineConfirmation(engine: String, model: String, selection: ComposerModelSelection?,
                                             context: ComposerCommandContext) -> String {
        if engine == "chat" { return "Agentic flows: Same as Chat" }
        if engine == "pi", selection == nil { return "Agentic flows: " + choiceLabel(engine, modelLabel: "Default") }
        return "Agentic flows: " + choiceLabel(engine, modelLabel: modelName(
            model: model, selection: selection, models: context.catalogModels[engine]))
    }

    /// "Agentic flows: Magician · GPT-6.1 Sol · High".
    public static func loopModelConfirmation(selection: ComposerModelSelection, context: ComposerCommandContext) -> String {
        "Agentic flows: " + choiceLabel("magician", modelLabel: modelName(model: "default", selection: selection,
                                                                          models: context.loopModels))
    }

    public static func voiceModeConfirmation(_ mode: ComposerVoiceMode) -> String {
        "Voice mode: \(mode.label)"
    }

    /// A confirmation's model: the catalog label and thinking level (as the
    /// "Now" line names them), else a non-default roster model.
    static func modelName(model: String, selection: ComposerModelSelection?, models: [ComposerModelOption]?) -> String? {
        if let selection { return selectionLabel(selection, in: models) }
        return model == "default" ? nil : model
    }

    public static func grantConfirmation(_ grant: BudgetGrant, title: String, resumed: Bool) -> String {
        let what = grant == .unlimited ? "Removed the budget cap on \(title)" : "Added \(grant.label.dropFirst()) to \(title)"
        return resumed ? "\(what) — resumed" : what
    }

    public static func defaultConfirmation(kind: BudgetRunKind, cap: BudgetCap) -> String {
        "\(kind.title) default cap: \(cap.label)"
    }

    /// The server's own words for a failed call: `message`, else `error`, else
    /// the body, else the status.
    public static func serverErrorMessage(status: Int, body: Data?) -> String {
        if let body,
           let object = try? JSONSerialization.jsonObject(with: body) as? [String: Any] {
            for key in ["message", "error", "detail"] {
                if let text = object[key] as? String, !text.trimmingCharacters(in: .whitespaces).isEmpty {
                    return text
                }
            }
        }
        if let body, let text = String(data: body, encoding: .utf8)?
            .trimmingCharacters(in: .whitespacesAndNewlines), !text.isEmpty {
            return String(text.prefix(200))
        }
        return status == 404 ? "Not available on this Magician (HTTP 404)" : "Request failed (HTTP \(status))"
    }
}

private extension ComposerCommandAction {
    static func pickTask(_ task: ComposerCommandTask) -> ComposerCommandAction {
        .pickBudgetTask(taskId: task.taskId, title: task.title)
    }
}

func composerUsdLabel(_ value: Double) -> String {
    value == value.rounded() ? "$\(Int(value))" : String(format: "$%.2f", value)
}
