import Foundation

// MARK: - Composer slash commands
//
// `/` at the start of the composer opens a command list styled like the `@`
// picker: `/budget`, `/budget default`, `/engine`, `/run-engine`. Choosing a
// command shows its options in the same picker; choosing an option runs it on
// the client (never as a chat message). Contract:
// docs/plans/2026-10-03-slash-commands-and-run-budgets.md. The Kotlin port is
// magdroid `chat/ComposerCommands.kt`; keep the two in step.
//
// Everything here is pure: trigger, command tree, option rows, filtering, the
// request bodies and the confirmation lines. The view model does the I/O.

public enum ComposerCommandId: String, CaseIterable, Equatable {
    case budget
    case budgetDefault = "budget default"
    case engine
    case runEngine = "run-engine"
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

/// What picking a row does.
public enum ComposerCommandAction: Equatable {
    /// Pop one level.
    case back
    /// Open a command's options.
    case open(ComposerCommandId)
    /// `/engine`: flip between "this device" and "default everywhere".
    case toggleEngineScope
    /// `/engine`: the per-device chat engine (and, `everywhere`, `PUT /plane/chat-engine`).
    case setChatEngine(engine: String, model: String, everywhere: Bool)
    /// `/run-engine`: `PUT /plane/engine`.
    case setRunEngine(engine: String, model: String)
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

    public init(id: String, badge: String, label: String, detail: String? = nil,
                current: Bool = false, action: ComposerCommandAction) {
        self.id = id; self.badge = badge; self.label = label; self.detail = detail
        self.current = current; self.action = action
    }

    /// Always shown whatever the filter says.
    var pinned: Bool {
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
    public var deviceEngine: String
    public var deviceModel: String
    public var runEngine: String?
    public var runModel: String?
    /// This chat's tasks, transcript order (oldest first).
    public var tasks: [ComposerCommandTask]
    public var budgetDefaults: BudgetDefaults?
    /// Why the defaults could not be read, when they could not.
    public var budgetDefaultsError: String?

    public init(engines: [(name: String, models: [String])] = [], deviceEngine: String = "magician",
                deviceModel: String = "default", runEngine: String? = nil, runModel: String? = nil,
                tasks: [ComposerCommandTask] = [], budgetDefaults: BudgetDefaults? = nil,
                budgetDefaultsError: String? = nil) {
        self.engines = engines; self.deviceEngine = deviceEngine; self.deviceModel = deviceModel
        self.runEngine = runEngine; self.runModel = runModel; self.tasks = tasks
        self.budgetDefaults = budgetDefaults; self.budgetDefaultsError = budgetDefaultsError
    }

    public static func == (lhs: ComposerCommandContext, rhs: ComposerCommandContext) -> Bool {
        lhs.engines.map { "\($0.name)=\($0.models.joined(separator: ","))" }
            == rhs.engines.map { "\($0.name)=\($0.models.joined(separator: ","))" }
            && lhs.deviceEngine == rhs.deviceEngine && lhs.deviceModel == rhs.deviceModel
            && lhs.runEngine == rhs.runEngine && lhs.runModel == rhs.runModel
            && lhs.tasks == rhs.tasks && lhs.budgetDefaults == rhs.budgetDefaults
            && lhs.budgetDefaultsError == rhs.budgetDefaultsError
    }
}

/// Where the picker is. Empty `stack` = the command list.
public enum ComposerCommandLevel: Equatable {
    case command(ComposerCommandId)
    case budgetTask(taskId: String, title: String)
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
    ]

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

    /// Pop a level. Returns the composer's new text.
    public static func back(state: inout ComposerCommandState) -> String {
        if !state.stack.isEmpty { state.stack.removeLast() }
        if state.stack.isEmpty {
            state.prefix = ""
            state.engineEverywhere = false
            return "/"
        }
        return state.prefix
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
        state.stack.count > 1 ? "Back to tasks" : "Back to commands"
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
            let rows = engineRows(context).map { pair in
                ComposerCommandRow(
                    id: "engine:\(pair.engine):\(pair.model)", badge: "Engine",
                    label: engineLabel(pair.engine, pair.model),
                    current: pair.engine == context.deviceEngine && pair.model == context.deviceModel,
                    action: .setChatEngine(engine: pair.engine, model: pair.model,
                                           everywhere: state.engineEverywhere))
            }
            return [scope] + rows
        case .command(.runEngine):
            return engineRows(context).map { pair in
                ComposerCommandRow(
                    id: "run:\(pair.engine):\(pair.model)", badge: "Run",
                    label: engineLabel(pair.engine, pair.model),
                    current: pair.engine == context.runEngine && pair.model == (context.runModel ?? "default"),
                    action: .setRunEngine(engine: pair.engine, model: pair.model))
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

    /// Engine × model pairs, roster order.
    static func engineRows(_ context: ComposerCommandContext) -> [(engine: String, model: String)] {
        context.engines.flatMap { engine in
            (engine.models.isEmpty ? ["default"] : engine.models).map { (engine.name, $0) }
        }
    }

    public static func engineLabel(_ engine: String, _ model: String) -> String {
        model == "default" ? engine : "\(engine) · \(model)"
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

    public static func engineBody(engine: String, model: String) -> [String: Any] {
        ["harness_engine": engine, "harness_model": model]
    }

    public static func chatEngineConfirmation(engine: String, model: String, everywhere: Bool) -> String {
        everywhere
            ? "Chat engine is now \(engineLabel(engine, model)) everywhere"
            : "Chat engine on this device: \(engineLabel(engine, model))"
    }

    public static func runEngineConfirmation(engine: String, model: String) -> String {
        "Run engine: \(engineLabel(engine, model))"
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
